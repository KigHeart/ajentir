//! The small interfaces through which the driver reaches the outside world.
//!
//! The durable driver performs no IO of its own. It asks a [`ModelPort`] for model
//! responses, a [`ToolPort`] to execute tools, and an [`InvalidToolCallPolicy`] how
//! to recover when the model names a tool that does not exist. Adapters for
//! concrete providers and tool registries implement these traits outside this
//! crate.
//!
//! The async methods return `impl Future` without a `Send` bound, so the traits
//! work on single-threaded and WASM executors alike. Callers on a multi-threaded
//! runtime use concrete implementations whose futures are `Send`, which the
//! compiler then propagates to the driver future.

use std::future::Future;

use rig_core::message::{Message, ToolCall, ToolResultContent};
use rig_run::{InvalidToolCallAction, InvalidToolCallContext, ModelTurn};

use crate::error::PortError;

/// The request the driver hands to a [`ModelPort`].
#[derive(Debug, Clone, Copy)]
pub struct ModelCall<'a> {
    /// The prompt for this turn.
    pub prompt: &'a Message,
    /// The conversation preceding the prompt.
    pub history: &'a [Message],
    /// One-based index of this model call within the run.
    pub turn: usize,
}

/// Produces the model's response to a [`ModelCall`].
pub trait ModelPort {
    /// Sends the request and returns the model's turn.
    ///
    /// The returned [`ModelTurn`] must name, in `executable_tool_names` and
    /// `allowed_tool_names`, the tools that were actually offered to the model for
    /// this call; the run validates the model's tool calls against them.
    ///
    /// # Errors
    ///
    /// Any error aborts the drive loop *before* anything is recorded for this
    /// attempt, so the call is simply made again on the next `drive`.
    fn complete(&self, call: ModelCall<'_>) -> impl Future<Output = Result<ModelTurn, PortError>>;
}

/// Why a tool did not produce a result.
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    /// The tool ran and failed in a way the model should hear about (bad
    /// arguments, a missing record, a rejected request). The message becomes the
    /// tool's result and the run continues.
    #[error("{0}")]
    Failed(String),
    /// The tool could not be run at all (infrastructure down, cancelled). The run
    /// stops without recording a result for this call, so it is attempted again
    /// on the next `drive`.
    #[error(transparent)]
    Abort(PortError),
}

/// Executes one tool call.
pub trait ToolPort {
    /// Runs the tool named by `call` with its arguments.
    ///
    /// Calls of one step may be executed concurrently. Each finished call is
    /// journaled immediately, so after an interruption only calls that had not
    /// finished are executed again — which makes tools **at-least-once**: one that
    /// was running when the process died runs a second time.
    ///
    /// # Errors
    ///
    /// See [`ToolError`].
    fn call(
        &self,
        call: &ToolCall,
    ) -> impl Future<Output = Result<Vec<ToolResultContent>, ToolError>>;
}

/// Decides how to recover when the model emits a tool call that is unknown or
/// not allowed for the turn.
pub trait InvalidToolCallPolicy {
    /// Chooses the recovery action for `context`.
    fn resolve(&self, context: &InvalidToolCallContext) -> InvalidToolCallAction;
}

/// The default policy: an invalid tool call fails the run.
#[derive(Debug, Clone, Copy, Default)]
pub struct FailFast;

impl InvalidToolCallPolicy for FailFast {
    fn resolve(&self, _context: &InvalidToolCallContext) -> InvalidToolCallAction {
        InvalidToolCallAction::fail()
    }
}

impl<F> InvalidToolCallPolicy for F
where
    F: Fn(&InvalidToolCallContext) -> InvalidToolCallAction,
{
    fn resolve(&self, context: &InvalidToolCallContext) -> InvalidToolCallAction {
        self(context)
    }
}
