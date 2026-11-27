//! Dgent: a terminal wrapper with an LLM overseer.
//!
//! Scaffold: one shell in a PTY, a status line, and an "ask" prompt wired to a
//! [`Backend`]. Prefix key is Ctrl-]; then `a` asks, `q` quits, Ctrl-] sends a
//! literal Ctrl-]. A proposed command waits in the panel for `y` / `n` / `e`.

mod keys;
mod ui;

use std::io::{Read, Write};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{self, Event as TermEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::{execute, terminal};
use dgent_backend::{Backend, EchoBackend, Event as AgentEvent, Request, ToolReply};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use tui_term::widget::PseudoTerminal;

/// How many screen lines go to the backend with a question.
const CONTEXT_LINES: usize = 40;
const PANEL_HEIGHT: u16 = 8;

enum Mode {
    Normal,
    Prefix,
    Ask(String),
    /// Editing the pending proposal before running it.
    Edit(String),
}

fn main() -> Result<()> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    let (cols, rows) = terminal::size()?;

    let pty = native_pty_system()
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .context("open pty")?;
    let mut command = CommandBuilder::new(shell);
    // Match what vt100 can render rather than inheriting the outer TERM. Not xterm-256color:
    // it advertises `rep` (CSI b), which vt100 ignores. screen-256color is also on every remote host.
    command.env("TERM", "screen-256color");
    command.cwd(std::env::current_dir().context("resolve launch directory")?);
    let mut child = pty.slave.spawn_command(command).context("spawn shell")?;
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

    let mut parser = vt100::Parser::new(rows, cols, 10_000);
    let mut backend: Box<dyn Backend> = Box::new(EchoBackend::default());
    let mut turn: Option<Receiver<AgentEvent>> = None;
    let mut panel = ui::Panel::default();
    let mut mode = Mode::Normal;
    let mut pane_size = (rows, cols);

    terminal::enable_raw_mode()?;
    execute!(
        std::io::stdout(),
        terminal::EnterAlternateScreen,
        event::EnableBracketedPaste
    )?;
    let mut term =
        ratatui::Terminal::new(ratatui::backend::CrosstermBackend::new(std::io::stdout()))?;

    // Ok(code): the exit code dgent should return (0 on prefix-quit, the shell's otherwise).
    let result: Result<u32> = (|| loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status.exit_code());
        }
        // Drain shell output, bounded so a flood (e.g. `yes`) can't starve key handling.
        for _ in 0..64 {
            match out_rx.try_recv() {
                Ok(bytes) => parser.process(&bytes),
                Err(mpsc::TryRecvError::Empty) => break,
                // PTY EOF doesn't mean the shell exited (`exec cmd >/dev/null ...`): keep the UI
                // live and let try_wait above report the exit.
                Err(mpsc::TryRecvError::Disconnected) => break,
            }
        }
        if let Some(rx) = &turn {
            while let Ok(ev) = rx.try_recv() {
                match ev {
                    AgentEvent::Text(t) => panel.push(&t),
                    AgentEvent::ProposeCommand { id, command } => {
                        // One at a time: a newer proposal supersedes (denies) an unanswered one.
                        if let Some(old) = panel.pending.replace(ui::Proposal { id, command }) {
                            answer(&mut *backend, &mut panel, old, None);
                            // An open edit belonged to the old proposal.
                            if matches!(mode, Mode::Edit(_)) {
                                mode = Mode::Normal;
                            }
                        }
                    }
                    AgentEvent::Error(e) => panel.push(&format!("\n[error] {e}")),
                    AgentEvent::Done => {}
                }
            }
        }

        let area: Rect = term.size()?.into();
        let [pane, status] =
            Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(area);
        // The panel overlays the bottom of the pane rather than shrinking it: vt100 drops
        // rows on a height decrease, so resizing for the panel would erase shell output.
        let editing = match &mode {
            Mode::Edit(cmd) => Some(cmd.as_str()),
            _ => None,
        };
        // The panel grows by the command's rows so the transcript stays readable beside it.
        let panel_h = if !panel.visible() {
            0
        } else {
            (PANEL_HEIGHT + panel.prompt_rows(editing, pane.width)).min(pane.height)
        };
        let side = Rect {
            y: pane.bottom() - panel_h,
            height: panel_h,
            ..pane
        };
        if (pane.height, pane.width) != pane_size {
            pane_size = (pane.height, pane.width);
            parser.screen_mut().set_size(pane.height, pane.width);
            pty.master.resize(PtySize {
                rows: pane.height,
                cols: pane.width,
                pixel_width: 0,
                pixel_height: 0,
            })?;
        }

        term.draw(|f| {
            f.render_widget(PseudoTerminal::new(parser.screen()), pane);
            if panel_h > 0 {
                panel.render(f, side, backend.name(), editing);
            }
            let mut line = match &mode {
                Mode::Normal if panel.pending.is_some() => {
                    Line::from(" dgent | approve: y run, n deny, e edit")
                }
                Mode::Normal => Line::from(" dgent | C-] a: ask  C-] q: quit  C-] c: clear panel"),
                Mode::Prefix => {
                    Line::from(" dgent | prefix: a ask, q quit, c clear, k/j scroll, C-] literal")
                }
                Mode::Ask(q) => Line::from(format!(" ask> {q}")),
                Mode::Edit(_) => Line::from(" dgent | edit: Enter run, Esc back"),
            };
            if let Some(hint) = &panel.hint {
                line.push_span(format!("  [{hint}]"));
            }
            f.render_widget(
                Paragraph::new(line).style(Style::default().reversed()),
                status,
            );
        })?;

        if !event::poll(Duration::from_millis(16))? {
            continue;
        }
        match event::read()? {
            TermEvent::Key(key) if key.kind != KeyEventKind::Release => {
                // crossterm decodes the raw 0x1d byte as Ctrl-5, so accept both spellings.
                let prefix = key.modifiers.contains(KeyModifiers::CONTROL)
                    && matches!(key.code, KeyCode::Char(']' | '5'));
                mode = match std::mem::replace(&mut mode, Mode::Normal) {
                    Mode::Normal if prefix => Mode::Prefix,
                    // While a proposal waits, keys answer it instead of reaching the shell.
                    // Plain keys only: Ctrl-Y or Alt-N must not answer a proposal by accident.
                    Mode::Normal if panel.pending.is_some() => match key.code {
                        _ if key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                        {
                            Mode::Normal
                        }
                        KeyCode::Char('y') => {
                            let p = panel.pending.take().unwrap();
                            let cmd = p.command.clone();
                            answer(&mut *backend, &mut panel, p, Some((&mut writer, cmd)));
                            Mode::Normal
                        }
                        KeyCode::Char('n') => {
                            let p = panel.pending.take().unwrap();
                            answer(&mut *backend, &mut panel, p, None);
                            Mode::Normal
                        }
                        KeyCode::Char('e') => {
                            Mode::Edit(panel.pending.as_ref().unwrap().command.clone())
                        }
                        _ => Mode::Normal,
                    },
                    Mode::Normal => {
                        pty_write(
                            &mut writer,
                            &keys::encode(key, parser.screen().application_cursor()),
                        );
                        Mode::Normal
                    }
                    Mode::Prefix => match key.code {
                        KeyCode::Char('q') => return Ok(0),
                        KeyCode::Char('a') => Mode::Ask(String::new()),
                        KeyCode::Char('c') => {
                            panel.reset("");
                            Mode::Normal
                        }
                        KeyCode::Char('k') => {
                            panel.scroll(PANEL_HEIGHT as isize / 2);
                            Mode::Normal
                        }
                        KeyCode::Char('j') => {
                            panel.scroll(-(PANEL_HEIGHT as isize / 2));
                            Mode::Normal
                        }
                        _ if prefix => {
                            pty_write(&mut writer, b"\x1d");
                            Mode::Normal
                        }
                        _ => Mode::Normal,
                    },
                    Mode::Ask(q) => match edit_line(key, q) {
                        Ok(q) => Mode::Ask(q),
                        Err(None) => Mode::Normal,
                        Err(Some(prompt)) => {
                            // A new turn replaces the old one, so its open proposal can't be answered later.
                            if let Some(old) = panel.pending.take() {
                                answer(&mut *backend, &mut panel, old, None);
                            }
                            let context = tail(&parser.screen().contents(), CONTEXT_LINES);
                            panel.reset(&format!("> {prompt}\n"));
                            match backend.ask(Request { prompt, context }) {
                                Ok(rx) => turn = Some(rx),
                                Err(e) => panel.push(&format!("[error] {e}")),
                            }
                            Mode::Normal
                        }
                    },
                    Mode::Edit(cmd) => match edit_line(key, cmd) {
                        Ok(cmd) => Mode::Edit(cmd),
                        // Esc (or an emptied line) goes back to the y/n/e choice.
                        Err(None) => Mode::Normal,
                        Err(Some(cmd)) => {
                            let p = panel.pending.take().unwrap();
                            answer(&mut *backend, &mut panel, p, Some((&mut writer, cmd)));
                            Mode::Normal
                        }
                    },
                };
            }
            TermEvent::Paste(text) => {
                if let Mode::Ask(q) | Mode::Edit(q) = &mut mode {
                    q.push_str(&text);
                } else if panel.pending.is_some() {
                    // Input is held while a proposal waits; a pasted newline would run a command.
                } else if parser.screen().bracketed_paste() {
                    pty_write(&mut writer, format!("\x1b[200~{text}\x1b[201~").as_bytes());
                } else {
                    pty_write(&mut writer, text.as_bytes());
                }
            }
            _ => {}
        }
    })();

    execute!(
        std::io::stdout(),
        event::DisableBracketedPaste,
        terminal::LeaveAlternateScreen
    )?;
    terminal::disable_raw_mode()?;
    let _ = child.kill();
    match result? {
        0 => Ok(()),
        code => {
            drop(term); // restores the cursor; process::exit skips destructors
            std::process::exit(code as i32)
        }
    }
}

/// Write to the shell. Errors are dropped: once the PTY closes nothing reads input,
/// and shell exit is detected via `try_wait`, not write failures.
fn pty_write(w: &mut (impl Write + ?Sized), bytes: &[u8]) {
    let _ = w.write_all(bytes);
}

/// Answer a proposal: `run` is `Some((pty, command))` to type `command` into the pane,
/// `None` to deny it.
fn answer(
    backend: &mut dyn Backend,
    panel: &mut ui::Panel,
    p: ui::Proposal,
    run: Option<(&mut dyn Write, String)>,
) {
    let reply = match run {
        Some((pty, cmd)) => {
            pty_write(pty, format!("{cmd}\r").as_bytes());
            panel.push(&format!("\n[ran] {cmd}\n"));
            // shortcut: no output capture until W1's Pane::run; the backend gets a placeholder.
            ToolReply::Output("(sent to pane; output capture not wired yet)".into())
        }
        None => {
            panel.push(&format!("\n[denied] {}\n", p.command));
            ToolReply::Denied
        }
    };
    if let Err(e) = backend.reply(&p.id, reply) {
        panel.push(&format!("[error] {e}\n"));
    }
}

/// One-line editor for the ask and edit prompts. `Ok` keeps editing; `Err(Some(line))` on
/// Enter with text, `Err(None)` on Esc or Enter on a blank line.
fn edit_line(key: KeyEvent, mut q: String) -> Result<String, Option<String>> {
    match key.code {
        KeyCode::Esc => Err(None),
        KeyCode::Enter if q.trim().is_empty() => Err(None),
        KeyCode::Enter => Err(Some(q)),
        KeyCode::Backspace => {
            q.pop();
            Ok(q)
        }
        KeyCode::Char(c) => {
            q.push(c);
            Ok(q)
        }
        _ => Ok(q),
    }
}

fn tail(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.trim_end().lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

#[cfg(test)]
mod tests {
    #[test]
    fn tail_keeps_last_lines() {
        assert_eq!(super::tail("a\nb\nc\n\n", 2), "b\nc");
        assert_eq!(super::tail("", 2), "");
    }
}
