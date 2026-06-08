//! Worktree metadata, written both inside the worktree (`.owt-meta.json`) and
//! into a central index under the app cache dir so `list` / `clean` can find it.

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Stored inside the worktree's private git admin dir, never in the working tree.
pub const META_FILENAME: &str = "owt-meta.json";
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
pub struct Metadata {
    pub schema_version: u32,
    pub name: String,
    pub branch: String,
    pub from_ref: String,
    pub base_commit: String,
    pub worktree_path: String,
    pub repo_common_dir: String,
    /// "oneshot" | "interactive"
    pub mode: String,
    pub command: Vec<String>,
    /// "discard" | "keep"
    pub on_exit: String,
    /// PID of the owning owt process (dead pid => orphan).
    pub pid: u32,
    /// ISO-8601 creation timestamp.
    pub created_at: String,
    /// "running"
    pub status: String,
}

/// Root cache directory. Overridable via `OWT_CACHE_DIR` (used for tests and to
/// let users relocate owt's state); otherwise the platform cache dir.
fn cache_root() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("OWT_CACHE_DIR") {
        if !dir.is_empty() {
            return Ok(PathBuf::from(dir));
        }
    }
    let proj =
        ProjectDirs::from("", "", "openworktree").context("cannot determine app directories")?;
    Ok(proj.cache_dir().to_path_buf())
}

/// Central directory holding one `<name>.json` index entry per live worktree.
pub fn index_dir() -> Result<PathBuf> {
    Ok(cache_root()?.join("index"))
}

/// Default base directory for worktrees: `<cache>/worktrees`.
pub fn default_worktree_base() -> Result<PathBuf> {
    Ok(cache_root()?.join("worktrees"))
}

impl Metadata {
    /// Write both copies: into the worktree's private git admin dir (so it can
    /// never be committed) and into the central index.
    pub fn write(&self, admin_dir: &Path) -> Result<()> {
        let json = serde_json::to_string_pretty(self)?;

        let co_located = admin_dir.join(META_FILENAME);
        std::fs::write(&co_located, &json)
            .with_context(|| format!("writing {}", co_located.display()))?;

        let dir = index_dir()?;
        std::fs::create_dir_all(&dir)?;
        let index_file = dir.join(format!("{}.json", self.name));
        std::fs::write(&index_file, &json)
            .with_context(|| format!("writing {}", index_file.display()))?;
        Ok(())
    }

    /// Remove the central index entry (the in-tree copy goes away with the dir).
    pub fn remove_index(name: &str) -> Result<()> {
        let index_file = index_dir()?.join(format!("{name}.json"));
        if index_file.exists() {
            std::fs::remove_file(&index_file)
                .with_context(|| format!("removing {}", index_file.display()))?;
        }
        Ok(())
    }
}
