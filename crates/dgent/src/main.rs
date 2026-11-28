//! Dgent: a terminal wrapper with an LLM overseer.
//!
//! Scaffold: one shell in a PTY, a status line, and an "ask" prompt wired to a
//! [`Backend`]. Prefix key is Ctrl-]; then `a` asks, `q` quits, Ctrl-] sends a
//! literal Ctrl-].

mod keys;
mod pane;

use std::sync::mpsc::Receiver;
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{self, Event as TermEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::{execute, terminal};
use dgent_backend::{Backend, EchoBackend, Event as AgentEvent, Request};
use pane::Pane;
use portable_pty::CommandBuilder;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use tui_term::widget::PseudoTerminal;

/// How many screen lines go to the backend with a question.
const CONTEXT_LINES: usize = 40;
const PANEL_HEIGHT: u16 = 8;

enum Mode {
    Normal,
    Prefix,
    Ask(String),
}

fn main() -> Result<()> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    let (cols, rows) = terminal::size()?;

    let mut command = CommandBuilder::new(shell);
    command.cwd(std::env::current_dir().context("resolve launch directory")?);
    let mut pane = Pane::spawn(command, rows, cols)?;

    let mut backend: Box<dyn Backend> = Box::new(EchoBackend);
    let mut turn: Option<Receiver<AgentEvent>> = None;
    let mut panel = String::new();
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
        if let Some(status) = pane.try_wait()? {
            return Ok(status.exit_code());
        }
        pane.pump();
        if let Some(rx) = &turn {
            while let Ok(ev) = rx.try_recv() {
                match ev {
                    AgentEvent::Text(t) => panel.push_str(&t),
                    AgentEvent::ProposeCommand { command, .. } => {
                        panel.push_str(&format!("\n[proposed] {command} (approval not wired yet)"))
                    }
                    AgentEvent::Error(e) => panel.push_str(&format!("\n[error] {e}")),
                    AgentEvent::Done => {}
                }
            }
        }

        let area: Rect = term.size()?.into();
        let [pane_area, status] =
            Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(area);
        // The panel overlays the bottom of the pane rather than shrinking it, so opening it
        // doesn't resize the shell (and scroll its output) every time.
        let panel_h = if panel.is_empty() {
            0
        } else {
            PANEL_HEIGHT.min(pane_area.height)
        };
        let side = Rect {
            y: pane_area.bottom() - panel_h,
            height: panel_h,
            ..pane_area
        };
        if (pane_area.height, pane_area.width) != pane_size {
            pane_size = (pane_area.height, pane_area.width);
            pane.resize(pane_area.height, pane_area.width)?;
        }

        term.draw(|f| {
            f.render_widget(PseudoTerminal::new(pane.screen()), pane_area);
            if panel_h > 0 {
                let block = Block::default()
                    .borders(Borders::TOP)
                    .title(format!(" {} ", backend.name()));
                f.render_widget(Clear, side);
                f.render_widget(
                    Paragraph::new(tail(&panel, PANEL_HEIGHT as usize - 1))
                        .wrap(Wrap { trim: false })
                        .block(block),
                    side,
                );
            }
            let line = match &mode {
                Mode::Normal => Line::from(" dgent | C-] a: ask  C-] q: quit  C-] c: clear panel"),
                Mode::Prefix => Line::from(" dgent | prefix: a ask, q quit, c clear, C-] literal"),
                Mode::Ask(q) => Line::from(format!(" ask> {q}")),
            };
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
                    Mode::Normal => {
                        pane.write(&keys::encode(key, pane.screen().application_cursor()));
                        Mode::Normal
                    }
                    Mode::Prefix => match key.code {
                        KeyCode::Char('q') => return Ok(0),
                        KeyCode::Char('a') => Mode::Ask(String::new()),
                        KeyCode::Char('c') => {
                            panel.clear();
                            Mode::Normal
                        }
                        _ if prefix => {
                            pane.write(b"\x1d");
                            Mode::Normal
                        }
                        _ => Mode::Normal,
                    },
                    Mode::Ask(q) => ask_key(key, q, |prompt| {
                        let context = tail(&pane.screen().contents(), CONTEXT_LINES);
                        panel = format!("> {prompt}\n");
                        match backend.ask(Request { prompt, context }) {
                            Ok(rx) => turn = Some(rx),
                            Err(e) => panel.push_str(&format!("[error] {e}")),
                        }
                    }),
                };
            }
            TermEvent::Paste(text) => {
                if let Mode::Ask(q) = &mut mode {
                    q.push_str(&text);
                } else if pane.screen().bracketed_paste() {
                    pane.write(format!("\x1b[200~{text}\x1b[201~").as_bytes());
                } else {
                    pane.write(text.as_bytes());
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
    pane.kill();
    match result? {
        0 => Ok(()),
        code => {
            drop(term); // restores the cursor; process::exit skips destructors
            std::process::exit(code as i32)
        }
    }
}

/// Line editing for the ask prompt. Calls `submit` on Enter.
fn ask_key(key: KeyEvent, mut q: String, submit: impl FnOnce(String)) -> Mode {
    match key.code {
        KeyCode::Esc => Mode::Normal,
        KeyCode::Enter if q.trim().is_empty() => Mode::Normal,
        KeyCode::Enter => {
            submit(q);
            Mode::Normal
        }
        KeyCode::Backspace => {
            q.pop();
            Mode::Ask(q)
        }
        KeyCode::Char(c) => {
            q.push(c);
            Mode::Ask(q)
        }
        _ => Mode::Ask(q),
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
