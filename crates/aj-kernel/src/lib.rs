#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic
    )
)]
//! Deterministic bulk-synchronous-parallel super-step commit boundary.
//!
//! The kernel does not know about model providers, tools, async executors or graph
//! routing. A scheduler supplies completed task writes and the next frontier.
//! The kernel verifies exact completion coverage, assembles a deterministic batch,
//! and creates the next immutable checkpoint. Failure never mutates the input.

use std::collections::BTreeSet;

use aj_state::{
    BatchError, Checkpoint, CheckpointError, Digest, Frontier, Schema, TaskId, TaskWrites,
    UpdateBatch,
};
use serde_json::Value;
use thiserror::Error;

/// Why a super-step could not be committed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum KernelError {
    /// A task produced more than one completion result.
    #[error("task {0} produced more than one completion result")]
    DuplicateResult(TaskId),
    /// The completion set does not exactly cover the checkpoint's frozen frontier.
    #[error(
        "completion set does not match frozen frontier: expected {expected}, received {received}"
    )]
    FrontierMismatch { expected: usize, received: usize },
    /// Task outputs could not be assembled into a deterministic batch.
    #[error(transparent)]
    Batch(#[from] BatchError),
    /// The checkpoint could not be advanced under the supplied schema.
    #[error(transparent)]
    Checkpoint(#[from] CheckpointError),
}

/// The durable result of one successfully committed super-step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuperStepCommit {
    checkpoint: Checkpoint,
    batch_digest: Digest,
}

impl SuperStepCommit {
    /// The immutable checkpoint produced by this super-step.
    #[must_use]
    pub fn checkpoint(&self) -> &Checkpoint {
        &self.checkpoint
    }

    /// The deterministic digest of all task writes committed in this step.
    #[must_use]
    pub fn batch_digest(&self) -> &Digest {
        &self.batch_digest
    }
}

/// Validates and commits one complete BSP super-step.
///
/// Results may be supplied in any completion order. The resulting batch is
/// ordered by stable task identity, not completion time. The next frontier is
/// supplied by the scheduler/planner and stored with the newly reduced state.
///
/// # Errors
///
/// Rejects duplicate, missing or unexpected task results and propagates invalid
/// state updates or checkpoint/schema errors.
pub fn commit_superstep(
    checkpoint: &Checkpoint,
    schema: &Schema,
    completed: &[TaskWrites],
    next_frontier: Frontier,
    metadata: std::collections::BTreeMap<String, Value>,
) -> Result<SuperStepCommit, KernelError> {
    let expected: BTreeSet<TaskId> = checkpoint
        .frontier()
        .tasks()
        .iter()
        .map(|task| task.id().clone())
        .collect();

    let mut received = BTreeSet::new();
    for result in completed {
        if !received.insert(result.task().clone()) {
            return Err(KernelError::DuplicateResult(result.task().clone()));
        }
    }

    if expected != received {
        return Err(KernelError::FrontierMismatch {
            expected: expected.len(),
            received: received.len(),
        });
    }

    let batch = UpdateBatch::from_task_writes(completed)?;
    let batch_digest = batch.digest();
    let next = checkpoint.advance(schema, &batch, next_frontier, metadata)?;

    Ok(SuperStepCommit {
        checkpoint: next,
        batch_digest,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use aj_state::{NodeName, ReducerKind, ThreadId, Update, ValueType};
    use serde_json::json;

    fn schema() -> Schema {
        Schema::builder("kernel-test", 1)
            .channel("total", ValueType::Integer, ReducerKind::SumI64)
            .build()
            .unwrap()
    }

    fn initial() -> Checkpoint {
        let tasks = Frontier::plan(
            0,
            vec![
                (NodeName::new("worker").unwrap(), json!({"n": 1})),
                (NodeName::new("worker").unwrap(), json!({"n": 2})),
            ],
        );
        Checkpoint::initial(
            ThreadId::new("thread-1").unwrap(),
            &schema(),
            &UpdateBatch::empty(),
            tasks,
            Default::default(),
        )
        .unwrap()
    }

    fn results(checkpoint: &Checkpoint) -> Vec<TaskWrites> {
        checkpoint
            .frontier()
            .tasks()
            .iter()
            .map(|task| {
                TaskWrites::new(
                    task.id().clone(),
                    vec![Update::named("total", task.input()["n"].clone()).unwrap()],
                )
            })
            .collect()
    }

    #[test]
    fn completion_order_does_not_change_checkpoint() {
        let first = initial();
        let forward = results(&first);
        let mut reverse = forward.clone();
        reverse.reverse();

        let a = commit_superstep(
            &first,
            &schema(),
            &forward,
            Frontier::empty(),
            Default::default(),
        )
        .unwrap();
        let b = commit_superstep(
            &first,
            &schema(),
            &reverse,
            Frontier::empty(),
            Default::default(),
        )
        .unwrap();

        assert_eq!(a.checkpoint().id(), b.checkpoint().id());
        assert_eq!(a.batch_digest(), b.batch_digest());
        assert_eq!(a.checkpoint().state().value("total"), Some(&json!(3)));
    }

    #[test]
    fn incomplete_frontier_is_rejected_without_changing_parent() {
        let first = initial();
        let mut completed = results(&first);
        let _removed = completed.pop();
        let before = first.clone();

        let result = commit_superstep(
            &first,
            &schema(),
            &completed,
            Frontier::empty(),
            Default::default(),
        );

        assert!(matches!(result, Err(KernelError::FrontierMismatch { .. })));
        assert_eq!(first, before);
    }

    #[test]
    fn duplicate_task_result_is_rejected() {
        let first = initial();
        let mut completed = results(&first);
        completed.push(completed[0].clone());

        assert!(matches!(
            commit_superstep(
                &first,
                &schema(),
                &completed,
                Frontier::empty(),
                Default::default()
            ),
            Err(KernelError::DuplicateResult(_))
        ));
    }
}
