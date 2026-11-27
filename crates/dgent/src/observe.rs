//! Command boundaries and failure detection, from the PTY byte stream alone.
//!
//! Remote shells are just bytes in the pane, so nothing here runs on the remote host.
//! Two sources, best first:
//!
//! - **OSC 133 marks** (`A` prompt, `B` input, `C` output, `D;exit` done). Many shell
//!   setups emit them already; [`INJECT`] adds them to the current bash/zsh for the
//!   session (typed by `C-] i`, nothing written to disk), local or over ssh.
//! - **Heuristics** when no marked command is in progress (an unmarked shell, or an ssh
//!   session inside a marked local one): a line ending like a prompt (`$ `, `# `, `% `,
//!   `❯ `) at the end of a read starts or ends a command, and failure is guessed from
//!   error patterns in the output. Exit codes are unknown there.

use std::ops::Range;

/// One line, typed into the active shell, that makes bash or zsh emit OSC 133 marks for
/// the rest of the session. Leading space keeps it out of history (`HISTCONTROL=ignorespace`,
/// zsh `HIST_IGNORE_SPACE`). `$_dg_s` carries `$?` past our printf so the user's own
/// `PROMPT_COMMAND` still sees it (`&& :` so `set -e` doesn't exit on it). `$_dg_on` makes a
/// second `C-] i` a no-op. zsh's branch is under `eval` so sh/dash can parse the line.
pub const INJECT: &str = concat!(
    r#" if [ -n "${_dg_on-}" ]; then :; elif [ -n "${ZSH_VERSION-}" ]; then _dg_on=1; eval '_dg_pre(){ printf "\033]133;D;%s\007\033]133;A\007" $?; }; "#,
    r#"_dg_exe(){ printf "\033]133;C\007"; }; precmd_functions=(_dg_pre ${precmd_functions-}); "#,
    r#"preexec_functions+=(_dg_exe); PS1="$PS1%{$(printf "\033]133;B\007")%}"'; "#,
    r#"elif [ -n "${BASH_VERSION-}" ]; then _dg_on=1; "#,
    r#"PROMPT_COMMAND='_dg_s=$?; printf "\033]133;D;%s\007\033]133;A\007" $_dg_s; (exit $_dg_s) && :'"${PROMPT_COMMAND:+;$PROMPT_COMMAND}"; "#,
    r#"PS0='\033]133;C\007'"${PS0-}"; PS1="$PS1"'\[\033]133;B\007\]'; "#,
    r#"else echo 'dgent: command marks need bash or zsh'; fi"#,
    "\r"
);

/// Bytes of output after `C-] i` within which its `D` is expected: the echoed line
/// (a few hundred bytes, more with line-editor redraws) plus the mark itself.
const INJECT_WINDOW: u64 = 4096;

/// Output kept per command: the last this many bytes.
const OUTPUT_CAP: usize = 16 * 1024;

const PROMPT_ENDS: [&str; 4] = ["$ ", "# ", "% ", "❯ "];

/// Lowercase substrings that mark an output line as an error.
const ERROR_PATTERNS: [&str; 12] = [
    "command not found",
    "no such file or directory",
    "permission denied",
    "operation not permitted",
    "error:",
    "fatal:",
    "traceback (most recent call last)",
    "segmentation fault",
    "connection refused",
    "could not resolve hostname",
    "unable to",
    "cannot ",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandFinished {
    /// The command line as echoed, escapes stripped.
    pub cmd: String,
    /// Absolute byte offsets of the command's output in the PTY stream. Without a `C` mark
    /// (bash < 4.4) it starts at `B`, so it includes the echoed command line.
    pub output_range: Range<u64>,
    /// The output as text, escapes stripped, at most the last `OUTPUT_CAP` bytes.
    pub output: String,
    /// From OSC 133 `D`. `None` when the boundary was inferred from a prompt.
    pub exit: Option<i32>,
    /// First output line matching a known error pattern.
    pub error_hint: Option<String>,
}

impl CommandFinished {
    pub fn failed(&self) -> bool {
        self.exit
            .map_or(self.error_hint.is_some(), |code| code != 0)
    }
}

#[derive(Default, PartialEq)]
enum Parse {
    #[default]
    Ground,
    Esc,
    /// ESC followed by intermediates, e.g. `ESC ( B`.
    EscInter,
    Csi,
    /// OSC, or (when `false`) DCS/APC/PM/SOS, whose payload we drop.
    Str(bool),
    StrEsc(bool),
}

#[derive(Default, PartialEq)]
enum Mark {
    /// No marked command in progress: heuristics apply.
    #[default]
    None,
    Prompt,
    Input {
        start: u64,
    },
    Output {
        start: u64,
    },
}

#[derive(Default, PartialEq)]
enum Guess {
    #[default]
    Idle,
    Prompt,
    Output {
        cmd: String,
        start: u64,
    },
}

#[derive(Default)]
pub struct Observer {
    pos: u64,
    esc: Parse,
    esc_start: u64,
    osc: Vec<u8>,
    /// Text since the last boundary, with CR and backspace applied to its last line.
    buf: Vec<u8>,
    /// Stream offset where `buf`'s last line starts.
    line_start: u64,
    cr: bool,
    mark: Mark,
    cmd: String,
    guess: Guess,
    /// Stream offset until which a `D` is taken as the injected line's own (see `injected`).
    injected: Option<u64>,
}

impl Observer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed raw PTY output; returns the commands that finished in it.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<CommandFinished> {
        let mut done = Vec::new();
        for &b in bytes {
            self.byte(b, &mut done);
            self.pos += 1;
        }
        done
    }

    /// Call after typing [`INJECT`]. Its first `D` comes from the shell that was just
    /// marked, which may be inside a marked command (ssh): that `D` must not end the outer one.
    /// If no `D` arrives soon (a shell without marks, or `C-] i` pressed inside an editor),
    /// the expectation lapses so it can't swallow a later command's `D`.
    pub fn injected(&mut self) {
        self.injected = Some(self.pos + INJECT_WINDOW);
    }

    /// Call when output has paused. A paused line that ends like a prompt is taken as one.
    /// shortcut: typeahead that lands right after the prompt hides it; marks (`C-] i`) don't
    /// have this problem.
    pub fn idle(&mut self) -> Option<CommandFinished> {
        let mut done = Vec::new();
        // A marked shell that ended at its prompt (Ctrl-D, or ssh dropped) sends no `C`/`D`.
        // Input after `B` is one line until `C`, so a newline then a prompt means an unmarked
        // shell is back: go back to guessing.
        if matches!(self.mark, Mark::Input { .. })
            && self.buf.contains(&b'\n')
            && looks_like_prompt(&last_line(&self.buf))
        {
            self.mark = Mark::None;
            self.guess = Guess::Idle;
        }
        if self.mark_allows_guess() && self.esc == Parse::Ground {
            self.guess_prompt(&mut done);
        }
        done.pop()
    }

    fn byte(&mut self, b: u8, done: &mut Vec<CommandFinished>) {
        match (&self.esc, b) {
            (Parse::Ground, 0x1b) => {
                self.esc = Parse::Esc;
                self.esc_start = self.pos;
            }
            (Parse::Ground, _) => self.text(b),
            (Parse::Esc, b'[') => self.esc = Parse::Csi,
            (Parse::Esc, b']') => {
                self.osc.clear();
                self.esc = Parse::Str(true);
            }
            (Parse::Esc, b'P' | b'_' | b'^' | b'X') => self.esc = Parse::Str(false),
            (Parse::Esc | Parse::EscInter, 0x20..=0x2f) => self.esc = Parse::EscInter,
            (Parse::Esc | Parse::EscInter, _) => self.esc = Parse::Ground,
            (Parse::Csi, 0x40..=0x7e) => self.esc = Parse::Ground,
            (Parse::Csi, _) => {}
            (Parse::Str(osc), 0x07) => {
                let osc = *osc;
                self.esc = Parse::Ground;
                if osc {
                    self.osc_done(done);
                }
            }
            (Parse::Str(osc), 0x1b) => self.esc = Parse::StrEsc(*osc),
            (Parse::Str(true), _) => {
                if self.osc.len() < 256 {
                    self.osc.push(b);
                }
            }
            (Parse::Str(false), _) => {}
            (Parse::StrEsc(osc), _) => {
                // ESC \ is ST; anything else after ESC also ends the string in practice.
                let osc = *osc;
                self.esc = Parse::Ground;
                if osc {
                    self.osc_done(done);
                }
            }
        }
    }

    fn text(&mut self, b: u8) {
        match b {
            b'\n' => {
                self.cr = false;
                if self.guess == Guess::Prompt && self.mark_allows_guess() {
                    let cmd = last_line(&self.buf).trim().to_string();
                    self.buf.clear();
                    // An empty line: look for the next prompt again.
                    self.guess = if cmd.is_empty() {
                        Guess::Idle
                    } else {
                        Guess::Output {
                            cmd,
                            start: self.pos + 1,
                        }
                    };
                } else {
                    self.buf.push(b'\n');
                }
                self.line_start = self.pos + 1;
            }
            b'\r' => self.cr = true,
            0x08 => {
                while let Some(c) = self.buf.pop() {
                    if c & 0xc0 != 0x80 {
                        break;
                    }
                }
            }
            b'\t' | 0x20.. if b != 0x7f => {
                if std::mem::take(&mut self.cr) {
                    // CR then text overwrites the line (progress bars, zsh's PROMPT_SP).
                    let start = self
                        .buf
                        .iter()
                        .rposition(|&c| c == b'\n')
                        .map_or(0, |i| i + 1);
                    self.buf.truncate(start);
                    self.line_start = self.pos;
                }
                self.buf.push(b);
                if self.buf.len() > 2 * OUTPUT_CAP {
                    self.buf.drain(..self.buf.len() - OUTPUT_CAP);
                }
            }
            _ => {}
        }
    }

    fn mark_allows_guess(&self) -> bool {
        matches!(self.mark, Mark::None | Mark::Output { .. })
    }

    fn osc_done(&mut self, done: &mut Vec<CommandFinished>) {
        let osc = String::from_utf8_lossy(&self.osc).into_owned();
        let mut parts = osc.split(';');
        if parts.next() != Some("133") {
            return;
        }
        self.guess = Guess::Idle;
        match parts.next() {
            Some("A") => self.mark = Mark::Prompt,
            Some("B") => {
                self.mark = Mark::Input {
                    start: self.pos + 1,
                };
                self.buf.clear();
            }
            Some("C") => {
                self.cmd = text_of(&self.buf).trim().to_string();
                self.buf.clear();
                self.mark = Mark::Output {
                    start: self.pos + 1,
                };
            }
            Some("D") => {
                let exit = parts.next().and_then(|c| c.trim().parse().ok());
                let end = self.esc_start;
                let mark = std::mem::replace(&mut self.mark, Mark::Prompt);
                match mark {
                    _ if self.injected.take().is_some_and(|until| self.pos <= until) => {}
                    Mark::Output { start } => {
                        let cmd = std::mem::take(&mut self.cmd);
                        self.emit(done, cmd, start..end, exit);
                    }
                    // No `C` (bash < 4.4 has no PS0): the first line is the command, and
                    // `output_range` includes it.
                    Mark::Input { start } => {
                        let text = text_of(&self.buf);
                        let (cmd, _) = text.split_once('\n').unwrap_or((&text, ""));
                        let cmd = cmd.trim().to_string();
                        let first = self.buf.iter().position(|&c| c == b'\n');
                        self.buf.drain(..first.map_or(self.buf.len(), |i| i + 1));
                        self.emit(done, cmd, start..end, exit);
                    }
                    // D with no command pending (startup, or an outer shell after an inner one).
                    _ => {}
                }
                self.buf.clear();
            }
            _ => {}
        }
    }

    fn guess_prompt(&mut self, done: &mut Vec<CommandFinished>) {
        if self.cr {
            return;
        }
        if !looks_like_prompt(&last_line(&self.buf)) {
            return;
        }
        match std::mem::replace(&mut self.guess, Guess::Prompt) {
            Guess::Prompt => self.guess = Guess::Prompt,
            Guess::Output { cmd, start } => {
                let keep = self
                    .buf
                    .iter()
                    .rposition(|&b| b == b'\n')
                    .map_or(0, |i| i + 1);
                self.buf.truncate(keep);
                let end = self.line_start;
                self.emit(done, cmd, start..end, None);
                self.buf.clear();
            }
            Guess::Idle => self.buf.clear(),
        }
    }

    fn emit(
        &mut self,
        done: &mut Vec<CommandFinished>,
        cmd: String,
        output_range: Range<u64>,
        exit: Option<i32>,
    ) {
        if cmd.is_empty() {
            return;
        }
        let tail = &self.buf[self.buf.len().saturating_sub(OUTPUT_CAP)..];
        let output = text_of(tail).trim_end().to_string();
        let error_hint = output
            .lines()
            .find(|l| {
                let l = l.to_lowercase();
                ERROR_PATTERNS.iter().any(|p| l.contains(p))
            })
            .map(|l| l.trim().to_string());
        self.buf.clear();
        done.push(CommandFinished {
            cmd,
            output_range,
            output,
            exit,
            error_hint,
        });
    }
}

fn text_of(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn looks_like_prompt(line: &str) -> bool {
    PROMPT_ENDS.iter().any(|end| line.ends_with(end))
}

fn last_line(buf: &[u8]) -> String {
    let start = buf.iter().rposition(|&c| c == b'\n').map_or(0, |i| i + 1);
    text_of(&buf[start..])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fixtures are raw PTY recordings (see `tests/fixtures/README.md`); `\0IDLE\0` marks
    /// where output paused before the next keystrokes.
    fn run(name: &str, chunk: usize) -> Vec<CommandFinished> {
        let bytes = std::fs::read(format!("tests/fixtures/{name}.bin")).unwrap();
        let mut o = Observer::new();
        let mut done = Vec::new();
        for (i, part) in bytes.split(|&b| b == 0).enumerate() {
            if i % 2 == 1 {
                match part {
                    b"IDLE" => done.extend(o.idle()),
                    b"INJECT" => o.injected(),
                    _ => panic!("bad fixture marker"),
                }
            } else {
                for c in part.chunks(chunk) {
                    done.extend(o.feed(c));
                }
            }
        }
        done
    }

    /// `(cmd, exit, failed)` for each command, same at any read size.
    fn summary(name: &str) -> Vec<(String, Option<i32>, bool)> {
        let whole = run(name, usize::MAX);
        for size in [1, 7] {
            assert_eq!(run(name, size), whole, "{name} read {size} bytes at a time");
        }
        whole
            .into_iter()
            .map(|f| (f.failed(), f.cmd, f.exit))
            .map(|(failed, cmd, exit)| (cmd, exit, failed))
            .collect()
    }

    fn s(cmd: &str, exit: Option<i32>, failed: bool) -> (String, Option<i32>, bool) {
        (cmd.into(), exit, failed)
    }

    #[test]
    fn bash_with_marks() {
        assert_eq!(
            summary("bash-marks"),
            [
                s("echo hi", Some(0), false),
                s("ls /nonexistent", Some(2), true),
                s("false", Some(1), true),
            ]
        );
        let f = &run("bash-marks", usize::MAX)[1];
        assert_eq!(
            f.output,
            "ls: cannot access '/nonexistent': No such file or directory"
        );
        assert_eq!(f.error_hint.as_deref(), Some(f.output.as_str()));
    }

    #[test]
    fn zsh_with_marks() {
        assert_eq!(
            summary("zsh-marks"),
            [
                s("PS1='%# '", None, false),
                s("echo hi", Some(0), false),
                s("ls /nonexistent", Some(2), true),
                s("false", Some(1), true),
            ]
        );
        assert_eq!(run("zsh-marks", usize::MAX)[1].output, "hi");
    }

    #[test]
    fn bash_without_marks() {
        // `false` fails silently, so without an exit code it looks fine; `exit` never
        // gets a prompt back.
        assert_eq!(
            summary("bash-plain"),
            [
                s("echo hi", None, false),
                s("ls /nonexistent", None, true),
                s("false", None, false),
            ]
        );
    }

    #[test]
    fn zsh_without_marks() {
        assert_eq!(
            summary("zsh-plain"),
            [
                s("PS1='%# '", None, false),
                s("echo hi", None, false),
                s("ls /nonexistent", None, true),
                s("false", None, false),
            ]
        );
    }

    #[test]
    fn unmarked_ssh_inside_marked_shell_then_injected() {
        assert_eq!(
            summary("ssh-nested"),
            [
                // Remote, before `C-] i`: guessed from the prompt.
                s("ls /nonexistent", None, true),
                s("true", None, false),
                // Remote, after: marks.
                s("false", Some(1), true),
                // ssh exits with the remote shell's last status. The remote `exit` has
                // no `D` of its own, so the outer `D` lands on it.
                s("exit", Some(1), true),
                // Local again.
                s(
                    "ssh -p 2222 -o BatchMode=yes nohost.invalid",
                    Some(255),
                    true
                ),
            ]
        );
    }

    #[test]
    fn injection_without_a_mark_lapses() {
        let mut o = Observer::new();
        o.feed(b"\x1b]133;B\x07vim\r\n\x1b]133;C\x07");
        // C-] i typed into vim: no D follows, the editor just redraws.
        o.injected();
        o.feed(&[b'~'; 5000]);
        let done = o.feed(b"\x1b]133;D;1\x07");
        assert_eq!(done.len(), 1, "vim's own D must still be reported");
        assert_eq!(done[0].exit, Some(1));
    }

    #[test]
    fn non_utf8_prompt_after_silent_command() {
        let mut o = Observer::new();
        o.feed(b"caf\xe9$ ");
        o.idle();
        o.feed(b"true\r\ncaf\xe9$ ");
        let f = o.idle().unwrap();
        assert_eq!((f.cmd.as_str(), f.output.as_str()), ("true", ""));
    }

    #[test]
    fn marked_shell_ending_at_its_prompt_hands_back_to_guessing() {
        let mut o = Observer::new();
        // Remote marked prompt, then Ctrl-D: no C or D, just the local unmarked prompt.
        o.feed(b"\x1b]133;A\x07r$ \x1b]133;B\x07logout\r\nConnection closed.\r\n$ ");
        assert_eq!(o.idle(), None);
        o.feed(b"ls /nope\r\nls: /nope: No such file or directory\r\n$ ");
        assert!(o.idle().unwrap().failed());
    }

    #[test]
    fn output_range_covers_output() {
        let bytes = b"\x1b]133;B\x07ls\r\n\x1b]133;C\x07nope\r\n\x1b]133;D;2\x07";
        let f = Observer::new().feed(bytes).remove(0);
        assert_eq!(
            &bytes[f.output_range.start as usize..f.output_range.end as usize],
            b"nope\r\n"
        );
        assert_eq!(f.output, "nope");
    }

    #[test]
    fn progress_bars_and_backspace_collapse() {
        let mut o = Observer::new();
        o.feed(b"$ ");
        o.idle();
        o.feed(b"mak\x08ke\r\n10%\r50%\r\x1b[Kerror: boom\r\n$ ");
        let f = o.idle().unwrap();
        assert_eq!(f.cmd, "make");
        assert_eq!(f.output, "error: boom");
        assert!(f.failed());
    }

    #[test]
    fn ordinary_output_has_no_error_hint() {
        let mut o = Observer::new();
        o.feed(b"$ ");
        o.idle();
        o.feed(b"cargo test\r\ntest result: ok. 3 passed; 0 failed\r\n$ ");
        assert_eq!(o.idle().unwrap().error_hint, None);
    }
}
