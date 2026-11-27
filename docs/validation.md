# Validating each MVP merge

The M1 work items are PRs #2 to #10. This page gives the order to merge them in, what should work after each merge, and a script that checks it on your Linux box.

## Merge order

**Don't merge in the order they were raised.** That order (#2, #3, #4, ...) puts config (#3) and the overseer (#4) first, but those two wire up things that land later: backends, redaction, failure detection. Merging them first means their placeholder code stays in, and every later PR has to rebase across them.

Every PR that edits `crates/dgent/src/main.rs` (#3, #4, #6, #7, #8, #9) conflicts with every other one. Whatever the order, each of those needs a rebase after the one before it merges. The order below keeps each rebase small: foundations first, then the PRs that wire them together.

| Phase | PR | Work item | Why it goes here | Rebase needed after the previous merge |
|---|---|---|---|---|
| 1 | #2 | W3 secret redaction | New crate, touches nothing else. #4 needs it. | None |
| 2 | #5 | W5 Claude backend | Backend crate only. #3 needs it. | None |
| 3 | #10 | W6 Codex backend | Backend crate only. #3 needs it. | Small: `dgent-backend/src/lib.rs` and `Cargo.toml` (both PRs add a module line and `serde_json`) |
| 4 | #8 | W1 Pane | Restructures `main.rs` (PTY, parser, writer move into `Pane`). Merging it first means the others rebase onto `Pane` once, instead of #8 rebasing onto all of them. | None |
| 5 | #7 | W7 approval panel | The biggest UI change. #6 and #9 hang off it (approval, `panel.hint`). | `main.rs`: write through `pane.write`. `smoke.sh`: keep both new sections |
| 6 | #9 | W2 failure detection | Needs the pane byte stream and the panel's badge. #4 consumes its failures. | `main.rs`: `Pane::pump` doesn't expose the bytes yet, so the observer needs a hook there. The badge can move to `panel.hint` |
| 7 | #6 | W4 MCP bridge | Needs the approval panel, so `run_in_pane` can be approved instead of always denied. | `main.rs`: send `run_in_pane` to `panel.pending`. **07-mcp-bridge.sh fails if it's still denied** |
| 8 | #4 | W8 overseer | Needs redaction (#2) and failures (#9). | `main.rs`: swap the identity redactor for `dgent_redact::redact`. **08-overseer.sh fails if it isn't swapped** |
| 9 | #3 | W9 config | Selects between everything above, so it goes last. | `main.rs`: build `ClaudeBackend`/`CodexBackend` for `--backend`, feed `context_lines`/`context_bytes` into the overseer's limits instead of its own `tail_bytes`, keep `dgent mcp` ahead of flag parsing |
| — | not raised | M2 integration | Pending. See [M2](#m2-integration-pending) | — |

`Cargo.lock` conflicts in several of these. Take either side, then run `cargo build` to regenerate it.

## Running the checks

After merging a phase:

```sh
git checkout main && git pull
./scripts/validate/04-pane.sh                 # the script for that phase
DGENT_SSH_HOST=myhost ./scripts/validate/04-pane.sh   # also run the SSH checks
```

Each script builds, runs that PR's tests and `scripts/smoke.sh`, drives the TUI in a detached tmux session, and ends with a few manual checks to do by hand in a real terminal. A non-zero exit means something failed. To re-check everything merged so far: `for s in scripts/validate/[0-9]*.sh; do "$s" || break; done`. A script whose PR isn't merged yet says so and exits 0.

What you need:
- `cargo`, `tmux`, `bash`. `zsh` is optional (one extra check in phase 6).
- `claude` with `claude login` done for phases 2, 7 and 9. `codex` with `codex login` done for phases 3 and 9. Those checks skip if the tool isn't installed.
- For SSH checks, `DGENT_SSH_HOST` must accept `ssh -o BatchMode=yes <host>` with no prompts (key or agent auth). The scripts start a plain `bash --norc` on the remote so its prompt is predictable. Nothing is copied to or installed on the remote.
- `DGENT_KEEP=1` leaves the tmux session up after a failure so you can `tmux attach` and look.

The TUI checks use a clean `bash --norc` with prompt `$ ` so results don't depend on your rc files. Your real shell and prompt are covered by the manual checks.

## Phase by phase

### 1. Secret redaction (#2): `01-redact.sh`

**Should work:** `dgent_redact::redact` replaces AWS key ids, GitHub and Slack tokens, JWTs, `Bearer` tokens, `password=`-style values and whole private key blocks with `[REDACTED]`, and returns ordinary shell output unchanged. Nothing in the TUI changes yet.

**Script:** unit tests, then pipes sample lines with each kind of secret through `redact()` via a throwaway binary, plus real `ls`/`git log`/`env` output that must come back byte for byte.

### 2. Claude backend (#5): `02-claude-backend.sh`

**Should work:** `ClaudeBackend` drives `claude -p` stream-json on your subscription login, ignoring any `ANTHROPIC_API_KEY`. Follow-up turns share one conversation. Claude's own tools are off. Not selectable in the TUI until phase 9.

**Script:** fixture tests, then the `claude_ask` example with a bogus `ANTHROPIC_API_KEY` set. It must still answer, remember a number across two turns, and fail to create a canary file when asked to.

### 3. Codex backend (#10): `03-codex-backend.sh`

**Should work:** `CodexBackend` drives `codex app-server` on your ChatGPT login, ignoring `OPENAI_API_KEY`/`CODEX_API_KEY`, and declines every attempt by Codex to run a command or patch a file.

**Script:** fixture tests and all backend tests together, then the `codex_ask` example with bogus API keys set. It must answer, and must not create a canary file when asked to run `touch`. This is the first run of #10 against a real login. The PR's own author couldn't reach api.openai.com.

### 4. Pane (#8): `04-pane.sh`

**Should work:** the shell runs inside `Pane`. A huge paste into a program that isn't reading no longer freezes Dgent, and `C-] q` still quits. Shrinking the window keeps the cursor line visible (on `main` the bottom rows were cut off). `TERM` is `screen-256color`, locally and over ssh. Dgent exits with the shell's exit code.

**Script:** pane unit tests, smoke test (includes the 2 MB paste), `$TERM`, shrinking 35 to 15 rows after `seq 1 60`, exit code. Over SSH: the remote sees `screen-256color` and 256 colours.

**By hand:** htop and vim over ssh, including a resize while htop runs.

### 5. Approval panel (#7): `05-approval.sh`

**Should work:** a command the agent proposes shows as `run? <cmd>` in the panel. `y` types it into the shell, `n` denies it, `e` edits it and Enter runs the edit. Ctrl-Y does nothing, and keys or pastes don't reach the shell while a proposal waits. The echo backend stands in for an agent: `C-] a` then `run <cmd>` makes it propose `<cmd>`.

**Script:** unit and smoke tests, then approve, deny, keys held back, a new ask replacing a pending proposal, and edit. Over SSH: an approved `echo "@$(hostname)-$(id -un)"` prints the remote host's name, which shows the command ran on the remote.

**By hand:** a very long proposed command, and the too-tall case. Scrolling with `C-] k` / `C-] j`.

**Not yet:** the backend gets a placeholder instead of the command's output. That needs M2.

### 6. Failure detection (#9): `06-failure-detection.sh`

**Should work:** with no setup, a failing command's error line shows on the status line (guessed from the prompt and error text, no exit code). `C-] i` adds OSC 133 marks to the current shell for that session only, local or remote. After that, the status line shows `exit N: <command>`, silent failures like `false` included, and a successful command clears it.

**Script:** fixture tests and smoke test. Locally: an unmarked failure, a clear, `C-] i`, `ls /nonexistent` gives `exit 2`, `false` gives `exit 1`, a success clears it. zsh `C-] i` if zsh is installed. Over SSH: an unmarked remote failure, `C-] i` on the remote then `exit 2`, and back on the local shell a failed `ssh nohost.invalid`.

**By hand:** your own shell and prompt, a remote login after logout (marks gone), and vim or a progress bar not raising a false badge.

### 7. MCP tool bridge (#6): `07-mcp-bridge.sh`

**Should work:** `dgent mcp` is a stdio MCP server offering `read_screen` and `run_in_pane`. The running TUI listens on an owner-only socket `dgent-<pid>.sock` (in `$XDG_RUNTIME_DIR`, else `/tmp`) and removes it on quit. `read_screen` returns the screen. After the rebase onto #7, `run_in_pane` shows as an approval in the panel and runs only on `y`.

**Script:** unit and integration tests, then `tools/list` and a no-socket call against `dgent mcp` on its own. Against the running TUI: socket mode 600, `read_screen` returns a marker echoed on screen, `run_in_pane` goes to the panel and `y` runs it (this FAILS if it's still auto-denied with #7 merged), socket gone after quit. Then `claude --mcp-config` lists `mcp__dgent__read_screen` and `mcp__dgent__run_in_pane`.

**Not yet:** backends don't start `dgent mcp` from the TUI (M2).

### 8. Overseer (#4): `08-overseer.sh`

**Should work:** `C-] a` sends the question plus at most 40 lines / 8 KiB of screen, redacted before it's cut. A detected failure only sets a badge unless auto mode is on, and auto mode is rate limited.

**Script:** unit tests (budget, rate limit, redaction on every path), smoke test. It checks the real `dgent_redact::redact` is in use (FAILS if the identity stand-in is still there). On a 60-row screen after `seq 1 300`, `C-] a` sends at most 40 lines. `C-] e` is checked if it's bound to a key, otherwise it's skipped as M2.

**By hand (after phase 9):** print a fake AWS key, ask a real backend what you printed. The key must not come back.

### 9. Config (#3): `09-config.sh`

**Should work:** `~/.config/dgent/config.toml` (`backend`, `context_lines`, `context_bytes`, `auto_suggest`, `claude_bin`, `codex_bin`) and `--backend claude|codex|echo`, with the flag beating the file. A bad flag, an unknown key, an unknown backend or a malformed file is an error before the TUI starts. After the rebase onto #5 and #10, `--backend claude` and `--backend codex` answer from your logins.

**Script:** unit and smoke tests. Five bad-input cases must fail with a message. `dgent mcp` still works. `context_lines = 5` in the file gives 5 lines of context. `--backend codex|claude` beats `backend = "echo"`. If the backends are wired in, it sends one real question to each.

## M2 integration (pending)

M2 isn't raised yet. It wires the pieces together, and the MVP isn't usable for real work until it lands:
- Backends start `dgent mcp` with `DGENT_SOCK` set, so the agent's `run_in_pane` reaches the approval panel. Codex may need `mcp_servers.dgent.env` for that.
- Approved commands use the `Pane::run` capture (it blocks today, so it needs a non-blocking form) and the backend gets the real output.
- `C-] e` explains the last failure, the overseer's badge replaces the phase 6 one, and auto mode follows `auto_suggest`.
- A turn can be interrupted (Codex can retry for minutes with no network).

Validating M2 means the three "MVP done" scenarios from `docs/PLAN.md`, by hand, once with each backend:
1. `ssh somehost`, run a failing command, press `C-] e`: you get a short explanation and a suggested fix.
2. `C-] a` "why is disk full on this box": approve or deny each proposed command, and the answer is built from their output on the remote host.
3. `dgent --backend claude` and `dgent --backend codex` both work with no API key in the environment.
