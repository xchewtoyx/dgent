# dgent

A terminal wrapper with an LLM overseer. It hosts your shell (and the `ssh`
sessions you start in it) in a PTY, and can explain failures or run commands
toward a goal you ask for, with your approval. Nothing is installed on remote
hosts. Linux first.

```sh
cargo run -p dgent
```

Keys: prefix `C-]`, then `a` ask, `c` clear panel, `q` quit, `C-]` sends a literal `C-]`.
The scaffold ships an echo backend; real backends are planned in [docs/PLAN.md](docs/PLAN.md).

Terminal notes:

- The shell gets `TERM=screen-256color`, which `ssh` passes on to remote hosts.
  dgent's emulator (vt100) can't do `rep`, which `xterm-256color` advertises, and
  screen's terminfo is on nearly every host. Apps that only recognise xterm key
  sequences for modified arrows (e.g. Vim's `<C-Left>`) may need a mapping.
- Shrinking the window keeps the cursor line visible and moves the top rows into
  scrollback. Narrowing it truncates lines; there is no reflow.
