# dgent

A terminal wrapper with an LLM overseer. It hosts your shell (and the `ssh`
sessions you start in it) in a PTY, and can explain failures or run commands
toward a goal you ask for, with your approval. Nothing is installed on remote
hosts. Linux first.

```sh
cargo run -p dgent
```

Needs Rust 1.89+. Install [rustup](https://rustup.rs) and put `~/.cargo/bin` first on `PATH`; it then uses the version pinned in `rust-toolchain.toml`. Distro cargo older than 1.84 fails with "resolver 3 is not valid".

Keys: prefix `C-]`, then `a` ask, `c` clear panel, `q` quit, `C-]` sends a literal `C-]`.
The scaffold ships an echo backend; real backends are planned in [docs/PLAN.md](docs/PLAN.md).
