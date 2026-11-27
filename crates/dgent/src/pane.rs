//! A shell in a PTY: output goes through a vt100 parser with scrollback, input goes
//! through a writer thread so a stalled PTY never blocks the UI loop.

use std::io::{Read, Write};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use portable_pty::{Child, CommandBuilder, ExitStatus, MasterPty, PtySize, native_pty_system};

/// Child TERM. Not xterm-256color: it advertises `rep` (CSI b), which vt100 0.16 ignores, so
/// ncurses apps would drop repeated characters. screen-256color matches what vt100 renders and
/// exists on practically every remote host, which matters because `ssh` passes TERM along.
/// Cost: apps that hardcode xterm key sequences for TERM=xterm* (e.g. Vim's modified arrows)
/// fall back to screen's, and Home/End are sent as screen's `ESC[1~`/`ESC[4~` (see keys.rs).
pub const TERM: &str = "screen-256color";
const SCROLLBACK: usize = 10_000;
/// For shells that emit no OSC 133 marks, `run` treats this long without output as the end.
const QUIET: Duration = Duration::from_secs(1);
/// Hard cap for `run`, for commands that never go quiet (e.g. `yes`).
const MAX_RUN: Duration = Duration::from_secs(30);

pub struct Pane {
    parser: vt100::Parser,
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    out_rx: Receiver<Vec<u8>>,
    in_tx: Sender<Vec<u8>>,
    /// Raw output since `run` typed its command, while a capture is active.
    capture: Option<Vec<u8>>,
    /// Newlines printed since the last OSC 133;D mark (MAX if none yet). Zero means the
    /// prompt we're at came straight after a mark, so the current shell (local or remote)
    /// emits marks and `run` can wait for one.
    lines_since_mark: usize,
}

/// What a command printed, and its exit code when the shell emits OSC 133 marks.
#[derive(Debug)]
#[allow(dead_code)] // read by approved agent commands once W7 lands
pub struct Capture {
    pub output: String,
    pub exit: Option<i32>,
}

impl Pane {
    pub fn spawn(mut command: CommandBuilder, rows: u16, cols: u16) -> Result<Self> {
        let pty = native_pty_system()
            .openpty(size(rows, cols))
            .context("open pty")?;
        command.env("TERM", TERM);
        let child = pty.slave.spawn_command(command).context("spawn shell")?;
        drop(pty.slave);
        let mut writer = pty.master.take_writer()?;
        let mut reader = pty.master.try_clone_reader()?;

        let (out_tx, out_rx) = mpsc::sync_channel::<Vec<u8>>(64);
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while let Ok(n @ 1..) = reader.read(&mut buf) {
                if out_tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
        // Unbounded on purpose: a paste is already in memory, and the UI must never wait on a
        // program that isn't reading. The thread is detached, so quitting never waits on it.
        let (in_tx, in_rx) = mpsc::channel::<Vec<u8>>();
        std::thread::spawn(move || {
            // Errors end the thread: once the PTY closes nothing reads input, and shell exit
            // is detected via try_wait, not write failures.
            for bytes in in_rx {
                if writer
                    .write_all(&bytes)
                    .and_then(|_| writer.flush())
                    .is_err()
                {
                    break;
                }
            }
        });

        Ok(Self {
            parser: vt100::Parser::new(rows, cols, SCROLLBACK),
            master: pty.master,
            child,
            out_rx,
            in_tx,
            capture: None,
            lines_since_mark: usize::MAX,
        })
    }

    pub fn screen(&self) -> &vt100::Screen {
        self.parser.screen()
    }

    /// Queue bytes for the shell. Never blocks.
    pub fn write(&self, bytes: &[u8]) {
        let _ = self.in_tx.send(bytes.to_vec());
    }

    /// Feed pending shell output to the parser, bounded so a flood (e.g. `yes`) can't starve
    /// key handling. Returns whether any output arrived.
    pub fn pump(&mut self) -> bool {
        let mut any = false;
        for _ in 0..64 {
            match self.out_rx.try_recv() {
                Ok(bytes) => {
                    self.parser.process(&bytes);
                    // shortcut: misses a mark split across reads; the next prompt catches it.
                    let newlines = |b: &[u8]| b.iter().filter(|&&c| c == b'\n').count();
                    self.lines_since_mark = match rfind(&bytes, b"\x1b]133;D") {
                        Some(d) => newlines(&bytes[d..]),
                        None => self.lines_since_mark.saturating_add(newlines(&bytes)),
                    };
                    if let Some(c) = &mut self.capture {
                        c.extend_from_slice(&bytes);
                    }
                    any = true;
                }
                // PTY EOF doesn't mean the shell exited (`exec cmd >/dev/null ...`): callers
                // rely on try_wait for that.
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        any
    }

    pub fn resize(&mut self, rows: u16, cols: u16) -> Result<()> {
        let screen = self.parser.screen();
        let (cur, _) = screen.cursor_position();
        // vt100 drops the bottom rows on a height decrease. Like xterm, scroll the main screen
        // up first so the cursor line stays visible and the top rows go to scrollback.
        // Full-screen apps on the alternate screen redraw on SIGWINCH; the shell rows hidden
        // behind them are still truncated (shortcut: vt100 only lets us scroll the active grid).
        // Narrowing still truncates each row (no reflow).
        if !screen.alternate_screen() && cur >= rows {
            let n = cur + 1 - rows;
            self.parser
                .process(format!("\x1b[{n}S\x1b[{n}A").as_bytes());
        }
        self.parser.screen_mut().set_size(rows, cols);
        self.master.resize(size(rows, cols))?;
        Ok(())
    }

    pub fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }

    pub fn kill(&mut self) {
        let _ = self.child.kill();
    }

    /// Type `cmd` and collect its output until the shell's OSC 133;D end-of-command mark, or,
    /// when the current prompt came without one, until output goes quiet (then `exit` is None).
    /// Capped at `MAX_RUN`. Blocks the caller.
    #[allow(dead_code)] // called by approved agent commands once W7 lands
    pub fn run(&mut self, cmd: &str) -> Capture {
        // Let the previous prompt (and its own D mark) arrive before capturing.
        self.wait_quiet(Duration::from_millis(100), Duration::from_secs(2));
        // A shell without marks (e.g. after `ssh` to an unmarked host), or a multi-line prompt,
        // falls back to the quiet timeout.
        let marked = self.lines_since_mark == 0;
        self.capture = Some(Vec::new());
        self.write(format!("{cmd}\r").as_bytes());
        let start = Instant::now();
        let mut last = Instant::now();
        loop {
            if self.pump() {
                last = Instant::now();
            } else {
                std::thread::sleep(Duration::from_millis(10));
            }
            let buf = self.capture.as_deref().unwrap_or_default();
            if let Some(c) = parse_capture(buf, true) {
                self.capture = None;
                return c;
            }
            if (!marked && last.elapsed() > QUIET) || start.elapsed() > MAX_RUN {
                let buf = self.capture.take().unwrap_or_default();
                return parse_capture(&buf, false).expect("unfinished capture always parses");
            }
        }
    }

    fn wait_quiet(&mut self, quiet: Duration, max: Duration) {
        let start = Instant::now();
        let mut last = Instant::now();
        while last.elapsed() < quiet && start.elapsed() < max {
            if self.pump() {
                last = Instant::now();
            } else {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

fn size(rows: u16, cols: u16) -> PtySize {
    PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    }
}

fn find(h: &[u8], n: &[u8]) -> Option<usize> {
    h.windows(n.len()).position(|w| w == n)
}

fn rfind(h: &[u8], n: &[u8]) -> Option<usize> {
    h.windows(n.len()).rposition(|w| w == n)
}

/// Index just past the OSC terminator (BEL or ESC \) at or after `from`, which must be
/// past the OSC's own `ESC ]`.
fn osc_end(buf: &[u8], from: usize) -> Option<usize> {
    let i = from + buf[from..].iter().position(|&b| b == 0x07 || b == 0x1b)?;
    match buf[i] {
        0x07 => Some(i + 1),
        _ if i + 1 < buf.len() => Some(i + 2),
        _ => None,
    }
}

/// Turn captured bytes into a `Capture`. With `need_end`, returns None until a complete
/// OSC 133;D mark has arrived; otherwise takes everything as output with an unknown exit.
/// A command that prints a D mark itself can fake its end and exit code (inherent to OSC 133).
fn parse_capture(buf: &[u8], need_end: bool) -> Option<Capture> {
    let mark = find(buf, b"\x1b]133;D").and_then(|d| Some((d, osc_end(buf, d + 7)?)));
    let (body, exit) = match mark {
        Some((d, end)) => {
            let params = String::from_utf8_lossy(&buf[d + 7..end]);
            let exit = params
                .trim_start_matches(';')
                .split([';', '\x07', '\x1b'])
                .next()
                .and_then(|s| s.parse().ok());
            (&buf[..d], exit)
        }
        None if need_end => return None,
        None => (buf, None),
    };
    // Output starts after the C (command started) mark; without one, skip the echoed command line.
    let output = match find(body, b"\x1b]133;C").and_then(|c| osc_end(body, c + 2)) {
        Some(start) => strip(&body[start..]),
        None => {
            let text = strip(body);
            text.split_once('\n')
                .map_or(String::new(), |(_, rest)| rest.into())
        }
    };
    Some(Capture { output, exit })
}

/// Drop escape sequences and carriage returns from terminal output.
fn strip(bytes: &[u8]) -> String {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            0x1b => match bytes.get(i + 1) {
                Some(b'[') => {
                    i += 2;
                    while i < bytes.len() && !(0x40..=0x7e).contains(&bytes[i]) {
                        i += 1;
                    }
                }
                Some(b']') => i = osc_end(bytes, i + 2).unwrap_or(bytes.len()) - 1,
                _ => i += 1,
            },
            b'\n' | b'\t' => out.push(bytes[i]),
            b if b < 0x20 || b == 0x7f => {}
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_escapes() {
        assert_eq!(
            strip(b"\x1b[1;31mred\x1b[0m\r\n\x1b]0;title\x07ok\x1b]133;A\x1b\\\x1b=x"),
            "red\nokx"
        );
    }

    #[test]
    fn parses_marks() {
        let c = parse_capture(b"ls\r\n\x1b]133;C\x07a\r\nb\r\n\x1b]133;D;2\x07$ ", true).unwrap();
        assert_eq!((c.output.as_str(), c.exit), ("a\nb\n", Some(2)));
        let c = parse_capture(b"ls\r\na\r\n\x1b]133;D;0\x1b\\$ ", true).unwrap();
        assert_eq!((c.output.as_str(), c.exit), ("a\n", Some(0)));
        assert!(parse_capture(b"ls\r\na\r\n\x1b]133;D;0", true).is_none());
        // A mark cut off at the quiet timeout still yields a capture, with unknown exit.
        let c = parse_capture(b"ls\r\na\r\n\x1b]133;D;0", false).unwrap();
        assert_eq!(c.exit, None);
        let c = parse_capture(b"ls\r\na\r\n$ ", false).unwrap();
        assert_eq!((c.output.as_str(), c.exit), ("a\n$ ", None));
    }

    fn sh(ps1: &str) -> Pane {
        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.env("PS1", ps1);
        Pane::spawn(cmd, 24, 80).unwrap()
    }

    #[test]
    fn runs_command_with_marks() {
        // dash expands $? in PS1, so the prompt reports the last exit like a precmd hook would.
        let mut p = sh("\x1b]133;D;$?\x07$ ");
        let c = p.run("echo hi; false");
        assert_eq!((c.output.trim(), c.exit), ("hi", Some(1)));
        // With marks, a silent pause longer than QUIET doesn't end the capture.
        let c = p.run("sleep 1.5; echo done");
        assert_eq!((c.output.trim(), c.exit), ("done", Some(0)));
        // An inner shell without marks (like ssh to an unmarked host) falls back to quiet.
        p.write(b"PS1='$ ' sh\r");
        let t = Instant::now();
        let c = p.run("echo inner");
        assert_eq!(c.exit, None);
        assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
        p.kill();
    }

    #[test]
    fn runs_command_without_marks() {
        let mut p = sh("$ ");
        let c = p.run("echo hi; false");
        assert_eq!(c.exit, None);
        assert!(c.output.starts_with("hi\n"), "{:?}", c.output);
        p.kill();
    }

    #[test]
    fn shrink_keeps_cursor_line() {
        let mut p = sh("$ ");
        p.run("seq 1 20");
        p.resize(5, 80).unwrap();
        let text = p.screen().contents();
        assert!(text.contains("20\n$"), "{text:?}");
        p.parser.screen_mut().set_scrollback(SCROLLBACK);
        assert!(p.screen().contents().contains('1'));
        p.kill();
    }

    #[test]
    fn write_does_not_block_when_shell_is_not_reading() {
        let mut p = sh("$ ");
        p.write(b"sleep 5\r");
        let t = Instant::now();
        p.write(&vec![b'x'; 4 << 20]);
        p.pump();
        assert!(t.elapsed() < Duration::from_secs(1));
        p.kill();
    }
}
