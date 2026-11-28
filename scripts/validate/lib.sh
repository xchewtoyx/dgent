#!/usr/bin/env bash
# Shared helpers for scripts/validate/NN-*.sh. Sourced, not run.
#
# Env:
#   DGENT_SSH_HOST   host for the SSH checks (key auth, no password prompt). Unset: SSH checks skip.
#   DGENT_KEEP=1     leave the tmux session running after a failure, to look at it.
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.." || exit 1

PASS=0 FAIL=0 SKIP=0
MANUAL=()
TMP=$(mktemp -d)
S=dgent-validate-$$
SSH_HOST=${DGENT_SSH_HOST:-}

step()   { printf '\n== %s\n' "$*"; }
ok()     { echo "  PASS  $*"; PASS=$((PASS + 1)); }
bad()    { echo "  FAIL  $*"; FAIL=$((FAIL + 1)); }
# With DGENT_KEEP=1, stop before the TUI session that saw a failure is quit or replaced.
kept()   { [ "${DGENT_KEEP:-}" = 1 ] && [ "$FAIL" -gt 0 ] && finish; return 0; }
skip()   { echo "  SKIP  $*"; SKIP=$((SKIP + 1)); }
manual() { MANUAL+=("$*"); }

cleanup() {
  if [ "${DGENT_KEEP:-}" = 1 ] && [ "$FAIL" -gt 0 ]; then
    echo "tmux session kept: tmux attach -t $S"
  else
    tmux kill-session -t "$S" 2>/dev/null
  fi
  rm -rf "$TMP"
}
trap cleanup EXIT

# check "description" cmd args...: PASS if the command succeeds, else FAIL with its output tail.
check() {
  local desc=$1; shift
  if "$@" >"$TMP/out" 2>&1; then ok "$desc"; else tail -n 15 "$TMP/out" | sed 's/^/        | /'; bad "$desc"; fi
}

# need PATH "PR #n": stop with a SKIP when that PR's code isn't on this checkout yet.
need() {
  [ -e "$1" ] && return 0
  echo "$1 is missing: $2 isn't merged into this checkout yet. Nothing to validate."
  exit 0
}

finish() {
  if [ ${#MANUAL[@]} -gt 0 ]; then
    printf '\n== Manual checks (by hand, in a real terminal)\n'
    local i=1; for m in "${MANUAL[@]}"; do echo "  $i. $m"; i=$((i + 1)); done
  fi
  printf '\n%s: %d passed, %d failed, %d skipped\n' "$(basename "$0")" "$PASS" "$FAIL" "$SKIP"
  [ "$FAIL" -eq 0 ]
  exit
}

build() {
  step "build"
  check "cargo build" cargo build -q
  BIN=$PWD/target/debug/dgent
  # Once config (PR #3) is in, the default backend is claude: the TUI checks use the echo backend.
  DGENT_ARGS=""
  [ -e crates/dgent/src/config.rs ] && DGENT_ARGS="--backend echo"
  # A predictable shell: no rc files, prompt "$ ". Your own rc files get the manual checks.
  # HISTFILE=/dev/null: never touch your saved history.
  printf '#!/bin/sh\nHISTFILE=/dev/null PS1="$ " exec bash --norc --noprofile -i\n' >"$TMP/bash"
  chmod +x "$TMP/bash"
  # scripts/smoke.sh expects echo replies; once config is in, point it at echo explicitly.
  printf '#!/bin/sh\nexec %q %s "$@"\n' "$BIN" "$DGENT_ARGS" >"$TMP/dgent-echo"
  chmod +x "$TMP/dgent-echo"
}
smoke() { check "scripts/smoke.sh" ./scripts/smoke.sh "$TMP/dgent-echo"; }

# --- tmux: drive the TUI the way scripts/smoke.sh does ---------------------------------------

# tstart [SHELL] [dgent args...]: start dgent in a fresh detached tmux session.
tstart() {
  kept
  local sh=${1:-$TMP/bash}; shift || true
  tmux kill-session -t "$S" 2>/dev/null
  # An already-running tmux server has its own env; pass the config dir through explicitly.
  local env=()
  [ -n "${XDG_CONFIG_HOME:-}" ] && env=(-e "XDG_CONFIG_HOME=$XDG_CONFIG_HOME")
  tmux new-session -d "${env[@]}" -s "$S" -x "${COLS:-120}" -y "${ROWS:-35}" \
    "SHELL=$sh $BIN ${*:-$DGENT_ARGS}; echo DGENT-EXIT=\$?; sleep 30"
  sleep 1.5
}
tkeys()   { tmux send-keys -t "$S" "$@"; }
ttype()   { tmux send-keys -t "$S" -l "$1"; tmux send-keys -t "$S" Enter; sleep "${2:-0.8}"; }
tscreen() { tmux capture-pane -t "$S" -p; }
tstatus() { tscreen | grep -v '^$' | tail -n 1; }
task()    { tkeys C-] a; sleep 0.3; ttype "$1" "${2:-0.8}"; }
tquit()   { kept; tkeys C-] q; sleep 0.8; }
tpid()    { pgrep -n -f "^$BIN" ; }

# texpect REGEX "description": PASS once the screen matches (waits up to ${WAIT:-5}s).
# VIEW=tstatus texpect ... matches the status line only.
texpect() {
  local i
  for ((i = 0; i < ${WAIT:-5} * 5; i++)); do
    "${VIEW:-tscreen}" | grep -qE -- "$1" && { ok "$2"; return 0; }
    sleep 0.2
  done
  tscreen | grep -v '^$' | tail -n 12 | sed 's/^/        | /'
  bad "$2 (screen never matched /$1/)"
  return 1
}

# tabsent REGEX "description": PASS if the screen does not match right now.
tabsent() {
  if "${VIEW:-tscreen}" | grep -qE -- "$1"; then
    tscreen | grep -v '^$' | tail -n 12 | sed 's/^/        | /'; bad "$2 (screen matched /$1/)"; return 1
  fi
  ok "$2"
}

# --- ssh ---------------------------------------------------------------------------------------

have_ssh() {
  [ -n "$SSH_HOST" ] || { skip "$1 (set DGENT_SSH_HOST=<host> with key auth to run it)"; return 1; }
  ssh -o BatchMode=yes -o ConnectTimeout=5 "$SSH_HOST" true 2>/dev/null && return 0
  skip "$1 (ssh -o BatchMode=yes $SSH_HOST failed: needs key auth, no prompts)"
  return 1
}

# tssh: inside the running dgent, ssh to $SSH_HOST and get a predictable remote bash, prompt "r$ ".
tssh() {
  ttype "ssh -o BatchMode=yes -t $SSH_HOST \"HISTFILE=/dev/null PS1='r\\\$ ' exec bash --norc --noprofile -i\"" 2.5
}
