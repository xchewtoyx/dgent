# Dgent MVP plan

## Goal and bar

A Linux binary Russell finds useful for basic tasks: a terminal wrapper whose
LLM overseer can explain what just went wrong and, when asked, run commands
toward a goal, with approval. SSH is the main use case. Nothing is installed
or authenticated on remote hosts. No distribution, packaging or other users.

**MVP done when**, inside `dgent`, Russell can:

1. `ssh somehost`, hit a failing command, press `C-] e`, and get a short explanation and a suggested fix.
2. Press `C-] a`, type "why is disk full on this box", approve or deny each proposed command, and get an answer built from their output.
3. Pick the backend (`--backend claude` or `--backend codex`), both using the existing subscription login (`claude login` / `codex login`), no API keys.

## Architecture

```
 ┌───────────────────────── dgent (local process) ─────────────────────────┐
 │  TUI (ratatui)  ── keys ──▶ Pane: PTY + vt100 parser + scrollback        │
 │      ▲                       │  runs $SHELL; `ssh host` happens inside   │
 │      │ events                ▼                                           │
 │  Overseer ◀── observations: command boundaries, exit codes, error hits   │
 │      │  decides *when* to involve the LLM and *what* context to send     │
 │      │  (bounded, redacted)                                              │
 │      ▼                                                                   │
 │  Backend trait ── Claude CLI (stream-json) | Codex app-server | echo     │
 │      ▲                                                                   │
 │      └── tool calls via `dgent mcp` bridge (unix socket) ──▶ approval UI │
 │                                         ──▶ typed into the active pane   │
 └──────────────────────────────────────────────────────────────────────────┘
```

Key decisions:

- **The agent sees the local PTY stream.** Remote sessions are just bytes in the pane, so there is nothing to install remotely. The trade-off is that command boundaries and exit codes on remote hosts have to be inferred (see W2).
- **The agent never executes anything itself.** Backends start with their built-in shell/file tools disabled. The only way to act is the `run_in_pane` tool, which Dgent shows to the user for approval, types into the active pane, and captures the output of. That makes "run it on the remote host" automatic, and keeps the user in control.
- **One tool bridge for all backends.** `dgent mcp` is a stdio MCP server subcommand. A backend CLI launches it (via its MCP config), and it relays `tools/call` over a unix socket (`$XDG_RUNTIME_DIR/dgent-<pid>.sock`) to the running TUI, which turns it into `Event::ProposeCommand` and blocks until the user decides. Tools: `run_in_pane(command)`, `read_screen()`.
- **Light oversight.** The LLM is only called on (a) an explicit ask, or (b) a detected failure, and for (b) the default is a status-line badge ("⚠ exit 1, C-] e to explain"), not an automatic call. An opt-in auto mode sends only the failed command and its output, capped, rate limited.
- **Bounded context.** Each request gets the prompt plus at most N lines / K bytes: the failed command and its output, or the visible screen tail. Everything passes through redaction first.

### Driving the backends from Rust

| Backend | How | Subscription auth | Disabling its own exec | Notes |
|---|---|---|---|---|
| Claude Code CLI | Spawn `claude -p --input-format stream-json --output-format stream-json --verbose`, keep the process for multi-turn, write user messages as JSON lines to stdin | Uses `claude login` credentials. **Do not pass `--bare`**: bare mode ignores OAuth and needs an API key | `--tools ""` plus `--strict-mcp-config --mcp-config '{"mcpServers":{"dgent":{"command":"dgent","args":["mcp"]}}}'` and `--allowedTools mcp__dgent__run_in_pane,mcp__dgent__read_screen` (Dgent does approval itself) | The Agent SDKs (TS/Python) wrap this same CLI protocol, so driving the CLI directly is the Rust route. Prior art: the `claude-codes` crate |
| Codex | Spawn `codex app-server`, JSON-RPC 2.0 over stdio (JSON lines): `initialize`, `thread/start`, `turn/start`, then `item/*` notifications; server sends approval requests for command execution | Uses `codex login` (ChatGPT sign-in) | Start the thread with a read-only sandbox and decline any `commandExecution` approval request; register the `dgent` MCP server through config overrides. **Verify exact field names** against `codex app-server generate-json-schema` for the installed version | Prior art: the `codex-codes` crate |
| Others (later) | Agent Client Protocol (ACP), JSON-RPC over stdio, Rust crate `agent-client-protocol` | Per agent (Gemini CLI, adapters for Claude/Codex) | Client-side tools are first class in ACP | One client would cover several agents; not needed for MVP |

Sources: [Claude Code headless docs](https://code.claude.com/docs/en/headless), [Claude Code CLI reference](https://code.claude.com/docs/en/cli-reference), [Codex app-server docs](https://developers.openai.com/codex/app-server), [agent-client-protocol crate](https://docs.rs/crate/agent-client-protocol/).

## What exists (M0, this scaffold)

- Cargo workspace: `crates/dgent` (binary) and `crates/dgent-backend` (trait + `EchoBackend`).
- TUI hosts `$SHELL` in a PTY (portable-pty + vt100 + tui-term), handles resize, paste and keys. Prefix `C-]`: `a` ask, `c` clear panel, `q` quit, `C-]` literal.
- `Backend` trait: `ask(Request) -> Receiver<Event>`, `reply(id, ToolReply)`. `Event` is `Text | ProposeCommand | Done | Error`.
- CI: fmt, clippy `-D warnings`, tests, and `scripts/smoke.sh` (drives the TUI in tmux).

## Work items (M1, parallel)

Each item touches its own module or crate, builds against the existing `Backend` trait, and is done when its condition holds and CI is green. Shared interfaces are fixed below so items do not wait on each other.

**W1. Pane and command capture** (`crates/dgent/src/pane.rs`)
Move PTY + parser into a `Pane` with scrollback and `fn run(&mut self, cmd) -> Capture { output, exit: Option<i32> }`, which types the command, then collects output until W2's end-of-command signal or a quiescence timeout.
Also: PTY writes must not block the UI loop (a large paste into a command that isn't reading input fills the PTY queue). Move writes to a writer thread with a bounded queue that a quit can bypass. vt100 truncates rows/columns on shrink; push dropped rows into scrollback or accept and document it.
Done: a test runs `/bin/sh` in a `Pane`, runs `echo hi; false`, and gets the output and (with W2 markers present) exit 1; `main.rs` uses `Pane`.

**W2. Command boundaries and failure detection** (`crates/dgent/src/observe.rs`)
Scan the PTY byte stream for OSC 133 (A/B/C/D;exit) marks, and fall back to heuristics (prompt regex, error patterns like `command not found`, `Permission denied`, `error:`). Add `C-] i`, which types one line into the current shell (local or remote) that sets a `PROMPT_COMMAND`/`precmd` emitting OSC 133. Session only, nothing written to the remote disk.
Done: unit tests over recorded byte fixtures (bash with marks, bash without, zsh) emit `CommandFinished { cmd, output_range, exit }`; injected integration works over `ssh localhost`.

**W3. Secret redaction** (`crates/dgent-redact`)
`pub fn redact(&str) -> Cow<str>` for private key blocks, AWS keys, GitHub/Slack/bearer tokens, `password=`/`secret=` style assignments, JWTs.
Done: table-driven tests for each pattern plus a no-false-positive test on ordinary shell output.

**W4. MCP tool bridge** (`crates/dgent-mcp`, `dgent mcp` subcommand)
Minimal stdio MCP server (`initialize`, `tools/list`, `tools/call`) for `run_in_pane` and `read_screen`, relaying calls as JSON lines over the unix socket named in `DGENT_SOCK`. TUI side: a socket listener that yields `(id, command)` and accepts a `ToolReply`.
Done: an integration test plays an MCP client against `dgent mcp` with a fake socket peer and round-trips a call; `claude --mcp-config` lists the tools.

**W5. Claude backend** (`crates/dgent-backend/src/claude.rs`)
Implement `Backend` over a long-lived `claude -p` stream-json process with the flags in the table. Map assistant text deltas to `Event::Text`, results to `Done`, process errors to `Error`. Bridge tool calls surface as `ProposeCommand` (from W4's listener; until it lands, test with a fake).
Done: parser tests on recorded stream-json fixtures; manual run with Russell's login answers a question with no API key set.

**W6. Codex backend** (`crates/dgent-backend/src/codex.rs`)
Implement `Backend` over `codex app-server`: handshake, one thread per pane, a turn per ask, stream agent message deltas, decline its own command-exec approvals, use the `dgent` MCP tool.
Done: fixture tests for the JSON-RPC exchange; manual run with `codex login` answers a question; no command runs outside Dgent's pane.

**W7. Approval and agent panel UX** (`crates/dgent/src/ui.rs`)
Panel shows streaming text and a pending proposal with `y` run, `n` deny, `e` edit-then-run; scrollable; status-line badge for overseer hints. Approved commands go to the pane via W1 (until then, write to the PTY directly).
Done: a scripted fake backend that proposes `echo approved` passes a tmux smoke test for approve, deny and edit.

**W8. Overseer policy** (`crates/dgent/src/overseer.rs`)
Turns observations into requests: explicit ask (screen tail), `C-] e` explain-last-failure (failed command + output), optional auto mode with rate limit. Enforces the context budget and runs redaction.
Done: unit tests for budget truncation, rate limiting, and that redaction always runs before a `Request` is built.

**W9. Config and backend selection** (`crates/dgent/src/config.rs`)
`--backend claude|codex|echo` flag and `~/.config/dgent/config.toml` (backend, context limits, auto-suggest on/off, backend binary paths).
Done: tests for defaults and file overrides; `dgent --backend codex` picks Codex.

## M2: integration (sequential, after M1)

Wire W1–W9 together in `main.rs` and check the three "MVP done" scenarios by hand over a real `ssh` session, with each backend. Fix what breaks, then tag `v0.1.0` for personal use.

## After the MVP (not planned in detail)

Multiple panes/tabs; ACP backend for Gemini and others; session transcript export; other OSes (macOS next, as portable-pty already supports it).
