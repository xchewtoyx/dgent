//! The contract between Dgent and an LLM backend.
//!
//! A backend is a long-lived conversation with some agent (Claude Code CLI,
//! Codex app-server, ...). Dgent sends it a question plus a bounded slice of
//! terminal context. The backend streams back text and, when it wants to act,
//! *proposes* a command. Dgent never lets the backend execute anything itself:
//! the user approves the proposal, Dgent types it into the pane, and hands the
//! captured output back with [`Backend::reply`].

mod codex;
pub use codex::CodexBackend;

use std::sync::mpsc::{self, Receiver};

use anyhow::Result;

/// One question to the backend.
#[derive(Debug, Clone, Default)]
pub struct Request {
    /// What the user asked, or an automatic trigger such as "last command failed".
    pub prompt: String,
    /// Bounded, already-redacted terminal context (screen text, recent commands).
    pub context: String,
}

/// Streamed back from the backend while a turn runs. A turn ends with `Done` or `Error`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Assistant text, possibly a partial chunk.
    Text(String),
    /// The backend wants this command run in the active pane. Answer with [`Backend::reply`].
    ProposeCommand {
        id: String,
        command: String,
    },
    Done,
    Error(String),
}

/// Dgent's answer to a [`Event::ProposeCommand`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolReply {
    Denied,
    /// The command ran; this is its captured (redacted) output.
    Output(String),
}

pub trait Backend: Send {
    fn name(&self) -> &str;
    /// Start a turn. Events arrive on the receiver until `Done` or `Error`.
    fn ask(&mut self, req: Request) -> Result<Receiver<Event>>;
    /// Answer a pending command proposal from the current turn.
    fn reply(&mut self, id: &str, reply: ToolReply) -> Result<()>;
}

/// Stand-in backend so the TUI can be exercised without any agent installed.
#[derive(Default)]
pub struct EchoBackend;

impl Backend for EchoBackend {
    fn name(&self) -> &str {
        "echo"
    }

    fn ask(&mut self, req: Request) -> Result<Receiver<Event>> {
        let (tx, rx) = mpsc::channel();
        let lines = req.context.lines().count();
        tx.send(Event::Text(format!(
            "echo: {} ({lines} lines of context)",
            req.prompt
        )))?;
        tx.send(Event::Done)?;
        Ok(rx)
    }

    fn reply(&mut self, _id: &str, _reply: ToolReply) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn echo_streams_text_then_done() {
        let mut b = EchoBackend;
        let rx = b
            .ask(Request {
                prompt: "hi".into(),
                context: "a\nb".into(),
            })
            .unwrap();
        let events: Vec<_> = rx.iter().collect();
        assert_eq!(
            events,
            vec![
                Event::Text("echo: hi (2 lines of context)".into()),
                Event::Done
            ]
        );
    }
}
