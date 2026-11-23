//! The recovery journal: finished model responses and tool results.
//!
//! Between two checkpoints a run does expensive, externally visible work — a model
//! call that is billed, tool calls with side effects. As each piece finishes it is
//! written to the store as the *task write* of the matching frontier task, under
//! [`JOURNAL_CHANNEL`]. If the process dies before the step is committed, the next
//! `resume` reads the journal back and skips everything already finished.
//!
//! A model task holds exactly one entry (the [`ModelTurn`]); each tool task holds
//! exactly one entry (the tool's [`UserContent`] result). Entries are immutable:
//! writing the same value again is a no-op, writing a different one is a conflict.

use std::collections::BTreeMap;

use ajentir::{Checkpoint, Checkpointer, Frontier, FrontierTask, StoreError, TaskWrites, Update};
use rig_core::message::UserContent;
use rig_run::ModelTurn;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;

use crate::{
    error::DurableRunError,
    schema::{JOURNAL_CHANNEL, MODEL_NODE, TOOL_NODE},
};

/// Journaled work of the current step, as read back from the store.
#[derive(Debug, Default)]
pub(crate) struct Journal {
    /// The model's response, if the current step is a model call that received one.
    pub(crate) model_turn: Option<ModelTurn>,
    /// Tool results by call position.
    pub(crate) tool_results: BTreeMap<usize, UserContent>,
}

pub(crate) fn encode<T: Serialize>(
    value: &T,
    what: &'static str,
) -> Result<Value, DurableRunError> {
    serde_json::to_value(value).map_err(|source| DurableRunError::Codec { what, source })
}

pub(crate) fn decode<T: DeserializeOwned>(
    value: &Value,
    what: &'static str,
) -> Result<T, DurableRunError> {
    serde_json::from_value(value.clone()).map_err(|source| DurableRunError::Codec { what, source })
}

/// The frontier's model task.
pub(crate) fn model_task(frontier: &Frontier) -> Result<&FrontierTask, DurableRunError> {
    frontier
        .tasks()
        .iter()
        .find(|task| task.node().as_str() == MODEL_NODE)
        .ok_or_else(|| DurableRunError::Corrupt("a model step has no model task".to_owned()))
}

/// The frontier's tool task for the call at `index`, if that call needs executing.
pub(crate) fn tool_task(frontier: &Frontier, index: usize) -> Option<&FrontierTask> {
    frontier
        .tasks()
        .iter()
        .find(|task| task.node().as_str() == TOOL_NODE && task_index(task) == Some(index))
}

fn task_index(task: &FrontierTask) -> Option<usize> {
    task.input()
        .get("index")
        .and_then(Value::as_u64)
        .and_then(|index| usize::try_from(index).ok())
}

fn single_entry(value: Value, task: &FrontierTask) -> Result<TaskWrites, DurableRunError> {
    Ok(TaskWrites::new(
        task.id().clone(),
        vec![Update::named(JOURNAL_CHANNEL, value)?],
    ))
}

/// Journals the model's response for the current model step.
pub(crate) fn write_model_turn(
    store: &dyn Checkpointer,
    head: &Checkpoint,
    turn: &ModelTurn,
) -> Result<(), DurableRunError> {
    let task = model_task(head.frontier())?;
    let writes = single_entry(encode(turn, "model turn")?, task)?;
    store.put_task_writes(head.thread(), &head.id(), &writes)?;
    Ok(())
}

/// Journals the result of the tool call at `index` of the current tool step.
pub(crate) fn write_tool_result(
    store: &dyn Checkpointer,
    head: &Checkpoint,
    index: usize,
    result: &UserContent,
) -> Result<(), DurableRunError> {
    let task = tool_task(head.frontier(), index)
        .ok_or_else(|| DurableRunError::Corrupt(format!("no tool task for call {index}")))?;
    let writes = single_entry(encode(result, "tool result")?, task)?;
    match store.put_task_writes(head.thread(), &head.id(), &writes) {
        Ok(()) => Ok(()),
        Err(StoreError::TaskWritesConflict(_)) => Err(DurableRunError::ToolResultConflict(index)),
        Err(other) => Err(other.into()),
    }
}

/// Reads back everything journaled against `head`.
pub(crate) fn load(
    store: &dyn Checkpointer,
    head: &Checkpoint,
) -> Result<Journal, DurableRunError> {
    let mut journal = Journal::default();
    for writes in store.task_writes(head.thread(), &head.id())? {
        let task = head.frontier().get(writes.task()).ok_or_else(|| {
            DurableRunError::Corrupt(format!(
                "journal entry for task `{}`, which is not in the frontier",
                writes.task()
            ))
        })?;
        let [entry] = writes.updates() else {
            return Err(DurableRunError::Corrupt(format!(
                "journal entry for task `{}` must hold exactly one update",
                writes.task()
            )));
        };
        if entry.channel().as_str() != JOURNAL_CHANNEL {
            return Err(DurableRunError::Corrupt(format!(
                "journal entry for task `{}` is on channel `{}`",
                writes.task(),
                entry.channel()
            )));
        }
        match task.node().as_str() {
            MODEL_NODE => {
                journal.model_turn = Some(decode(entry.value(), "journaled model turn")?);
            }
            TOOL_NODE => {
                let index = task_index(task).ok_or_else(|| {
                    DurableRunError::Corrupt(format!(
                        "tool task `{}` has no call position",
                        task.id()
                    ))
                })?;
                journal
                    .tool_results
                    .insert(index, decode(entry.value(), "journaled tool result")?);
            }
            other => {
                return Err(DurableRunError::Corrupt(format!(
                    "journal entry for a task of unknown kind `{other}`"
                )));
            }
        }
    }
    Ok(journal)
}
