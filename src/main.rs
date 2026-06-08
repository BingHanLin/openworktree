//! openworktree (`owt`): ephemeral git worktree sandbox.

mod cli;
mod config;
mod git;
mod include;
mod metadata;
mod naming;
mod session;
mod signal;
mod state;

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use config::Config;

use anyhow::{bail, Context, Result};
use clap::Parser;

use cli::{Cli, OnExit, RunArgs, Sub};
use session::{CreateOpts, Session};

fn main() {
    let code = match run_cli() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("owt: error: {e:#}");
            1
        }
    };
    std::process::exit(code);
}

fn run_cli() -> Result<i32> {
    let argv = expand_alias(std::env::args().collect())?;
    let cli = Cli::parse_from(argv);
    dispatch(cli)
}

/// If the first argument is `@name`, replace it with the alias's args from
/// config (later args are kept, so they can extend or override the alias).
fn expand_alias(argv: Vec<String>) -> Result<Vec<String>> {
    let Some(first) = argv.get(1) else {
        return Ok(argv);
    };
    let Some(name) = first.strip_prefix('@') else {
        return Ok(argv);
    };
    if name.is_empty() {
        bail!("missing alias name after '@'");
    }

    let config = Config::load()?;
    let alias = config.alias.get(name).with_context(|| {
        format!("unknown alias '@{name}' (define [alias.{name}] in config.toml)")
    })?;

    let mut out = Vec::with_capacity(argv.len() + alias.args.len());
    out.push(argv[0].clone());
    out.extend(alias.args.iter().cloned());
    out.extend(argv[2..].iter().cloned());
    Ok(out)
}

fn dispatch(cli: Cli) -> Result<i32> {
    match cli.sub {
        Some(Sub::List { all, json }) => cmd_list(all, json),
        Some(Sub::Clean {
            name,
            running,
            all,
            force,
            yes,
            dry_run,
        }) => cmd_clean(name.as_deref(), running, all, force, yes, dry_run),
        None => run(cli.run),
    }
}

fn run(args: RunArgs) -> Result<i32> {
    let on_exit = if args.keep { OnExit::Keep } else { args.on_exit };
    let fan_out = !args.each.is_empty() || args.shard.is_some();

    // --shell is only meaningful for interactive mode.
    if args.shell.is_some() && !args.interactive {
        bail!("--shell only applies to interactive mode (-i)");
    }

    if args.interactive {
        if fan_out {
            bail!("interactive mode (-i) cannot be combined with --each / --shard");
        }
        if !args.command.is_empty() {
            bail!("interactive mode (-i) does not take a command");
        }
        // Interactive worktrees are never auto-cleaned, so a keep policy is a no-op.
        if args.keep || args.on_exit == OnExit::Keep {
            bail!(
                "--keep / --on-exit have no effect with -i (interactive never auto-cleans; \
                 use `owt clean` afterwards)"
            );
        }
        return run_interactive(&args, on_exit);
    }

    if fan_out {
        if !args.each.is_empty() && args.shard.is_some() {
            bail!("--each and --shard are mutually exclusive");
        }
        // Each fan-out job creates its own auto-named worktree, so a single
        // --name / --dir cannot apply to all of them.
        if args.name.is_some() || args.dir.is_some() {
            bail!("--name / --dir cannot be used with --each / --shard (each job gets its own worktree)");
        }
        if args.command.is_empty() {
            bail!("no command given for --each / --shard");
        }
        return run_fanout(&args, on_exit);
    }

    if args.command.is_empty() {
        bail!("no command given; use `owt -- <command>` or `owt -i`");
    }
    run_oneshot(&args, on_exit)
}

fn run_oneshot(args: &RunArgs, on_exit: OnExit) -> Result<i32> {
    let mut session = Session::create(CreateOpts {
        from: &args.from,
        name: args.name.as_deref(),
        dir: args.dir.as_deref(),
        include: &args.include,
        setup: args.setup.as_deref(),
        on_exit,
        interactive: false,
        command: args.command.clone(),
        progress: true,
    })?;

    let code = exec(&session.worktree_path, &args.command, &[])?;

    if let Err(e) = session.finish() {
        eprintln!("owt: warning: cleanup failed: {e:#}");
    }
    Ok(code)
}

fn run_interactive(args: &RunArgs, on_exit: OnExit) -> Result<i32> {
    let session = Session::create(CreateOpts {
        from: &args.from,
        name: args.name.as_deref(),
        dir: args.dir.as_deref(),
        include: &args.include,
        setup: args.setup.as_deref(),
        on_exit,
        interactive: true,
        command: Vec::new(),
        progress: true,
    })?;

    eprintln!(
        "owt: entered worktree '{}' (branch {})\n     exit the shell to return; this worktree is NOT auto-cleaned.",
        session.worktree_path.display(),
        session.branch
    );

    let shell = Config::load()?.resolve_shell(args.shell.as_deref());
    let status = Command::new(&shell)
        .current_dir(&session.worktree_path)
        .status()
        .with_context(|| format!("launching shell '{shell}'"))?;

    Ok(status.code().unwrap_or(0))
}

/// Run each ref (`--each`) or shard (`--shard`) in its own worktree in parallel.
/// Worktree creation/cleanup is serialized (git mutates the repo); the commands
/// themselves run concurrently. Returns 0 only if every job succeeded.
fn run_fanout(args: &RunArgs, on_exit: OnExit) -> Result<i32> {
    signal::install();

    // (label, from_ref, extra_env) per job.
    let jobs: Vec<(String, String, Vec<(String, String)>)> = if let Some(n) = args.shard {
        if n == 0 {
            bail!("--shard must be >= 1");
        }
        (0..n)
            .map(|i| {
                (
                    format!("shard-{i}"),
                    args.from.clone(),
                    vec![
                        ("OWT_INDEX".to_string(), i.to_string()),
                        ("OWT_TOTAL".to_string(), n.to_string()),
                    ],
                )
            })
            .collect()
    } else {
        args.each
            .iter()
            .map(|r| (r.clone(), r.clone(), vec![("OWT_REF".to_string(), r.clone())]))
            .collect()
    };

    println!("owt: fanning out across {} job(s)", jobs.len());

    // `repo_lock` serializes git mutations; `print_lock` keeps each job's output
    // block contiguous (no interleaving) when printed on completion.
    let repo_lock = Arc::new(Mutex::new(()));
    let print_lock = Arc::new(Mutex::new(()));
    let mut handles = Vec::new();
    for (label, from, env) in jobs {
        let repo_lock = repo_lock.clone();
        let print_lock = print_lock.clone();
        let command = args.command.clone();
        let include = args.include.clone();
        let setup = args.setup.clone();
        handles.push(std::thread::spawn(move || {
            run_job(
                &repo_lock,
                &print_lock,
                &label,
                &from,
                &include,
                setup.as_deref(),
                on_exit,
                &command,
                &env,
            )
        }));
    }

    let mut results: Vec<(String, i32)> = Vec::new();
    for h in handles {
        match h.join() {
            Ok(r) => results.push(r),
            Err(_) => results.push(("<panicked>".to_string(), 1)),
        }
    }

    results.sort_by(|a, b| a.0.cmp(&b.0));
    println!("\nowt: fan-out results:");
    for (label, code) in &results {
        println!("  {label:<16} exit {code}");
    }

    let failures = results.iter().filter(|(_, c)| *c != 0).count();
    Ok(if failures == 0 { 0 } else { 1 })
}

/// Create a worktree, run the command in it, and clean up. Creation and cleanup
/// hold `repo_lock` (git mutates the repo); the command runs without the lock.
#[allow(clippy::too_many_arguments)]
fn run_job(
    repo_lock: &Mutex<()>,
    print_lock: &Mutex<()>,
    label: &str,
    from: &str,
    include: &[String],
    setup: Option<&str>,
    on_exit: OnExit,
    command: &[String],
    env: &[(String, String)],
) -> (String, i32) {
    let created = {
        let _guard = repo_lock.lock().unwrap();
        Session::create(CreateOpts {
            from,
            name: None,
            dir: None,
            include,
            setup,
            on_exit,
            interactive: false,
            command: command.to_vec(),
            progress: false,
        })
    };

    let mut session = match created {
        Ok(s) => s,
        Err(e) => {
            eprintln!("owt: [{label}] failed to create worktree: {e:#}");
            return (label.to_string(), 1);
        }
    };

    let mut full_env = env.to_vec();
    full_env.push(("OWT_LABEL".to_string(), label.to_string()));

    // Capture this job's output so it can be printed as one contiguous block.
    let (code, out, err) = match exec_capture(&session.worktree_path, command, &full_env) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("owt: [{label}] failed to run: {e:#}");
            (1, Vec::new(), Vec::new())
        }
    };

    {
        let _p = print_lock.lock().unwrap();
        let mut so = std::io::stdout().lock();
        let _ = writeln!(so, "\n=== [{label}] exit {code} ===");
        let _ = so.write_all(&out);
        let _ = so.flush();
        drop(so);
        if !err.is_empty() {
            let mut se = std::io::stderr().lock();
            let _ = se.write_all(&err);
            let _ = se.flush();
        }
    }

    {
        let _guard = repo_lock.lock().unwrap();
        if let Err(e) = session.finish() {
            eprintln!("owt: [{label}] cleanup failed: {e:#}");
        }
    }
    (label.to_string(), code)
}

/// Spawn the command in the worktree, passing through the exit code. On Ctrl+C /
/// termination (shared signal flag) we kill the child and let cleanup proceed.
fn exec(worktree: &Path, command: &[String], env: &[(String, String)]) -> Result<i32> {
    signal::install();

    let mut cmd = Command::new(&command[0]);
    cmd.args(&command[1..]).current_dir(worktree);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawning '{}'", command[0]))?;

    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if signal::requested() {
            let _ = child.kill();
            break child.wait()?;
        }
        std::thread::sleep(Duration::from_millis(80));
    };

    // 130 is the conventional "terminated by SIGINT" code.
    Ok(status
        .code()
        .unwrap_or(if signal::requested() { 130 } else { 1 }))
}

/// Like [`exec`], but captures stdout/stderr (for fan-out, so each job's output
/// can be printed as one contiguous block). Drain threads prevent pipe-buffer
/// deadlock while we poll for completion / shutdown.
fn exec_capture(
    worktree: &Path,
    command: &[String],
    env: &[(String, String)],
) -> Result<(i32, Vec<u8>, Vec<u8>)> {
    signal::install();

    let mut cmd = Command::new(&command[0]);
    cmd.args(&command[1..])
        .current_dir(worktree)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawning '{}'", command[0]))?;

    let mut out_pipe = child.stdout.take().expect("piped stdout");
    let mut err_pipe = child.stderr.take().expect("piped stderr");
    let out_h = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = out_pipe.read_to_end(&mut buf);
        buf
    });
    let err_h = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = err_pipe.read_to_end(&mut buf);
        buf
    });

    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if signal::requested() {
            let _ = child.kill();
            break child.wait()?;
        }
        std::thread::sleep(Duration::from_millis(80));
    };

    let out = out_h.join().unwrap_or_default();
    let err = err_h.join().unwrap_or_default();
    let code = status
        .code()
        .unwrap_or(if signal::requested() { 130 } else { 1 });
    Ok((code, out, err))
}

fn cmd_list(all: bool, json: bool) -> Result<i32> {
    let mut views = state::collect()?;
    // The main worktree is the repo itself; never listed as a manageable entry.
    views.retain(|v| v.source != state::Source::Main);
    if !all {
        views.retain(|v| v.source == state::Source::Owt);
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&views)?);
        return Ok(0);
    }

    if views.is_empty() {
        println!("no worktrees");
        return Ok(0);
    }

    println!(
        "{:<20} {:<22} {:<9} {:<8} {}",
        "NAME", "BRANCH", "SOURCE", "STATUS", "PATH"
    );
    for v in &views {
        println!(
            "{:<20} {:<22} {:<9} {:<8} {}",
            v.name.as_deref().unwrap_or("-"),
            v.branch.as_deref().unwrap_or("-"),
            format!("{:?}", v.source).to_lowercase(),
            format!("{:?}", v.status).to_lowercase(),
            v.path
        );
    }
    Ok(0)
}

fn cmd_clean(
    name: Option<&str>,
    running: bool,
    all: bool,
    force: bool,
    yes: bool,
    dry_run: bool,
) -> Result<i32> {
    let plans = state::plan_clean(name, running, all, force)?;

    if let Some(n) = name {
        if plans.is_empty() {
            bail!("no owt worktree named '{}'", n);
        }
    }

    let removable: Vec<&state::Plan> = plans.iter().filter(|p| p.skip.is_none()).collect();
    let skipped: Vec<&state::Plan> = plans.iter().filter(|p| p.skip.is_some()).collect();

    if removable.is_empty() && skipped.is_empty() {
        println!("nothing to clean");
        return Ok(0);
    }

    println!("The following worktrees will be removed (main worktree is never touched):");
    for p in &removable {
        let mut tags = Vec::new();
        if p.view.status == state::Liveness::Running {
            tags.push("running");
        }
        if p.dirty {
            tags.push("dirty");
        }
        let tag = if tags.is_empty() {
            String::new()
        } else {
            format!("  [{}]", tags.join(", "))
        };
        println!(
            "  {:<18} {:<9} {}{}",
            p.view.name.as_deref().unwrap_or("-"),
            format!("{:?}", p.view.source).to_lowercase(),
            p.view.path,
            tag
        );
    }
    for p in &skipped {
        println!(
            "  SKIP {:<13} {} ({})",
            p.view.name.as_deref().unwrap_or("-"),
            p.view.path,
            p.skip.as_deref().unwrap_or("")
        );
    }

    if dry_run {
        println!("(dry run; nothing removed)");
        return Ok(0);
    }
    if removable.is_empty() {
        println!("nothing removable (all skipped for safety; use --force to override)");
        return Ok(0);
    }
    if !yes && !confirm("Proceed?")? {
        println!("aborted");
        return Ok(0);
    }

    let mut removed = 0;
    for p in &removable {
        match state::remove(p, force) {
            Ok(()) => removed += 1,
            Err(e) => eprintln!("owt: warning: failed to remove {}: {e:#}", p.view.path),
        }
    }
    println!("removed {removed} worktree(s)");
    Ok(0)
}

/// Prompt on stderr and read a yes/no answer from stdin (default: no).
fn confirm(prompt: &str) -> Result<bool> {
    use std::io::Write;
    eprint!("{prompt} [y/N] ");
    std::io::stderr().flush().ok();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let a = line.trim().to_ascii_lowercase();
    Ok(a == "y" || a == "yes")
}

