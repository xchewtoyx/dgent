Raw PTY output recorded from real shells (Python `pty.fork`, `TERM=screen-256color`,
`PS1='$ '` or zsh `PS1='%# '`), read by the tests in `src/observe.rs`.

- `\0IDLE\0`: output paused here before the next keystrokes (the TUI calls `Observer::idle`).
- `\0INJECT\0`: the `C-] i` line (`observe::INJECT`) was typed next (`Observer::injected`).

| File | Shell | Steps |
|---|---|---|
| `bash-marks.bin` | `bash --norc -i` | inject, `echo hi`, `ls /nonexistent`, empty line, `false`, `exit` |
| `bash-plain.bin` | `bash --norc -i` | same, no inject |
| `zsh-marks.bin` | `zsh -f -i` | `PS1='%# '`, inject, then as bash |
| `zsh-plain.bin` | `zsh -f -i` | `PS1='%# '`, then as bash, no inject |
| `ssh-nested.bin` | `bash --norc -i` | inject, `ssh -tt localhost`, `ls /nonexistent`, `true`, inject (remote), `false`, `exit`, `ssh nohost.invalid`, `exit` |
