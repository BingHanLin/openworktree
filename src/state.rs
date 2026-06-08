//! Worktree inspection (`list`) and garbage collection (`clean`).

use anyhow::Result;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::git;
use crate::metadata::{self, Metadata};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Main,
    Owt,
    External,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Liveness {
    Running,
    Orphan,
    #[serde(rename = "-")]
    Na,
}

#[derive(Debug, Serialize)]
pub struct View {
    pub name: Option<String>,
    pub branch: Option<String>,
    pub source: Source,
    pub status: Liveness,
    pub locked: bool,
    pub path: String,
}

/// Build a view of every git worktree (including the main one).
pub fn collect() -> Result<Vec<View>> {
    let porcelain = git::worktree_list()?;
    let main_path = git::common_dir()?
        .parent()
        .map(normalize)
        .unwrap_or_default();

    // Index entries keyed by owt name, for pid / liveness lookup.
    let index: Vec<Metadata> = load_index()?;

    let mut views = Vec::new();
    for block in porcelain.split("\n\n") {
        let block = block.trim();
        if block.is_empty() {
            continue;
        }
        let mut path: Option<String> = None;
        let mut branch: Option<String> = None;
        let mut locked = false;
        for line in block.lines() {
            if let Some(p) = line.strip_prefix("worktree ") {
                path = Some(p.trim().to_string());
            } else if let Some(b) = line.strip_prefix("branch ") {
                branch = Some(b.trim().trim_start_matches("refs/heads/").to_string());
            } else if line == "locked" || line.starts_with("locked ") {
                locked = true;
            }
        }
        let Some(path) = path else { continue };

        let is_main = normalize(Path::new(&path)) == main_path;
        let owt_name = branch
            .as_deref()
            .and_then(|b| b.strip_prefix("owt/"))
            .map(|s| s.to_string());

        let (source, name, status) = if is_main {
            (Source::Main, None, Liveness::Na)
        } else if let Some(n) = owt_name {
            let live = match index.iter().find(|m| m.name == n) {
                Some(m) if is_alive(m.pid) => Liveness::Running,
                _ => Liveness::Orphan,
            };
            (Source::Owt, Some(n), live)
        } else {
            (Source::External, None, Liveness::Na)
        };

        views.push(View {
            name,
            branch,
            source,
            status,
            locked,
            path,
        });
    }
    Ok(views)
}

/// Load all central index metadata entries.
pub fn load_index() -> Result<Vec<Metadata>> {
    let dir = metadata::index_dir()?;
    let mut out = Vec::new();
    if dir.exists() {
        for entry in std::fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                if let Ok(text) = std::fs::read_to_string(&path) {
                    if let Ok(m) = serde_json::from_str::<Metadata>(&text) {
                        out.push(m);
                    }
                }
            }
        }
    }
    Ok(out)
}

/// What a `clean` run intends to do with one worktree.
pub struct Plan {
    pub view: View,
    pub dirty: bool,
    /// Some(reason) means it will be skipped for safety.
    pub skip: Option<String>,
}

/// Decide which worktrees to remove given the clean flags and guardrails.
pub fn plan_clean(name: Option<&str>, running: bool, all: bool, force: bool) -> Result<Vec<Plan>> {
    let views = collect()?;
    let mut plans = Vec::new();

    for view in views {
        // Guardrail 1: never touch the main worktree.
        if view.source == Source::Main {
            continue;
        }

        // Scope selection.
        let selected = if let Some(n) = name {
            view.source == Source::Owt && view.name.as_deref() == Some(n)
        } else if all {
            true // all non-main worktrees, including external
        } else if running {
            view.source == Source::Owt
        } else {
            view.source == Source::Owt && view.status == Liveness::Orphan
        };
        if !selected {
            continue;
        }

        // Guardrails 3 & 4: dirty / locked are skipped unless forced.
        let dirty = git::is_dirty(Path::new(&view.path)).unwrap_or(false);
        let skip = if view.locked && !force {
            Some("locked (use --force)".to_string())
        } else if dirty && !force {
            Some("uncommitted changes (use --force)".to_string())
        } else {
            None
        };

        plans.push(Plan { view, dirty, skip });
    }

    Ok(plans)
}

/// Remove a single worktree per its plan. Owt-owned worktrees also have their
/// branch and index entry removed; external ones keep their branch.
pub fn remove(plan: &Plan, force: bool) -> Result<()> {
    let path = Path::new(&plan.view.path);
    if path.exists() {
        git::worktree_remove(path, force)?;
    } else {
        // Directory already gone; clear the dangling registration.
        git::prune()?;
    }
    if plan.view.source == Source::Owt {
        if let Some(branch) = &plan.view.branch {
            let _ = git::branch_delete(branch);
        }
        if let Some(name) = &plan.view.name {
            let _ = Metadata::remove_index(name);
        }
    }
    Ok(())
}

/// Best-effort, cross-platform "is this pid alive?" check.
/// Defaults to `true` on error so we never auto-remove a possibly-live session.
fn is_alive(pid: u32) -> bool {
    #[cfg(windows)]
    {
        let out = Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
            .output();
        match out {
            Ok(o) => String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()),
            Err(_) => true,
        }
    }
    #[cfg(unix)]
    {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .map(|s| s.success())
            .unwrap_or(true)
    }
}

/// Normalize a path for comparison (canonicalize when possible).
fn normalize(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}
