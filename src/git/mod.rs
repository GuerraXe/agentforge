//! The one safe Git abstraction — SPEC.md §6, docs/ARCHITECTURE.md §6.
//!
//! Every method shells out to the real `git` binary via `Executor` — no other module runs a
//! `git` command directly, and `git` itself never spawns a process except through `Executor`,
//! which is what makes "all subprocess execution goes through one controlled layer" true of git
//! operations too, not just agent/evaluator ones. Every invocation passes arguments as an array
//! (`ProcessSpec.args: Vec<String>`), never a shell-interpolated string.

pub mod worktree;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use thiserror::Error;

use crate::audit::NullAuditSink;
use crate::domain::{CommandPolicy, DiffStats, ExecutionBudget, ProcessSpec};
use crate::exec::Executor;

#[derive(Debug, Error)]
pub enum Error {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("git execution error: {0}")]
    Exec(#[from] crate::exec::Error),
    #[error("git command failed: {0}")]
    CommandFailed(String),
    #[error("not a valid commit-ish: {0}")]
    InvalidCommitish(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Generous, fixed budget for AgentForge's own internal git plumbing calls — these aren't
/// policy-governed (that's for the agent/evaluator processes the *user* configures), so a
/// single conservative constant is enough rather than a configurable field.
/// `write_commit`'s fixed author/committer date — deliberately not the wall clock, so the
/// resulting commit SHA is a pure function of `(parent, changed_files, message)`.
const FIXED_COMMIT_DATE: &str = "1970-01-01T00:00:00Z";

fn git_budget() -> ExecutionBudget {
    ExecutionBudget {
        timeout_secs: 120,
        max_output_bytes: 100_000_000,
    }
}

pub struct GitRepo {
    root: PathBuf,
    exec: Arc<dyn Executor>,
}

impl GitRepo {
    /// Validates `root` exists and is a real git repository — SPEC.md §6's "validated paths and
    /// repository boundaries."
    pub fn open(root: impl Into<PathBuf>, exec: Arc<dyn Executor>) -> Result<Self> {
        let root = root.into();
        if !root.is_dir() {
            return Err(Error::CommandFailed(format!(
                "{} is not an existing directory",
                root.display()
            )));
        }
        let repo = GitRepo { root, exec };
        repo.run_git(&repo.root.clone(), &["rev-parse", "--git-dir"])?;
        Ok(repo)
    }

    /// Resolves any commit-ish to a 40-hex SHA — SPEC.md §20 (R1). Rejects anything that
    /// doesn't resolve to a real commit object, rather than echoing the input back.
    pub fn resolve_commit(&self, commit_ish: &str) -> Result<String> {
        if commit_ish.trim().is_empty() {
            return Err(Error::InvalidCommitish(
                "commit-ish must not be empty".to_string(),
            ));
        }
        let spec_arg = format!("{commit_ish}^{{commit}}");
        let sha = self
            .run_git(
                &self.root.clone(),
                &["rev-parse", "--verify", "--quiet", &spec_arg],
            )
            .map_err(|_| Error::InvalidCommitish(commit_ish.to_string()))?;
        if sha.len() != 40 || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(Error::InvalidCommitish(commit_ish.to_string()));
        }
        Ok(sha)
    }

    pub fn status_porcelain(&self, path: &Path) -> Result<String> {
        self.run_git(path, &["status", "--porcelain"])
    }

    /// The commit `worktree_path` currently has checked out — used to inspect a workspace
    /// without assuming which base ref it was created from is still meaningful (a workspace can
    /// be checked out to a different commit later, e.g. by `bisect`).
    pub fn head_commit(&self, worktree_path: &Path) -> Result<String> {
        self.run_git(worktree_path, &["rev-parse", "HEAD"])
    }

    pub fn worktree_add(&self, path: &Path, commit: &str) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let path_str = path.to_string_lossy().to_string();
        self.run_git(
            &self.root.clone(),
            &["worktree", "add", "--detach", &path_str, commit],
        )?;
        Ok(())
    }

    pub fn worktree_remove(&self, path: &Path) -> Result<()> {
        let path_str = path.to_string_lossy().to_string();
        self.run_git(
            &self.root.clone(),
            &["worktree", "remove", "--force", &path_str],
        )?;
        Ok(())
    }

    /// Clears administrative records (`.git/worktrees/<name>`) for worktrees whose working
    /// directory is already gone — used to keep `git worktree list` honest after an idempotent
    /// removal skipped calling `worktree remove` because there was nothing left on disk to
    /// remove.
    pub fn prune_worktrees(&self) -> Result<()> {
        self.run_git(&self.root.clone(), &["worktree", "prune"])?;
        Ok(())
    }

    pub fn worktree_checkout(&self, worktree_path: &Path, commit: &str) -> Result<()> {
        self.run_git(worktree_path, &["checkout", "--detach", "--quiet", commit])?;
        Ok(())
    }

    /// Restores exactly one path inside `worktree_path` to its content at `commit` — recreating
    /// it if currently absent, overwriting it if currently modified. This is
    /// `fault::FaultInjector::restore`'s entire mechanism (SPEC.md §10 Amendment): every fault
    /// kind it supports (delete, or rewrite) is undone by checking out the single file the fault
    /// touched, never the whole worktree.
    pub fn restore_path(&self, worktree_path: &Path, commit: &str, rel_path: &str) -> Result<()> {
        self.run_git(
            worktree_path,
            &["checkout", "--quiet", commit, "--", rel_path],
        )?;
        Ok(())
    }

    /// Unified diff of the worktree against `base_ref`. Note: like plain `git diff`, this does
    /// not include never-`git add`-ed untracked files — acceptable for the shared foundation
    /// this module currently supports (patch capture as part of full experiment orchestration
    /// is out of scope here; see SPEC.md §4).
    pub fn diff(&self, worktree_path: &Path, base_ref: &str) -> Result<String> {
        self.run_git(worktree_path, &["diff", base_ref])
    }

    pub fn diff_stats(&self, worktree_path: &Path, base_ref: &str) -> Result<DiffStats> {
        let raw = self.run_git(worktree_path, &["diff", "--numstat", base_ref])?;
        Ok(parse_numstat(&raw))
    }

    pub fn apply_patch(&self, worktree_path: &Path, patch: &str) -> Result<()> {
        let patch_path = unique_temp_path("patch");
        std::fs::write(&patch_path, patch)?;
        let patch_str = patch_path.to_string_lossy().to_string();
        let result = self.run_git(worktree_path, &["apply", &patch_str]);
        crate::exec::remove_file_best_effort(&patch_path);
        result.map(|_| ())
    }

    /// Ordered candidate list for bisect: excludes `good`, includes `bad`, chronological —
    /// SPEC.md §13.
    pub fn rev_list_ancestry_path(&self, good: &str, bad: &str) -> Result<Vec<String>> {
        let range = format!("{good}..{bad}");
        let raw = self.run_git(
            &self.root.clone(),
            &["rev-list", "--ancestry-path", "--reverse", &range],
        )?;
        Ok(raw
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect())
    }

    /// SPEC.md §13: bisect requires `good` to be an ancestor of `bad`. Exit code 0/1 here is a
    /// valid yes/no answer, not a command failure — handled directly rather than through
    /// `run_git`'s "nonzero exit is an error" convention.
    pub fn is_ancestor(&self, ancestor: &str, descendant: &str) -> Result<bool> {
        let outcome = self.spawn_git(
            &self.root.clone(),
            &["merge-base", "--is-ancestor", ancestor, descendant],
            &[],
        )?;
        crate::exec::remove_file_best_effort(&outcome.stdout_path);
        crate::exec::remove_file_best_effort(&outcome.stderr_path);
        Ok(outcome.exit_code == Some(0))
    }

    /// Every file tracked at `commit`, forward-slash-joined and repo-root-relative — the input
    /// `mutation::MutationEngine::find_candidates` globs against, per SPEC.md §10's requirement
    /// that candidate discovery reads blobs at `base_commit`, never a checked-out worktree.
    pub fn list_tree_files(&self, commit: &str) -> Result<Vec<String>> {
        let raw = self.run_git(
            &self.root.clone(),
            &["ls-tree", "-r", "--name-only", "-z", commit],
        )?;
        Ok(raw
            .split('\0')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(|p| p.replace('\\', "/"))
            .collect())
    }

    /// Structured diff between two commits — unlike `diff_stats` (working tree vs. a ref), this
    /// never touches a worktree, so it's safe to call against a mutation ref with no worktree of
    /// its own (SPEC.md §10, §20 U4).
    pub fn diff_stats_between(&self, from_commit: &str, to_commit: &str) -> Result<DiffStats> {
        let raw = self.run_git(
            &self.root.clone(),
            &["diff", "--numstat", from_commit, to_commit],
        )?;
        Ok(parse_numstat(&raw))
    }

    /// Points `ref_name` at `commit` — used both by `write_commit`'s own `update-ref` step and,
    /// standalone, by callers that need to move an already-written commit to a different ref
    /// (e.g. `mutate` re-homing a sanity-gate-rejected mutant under
    /// `refs/agentforge/mutants/rejected/...`).
    pub fn update_ref(&self, ref_name: &str, commit: &str) -> Result<()> {
        self.run_git(&self.root.clone(), &["update-ref", ref_name, commit])?;
        Ok(())
    }

    /// Deletes `ref_name` — the entire cleanup surface for a mutant commit (SPEC.md §10): pure
    /// git plumbing never created a worktree or touched `HEAD`, so nothing else needs restoring.
    /// Idempotent: deleting an already-absent ref is not an error.
    pub fn delete_ref(&self, ref_name: &str) -> Result<()> {
        match self.run_git(&self.root.clone(), &["update-ref", "-d", ref_name]) {
            Ok(_) => Ok(()),
            Err(Error::CommandFailed(_)) => Ok(()),
            Err(e) => Err(e),
        }
    }

    pub fn read_blob(&self, commit: &str, path: &str) -> Result<Vec<u8>> {
        let arg = format!("{commit}:{path}");
        let outcome = self.spawn_git(&self.root.clone(), &["show", &arg], &[])?;
        let result = if outcome.exit_code != Some(0) {
            Err(Error::CommandFailed(format!("git show {arg} failed")))
        } else {
            std::fs::read(&outcome.stdout_path).map_err(Error::Io)
        };
        crate::exec::remove_file_best_effort(&outcome.stdout_path);
        crate::exec::remove_file_best_effort(&outcome.stderr_path);
        result
    }

    /// Pure plumbing: write blob(s) + tree + commit objects, move `update_ref`, return the new
    /// commit SHA. No worktree is touched — SPEC.md §20 (U4). Uses a throwaway `GIT_INDEX_FILE`
    /// so this never disturbs the real repository's index.
    ///
    /// The commit's author/committer date is fixed (`GIT_AUTHOR_DATE`/`GIT_COMMITTER_DATE`
    /// pinned to the Unix epoch), not the wall clock — the same `(parent, changed_files,
    /// message)` always produces the byte-identical commit SHA, regardless of when or how many
    /// times it's called. This is what makes `mutation::MutationEngine::apply` a pure function of
    /// its inputs (SPEC.md §10's determinism contract) rather than merely deterministic-if-lucky
    /// about which second the wall clock reads.
    pub fn write_commit(
        &self,
        parent: &str,
        changed_files: &[(String, Vec<u8>)],
        message: &str,
        update_ref: &str,
    ) -> Result<String> {
        let index_path = unique_temp_path("index");
        let index_env = vec![(
            "GIT_INDEX_FILE".to_string(),
            index_path.to_string_lossy().to_string(),
        )];
        let root = self.root.clone();

        let result = (|| -> Result<String> {
            self.run_git_env(&root, &["read-tree", parent], &index_env)?;

            for (path, contents) in changed_files {
                let tmp = unique_temp_path("blob");
                std::fs::write(&tmp, contents)?;
                let tmp_str = tmp.to_string_lossy().to_string();
                let blob_sha =
                    self.run_git_env(&root, &["hash-object", "-w", "--", &tmp_str], &index_env)?;
                crate::exec::remove_file_best_effort(&tmp);
                self.run_git_env(
                    &root,
                    &[
                        "update-index",
                        "--add",
                        "--cacheinfo",
                        "100644",
                        &blob_sha,
                        path,
                    ],
                    &index_env,
                )?;
            }

            let tree_sha = self.run_git_env(&root, &["write-tree"], &index_env)?;
            let mut commit_env = index_env.clone();
            commit_env.push(("GIT_AUTHOR_DATE".to_string(), FIXED_COMMIT_DATE.to_string()));
            commit_env.push((
                "GIT_COMMITTER_DATE".to_string(),
                FIXED_COMMIT_DATE.to_string(),
            ));
            let commit_sha = self.run_git_env(
                &root,
                &["commit-tree", &tree_sha, "-p", parent, "-m", message],
                &commit_env,
            )?;
            self.run_git_env(&root, &["update-ref", update_ref, &commit_sha], &index_env)?;
            Ok(commit_sha)
        })();

        crate::exec::remove_file_best_effort(&index_path);
        result
    }

    fn run_git(&self, cwd: &Path, args: &[&str]) -> Result<String> {
        self.run_git_env(cwd, args, &[])
    }

    /// Retries a bounded number of times, on Windows only, when git itself reports "Permission
    /// denied" while writing a loose object/index/lock file. This is a well-documented Windows
    /// characteristic, not an AgentForge bug: another process (most commonly real-time antivirus
    /// scanning) can briefly hold an exclusive handle on a just-created file, so `git`'s own
    /// `CreateFile` call loses a race it would win a few milliseconds later. Under heavy
    /// concurrent git activity (many worktrees/commits at once, as `race`/the test suite both do)
    /// this is no longer rare enough to ignore. Any other failure (including a genuine, durable
    /// permission problem, which keeps failing past the retry budget) still surfaces as `Err`.
    fn run_git_env(
        &self,
        cwd: &Path,
        args: &[&str],
        extra_env: &[(String, String)],
    ) -> Result<String> {
        const MAX_ATTEMPTS: u32 = if cfg!(windows) { 8 } else { 1 };
        let mut attempt = 0;
        loop {
            attempt += 1;
            let outcome = self.spawn_git(cwd, args, extra_env)?;
            let stdout = std::fs::read_to_string(&outcome.stdout_path).unwrap_or_default();
            let stderr = std::fs::read_to_string(&outcome.stderr_path).unwrap_or_default();
            // Every internal git plumbing call lands here — unlike `workspace exec`'s captured
            // output (deliberately left on disk and printed to the user for inspection), nothing
            // ever reads these paths again once this function returns, so leaving them behind
            // would leak two files into the OS temp dir per git invocation forever (this codebase
            // runs many dozens of these per experiment/race/bisect).
            crate::exec::remove_file_best_effort(&outcome.stdout_path);
            crate::exec::remove_file_best_effort(&outcome.stderr_path);
            if outcome.exit_code == Some(0) && !outcome.timed_out {
                return Ok(stdout.trim_end().to_string());
            }
            let transient_windows_lock = cfg!(windows) && stderr.contains("Permission denied");
            if transient_windows_lock && attempt < MAX_ATTEMPTS {
                std::thread::sleep(std::time::Duration::from_millis(20 * u64::from(attempt)));
                continue;
            }
            return Err(Error::CommandFailed(format!(
                "git {} (in {}) failed: exit={:?} timed_out={} stderr={}",
                args.join(" "),
                cwd.display(),
                outcome.exit_code,
                outcome.timed_out,
                stderr.trim()
            )));
        }
    }

    fn spawn_git(
        &self,
        cwd: &Path,
        args: &[&str],
        extra_env: &[(String, String)],
    ) -> Result<crate::domain::ProcessOutcome> {
        let spec = ProcessSpec {
            program: "git".to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
            extra_env: extra_env.to_vec(),
        };
        // git is AgentForge's own trusted plumbing, never agent/evaluator-controlled — no
        // command-program or filesystem-root restriction applies to it.
        Ok(self.exec.spawn(
            &spec,
            cwd,
            &[],
            &git_budget(),
            &CommandPolicy::unrestricted(),
            &NullAuditSink,
        )?)
    }
}

/// Shared by `diff_stats` (working tree vs. a ref) and `diff_stats_between` (commit vs. commit) —
/// both just run `git diff --numstat` with different arguments and parse the same output shape.
fn parse_numstat(raw: &str) -> DiffStats {
    let mut stats = DiffStats::default();
    for line in raw.lines() {
        let mut parts = line.split('\t');
        let (Some(added), Some(removed)) = (parts.next(), parts.next()) else {
            continue;
        };
        stats.files_changed += 1;
        stats.lines_added += added.trim().parse::<u32>().unwrap_or(0);
        stats.lines_removed += removed.trim().parse::<u32>().unwrap_or(0);
    }
    stats
}

fn unique_temp_path(kind: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "agentforge-{kind}-{}",
        crate::domain::ids::new_id(chrono::Utc::now())
    ))
}
