//! The agent panel: streamed backend text, scrollback, and the pending command proposal.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
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

        // Wrapped, never truncated: the user must see the whole command they approve.
        let (prompt, style) = match (&self.pending, editing) {
            (_, Some(cmd)) => (format!("edit> {cmd}"), Style::default().reversed()),
            (Some(p), None) => (format!("run? {}", p.command), Style::default().bold()),
            (None, None) => (String::new(), Style::default()),
        };
        let prompt = wrap(&prompt, inner.width as usize);
        let prompt_h = prompt.len().min(inner.height as usize);
        let [body, ask] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(prompt_h as u16)])
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
        // shortcut: a command taller than the panel shows only its tail; fine below ~500 chars.
        let shown: Vec<Line> = prompt[prompt.len() - prompt_h..]
            .iter()
            .map(|l| Line::from(l.as_str()))
            .collect();
        f.render_widget(Paragraph::new(shown).style(style), ask);
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
