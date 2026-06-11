//! Command-line interface definitions.

use clap::{Args, Parser, Subcommand, ValueEnum};

/// Ephemeral git worktree sandbox: create a worktree, run a command inside it,
/// then clean up. Acts as a transparent prefix, e.g. `owt -- npm test`.
#[derive(Parser, Debug)]
#[command(name = "owt", version, about, long_about = None)]
#[command(args_conflicts_with_subcommands = true)]
// Let a later flag override an earlier one so `owt @alias --from X` can override
// a --from baked into the alias instead of erroring on the duplicate.
#[command(args_override_self = true)]
pub struct Cli {
    /// Run options (used when no subcommand is given).
    #[command(flatten)]
    pub run: RunArgs,

    /// Subcommands for inspecting and cleaning up worktrees.
    #[command(subcommand)]
    pub sub: Option<Sub>,
}

#[derive(Args, Debug)]
pub struct RunArgs {
    /// Interactive mode: drop into a shell inside the worktree (never auto-cleaned).
    #[arg(short, long)]
    pub interactive: bool,

    /// Shell to launch in interactive mode (overrides config / $SHELL).
    #[arg(long)]
    pub shell: Option<String>,

    /// Run the command once per ref, each in its own worktree, in parallel.
    /// Comma-separated, e.g. `--each main,feat-a,feat-b`.
    #[arg(long, value_delimiter = ',')]
    pub each: Vec<String>,

    /// Run the command in N parallel worktrees from the same ref; each sees
    /// OWT_INDEX (0..N) and OWT_TOTAL for self-sharding.
    #[arg(long)]
    pub shard: Option<usize>,

    /// Source ref the worktree is created from (default: config `from`, else HEAD).
    #[arg(long)]
    pub from: Option<String>,

    /// Worktree / branch name. Random readable name if omitted.
    #[arg(long)]
    pub name: Option<String>,

    /// Where to place the worktree. Defaults to the app cache directory.
    #[arg(long)]
    pub dir: Option<String>,

    /// Extra path/glob to copy into the worktree (repeatable; adds to .worktreeinclude).
    #[arg(long)]
    pub include: Vec<String>,

    /// Command to run before the main command (e.g. "npm ci").
    #[arg(long)]
    pub setup: Option<String>,

    /// What to do with the worktree when a oneshot command finishes.
    #[arg(long, value_enum, default_value_t = OnExit::Discard)]
    pub on_exit: OnExit,

    /// Shorthand for `--on-exit keep`.
    #[arg(long)]
    pub keep: bool,

    /// Create the worktree in detached HEAD, without making an `owt/<name>`
    /// branch. Keeps the branch namespace clean; conflicts with --keep.
    #[arg(long)]
    pub detach: bool,

    /// The command (and its arguments) to run inside the worktree.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub command: Vec<String>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum OnExit {
    /// Remove the worktree, discard uncommitted changes, delete the branch.
    Discard,
    /// Remove the worktree but auto-commit changes and keep the branch.
    Keep,
}

#[derive(Subcommand, Debug)]
pub enum Sub {
    /// List worktrees created by owt (use --all to include external ones).
    List {
        /// Include worktrees not created by owt (read-only).
        #[arg(long)]
        all: bool,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Clean up worktrees. Default: only owt orphans (dead sessions).
    Clean {
        /// Name of a specific owt worktree to remove.
        name: Option<String>,
        /// Also remove still-running owt worktrees.
        #[arg(long)]
        running: bool,
        /// Remove ALL non-main worktrees, including external ones.
        #[arg(long)]
        all: bool,
        /// Force removal even with uncommitted changes / locks.
        #[arg(long)]
        force: bool,
        /// Skip the confirmation prompt.
        #[arg(long)]
        yes: bool,
        /// Show what would be removed without removing anything.
        #[arg(long)]
        dry_run: bool,
    },
}
