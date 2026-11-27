//! Claude Code CLI backend: one long-lived `claude -p` process speaking
//! stream-json on stdin/stdout, authenticated by the user's `claude login`
//! subscription. Never pass `--bare` (it ignores OAuth and needs an API key).

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::{Backend, Event, Request, ToolReply};

const SYSTEM_PROMPT: &str = "You are Dgent, an assistant watching the user's terminal (often an ssh session). \
Answer briefly and concretely. You cannot run anything yourself; when a command would help, \
propose it with the run_in_pane tool if available, otherwise say what to run.";

/// How to start `claude`.
#[derive(Debug, Clone)]
pub struct ClaudeConfig {
    /// Path or name of the `claude` binary.
    pub program: String,
    /// Command that starts Dgent's stdio MCP bridge (`dgent mcp`, W4). `None` gives Claude no tools at all.
    pub mcp_command: Option<(String, Vec<String>)>,
}

impl Default for ClaudeConfig {
    fn default() -> Self {
        Self {
            program: "claude".into(),
            mcp_command: None,
        }
    }
}

impl ClaudeConfig {
    fn args(&self) -> Vec<String> {
        let mut a: Vec<String> = [
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--include-partial-messages",
            "--no-session-persistence",
            "--tools",
            "",
            "--strict-mcp-config",
            // Only ~/.claude settings: a project's .claude/ hooks would run unapproved in print mode.
            "--setting-sources",
            "user",
            "--append-system-prompt",
            SYSTEM_PROMPT,
        ]
        .map(String::from)
        .into();
        if let Some((cmd, args)) = &self.mcp_command {
            let cfg = json!({"mcpServers": {"dgent": {"command": cmd, "args": args}}});
            a.extend([
                "--mcp-config".into(),
                cfg.to_string(),
                "--allowedTools".into(),
                "mcp__dgent__run_in_pane,mcp__dgent__read_screen".into(),
            ]);
        }
        a
    }
}

type Turn = Arc<Mutex<Option<Sender<Event>>>>;
type ReplySink = Box<dyn FnMut(&str, ToolReply) -> Result<()> + Send>;

pub struct ClaudeBackend {
    child: Child,
    stdin: ChildStdin,
    turn: Turn,
    reply_sink: Option<ReplySink>,
}

/// Lets the tool bridge (W4) inject `ProposeCommand` into whichever turn is running.
#[derive(Clone)]
pub struct Proposer(Turn);

impl Proposer {
    pub fn propose(&self, id: String, command: String) -> bool {
        match &*self.0.lock().unwrap() {
            Some(tx) => tx.send(Event::ProposeCommand { id, command }).is_ok(),
            None => false,
        }
    }
}

impl ClaudeBackend {
    pub fn spawn(cfg: &ClaudeConfig) -> Result<Self> {
        let mut child = Command::new(&cfg.program)
            .args(cfg.args())
            // Subscription login only: these would take precedence over `claude login`.
            .env_remove("ANTHROPIC_API_KEY")
            .env_remove("ANTHROPIC_AUTH_TOKEN")
            .env_remove("CLAUDE_CODE_USE_BEDROCK")
            .env_remove("CLAUDE_CODE_USE_VERTEX")
            .env_remove("CLAUDE_CODE_USE_FOUNDRY")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("starting {}", cfg.program))?;
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let mut stderr = child.stderr.take().unwrap();
        let turn: Turn = Arc::default();

        let stderr_buf = Arc::new(Mutex::new(String::new()));
        let buf = stderr_buf.clone();
        thread::spawn(move || {
            let mut s = String::new();
            let _ = stderr.read_to_string(&mut s);
            *buf.lock().unwrap() = s;
        });

        let t = turn.clone();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let Some(ev) = parse_line(&line) else {
                    continue;
                };
                let end = matches!(ev, Event::Done | Event::Error(_));
                let mut guard = t.lock().unwrap();
                if let Some(tx) = guard.as_ref() {
                    let _ = tx.send(ev);
                }
                if end {
                    *guard = None;
                }
            }
            // Give stderr a moment to be collected so the error says why claude died.
            thread::sleep(std::time::Duration::from_millis(100));
            let why = stderr_buf.lock().unwrap().trim().to_string();
            if let Some(tx) = t.lock().unwrap().take() {
                let _ = tx.send(Event::Error(format!("claude exited: {why}")));
            }
        });

        Ok(Self {
            child,
            stdin,
            turn,
            reply_sink: None,
        })
    }

    pub fn proposer(&self) -> Proposer {
        Proposer(self.turn.clone())
    }

    /// Where `reply` sends the user's decision (W4's socket listener).
    pub fn set_reply_sink(&mut self, sink: ReplySink) {
        self.reply_sink = Some(sink);
    }
}

impl Drop for ClaudeBackend {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Backend for ClaudeBackend {
    fn name(&self) -> &str {
        "claude"
    }

    fn ask(&mut self, req: Request) -> Result<Receiver<Event>> {
        let (tx, rx) = mpsc::channel();
        {
            let mut turn = self.turn.lock().unwrap();
            if turn.is_some() {
                bail!("claude is still answering the previous question");
            }
            *turn = Some(tx);
        }
        let line = user_message(&req).to_string() + "\n";
        if let Err(e) = self
            .stdin
            .write_all(line.as_bytes())
            .and_then(|_| self.stdin.flush())
        {
            *self.turn.lock().unwrap() = None;
            return Err(e).context("writing to claude");
        }
        Ok(rx)
    }

    fn reply(&mut self, id: &str, reply: ToolReply) -> Result<()> {
        match &mut self.reply_sink {
            Some(sink) => sink(id, reply),
            None => bail!("no tool bridge connected"),
        }
    }
}

fn user_message(req: &Request) -> Value {
    let mut text = req.prompt.clone();
    if !req.context.is_empty() {
        text += &format!("\n\nTerminal context:\n```\n{}\n```", req.context);
    }
    json!({"type": "user", "message": {"role": "user", "content": text}})
}

/// Map one stream-json line to an event. Full `assistant` messages are ignored
/// because their text already arrived as `text_delta`s.
fn parse_line(line: &str) -> Option<Event> {
    let v: Value = serde_json::from_str(line).ok()?;
    match v["type"].as_str()? {
        "stream_event" if v["event"]["delta"]["type"] == "text_delta" => Some(Event::Text(
            v["event"]["delta"]["text"].as_str()?.to_string(),
        )),
        // Backstop for credentials set in ~/.claude settings: "none" means the subscription login.
        // shortcut: the turn keeps running in claude after this error; restart the backend if it fires.
        "system"
            if v["subtype"] == "init"
                && !matches!(v["apiKeySource"].as_str(), None | Some("none")) =>
        {
            Some(Event::Error(format!(
                "claude is using {} instead of your subscription login",
                v["apiKeySource"]
            )))
        }
        "result" if v["is_error"] == true => {
            let errors: Vec<&str> = v["errors"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            let msg = match v["result"].as_str() {
                Some(r) if !r.is_empty() => r.to_string(),
                _ if !errors.is_empty() => errors.join("; "),
                _ => v["subtype"].as_str().unwrap_or("claude error").to_string(),
            };
            Some(Event::Error(msg))
        }
        "result" => Some(Event::Done),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_fixture(s: &str) -> Vec<Event> {
        s.lines().filter_map(parse_line).collect()
    }

    #[test]
    fn recorded_turn_maps_to_text_then_done() {
        let events = parse_fixture(include_str!("../tests/fixtures/claude_hello.jsonl"));
        assert_eq!(events, vec![Event::Text("hello there".into()), Event::Done]);
    }

    #[test]
    fn error_result_maps_to_error() {
        let events = parse_fixture(include_str!("../tests/fixtures/claude_not_logged_in.jsonl"));
        assert_eq!(
            events,
            vec![Event::Error("Not logged in · Please run /login".into())]
        );
    }

    #[test]
    fn never_bare_and_no_builtin_tools() {
        let a = ClaudeConfig::default().args();
        assert!(!a.iter().any(|x| x == "--bare"));
        let i = a.iter().position(|x| x == "--tools").unwrap();
        assert_eq!(a[i + 1], "");
        assert!(!a.iter().any(|x| x == "--mcp-config"));
    }

    #[test]
    fn context_goes_into_user_message() {
        let v = user_message(&Request {
            prompt: "why?".into(),
            context: "$ false".into(),
        });
        assert_eq!(v["type"], "user");
        let text = v["message"]["content"].as_str().unwrap();
        assert!(text.starts_with("why?") && text.contains("$ false"));
    }

    /// A fake `claude` (a shell script echoing the fixture) exercises spawn, ask and the reader thread.
    #[test]
    fn spawned_process_streams_a_turn_and_reports_exit() {
        let dir = std::env::temp_dir().join(format!("dgent-fake-claude-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fixture = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/claude_hello.jsonl"
        );
        let script = dir.join("claude");
        std::fs::write(
            &script,
            format!("#!/bin/sh\nread line\ncat '{fixture}'\nread line\necho boom >&2\n"),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let mut b = ClaudeBackend::spawn(&ClaudeConfig {
            program: script.to_string_lossy().into(),
            mcp_command: None,
        })
        .unwrap();
        let rx = b
            .ask(Request {
                prompt: "hi".into(),
                context: String::new(),
            })
            .unwrap();
        assert_eq!(
            rx.iter().collect::<Vec<_>>(),
            vec![Event::Text("hello there".into()), Event::Done]
        );

        let rx = b
            .ask(Request {
                prompt: "again".into(),
                context: String::new(),
            })
            .unwrap();
        let events: Vec<_> = rx.iter().collect();
        assert!(
            matches!(&events[..], [Event::Error(e)] if e.contains("boom")),
            "{events:?}"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn proposer_reaches_running_turn_and_reply_hits_sink() {
        let turn: Turn = Arc::default();
        let (tx, rx) = mpsc::channel();
        *turn.lock().unwrap() = Some(tx);
        assert!(Proposer(turn.clone()).propose("1".into(), "df -h".into()));
        assert_eq!(
            rx.recv().unwrap(),
            Event::ProposeCommand {
                id: "1".into(),
                command: "df -h".into()
            }
        );
        *turn.lock().unwrap() = None;
        assert!(!Proposer(turn).propose("2".into(), "ls".into()));
    }

    #[test]
    fn reply_forwards_id_and_decision_to_sink() {
        let (tx, rx) = mpsc::channel();
        let mut b = ClaudeBackend::spawn(&ClaudeConfig {
            program: "cat".into(),
            mcp_command: None,
        })
        .unwrap();
        assert!(b.reply("1", ToolReply::Denied).is_err());
        b.set_reply_sink(Box::new(move |id, r| Ok(tx.send((id.to_string(), r))?)));
        b.reply("7", ToolReply::Output("ok".into())).unwrap();
        assert_eq!(
            rx.recv().unwrap(),
            ("7".into(), ToolReply::Output("ok".into()))
        );
    }

    #[test]
    fn errors_array_and_non_subscription_auth_surface() {
        assert_eq!(
            parse_line(
                r#"{"type":"result","subtype":"error_during_execution","is_error":true,"errors":["bad settings.json"]}"#
            ),
            Some(Event::Error("bad settings.json".into()))
        );
        assert_eq!(
            parse_line(r#"{"type":"system","subtype":"init","apiKeySource":"none"}"#),
            None
        );
        assert!(matches!(
            parse_line(r#"{"type":"system","subtype":"init","apiKeySource":"apiKeyHelper"}"#),
            Some(Event::Error(e)) if e.contains("apiKeyHelper")
        ));
    }
}
