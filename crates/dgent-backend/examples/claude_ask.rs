//! Manual check of the Claude backend: `cargo run -p dgent-backend --example claude_ask -- "question" ["follow-up" ...]`.
//! Each argument is one turn in the same conversation.

use std::io::Write;

use dgent_backend::claude::{ClaudeBackend, ClaudeConfig};
use dgent_backend::{Backend, Event, Request};

fn main() -> anyhow::Result<()> {
    let mut b = ClaudeBackend::spawn(&ClaudeConfig::default())?;
    for prompt in std::env::args().skip(1) {
        println!("> {prompt}");
        for ev in b.ask(Request {
            prompt,
            context: String::new(),
        })? {
            match ev {
                Event::Text(t) => print!("{t}"),
                Event::Error(e) => anyhow::bail!("{e}"),
                _ => {}
            }
            std::io::stdout().flush()?;
        }
        println!();
    }
    Ok(())
}
