//! End-to-end tests driving the `owt` binary against throwaway git repos.
//! Each test gets an isolated repo and cache dir (via `OWT_CACHE_DIR`).

use std::path::Path;
use std::process::{Command, Output};
use tempfile::TempDir;

// Shell invocation prefix for running an arbitrary command string.
#[cfg(windows)]
const SH: [&str; 2] = ["cmd", "/C"];
#[cfg(not(windows))]
const SH: [&str; 2] = ["sh", "-c"];

#[cfg(windows)]
const CAT: &str = "type";
#[cfg(not(windows))]
const CAT: &str = "cat";

/// Build a shell command string that echoes an environment variable.
#[cfg(windows)]
fn echo_env(var: &str) -> String {
    format!("echo %{var}%")
}
#[cfg(not(windows))]
fn echo_env(var: &str) -> String {
    format!("echo ${var}")
}

struct Env {
    repo: TempDir,
    cache: TempDir,
}

fn git(repo: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(repo)
        .status()
        .expect("spawn git");
    assert!(status.success(), "git {args:?} failed");
}

fn git_out(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("spawn git");
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn setup() -> Env {
    let repo = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();
    // Force the initial branch to "main" regardless of the host's git default
    // (CI runners may default to "master").
    git(
        repo.path(),
        &["-c", "init.defaultBranch=main", "init", "-q"],
    );
    git(repo.path(), &["config", "user.email", "t@t.co"]);
    git(repo.path(), &["config", "user.name", "t"]);
    std::fs::write(repo.path().join("file.txt"), "hello\n").unwrap();
    git(repo.path(), &["add", "-A"]);
    git(repo.path(), &["commit", "-qm", "init"]);
    Env { repo, cache }
}

fn owt(env: &Env, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_owt"))
        .args(args)
        .current_dir(env.repo.path())
        .env("OWT_CACHE_DIR", env.cache.path())
        .env("OWT_CONFIG", env.cache.path().join("config.toml"))
        .output()
        .expect("spawn owt")
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}

/// Number of linked worktrees (excludes the main one).
fn linked_worktree_count(repo: &Path) -> usize {
    git_out(repo, &["worktree", "list", "--porcelain"])
        .split("\n\n")
        .filter(|b| b.trim_start().starts_with("worktree "))
        .count()
        .saturating_sub(1)
}

#[test]
fn oneshot_runs_in_worktree_and_cleans_up() {
    let env = setup();
    let out = owt(&env, &["--", "git", "rev-parse", "--show-toplevel"]);
    assert!(out.status.success());

    // The command ran somewhere other than the source repo.
    let toplevel = stdout(&out).trim().to_string();
    assert!(!toplevel.is_empty());
    assert_ne!(
        Path::new(&toplevel).canonicalize().ok(),
        env.repo.path().canonicalize().ok()
    );

    // Nothing left behind.
    assert_eq!(linked_worktree_count(env.repo.path()), 0);
    assert!(git_out(env.repo.path(), &["branch", "--list", "owt/*"])
        .trim()
        .is_empty());
}

#[test]
fn exit_code_is_passed_through() {
    let env = setup();
    let out = owt(&env, &["--", SH[0], SH[1], "exit 42"]);
    assert_eq!(out.status.code(), Some(42));
}

#[test]
fn worktree_is_isolated_from_main() {
    let env = setup();
    let out = owt(&env, &["--", SH[0], SH[1], "echo LEAK> file.txt"]);
    assert!(out.status.success());

    let main = std::fs::read_to_string(env.repo.path().join("file.txt")).unwrap();
    assert!(main.contains("hello"));
    assert!(!main.contains("LEAK"));
}

#[test]
fn keep_retains_branch_without_leaking_metadata() {
    let env = setup();
    let out = owt(
        &env,
        &[
            "--name",
            "kept",
            "--keep",
            "--",
            SH[0],
            SH[1],
            "echo x> newfile.txt",
        ],
    );
    assert!(out.status.success());

    // Branch retained.
    assert!(git_out(env.repo.path(), &["branch", "--list", "owt/kept"]).contains("owt/kept"));
    // The user's real file made it onto the branch...
    let tree = git_out(
        env.repo.path(),
        &["ls-tree", "-r", "--name-only", "owt/kept"],
    );
    assert!(tree.contains("newfile.txt"));
    // ...but our metadata never did.
    assert!(!tree.contains("owt-meta.json"));
    // Worktree directory itself is gone.
    assert_eq!(linked_worktree_count(env.repo.path()), 0);

    git(env.repo.path(), &["branch", "-D", "owt/kept"]);
}

#[test]
fn detach_creates_no_branch_but_stays_tracked() {
    let env = setup();
    // Interactive keeps the worktree (never auto-cleaned); --detach must build it
    // in detached HEAD without creating an owt/* branch. `git` is a stand-in
    // shell that exits immediately.
    let out = owt(&env, &["-i", "--detach", "--name", "dx", "--shell", "git"]);
    let _ = out.status;

    // The worktree exists but no branch was created.
    assert_eq!(linked_worktree_count(env.repo.path()), 1);
    assert!(
        git_out(env.repo.path(), &["branch", "--list", "owt/*"])
            .trim()
            .is_empty(),
        "detached worktree must not create an owt/* branch"
    );

    // It is still recognized as owt-owned via the central index (by path),
    // not via a branch prefix.
    let s = stdout(&owt(&env, &["list"]));
    assert!(
        s.contains("dx"),
        "detached worktree should be listed as owt: {s}"
    );

    // And clean can target it by name.
    let out = owt(&env, &["clean", "dx", "--yes"]);
    assert!(out.status.success());
    assert_eq!(linked_worktree_count(env.repo.path()), 0);
}

#[test]
fn new_creates_persistent_worktree_and_prints_path() {
    let env = setup();
    let out = owt(&env, &["new", "--name", "foo"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // stdout is exactly the worktree path (for `cd "$(owt new)"`).
    let path = stdout(&out).trim().to_string();
    assert!(
        Path::new(&path).is_dir(),
        "printed path should exist: {path}"
    );
    // Created with a branch by default; the worktree persists (no auto-clean).
    assert!(git_out(env.repo.path(), &["branch", "--list", "owt/foo"]).contains("owt/foo"));
    assert_eq!(linked_worktree_count(env.repo.path()), 1);

    // Listed as standalone, and a plain `clean` must NOT remove it.
    let s = stdout(&owt(&env, &["list"]));
    assert!(s.contains("standalone"), "should be standalone: {s}");
    let _ = owt(&env, &["clean", "--yes"]);
    assert_eq!(
        linked_worktree_count(env.repo.path()),
        1,
        "default clean must leave standalone worktrees"
    );

    // Removable by name.
    let out = owt(&env, &["clean", "foo", "--yes"]);
    assert!(out.status.success());
    assert_eq!(linked_worktree_count(env.repo.path()), 0);
    assert!(git_out(env.repo.path(), &["branch", "--list", "owt/foo"])
        .trim()
        .is_empty());
}

#[test]
fn new_detach_creates_no_branch() {
    let env = setup();
    let out = owt(&env, &["new", "--detach", "--name", "nob"]);
    assert!(out.status.success());
    assert!(
        git_out(env.repo.path(), &["branch", "--list", "owt/*"])
            .trim()
            .is_empty(),
        "--detach must not create a branch"
    );
    // Still tracked (standalone) so it can be cleaned by name.
    let out = owt(&env, &["clean", "nob", "--yes"]);
    assert!(out.status.success());
    assert_eq!(linked_worktree_count(env.repo.path()), 0);
}

#[test]
fn parent_dir_places_auto_subdir_under_parent() {
    let env = setup();
    let parent = env.cache.path().join("myparent");
    let out = owt(
        &env,
        &[
            "--parent-dir",
            parent.to_str().unwrap(),
            "--",
            "git",
            "rev-parse",
            "--show-toplevel",
        ],
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // The worktree sat directly under the given parent dir, in an auto subdir.
    let toplevel = stdout(&out).trim().to_string();
    let got_parent = Path::new(&toplevel).parent().unwrap();
    assert_eq!(
        got_parent.canonicalize().ok(),
        parent.canonicalize().ok(),
        "worktree should sit directly under --parent-dir (got {toplevel})"
    );
    // Oneshot discard cleaned it up.
    assert_eq!(linked_worktree_count(env.repo.path()), 0);
}

#[test]
fn dir_and_parent_dir_are_mutually_exclusive() {
    let env = setup();
    let out = owt(
        &env,
        &["--dir", "x", "--parent-dir", "y", "--", "git", "status"],
    );
    assert!(!out.status.success());
    assert_eq!(linked_worktree_count(env.repo.path()), 0);
}

#[test]
fn unknown_flag_errors_instead_of_becoming_the_command() {
    let env = setup();
    // A mistyped owt flag must be reported, not silently swallowed into the
    // command (which would build a worktree and try to run the flag as a program).
    let out = owt(
        &env,
        &[
            "--parnet-dir",
            "x",
            "--",
            "git",
            "rev-parse",
            "--show-toplevel",
        ],
    );
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("unexpected argument"),
        "stderr should report the bad flag: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // No worktree was created.
    assert_eq!(linked_worktree_count(env.repo.path()), 0);
}

#[test]
fn command_flags_after_separator_still_work() {
    let env = setup();
    // The command's own hyphenated args (after `--`) must reach the command,
    // not be parsed as owt flags.
    let out = owt(&env, &["--", "git", "rev-parse", "--show-toplevel"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!stdout(&out).trim().is_empty());
}

#[test]
fn detach_conflicts_with_keep() {
    let env = setup();
    // keep needs a branch to retain its commit; detach has none.
    let out = owt(&env, &["--detach", "--keep", "--", SH[0], SH[1], "echo hi"]);
    assert!(!out.status.success());
    assert_eq!(
        linked_worktree_count(env.repo.path()),
        0,
        "must not create a worktree on a rejected combo"
    );
}

#[test]
fn worktreeinclude_copies_and_negates_and_extra_include() {
    let env = setup();
    let repo = env.repo.path();
    std::fs::write(repo.join(".gitignore"), "*.local\nsecret.txt\n").unwrap();
    std::fs::write(repo.join("keep.local"), "KEEP").unwrap();
    std::fs::write(repo.join("drop.local"), "DROP").unwrap();
    std::fs::write(repo.join("secret.txt"), "SECRET").unwrap();
    std::fs::write(repo.join(".worktreeinclude"), "*.local\n!drop.local\n").unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-qm", "add include"]);

    // keep.local should be present, drop.local excluded, secret.txt via --include.
    let cmd = format!("{CAT} keep.local && {CAT} secret.txt");
    let out = owt(&env, &["--include", "secret.txt", "--", SH[0], SH[1], &cmd]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let s = stdout(&out);
    assert!(s.contains("KEEP"));
    assert!(s.contains("SECRET"));

    // drop.local must not have been copied: cat'ing it should fail.
    let cmd = format!("{CAT} drop.local");
    let out = owt(&env, &["--", SH[0], SH[1], &cmd]);
    assert!(
        !out.status.success(),
        "drop.local should not exist in worktree"
    );
}

#[test]
fn metadata_not_visible_in_working_tree() {
    let env = setup();
    let out = owt(&env, &["--", "git", "status", "--porcelain"]);
    assert!(out.status.success());
    assert!(stdout(&out).trim().is_empty(), "worktree should be clean");
}

#[test]
fn list_distinguishes_owt_from_external() {
    let env = setup();
    let repo = env.repo.path();
    let owt_wt = env.cache.path().join("owt_orphan");
    let ext_wt = env.cache.path().join("ext");
    git(
        repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "owt/orphan",
            owt_wt.to_str().unwrap(),
            "HEAD",
        ],
    );
    git(
        repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feat/ext",
            ext_wt.to_str().unwrap(),
            "HEAD",
        ],
    );

    // Default list: only owt-owned.
    let s = stdout(&owt(&env, &["list"]));
    assert!(s.contains("orphan"));
    assert!(!s.contains("external"));

    // --all: includes the external one.
    let s = stdout(&owt(&env, &["list", "--all"]));
    assert!(s.contains("orphan"));
    assert!(s.contains("external"));
}

#[test]
fn clean_default_removes_owt_orphans_only() {
    let env = setup();
    let repo = env.repo.path();
    let owt_wt = env.cache.path().join("owt_orphan");
    let ext_wt = env.cache.path().join("ext");
    git(
        repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "owt/orphan",
            owt_wt.to_str().unwrap(),
            "HEAD",
        ],
    );
    git(
        repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feat/ext",
            ext_wt.to_str().unwrap(),
            "HEAD",
        ],
    );

    let out = owt(&env, &["clean", "--yes"]);
    assert!(out.status.success());

    // owt orphan removed (branch gone), external untouched.
    assert!(git_out(repo, &["branch", "--list", "owt/orphan"])
        .trim()
        .is_empty());
    assert!(git_out(repo, &["branch", "--list", "feat/ext"]).contains("feat/ext"));
    assert_eq!(linked_worktree_count(repo), 1); // only the external remains
}

#[test]
fn clean_all_skips_dirty_without_force_then_removes_with_force() {
    let env = setup();
    let repo = env.repo.path();
    let ext_wt = env.cache.path().join("ext");
    git(
        repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feat/ext",
            ext_wt.to_str().unwrap(),
            "HEAD",
        ],
    );
    std::fs::write(ext_wt.join("file.txt"), "hello\nDIRTY\n").unwrap();

    // Without --force the dirty external is skipped.
    let out = owt(&env, &["clean", "--all", "--yes"]);
    assert!(out.status.success());
    assert_eq!(
        linked_worktree_count(repo),
        1,
        "dirty external must be kept"
    );

    // With --force it is removed, but its branch is retained (external).
    let out = owt(&env, &["clean", "--all", "--force", "--yes"]);
    assert!(out.status.success());
    assert_eq!(linked_worktree_count(repo), 0);
    assert!(git_out(repo, &["branch", "--list", "feat/ext"]).contains("feat/ext"));
}

#[test]
fn clean_recovers_worktree_with_missing_gitlink() {
    let env = setup();
    let repo = env.repo.path();

    // Create an owt worktree (tracked in the index), then simulate the breakage:
    // delete its `.git` gitlink so `git worktree remove` would fail validation.
    let out = owt(&env, &["new", "--name", "broke"]);
    assert!(out.status.success());
    let path = stdout(&out).trim().to_string();
    std::fs::remove_file(Path::new(&path).join(".git")).unwrap();

    // A plain `clean` must now recover it (prune + delete dir), not error out.
    let out = owt(&env, &["clean", "broke", "--yes"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !Path::new(&path).exists(),
        "leftover directory should be removed"
    );
    assert_eq!(linked_worktree_count(repo), 0);
    // Index entry cleared...
    assert!(!stdout(&owt(&env, &["list"])).contains("broke"));
    // ...but the branch is kept (it may hold the only copy of any commits).
    assert!(git_out(repo, &["branch", "--list", "owt/broke"]).contains("owt/broke"));

    git(repo, &["branch", "-D", "owt/broke"]);
}

#[test]
fn clean_never_removes_main_worktree() {
    let env = setup();
    let repo = env.repo.path();
    // Even the nuclear option leaves the main worktree intact.
    let out = owt(&env, &["clean", "--all", "--force", "--yes"]);
    assert!(out.status.success());
    assert!(repo.join("file.txt").exists());
    assert!(repo.join(".git").exists());
}

#[test]
fn interactive_honors_shell_and_keeps_worktree() {
    let env = setup();
    // Use `git` as a stand-in "shell": it exits immediately (no hang) and is not
    // the default cmd/sh, proving --shell was honored.
    let out = owt(&env, &["-i", "--shell", "git"]);
    // git with no args returns non-zero, but the point is it ran and returned.
    let _ = out.status;
    // Interactive mode must NOT auto-clean its worktree.
    assert_eq!(linked_worktree_count(env.repo.path()), 1);
}

#[test]
fn alias_expands_from_config() {
    let env = setup();
    let cfg = env.cache.path().join("config.toml");
    std::fs::write(
        &cfg,
        format!(
            "[alias.hi]\nargs = [\"--\", \"{}\", \"{}\", \"echo ALIASED\"]\n",
            SH[0], SH[1]
        ),
    )
    .unwrap();

    let out = owt(&env, &["@hi"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout(&out).contains("ALIASED"));
}

#[test]
fn alias_extra_args_override() {
    let env = setup();
    let cfg = env.cache.path().join("config.toml");
    // Alias supplies flags only (no trailing command); user appends more flags
    // and the command. A later --name overrides the alias's (clap: last wins).
    std::fs::write(
        &cfg,
        "[alias.k]\nargs = [\"--keep\", \"--name\", \"aliasname\"]\n",
    )
    .unwrap();

    let out = owt(
        &env,
        &["@k", "--name", "overridden", "--", SH[0], SH[1], "echo hi"],
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let branches = git_out(env.repo.path(), &["branch", "--list", "owt/*"]);
    assert!(branches.contains("owt/overridden"), "branches: {branches}");
    assert!(!branches.contains("owt/aliasname"));
    git(env.repo.path(), &["branch", "-D", "owt/overridden"]);
}

#[test]
fn config_default_from_is_used() {
    let env = setup();
    let repo = env.repo.path();
    // Branch 'other' has a marker file that main lacks.
    git(repo, &["checkout", "-q", "-b", "other"]);
    std::fs::write(repo.join("marker.txt"), "OTHER").unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-qm", "other marker"]);
    git(repo, &["checkout", "-q", "main"]);

    std::fs::write(env.cache.path().join("config.toml"), "from = \"other\"\n").unwrap();

    let cmd = format!("{CAT} marker.txt");
    let out = owt(&env, &["--", SH[0], SH[1], &cmd]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout(&out).contains("OTHER"),
        "worktree should be based on 'other'"
    );
}

#[test]
fn from_flag_overrides_config_default() {
    let env = setup();
    let repo = env.repo.path();
    git(repo, &["checkout", "-q", "-b", "other"]);
    std::fs::write(repo.join("marker.txt"), "OTHER").unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-qm", "other marker"]);
    git(repo, &["checkout", "-q", "main"]);

    std::fs::write(env.cache.path().join("config.toml"), "from = \"other\"\n").unwrap();

    // --from main overrides the config default, so marker.txt must be absent.
    let cmd = format!("{CAT} marker.txt");
    let out = owt(&env, &["--from", "main", "--", SH[0], SH[1], &cmd]);
    assert!(
        !out.status.success(),
        "from=main worktree should not contain marker.txt"
    );
}

#[test]
fn unknown_alias_errors() {
    let env = setup();
    let out = owt(&env, &["@nope"]);
    assert!(!out.status.success());
}

#[test]
fn rejects_silently_ignored_flag_combos() {
    let env = setup();

    // --shell without -i.
    let out = owt(&env, &["--shell", "bash", "--", "git", "status"]);
    assert!(!out.status.success());

    // --keep with -i (interactive never auto-cleans).
    let out = owt(&env, &["-i", "--keep"]);
    assert!(!out.status.success());
    assert_eq!(
        linked_worktree_count(env.repo.path()),
        0,
        "must not create a worktree"
    );

    // --name / --dir with fan-out.
    let out = owt(
        &env,
        &["--each", "main", "--name", "x", "--", "git", "status"],
    );
    assert!(!out.status.success());
}

#[test]
fn each_runs_per_ref_and_cleans_up() {
    let env = setup();
    let repo = env.repo.path();
    git(repo, &["branch", "b"]);

    let cmd = echo_env("OWT_REF");
    let out = owt(&env, &["--each", "main,b", "--", SH[0], SH[1], &cmd]);
    assert!(out.status.success());
    let s = stdout(&out);
    assert!(s.contains("main"), "missing main output: {s}");
    assert!(s.contains("b"), "missing b output: {s}");
    // Fan-out worktrees are discarded by default.
    assert_eq!(linked_worktree_count(repo), 0);
}

#[test]
fn each_aggregates_failure_into_nonzero_exit() {
    let env = setup();
    git(env.repo.path(), &["branch", "b"]);
    let out = owt(&env, &["--each", "main,b", "--", SH[0], SH[1], "exit 3"]);
    assert!(!out.status.success(), "any failing job should fail the run");
}

#[test]
fn shard_exposes_index_to_each_worktree() {
    let env = setup();
    let cmd = echo_env("OWT_INDEX");
    let out = owt(&env, &["--shard", "3", "--", SH[0], SH[1], &cmd]);
    assert!(out.status.success());
    let s = stdout(&out);
    for i in ["0", "1", "2"] {
        assert!(s.contains(i), "missing shard index {i} in: {s}");
    }
    assert_eq!(linked_worktree_count(env.repo.path()), 0);
}

#[test]
fn dry_run_removes_nothing() {
    let env = setup();
    let repo = env.repo.path();
    let owt_wt = env.cache.path().join("owt_orphan");
    git(
        repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "owt/orphan",
            owt_wt.to_str().unwrap(),
            "HEAD",
        ],
    );

    let out = owt(&env, &["clean", "--dry-run"]);
    assert!(out.status.success());
    assert!(stdout(&out).contains("dry run"));
    assert_eq!(
        linked_worktree_count(repo),
        1,
        "dry-run must not remove anything"
    );
}
