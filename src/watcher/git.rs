//! Git repository awareness for the file watcher.
//!
//! Detects when a watched directory's git HEAD moves (commit, branch switch,
//! pull, merge, rebase) so the watcher can trigger a single reconcile instead
//! of reacting to hundreds of individual file events.
//!
//! Deliberately dependency-light: reads `.git/HEAD`, loose refs, and
//! `packed-refs` directly from the filesystem — no git library, no shelling
//! out to `git`.

use std::path::{Path, PathBuf};

/// A git repository discovered under (or containing) a watched directory.
#[derive(Debug, Clone)]
pub struct GitRepo {
    /// The working-tree root that is being watched.
    pub work_root: PathBuf,
    /// The resolved git directory (handles `.git` file indirection for
    /// worktrees and submodules).
    pub git_dir: PathBuf,
}

impl GitRepo {
    /// Discover the git repository for a watched directory.
    ///
    /// Checks `<root>/.git`. If it is a directory, that is the git dir.
    /// If it is a file (linked worktree / submodule), follows the
    /// `gitdir: <path>` pointer. Returns `None` if the root is not inside
    /// a git work tree.
    #[must_use]
    pub fn discover(root: &Path) -> Option<Self> {
        let dot_git = root.join(".git");

        if dot_git.is_dir() {
            return Some(Self {
                work_root: root.to_path_buf(),
                git_dir: dot_git,
            });
        }

        if dot_git.is_file() {
            // Worktree/submodule: ".git" is a file containing "gitdir: <path>"
            let contents = std::fs::read_to_string(&dot_git).ok()?;
            let pointer = contents.strip_prefix("gitdir:")?.trim();
            let git_dir = if Path::new(pointer).is_absolute() {
                PathBuf::from(pointer)
            } else {
                root.join(pointer)
            };
            if git_dir.is_dir() {
                return Some(Self {
                    work_root: root.to_path_buf(),
                    git_dir,
                });
            }
        }

        None
    }

    /// Check whether a filesystem event path is git metadata that signals a
    /// HEAD/ref move for this repository (HEAD, packed-refs, or anything
    /// under `refs/`).
    #[must_use]
    pub fn is_meta_path(&self, path: &Path) -> bool {
        let Ok(rel) = path.strip_prefix(&self.git_dir) else {
            return false;
        };
        let rel_str = rel.to_string_lossy();
        rel_str == "HEAD" || rel_str == "packed-refs" || rel.starts_with("refs")
    }

    /// Resolve the commit hash HEAD currently points to.
    ///
    /// Reads `HEAD` directly; follows a symbolic ref through loose refs and
    /// `packed-refs`. For linked worktrees, refs are looked up in the common
    /// git dir (via the `commondir` file). Returns `None` if HEAD cannot be
    /// resolved (e.g. unborn branch) — callers treat a `None -> Some`
    /// transition as a change.
    #[must_use]
    pub fn head_commit(&self) -> Option<String> {
        let head = std::fs::read_to_string(self.git_dir.join("HEAD")).ok()?;
        let head = head.trim();

        let Some(ref_name) = head.strip_prefix("ref:").map(str::trim) else {
            // Detached HEAD: the file contains the commit hash itself.
            return Some(head.to_string());
        };

        // Ref lookup dirs: the git dir itself, plus the common dir for
        // linked worktrees (refs live in the main repository's git dir).
        let mut ref_dirs = vec![self.git_dir.clone()];
        if let Ok(common) = std::fs::read_to_string(self.git_dir.join("commondir")) {
            let common = common.trim();
            let common_dir = if Path::new(common).is_absolute() {
                PathBuf::from(common)
            } else {
                self.git_dir.join(common)
            };
            ref_dirs.push(common_dir);
        }

        for dir in &ref_dirs {
            // Loose ref file
            if let Ok(hash) = std::fs::read_to_string(dir.join(ref_name)) {
                let hash = hash.trim();
                if !hash.is_empty() {
                    return Some(hash.to_string());
                }
            }
            // packed-refs fallback
            if let Some(hash) = lookup_packed_ref(&dir.join("packed-refs"), ref_name) {
                return Some(hash);
            }
        }

        None
    }
}

/// Look up a ref in a `packed-refs` file.
///
/// Lines have the form `<hash> <refname>`; `#` lines are comments and
/// `^` lines are peeled tags.
fn lookup_packed_ref(packed_refs: &Path, ref_name: &str) -> Option<String> {
    let contents = std::fs::read_to_string(packed_refs).ok()?;
    for line in contents.lines() {
        if line.starts_with('#') || line.starts_with('^') {
            continue;
        }
        if let Some((hash, name)) = line.split_once(' ') {
            if name.trim() == ref_name {
                return Some(hash.trim().to_string());
            }
        }
    }
    None
}

/// Discover git repositories for a set of watch directories.
///
/// Directories that are not git work trees are silently skipped (the plain
/// file watcher still covers them).
#[must_use]
pub fn discover_repos(watch_dirs: &[PathBuf]) -> Vec<GitRepo> {
    let mut repos = Vec::new();
    for dir in watch_dirs {
        if let Some(repo) = GitRepo::discover(dir) {
            tracing::info!(
                root = %repo.work_root.display(),
                git_dir = %repo.git_dir.display(),
                "Git repository detected — HEAD changes will trigger reconcile"
            );
            repos.push(repo);
        }
    }
    repos
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Build a minimal fake git dir with HEAD on a branch.
    fn fake_repo(tmp: &TempDir, branch: &str, hash: &str) -> PathBuf {
        let root = tmp.path().to_path_buf();
        let git_dir = root.join(".git");
        fs::create_dir_all(git_dir.join("refs/heads")).unwrap();
        fs::write(git_dir.join("HEAD"), format!("ref: refs/heads/{branch}\n")).unwrap();
        fs::write(git_dir.join("refs/heads").join(branch), format!("{hash}\n")).unwrap();
        root
    }

    #[test]
    fn test_discover_git_dir() {
        let tmp = TempDir::new().unwrap();
        let root = fake_repo(&tmp, "main", "abc123");
        let repo = GitRepo::discover(&root).unwrap();
        assert_eq!(repo.git_dir, root.join(".git"));
        assert_eq!(repo.work_root, root);
    }

    #[test]
    fn test_discover_non_repo() {
        let tmp = TempDir::new().unwrap();
        assert!(GitRepo::discover(tmp.path()).is_none());
    }

    #[test]
    fn test_discover_gitfile_worktree() {
        let tmp = TempDir::new().unwrap();
        let real_git = tmp.path().join("real-git");
        fs::create_dir_all(&real_git).unwrap();
        let work = tmp.path().join("worktree");
        fs::create_dir_all(&work).unwrap();
        fs::write(
            work.join(".git"),
            format!("gitdir: {}\n", real_git.display()),
        )
        .unwrap();

        let repo = GitRepo::discover(&work).unwrap();
        assert_eq!(repo.git_dir, real_git);
    }

    #[test]
    fn test_head_commit_loose_ref() {
        let tmp = TempDir::new().unwrap();
        let root = fake_repo(&tmp, "main", "abc123");
        let repo = GitRepo::discover(&root).unwrap();
        assert_eq!(repo.head_commit().as_deref(), Some("abc123"));
    }

    #[test]
    fn test_head_commit_changes_on_branch_switch() {
        let tmp = TempDir::new().unwrap();
        let root = fake_repo(&tmp, "main", "abc123");
        let repo = GitRepo::discover(&root).unwrap();

        // Create a second branch and point HEAD at it (what `git checkout` does)
        fs::write(root.join(".git/refs/heads/feature"), "def456\n").unwrap();
        fs::write(root.join(".git/HEAD"), "ref: refs/heads/feature\n").unwrap();

        assert_eq!(repo.head_commit().as_deref(), Some("def456"));
    }

    #[test]
    fn test_head_commit_changes_on_commit() {
        let tmp = TempDir::new().unwrap();
        let root = fake_repo(&tmp, "main", "abc123");
        let repo = GitRepo::discover(&root).unwrap();

        // Advancing the branch ref (what `git commit` / `git pull` do)
        fs::write(root.join(".git/refs/heads/main"), "fresh789\n").unwrap();
        assert_eq!(repo.head_commit().as_deref(), Some("fresh789"));
    }

    #[test]
    fn test_head_commit_detached() {
        let tmp = TempDir::new().unwrap();
        let root = fake_repo(&tmp, "main", "abc123");
        fs::write(root.join(".git/HEAD"), "deadbeef\n").unwrap();
        let repo = GitRepo::discover(&root).unwrap();
        assert_eq!(repo.head_commit().as_deref(), Some("deadbeef"));
    }

    #[test]
    fn test_head_commit_packed_refs() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        let git_dir = root.join(".git");
        fs::create_dir_all(&git_dir).unwrap();
        fs::write(git_dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        fs::write(
            git_dir.join("packed-refs"),
            "# pack-refs with: peeled fully-peeled sorted\n\
             cafebabe refs/heads/main\n\
             ^ignored-peel-line\n",
        )
        .unwrap();

        let repo = GitRepo::discover(&root).unwrap();
        assert_eq!(repo.head_commit().as_deref(), Some("cafebabe"));
    }

    #[test]
    fn test_is_meta_path() {
        let tmp = TempDir::new().unwrap();
        let root = fake_repo(&tmp, "main", "abc123");
        let repo = GitRepo::discover(&root).unwrap();

        assert!(repo.is_meta_path(&root.join(".git/HEAD")));
        assert!(repo.is_meta_path(&root.join(".git/packed-refs")));
        assert!(repo.is_meta_path(&root.join(".git/refs/heads/main")));
        assert!(!repo.is_meta_path(&root.join(".git/objects/ab/cdef")));
        assert!(!repo.is_meta_path(&root.join("src/main.rs")));
        assert!(!repo.is_meta_path(Path::new("/elsewhere/.git/HEAD")));
    }

    #[test]
    fn test_discover_repos_mixed() {
        let repo_tmp = TempDir::new().unwrap();
        let plain_tmp = TempDir::new().unwrap();
        let root = fake_repo(&repo_tmp, "main", "abc123");

        let repos = discover_repos(&[root.clone(), plain_tmp.path().to_path_buf()]);
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0].work_root, root);
    }
}
