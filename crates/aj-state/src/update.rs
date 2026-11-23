//! Updates, writes and deterministic update batches.
//!
//! In a superstep several tasks may produce updates at the same time. Their
//! completion order is a scheduling accident and must never influence state. An
//! [`UpdateBatch`] therefore orders every [`Write`] by `(writer, sequence)` —
//! facts about *who wrote what*, not *who finished first* — and rejects duplicate
//! positions. Applying the same set of writes in any arrival order yields the
//! same batch, the same state and the same digest.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    digest::{Digest, DigestBuilder},
    ids::{ChannelName, NameError, TaskId, WriterId},
};

/// One value destined for one channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Update {
    channel: ChannelName,
    value: Value,
}

impl Update {
    /// Creates an update for `channel`.
    #[must_use]
    pub fn new(channel: ChannelName, value: Value) -> Self {
        Self { channel, value }
    }

    /// Creates an update, validating the channel name.
    ///
    /// # Errors
    ///
    /// Returns [`NameError`] if `channel` is not a valid channel name.
    pub fn named(channel: &str, value: Value) -> Result<Self, NameError> {
        Ok(Self::new(ChannelName::new(channel)?, value))
    }

    /// The target channel.
    #[must_use]
    pub fn channel(&self) -> &ChannelName {
        &self.channel
    }

    /// The carried value.
    #[must_use]
    pub fn value(&self) -> &Value {
        &self.value
    }
}

/// An update attributed to a writer at a position in that writer's output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Write {
    writer: WriterId,
    sequence: u32,
    update: Update,
}

impl Write {
    /// Creates a write.
    #[must_use]
    pub fn new(writer: WriterId, sequence: u32, update: Update) -> Self {
        Self {
            writer,
            sequence,
            update,
        }
    }

    /// Who produced the update.
    #[must_use]
    pub fn writer(&self) -> &WriterId {
        &self.writer
    }

    /// The update's position within its writer's output.
    #[must_use]
    pub fn sequence(&self) -> u32 {
        self.sequence
    }

    /// The update itself.
    #[must_use]
    pub fn update(&self) -> &Update {
        &self.update
    }
}

/// Why a batch could not be assembled.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BatchError {
    /// Two writes claim the same `(writer, sequence)` position.
    #[error("writer `{writer}` has two writes at sequence {sequence}")]
    DuplicateWrite {
        /// The writer.
        writer: WriterId,
        /// The contested position.
        sequence: u32,
    },
    /// The same writer was supplied twice in [`UpdateBatch::from_writer_outputs`].
    #[error("writer `{0}` was supplied more than once")]
    DuplicateWriter(WriterId),
    /// A single writer produced more updates than a sequence number can address.
    #[error("writer `{0}` produced more than u32::MAX updates")]
    TooManyUpdates(WriterId),
}

/// All writes of one superstep, in deterministic order.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(try_from = "Vec<Write>", into = "Vec<Write>")]
pub struct UpdateBatch {
    writes: Vec<Write>,
}

impl UpdateBatch {
    /// A batch with no writes.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Orders `writes` by `(writer, sequence)` and rejects duplicate positions.
    ///
    /// # Errors
    ///
    /// Returns [`BatchError::DuplicateWrite`] if two writes share a position.
    pub fn new(mut writes: Vec<Write>) -> Result<Self, BatchError> {
        writes.sort_by(|left, right| {
            (&left.writer, left.sequence).cmp(&(&right.writer, right.sequence))
        });
        for pair in writes.windows(2) {
            if let [first, second] = pair
                && first.writer == second.writer
                && first.sequence == second.sequence
            {
                return Err(BatchError::DuplicateWrite {
                    writer: first.writer.clone(),
                    sequence: first.sequence,
                });
            }
        }
        Ok(Self { writes })
    }

    /// Builds a batch from each writer's ordered output. A writer's updates take
    /// sequence numbers `0, 1, 2, …` in the order given; the order in which
    /// writers are supplied is irrelevant.
    ///
    /// # Errors
    ///
    /// Returns [`BatchError::DuplicateWriter`] if a writer appears twice, or
    /// [`BatchError::TooManyUpdates`] if one writer's output exceeds `u32::MAX`.
    pub fn from_writer_outputs(
        outputs: impl IntoIterator<Item = (WriterId, Vec<Update>)>,
    ) -> Result<Self, BatchError> {
        let mut seen = BTreeSet::new();
        let mut writes = Vec::new();
        for (writer, updates) in outputs {
            if !seen.insert(writer.clone()) {
                return Err(BatchError::DuplicateWriter(writer));
            }
            for (index, update) in updates.into_iter().enumerate() {
                let sequence =
                    u32::try_from(index).map_err(|_| BatchError::TooManyUpdates(writer.clone()))?;
                writes.push(Write::new(writer.clone(), sequence, update));
            }
        }
        Self::new(writes)
    }

    /// Builds the batch formed by completed tasks' writes, attributing each task's
    /// updates to the task id.
    ///
    /// # Errors
    ///
    /// Same as [`UpdateBatch::from_writer_outputs`].
    pub fn from_task_writes(tasks: &[TaskWrites]) -> Result<Self, BatchError> {
        Self::from_writer_outputs(
            tasks
                .iter()
                .map(|task| (WriterId::from(&task.task), task.updates.clone())),
        )
    }

    /// The writes in application order.
    #[must_use]
    pub fn writes(&self) -> &[Write] {
        &self.writes
    }

    /// Number of writes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.writes.len()
    }

    /// Whether the batch has no writes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.writes.is_empty()
    }

    /// A digest over every write in order.
    #[must_use]
    pub fn digest(&self) -> Digest {
        let mut builder = DigestBuilder::new("ajentir.batch.v1");
        builder.u64(u64::try_from(self.writes.len()).unwrap_or(u64::MAX));
        for write in &self.writes {
            builder
                .str(write.writer.as_str())
                .u64(u64::from(write.sequence))
                .str(write.update.channel.as_str())
                .value(&write.update.value);
        }
        builder.finish()
    }
}

impl TryFrom<Vec<Write>> for UpdateBatch {
    type Error = BatchError;

    fn try_from(writes: Vec<Write>) -> Result<Self, BatchError> {
        Self::new(writes)
    }
}

impl From<UpdateBatch> for Vec<Write> {
    fn from(batch: UpdateBatch) -> Self {
        batch.writes
    }
}

/// The complete output of one finished task: what it wrote before the step ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskWrites {
    task: TaskId,
    updates: Vec<Update>,
}

impl TaskWrites {
    /// Records that `task` finished with `updates`.
    #[must_use]
    pub fn new(task: TaskId, updates: Vec<Update>) -> Self {
        Self { task, updates }
    }

    /// The finished task.
    #[must_use]
    pub fn task(&self) -> &TaskId {
        &self.task
    }

    /// What it wrote, in order.
    #[must_use]
    pub fn updates(&self) -> &[Update] {
        &self.updates
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn writer(name: &str) -> WriterId {
        WriterId::new(name).unwrap()
    }

    fn update(channel: &str, value: Value) -> Update {
        Update::named(channel, value).unwrap()
    }

    #[test]
    fn writes_are_ordered_by_writer_then_sequence() {
        let batch = UpdateBatch::new(vec![
            Write::new(writer("b"), 0, update("c", json!(1))),
            Write::new(writer("a"), 1, update("c", json!(2))),
            Write::new(writer("a"), 0, update("c", json!(3))),
        ])
        .unwrap();
        let order: Vec<(&str, u32)> = batch
            .writes()
            .iter()
            .map(|w| (w.writer().as_str(), w.sequence()))
            .collect();
        assert_eq!(order, vec![("a", 0), ("a", 1), ("b", 0)]);
    }

    #[test]
    fn supply_order_does_not_change_the_batch_or_its_digest() {
        let one = UpdateBatch::from_writer_outputs([
            (
                writer("x"),
                vec![update("c", json!(1)), update("d", json!(2))],
            ),
            (writer("y"), vec![update("c", json!(3))]),
        ])
        .unwrap();
        let two = UpdateBatch::from_writer_outputs([
            (writer("y"), vec![update("c", json!(3))]),
            (
                writer("x"),
                vec![update("c", json!(1)), update("d", json!(2))],
            ),
        ])
        .unwrap();
        assert_eq!(one, two);
        assert_eq!(one.digest(), two.digest());
    }

    #[test]
    fn digest_depends_on_content() {
        let one =
            UpdateBatch::from_writer_outputs([(writer("x"), vec![update("c", json!(1))])]).unwrap();
        let two =
            UpdateBatch::from_writer_outputs([(writer("x"), vec![update("c", json!(2))])]).unwrap();
        assert_ne!(one.digest(), two.digest());
        assert_ne!(one.digest(), UpdateBatch::empty().digest());
    }

    #[test]
    fn duplicate_positions_are_rejected() {
        let err = UpdateBatch::new(vec![
            Write::new(writer("a"), 0, update("c", json!(1))),
            Write::new(writer("a"), 0, update("c", json!(2))),
        ])
        .unwrap_err();
        assert_eq!(
            err,
            BatchError::DuplicateWrite {
                writer: writer("a"),
                sequence: 0
            }
        );
        let err = UpdateBatch::from_writer_outputs([
            (writer("a"), vec![update("c", json!(1))]),
            (writer("a"), vec![update("c", json!(2))]),
        ])
        .unwrap_err();
        assert_eq!(err, BatchError::DuplicateWriter(writer("a")));
    }

    #[test]
    fn task_writes_are_attributed_to_the_task() {
        let task = TaskId::new("task-1").unwrap();
        let batch =
            UpdateBatch::from_task_writes(&[TaskWrites::new(task, vec![update("c", json!(1))])])
                .unwrap();
        assert_eq!(batch.writes().first().unwrap().writer().as_str(), "task-1");
        assert_eq!(batch.len(), 1);
        assert!(!batch.is_empty());
    }

    #[test]
    fn serde_revalidates_on_load() {
        let batch =
            UpdateBatch::from_writer_outputs([(writer("a"), vec![update("c", json!(1))])]).unwrap();
        let json = serde_json::to_string(&batch).unwrap();
        assert_eq!(serde_json::from_str::<UpdateBatch>(&json).unwrap(), batch);

        let duplicated = r#"[
            {"writer":"a","sequence":0,"update":{"channel":"c","value":1}},
            {"writer":"a","sequence":0,"update":{"channel":"c","value":2}}
        ]"#;
        assert!(serde_json::from_str::<UpdateBatch>(duplicated).is_err());
    }
}
