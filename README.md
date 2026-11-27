# dgent

A terminal wrapper with an LLM overseer. It hosts your shell (and the `ssh`
sessions you start in it) in a PTY, and can explain failures or run commands
toward a goal you ask for, with your approval. Nothing is installed on remote
hosts. Linux first.

```sh
cargo run -p dgent
```

Keys: prefix `C-]`, then `a` ask, `i` add command marks (OSC 133) to the current shell, local or over ssh, for exact exit codes, `c` clear panel, `q` quit, `C-]` sends a literal `C-]`. A failed command shows on the status line.
The scaffold ships an echo backend; real backends are planned in [docs/PLAN.md](docs/PLAN.md).
