//! Settings from `~/.config/dgent/config.toml`, overridden by command-line flags.
//!
//! ```toml
//! backend = "codex"        # claude | codex | echo
//! context_lines = 40       # screen lines sent with a request
//! context_bytes = 16384    # hard cap on context size
//! auto_suggest = false     # ask the LLM on detected failures without a keypress
//! claude_bin = "claude"
//! codex_bin = "codex"
//! ```

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BackendKind {
    Claude,
    Codex,
    Echo,
}

impl std::str::FromStr for BackendKind {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "claude" => Ok(Self::Claude),
            "codex" => Ok(Self::Codex),
            "echo" => Ok(Self::Echo),
            _ => bail!("unknown backend {s:?} (expected claude, codex or echo)"),
        }
    }
}

// auto_suggest and the *_bin paths are read by the overseer (W8) and backends (W5/W6).
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub backend: BackendKind,
    pub context_lines: usize,
    pub context_bytes: usize,
    pub auto_suggest: bool,
    pub claude_bin: PathBuf,
    pub codex_bin: PathBuf,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            backend: BackendKind::Claude,
            context_lines: 40,
            context_bytes: 16 * 1024,
            auto_suggest: false,
            claude_bin: "claude".into(),
            codex_bin: "codex".into(),
        }
    }
}

impl Config {
    /// Load the config file (if any), then apply `args` (without the program name).
    pub fn load(args: impl IntoIterator<Item = String>) -> Result<Self> {
        let mut cfg = match default_path() {
            Some(p) => Self::from_file(&p)?,
            None => Self::default(),
        };
        cfg.apply_args(args)?;
        Ok(cfg)
    }

    /// A missing file means defaults; an unreadable or invalid one is an error.
    fn from_file(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(s) => toml::from_str(&s).with_context(|| format!("parse {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
        }
    }

    fn apply_args(&mut self, args: impl IntoIterator<Item = String>) -> Result<()> {
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            let value = match arg.split_once('=') {
                Some(("--backend", v)) => v.to_string(),
                None if arg == "--backend" => args.next().context("--backend needs a value")?,
                _ => bail!("unknown argument {arg:?} (usage: dgent [--backend claude|codex|echo])"),
            };
            self.backend = value.parse()?;
        }
        Ok(())
    }
}

fn default_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute()) // XDG: a relative value is treated as unset
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("dgent/config.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn missing_file_gives_defaults() {
        let cfg = Config::from_file(Path::new("/nonexistent/dgent.toml")).unwrap();
        assert_eq!(cfg, Config::default());
    }

    #[test]
    fn file_overrides_some_fields() {
        let dir = std::env::temp_dir().join(format!("dgent-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            "backend = \"echo\"\ncontext_lines = 10\nauto_suggest = true\n",
        )
        .unwrap();
        let cfg = Config::from_file(&path).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(cfg.backend, BackendKind::Echo);
        assert_eq!(cfg.context_lines, 10);
        assert!(cfg.auto_suggest);
        assert_eq!(cfg.context_bytes, Config::default().context_bytes);
        assert_eq!(cfg.claude_bin, PathBuf::from("claude"));
    }

    #[test]
    fn bad_file_is_an_error() {
        assert!(toml::from_str::<Config>("backend = \"gpt\"").is_err());
        assert!(toml::from_str::<Config>("contxt_lines = 5").is_err());
    }

    #[test]
    fn backend_flag_overrides_file() {
        let mut cfg = Config {
            backend: BackendKind::Echo,
            ..Config::default()
        };
        cfg.apply_args(args(&["--backend", "codex"])).unwrap();
        assert_eq!(cfg.backend, BackendKind::Codex);
        cfg.apply_args(args(&["--backend=claude"])).unwrap();
        assert_eq!(cfg.backend, BackendKind::Claude);
    }

    #[test]
    fn bad_args_are_errors() {
        let mut cfg = Config::default();
        assert!(cfg.apply_args(args(&["--backend"])).is_err());
        assert!(cfg.apply_args(args(&["--backend", "gpt"])).is_err());
        assert!(cfg.apply_args(args(&["--verbose"])).is_err());
    }
}
