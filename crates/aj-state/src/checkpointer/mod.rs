//! The [`Checkpointer`] contract and its implementations.
//!
//! A checkpointer stores [`Checkpoint`]s in **threads**. A thread is a strictly
//! linear chain: a new checkpoint is accepted only if its parent is the thread's
//! current head and its step is exactly one more. That rule makes concurrent
//! writers safe without locks held across tasks — the loser of a race gets
//! [`StoreError::HeadMismatch`] and must reload — and it means "resume from an
//! earlier point" is never an in-place rewind: it is a [fork](crate::fork) into a
//! new thread, leaving the original history intact.
//!
//! Every implementation applies the same rules through the shared functions in
//! this module, and every implementation is exercised by the same conformance
//! suite in `tests/`.

mod memory;
#[cfg(feature = "sqlite")]
mod sqlite;

use crate::{
    checkpoint::{Checkpoint, CheckpointError, CheckpointId},
    ids::{TaskId, ThreadId},
    schema::SchemaRef,
    update::{BatchError, TaskWrites},
};

pub use memory::InMemoryCheckpointer;
#[cfg(feature = "sqlite")]
pub use sqlite::SqliteCheckpointer;

/// A checkpointer operation failed.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// The checkpoint itself is invalid or fails verification.
    #[error(transparent)]
    Checkpoint(#[from] CheckpointError),
    /// Task writes could not be assembled into a batch.
    #[error(transparent)]
    Batch(#[from] BatchError),
    /// The checkpoint's parent is not the thread's head.
    #[error(
        "thread `{thread}` head is {}, but the checkpoint's parent is {}",
        show(.head),
        show(.expected_parent)
    )]
    HeadMismatch {
        /// The thread written to.
        thread: ThreadId,
        /// The parent the checkpoint names.
        expected_parent: Option<CheckpointId>,
        /// The thread's actual head.
        head: Option<CheckpointId>,
    },
    /// The checkpoint's step is not one past the head's.
    #[error("thread `{thread}` expects step {expected}, found {found}")]
    StepMismatch {
        /// The thread written to.
        thread: ThreadId,
        /// The step the thread requires next.
        expected: u64,
        /// The step the checkpoint carries.
        found: u64,
    },
    /// No such checkpoint in the thread.
    #[error("thread `{thread}` has no checkpoint {checkpoint}")]
    NotFound {
        /// The thread searched.
        thread: ThreadId,
        /// The missing checkpoint.
        checkpoint: CheckpointId,
    },
    /// Task writes may only be recorded against the thread's head checkpoint.
    #[error("checkpoint {checkpoint} is not the head of thread `{thread}`")]
    NotHead {
        /// The thread.
        thread: ThreadId,
        /// The checkpoint the writes were aimed at.
        checkpoint: CheckpointId,
    },
    /// The task is not in the head checkpoint's frontier.
    #[error("task `{0}` is not in the checkpoint's frontier")]
    UnknownTask(TaskId),
    /// The task already has different writes recorded.
    #[error("task `{0}` already has different writes recorded for this checkpoint")]
    TaskWritesConflict(TaskId),
    /// Stored data failed to decode or contradicts its own index.
    #[error("stored data is corrupt: {0}")]
    Corrupt(String),
    /// The storage backend failed.
    #[error("storage failure: {0}")]
    Storage(String),
}

fn show(id: &Option<CheckpointId>) -> String {
    match id {
        Some(id) => id.to_string(),
        None => "empty".to_owned(),
    }
}

/// A one-line description of a stored checkpoint, cheap to list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointSummary {
    /// The checkpoint's id.
    pub id: CheckpointId,
    /// Its parent in the thread.
    pub parent: Option<CheckpointId>,
    /// Its step.
    pub step: u64,
    /// The schema it was written under.
    pub schema: SchemaRef,
    /// How many tasks its frontier holds.
    pub pending_tasks: usize,
}

impl CheckpointSummary {
    pub(crate) fn of(checkpoint: &Checkpoint) -> Self {
        Self {
            id: checkpoint.id(),
            parent: checkpoint.parent(),
            step: checkpoint.step(),
            schema: checkpoint.schema().clone(),
            pending_tasks: checkpoint.frontier().len(),
        }
    }
}

/// Durable (or at least shared) storage for checkpoints.
///
/// The interface is synchronous and implementations are `Send + Sync`. Callers on
/// an async runtime should run calls through its blocking-task facility.
pub trait Checkpointer: Send + Sync {
    /// Appends `checkpoint` to its thread.
    ///
    /// The checkpoint must verify, its parent must be the thread's head (or
    /// `None` for an empty thread) and its step must be one past the head's.
    /// Putting the checkpoint that is already the head succeeds without change,
    /// so retries after an ambiguous failure are safe.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] if the checkpoint fails verification or any of the
    /// rules above, or if storage fails.
    fn put(&self, checkpoint: &Checkpoint) -> Result<(), StoreError>;

    /// Loads one checkpoint, verified, or `None` if the thread has no such id.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] if storage fails or the stored data is corrupt or
    /// fails verification.
    fn get(&self, thread: &ThreadId, id: &CheckpointId) -> Result<Option<Checkpoint>, StoreError>;

    /// Loads the newest checkpoint of the thread, or `None` for an unknown thread.
    ///
    /// # Errors
    ///
    /// As for [`Checkpointer::get`].
    fn head(&self, thread: &ThreadId) -> Result<Option<Checkpoint>, StoreError>;

    /// Lists the thread's checkpoints from oldest to newest; empty for an unknown
    /// thread.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] if storage fails.
    fn history(&self, thread: &ThreadId) -> Result<Vec<CheckpointSummary>, StoreError>;

    /// Lists all thread ids in ascending order.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] if storage fails.
    fn threads(&self) -> Result<Vec<ThreadId>, StoreError>;

    /// Records the complete output of a task that finished while `checkpoint` was
    /// the head, so a crash before the step is committed does not lose it.
    ///
    /// Recording identical writes again succeeds; recording *different* writes for
    /// the same task is [`StoreError::TaskWritesConflict`].
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::NotHead`] if `checkpoint` is not the thread's head,
    /// [`StoreError::UnknownTask`] if the task is not in its frontier,
    /// [`StoreError::TaskWritesConflict`] as above, or a storage error.
    fn put_task_writes(
        &self,
        thread: &ThreadId,
        checkpoint: &CheckpointId,
        writes: &TaskWrites,
    ) -> Result<(), StoreError>;

    /// Loads the task writes recorded against `checkpoint`, ordered by task id.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] if storage fails or stored data is corrupt.
    fn task_writes(
        &self,
        thread: &ThreadId,
        checkpoint: &CheckpointId,
    ) -> Result<Vec<TaskWrites>, StoreError>;

    /// Deletes a thread and everything recorded against it. Returns whether the
    /// thread existed.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] if storage fails.
    fn delete_thread(&self, thread: &ThreadId) -> Result<bool, StoreError>;
}

/// The identity of a thread's newest checkpoint, as far as `put` needs to know.
#[derive(Debug, Clone, Copy)]
pub(crate) struct HeadInfo {
    pub(crate) id: CheckpointId,
    pub(crate) step: u64,
}

/// What `put` should do once the rules have passed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PutDecision {
    Insert,
    AlreadyStored,
}

/// The rules every checkpointer applies to a `put`.
pub(crate) fn check_put(
    head: Option<&HeadInfo>,
    checkpoint: &Checkpoint,
) -> Result<PutDecision, StoreError> {
    checkpoint.verify()?;
    let Some(head) = head else {
        return if checkpoint.parent().is_some() {
            Err(StoreError::HeadMismatch {
                thread: checkpoint.thread().clone(),
                expected_parent: checkpoint.parent(),
                head: None,
            })
        } else {
            Ok(PutDecision::Insert)
        };
    };
    if head.id == checkpoint.id() {
        return Ok(PutDecision::AlreadyStored);
    }
    if checkpoint.parent() != Some(head.id) {
        return Err(StoreError::HeadMismatch {
            thread: checkpoint.thread().clone(),
            expected_parent: checkpoint.parent(),
            head: Some(head.id),
        });
    }
    let expected = head
        .step
        .checked_add(1)
        .ok_or(StoreError::Checkpoint(CheckpointError::StepOverflow))?;
    if checkpoint.step() == expected {
        Ok(PutDecision::Insert)
    } else {
        Err(StoreError::StepMismatch {
            thread: checkpoint.thread().clone(),
            expected,
            found: checkpoint.step(),
        })
    }
}

/// The rules every checkpointer applies before accepting task writes.
pub(crate) fn check_task_writes_target(
    head: Option<&Checkpoint>,
    thread: &ThreadId,
    checkpoint: &CheckpointId,
    task: &TaskId,
) -> Result<(), StoreError> {
    let Some(head) = head else {
        return Err(StoreError::NotFound {
            thread: thread.clone(),
            checkpoint: *checkpoint,
        });
    };
    if head.id() != *checkpoint {
        return Err(StoreError::NotHead {
            thread: thread.clone(),
            checkpoint: *checkpoint,
        });
    }
    if head.frontier().contains(task) {
        Ok(())
    } else {
        Err(StoreError::UnknownTask(task.clone()))
    }
}
