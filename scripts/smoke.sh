#!/usr/bin/env bash
# Drive the TUI in a detached tmux session: run a command, ask the echo backend, quit.
set -euo pipefail
bin=${1:-target/debug/dgent}
s=dgent-smoke-$$
trap 'tmux kill-session -t "$s" 2>/dev/null || true' EXIT
tmux new-session -d -s "$s" -x 100 -y 30 "SHELL=/bin/sh $bin"
sleep 1.5
tmux send-keys -t "$s" 'echo dgent-$((40+2))' Enter; sleep 0.7
tmux send-keys -t "$s" C-] a; sleep 0.3
tmux send-keys -t "$s" 'what failed' Enter; sleep 0.7
screen=$(tmux capture-pane -t "$s" -p)
tmux send-keys -t "$s" C-] q; sleep 0.7
grep -q 'dgent-42' <<<"$screen" || { echo "shell output missing"; echo "$screen"; exit 1; }
grep -q 'echo: what failed' <<<"$screen" || { echo "backend reply missing"; echo "$screen"; exit 1; }
! tmux has-session -t "$s" 2>/dev/null || { echo "dgent did not quit"; exit 1; }
# The shell's exit status becomes dgent's.
tmux new-session -d -s "$s" -x 100 -y 30 "SHELL=/bin/sh $bin; echo EXIT=\$?; sleep 5"
sleep 1.5
tmux send-keys -t "$s" 'exit 3' Enter; sleep 0.7
tmux capture-pane -t "$s" -p | grep -q 'EXIT=3' || { echo "exit status not propagated"; exit 1; }
# Quit stays responsive when nothing reads input: a big paste into a raw-mode program that
# isn't reading (fills the PTY input queue), and a shell exec'd with output off the PTY.
tmux kill-session -t "$s" 2>/dev/null || true
for setup in 'paste' 'exec sleep 30 >/dev/null'; do
  tmux new-session -d -s "$s" -x 100 -y 30 "SHELL=/bin/sh $bin"
  sleep 1.5
  if [ "$setup" = paste ]; then
    tmux send-keys -t "$s" 'stty raw -echo; sleep 30' Enter; sleep 0.3
    head -c 2000000 /dev/zero | tr '\0' x | tmux load-buffer -b "$s" -
    tmux paste-buffer -p -d -b "$s" -t "$s"; sleep 0.5
  else
    tmux send-keys -t "$s" "$setup" Enter; sleep 0.5
  fi
  tmux send-keys -t "$s" C-] q; sleep 1
  ! tmux has-session -t "$s" 2>/dev/null || { echo "dgent did not quit after: $setup"; exit 1; }
done
echo ok
