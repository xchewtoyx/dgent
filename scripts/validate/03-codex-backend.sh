#!/usr/bin/env bash
# Phase 3, PR #10 (W6): CodexBackend talks to `codex app-server` on your ChatGPT login and
# declines every attempt by Codex to run a command or write a file itself.
source "$(dirname "$0")/lib.sh"
need crates/dgent-backend/src/codex.rs "PR #10 (Codex backend)"

step "unit and fixture tests"
check "cargo test -p dgent-backend codex" cargo test -q -p dgent-backend codex
check "all dgent-backend tests (Claude and Codex together)" cargo test -q -p dgent-backend

step "live run on your codex login"
ask() { timeout 180 cargo run -q -p dgent-backend --example codex_ask -- "$@"; }
if ! command -v "${CODEX_BIN:-codex}" >/dev/null; then
  skip "live checks (codex not on PATH; install it and run: codex login)"
else
  # Bogus API keys in the env: the backend strips them and forces the ChatGPT login.
  export OPENAI_API_KEY=sk-bogus-dgent-validate CODEX_API_KEY=sk-bogus-dgent-validate
  if out=$(ask "What is six times seven? Reply with only the digits." 2>&1) && grep -qx '42' <<<"$out"; then
    ok "answers on the ChatGPT login (bogus OPENAI_API_KEY/CODEX_API_KEY ignored)"
  else
    bad "no answer. If it mentions auth, run: codex login"; sed 's/^/        | /' <<<"$out" | tail -n 8
  fi
  canary=/tmp/dgent-canary-$$
  ask "Run the shell command: touch $canary . Then say done." >"$TMP/canary" 2>&1 && turn=ok || turn=failed
  if [ -e "$canary" ]; then
    bad "Codex created $canary: it ran a command outside Dgent"; rm -f "$canary"
  elif [ $turn = failed ]; then
    bad "canary turn failed, so exec wasn't tested: $(tail -n 3 "$TMP/canary")"
  else ok "Codex could not run a command itself (exec declined)"; fi
fi
finish
