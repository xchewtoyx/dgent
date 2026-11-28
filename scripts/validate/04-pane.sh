#!/usr/bin/env bash
# Phase 4, PR #8 (W1): the shell lives in a Pane. Writes never block the UI, shrinking keeps the
# cursor line (older rows go to scrollback), TERM is screen-256color, Pane::run captures output.
source "$(dirname "$0")/lib.sh"
need crates/dgent/src/pane.rs "PR #8 (pane and command capture)"
build

step "unit tests (Pane::run gets output and exit code, shrink, non-blocking writes)"
check "cargo test -p dgent pane" cargo test -q -p dgent pane

step "smoke test (includes quit after a 2 MB paste into a raw-mode program)"
smoke

step "TUI: terminal type and shrinking"
tstart
ttype 'echo "T=$TERM"'
texpect '^T=screen-256color' "TERM inside dgent is screen-256color"
ttype 'seq 1 60; echo end-of-seq'
tmux resize-window -t "$S" -y 15; sleep 1
texpect '^end-of-seq' "after shrinking 35 -> 15 rows, the last output line is still on screen"
ttype 'echo still-typing' 
texpect '^still-typing' "shell still works after the shrink"
tmux resize-window -t "$S" -y 35; sleep 0.5
kept
ttype 'exit 7' 1
texpect 'DGENT-EXIT=7' "dgent exits with the shell's exit status"

step "over ssh"
if have_ssh "TERM on the remote host"; then
  tstart
  tssh
  ttype 'echo "RT=$TERM colors=$(tput colors)"' 1.5
  texpect '^RT=screen-256color colors=256' "remote sees TERM=screen-256color with 256 colours"
  ttype 'exit' 1.5
  tquit
fi

manual "In dgent: ssh to a real host and run htop (or top), then vim. Both should draw cleanly, with colour. Resize the window while htop runs; it should redraw."
manual "In dgent: run 'seq 1 200', then make the window much shorter. The prompt stays visible. (Scrolling back up isn't in the UI yet.)"
manual "In dgent: paste a large block (a few thousand lines) into 'cat > /dev/null', then press Ctrl-D. The UI stays responsive throughout."
finish
