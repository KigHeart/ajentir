//! Turns a step boundary of the agent state machine into checkpoint ingredients.

use std::collections::BTreeMap;

use ajentir::{Frontier, NodeName, Update, UpdateBatch, WriterId};
use rig_run::{AgentRun, AgentRunStep};
use serde_json::{Value, json};

use crate::{
    error::DurableRunError,
    journal::encode,
    schema::{MODEL_NODE, RUN_CHANNEL, STEP_CHANNEL, TOOL_NODE},
};

/// The writer every boundary update is attributed to.
const WRITER: &str = "driver";

/// Everything needed to seal the checkpoint for one step boundary.
pub(crate) struct Boundary {
    /// Replaces the stored run and step.
    pub(crate) batch: UpdateBatch,
    /// The work the step requires.
    pub(crate) frontier: Frontier,
    /// Human-readable summary for inspecting a thread without decoding it.
    pub(crate) metadata: BTreeMap<String, Value>,
}

/// Builds the ingredients for the checkpoint at `checkpoint_step`, where `run` is
/// the machine *after* it produced `step`.
pub(crate) fn build(
    run: &AgentRun,
    step: &AgentRunStep,
    checkpoint_step: u64,
) -> Result<Boundary, DurableRunError> {
    let batch = UpdateBatch::from_writer_outputs([(
        WriterId::new(WRITER)?,
        vec![
            Update::named(RUN_CHANNEL, encode(run, "agent run")?)?,
            Update::named(STEP_CHANNEL, encode(step, "pending step")?)?,
        ],
    )])?;

    let (phase, tasks) = match step {
        AgentRunStep::CallModel { turn, .. } => (
            "model",
            vec![(NodeName::new(MODEL_NODE)?, json!({ "turn": turn }))],
        ),
        AgentRunStep::CallTools { calls } => {
            // Calls answered by invalid-call recovery need no execution, so they get
            // no task; the rest are numbered by their position in the step.
            let mut tasks = Vec::new();
            for (index, call) in calls.iter().enumerate() {
                if call.preresolved_result.is_none() {
                    tasks.push((
                        NodeName::new(TOOL_NODE)?,
                        json!({ "index": index, "name": call.tool_call.function.name }),
                    ));
                }
            }
            ("tools", tasks)
        }
        AgentRunStep::Done(_) => ("done", Vec::new()),
    };

    let mut metadata = BTreeMap::new();
    metadata.insert("phase".to_owned(), Value::from(phase));
    metadata.insert("turn".to_owned(), Value::from(run.turn()));
    Ok(Boundary {
        batch,
        frontier: Frontier::plan(checkpoint_step, tasks),
        metadata,
    })
}
