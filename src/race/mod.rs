//! Composition on top of `experiment` — SPEC.md §12, docs/ARCHITECTURE.md §10.
//!
//! No separate execution path: `race` cannot drift from what a bare `run` does, because it
//! calls `ExperimentRunner::run` once per participant.

use std::panic::{self, AssertUnwindSafe};
use std::sync::Arc;

use thiserror::Error;

use crate::adapter::AgentAdapter;
use crate::domain::{AgentConfig, PermissionPolicy, RaceParticipant, RaceRecord, TaskSpec};
use crate::experiment::ExperimentRunner;
use crate::store::Store;

#[derive(Debug, Error)]
pub enum Error {
    #[error("experiment error: {0}")]
    Experiment(#[from] crate::experiment::Error),
    #[error("store error: {0}")]
    Store(#[from] crate::store::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

pub struct RaceRunner {
    runner: Arc<ExperimentRunner>,
    store: Arc<Store>,
}

/// A single, ready-to-run slot in the expanded participant list.
struct Participant {
    race_index: u32,
    agent_config: AgentConfig,
    agent: Arc<dyn AgentAdapter>,
}

impl RaceRunner {
    pub fn new(runner: Arc<ExperimentRunner>, store: Arc<Store>) -> Self {
        RaceRunner { runner, store }
    }

    /// Expands `agents` (listed order) × `repeat` into a `race_index`-ordered participant list
    /// before any execution starts — SPEC.md §20 (R2/T1) — then runs each bounded by
    /// `max_parallel`. A participant's internal failure doesn't abort the others — SPEC.md §20
    /// (F3): `ExperimentRunner::run` never returns `Err` for that case, only a `Failed`-status
    /// record, so every participant that gets as far as having an experiment id still lands in
    /// the race's own participant list.
    ///
    /// Takes already-resolved adapters, one per agent config, rather than resolving them
    /// internally by name — this mirrors `ExperimentRunner::run`'s own `agent`/`agent_config`
    /// split and is what lets tests drive a race with `FakeAdapter` instances instead of the
    /// real Claude Code adapter. Name-based resolution (`adapter::resolve`) is `cli`'s job, not
    /// this module's.
    ///
    /// No race-specific scoring or ranking logic lives here: every participant is scored the
    /// exact same way `run` scores it (`ExperimentRunner::run` → `scoring::score`), and the
    /// leaderboard is computed live, later, by `report::Reporter` from each participant's own
    /// persisted `score.json` — SPEC.md §20 (D3). This module's only job is fan-out and
    /// bookkeeping.
    pub fn run_race(
        &self,
        task: &TaskSpec,
        agents: &[(AgentConfig, Arc<dyn AgentAdapter>)],
        repeat: u32,
        max_parallel: u32,
    ) -> Result<RaceRecord> {
        let race_id = crate::domain::ids::new_id(chrono::Utc::now());
        let policy = default_policy();

        // Outer loop over the listed agents, inner loop over repeat — SPEC.md §12 — computed in
        // full before any worktree is created or any process spawned.
        let mut participants = Vec::with_capacity(agents.len() * repeat.max(1) as usize);
        let mut race_index = 0u32;
        for (agent_config, agent) in agents {
            for _ in 0..repeat {
                participants.push(Participant {
                    race_index,
                    agent_config: agent_config.clone(),
                    agent: agent.clone(),
                });
                race_index += 1;
            }
        }

        // Bounded, chunked fan-out: never more than `max_parallel` `ExperimentRunner::run` calls
        // in flight at once. Worktree creation/removal is independently serialized inside
        // `run` itself (SPEC.md §7), so this bound only limits concurrent execution, not safety.
        let chunk_size = max_parallel.max(1) as usize;
        let mut outcomes: Vec<(
            u32,
            crate::experiment::Result<crate::domain::ExperimentRecord>,
        )> = Vec::with_capacity(participants.len());
        for chunk in participants.chunks(chunk_size) {
            std::thread::scope(|scope| {
                let handles: Vec<_> = chunk
                    .iter()
                    .map(|p| {
                        let runner = self.runner.clone();
                        let policy = &policy;
                        scope.spawn(move || {
                            // `ExperimentRunner::run` never panics under normal operation — every
                            // internal failure it can hit collapses to a `Failed`-status `Ok`
                            // record (SPEC.md §12 F3) — but this still guards against one doing
                            // so anyway (e.g. a future bug on a path that processes repo/agent-
                            // controlled data): without `catch_unwind` here, a single panicking
                            // participant would unwind straight past `std::thread::scope` and
                            // lose every already-collected result for the *whole* race, not just
                            // fail that one participant.
                            let result = panic::catch_unwind(AssertUnwindSafe(|| {
                                runner.run(task, p.agent.as_ref(), &p.agent_config, policy)
                            }))
                            .unwrap_or_else(|payload| {
                                Err(crate::experiment::Error::Panicked(panic_message(
                                    payload.as_ref(),
                                )))
                            });
                            (p.race_index, result)
                        })
                    })
                    .collect();
                for handle in handles {
                    outcomes.push(
                        handle
                            .join()
                            .expect("race participant thread itself panicked while joining"),
                    );
                }
            });
        }
        outcomes.sort_by_key(|(index, _)| *index);

        let mut race_participants = Vec::with_capacity(outcomes.len());
        for (index, outcome) in outcomes {
            // A participant that never got as far as producing an `ExperimentRecord` at all
            // (e.g. worktree creation itself failed) has nothing to stamp or list — this is
            // distinct from SPEC.md §12 (F3)'s "ExperimentRecord ends Failed" case, which always
            // does get a participant entry (see `run`'s own doc comment).
            let Ok(mut record) = outcome else {
                continue;
            };
            record.race_id = Some(race_id.clone());
            record.race_index = Some(index);
            self.store.save_experiment(&record)?;
            race_participants.push((record.id, RaceParticipant { race_index: index }));
        }

        let record = RaceRecord {
            id: race_id,
            task_id: task.id.clone(),
            max_parallel,
            participants: race_participants,
        };
        self.store.save_race(&record)?;
        Ok(record)
    }
}

/// Best-effort extraction of a human-readable message from a caught panic payload — covers the
/// two shapes `panic!`/`.expect()`/`.unwrap()` actually produce (`&'static str` for a literal,
/// `String` for a formatted one); anything else (a custom `panic_any` payload) falls back to a
/// fixed message rather than failing to construct the `Failed`-equivalent error at all.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "non-string panic payload".to_string()
    }
}

/// The `PermissionPolicy` every race participant runs under. `race`'s own CLI row (SPEC.md §6)
/// takes no `--policy` flag (unlike `run`), so every participant runs under the same generous,
/// uniform, built-in default `run`'s own `--policy`-omitted fallback uses —
/// `PermissionPolicy::generous_default`.
fn default_policy() -> PermissionPolicy {
    PermissionPolicy::generous_default("race-default")
}
