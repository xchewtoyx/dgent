//! Dgent: a terminal wrapper with an LLM overseer.
//!
//! Scaffold: one shell in a PTY, a status line, and an "ask" prompt wired to a
//! [`Backend`]. Prefix key is Ctrl-]; then `a` asks, `q` quits, Ctrl-] sends a
//! literal Ctrl-].

mod keys;

use std::io::{Read, Write};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{self, Event as TermEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::{execute, terminal};
use dgent_backend::{Backend, EchoBackend, Event as AgentEvent, Request};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
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

    let pty = native_pty_system()
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .context("open pty")?;
    let mut child = pty
        .slave
        .spawn_command(CommandBuilder::new(shell))
        .context("spawn shell")?;
    drop(pty.slave);
    let mut writer = pty.master.take_writer()?;
    let mut reader = pty.master.try_clone_reader()?;

    let (out_tx, out_rx) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        while let Ok(n @ 1..) = reader.read(&mut buf) {
            if out_tx.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });

    let mut parser = vt100::Parser::new(rows, cols, 10_000);
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

    let result: Result<()> = (|| loop {
        // Drain shell output; EOF on the reader thread closes the channel.
        loop {
            match out_rx.try_recv() {
                Ok(bytes) => parser.process(&bytes),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => return Ok(()),
            }
        }
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
        let panel_h = if panel.is_empty() { 0 } else { PANEL_HEIGHT };
        let [pane, side, status] = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(panel_h),
            Constraint::Length(1),
        ])
        .areas(area);
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
                let block = Block::default()
                    .borders(Borders::TOP)
                    .title(format!(" {} ", backend.name()));
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
                        writer
                            .write_all(&keys::encode(key, parser.screen().application_cursor()))?;
                        Mode::Normal
                    }
                    Mode::Prefix => match key.code {
                        KeyCode::Char('q') => return Ok(()),
                        KeyCode::Char('a') => Mode::Ask(String::new()),
                        KeyCode::Char('c') => {
                            panel.clear();
                            Mode::Normal
                        }
                        _ if prefix => {
                            writer.write_all(b"\x1d")?;
                            Mode::Normal
                        }
                        _ => Mode::Normal,
                    },
                    Mode::Ask(q) => ask_key(key, q, |prompt| {
                        let context = tail(&parser.screen().contents(), CONTEXT_LINES);
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
                } else if parser.screen().bracketed_paste() {
                    writer.write_all(format!("\x1b[200~{text}\x1b[201~").as_bytes())?;
                } else {
                    writer.write_all(text.as_bytes())?;
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
    result
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
