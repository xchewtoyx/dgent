#!/usr/bin/env bash
# Phase 8, PR #4 (W8): the overseer decides when the LLM is called and what it sees. C-] a sends
# the question plus the screen tail, capped (default 40 lines / 8 KiB), redacted before it's cut.
# A detected failure only sets a badge unless auto mode is on, and auto mode is rate limited.
source "$(dirname "$0")/lib.sh"
need crates/dgent/src/overseer.rs "PR #4 (overseer)"
build

step "unit tests (budget, rate limit, redaction on every path and before truncation)"
check "cargo test -p dgent overseer" cargo test -q -p dgent overseer
smoke

step "redaction is the real one"
if [ -e crates/dgent-redact ]; then
  grep -rq 'dgent_redact' crates/dgent/src && ok "overseer uses dgent_redact::redact" \
    || bad "overseer still uses the identity redactor (main.rs 'shortcut'); PR #2 is in, so the rebase should swap it"
else
  skip "PR #2 (dgent-redact) not in this checkout; the identity redactor is expected"
fi

step "TUI: context budget (60-row screen, so there is more than the cap)"
ROWS=60 tstart
ttype 'seq 1 300'
task 'how much context'
n=$(tscreen | grep -Eo 'echo: how much context \([0-9]+ lines' | grep -Eo '[0-9]+' | tail -n 1)
if [ -z "$n" ]; then bad "no echo reply to C-] a"; tscreen | tail -n 8 | sed 's/^/        | /'
elif [ "$n" -ge 1 ] && [ "$n" -le 40 ]; then ok "C-] a sent $n lines of screen context (cap is 40)"
else bad "C-] a sent $n lines (over the 40-line cap)"; fi

step "TUI: explain last failure"
tkeys C-] i; sleep 1
ttype 'ls /nope-explain'
tkeys C-] e; sleep 1
if tscreen | grep -q 'exited with status'; then
  ok "C-] e asks about the failed command"
  texpect 'C-] e to explain|exit 2' "failure badge offers C-] e"
else
  skip "C-] e isn't wired to a key yet (the overseer has it; M2 wires it with failure detection)"
fi
tquit

manual "Once a real backend is selectable (after phase 9): echo AKIAIOSFODNN7EXAMPLE, then C-] a 'what did I just print?'. The answer must not contain the key (it was sent as [REDACTED])."
finish
