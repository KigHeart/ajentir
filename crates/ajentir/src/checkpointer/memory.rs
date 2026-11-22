//! An in-process checkpointer. **Not durable**: everything is lost when the value
//! is dropped. Use it for tests, ephemeral runs and as the reference behaviour the
//! durable backends must match.

use std::{
    collections::BTreeMap,
    sync::{Mutex, MutexGuard},
};

use super::{
    CheckpointSummary, Checkpointer, HeadInfo, PutDecision, StoreError, check_put,
    check_task_writes_target,
};
use crate::{
    checkpoint::{Checkpoint, CheckpointId},
    ids::{TaskId, ThreadId},
    update::TaskWrites,
};

#[derive(Debug, Default)]
struct ThreadLog {
    chain: Vec<Checkpoint>,
    writes: BTreeMap<CheckpointId, BTreeMap<TaskId, TaskWrites>>,
}

/// Keeps every thread in memory behind a mutex.
#[derive(Debug, Default)]
pub struct InMemoryCheckpointer {
    threads: Mutex<BTreeMap<ThreadId, ThreadLog>>,
}

impl InMemoryCheckpointer {
    /// Creates an empty checkpointer.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> Result<MutexGuard<'_, BTreeMap<ThreadId, ThreadLog>>, StoreError> {
        self.threads
            .lock()
            .map_err(|_| StoreError::Storage("in-memory checkpointer lock poisoned".to_owned()))
    }
}

impl Checkpointer for InMemoryCheckpointer {
    fn put(&self, checkpoint: &Checkpoint) -> Result<(), StoreError> {
        let mut threads = self.lock()?;
        let head = threads
            .get(checkpoint.thread())
            .and_then(|log| log.chain.last())
            .map(|newest| HeadInfo {
                id: newest.id(),
                step: newest.step(),
            });
        match check_put(head.as_ref(), checkpoint)? {
            PutDecision::AlreadyStored => Ok(()),
            PutDecision::Insert => {
                threads
                    .entry(checkpoint.thread().clone())
                    .or_default()
                    .chain
                    .push(checkpoint.clone());
                Ok(())
            }
        }
    }

    fn get(&self, thread: &ThreadId, id: &CheckpointId) -> Result<Option<Checkpoint>, StoreError> {
        let threads = self.lock()?;
        Ok(threads
            .get(thread)
            .and_then(|log| log.chain.iter().find(|stored| stored.id() == *id))
            .cloned())
    }

    fn head(&self, thread: &ThreadId) -> Result<Option<Checkpoint>, StoreError> {
        let threads = self.lock()?;
        Ok(threads
            .get(thread)
            .and_then(|log| log.chain.last())
            .cloned())
    }

    fn history(&self, thread: &ThreadId) -> Result<Vec<CheckpointSummary>, StoreError> {
        let threads = self.lock()?;
        Ok(threads
            .get(thread)
            .map(|log| log.chain.iter().map(CheckpointSummary::of).collect())
            .unwrap_or_default())
    }

    fn threads(&self) -> Result<Vec<ThreadId>, StoreError> {
        Ok(self.lock()?.keys().cloned().collect())
    }

    fn put_task_writes(
        &self,
        thread: &ThreadId,
        checkpoint: &CheckpointId,
        writes: &TaskWrites,
    ) -> Result<(), StoreError> {
        let mut threads = self.lock()?;
        check_task_writes_target(
            threads.get(thread).and_then(|log| log.chain.last()),
            thread,
            checkpoint,
            writes.task(),
        )?;
        let recorded = threads
            .get_mut(thread)
            .ok_or_else(|| StoreError::NotFound {
                thread: thread.clone(),
                checkpoint: *checkpoint,
            })?
            .writes
            .entry(*checkpoint)
            .or_default();
        match recorded.get(writes.task()) {
            Some(existing) if existing == writes => Ok(()),
            Some(_) => Err(StoreError::TaskWritesConflict(writes.task().clone())),
            None => {
                recorded.insert(writes.task().clone(), writes.clone());
                Ok(())
            }
        }
    }

    fn task_writes(
        &self,
        thread: &ThreadId,
        checkpoint: &CheckpointId,
    ) -> Result<Vec<TaskWrites>, StoreError> {
        let threads = self.lock()?;
        Ok(threads
            .get(thread)
            .and_then(|log| log.writes.get(checkpoint))
            .map(|recorded| recorded.values().cloned().collect())
            .unwrap_or_default())
    }

    fn delete_thread(&self, thread: &ThreadId) -> Result<bool, StoreError> {
        Ok(self.lock()?.remove(thread).is_some())
    }
}
