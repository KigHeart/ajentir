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
//! Deterministic BSP commit and recovery boundary.
//!
//! The kernel does not know about model providers, tools, async executors or graph
//! routing. A scheduler supplies completed task writes and the next frontier. The
//! kernel verifies exact completion coverage, assembles a deterministic batch,
//! and creates the next immutable checkpoint. Recovery planning reconciles a
//! checkpoint's frozen frontier with task outputs already persisted by a
//! Checkpointer. Failure never mutates the input checkpoint.

use std::collections::{BTreeMap, BTreeSet};

use aj_state::{
    BatchError, Checkpoint, CheckpointError, Checkpointer, Digest, Frontier, FrontierTask, Schema,
    TaskId, TaskWrites, UpdateBatch,
};
use serde_json::Value;
use thiserror::Error;

/// Why a super-step could not be committed or recovered.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum KernelError {
    /// A task produced more than one completion result.
    #[error("task {0} produced more than one completion result")]
    DuplicateResult(TaskId),
    /// A stored task output appeared more than once during recovery.
    #[error("recovery store returned duplicate output for task {0}")]
    DuplicateStoredResult(TaskId),
    /// A stored task output does not belong to the checkpoint's frozen frontier.
    #[error("recovery store returned output for task {0}, which is not in the frontier")]
    UnexpectedStoredResult(TaskId),
    /// The completion set does not exactly cover the frozen frontier.
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
    /// Persisted recovery data could not be loaded.
    #[error("could not load persisted recovery data: {0}")]
    Store(String),
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

/// A restart plan reconstructed from one checkpoint and its persisted task outputs.
///
/// A task is considered complete only when its full TaskWrites value has been
/// durably recorded by the checkpointer. Tasks in pending_tasks must be executed
/// again. Persisting a task result is idempotent for identical output and rejects
/// conflicting output, but this does not make external side effects exactly-once:
/// an effect performed before its result is recorded may be repeated after a crash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryPlan {
    completed_writes: Vec<TaskWrites>,
    pending_tasks: Vec<FrontierTask>,
}

impl RecoveryPlan {
    /// Task outputs already persisted for this checkpoint, ordered by task id.
    #[must_use]
    pub fn completed_writes(&self) -> &[TaskWrites] {
        &self.completed_writes
    }

    /// Frontier tasks for which no durable output exists, ordered by task id.
    #[must_use]
    pub fn pending_tasks(&self) -> &[FrontierTask] {
        &self.pending_tasks
    }

    /// Whether every task in the frozen frontier has a durable output.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.pending_tasks.is_empty()
    }
}

/// Reconciles a checkpoint's frozen frontier with outputs already persisted by
/// the checkpointer.
///
/// This is the restart boundary: completed tasks are reused, and only tasks with
/// no durable output are returned for execution. The checkpointer verifies that
/// each recorded output belongs to the current thread head and frontier when it
/// is written; this function additionally rejects unexpected or duplicate values
/// from a storage implementation.
///
/// # Errors
///
/// Returns a storage error if outputs cannot be loaded, or a kernel error if the
/// stored outputs contain duplicate or unexpected task identities.
pub fn plan_recovery(
    checkpointer: &impl Checkpointer,
    checkpoint: &Checkpoint,
) -> Result<RecoveryPlan, KernelError> {
    let stored = checkpointer
        .task_writes(checkpoint.thread(), &checkpoint.id())
        .map_err(|error| KernelError::Store(error.to_string()))?;
    let mut completed = BTreeMap::new();

    for writes in stored {
        let task = writes.task().clone();
        if !checkpoint.frontier().contains(&task) {
            return Err(KernelError::UnexpectedStoredResult(task));
        }
        if completed.insert(task.clone(), writes).is_some() {
            return Err(KernelError::DuplicateStoredResult(task));
        }
    }

    let mut completed_writes = Vec::with_capacity(completed.len());
    let mut pending_tasks = Vec::new();

    for task in checkpoint.frontier().tasks() {
        match completed.remove(task.id()) {
            Some(writes) => completed_writes.push(writes),
            None => pending_tasks.push(task.clone()),
        }
    }

    Ok(RecoveryPlan {
        completed_writes,
        pending_tasks,
    })
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
    use aj_state::{
        Checkpointer, InMemoryCheckpointer, NodeName, ReducerKind, ThreadId, Update, ValueType,
    };
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

    fn result_for(task: &FrontierTask) -> TaskWrites {
        TaskWrites::new(
            task.id().clone(),
            vec![Update::named("total", task.input()["n"].clone()).unwrap()],
        )
    }

    fn results(checkpoint: &Checkpoint) -> Vec<TaskWrites> {
        checkpoint
            .frontier()
            .tasks()
            .iter()
            .map(result_for)
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
    fn recovery_reuses_persisted_outputs_and_returns_only_missing_tasks() {
        let checkpoint = initial();
        let store = InMemoryCheckpointer::new();
        store.put(&checkpoint).unwrap();

        let first_task = checkpoint.frontier().tasks().first().unwrap();
        let persisted = result_for(first_task);
        store
            .put_task_writes(checkpoint.thread(), &checkpoint.id(), &persisted)
            .unwrap();

        let plan = plan_recovery(&store, &checkpoint).unwrap();
        assert_eq!(plan.completed_writes(), &[persisted]);
        assert_eq!(plan.pending_tasks().len(), 1);
        assert!(!plan.is_complete());
        assert_ne!(plan.pending_tasks().first().unwrap().id(), first_task.id());
    }

    #[test]
    fn fully_recovered_frontier_can_be_committed_without_rerunning_tasks() {
        let checkpoint = initial();
        let store = InMemoryCheckpointer::new();
        store.put(&checkpoint).unwrap();

        for writes in results(&checkpoint) {
            store
                .put_task_writes(checkpoint.thread(), &checkpoint.id(), &writes)
                .unwrap();
        }

        let plan = plan_recovery(&store, &checkpoint).unwrap();
        assert!(plan.is_complete());
        assert!(plan.pending_tasks().is_empty());

        let commit = commit_superstep(
            &checkpoint,
            &schema(),
            plan.completed_writes(),
            Frontier::empty(),
            Default::default(),
        )
        .unwrap();
        assert_eq!(commit.checkpoint().state().value("total"), Some(&json!(3)));
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
    #[test]
    fn sqlite_restart_recovers_completed_task_outputs() {
        use aj_state::SqliteCheckpointer;
        use assert_fs::TempDir;

        let directory = TempDir::new().unwrap();
        let database = directory.path().join("ajentir-recovery.sqlite");
        let checkpoint = initial();

        {
            let store = SqliteCheckpointer::open(&database).unwrap();
            store.put(&checkpoint).unwrap();
            let task = checkpoint.frontier().tasks().first().unwrap();
            store
                .put_task_writes(checkpoint.thread(), &checkpoint.id(), &result_for(task))
                .unwrap();
        }

        let reopened = SqliteCheckpointer::open(&database).unwrap();
        let plan = plan_recovery(&reopened, &checkpoint).unwrap();

        assert_eq!(plan.completed_writes().len(), 1);
        assert_eq!(plan.pending_tasks().len(), 1);
        assert!(!plan.is_complete());
    }
}