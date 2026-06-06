//! A worktree session: creation, environment preparation, and cleanup.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

use crate::cli::OnExit;
use crate::git;
use crate::metadata::{self, Metadata, SCHEMA_VERSION};
use crate::naming;

pub struct Session {
    pub name: String,
    pub branch: String,
    pub worktree_path: PathBuf,
    pub on_exit: OnExit,
    /// Original command, used for the keep-mode auto-commit message.
    command: Vec<String>,
    /// Whether finishing should clean up automatically (oneshot yes, interactive no).
    auto_clean: bool,
    /// Set once cleanup has run so the Drop guard does not double-clean.
    cleaned: bool,
}

/// Parameters for creating a session.
pub struct CreateOpts<'a> {
    pub from: &'a str,
    pub name: Option<&'a str>,
    pub dir: Option<&'a str>,
    pub include: &'a [String],
    pub setup: Option<&'a str>,
    pub on_exit: OnExit,
    pub interactive: bool,
    pub command: Vec<String>,
}

impl Session {
    pub fn create(opts: CreateOpts) -> Result<Self> {
        let common_dir = git::common_dir()?;
        let repo = git::repo_name(&common_dir);
        let base_commit = git::resolve_commit(opts.from)?;
        // Source tree to copy includes from (captured before creating the worktree).
        let toplevel = git::toplevel()?;

        // Resolve a unique name and matching branch.
        let name = resolve_name(opts.name)?;
        let branch = format!("owt/{name}");

        // Resolve the worktree path.
        let worktree_path = match opts.dir {
            Some(d) => PathBuf::from(d),
            None => metadata::default_worktree_base()?.join(format!("{repo}__{name}")),
        };
        if worktree_path.exists() {
            bail!("worktree path already exists: {}", worktree_path.display());
        }
        if let Some(parent) = worktree_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }

        git::worktree_add(&worktree_path, &branch, opts.from)?;

        let mut session = Session {
            name: name.clone(),
            branch: branch.clone(),
            worktree_path: worktree_path.clone(),
            on_exit: opts.on_exit,
            command: opts.command.clone(),
            auto_clean: !opts.interactive,
            cleaned: false,
        };

        // From here on, failures must clean up the half-built worktree; the Drop
        // guard handles that for oneshot. For interactive, clean up explicitly.
        //
        // Copy / symlink gitignored-but-needed files from the source tree.
        match crate::include::apply(&toplevel, &worktree_path, opts.include) {
            Ok(n) if n > 0 => eprintln!("owt: included {n} path(s) into worktree"),
            Ok(_) => {}
            Err(e) => {
                session.cleanup_on_error();
                return Err(e).context("applying .worktreeinclude");
            }
        }

        if let Some(cmd) = opts.setup {
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
            mode: if opts.interactive { "interactive" } else { "oneshot" }.to_string(),
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
        // Best-effort: branch deletion can fail if already gone.
        let _ = git::branch_delete(&self.branch);
        Metadata::remove_index(&self.name)?;
        Ok(())
    }

    fn keep(&self) -> Result<()> {
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
        let _ = git::branch_delete(&self.branch);
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
fn resolve_name(requested: Option<&str>) -> Result<String> {
    match requested {
        Some(n) => {
            let branch = format!("owt/{n}");
            if git::branch_exists(&branch) {
                bail!("name '{}' already in use (branch {} exists)", n, branch);
            }
            Ok(n.to_string())
        }
        None => {
            for _ in 0..50 {
                let candidate = naming::random_name();
                if !git::branch_exists(&format!("owt/{candidate}")) {
                    return Ok(candidate);
                }
            }
            bail!("could not generate a unique worktree name");
        }
    }
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
