//! Manual check of the Codex backend with your `codex login` (no dgent tools):
//! `cargo run -p dgent-backend --example codex_ask -- "what is 2+2"`
use std::io::Write;

use dgent_backend::{Backend, CodexBackend, Event, Request};

fn main() -> anyhow::Result<()> {
    let prompt = std::env::args().skip(1).collect::<Vec<_>>().join(" ");
    let bin = std::env::var("CODEX_BIN").unwrap_or_else(|_| "codex".into());
    let mut b = CodexBackend::spawn(&bin, None)?;
    for ev in b.ask(Request {
        prompt,
        context: String::new(),
    })? {
        match ev {
            Event::Text(t) => print!("{t}"),
            Event::Error(e) => anyhow::bail!(e),
            other => println!("\n{other:?}"),
        }
        std::io::stdout().flush()?;
    }
    Ok(())
}
