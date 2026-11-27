#!/usr/bin/env bash
# Phase 2, PR #5 (W5): ClaudeBackend talks to `claude -p` stream-json on your subscription login,
# keeps one conversation across turns, and can't run anything itself.
source "$(dirname "$0")/lib.sh"
need crates/dgent-backend/src/claude.rs "PR #5 (Claude backend)"

step "unit and fixture tests"
check "cargo test -p dgent-backend claude" cargo test -q -p dgent-backend claude

step "live run on your claude login"
ask() { timeout 180 cargo run -q -p dgent-backend --example claude_ask -- "$@"; }
if ! command -v claude >/dev/null; then
  skip "live checks (claude not on PATH; install Claude Code and run: claude login)"
else
  # A bogus API key in the env: the backend strips it, so the answer must come from the login.
  export ANTHROPIC_API_KEY=sk-ant-bogus-dgent-validate
  # The example echoes the prompt as "> ...", so match the answer line, not the question.
  if out=$(ask "What is six times seven? Reply with only the digits." 2>&1) && grep -qx '42' <<<"$out"; then
    ok "answers on the subscription login (a bogus ANTHROPIC_API_KEY was ignored)"
  else
    bad "no answer. If it says not logged in, run: claude login"; sed 's/^/        | /' <<<"$out" | tail -n 8
  fi
  out=$(ask "Pick a random number from 1 to 1000. Reply with only the number." \
            "What number did you just pick? Reply with only the number." 2>&1)
  mapfile -t nums < <(grep -E '^[0-9]+$' <<<"$out")
  [ ${#nums[@]} = 2 ] && [ "${nums[0]}" = "${nums[1]}" ] && ok "second turn remembers the first (same conversation)" \
    || bad "turns don't share a conversation: $(tr '\n' ' ' <<<"$out")"
  canary=/tmp/dgent-canary-$$
  ask "Use any tool you have to create the file $canary, then say done." >/dev/null 2>&1
  [ -e "$canary" ] && { bad "Claude created $canary: its own tools are NOT disabled"; rm -f "$canary"; } \
    || ok "Claude could not create a file itself (built-in tools disabled)"
fi

echo "Note: not selectable in the TUI until PR #3 (config) is rebased to add it; 09-config.sh checks that."
finish
