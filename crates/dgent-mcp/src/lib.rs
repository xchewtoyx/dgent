//! The tool bridge between a backend agent and the running Dgent TUI.
//!
//! A backend CLI launches `dgent mcp` as a stdio MCP server ([`serve`]). Each
//! `tools/call` is relayed as one JSON line over the unix socket named in
//! `DGENT_SOCK` to the TUI's [`Listener`], which hands it out as a [`Call`] and
//! writes back the user's [`ToolReply`]. The agent never runs anything itself.
//!
//! Socket protocol, one connection per call:
//! request `{"id":"<n>","tool":"run_in_pane","command":"..."}` or `{"id":"<n>","tool":"read_screen"}`,
//! reply `{"output":"..."}` or `{"denied":true}`.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};

use anyhow::{Context, Result, bail};
use dgent_backend::ToolReply;
use serde_json::{Value, json};

/// The env var `dgent mcp` reads to find the TUI's socket.
pub const SOCK_ENV: &str = "DGENT_SOCK";

/// `$XDG_RUNTIME_DIR/dgent-<pid>.sock`, or the temp dir when that is unset.
pub fn socket_path() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(format!("dgent-{}.sock", std::process::id()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tool {
    RunInPane(String),
    ReadScreen,
}

/// One tool call waiting for the user. Dropping it without [`Call::reply`] fails the call.
#[derive(Debug)]
pub struct Call {
    pub id: String,
    pub tool: Tool,
    stream: UnixStream,
}

impl Call {
    pub fn reply(mut self, reply: &ToolReply) -> Result<()> {
        let msg = match reply {
            ToolReply::Denied => json!({ "denied": true }),
            ToolReply::Output(o) => json!({ "output": o }),
        };
        writeln!(self.stream, "{msg}")?;
        Ok(())
    }
}

/// TUI side: accepts relayed calls on a unix socket. Removes the socket file on drop.
pub struct Listener {
    path: PathBuf,
    pub calls: Receiver<Call>,
}

impl Listener {
    pub fn bind(path: &Path) -> Result<Self> {
        let _ = std::fs::remove_file(path); // stale socket from a crashed run
        let sock = UnixListener::bind(path).with_context(|| format!("bind {}", path.display()))?;
        let (tx, calls) = mpsc::channel();
        std::thread::spawn(move || {
            for stream in sock.incoming().flatten() {
                let tx = tx.clone();
                // A thread per connection so a slow or silent client can't block others.
                std::thread::spawn(move || {
                    if let Some(call) = read_call(stream) {
                        let _ = tx.send(call);
                    }
                });
            }
        });
        Ok(Self {
            path: path.to_owned(),
            calls,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn read_call(stream: UnixStream) -> Option<Call> {
    let mut line = String::new();
    BufReader::new(stream.try_clone().ok()?)
        .read_line(&mut line)
        .ok()?;
    let v: Value = serde_json::from_str(&line).ok()?;
    let tool = match v["tool"].as_str()? {
        "run_in_pane" => Tool::RunInPane(v["command"].as_str()?.to_owned()),
        "read_screen" => Tool::ReadScreen,
        _ => return None,
    };
    Some(Call {
        id: v["id"].as_str()?.to_owned(),
        tool,
        stream,
    })
}

/// `dgent mcp`: a minimal MCP server on `input`/`output` (newline-delimited JSON-RPC).
/// `sock` is the TUI socket; without one, tools are still listed but calls fail.
pub fn serve(input: impl BufRead, mut output: impl Write, sock: Option<&Path>) -> Result<()> {
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => handle(&msg, sock),
            Err(e) => Some(
                json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":e.to_string()}}),
            ),
        };
        if let Some(reply) = reply {
            writeln!(output, "{reply}")?;
            output.flush()?;
        }
    }
    Ok(())
}

/// Answer one JSON-RPC message; `None` for notifications.
fn handle(msg: &Value, sock: Option<&Path>) -> Option<Value> {
    let id = msg.get("id")?.clone();
    let params = &msg["params"];
    let result = match msg["method"].as_str().unwrap_or_default() {
        "initialize" => json!({
            "protocolVersion": params["protocolVersion"].as_str().unwrap_or("2025-06-18"),
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "dgent", "version": env!("CARGO_PKG_VERSION") },
        }),
        "ping" => json!({}),
        "tools/list" => json!({ "tools": tools() }),
        "tools/call" => {
            let (text, is_error) = match call_tool(params, sock) {
                Ok(Some(out)) => (out, false),
                Ok(None) => ("The user denied this command.".to_owned(), true),
                Err(e) => (format!("dgent: {e:#}"), true),
            };
            json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
        }
        m => {
            return Some(
                json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":format!("unknown method {m}")}}),
            );
        }
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

fn tools() -> Value {
    json!([
        {
            "name": "run_in_pane",
            "description": "Propose a shell command to run in the user's active terminal pane, which may be an ssh session on a remote host. The user approves or denies it. On approval Dgent types it into the pane and returns the captured output.",
            "inputSchema": {
                "type": "object",
                "properties": { "command": { "type": "string", "description": "One shell command line." } },
                "required": ["command"],
            },
        },
        {
            "name": "read_screen",
            "description": "Return the text currently visible in the user's active terminal pane.",
            "inputSchema": { "type": "object", "properties": {} },
        },
    ])
}

/// Relay a `tools/call` to the TUI. `Ok(None)` means the user denied it.
fn call_tool(params: &Value, sock: Option<&Path>) -> Result<Option<String>> {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    let id = format!(
        "{}-{}",
        std::process::id(),
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    );
    let req = match params["name"].as_str() {
        Some("run_in_pane") => {
            let Some(cmd) = params["arguments"]["command"].as_str() else {
                bail!("run_in_pane needs a string `command`");
            };
            json!({ "id": id, "tool": "run_in_pane", "command": cmd })
        }
        Some("read_screen") => json!({ "id": id, "tool": "read_screen" }),
        other => bail!("unknown tool {other:?}"),
    };
    let sock = sock.with_context(|| format!("{SOCK_ENV} is not set; is Dgent running?"))?;
    let mut stream =
        UnixStream::connect(sock).with_context(|| format!("connect {}", sock.display()))?;
    writeln!(stream, "{req}")?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    let reply: Value = serde_json::from_str(&line).context("no reply from Dgent")?;
    if reply["denied"] == true {
        return Ok(None);
    }
    match reply["output"].as_str() {
        Some(o) => Ok(Some(o.to_owned())),
        None => bail!("bad reply from Dgent: {line}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rpc(input: &str) -> Vec<Value> {
        let mut out = Vec::new();
        serve(input.as_bytes(), &mut out, None).unwrap();
        String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[test]
    fn handshake_list_and_errors() {
        let out = rpc(concat!(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}"#,
            "\n",
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            "\n",
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            "\nnot json\n",
            r#"{"jsonrpc":"2.0","id":3,"method":"nope"}"#,
            "\n",
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"read_screen"}}"#,
            "\n",
        ));
        assert_eq!(out.len(), 5, "notification must get no reply: {out:?}");
        assert_eq!(out[0]["result"]["protocolVersion"], "2025-03-26");
        let names: Vec<_> = out[1]["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["run_in_pane", "read_screen"]);
        assert_eq!(out[2]["error"]["code"], -32700);
        assert_eq!(out[3]["error"]["code"], -32601);
        assert_eq!(
            out[4]["result"]["isError"], true,
            "no socket: call fails, server lives"
        );
    }
}
