#!/usr/bin/env bash
# Phase 7, PR #6 (W4): `dgent mcp` is the agent's only way to act. It's a stdio MCP server with
# two tools, read_screen and run_in_pane, and relays each call over a unix socket to the running
# TUI. read_screen answers with the screen; run_in_pane goes to the approval panel.
source "$(dirname "$0")/lib.sh"
need crates/dgent-mcp "PR #6 (MCP tool bridge)"
build

step "unit and integration tests"
check "cargo test -p dgent-mcp" cargo test -q -p dgent-mcp
check "cargo test -p dgent --test mcp (real dgent mcp process round-trip)" cargo test -q -p dgent --test mcp
check "scripts/smoke.sh" ./scripts/smoke.sh "$BIN"

INIT='{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}'
INITED='{"jsonrpc":"2.0","method":"notifications/initialized"}'
# mcp SOCK METHOD-JSON: one MCP session against `dgent mcp`, prints the last reply.
mcp() { printf '%s\n' "$INIT" "$INITED" "$2" | DGENT_SOCK=$1 timeout "${3:-20}" "$BIN" mcp | tail -n 1; }
call() { printf '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"%s","arguments":%s}}' "$1" "$2"; }

step "dgent mcp on its own"
out=$(mcp "" '{"jsonrpc":"2.0","id":2,"method":"tools/list"}')
grep -q '"run_in_pane"' <<<"$out" && grep -q '"read_screen"' <<<"$out" \
  && ok "tools/list offers run_in_pane and read_screen" || bad "tools/list: $out"
out=$(mcp /nonexistent.sock "$(call read_screen '{}')")
grep -q '"isError":true' <<<"$out" && ok "with no Dgent running, a call fails cleanly (isError)" || bad "no-socket call: $out"

step "against the running TUI"
tstart
ttype 'echo screen-marker-$((40+2))'
pid=$(pgrep -n -f "^$BIN( |$)")
sock=$(ls "${XDG_RUNTIME_DIR:-/nonexistent}/dgent-$pid.sock" "${TMPDIR:-/tmp}/dgent-$pid.sock" 2>/dev/null | head -n 1)
if [ -z "$sock" ]; then
  bad "no socket dgent-$pid.sock in \$XDG_RUNTIME_DIR or ${TMPDIR:-/tmp}"
else
  ok "TUI listens on $sock"
  [ "$(stat -c %a "$sock")" = 600 ] && ok "socket is owner-only (600)" || bad "socket mode is $(stat -c %a "$sock")"
  out=$(mcp "$sock" "$(call read_screen '{}')")
  grep -q 'screen-marker-42' <<<"$out" && ok "read_screen returns what's on screen" || bad "read_screen: $out"

  mcp "$sock" "$(call run_in_pane '{"command":"echo via-mcp-$((1+1))"}')" 60 >"$TMP/rip" &
  sleep 1.5
  if tscreen | grep -q 'run? echo via-mcp'; then
    ok "run_in_pane shows up as a proposal in the panel"
    tkeys y; wait
    texpect '^via-mcp-2$' "approving it runs the command in the pane"
    grep -q '"isError":false' "$TMP/rip" && ok "the agent gets a non-error result" || bad "result: $(cat "$TMP/rip")"
  else
    wait
    if [ -e crates/dgent/src/ui.rs ]; then
      bad "run_in_pane was not routed to the approval panel (PR #7 is in, so the rebase should wire it). Reply: $(cat "$TMP/rip")"
    else
      grep -q '"isError":true' "$TMP/rip" && ok "run_in_pane is denied (expected until PR #7's approval panel is in)" \
        || bad "run_in_pane: $(cat "$TMP/rip")"
    fi
    tabsent '^via-mcp-2$' "nothing ran without approval"
  fi
  tquit
  [ -e "$sock" ] && bad "socket left behind after quit" || ok "socket removed on quit"
fi

step "Claude sees the tools"
if command -v claude >/dev/null; then
  cfg="{\"mcpServers\":{\"dgent\":{\"command\":\"$BIN\",\"args\":[\"mcp\"]}}}"
  init=$(timeout 120 claude -p --tools "" --strict-mcp-config --mcp-config "$cfg" --max-turns 1 \
         --output-format stream-json --verbose "Reply ok." </dev/null 2>/dev/null | grep -m1 '"subtype":"init"')
  grep -q 'mcp__dgent__read_screen' <<<"$init" && grep -q 'mcp__dgent__run_in_pane' <<<"$init" \
    && ok "claude --mcp-config lists mcp__dgent__read_screen and run_in_pane" || bad "claude init event: ${init:0:300}"
else
  skip "claude not on PATH"
fi

echo "Note: backends don't launch 'dgent mcp' from the TUI yet. That's M2 (set DGENT_SOCK for the agent)."
finish
