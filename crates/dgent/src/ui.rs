//! The agent panel: streamed backend text, scrollback, and the pending command proposal.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Style, Stylize};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

/// A command the backend wants typed into the pane, awaiting the user's y/n/e.
pub struct Proposal {
    pub id: String,
    pub command: String,
}

#[derive(Default)]
pub struct Panel {
    text: String,
    /// Wrapped lines scrolled up from the bottom; clamped on render.
    scroll: usize,
    pub pending: Option<Proposal>,
    /// Overseer hint shown as a status-line badge (set by the overseer, W8).
    pub hint: Option<String>,
}

impl Panel {
    pub fn push(&mut self, s: &str) {
        self.text.push_str(s);
    }

    /// Start a fresh transcript. A pending proposal is kept: it still needs an answer.
    pub fn reset(&mut self, s: &str) {
        self.text = s.into();
        self.scroll = 0;
    }

    pub fn visible(&self) -> bool {
        !self.text.is_empty() || self.pending.is_some()
    }

    /// Positive scrolls up (older text).
    pub fn scroll(&mut self, by: isize) {
        self.scroll = self.scroll.saturating_add_signed(by);
    }

    /// `editing` is the command being edited, shown in place of the proposal.
    pub fn render(&mut self, f: &mut Frame, area: Rect, title: &str, editing: Option<&str>) {
        let block = Block::default()
            .borders(Borders::TOP)
            .title(format!(" {title} "));
        let inner = block.inner(area);
        f.render_widget(Clear, area);
        f.render_widget(block, area);

        let prompt = match (&self.pending, editing) {
            (_, Some(cmd)) => Some(Line::from(format!(" edit> {cmd}")).reversed()),
            (Some(p), None) => Some(Line::from(vec![
                " run? ".bold(),
                p.command.clone().into(),
                "   y run  n deny  e edit".dim(),
            ])),
            (None, None) => None,
        };
        let [body, ask] = Layout::vertical([
            Constraint::Min(0),
            Constraint::Length(prompt.is_some() as u16),
        ])
        .areas(inner);

        let lines = wrap(self.text.trim_end(), body.width as usize);
        let h = body.height as usize;
        self.scroll = self.scroll.min(lines.len().saturating_sub(h));
        let end = lines.len() - self.scroll;
        let shown: Vec<Line> = lines[end.saturating_sub(h)..end]
            .iter()
            .map(|l| Line::from(l.as_str()))
            .collect();
        f.render_widget(Paragraph::new(shown), body);
        if let Some(p) = prompt {
            f.render_widget(Paragraph::new(p).style(Style::default()), ask);
        }
    }
}

/// Hard-wrap each line at `width` chars so scrolling counts screen rows exactly.
/// shortcut: counts chars, not display width; wide (CJK/emoji) text can overflow a row.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    for line in text.lines() {
        let chars: Vec<char> = line.chars().collect();
        if chars.is_empty() {
            out.push(String::new());
        }
        out.extend(chars.chunks(width).map(|c| c.iter().collect()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::wrap;

    #[test]
    fn wrap_splits_long_lines_and_keeps_blank_ones() {
        assert_eq!(wrap("abcde\n\nf", 2), ["ab", "cd", "e", "", "f"]);
        assert!(wrap("", 5).is_empty());
    }
}
