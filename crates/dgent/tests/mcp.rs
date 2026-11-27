//! Plays an MCP client against a real `dgent mcp` process, with a `Listener` as the TUI peer.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use dgent_backend::ToolReply;
use dgent_mcp::{Listener, Tool};
use serde_json::{Value, json};

#[test]
fn tool_call_round_trips_through_the_socket() {
    let sock = std::env::temp_dir().join(format!("dgent-test-{}.sock", std::process::id()));
    let listener = Listener::bind(&sock).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_dgent"))
        .arg("mcp")
        .env(dgent_mcp::SOCK_ENV, &sock)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut rpc = |msg: Value| -> Value {
        writeln!(stdin, "{msg}").unwrap();
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    };

    rpc(
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}),
    );
    let peer = std::thread::spawn(move || {
        let call = listener.calls.recv().unwrap();
        assert_eq!(call.tool, Tool::RunInPane("df -h".into()));
        call.reply(&ToolReply::Output("/dev/sda1 100%".into()))
            .unwrap();
        let call = listener.calls.recv().unwrap();
        call.reply(&ToolReply::Denied).unwrap();
    });
    let call = |id| {
        json!({"jsonrpc":"2.0","id":id,"method":"tools/call",
        "params":{"name":"run_in_pane","arguments":{"command":"df -h"}}})
    };

    let ok = rpc(call(2));
    assert_eq!(ok["id"], 2);
    assert_eq!(ok["result"]["content"][0]["text"], "/dev/sda1 100%");
    assert_eq!(ok["result"]["isError"], false);
    let denied = rpc(call(3));
    assert_eq!(denied["result"]["isError"], true);

    peer.join().unwrap();
    drop(stdin);
    assert!(child.wait().unwrap().success());
}
