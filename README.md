# openworktree (`owt`)

An **ephemeral git worktree sandbox**. Create a throwaway worktree, run a command
inside it, then clean up — a transparent prefix you put in front of any command:

```sh
owt -- npm test
```

`owt` creates a fresh worktree from your current `HEAD`, `cd`s into it, runs
`npm test` there, passes the exit code straight back, and removes the worktree
when the command finishes (even on Ctrl+C).

## Why not just a worktree manager?

Most worktree tools are *persistent managers* (create / list / rename / remove
worktrees as long-lived assets). `owt` is the opposite: a **run-and-discard
sandbox** meant to be a primitive for scripts and agents. It deliberately is
**not** a manager — no `mv`/`rename`/editing, only GC-level `list` / `clean`.

## Compared to plain `git worktree`

Running a command in a throwaway worktree by hand is several steps you must not
forget to undo:

```sh
git worktree add -b tmp ../tmp HEAD   # create
cd ../tmp                             # enter
npm test                              # run
cd -                                  # leave
git worktree remove --force ../tmp    # clean up the dir
git branch -D tmp                     # clean up the branch
```

With `owt` it is one line, and cleanup (including on Ctrl+C or failure) is
automatic:

```sh
owt -- npm test
```

On top of that one-liner, `owt` adds things raw `git worktree` has no notion of:

| | plain `git worktree` | `owt` |
|---|---|---|
| Run a command in the worktree | manual `cd` + run | `owt -- <cmd>` (cwd set for you) |
| Cleanup after | manual, easy to forget | automatic (`discard`) or `--keep` |
| Cleanup on Ctrl+C / crash | left behind | removed (signal-safe) + `owt clean` GC |
| Exit code of your command | lost in the steps | passed straight through |
| Unique naming / location | you pick every time | auto (random name, cache dir) |
| Copy `.env` / `node_modules` in | manual | `.worktreeinclude` / `--include` |
| Run across many branches | scripting | `owt --each a,b,c -- <cmd>` |
| Isolated parallel shards | scripting | `owt --shard N -- <cmd>` |

`owt` is a thin convenience layer over `git worktree` (it shells out to it), so
your git config, hooks, and credentials all still apply.

## Install

Prebuilt binaries for Linux, macOS, and Windows are attached to each
[GitHub Release](https://github.com/BingHanLin/openworktree/releases) — download,
extract, and put `owt` on your `PATH`. Or build from source:

```sh
cargo install --path .
# or, for a dev build:
cargo build --release   # binary at target/release/owt
```

Requires `git` on `PATH`.

## Usage

### One-shot (default)

```sh
owt -- <command> [args...]
```

The command runs with its working directory set to a fresh worktree. `owt` exits
with the command's exit code. Preparation progress (creating the worktree,
copying includes, running `--setup`, ready) is printed to **stderr**, so it never
mixes into the command's stdout. (Fan-out runs stay quiet to avoid interleaving.)

### Interactive

```sh
owt -i
```

Drops you into a shell inside a new worktree. The worktree is **not** auto-cleaned
when you exit — use `owt clean` later. The shell is chosen by `--shell`, then the
`shell` config key, then `$SHELL` / `%ComSpec%`.

### Fan-out (parallel)

Run the same command across several refs, each in its own worktree, in parallel:

```sh
owt --each main,feat-a,feat-b -- npm test
```

Each job sets `OWT_REF` (and `OWT_LABEL`). Useful for cross-branch comparison and
regression checks.

Or split work into N isolated parallel shards from one ref:

```sh
owt --shard 4 -- pytest --shard-id $OWT_INDEX
```

Each shard sets `OWT_INDEX` (`0..N`) and `OWT_TOTAL`. Worktree isolation lets the
shards run side by side without colliding on files or ports. Worktree
creation/cleanup is serialized (git mutates the repo); the commands run
concurrently. The run exits non-zero if any job fails.

Each job's output is captured and printed as one contiguous block under a
`=== [label] exit N ===` header (no interleaving), followed by a per-job
exit-code summary.

### Options (creation)

| Flag | Default | Description |
|------|---------|-------------|
| `--from <ref>` | config `from`, else `HEAD` | Source ref the worktree is based on |
| `--name <name>` | random `adjective-noun` | Worktree / branch name (errors if taken) |
| `--dir <path>` | `<cache>/worktrees/<repo>__<name>` | Exact worktree path (used verbatim) |
| `--parent-dir <path>` | — | Parent dir to create the auto `<repo>__<name>` subdir under |
| `--include <glob>` | — | Extra path/glob to copy in (repeatable) |
| `--setup <cmd>` | — | Command to run before the main command (e.g. `npm ci`) |
| `--on-exit <discard\|keep>` | `discard` | What to do with the worktree at the end (one-shot) |
| `--keep` | — | Shorthand for `--on-exit keep` |
| `--detach` | — | Build the worktree in detached HEAD, with no `owt/<name>` branch (conflicts with `--keep`) |
| `--shell <shell>` | config / `$SHELL` | Shell for interactive mode (`-i`) |
| `--each <refs>` | — | Run once per comma-separated ref, in parallel |
| `--shard <N>` | — | Run in N parallel worktrees from one ref |

### On-exit policies (one-shot)

| Policy | Worktree dir | Uncommitted changes | Mid-run commits | Branch |
|--------|--------------|---------------------|-----------------|--------|
| `discard` (default) | removed | discarded | removed | deleted |
| `keep` | removed | **auto-committed** | kept | **kept** (empty branch kept too) |

The `keep` auto-commit message is `owt: <command> @ <timestamp>`.

### Detached worktrees (`--detach`)

By default `owt` creates an `owt/<name>` branch for each worktree. Pass
`--detach` to skip that and build the worktree in **detached HEAD** instead:

```sh
owt --detach -- npm test
```

No branch is created or occupied, so the branch namespace stays clean — handy
for throwaway runs and for agents spinning up many sandboxes. `owt` still tracks
the worktree (via its central index, matched by path rather than branch), so
`list` and `clean` manage detached worktrees exactly like branch-backed ones.

`--detach` is incompatible with `--keep` (and `--on-exit keep`): `keep` retains
its auto-commit on the branch, which a detached worktree has none of — the commit
would become unreachable and be garbage-collected, so the combination is rejected
up front.

### Where the worktree goes (`--dir` / `--parent-dir`)

By default, worktrees land under the cache dir at
`<cache>/worktrees/<repo>__<name>` (the `<repo>__<name>` subdir is added for you;
`<cache>` is the platform cache dir, overridable with `OWT_CACHE_DIR`). There are
two ways to override the location:

**`--parent-dir <path>`** — relocate just the parent. The automatic
`<repo>__<name>` subdir is still appended, so each run gets its own uniquely
named directory:

```sh
owt --parent-dir D:/tmp -- npm test
# worktree at D:/tmp/<repo>__<name>, e.g. D:/tmp/myrepo__brave-otter
```

Because every run gets a distinct subdir, `--parent-dir` **works with
`--each` / `--shard`** — all jobs share the parent while each lands in its own
auto-named directory.

**`--dir <path>`** — set the exact worktree path, used **verbatim** (no
`<repo>__<name>` suffix appended):

```sh
owt --dir D:/tmp/t1   -- npm test   # worktree at D:/tmp/t1 (absolute, as-is)
owt --dir ../mytree   -- npm test   # relative to where you run owt
```

Notes for both:

- Relative paths resolve against the directory you invoke `owt` from (it does not
  `cd` first).
- The target worktree directory **must not already exist** (a `git worktree add`
  requirement); missing parent directories are created for you.
- `--dir` and `--parent-dir` are mutually exclusive. `--dir` cannot be combined
  with `--each` / `--shard` (each fan-out job needs its own distinct worktree —
  use `--parent-dir` for that).

## `.worktreeinclude`

A fresh worktree is a clean checkout — it has no `node_modules`, no `.env`, etc.
Put a `.worktreeinclude` file at your repo root to copy (or symlink) the
gitignored-but-needed files into every new worktree. Syntax mirrors `.gitignore`:

```gitignore
# copy a file
.env

# symlink a directory (cheap; '@' prefix)
@node_modules

# globs are supported
config/*.local

# exclude a previously matched path ('!' prefix)
!config/secret.local
```

- `@` → symlink instead of copy.
- `!` → exclude.
- On Windows, symlinks require Developer Mode or admin; `owt` falls back to a copy
  (with a warning) when the privilege is missing.

## Inspecting & cleaning up

`owt` records each worktree it creates and can garbage-collect leftovers (e.g.
from a crash or `kill -9`).

```sh
owt list            # worktrees owt created (running / orphan)
owt list --all      # every non-main worktree, incl. external ones (read-only)
owt list --json     # machine-readable
```

```sh
owt clean                 # remove owt orphans (dead sessions) — safe default
owt clean <name>          # remove one specific owt worktree
owt clean --running       # also remove still-running owt worktrees
owt clean --all           # remove ALL non-main worktrees, incl. external ones
```

`clean` flags: `--force` (override uncommitted changes / locks), `--yes` (skip the
confirmation prompt), `--dry-run` (show what would happen).

**Safety guardrails** (especially for `--all`):

1. The **main worktree is never removed**.
2. You get a preview + confirmation prompt (skip with `--yes`).
3. Worktrees with **uncommitted changes are skipped** unless `--force`.
4. **Locked** worktrees are skipped unless `--force`.
5. `--dry-run` removes nothing.

External worktrees keep their branches; only owt-owned worktrees have their
`owt/<name>` branch deleted on removal.

## Configuration

| Env var | Effect |
|---------|--------|
| `OWT_CACHE_DIR` | Override where owt stores worktrees and its index (default: the platform cache dir) |
| `OWT_CONFIG` | Path to the config file (default: `<config>/openworktree/config.toml`) |

Config file (`config.toml`), all keys optional:

```toml
# Shell used by interactive mode (owt -i)
shell = "/bin/zsh"

# Default source ref when --from is not given (otherwise HEAD)
from = "origin/main"

# Aliases: saved argument presets, invoked as `owt @<name>`
[alias.oc]
args = ["--from", "origin/main", "-i", "--shell", "opencode"]

[alias.t]
args = ["--keep", "--setup", "npm ci", "--", "npm", "test"]
```

## Aliases

Save a frequently-used invocation under `[alias.<name>]` and run it with the `@`
prefix:

```sh
owt @oc                 # expands to: owt --from origin/main -i --shell opencode
owt @t                  # expands to: owt --keep --setup "npm ci" -- npm test
```

Extra arguments after `@name` are appended to the alias. A later flag overrides
an earlier one, so `owt @oc --from HEAD` overrides the alias's `--from`. Note that
if the alias ends with `-- <command>`, anything you append goes to that command,
not to owt.

Per-worktree metadata is stored inside git's private admin dir
(`.git/worktrees/<id>/owt-meta.json`), never in the working tree — so it can never
be accidentally committed.

## Status

Implemented: one-shot & interactive runs (with shell selection), exit-code
passthrough, Ctrl+C-safe cleanup, `.worktreeinclude`, `list`, `clean`, and
fan-out (`--each` / `--shard`).
