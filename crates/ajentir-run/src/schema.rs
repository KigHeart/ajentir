//! The `ajentir` schema a durable run is stored under, and the names it uses.
//!
//! Every checkpoint of a run holds two channels:
//!
//! - [`RUN_CHANNEL`]: the serialized [`rig_run::AgentRun`] as it stands at a step
//!   boundary — after the machine produced its next step, before anyone acted on it.
//! - [`STEP_CHANNEL`]: that next step ([`rig_run::AgentRunStep`]) itself. It is
//!   stored rather than recomputed so a resumed process does not call the machine's
//!   `next_step` a second time for the same boundary.
//!
//! The checkpoint's **frontier** is the work that step requires: one `model` task
//! for a model call, one `tool` task per call that still has to be executed, and
//! nothing once the run is done.
//!
//! Finished work is recorded as **task writes** under [`JOURNAL_CHANNEL`]: the
//! model's response for the model task, a tool's result for each tool task. This
//! channel is deliberately *not* declared in the schema. Task writes are a
//! recovery journal read back only by this crate; they are never applied to state
//! as channel updates, because the finished step is committed by replacing
//! [`RUN_CHANNEL`] and [`STEP_CHANNEL`] wholesale.

use ajentir::{ReducerKind, Schema, ValueType};

use crate::error::DurableRunError;

/// Name of the schema every durable run is stored under.
pub const SCHEMA_NAME: &str = "ajentir-run";

/// Version of that schema. A change to the channels or their meaning must raise it
/// and ship an `ajentir::Migration`.
pub const SCHEMA_VERSION: u32 = 1;

/// Channel holding the serialized agent run.
pub const RUN_CHANNEL: &str = "run";

/// Channel holding the pending step.
pub const STEP_CHANNEL: &str = "step";

/// Channel name under which finished work is journaled as task writes. Not part of
/// the schema; see the module documentation.
pub const JOURNAL_CHANNEL: &str = "journal";

/// Frontier node for a pending model call.
pub const MODEL_NODE: &str = "model";

/// Frontier node for a pending tool call.
pub const TOOL_NODE: &str = "tool";

/// Builds the schema durable runs are stored under.
///
/// # Errors
///
/// Returns [`DurableRunError::Schema`] only if the fixed definition above were
/// invalid, which the crate's tests rule out.
pub fn schema() -> Result<Schema, DurableRunError> {
    Ok(Schema::builder(SCHEMA_NAME, SCHEMA_VERSION)
        .channel(RUN_CHANNEL, ValueType::Object, ReducerKind::Overwrite)
        .channel(STEP_CHANNEL, ValueType::Object, ReducerKind::Overwrite)
        .build()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_schema_builds_with_both_channels() {
        let schema = schema().unwrap();
        assert_eq!(schema.name().as_str(), SCHEMA_NAME);
        assert_eq!(schema.version().get(), SCHEMA_VERSION);
        assert!(schema.channel(RUN_CHANNEL).is_some());
        assert!(schema.channel(STEP_CHANNEL).is_some());
        assert!(schema.channel(JOURNAL_CHANNEL).is_none());
    }

    #[test]
    fn the_fingerprint_is_stable_across_builds() {
        assert_eq!(
            schema().unwrap().fingerprint(),
            schema().unwrap().fingerprint()
        );
    }
}
