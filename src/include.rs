//! `.worktreeinclude`: copy (or symlink) gitignored-but-needed files into a new
//! worktree.
//!
//! Syntax mirrors `.gitignore`, one entry per line:
//!   - `# ...`        comment
//!   - `path/glob`    copy this file/dir/glob into the worktree
//!   - `@path`        symlink instead of copy (cheap for e.g. `@node_modules`)
//!   - `!path`        exclude a previously matched path
//!
//! Patterns are resolved relative to the source working tree's top level.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

const INCLUDE_FILE: &str = ".worktreeinclude";

#[derive(Debug)]
struct Rule {
    pattern: String,
    symlink: bool,
    negate: bool,
}

fn parse(content: &str) -> Vec<Rule> {
    let mut rules = Vec::new();
    for raw in content.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut negate = false;
        let mut symlink = false;
        let mut rest = line;
        // Order-independent leading markers: `!` (exclude) and `@` (symlink).
        loop {
            if let Some(s) = rest.strip_prefix('!') {
                negate = true;
                rest = s.trim_start();
            } else if let Some(s) = rest.strip_prefix('@') {
                symlink = true;
                rest = s.trim_start();
            } else {
                break;
            }
        }
        if rest.is_empty() {
            continue;
        }
        rules.push(Rule {
            pattern: rest.to_string(),
            symlink,
            negate,
        });
    }
    rules
}

/// Apply `.worktreeinclude` (plus any extra `--include` patterns) from `src`
/// into `dest`. Returns the number of top-level entries copied/linked.
pub fn apply(src: &Path, dest: &Path, extra: &[String]) -> Result<usize> {
    let mut rules = Vec::new();

    let include_file = src.join(INCLUDE_FILE);
    if include_file.exists() {
        let content = std::fs::read_to_string(&include_file)
            .with_context(|| format!("reading {}", include_file.display()))?;
        rules.extend(parse(&content));
    }
    // Ad-hoc `--include` patterns are plain copy rules.
    for p in extra {
        rules.push(Rule {
            pattern: p.clone(),
            symlink: false,
            negate: false,
        });
    }

    if rules.is_empty() {
        return Ok(0);
    }

    let excludes: Vec<glob::Pattern> = rules
        .iter()
        .filter(|r| r.negate)
        .filter_map(|r| glob::Pattern::new(&r.pattern).ok())
        .collect();

    let mut count = 0;
    for rule in rules.iter().filter(|r| !r.negate) {
        for source_path in resolve(src, &rule.pattern)? {
            let rel = match source_path.strip_prefix(src) {
                Ok(r) => r,
                Err(_) => continue,
            };
            if excludes.iter().any(|p| p.matches_path(rel)) {
                continue;
            }
            let target = dest.join(rel);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            if rule.symlink {
                place_symlink(&source_path, &target)?;
            } else {
                copy_any(&source_path, &target)?;
            }
            count += 1;
        }
    }
    Ok(count)
}

/// Resolve a single pattern (literal path or glob) to existing source paths.
fn resolve(src: &Path, pattern: &str) -> Result<Vec<PathBuf>> {
    let has_glob = pattern.contains(['*', '?', '[']);
    if has_glob {
        let joined = src.join(pattern);
        let pat = joined.to_string_lossy();
        let mut out = Vec::new();
        for p in glob::glob(&pat)
            .with_context(|| format!("bad glob '{pattern}'"))?
            .flatten()
        {
            out.push(p);
        }
        Ok(out)
    } else {
        let p = src.join(pattern);
        Ok(if p.exists() { vec![p] } else { Vec::new() })
    }
}

/// Copy a file or directory (recursively).
fn copy_any(src: &Path, dest: &Path) -> Result<()> {
    if src.is_dir() {
        copy_dir(src, dest)
    } else {
        std::fs::copy(src, dest)
            .map(|_| ())
            .with_context(|| format!("copying {} -> {}", src.display(), dest.display()))
    }
}

fn copy_dir(src: &Path, dest: &Path) -> Result<()> {
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        if from.is_dir() {
            copy_dir(&from, &to)?;
        } else {
            std::fs::copy(&from, &to).with_context(|| format!("copying {}", from.display()))?;
        }
    }
    Ok(())
}

/// Create a symlink, falling back to a copy if the OS refuses (e.g. Windows
/// without the privilege to create symlinks).
fn place_symlink(src: &Path, dest: &Path) -> Result<()> {
    match symlink_impl(src, dest) {
        Ok(()) => Ok(()),
        Err(e) => {
            eprintln!(
                "owt: warning: could not symlink {} ({e}); copying instead",
                dest.display()
            );
            copy_any(src, dest)
        }
    }
}

#[cfg(unix)]
fn symlink_impl(src: &Path, dest: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(src, dest)
}

#[cfg(windows)]
fn symlink_impl(src: &Path, dest: &Path) -> std::io::Result<()> {
    if src.is_dir() {
        std::os::windows::fs::symlink_dir(src, dest)
    } else {
        std::os::windows::fs::symlink_file(src, dest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_markers_and_skips_noise() {
        let rules = parse("# comment\n\n.env\n@node_modules\n!drop.local\n@ ! both\n");
        assert_eq!(rules.len(), 4);

        assert_eq!(rules[0].pattern, ".env");
        assert!(!rules[0].symlink && !rules[0].negate);

        assert_eq!(rules[1].pattern, "node_modules");
        assert!(rules[1].symlink && !rules[1].negate);

        assert_eq!(rules[2].pattern, "drop.local");
        assert!(!rules[2].symlink && rules[2].negate);

        // Markers may appear in any order, with whitespace.
        assert_eq!(rules[3].pattern, "both");
        assert!(rules[3].symlink && rules[3].negate);
    }
}
