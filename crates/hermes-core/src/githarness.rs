//! Transactional git harness.
//!
//! Snapshot/guard/undo primitives so agent edit runs are reversible:
//!
//! 1. [`GitHarness::snapshot`] records HEAD plus any dirty working-tree
//!    changes as a patch before the agent touches files.
//! 2. [`GitHarness::guard_clean_snapshot`] refuses to snapshot when the tree
//!    has uncommitted *tracked* changes that don't belong to this session
//!    (dirty tree protection).
//! 3. [`GitHarness::generate_commit_message`] derives a Conventional Commit
//!    subject from the staged diff.
//! 4. [`GitHarness::undo`] restores HEAD and re-applies the saved patch,
//!    rolling back whatever the agent did.
//!
//! All git invocations are synchronous subprocess calls; the harness is
//! intended for agent/tool code paths, not hot loops.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{Error, Result};

/// A recorded pre-run repository state.
#[derive(Debug, Clone)]
pub struct RepoSnapshot {
    /// HEAD commit at snapshot time (`None` on an unborn branch).
    pub head: Option<String>,
    /// Working-tree patch against HEAD (empty if the tree was clean).
    pub dirty_patch: String,
}

/// Transactional wrapper around a git working tree.
pub struct GitHarness {
    repo_root: PathBuf,
}

impl GitHarness {
    /// Open the repository containing `path` (uses `git rev-parse --show-toplevel`).
    pub fn open(path: &Path) -> Result<Self> {
        let root = git_stdout(path, &["rev-parse", "--show-toplevel"])?;
        Ok(Self {
            repo_root: PathBuf::from(root.trim()),
        })
    }

    /// Repository root this harness operates on.
    pub fn repo_root(&self) -> &Path {
        &self.repo_root
    }

    fn run(&self, args: &[&str]) -> Result<String> {
        git_stdout(&self.repo_root, args)
    }

    /// True when tracked files differ from HEAD (staged or unstaged).
    pub fn has_dirty_tracked_files(&self) -> Result<bool> {
        let status = self.run(&["status", "--porcelain", "--untracked-files=no"])?;
        Ok(!status.trim().is_empty())
    }

    /// Record HEAD + dirty patch. Fails if the tree already has uncommitted
    /// tracked changes and `allow_dirty` is false (dirty-tree protection).
    pub fn snapshot(&self, allow_dirty: bool) -> Result<RepoSnapshot> {
        if !allow_dirty && self.has_dirty_tracked_files()? {
            return Err(Error::Config(
                "Working tree has uncommitted changes; commit or stash them before running a transactional edit.".to_string(),
            ));
        }
        let head = self
            .run(&["rev-parse", "--verify", "HEAD"])
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        // Capture both staged and unstaged changes as one patch.
        let mut patch = self.run(&["diff", "HEAD", "--binary"]).unwrap_or_default();
        if patch.is_empty() {
            patch = self.run(&["diff", "--binary"]).unwrap_or_default();
        }
        Ok(RepoSnapshot {
            head,
            dirty_patch: patch,
        })
    }

    /// Convenience wrapper: snapshot with dirty-tree protection enabled.
    pub fn guard_clean_snapshot(&self) -> Result<RepoSnapshot> {
        self.snapshot(false)
    }

    /// Stage everything and commit with a derived Conventional Commit message.
    /// Returns the new commit's short hash. `None` when there is nothing
    /// to commit.
    pub fn commit_transaction(&self, fallback_subject: &str) -> Result<Option<String>> {
        self.run(&["add", "-A"])?;
        if !self.has_dirty_tracked_files()? {
            return Ok(None);
        }
        let message = self.generate_commit_message(fallback_subject)?;
        let tmp = std::env::temp_dir().join(format!("hermes-commit-msg-{}", std::process::id()));
        std::fs::write(&tmp, &message)
            .map_err(|e| Error::Config(format!("failed writing commit message file: {}", e)))?;
        let tmp_str = tmp.to_string_lossy().to_string();
        let result = self.run(&["commit", "-F", &tmp_str]);
        let _ = std::fs::remove_file(&tmp);
        result?;
        let hash = self.run(&["rev-parse", "--short", "HEAD"])?;
        Ok(Some(hash.trim().to_string()))
    }

    /// Derive a Conventional Commit subject from the staged diff.
    ///
    /// Heuristic: `test:` when only test files changed, `docs:` for docs,
    /// `chore:` for dependency manifests, else `feat:` for new files and
    /// `fix:`/`refactor:` based on deletion-to-addition ratio.
    pub fn generate_commit_message(&self, fallback_subject: &str) -> Result<String> {
        let stat = self.run(&["diff", "--cached", "--numstat"])?;
        let name_status = self.run(&["diff", "--cached", "--name-status"])?;
        let subject = classify_commit(&stat, &name_status, fallback_subject);
        Ok(subject)
    }

    /// Restore a snapshot: reset tracked files to the recorded HEAD and
    /// re-apply the pre-existing dirty patch. Untracked files created after
    /// the snapshot are *not* removed unless they collide with the patch.
    pub fn undo(&self, snapshot: &RepoSnapshot) -> Result<()> {
        if let Some(head) = &snapshot.head {
            self.run(&["reset", "--hard", head])?;
        } else {
            // Unborn branch: no HEAD to reset to; drop tracked changes.
            self.run(&["checkout", "--", "."]).ok();
        }
        if !snapshot.dirty_patch.is_empty() {
            // 3way tolerates minor drift between snapshot and undo time.
            let tmp =
                std::env::temp_dir().join(format!("hermes-undo-{}.patch", std::process::id()));
            std::fs::write(&tmp, &snapshot.dirty_patch)
                .map_err(|e| Error::Config(format!("failed writing undo patch: {}", e)))?;
            let tmp_str = tmp.to_string_lossy().to_string();
            let result = self.run(&["apply", "--3way", &tmp_str]);
            let _ = std::fs::remove_file(&tmp);
            if let Err(e) = result {
                return Err(Error::Config(format!(
                    "undo restored HEAD but failed to re-apply pre-existing changes: {}",
                    e
                )));
            }
        }
        Ok(())
    }
}

fn git_stdout(dir: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|e| Error::Config(format!("failed to run git {:?}: {}", args, e)))?;
    if !output.status.success() {
        return Err(Error::Config(format!(
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Classify a Conventional Commit subject from staged numstat/name-status.
/// Pure so it can be unit-tested without a repository.
fn classify_commit(numstat: &str, name_status: &str, fallback_subject: &str) -> String {
    let mut adds = 0u64;
    let mut dels = 0u64;
    let mut files: Vec<&str> = Vec::new();
    let mut saw_new_file = false;

    for line in numstat.lines() {
        let mut parts = line.split('\t');
        let a = parts.next().unwrap_or("0");
        let d = parts.next().unwrap_or("0");
        if let Some(path) = parts.next() {
            files.push(path);
        }
        adds += a.parse::<u64>().unwrap_or(0);
        dels += d.parse::<u64>().unwrap_or(0);
    }
    for line in name_status.lines() {
        if line.starts_with('A') {
            saw_new_file = true;
        }
    }

    let only_tests = !files.is_empty()
        && files.iter().all(|f| {
            f.contains("test")
                || f.contains("tests/")
                || f.ends_with("_test.go")
                || f.ends_with(".snap")
        });
    let only_docs = !files.is_empty()
        && files
            .iter()
            .all(|f| f.ends_with(".md") || f.starts_with("docs/") || f.contains("README"));
    let only_deps = !files.is_empty()
        && files.iter().all(|f| {
            f.ends_with("Cargo.toml")
                || f.ends_with("Cargo.lock")
                || f.ends_with("package.json")
                || f.ends_with("package-lock.json")
                || f.ends_with("pyproject.toml")
        });

    let kind = if only_tests {
        "test"
    } else if only_docs {
        "docs"
    } else if only_deps {
        "chore"
    } else if saw_new_file && dels == 0 {
        "feat"
    } else if dels > adds {
        "refactor"
    } else if adds > 0 && dels > 0 {
        "fix"
    } else {
        "feat"
    };

    let scope = dominant_dir(&files);
    let subject = if scope.is_empty() {
        format!("{}: {}", kind, fallback_subject)
    } else {
        format!("{}({}): {}", kind, scope, fallback_subject)
    };
    // Conventional Commits subjects stay under ~72 chars.
    if subject.len() > 72 {
        subject.chars().take(72).collect()
    } else {
        subject
    }
}

/// Most common top-level directory among changed files, lowercased; used as
/// the conventional-commit scope. Empty when files span multiple roots.
fn dominant_dir(files: &[&str]) -> String {
    let mut roots: Vec<&str> = Vec::new();
    for f in files {
        let root = f.split('/').next().unwrap_or("");
        if !root.is_empty() && root.contains('.') {
            continue; // top-level file like Cargo.toml
        }
        if !roots.contains(&root) {
            roots.push(root);
        }
    }
    if roots.len() == 1 {
        roots[0].to_string()
    } else {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_test_only_changes() {
        let subject = classify_commit(
            "10\t2\tcrates/core/tests/foo.rs\n",
            "M\tcrates/core/tests/foo.rs\n",
            "covers edge cases",
        );
        assert!(subject.starts_with("test"), "got: {}", subject);
    }

    #[test]
    fn classify_docs_only_changes() {
        let subject = classify_commit("5\t1\tREADME.md\n", "M\tREADME.md\n", "update usage");
        assert!(subject.starts_with("docs:"), "got: {}", subject);
    }

    #[test]
    fn classify_new_file_feat() {
        let subject = classify_commit(
            "80\t0\tcrates/core/src/new.rs\n",
            "A\tcrates/core/src/new.rs\n",
            "add new module",
        );
        assert!(subject.starts_with("feat"), "got: {}", subject);
    }

    #[test]
    fn classify_refactor_when_deletions_dominate() {
        let subject = classify_commit(
            "5\t40\tsrc/lib.rs\n",
            "M\tsrc/lib.rs\n",
            "simplify internals",
        );
        assert!(subject.starts_with("refactor"), "got: {}", subject);
    }

    #[test]
    fn classify_scope_from_single_root() {
        let subject = classify_commit(
            "3\t1\tcrates/core/src/a.rs\n2\t1\tcrates/core/src/b.rs\n",
            "M\tcrates/core/src/a.rs\nM\tcrates/core/src/b.rs\n",
            "touch core",
        );
        // scope only when all files share ONE top-level dir — here they don't
        // ("crates" is shared) so scope appears.
        assert!(subject.contains("(crates)"), "got: {}", subject);
    }

    #[test]
    fn classify_truncates_long_subjects() {
        let long = "x".repeat(200);
        let subject = classify_commit("1\t1\tREADME.md\n", "M\tREADME.md\n", &long);
        assert!(subject.len() <= 72);
    }

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn init_repo() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "hermes_githarness_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init"]);
        git(&dir, &["config", "user.name", "Hermes Test"]);
        git(&dir, &["config", "user.email", "hermes@example.com"]);
        std::fs::write(dir.join("base.txt"), "base\n").unwrap();
        git(&dir, &["add", "-A"]);
        git(&dir, &["commit", "-m", "chore: init"]);
        dir
    }

    #[test]
    fn snapshot_commit_undo_roundtrip() {
        let dir = init_repo();
        let harness = GitHarness::open(&dir).unwrap();

        // Clean tree snapshot succeeds with protection on.
        let snap = harness.guard_clean_snapshot().unwrap();
        assert!(snap.head.is_some());
        assert!(snap.dirty_patch.is_empty());

        // Simulate an agent turn: modify an existing file, add a new one.
        std::fs::write(dir.join("base.txt"), "base\nchanged\n").unwrap();
        std::fs::write(dir.join("new.txt"), "brand new\n").unwrap();

        // Dirty-tree protection triggers now.
        assert!(harness.guard_clean_snapshot().is_err());

        let hash = harness
            .commit_transaction("apply agent edits")
            .unwrap()
            .expect("expected a commit");
        assert!(!hash.is_empty());
        let log = git_stdout(&dir, &["log", "-1", "--pretty=%s"]).unwrap();
        assert!(
            log.contains("apply agent edits"),
            "unexpected subject: {}",
            log
        );

        // Undo returns to the snapshot state: base.txt back to original,
        // new.txt removed by reset --hard (it was committed, not untracked).
        harness.undo(&snap).unwrap();
        let content = std::fs::read_to_string(dir.join("base.txt")).unwrap();
        // Tolerate CRLF normalization (core.autocrlf on Windows).
        assert_eq!(content.replace("\r\n", "\n"), "base\n");
        assert!(!dir.join("new.txt").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn commit_transaction_noop_when_nothing_staged() {
        let dir = init_repo();
        let harness = GitHarness::open(&dir).unwrap();
        let result = harness.commit_transaction("nothing to do").unwrap();
        assert!(result.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
