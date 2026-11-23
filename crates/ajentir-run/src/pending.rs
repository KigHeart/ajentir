//! What a durable run is waiting for.

use rig_core::message::{Message, UserContent};
use rig_run::{ModelTurn, PendingToolCall, PromptResponse};

/// Phase text for a run waiting on the model.
pub(crate) const PHASE_MODEL: &str = "a model call";
/// Phase text for a run waiting on tool results.
pub(crate) const PHASE_TOOLS: &str = "tool calls";
/// Phase text for a finished run.
pub(crate) const PHASE_DONE: &str = "nothing (the run is done)";

/// A model call that has to be made.
#[derive(Debug, Clone)]
pub struct ModelStep {
    /// The prompt for this turn: the latest message of the run.
    pub prompt: Message,
    /// The conversation preceding `prompt`.
    pub history: Vec<Message>,
    /// One-based index of this model call within the run.
    pub turn: usize,
    /// The model's response if a previous attempt already received and journaled
    /// it before being interrupted. When present the model must **not** be called
    /// again; submit this turn instead.
    pub journaled: Option<ModelTurn>,
}

/// One tool call of the pending step.
#[derive(Debug, Clone)]
pub struct ToolSlot {
    /// Position of the call within the step; the key results are recorded under.
    pub index: usize,
    /// The call as the model emitted it.
    pub call: PendingToolCall,
    /// The call's result when it is already known: either invalid-call recovery
    /// answered it, or a previous attempt executed it and journaled the result.
    /// A slot with a result must **not** be executed again.
    pub result: Option<UserContent>,
}

/// The single thing a durable run needs next.
#[derive(Debug, Clone)]
pub enum Pending {
    /// Call the model.
    Model(ModelStep),
    /// Execute the slots that have no result yet, then complete the step.
    Tools(Vec<ToolSlot>),
    /// The run finished with this response.
    Done(PromptResponse),
}

impl Pending {
    /// Whether the run has finished.
    #[must_use]
    pub fn is_done(&self) -> bool {
        matches!(self, Self::Done(_))
    }
}
