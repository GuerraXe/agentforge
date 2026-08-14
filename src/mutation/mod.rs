//! Reproducibility infrastructure shared with experiments — SPEC.md §10, docs/ARCHITECTURE.md §9.
//!
//! `apply()` calls the same `GitRepo::write_commit` plumbing primitive nothing else currently
//! needs; `sanity_check()` calls the same `Evaluator::evaluate()` against the same
//! Evaluation-flavor worktree that `task add`'s baseline capture uses. No new way to touch git,
//! spawn a process, or judge a patch is introduced here.
//!
//! Candidate discovery and mutation application are pure functions of `(operator, target_glob,
//! seed, operator_version, base_commit)` — SPEC.md §10's determinism contract. Both read blobs
//! directly via `GitRepo::list_tree_files`/`read_blob`, never a checked-out worktree, so `apply`
//! never creates one (SPEC.md §20, U4).

use std::sync::Arc;

use thiserror::Error;

use crate::domain::{
    EvaluatorSpec, EvaluatorVerdict, MutationOperator, MutationRef, MutationSpec, MutationTarget,
};
use crate::evaluator::Evaluator;
use crate::git::worktree::WorktreeManager;
use crate::git::GitRepo;

#[derive(Debug, Error)]
pub enum Error {
    #[error("git error: {0}")]
    Git(#[from] crate::git::Error),
    #[error("evaluator error: {0}")]
    Evaluator(#[from] crate::evaluator::Error),
    #[error("invalid target_glob {glob:?}: {reason}")]
    InvalidGlob { glob: String, reason: String },
    #[error("no candidate sites found for operator {operator:?} matching {glob}")]
    NoCandidates {
        operator: crate::domain::MutationOperator,
        glob: String,
    },
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub file: String,
    pub line: u32,
    pub column: u32,
}

/// A minimal, module-agnostic error for `matching_tracked_files` below — same pattern as
/// `domain::ids::IdError`. `fault::Error` and `mutant::Error` each convert it into their own
/// `InvalidGlob`/`Git` variant.
pub(crate) enum MatchError {
    InvalidGlob(String),
    Git(crate::git::Error),
}

/// Compiles `target_glob` and returns every file tracked at `base_commit` that matches it,
/// byte-wise sorted (forward-slash-joined, case-sensitive — SPEC.md §20, R4). The candidate-
/// discovery scaffolding shared verbatim by `MutationEngine::find_candidates`,
/// `fault::FaultInjector::find_candidates`, and `mutant::MutantTester::find_candidates`, which
/// then differ only in how they scan the matched files.
pub(crate) fn matching_tracked_files(
    git: &GitRepo,
    base_commit: &str,
    target_glob: &str,
) -> std::result::Result<Vec<String>, MatchError> {
    let pattern =
        glob::Pattern::new(target_glob).map_err(|e| MatchError::InvalidGlob(e.to_string()))?;
    let mut files = git.list_tree_files(base_commit).map_err(MatchError::Git)?;
    files.retain(|f| pattern.matches(f));
    files.sort();
    Ok(files)
}

pub struct MutationEngine {
    git: Arc<GitRepo>,
    worktrees: Arc<WorktreeManager>,
    evaluator: Arc<Evaluator>,
}

impl MutationEngine {
    pub fn new(
        git: Arc<GitRepo>,
        worktrees: Arc<WorktreeManager>,
        evaluator: Arc<Evaluator>,
    ) -> Self {
        MutationEngine {
            git,
            worktrees,
            evaluator,
        }
    }

    /// Deterministic candidate discovery — SPEC.md §10. Cross-platform normalization: paths
    /// compared forward-slash-joined, byte-wise, case-sensitive, regardless of host OS.
    pub fn find_candidates(
        &self,
        base_commit: &str,
        spec: &MutationSpec,
    ) -> Result<Vec<Candidate>> {
        let files = matching_tracked_files(&self.git, base_commit, &spec.target_glob).map_err(
            |e| match e {
                MatchError::InvalidGlob(reason) => Error::InvalidGlob {
                    glob: spec.target_glob.clone(),
                    reason,
                },
                MatchError::Git(e) => Error::Git(e),
            },
        )?;

        let mut candidates = Vec::new();
        for file in &files {
            let blob = self.git.read_blob(base_commit, file)?;
            let text = String::from_utf8_lossy(&blob).into_owned();
            for (idx, line) in text.split('\n').enumerate() {
                if is_comment_line(line) {
                    continue;
                }
                for m in scan_line(spec.operator, line) {
                    candidates.push(Candidate {
                        file: file.clone(),
                        line: (idx + 1) as u32,
                        column: m.column as u32,
                    });
                }
            }
        }
        // Already produced in (normalized_path, line, column) order: `files` is sorted, and each
        // file's lines/matches are scanned top-to-bottom, left-to-right.
        Ok(candidates)
    }

    /// Pure git plumbing — no worktree created for this step. `seed % candidates.len()` selects
    /// the mutated site — SPEC.md §20 (U4).
    pub fn apply(
        &self,
        base_commit: &str,
        spec: &MutationSpec,
        task_id: &str,
    ) -> Result<MutationRef> {
        let candidates = self.find_candidates(base_commit, spec)?;
        if candidates.is_empty() {
            return Err(Error::NoCandidates {
                operator: spec.operator,
                glob: spec.target_glob.clone(),
            });
        }
        let chosen = &candidates[(spec.seed % candidates.len() as u64) as usize];

        let original_bytes = self.git.read_blob(base_commit, &chosen.file)?;
        let original = String::from_utf8_lossy(&original_bytes).into_owned();
        // `chosen` came from `find_candidates` scanning this exact blob, so a matching site at
        // (line, column) is always found — a missing match here would mean the file changed
        // between the two reads, which can't happen against an immutable commit object.
        let mutated = mutate_file_contents(&original, spec.operator, chosen.line, chosen.column)
            .expect("candidate selected by find_candidates must be reproducible by apply");

        let mutant_ref = format!("refs/agentforge/mutants/{task_id}");
        // Deliberately excludes `task_id`: the commit object itself must be a pure function of
        // `(operator, target_glob, seed, operator_version, base_commit)` — SPEC.md §10's
        // determinism contract — so the same spec applied under two different task ids still
        // produces a byte-identical `mutant_commit`. `task_id` only selects which ref points at
        // that commit, not the commit's own content.
        let message = format!(
            "agentforge mutate: {:?} at {}:{}:{} (seed={}, operator_version={})",
            spec.operator,
            chosen.file,
            chosen.line,
            chosen.column,
            spec.seed,
            spec.operator_version
        );
        let mutant_commit = self.git.write_commit(
            base_commit,
            &[(chosen.file.clone(), mutated.into_bytes())],
            &message,
            &mutant_ref,
        )?;
        let diff_stats = self.git.diff_stats_between(base_commit, &mutant_commit)?;

        Ok(MutationRef {
            spec: spec.clone(),
            base_commit: base_commit.to_string(),
            mutant_commit,
            sanity_checked: false,
            selected_target: MutationTarget {
                file: chosen.file.clone(),
                line: chosen.line,
                column: chosen.column,
            },
            mutant_ref,
            diff_stats,
            applied_at: chrono::Utc::now(),
        })
    }

    /// Uses an Evaluation-flavor worktree + the shared `Evaluator`. The worktree is always
    /// removed before returning, even if `evaluate()` itself errors.
    pub fn sanity_check(
        &self,
        mutation_ref: &MutationRef,
        evaluator_spec: &EvaluatorSpec,
    ) -> Result<EvaluatorVerdict> {
        let handle = self
            .worktrees
            .create_evaluation_worktree(&mutation_ref.mutant_commit)?;
        let outcome =
            self.evaluator
                .evaluate(&handle.path, evaluator_spec, &crate::audit::NullAuditSink);
        let cleanup = self.worktrees.remove(&handle);
        let verdict = outcome?;
        cleanup?;
        Ok(verdict)
    }

    /// The entire cleanup surface for a mutant no longer needed: deletes `mutant_ref`. Pure git
    /// plumbing never created a worktree or touched `HEAD`, so nothing else needs restoring —
    /// SPEC.md §10.
    pub fn discard(&self, mutation_ref: &MutationRef) -> Result<()> {
        Ok(self.git.delete_ref(&mutation_ref.mutant_ref)?)
    }
}

/// Best-effort comment skip (SPEC.md §10's stated, non-AST-parser limitation, §20 T4): a whole
/// line whose first non-whitespace characters are `//` is skipped entirely.
///
/// `pub(crate)`: shared with `mutant::MutantTester`, which reuses this exact operator-scanning
/// code rather than a second copy of it (SPEC.md §10 Amendment, standalone mutation testing).
pub(crate) fn is_comment_line(line: &str) -> bool {
    line.trim_start().starts_with("//")
}

/// Best-effort string-literal skip: characters between unescaped `"` pairs are blanked out
/// (same byte length preserved, so match offsets still index the real line) before an operator's
/// regex runs — SPEC.md §10, §20 T4. A heuristic, not a parser; documented limitation, not a
/// claim of universal correctness.
fn mask_string_literals(line: &str) -> String {
    let bytes = line.as_bytes();
    let mut out = bytes.to_vec();
    let mut in_string = false;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'"' && (i == 0 || bytes[i - 1] != b'\\') {
            in_string = !in_string;
            i += 1;
            continue;
        }
        if in_string {
            out[i] = b' ';
        }
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| line.to_string())
}

pub(crate) struct ScanMatch {
    pub(crate) start: usize,
    pub(crate) end: usize,
    /// 1-based; the candidate's recorded column.
    pub(crate) column: usize,
    pub(crate) replacement: String,
}

/// Applies `op`'s fixed regex to `original_line`, masked for string literals. One `ScanMatch` per
/// site, in left-to-right order — the concrete mechanism behind SPEC.md §10's five operators.
///
/// `pub(crate)`: shared with `mutant::MutantTester::find_candidates` (SPEC.md §10 Amendment,
/// standalone mutation testing) — the same operator-matching code, not a duplicate.
pub(crate) fn scan_line(op: MutationOperator, original_line: &str) -> Vec<ScanMatch> {
    let masked = mask_string_literals(original_line);
    let re = operator_regex(op);
    let mut out = Vec::new();
    for caps in re.captures_iter(&masked) {
        match op {
            MutationOperator::BooleanFlip => {
                let m = caps.get(1).expect("group 1 always present for BooleanFlip");
                let replacement = if m.as_str() == "true" {
                    "false"
                } else {
                    "true"
                };
                out.push(ScanMatch {
                    start: m.start(),
                    end: m.end(),
                    column: m.start() + 1,
                    replacement: replacement.to_string(),
                });
            }
            MutationOperator::NegateCondition => {
                let whole = caps.get(0).expect("group 0 is always the whole match");
                let cond = caps.get(1).map(|c| c.as_str()).unwrap_or("");
                out.push(ScanMatch {
                    start: whole.start(),
                    end: whole.end(),
                    column: whole.start() + 1,
                    replacement: format!("if (!({cond}))"),
                });
            }
            MutationOperator::OffByOne => {
                let m = caps.get(1).expect("group 1 always present for OffByOne");
                let replacement = match m.as_str() {
                    "<" => "<=",
                    "<=" => "<",
                    ">" => ">=",
                    ">=" => ">",
                    other => other,
                };
                out.push(ScanMatch {
                    start: m.start(),
                    end: m.end(),
                    column: m.start() + 1,
                    replacement: replacement.to_string(),
                });
            }
            MutationOperator::SwapOperator => {
                let m = caps
                    .get(1)
                    .expect("group 1 always present for SwapOperator");
                let replacement = match m.as_str() {
                    "+" => "-",
                    "-" => "+",
                    "*" => "/",
                    "/" => "*",
                    other => other,
                };
                out.push(ScanMatch {
                    start: m.start(),
                    end: m.end(),
                    column: m.start() + 1,
                    replacement: replacement.to_string(),
                });
            }
            MutationOperator::DeleteStatement => {
                let whole = caps.get(0).expect("group 0 is always the whole match");
                out.push(ScanMatch {
                    start: whole.start(),
                    end: whole.end(),
                    column: whole.start() + 1,
                    replacement: String::new(),
                });
            }
        }
    }
    out
}

/// Each operator's fixed regex — bumping `MutationSpec.operator_version` is required whenever
/// one of these changes (SPEC.md §20, R5).
fn operator_regex(op: MutationOperator) -> regex::Regex {
    let pattern = match op {
        MutationOperator::BooleanFlip => r"\b(true|false)\b",
        MutationOperator::NegateCondition => r"if\s*\(([^()\n]*)\)",
        MutationOperator::OffByOne => r"(<=|>=|<|>)",
        MutationOperator::SwapOperator => r"[ \t]([+\-*/])[ \t]",
        MutationOperator::DeleteStatement => r"^\s*\S.*;\s*$",
    };
    regex::Regex::new(pattern).expect("fixed, hand-written operator regex must always compile")
}

/// Re-applies `scan_line` to the exact `(target_line, target_column)` site and splices in its
/// replacement — the same site `find_candidates` discovered, re-derived rather than passed
/// around as a byte range so `apply` has no state beyond `(operator, target_glob, seed,
/// operator_version, base_commit)`.
///
/// `pub(crate)`: shared with `mutant::MutantTester::apply` (SPEC.md §10 Amendment, standalone
/// mutation testing) — the same transform, not a duplicate.
pub(crate) fn mutate_file_contents(
    original: &str,
    op: MutationOperator,
    target_line: u32,
    target_column: u32,
) -> Option<String> {
    let mut lines: Vec<String> = original.split('\n').map(str::to_string).collect();
    let idx = target_line.checked_sub(1)? as usize;
    let line = lines.get(idx)?.clone();
    let m = scan_line(op, &line)
        .into_iter()
        .find(|m| m.column == target_column as usize)?;
    let mut new_line = String::with_capacity(line.len());
    new_line.push_str(&line[..m.start]);
    new_line.push_str(&m.replacement);
    new_line.push_str(&line[m.end..]);
    lines[idx] = new_line;
    Some(lines.join("\n"))
}
