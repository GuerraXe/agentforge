//! Repository fault injection — SPEC.md §10 Amendment (2026-08-12), docs/ARCHITECTURE.md §9a.
//!
//! A second, distinct mechanism from `mutation::MutationEngine` (docs/ARCHITECTURE.md §9): where
//! mutation testing scans tracked source lines for a small logic-bug regex, `FaultInjector`
//! simulates a broken *repository/environment state* — a missing file, a corrupted config value,
//! a stale generated artifact, a corrupted dependency version pin. These can't be expressed as a
//! `MutationEngine` operator: "this file is absent" and "this generated artifact is stale" are
//! working-tree states a git blob/tree/commit alone can't carry, so — unlike `MutationEngine::
//! apply`, which is pure git plumbing and never creates a worktree (SPEC.md §20, U4) —
//! `FaultInjector::inject` always materializes an isolated fault worktree
//! (`WorktreeKind::Fault`) and writes the fault directly into it. The source repository itself is
//! never touched.
//!
//! Candidate discovery and fault application are pure functions of `(kind, target_glob, seed,
//! fault_version, base_commit)` — mirroring SPEC.md §10's mutation determinism contract. Which
//! `id` a fault is injected under (and therefore its `worktree_path`) is deliberately excluded
//! from that contract, exactly as `task_id` is excluded from `MutationRef.mutant_commit`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use thiserror::Error;

use crate::domain::{FaultKind, FaultRef, FaultSpec, FaultTarget};
use crate::git::worktree::{WorktreeHandle, WorktreeKind, WorktreeManager};
use crate::git::GitRepo;

#[derive(Debug, Error)]
pub enum Error {
    #[error("git error: {0}")]
    Git(#[from] crate::git::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid target_glob {glob:?}: {reason}")]
    InvalidGlob { glob: String, reason: String },
    #[error("no candidate sites found for fault kind {kind:?} matching {glob}")]
    NoCandidates { kind: FaultKind, glob: String },
    #[error("fault id must not be empty")]
    EmptyId,
    #[error("fault id {0:?} is invalid — only ASCII letters, digits, '-', and '_' are allowed")]
    InvalidId(String),
    #[error("candidate path {0:?} escapes the fault workspace root — refusing to touch it")]
    PathEscapesWorkspace(String),
    #[error(
        "candidate path {0:?} is a symlink inside the fault workspace — refusing to follow it \
         off the isolated worktree"
    )]
    TargetIsSymlink(String),
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaultCandidate {
    pub file: String,
    /// `None` for whole-file kinds (`MissingFile`, `StaleArtifact`).
    pub line: Option<u32>,
}

pub struct FaultInjector {
    git: Arc<GitRepo>,
    worktrees: Arc<WorktreeManager>,
}

impl FaultInjector {
    pub fn new(git: Arc<GitRepo>, worktrees: Arc<WorktreeManager>) -> Self {
        FaultInjector { git, worktrees }
    }

    /// Deterministic candidate discovery — same cross-platform normalization as
    /// `MutationEngine::find_candidates` (forward-slash-joined, byte-wise, case-sensitive path
    /// comparison; SPEC.md §20, R4). Every returned `file` is checked against path-traversal
    /// before being handed back, so a caller never receives a candidate `inject`/`restore` would
    /// have to reject later.
    pub fn find_candidates(
        &self,
        base_commit: &str,
        spec: &FaultSpec,
    ) -> Result<Vec<FaultCandidate>> {
        let files =
            crate::mutation::matching_tracked_files(&self.git, base_commit, &spec.target_glob)
                .map_err(|e| match e {
                    crate::mutation::MatchError::InvalidGlob(reason) => Error::InvalidGlob {
                        glob: spec.target_glob.clone(),
                        reason,
                    },
                    crate::mutation::MatchError::Git(e) => Error::Git(e),
                })?;
        for file in &files {
            if !is_safe_relative_path(file) {
                return Err(Error::PathEscapesWorkspace(file.clone()));
            }
        }

        let mut candidates = Vec::new();
        for file in &files {
            match spec.kind {
                FaultKind::MissingFile | FaultKind::StaleArtifact => {
                    candidates.push(FaultCandidate {
                        file: file.clone(),
                        line: None,
                    });
                }
                FaultKind::BrokenConfigValue | FaultKind::DependencyCorruption => {
                    let blob = self.git.read_blob(base_commit, file)?;
                    let text = String::from_utf8_lossy(&blob).into_owned();
                    for (idx, line) in text.split('\n').enumerate() {
                        if is_comment_line(line) {
                            continue;
                        }
                        if line_matches(spec.kind, line) {
                            candidates.push(FaultCandidate {
                                file: file.clone(),
                                line: Some((idx + 1) as u32),
                            });
                        }
                    }
                }
            }
        }
        // Already in (normalized_path, line) order: `files` is sorted, each file's lines are
        // scanned top-to-bottom.
        Ok(candidates)
    }

    /// Selects `candidates[seed % candidates.len()]`, materializes a fresh `WorktreeKind::Fault`
    /// worktree checked out at `base_commit`, and writes the fault directly into it — the source
    /// repository is never opened for writing. `id` must be non-empty and
    /// `[A-Za-z0-9_-]`-only (validated here, not left to the caller, since it becomes both this
    /// fault workspace's directory name and its `Store` key).
    pub fn inject(&self, base_commit: &str, spec: &FaultSpec, id: &str) -> Result<FaultRef> {
        validate_id(id).map_err(|e| match e {
            IdError::Empty => Error::EmptyId,
            IdError::Invalid(id) => Error::InvalidId(id),
        })?;
        let candidates = self.find_candidates(base_commit, spec)?;
        if candidates.is_empty() {
            return Err(Error::NoCandidates {
                kind: spec.kind,
                glob: spec.target_glob.clone(),
            });
        }
        let chosen = &candidates[(spec.seed % candidates.len() as u64) as usize];

        let handle = self.worktrees.create_fault_worktree(id, base_commit)?;
        let target_path =
            safe_join(&handle.path, &chosen.file).map_err(|e| Error::PathEscapesWorkspace(e.0))?;
        reject_symlink(&target_path).map_err(|_| Error::TargetIsSymlink(chosen.file.clone()))?;

        let description =
            match spec.kind {
                FaultKind::MissingFile => {
                    std::fs::remove_file(&target_path)?;
                    format!("deleted tracked file {}", chosen.file)
                }
                FaultKind::StaleArtifact => {
                    std::fs::write(&target_path, STALE_MARKER_CONTENT)?;
                    format!(
                        "{}: overwrote contents with a fixed, timestamp-independent stale marker",
                        chosen.file
                    )
                }
                FaultKind::BrokenConfigValue => {
                    let line = chosen
                        .line
                        .expect("BrokenConfigValue candidates always carry a line");
                    let original = std::fs::read_to_string(&target_path)?;
                    let rewrite = rewrite_line(&original, line, broken_config_replacement)
                        .ok_or_else(|| Error::NoCandidates {
                            kind: spec.kind,
                            glob: spec.target_glob.clone(),
                        })?;
                    std::fs::write(&target_path, &rewrite.new_contents)?;
                    format!(
                        "{}:{line}: replaced `{}` with `{}`",
                        chosen.file,
                        rewrite.original_line.trim(),
                        rewrite.new_line.trim()
                    )
                }
                FaultKind::DependencyCorruption => {
                    let line = chosen
                        .line
                        .expect("DependencyCorruption candidates always carry a line");
                    let original = std::fs::read_to_string(&target_path)?;
                    let rewrite = rewrite_line(&original, line, dependency_corruption_replacement)
                        .ok_or_else(|| Error::NoCandidates {
                            kind: spec.kind,
                            glob: spec.target_glob.clone(),
                        })?;
                    std::fs::write(&target_path, &rewrite.new_contents)?;
                    format!(
                        "{}:{line}: replaced `{}` with `{}`",
                        chosen.file,
                        rewrite.original_line.trim(),
                        rewrite.new_line.trim()
                    )
                }
            };

        let diff_stats = self.git.diff_stats(&handle.path, base_commit)?;

        Ok(FaultRef {
            id: id.to_string(),
            spec: spec.clone(),
            base_commit: base_commit.to_string(),
            selected_target: FaultTarget {
                file: chosen.file.clone(),
                line: chosen.line,
            },
            description,
            worktree_path: handle.path,
            diff_stats,
            applied_at: chrono::Utc::now(),
        })
    }

    /// Reverses the fault in place: `git checkout <base_commit> -- <file>` inside the fault
    /// workspace, which recreates a deleted file or overwrites a rewritten one — the workspace
    /// itself stays alive for reuse. The alternative to `discard` when the workspace is still
    /// needed for something else.
    pub fn restore(&self, fault_ref: &FaultRef) -> Result<()> {
        Ok(self.git.restore_path(
            &fault_ref.worktree_path,
            &fault_ref.base_commit,
            &fault_ref.selected_target.file,
        )?)
    }

    /// Disposes of the entire fault workspace — the alternative to `restore` when the workspace
    /// itself is no longer needed. Idempotent, mirroring `WorktreeManager::remove`.
    pub fn discard(&self, fault_ref: &FaultRef) -> Result<()> {
        let handle = WorktreeHandle {
            path: fault_ref.worktree_path.clone(),
            kind: WorktreeKind::Fault,
        };
        Ok(self.worktrees.remove(&handle)?)
    }
}

const STALE_MARKER_CONTENT: &str = "AGENTFORGE_STALE_MARKER\n";
const BROKEN_CONFIG_SENTINEL: &str = "__AGENTFORGE_BROKEN__";
const CORRUPTED_VERSION_SENTINEL: &str = "0.0.0-agentforge-corrupted";

/// Best-effort comment skip, deliberately broader than `mutation`'s (which only knows `//`):
/// fault targets skew toward config/manifest files, where `#` is the common comment leader
/// (TOML, YAML, `.env`). A heuristic, not a parser — same stated limitation as SPEC.md §10, T4.
fn is_comment_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("//") || trimmed.starts_with('#')
}

fn line_matches(kind: FaultKind, line: &str) -> bool {
    match kind {
        FaultKind::BrokenConfigValue => broken_config_regex().is_match(line),
        FaultKind::DependencyCorruption => dependency_version_regex().is_match(line),
        FaultKind::MissingFile | FaultKind::StaleArtifact => false,
    }
}

/// `key = value` / `key: value` / `"key": value` — TOML/YAML/simple-JSON shaped, not a real
/// parser for any of them (same class of heuristic as `mutation`'s regex operators).
fn broken_config_regex() -> regex::Regex {
    regex::Regex::new(r#"^(\s*"?[A-Za-z0-9_.\-]+"?\s*[:=]\s*)\S.*$"#)
        .expect("fixed, hand-written regex must always compile")
}

/// A semver-shaped substring: `1.2.3`, optionally with a `-`/`+` prerelease/build suffix.
fn dependency_version_regex() -> regex::Regex {
    regex::Regex::new(r"\b\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.\-]+)?\b")
        .expect("fixed, hand-written regex must always compile")
}

fn broken_config_replacement(line: &str) -> Option<String> {
    let caps = broken_config_regex().captures(line)?;
    let prefix = caps.get(1)?.as_str();
    Some(format!("{prefix}{BROKEN_CONFIG_SENTINEL}"))
}

fn dependency_corruption_replacement(line: &str) -> Option<String> {
    let m = dependency_version_regex().find(line)?;
    let mut new_line = String::with_capacity(line.len());
    new_line.push_str(&line[..m.start()]);
    new_line.push_str(CORRUPTED_VERSION_SENTINEL);
    new_line.push_str(&line[m.end()..]);
    Some(new_line)
}

struct LineRewrite {
    original_line: String,
    new_line: String,
    new_contents: String,
}

/// Applies `transform` to `original`'s 1-based `target_line` and splices the result back into
/// the full file — re-scanning the exact line `find_candidates` selected rather than threading a
/// byte range through, mirroring `mutation::mutate_file_contents`'s re-derivation approach.
fn rewrite_line(
    original: &str,
    target_line: u32,
    transform: impl Fn(&str) -> Option<String>,
) -> Option<LineRewrite> {
    let mut lines: Vec<String> = original.split('\n').map(str::to_string).collect();
    let idx = target_line.checked_sub(1)? as usize;
    let original_line = lines.get(idx)?.clone();
    let new_line = transform(&original_line)?;
    lines[idx] = new_line.clone();
    let new_contents = lines.join("\n");
    Some(LineRewrite {
        original_line,
        new_line,
        new_contents,
    })
}

/// Re-exported so existing callers (`fault::validate_id`, `mutant::MutantTester::apply` via
/// `crate::fault::validate_id`) don't need to change — the check itself now lives in
/// `domain::ids`, shared with `workspace::WorkspaceManager` too, rather than three independently
/// mirrored copies of the same `[A-Za-z0-9_-]`-only rule.
pub(crate) use crate::domain::ids::{validate_id, IdError};

/// A tracked path is never expected to contain a parent-dir component or be absolute (git
/// itself refuses to store such paths), but this is checked anyway as defense in depth before
/// any candidate is trusted, and again before every direct filesystem write `inject` performs —
/// the same "catch a future bug, not because it's expected to trigger today" posture as
/// `workspace::ensure_within_state_root`.
///
/// `pub(crate)`: reused by `mutant::MutantTester::apply` for the identical defense-in-depth
/// check against its own candidate paths.
pub(crate) fn is_safe_relative_path(path: &str) -> bool {
    let p = Path::new(path);
    !p.is_absolute()
        && !p
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
}

/// `pub(crate)`: reused by `mutant::MutantTester::apply`, mirroring `FaultInjector::inject`'s own
/// use of it.
pub(crate) fn safe_join(root: &Path, rel: &str) -> std::result::Result<PathBuf, PathEscape> {
    if !is_safe_relative_path(rel) {
        return Err(PathEscape(rel.to_string()));
    }
    Ok(root.join(rel))
}

/// Defense against a hostile target repository: `find_candidates`/`is_safe_relative_path` only
/// check the *string shape* of a tracked path (no `..`/absolute components) — they say nothing
/// about what the path resolves to on disk once checked out. A git-tracked symlink (git stores
/// these as ordinary blobs with mode `120000`; `git worktree add`/`checkout` materializes them as
/// real symlinks on any platform/config that honors `core.symlinks`) whose target points outside
/// the fault/mutant worktree — into another worktree, the user's home directory, anywhere the
/// process can write — would otherwise be silently followed by `std::fs::write`/
/// `std::fs::read_to_string`/`std::fs::remove_file`, letting an untrusted repository under test
/// escape its isolated worktree the moment `fault inject`/`mutant apply` touches that candidate.
/// Checked with `symlink_metadata` (never follows the link itself) right after the candidate's
/// path is resolved, before any read/write/remove touches it — refusing unconditionally rather
/// than trying to characterize "safe" symlink targets.
///
/// `pub(crate)`: reused by `mutant::MutantTester::apply` for its own candidate target, mirroring
/// `safe_join`'s reuse.
pub(crate) fn reject_symlink(path: &Path) -> std::result::Result<(), PathEscape> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            Err(PathEscape(path.to_string_lossy().into_owned()))
        }
        _ => Ok(()),
    }
}

/// A minimal, module-agnostic error for the shared `safe_join` helper — see `domain::ids::IdError`.
#[derive(Debug)]
pub(crate) struct PathEscape(pub(crate) String);
