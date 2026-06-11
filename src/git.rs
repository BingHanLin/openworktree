//! Thin wrappers around the `git` binary.
//!
//! We shell out instead of linking libgit2 so we inherit the user's git
//! configuration, credentials and hooks for free.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Run a git command and return trimmed stdout, erroring on a non-zero exit.
fn git(args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .output()
        .with_context(|| format!("failed to spawn git {:?}", args))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git {:?} failed: {}", args, stderr.trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Path to the shared `.git` directory (works from the main repo or any worktree).
/// Errors when not inside any git tree.
pub fn common_dir() -> Result<PathBuf> {
    let out = git(&["rev-parse", "--git-common-dir"]).context("not inside a git repository")?;
    let path = PathBuf::from(out);
    // `--git-common-dir` may be relative to the current directory; make it absolute.
    let abs = std::fs::canonicalize(&path).unwrap_or(path);
    Ok(abs)
}

/// Top level of the current working tree (the source we copy includes from).
pub fn toplevel() -> Result<PathBuf> {
    let out = git(&["rev-parse", "--show-toplevel"]).context("not inside a git working tree")?;
    let path = PathBuf::from(out);
    Ok(std::fs::canonicalize(&path).unwrap_or(path))
}

/// Best-effort repository name, derived from the parent of the common `.git` dir.
pub fn repo_name(common_dir: &Path) -> String {
    common_dir
        .parent()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "repo".to_string())
}

/// Resolve a ref to a full commit SHA.
pub fn resolve_commit(reference: &str) -> Result<String> {
    git(&["rev-parse", reference]).with_context(|| format!("cannot resolve ref '{}'", reference))
}

/// Whether a branch already exists.
pub fn branch_exists(branch: &str) -> bool {
    git(&[
        "rev-parse",
        "--verify",
        "--quiet",
        &format!("refs/heads/{branch}"),
    ])
    .is_ok()
}

/// Create a worktree at `path` based on `from_ref`. With `Some(branch)` a new
/// branch is created and checked out; with `None` the worktree is left in
/// detached HEAD (no branch is created or occupied).
pub fn worktree_add(path: &Path, branch: Option<&str>, from_ref: &str) -> Result<()> {
    let path = path.to_string_lossy();
    let mut args = vec!["worktree", "add"];
    match branch {
        Some(b) => args.extend(["-b", b]),
        None => args.push("--detach"),
    }
    args.extend([path.as_ref(), from_ref]);
    git(&args).map(|_| ())
}

/// Stage everything and commit inside the worktree. Returns whether a commit
/// was actually created (false when there was nothing to commit).
pub fn commit_all(worktree: &Path, message: &str) -> Result<bool> {
    let wt = worktree.to_string_lossy();
    git(&["-C", &wt, "add", "-A"])?;
    // `diff --cached --quiet` exits non-zero when there are staged changes.
    let staged_changes = git(&["-C", &wt, "diff", "--cached", "--quiet"]).is_err();
    if !staged_changes {
        return Ok(false);
    }
    git(&["-C", &wt, "commit", "-m", message])?;
    Ok(true)
}

/// Remove a worktree directory (and its git registration).
/// When `force` is set we pass `-f -f`: git needs the doubled flag to override a
/// locked worktree (a single `-f` only overrides uncommitted changes).
pub fn worktree_remove(path: &Path, force: bool) -> Result<()> {
    let p = path.to_string_lossy();
    let mut args = vec!["worktree", "remove"];
    if force {
        args.push("--force");
        args.push("--force");
    }
    args.push(&p);
    git(&args).map(|_| ())
}

/// Delete a branch (force).
pub fn branch_delete(branch: &str) -> Result<()> {
    git(&["branch", "-D", branch]).map(|_| ())
}

/// Raw `git worktree list --porcelain` output.
pub fn worktree_list() -> Result<String> {
    git(&["worktree", "list", "--porcelain"])
}

/// Whether a worktree has uncommitted changes (tracked or untracked).
pub fn is_dirty(worktree: &Path) -> Result<bool> {
    let wt = worktree.to_string_lossy();
    let out = git(&["-C", &wt, "status", "--porcelain"])?;
    Ok(!out.trim().is_empty())
}

/// Prune administrative entries for worktrees whose directories are gone.
pub fn prune() -> Result<()> {
    git(&["worktree", "prune"]).map(|_| ())
}

/// Path to a worktree's private git admin directory (`.git/worktrees/<id>`).
/// This lives outside the working tree, so files here can never be committed,
/// and git removes the whole directory when the worktree is removed/pruned.
pub fn admin_dir(worktree: &Path) -> Result<PathBuf> {
    let wt = worktree.to_string_lossy();
    let out = git(&["-C", &wt, "rev-parse", "--git-dir"])?;
    let path = PathBuf::from(&out);
    let path = if path.is_relative() {
        worktree.join(path)
    } else {
        path
    };
    Ok(std::fs::canonicalize(&path).unwrap_or(path))
}
