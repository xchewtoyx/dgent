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

# Approval: the echo backend proposes the command after "run "; approve, deny, then edit.
tmux kill-session -t "$s"
propose() { tmux send-keys -t "$s" C-] a; sleep 0.3; tmux send-keys -t "$s" "run $1" Enter; sleep 0.5; }
tmux new-session -d -s "$s" -x 100 -y 30 "SHELL=/bin/sh $bin"
sleep 1.5
propose 'echo approved'
tmux capture-pane -t "$s" -p | grep -q 'run? echo approved' || { echo "proposal not shown"; tmux capture-pane -t "$s" -p; exit 1; }
tmux send-keys -t "$s" y; sleep 0.7
screen=$(tmux capture-pane -t "$s" -p)
grep -qx 'approved' <<<"$screen" || { echo "approved command did not run"; echo "$screen"; exit 1; }
grep -q 'echo: ran' <<<"$screen" || { echo "backend not told it ran"; echo "$screen"; exit 1; }
propose 'echo nope'
tmux send-keys -t "$s" C-y; sleep 0.3  # modified keys must not answer
tmux capture-pane -t "$s" -p | grep -q 'run? echo nope' || { echo "Ctrl-Y answered the proposal"; exit 1; }
tmux send-keys -t "$s" n; sleep 0.7
screen=$(tmux capture-pane -t "$s" -p)
grep -q 'echo: denied' <<<"$screen" || { echo "deny not reported"; echo "$screen"; exit 1; }
! grep -qx 'nope' <<<"$screen" || { echo "denied command ran"; echo "$screen"; exit 1; }
propose 'echo approved'
tmux send-keys -t "$s" e BSpace BSpace BSpace BSpace BSpace BSpace BSpace BSpace edited Enter; sleep 0.7
screen=$(tmux capture-pane -t "$s" -p)
grep -qx 'edited' <<<"$screen" || { echo "edited command did not run"; echo "$screen"; exit 1; }
tmux send-keys -t "$s" C-] q; sleep 0.5
echo ok
