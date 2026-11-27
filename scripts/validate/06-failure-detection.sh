#!/usr/bin/env bash
# Phase 6, PR #9 (W2): Dgent notices failed commands from the PTY stream alone. With OSC 133
# marks (C-] i adds them to the current shell, local or remote) it gets exact exit codes;
# without them it guesses from the prompt and error text. The status line shows the last failure.
source "$(dirname "$0")/lib.sh"
need crates/dgent/src/observe.rs "PR #9 (failure detection)"
build

step "unit tests over recorded bash, zsh and ssh sessions; smoke test"
check "cargo test -p dgent observe" cargo test -q -p dgent observe
check "scripts/smoke.sh" ./scripts/smoke.sh "$BIN"

step "TUI: local bash"
tstart
ttype 'ls /nope-a'
VIEW=tstatus texpect "cannot access '/nope-a'" "without marks: error text shows on the status line"
ttype 'true'
VIEW=tstatus tabsent 'cannot access' "a command that prints no error clears it"
tkeys C-] i; sleep 1
ttype 'ls /nonexistent'
VIEW=tstatus texpect 'exit 2: ls /nonexistent' "after C-] i: exact exit code and command"
ttype 'false'
VIEW=tstatus texpect 'exit 1: false' "a silent failure (false) is caught once marked"
ttype 'echo fine'
VIEW=tstatus tabsent 'exit [0-9]' "a successful command clears the badge"
tquit

step "TUI: zsh"
if command -v zsh >/dev/null; then
  printf '#!/bin/sh\nPS1="%% " exec zsh -f -i\n' >"$TMP/zsh"; chmod +x "$TMP/zsh"
  tstart "$TMP/zsh"
  tkeys C-] i; sleep 1
  ttype 'ls /nonexistent'
  VIEW=tstatus texpect 'exit [12]: ls /nonexistent' "zsh after C-] i: exit code and command"
  tquit
else
  skip "zsh not installed"
fi

step "over ssh (nothing is installed or written on the remote)"
if have_ssh "remote failure detection"; then
  tstart
  tssh
  ttype 'ls /nope-r' 1.5
  VIEW=tstatus texpect "cannot access '/nope-r'" "remote, unmarked: error text detected"
  tkeys C-] i; sleep 1.5
  ttype 'grep -q x /etc/nope' 1.5
  VIEW=tstatus texpect 'exit 2: grep -q x /etc/nope' "remote after C-] i: exact exit code"
  ttype 'exit' 2
  ttype 'ssh -o BatchMode=yes -o ConnectTimeout=3 nohost.invalid' 4
  VIEW=tstatus texpect 'resolve|not known|exit 255' "back on the local shell: a failed ssh is detected"
  tquit
fi

manual "In your normal shell (your own rc files and prompt): run 'ls /nope'. The status line shows the error. Press C-] i, run 'ls /nope' again: it shows 'exit 2: ls /nope'. Your prompt and \$? behave as before. If your HISTCONTROL has ignorespace, 'history' doesn't show the injected line."
manual "ssh to a real host, press C-] i, run a failing command: exit code shows. Log out and back in: the marks are gone (session only), and ~/.bashrc on the remote is unchanged."
manual "Run a full-screen program (vim, then :q) and a progress bar (e.g. curl -o /dev/null on a big file). No false failure badge appears."
finish
