//! The finder's candidate source: a bounded, gitignore-aware filesystem walk.
//!
//! Adapted from `~/src/levelup/sleipnir/src/walk.rs` (minus its frecency/DB
//! ranking). The walk roots at the enclosing git repo (or the cwd if not in a
//! repo) and scores each file by **mtime recency** — a cheap "I just touched
//! this" signal — so the finder's empty-query order is most-recently-edited
//! first. `.gitignore` and hidden files are respected, and a few heavy build
//! directories are pruned even outside a repo.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use ignore::WalkBuilder;

/// Stop the walk after this many entries (a capped walk that looks complete is
/// a lie — but a backstop against pathological trees is still worth having).
const WALK_CAP: usize = 20_000;
/// Don't descend deeper than this.
const WALK_MAX_DEPTH: usize = 24;
/// Pruned even when no `.gitignore` covers them (e.g. outside a repo).
const PRUNE_DIRS: &[&str] = &["target", "node_modules", ".git"];

/// One file the finder can open. `display` is relative to the walk root.
#[derive(Clone, Debug)]
pub struct Candidate {
    pub path: PathBuf,
    pub display: String,
    /// Seconds since the epoch; higher is more recently modified.
    pub mtime: i64,
}

/// The nearest ancestor of `start` containing `.git` (the repo root), else `None`.
pub fn repo_root(start: &Path) -> Option<PathBuf> {
    let mut cur = Some(start);
    while let Some(dir) = cur {
        if dir.join(".git").exists() {
            return Some(dir.to_path_buf());
        }
        cur = dir.parent();
    }
    None
}

/// The directory the finder should walk: the repo root enclosing `cwd`, or `cwd`
/// itself when not in a repo.
pub fn finder_root(cwd: &Path) -> PathBuf {
    repo_root(cwd).unwrap_or_else(|| cwd.to_path_buf())
}

/// Walk `root` and return file candidates, gitignore-aware, sorted most-recently
/// modified first.
pub fn walk(root: &Path) -> Vec<Candidate> {
    let walker = WalkBuilder::new(root)
        .max_depth(Some(WALK_MAX_DEPTH))
        .filter_entry(|e| {
            // Prune heavy dirs by name (don't even descend); files always pass.
            !e.file_type().is_some_and(|ft| ft.is_dir())
                || !PRUNE_DIRS.contains(&e.file_name().to_string_lossy().as_ref())
        })
        .build();

    let mut candidates = Vec::new();
    for entry in walker.flatten() {
        if candidates.len() >= WALK_CAP {
            break;
        }
        // Files only — directories are structure, not open targets.
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let path = entry.path();
        let display = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned();
        let mtime = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        candidates.push(Candidate {
            path: path.to_path_buf(),
            display,
            mtime,
        });
    }

    candidates.sort_by(|a, b| b.mtime.cmp(&a.mtime).then_with(|| a.display.cmp(&b.display)));
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn temp_dir(tag: &str) -> PathBuf {
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("leap_walk_{}_{tag}_{n}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn walk_lists_files_relative_and_skips_pruned_dirs() {
        let dir = temp_dir("basic");
        fs::write(dir.join("a.txt"), "a").unwrap();
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join("sub/b.txt"), "b").unwrap();
        fs::create_dir_all(dir.join("target")).unwrap();
        fs::write(dir.join("target/ignored.txt"), "x").unwrap();

        let found = walk(&dir);
        let names: Vec<&str> = found.iter().map(|c| c.display.as_str()).collect();
        assert!(names.contains(&"a.txt"));
        assert!(names.contains(&"sub/b.txt"));
        assert!(!names.iter().any(|n| n.contains("target")), "target/ pruned");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn walk_orders_by_mtime_desc() {
        let dir = temp_dir("mtime");
        // Two files; bump the second's mtime so it sorts first.
        fs::write(dir.join("old.txt"), "o").unwrap();
        fs::write(dir.join("new.txt"), "n").unwrap();
        let future = std::time::SystemTime::now() + std::time::Duration::from_secs(120);
        fs::File::open(dir.join("new.txt"))
            .unwrap()
            .set_modified(future)
            .unwrap();

        let found = walk(&dir);
        assert_eq!(found.first().map(|c| c.display.as_str()), Some("new.txt"));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn repo_root_finds_dotgit_ancestor() {
        let dir = temp_dir("repo");
        fs::create_dir_all(dir.join(".git")).unwrap();
        let nested = dir.join("a/b");
        fs::create_dir_all(&nested).unwrap();
        assert_eq!(
            repo_root(&nested).and_then(|r| fs::canonicalize(r).ok()),
            fs::canonicalize(&dir).ok()
        );
        fs::remove_dir_all(&dir).ok();
    }
}
