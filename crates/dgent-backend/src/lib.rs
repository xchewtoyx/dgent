//! The contract between Dgent and an LLM backend.
//!
//! A backend is a long-lived conversation with some agent (Claude Code CLI,
//! Codex app-server, ...). Dgent sends it a question plus a bounded slice of
//! terminal context. The backend streams back text and, when it wants to act,
//! *proposes* a command. Dgent never lets the backend execute anything itself:
//! the user approves the proposal, Dgent types it into the pane, and hands the
//! captured output back with [`Backend::reply`].

use std::sync::mpsc::{self, Receiver, Sender};

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
/// A prompt `run <cmd>` makes it propose `<cmd>` and finish the turn on [`Backend::reply`].
#[derive(Default)]
pub struct EchoBackend {
    pending: Option<Sender<Event>>,
}

impl Backend for EchoBackend {
    fn name(&self) -> &str {
        "echo"
    }

    fn ask(&mut self, req: Request) -> Result<Receiver<Event>> {
        let (tx, rx) = mpsc::channel();
        if let Some(command) = req.prompt.strip_prefix("run ") {
            tx.send(Event::ProposeCommand {
                id: "echo-1".into(),
                command: command.into(),
            })?;
            self.pending = Some(tx);
            return Ok(rx);
        }
        let lines = req.context.lines().count();
        tx.send(Event::Text(format!(
            "echo: {} ({lines} lines of context)",
            req.prompt
        )))?;
        tx.send(Event::Done)?;
        Ok(rx)
    }

    fn reply(&mut self, _id: &str, reply: ToolReply) -> Result<()> {
        if let Some(tx) = self.pending.take() {
            let text = match reply {
                ToolReply::Denied => "echo: denied".into(),
                ToolReply::Output(out) => format!("echo: ran ({out})"),
            };
            // The UI may have dropped this turn already; nothing to report then.
            let _ = tx.send(Event::Text(text));
            let _ = tx.send(Event::Done);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn echo_streams_text_then_done() {
        let mut b = EchoBackend::default();
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

    #[test]
    fn echo_proposes_run_and_finishes_on_reply() {
        let mut b = EchoBackend::default();
        let rx = b
            .ask(Request {
                prompt: "run echo approved".into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            rx.recv().unwrap(),
            Event::ProposeCommand {
                id: "echo-1".into(),
                command: "echo approved".into()
            }
        );
        b.reply("echo-1", ToolReply::Denied).unwrap();
        let rest: Vec<_> = rx.iter().collect();
        assert_eq!(rest, vec![Event::Text("echo: denied".into()), Event::Done]);
    }
}
