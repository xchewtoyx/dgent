//! Codex backend over `codex app-server`: JSON-RPC 2.0, one JSON object per
//! line on stdio. Method and field names were checked against
//! `codex app-server generate-json-schema` from codex-cli 0.172.0.
//!
//! Auth is the `codex login` (ChatGPT subscription) credentials; API-key login
//! is refused. Codex's own shell and other acting tools are disabled, the
//! thread is read-only sandboxed, and any exec/patch approval it still asks
//! for is declined. Its only way to act is the `dgent` MCP server (`dgent mcp`),
//! whose `run_in_pane` calls reach the user through Dgent's tool bridge, so
//! this backend never emits [`Event::ProposeCommand`] itself.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

use crate::{Backend, Event, Request, ToolReply};

const INSTRUCTIONS: &str = "You are Dgent, an assistant watching the user's terminal. \
You cannot run commands yourself. To run one, call the dgent run_in_pane tool: the user \
approves it and it runs in their terminal, which may be an ssh session on another host. \
Use read_screen to see the terminal. Keep answers short.";

/// Built-in Codex tools that could act outside Dgent's pane.
const DISABLED_FEATURES: &[&str] = &[
    "shell_tool",
    "unified_exec",
    "apps",
    "plugins",
    "browser_use",
    "in_app_browser",
    "computer_use",
    "multi_agent",
];

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

type Writer = Arc<Mutex<Box<dyn Write + Send>>>;
type OnResponse = Box<dyn FnOnce(Result<Value, String>) + Send>;

#[derive(Default)]
struct Shared {
    pending: HashMap<u64, OnResponse>,
    /// The running turn's event channel; cleared once it gets `Done` or `Error`.
    turn: Option<Sender<Event>>,
}

pub struct CodexBackend {
    writer: Writer,
    shared: Arc<Mutex<Shared>>,
    next_id: u64,
    thread_id: String,
    child: Option<Child>,
}

impl CodexBackend {
    /// Spawn `<codex_bin> app-server` and start one thread (one per pane).
    /// `dgent_exe` is the binary Codex runs as `<dgent_exe> mcp` for the tool
    /// bridge; `None` leaves Codex with no tools at all.
    pub fn spawn(codex_bin: &str, dgent_exe: Option<&Path>) -> Result<Self> {
        let mut cmd = Command::new(codex_bin);
        cmd.arg("app-server");
        for f in DISABLED_FEATURES {
            cmd.args(["--disable", f]);
        }
        if let Some(exe) = dgent_exe {
            // A JSON string literal is also a valid TOML basic string.
            let exe = Value::String(exe.to_string_lossy().into_owned());
            cmd.arg("-c")
                .arg(format!("mcp_servers.dgent.command={exe}"))
                .args(["-c", "mcp_servers.dgent.args=[\"mcp\"]"]);
        }
        cmd.args(["-c", "forced_login_method=\"chatgpt\""])
            .env_remove("OPENAI_API_KEY")
            .env_remove("CODEX_API_KEY")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Codex writes logs to stderr; keep them off the TUI.
            .stderr(Stdio::null());
        let mut child = cmd
            .spawn()
            .with_context(|| format!("starting `{codex_bin} app-server`"))?;
        let stdin = child.stdin.take().expect("piped");
        let stdout = child.stdout.take().expect("piped");
        let mut b = match Self::start(stdout, stdin) {
            Ok(b) => b,
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e);
            }
        };
        b.child = Some(child);
        Ok(b)
    }

    /// Run the handshake over an existing transport (the child's pipes, or a fake in tests).
    fn start(
        reader: impl Read + Send + 'static,
        writer: impl Write + Send + 'static,
    ) -> Result<Self> {
        let writer: Writer = Arc::new(Mutex::new(Box::new(writer)));
        let shared = Arc::new(Mutex::new(Shared::default()));
        {
            let (writer, shared) = (writer.clone(), shared.clone());
            thread::spawn(move || read_loop(reader, &writer, &shared));
        }
        let mut b = Self {
            writer,
            shared,
            next_id: 0,
            thread_id: String::new(),
            child: None,
        };
        b.call_wait(
            "initialize",
            json!({"clientInfo": {"name": "dgent", "version": env!("CARGO_PKG_VERSION")}}),
        )?;
        b.send(&json!({"method": "initialized"}))?;
        let started = b.call_wait(
            "thread/start",
            json!({
                "sandbox": "read-only",
                "approvalPolicy": "untrusted",
                "ephemeral": true,
                "developerInstructions": INSTRUCTIONS,
            }),
        )?;
        b.thread_id = started["thread"]["id"]
            .as_str()
            .ok_or_else(|| anyhow!("thread/start returned no thread id"))?
            .to_owned();
        Ok(b)
    }

    fn send(&self, msg: &Value) -> Result<()> {
        let mut w = self.writer.lock().unwrap();
        writeln!(w, "{msg}")?;
        w.flush()?;
        Ok(())
    }

    fn call(&mut self, method: &str, params: Value, on_response: OnResponse) -> Result<()> {
        let id = self.next_id;
        self.next_id += 1;
        self.shared.lock().unwrap().pending.insert(id, on_response);
        self.send(&json!({"id": id, "method": method, "params": params}))
    }

    fn call_wait(&mut self, method: &str, params: Value) -> Result<Value> {
        let (tx, rx) = mpsc::channel();
        self.call(
            method,
            params,
            Box::new(move |r| {
                let _ = tx.send(r);
            }),
        )?;
        match rx.recv_timeout(HANDSHAKE_TIMEOUT) {
            Ok(r) => r.map_err(|e| anyhow!("codex {method}: {e}")),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(anyhow!("codex {method}: timed out")),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err(anyhow!("codex app-server exited during {method}"))
            }
        }
    }
}

impl Backend for CodexBackend {
    fn name(&self) -> &str {
        "codex"
    }

    fn ask(&mut self, req: Request) -> Result<Receiver<Event>> {
        let (tx, rx) = mpsc::channel();
        {
            let mut s = self.shared.lock().unwrap();
            // One turn at a time, so a late event from an old turn can't reach a new one.
            if s.turn.is_some() {
                bail!("codex is still answering the previous question");
            }
            s.turn = Some(tx);
        }
        let text = if req.context.is_empty() {
            req.prompt
        } else {
            format!("{}\n\nTerminal context:\n{}", req.prompt, req.context)
        };
        let shared = self.shared.clone();
        self.call(
            "turn/start",
            json!({"threadId": self.thread_id, "input": [{"type": "text", "text": text}]}),
            Box::new(move |r| {
                if let Err(e) = r {
                    // No turn started, so no turn/completed will end it.
                    if let Some(tx) = shared.lock().unwrap().turn.take() {
                        let _ = tx.send(Event::Error(e));
                    }
                }
            }),
        )?;
        Ok(rx)
    }

    /// Proposals come from the `dgent mcp` bridge, which takes the reply itself.
    fn reply(&mut self, _id: &str, _reply: ToolReply) -> Result<()> {
        Ok(())
    }
}

impl Drop for CodexBackend {
    fn drop(&mut self) {
        if let Some(c) = &mut self.child {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

fn read_loop(reader: impl Read, writer: &Writer, shared: &Mutex<Shared>) {
    for line in BufReader::new(reader).lines() {
        let Ok(line) = line else { break };
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        match (msg.get("method").and_then(Value::as_str), msg.get("id")) {
            (Some(method), Some(id)) => {
                let reply = match server_request_result(method, &msg["params"]) {
                    Some(result) => json!({"id": id, "result": result}),
                    None => {
                        json!({"id": id, "error": {"code": -32601, "message": "not supported by dgent"}})
                    }
                };
                let mut w = writer.lock().unwrap();
                let _ = writeln!(w, "{reply}").and_then(|_| w.flush());
            }
            (Some(method), None) => {
                let Some(ev) = notification_event(method, &msg["params"]) else {
                    continue;
                };
                let mut s = shared.lock().unwrap();
                let ends = matches!(ev, Event::Done | Event::Error(_));
                if let Some(tx) = &s.turn {
                    let _ = tx.send(ev);
                }
                if ends {
                    s.turn = None;
                }
            }
            (None, Some(id)) => {
                let Some(cb) = id
                    .as_u64()
                    .and_then(|id| shared.lock().unwrap().pending.remove(&id))
                else {
                    continue;
                };
                cb(match msg.get("error") {
                    Some(e) => Err(e["message"].as_str().unwrap_or("error").to_owned()),
                    None => Ok(msg["result"].clone()),
                });
            }
            (None, None) => {}
        }
    }
    // The app-server is gone: fail the running turn and anything still waiting.
    let mut s = shared.lock().unwrap();
    if let Some(tx) = s.turn.take() {
        let _ = tx.send(Event::Error("codex app-server exited".into()));
    }
    s.pending.clear();
}

/// Map a server notification to a turn event, if it is one Dgent shows.
fn notification_event(method: &str, params: &Value) -> Option<Event> {
    match method {
        "item/agentMessage/delta" => Some(Event::Text(params["delta"].as_str()?.to_owned())),
        "turn/completed" => {
            let turn = &params["turn"];
            Some(if turn["status"] == "failed" {
                Event::Error(error_message(&turn["error"]))
            } else {
                Event::Done
            })
        }
        "error" if params["willRetry"] == false => {
            Some(Event::Error(error_message(&params["error"])))
        }
        _ => None,
    }
}

fn error_message(e: &Value) -> String {
    let msg = e["message"].as_str().unwrap_or("codex turn failed");
    match e["additionalDetails"].as_str() {
        Some(d) => format!("{msg}: {d}"),
        None => msg.to_owned(),
    }
}

/// Answer requests Codex sends us. Anything that would act outside the pane is declined.
fn server_request_result(method: &str, params: &Value) -> Option<Value> {
    Some(match method {
        "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
            json!({"decision": "decline"})
        }
        "execCommandApproval" | "applyPatchApproval" => {
            json!({"decision": {"denied": {"rejection": "Run commands with the dgent run_in_pane tool."}}})
        }
        "item/permissions/requestApproval" => json!({"permissions": {}}),
        // Codex asks before calling an MCP tool; Dgent's bridge does the real approval.
        "mcpServer/elicitation/request" if params["serverName"] == "dgent" => {
            json!({"action": "accept"})
        }
        "mcpServer/elicitation/request" => json!({"action": "decline"}),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fake app-server end: read what the client sent, write what Codex would.
    struct Server {
        rx: BufReader<std::io::PipeReader>,
        tx: std::io::PipeWriter,
    }

    impl Server {
        fn recv(&mut self) -> Value {
            let mut line = String::new();
            self.rx.read_line(&mut line).unwrap();
            serde_json::from_str(&line).unwrap()
        }
        fn send(&mut self, v: Value) {
            writeln!(self.tx, "{v}").unwrap();
        }
    }

    fn connect() -> (CodexBackend, Server) {
        let (client_rx, server_tx) = std::io::pipe().unwrap();
        let (server_rx, client_tx) = std::io::pipe().unwrap();
        let mut server = Server {
            rx: BufReader::new(server_rx),
            tx: server_tx,
        };
        let handle = thread::spawn(move || {
            let init = server.recv();
            assert_eq!(init["method"], "initialize");
            assert_eq!(init["params"]["clientInfo"]["name"], "dgent");
            server.send(json!({"id": init["id"], "result": {"userAgent": "dgent/0.172.0", "platformOs": "linux"}}));
            assert_eq!(server.recv(), json!({"method": "initialized"}));
            let start = server.recv();
            assert_eq!(start["method"], "thread/start");
            assert_eq!(start["params"]["sandbox"], "read-only");
            // Unrelated notifications arrive before the response.
            server.send(
                json!({"method": "remoteControl/status/changed", "params": {"status": "disabled"}}),
            );
            server.send(json!({"id": start["id"], "result": {"thread": {"id": "t1"}, "sandbox": {"type": "readOnly"}}}));
            server
        });
        let backend = CodexBackend::start(client_rx, client_tx).unwrap();
        (backend, handle.join().unwrap())
    }

    #[test]
    fn turn_streams_text_and_declines_exec() {
        let (mut b, mut server) = connect();
        let rx = b
            .ask(Request {
                prompt: "why is disk full".into(),
                context: "$ df -h".into(),
            })
            .unwrap();

        let turn = server.recv();
        assert_eq!(turn["method"], "turn/start");
        assert_eq!(turn["params"]["threadId"], "t1");
        let text = turn["params"]["input"][0]["text"].as_str().unwrap();
        assert!(text.starts_with("why is disk full") && text.ends_with("$ df -h"));
        server.send(json!({"id": turn["id"], "result": {"turn": {"id": "u1", "status": "inProgress", "items": []}}}));

        server.send(
            json!({"id": 7, "method": "item/commandExecution/requestApproval",
            "params": {"threadId": "t1", "turnId": "u1", "itemId": "i1", "command": "df -h"}}),
        );
        assert_eq!(
            server.recv(),
            json!({"id": 7, "result": {"decision": "decline"}})
        );
        server.send(json!({"id": 8, "method": "mcpServer/elicitation/request",
            "params": {"threadId": "t1", "serverName": "dgent"}}));
        assert_eq!(server.recv()["result"]["action"], "accept");

        for d in ["Disk is ", "full."] {
            server.send(json!({"method": "item/agentMessage/delta",
                "params": {"threadId": "t1", "turnId": "u1", "itemId": "m1", "delta": d}}));
        }
        server.send(json!({"method": "turn/completed",
            "params": {"threadId": "t1", "turn": {"id": "u1", "status": "completed", "items": []}}}));

        // A second ask while the turn runs is refused.
        assert!(b.ask(Request::default()).is_err());
        let events: Vec<_> = rx.iter().collect();
        assert_eq!(
            events,
            vec![
                Event::Text("Disk is ".into()),
                Event::Text("full.".into()),
                Event::Done
            ]
        );
    }

    #[test]
    fn rejected_turn_start_is_an_error() {
        let (mut b, mut server) = connect();
        let rx = b.ask(Request::default()).unwrap();
        let turn = server.recv();
        server
            .send(json!({"id": turn["id"], "error": {"code": -32600, "message": "not logged in"}}));
        // The server stays up, yet the turn ends and the next ask is allowed.
        assert_eq!(
            rx.iter().collect::<Vec<_>>(),
            vec![Event::Error("not logged in".into())]
        );
        b.ask(Request::default()).unwrap();
        assert_eq!(server.recv()["method"], "turn/start");
    }

    #[test]
    fn server_exit_ends_the_turn() {
        let (mut b, server) = connect();
        let rx = b.ask(Request::default()).unwrap();
        drop(server);
        assert_eq!(
            rx.recv().unwrap(),
            Event::Error("codex app-server exited".into())
        );
    }

    #[test]
    fn errors_end_turns_only_when_final() {
        // Recorded from codex-cli 0.172.0 with no network.
        let retrying = json!({"error": {"message": "Reconnecting... 2/5", "additionalDetails": "stream disconnected"},
            "willRetry": true, "threadId": "t1", "turnId": "u1"});
        assert_eq!(notification_event("error", &retrying), None);
        let failed = json!({"threadId": "t1", "turn": {"id": "u1", "status": "failed",
            "error": {"message": "usage limit reached", "additionalDetails": null}}});
        assert_eq!(
            notification_event("turn/completed", &failed),
            Some(Event::Error("usage limit reached".into()))
        );
    }

    #[test]
    fn unknown_server_requests_are_refused() {
        assert_eq!(
            server_request_result("item/tool/requestUserInput", &json!({})),
            None
        );
        assert_eq!(
            server_request_result(
                "mcpServer/elicitation/request",
                &json!({"serverName": "other"})
            ),
            Some(json!({"action": "decline"}))
        );
    }
}
