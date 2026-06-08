//! User configuration (`config.toml`).
//!
//! Location: `$OWT_CONFIG` if set, else the platform config dir
//! (`<config>/openworktree/config.toml`). Missing file = all defaults.

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Default, Deserialize)]
pub struct Config {
    /// Shell used by interactive mode (`owt -i`).
    pub shell: Option<String>,

    /// Default source ref when `--from` is not given (otherwise HEAD).
    pub from: Option<String>,

    /// Named argument presets, invoked as `owt @<name>`.
    #[serde(default)]
    pub alias: HashMap<String, Alias>,
}

/// A saved set of arguments, e.g. `[alias.oc] args = ["--from", "origin/main", ...]`.
#[derive(Debug, Deserialize)]
pub struct Alias {
    pub args: Vec<String>,
}

fn config_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("OWT_CONFIG") {
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    ProjectDirs::from("", "", "openworktree").map(|p| p.config_dir().join("config.toml"))
}

impl Config {
    pub fn load() -> Result<Config> {
        let Some(path) = config_path() else {
            return Ok(Config::default());
        };
        if !path.exists() {
            return Ok(Config::default());
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    /// Resolve the interactive shell: `--shell` > config > `$SHELL`/`ComSpec` > default.
    pub fn resolve_shell(&self, cli_shell: Option<&str>) -> String {
        if let Some(s) = cli_shell {
            return s.to_string();
        }
        if let Some(s) = &self.shell {
            return s.clone();
        }
        if cfg!(windows) {
            std::env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".to_string())
        } else {
            std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_precedence_cli_over_config() {
        let cfg = Config {
            shell: Some("from_config".to_string()),
            from: None,
            alias: Default::default(),
        };
        assert_eq!(cfg.resolve_shell(Some("from_cli")), "from_cli");
        assert_eq!(cfg.resolve_shell(None), "from_config");
    }
}
