#!/usr/bin/env bash
# Phase 5, PR #7 (W7): a proposed command waits in the agent panel for y (run), n (deny) or
# e (edit, then Enter to run). Nothing reaches the shell while it waits. The echo backend stands
# in for an agent: asking "run <cmd>" makes it propose <cmd>.
source "$(dirname "$0")/lib.sh"
need crates/dgent/src/ui.rs "PR #7 (approval panel)"
build

step "unit tests and smoke test (approve, deny, edit, Ctrl-Y ignored)"
check "cargo test -p dgent ui" cargo test -q -p dgent ui
smoke

step "TUI: approval flow"
tstart
task "run echo approved-\$((6*7))"
texpect 'run\? echo approved-\$\(\(6\*7\)\)' "proposal shown in the panel"
texpect 'approve: y run, n deny, e edit' "status line offers y / n / e"
tkeys x z; sleep 0.4
tkeys n; sleep 0.6
tabsent '^\$ .*xz' "keys typed while a proposal waits don't reach the shell"
texpect 'echo: denied' "n denies, and the backend is told"
task "run echo approved-\$((6*7))"
tkeys y; sleep 0.8
texpect '^approved-42$' "y types the command into the shell and it runs"
texpect 'echo: ran' "the backend is told it ran"
task "run echo first"
task "run echo second"
texpect 'run\? echo second' "a new ask replaces the unanswered proposal"
tabsent '^first$' "the replaced proposal never ran"
tkeys e; sleep 0.3
tkeys BSpace BSpace BSpace BSpace BSpace BSpace; ttype "edited-ok"
texpect '^edited-ok$' "e then Enter runs the edited command"
tquit
tabsent 'DGENT-EXIT=[1-9]' "C-] q quits cleanly"

step "over ssh: an approved command runs on the remote host"
if have_ssh "approve over ssh"; then
  remote=$(ssh -o BatchMode=yes "$SSH_HOST" 'echo "$(hostname)-$(id -un)"')
  tstart
  tssh
  task 'run echo "@$(hostname)-$(id -un)"'
  tkeys y; sleep 1.5
  texpect "^@$remote\$" "approved command ran on $SSH_HOST (printed $remote)"
  ttype 'exit' 1.5
  tquit
fi

manual "Ask for a very long command (C-] a, then: run echo followed by ~400 characters). The panel grows to show it all; shrink the window until it can't fit and check y does nothing and the status line says it's too tall."
manual "With a long transcript in the panel, C-] k and C-] j scroll it."
echo "Note: until M2 the backend gets a placeholder instead of the command's output ('output capture not wired yet')."
finish
