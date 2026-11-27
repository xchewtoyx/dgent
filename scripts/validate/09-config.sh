#!/usr/bin/env bash
# Phase 9, PR #3 (W9): ~/.config/dgent/config.toml plus --backend claude|codex|echo. Bad config
# is an error, not ignored. Once rebased onto the backends, --backend really picks Claude or Codex.
source "$(dirname "$0")/lib.sh"
need crates/dgent/src/config.rs "PR #3 (config)"
build

step "unit tests"
check "cargo test -p dgent config" cargo test -q -p dgent config
smoke

step "bad flags and bad files are errors (no TUI starts)"
export XDG_CONFIG_HOME=$TMP/cfg
mkdir -p "$XDG_CONFIG_HOME/dgent"
cf=$XDG_CONFIG_HOME/dgent/config.toml
fails() { local want=$1; shift; out=$("$BIN" "$@" </dev/null 2>&1); [ $? -ne 0 ] && grep -qi -- "$want" <<<"$out"; }
check "--backend bogus is rejected" fails 'unknown backend' --backend bogus
check "an unknown flag prints usage" fails 'usage' --frobnicate
echo 'backend = "nope"' >"$cf";   check "unknown backend in the file is rejected" fails 'parse' 
echo 'colour = true' >"$cf";      check "unknown key in the file is rejected" fails 'parse'
echo 'backend = ' >"$cf";         check "malformed file is rejected" fails 'parse'
rm -f "$cf"
if [ -e crates/dgent-mcp ]; then
  out=$(printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"tools/list"}' | timeout 10 "$BIN" mcp 2>&1)
  grep -q run_in_pane <<<"$out" && ok "'dgent mcp' still works (not eaten by flag parsing)" || bad "dgent mcp: $out"
fi

step "TUI: the file and the flag"
printf 'backend = "echo"\ncontext_lines = 5\n' >"$cf"
tstart "$TMP/bash" " "
ttype 'seq 1 50'
task 'from the file'
texpect 'echo: from the file \(5 lines of context\)' "context_lines from the file is used"
tquit
for b in codex claude; do
  tstart "$TMP/bash" --backend "$b"
  texpect "$b|${b^}" "--backend $b overrides the file (shown in the status line or panel)"
  tquit
done
rm -f "$cf"

step "TUI: real backends (needs the backend rebase and your logins)"
for pair in claude:ClaudeBackend:claude.rs codex:CodexBackend:codex.rs; do
  IFS=: read -r b type file <<<"$pair"
  if [ ! -e "crates/dgent-backend/src/$file" ]; then
    skip "--backend $b: its backend PR isn't merged"
  elif ! grep -q "$type" crates/dgent/src/*.rs; then
    bad "--backend $b still falls back to echo; its backend is merged, so the config rebase should build $type"
  elif ! command -v "$b" >/dev/null; then
    skip "--backend $b: $b not on PATH"
  else
    tstart "$TMP/bash" --backend "$b"
    task 'What is six times seven? Reply with only the digits.' 1
    # The question has no "42" in it, so a match is the backend's answer.
    WAIT=90 texpect '(^|[^0-9])42([^0-9]|$)' "--backend $b answers from your $b login"
    tquit
  fi
done

manual "Put backend = \"codex\" in ~/.config/dgent/config.toml, start dgent with no flags: it uses Codex. Then dgent --backend claude: Claude wins over the file."
finish
