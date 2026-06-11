//! A worktree session: creation, environment preparation, and cleanup.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

use crate::cli::OnExit;
use crate::git;
use crate::metadata::{self, Metadata, SCHEMA_VERSION};
use crate::naming;

pub struct Session {
    pub name: String,
    /// The `owt/<name>` branch, or `None` for a detached worktree.
    pub branch: Option<String>,
    pub worktree_path: PathBuf,
    pub on_exit: OnExit,
    /// Original command, used for the keep-mode auto-commit message.
    command: Vec<String>,
    /// Whether finishing should clean up automatically (oneshot yes, interactive no).
    auto_clean: bool,
    /// Set once cleanup has run so the Drop guard does not double-clean.
    cleaned: bool,
}

/// How a worktree session is used. Determines whether the worktree is
/// auto-cleaned on finish and the `mode` label stored in metadata.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// `owt -- <cmd>`: run a command, then clean up (auto-cleaned).
    Oneshot,
    /// `owt -i`: drop into a shell; kept until `owt clean`.
    Interactive,
    /// `owt new`: create and exit; kept until `owt clean`.
    Standalone,
}

impl Mode {
    /// Oneshot sessions clean themselves up; the rest persist until `owt clean`.
    fn auto_clean(self) -> bool {
        matches!(self, Mode::Oneshot)
    }

    fn label(self) -> &'static str {
        match self {
            Mode::Oneshot => "oneshot",
            Mode::Interactive => "interactive",
            Mode::Standalone => "standalone",
        }
    }
}

/// Parameters for creating a session.
pub struct CreateOpts<'a> {
    pub from: &'a str,
    pub name: Option<&'a str>,
    pub dir: Option<&'a str>,
    /// Parent directory the worktree's auto `<repo>__<name>` subdir goes under.
    /// Mutually exclusive with `dir` (which is verbatim).
    pub parent_dir: Option<&'a str>,
    pub include: &'a [String],
    pub setup: Option<&'a str>,
    pub on_exit: OnExit,
    /// Create the worktree detached (no `owt/<name>` branch).
    pub detach: bool,
    /// How the worktree is used, which decides auto-cleanup and the recorded
    /// mode label.
    pub mode: Mode,
    pub command: Vec<String>,
    /// Print step-by-step progress to stderr (off for fan-out to avoid interleaving).
    pub progress: bool,
}

impl Session {
    pub fn create(opts: CreateOpts) -> Result<Self> {
        let common_dir = git::common_dir()?;
        let repo = git::repo_name(&common_dir);
        let base_commit = git::resolve_commit(opts.from)?;
        // Source tree to copy includes from (captured before creating the worktree).
        let toplevel = git::toplevel()?;

        // Resolve a unique name; detached worktrees have no branch.
        let name = resolve_name(opts.name, opts.detach)?;
        let branch = if opts.detach {
            None
        } else {
            Some(format!("owt/{name}"))
        };

        // Resolve the worktree path. --dir is used verbatim; otherwise the
        // worktree gets an auto `<repo>__<name>` subdir under either --parent-dir
        // or the default cache base.
        let worktree_path = if let Some(d) = opts.dir {
            PathBuf::from(d)
        } else {
            let base = match opts.parent_dir {
                Some(b) => PathBuf::from(b),
                None => metadata::default_worktree_base()?,
            };
            base.join(format!("{repo}__{name}"))
        };
        if worktree_path.exists() {
            bail!("worktree path already exists: {}", worktree_path.display());
        }
        if let Some(parent) = worktree_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }

        let short = base_commit.get(..8).unwrap_or(&base_commit);
        if opts.progress {
            eprintln!(
                "owt: creating worktree '{name}' from {} ({short})",
                opts.from
            );
        }
        git::worktree_add(&worktree_path, branch.as_deref(), opts.from)?;

        let mut session = Session {
            name: name.clone(),
            branch: branch.clone(),
            worktree_path: worktree_path.clone(),
            on_exit: opts.on_exit,
            command: opts.command.clone(),
            auto_clean: opts.mode.auto_clean(),
            cleaned: false,
        };

        // From here on, failures must clean up the half-built worktree; the Drop
        // guard handles that for oneshot. For interactive, clean up explicitly.
        //
        // Copy / symlink gitignored-but-needed files from the source tree.
        match crate::include::apply(&toplevel, &worktree_path, opts.include) {
            Ok(n) if n > 0 && opts.progress => {
                eprintln!("owt: included {n} path(s) into worktree")
            }
            Ok(_) => {}
            Err(e) => {
                session.cleanup_on_error();
                return Err(e).context("applying .worktreeinclude");
            }
        }

        if let Some(cmd) = opts.setup {
            if opts.progress {
                eprintln!("owt: running setup: {cmd}");
            }
            if let Err(e) = run_in(&worktree_path, cmd) {
                session.cleanup_on_error();
                return Err(e).context("setup command failed");
            }
        }

        let meta = Metadata {
            schema_version: SCHEMA_VERSION,
            name,
            branch,
            from_ref: opts.from.to_string(),
            base_commit,
            worktree_path: worktree_path.to_string_lossy().to_string(),
            repo_common_dir: common_dir.to_string_lossy().to_string(),
            mode: opts.mode.label().to_string(),
            command: opts.command,
            on_exit: match opts.on_exit {
                OnExit::Discard => "discard",
                OnExit::Keep => "keep",
            }
            .to_string(),
            pid: std::process::id(),
            created_at: chrono::Utc::now().to_rfc3339(),
            status: "running".to_string(),
        };
        // Store the co-located copy in the worktree's private git admin dir.
        let admin = match git::admin_dir(&worktree_path) {
            Ok(dir) => dir,
            Err(e) => {
                session.cleanup_on_error();
                return Err(e).context("locating worktree git admin dir");
            }
        };
        if let Err(e) = meta.write(&admin) {
            session.cleanup_on_error();
            return Err(e).context("writing worktree metadata");
        }

        if opts.progress {
            eprintln!("owt: ready at {}", worktree_path.display());
        }
        Ok(session)
    }

    /// Run the configured oneshot cleanup policy and disarm the Drop guard.
    pub fn finish(&mut self) -> Result<()> {
        if self.cleaned {
            return Ok(());
        }
        match self.on_exit {
            OnExit::Discard => self.discard()?,
            OnExit::Keep => self.keep()?,
        }
        self.cleaned = true;
        Ok(())
    }

    fn discard(&self) -> Result<()> {
        git::worktree_remove(&self.worktree_path, true)?;
        // Best-effort: branch deletion can fail if already gone. Detached
        // worktrees have no branch to delete.
        if let Some(branch) = &self.branch {
            let _ = git::branch_delete(branch);
        }
        Metadata::remove_index(&self.name)?;
        Ok(())
    }

    fn keep(&self) -> Result<()> {
        // Keep relies on a branch to retain the auto-commit; a detached commit
        // would become unreachable once the worktree is removed. The CLI rejects
        // --detach + keep, so this is just a safety net.
        if self.branch.is_none() {
            bail!("internal error: keep policy requires a branch (detached session)");
        }
        let message = format!(
            "owt: {} @ {}",
            self.command.join(" "),
            chrono::Utc::now().to_rfc3339()
        );
        // Metadata lives in the git admin dir, not the working tree, so there is
        // nothing to scrub before committing. An empty branch is still kept.
        git::commit_all(&self.worktree_path, &message)?;
        git::worktree_remove(&self.worktree_path, true)?;
        // Branch is intentionally retained.
        Metadata::remove_index(&self.name)?;
        Ok(())
    }

    /// Best-effort teardown used when creation fails partway through.
    fn cleanup_on_error(&mut self) {
        let _ = git::worktree_remove(&self.worktree_path, true);
        if let Some(branch) = &self.branch {
            let _ = git::branch_delete(branch);
        }
        let _ = Metadata::remove_index(&self.name);
        self.cleaned = true;
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Safety net: a oneshot session that was never finished (panic, early
        // return) is force-discarded so we never leak a worktree.
        if self.auto_clean && !self.cleaned {
            let _ = self.discard();
        }
    }
}

/// Generate or validate the worktree name.
///
/// A name is taken if its `owt/<name>` branch exists or (for detached
/// worktrees, which have no branch) a central index entry already owns it.
fn resolve_name(requested: Option<&str>, detach: bool) -> Result<String> {
    match requested {
        Some(n) => {
            if let Some(reason) = name_conflict(n, detach) {
                bail!("name '{}' already in use ({})", n, reason);
            }
            Ok(n.to_string())
        }
        None => {
            for _ in 0..50 {
                let candidate = naming::random_name();
                if name_conflict(&candidate, detach).is_none() {
                    return Ok(candidate);
                }
            }
            bail!("could not generate a unique worktree name");
        }
    }
}

/// Describe why `name` is unavailable, or `None` if it is free. For branch-based
/// sessions a matching `owt/<name>` branch is the conflict; for detached ones
/// only the index can reveal a collision.
fn name_conflict(name: &str, detach: bool) -> Option<String> {
    let branch = format!("owt/{name}");
    if !detach && git::branch_exists(&branch) {
        return Some(format!("branch {branch} exists"));
    }
    if Metadata::index_exists(name) {
        return Some("an owt worktree with this name already exists".to_string());
    }
    None
}

/// Run a shell command string inside a directory, inheriting stdio.
fn run_in(dir: &Path, command: &str) -> Result<()> {
    use std::process::Command;
    let mut child = if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.args(["/C", command]);
        c
    } else {
        let mut c = Command::new("sh");
        c.args(["-c", command]);
        c
    };
    let status = child
        .current_dir(dir)
        .status()
        .with_context(|| format!("running '{command}'"))?;
    if !status.success() {
        bail!("command '{}' exited with {}", command, status);
    }
    Ok(())
}
