//! Overseer policy: decides when the LLM is involved and what context it gets.
//!
//! Light by default. The LLM is called only on an explicit ask or `C-] e`. A
//! detected failure just yields a status-line badge, unless auto mode is on, and
//! then at most once per `min_interval`. Every [`Request`] is built by
//! [`Overseer::build`], which redacts the full text before truncating it, so a
//! secret cannot slip through by being cut in half.

use std::time::{Duration, Instant};

use dgent_backend::Request;

/// Context budget and auto-mode rate limit.
#[derive(Debug, Clone)]
pub struct Limits {
    pub max_lines: usize,
    pub max_bytes: usize,
    /// Minimum time between automatic (unasked) requests.
    pub min_interval: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_lines: 40,
            max_bytes: 8 * 1024,
            min_interval: Duration::from_secs(60),
        }
    }
}

/// A command that exited nonzero, as reported by failure detection.
#[derive(Debug, Clone)]
pub struct Failure {
    pub command: String,
    pub output: String,
    pub exit: i32,
}

/// What to do about a failure.
#[derive(Debug)]
pub enum Reaction {
    /// Show this on the status line; no LLM call.
    Badge(String),
    /// Auto mode: send this request now.
    Ask(Request),
}

pub struct Overseer {
    limits: Limits,
    redact: fn(&str) -> String,
    auto: bool,
    last_auto: Option<Instant>,
    last_failure: Option<Failure>,
}

impl Overseer {
    pub fn new(limits: Limits, redact: fn(&str) -> String, auto: bool) -> Self {
        Self {
            limits,
            redact,
            auto,
            last_auto: None,
            last_failure: None,
        }
    }

    /// `C-] a`: the user's question plus the visible screen tail.
    pub fn ask(&self, question: &str, screen: &str) -> Request {
        self.build(question, screen)
    }

    /// Record a failure. Returns a badge, or a request if auto mode is on and not rate limited.
    pub fn on_failure(&mut self, failure: Failure, now: Instant) -> Reaction {
        let exit = failure.exit;
        self.last_failure = Some(failure);
        let due = self
            .last_auto
            .is_none_or(|t| now.duration_since(t) >= self.limits.min_interval);
        if self.auto && due {
            self.last_auto = Some(now);
            if let Some(req) = self.explain_last() {
                return Reaction::Ask(req);
            }
        }
        Reaction::Badge(format!("⚠ exit {exit}, C-] e to explain"))
    }

    /// `C-] e`: explain the most recent failure, if there is one.
    pub fn explain_last(&self) -> Option<Request> {
        let f = self.last_failure.as_ref()?;
        // The command goes in the prompt so a long output tail can't push it out of the budget.
        // Redact before capping, as in `build`. Keep the head: the invocation matters most.
        let command = head(&(self.redact)(&f.command), 5, 512);
        let prompt = format!(
            "`{command}` exited with status {}. Briefly explain why and suggest a fix. Its output follows.",
            f.exit
        );
        Some(self.build(&prompt, &f.output))
    }

    /// The only place a `Request` is made: redact everything, then apply the budget.
    fn build(&self, prompt: &str, context: &str) -> Request {
        let context = (self.redact)(context);
        Request {
            prompt: (self.redact)(prompt),
            context: truncate(&context, self.limits.max_lines, self.limits.max_bytes),
        }
    }
}

/// Keep the first `max_lines` lines and `max_bytes` bytes, ending in `…` if anything was cut.
fn head(s: &str, max_lines: usize, max_bytes: usize) -> String {
    let mut out = s.lines().take(max_lines).collect::<Vec<_>>().join("\n");
    let mut end = out.len().min(max_bytes);
    while !out.is_char_boundary(end) {
        end -= 1;
    }
    out.truncate(end);
    if out.len() < s.trim_end().len() {
        out.push('…');
    }
    out
}

/// Keep the last `max_lines` lines, then the last `max_bytes` bytes (the end is where errors are).
/// A `[truncated]` marker line is added when bytes are cut, counted inside both limits.
fn truncate(s: &str, max_lines: usize, max_bytes: usize) -> String {
    const MARKER: &str = "[truncated]\n";
    let s = super::tail(s, max_lines);
    if s.len() <= max_bytes {
        return s;
    }
    // Leave room for the marker unless the budget is too small to hold it plus some text.
    let marker = if max_bytes > MARKER.len() && max_lines > 1 {
        MARKER
    } else {
        ""
    };
    let mut start = s.len() - (max_bytes - marker.len());
    while !s.is_char_boundary(start) {
        start += 1;
    }
    let lines = max_lines - usize::from(!marker.is_empty());
    format!("{marker}{}", super::tail(&s[start..], lines))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mask(s: &str) -> String {
        s.replace("hunter2", "[REDACTED]")
    }

    fn overseer(auto: bool) -> Overseer {
        Overseer::new(Limits::default(), mask, auto)
    }

    fn fail(output: &str) -> Failure {
        Failure {
            command: "make".into(),
            output: output.into(),
            exit: 2,
        }
    }

    #[test]
    fn budget_keeps_tail_lines_and_bytes() {
        let text: String = (0..100).map(|i| format!("line {i}\n")).collect();
        assert_eq!(truncate(&text, 3, 1000), "line 97\nline 98\nline 99");
        assert_eq!(truncate(&text, 3, 19), "[truncated]\nline 99");
        // Too small for the marker: plain suffix.
        assert_eq!(truncate(&text, 3, 7), "line 99");
        // Never splits a multi-byte char.
        assert_eq!(truncate("ééé", 10, 3), "é");
        // Marker counts against both limits even when every line survives the byte cut.
        let wide: String = (0..40).map(|_| format!("{}\n", "x".repeat(205))).collect();
        let t = truncate(&wide, 40, 8192);
        assert!(t.starts_with("[truncated]\n"));
        assert!(t.len() <= 8192 && t.lines().count() <= 40);
    }

    #[test]
    fn requests_respect_budget() {
        let screen = "x\n".repeat(1000);
        let req = overseer(false).ask("why", &screen);
        assert_eq!(req.context.lines().count(), Limits::default().max_lines);
        assert!(req.context.len() <= Limits::default().max_bytes);
    }

    #[test]
    fn redaction_runs_before_every_request() {
        let mut o = overseer(true);
        let ask = o.ask("is hunter2 ok", "pw=hunter2");
        assert_eq!(ask.prompt, "is [REDACTED] ok");
        assert_eq!(ask.context, "pw=[REDACTED]");

        let Reaction::Ask(auto) = o.on_failure(fail("bad hunter2"), Instant::now()) else {
            panic!("auto mode should ask");
        };
        assert!(!auto.context.contains("hunter2"));
        assert!(!o.explain_last().unwrap().context.contains("hunter2"));
    }

    #[test]
    fn redaction_sees_text_before_truncation() {
        // A secret straddling the byte cut must still be caught, so redact first.
        let o = Overseer::new(
            Limits {
                max_bytes: 4,
                ..Limits::default()
            },
            mask,
            false,
        );
        assert!(!o.ask("q", "aaaa hunter2").context.contains("ter2"));
    }

    #[test]
    fn failure_without_auto_is_only_a_badge() {
        let mut o = overseer(false);
        assert!(o.explain_last().is_none());
        assert!(matches!(
            o.on_failure(fail("oops"), Instant::now()),
            Reaction::Badge(b) if b.contains("exit 2")
        ));
        let req = o.explain_last().unwrap();
        assert!(req.prompt.contains("`make` exited with status 2"));
        assert_eq!(req.context, "oops");

        o.on_failure(
            Failure {
                command: "y".repeat(10_000),
                ..fail("")
            },
            Instant::now(),
        );
        assert!(o.explain_last().unwrap().prompt.len() < 1024);
        // A long heredoc keeps its invocation and is marked as cut.
        o.on_failure(
            Failure {
                command: format!("psql -f - <<EOF\n{}EOF", "select 1;\n".repeat(20)),
                ..fail("")
            },
            Instant::now(),
        );
        let p = o.explain_last().unwrap().prompt;
        assert!(p.contains("`psql -f - <<EOF\n") && p.contains("…`"));
    }

    #[test]
    fn auto_mode_is_rate_limited() {
        let mut o = overseer(true);
        let t0 = Instant::now();
        assert!(matches!(o.on_failure(fail("a"), t0), Reaction::Ask(_)));
        let soon = t0 + Duration::from_secs(10);
        assert!(matches!(o.on_failure(fail("b"), soon), Reaction::Badge(_)));
        // The badge still points C-] e at the newest failure.
        assert!(o.explain_last().unwrap().context.contains('b'));
        let later = t0 + Limits::default().min_interval;
        assert!(matches!(o.on_failure(fail("c"), later), Reaction::Ask(_)));
    }
}
