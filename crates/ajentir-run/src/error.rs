//! Errors of the durable run session and of the driver loop.

use ajentir::{BatchError, CheckpointError, NameError, SchemaError, StoreError, ThreadId};
use rig_run::PromptError;

/// A boxed error from a model or tool implementation.
pub type PortError = Box<dyn std::error::Error + Send + Sync>;

/// An operation on a [`DurableRun`](crate::DurableRun) failed.
///
/// Unless stated otherwise, a failure leaves the **stored** run exactly as it was
/// at its last checkpoint. Failures raised by the underlying agent state machine
/// (see [`DurableRunError::Run`]) additionally poison the in-memory session,
/// because that machine may have been partly advanced; drop it and
/// [`DurableRun::resume`](crate::DurableRun::resume) to continue from the last
/// checkpoint.
#[derive(Debug, thiserror::Error)]
pub enum DurableRunError {
    /// The checkpoint store failed or refused a write.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// A checkpoint could not be built, or does not belong to this crate's schema.
    #[error(transparent)]
    Checkpoint(#[from] CheckpointError),
    /// The crate's own state schema could not be built.
    #[error(transparent)]
    Schema(#[from] SchemaError),
    /// A thread, node or channel name was rejected.
    #[error(transparent)]
    Name(#[from] NameError),
    /// A checkpoint's update batch could not be assembled.
    #[error(transparent)]
    Batch(#[from] BatchError),
    /// The agent state machine rejected the operation.
    #[error("agent run failed: {0}")]
    Run(#[source] Box<PromptError>),
    /// Run state or journal data could not be encoded or decoded.
    #[error("cannot encode or decode {what}: {source}")]
    Codec {
        /// What was being converted.
        what: &'static str,
        /// The JSON error.
        #[source]
        source: serde_json::Error,
    },
    /// `start` was called for a thread that already holds checkpoints.
    #[error("thread `{0}` already exists")]
    ThreadExists(ThreadId),
    /// `resume` was called for a thread with no checkpoints.
    #[error("thread `{0}` has no checkpoints")]
    NoSuchThread(ThreadId),
    /// The operation does not apply to the run's current phase.
    #[error("run is waiting for {actual}, but this operation needs {expected}")]
    WrongPhase {
        /// The phase the operation requires.
        expected: &'static str,
        /// The phase the run is actually in.
        actual: &'static str,
    },
    /// `resolve_invalid_tool_call` was called with no invalid call outstanding.
    #[error("no invalid tool call is waiting for a resolution")]
    NothingToResolve,
    /// A model turn must not be submitted while an invalid tool call is unresolved.
    #[error("an invalid tool call must be resolved before anything else")]
    ResolutionPending,
    /// A tool result referred to a call position that does not exist.
    #[error("the pending tool step has no call at position {0}")]
    NoSuchToolCall(usize),
    /// A tool result was recorded for a call that already has a different one.
    #[error("tool call {0} already has a different result")]
    ToolResultConflict(usize),
    /// `complete_tools` was called before every call had a result.
    #[error("tool calls without results: {missing:?}")]
    ToolsIncomplete {
        /// Positions of the calls still unanswered.
        missing: Vec<usize>,
    },
    /// Stored run state or journal entries are inconsistent.
    #[error("stored run data is inconsistent: {0}")]
    Corrupt(String),
    /// An earlier failure left the in-memory session unusable.
    #[error("this session failed earlier and must be resumed from its checkpoint")]
    Poisoned,
}

impl From<PromptError> for DurableRunError {
    fn from(error: PromptError) -> Self {
        Self::Run(Box::new(error))
    }
}

/// [`DurableRun::drive`](crate::DurableRun::drive) stopped before the run finished.
///
/// In every case the run is intact at its last checkpoint and everything the
/// driver had journaled; call `drive` again, or `resume` in another process.
#[derive(Debug, thiserror::Error)]
pub enum DriveError {
    /// The session itself failed.
    #[error(transparent)]
    Run(#[from] DurableRunError),
    /// The model implementation failed; nothing from that attempt was recorded.
    #[error("model call failed: {0}")]
    Model(#[source] PortError),
    /// A tool implementation asked to abort the run instead of reporting a
    /// failure to the model.
    #[error("tool `{tool}` aborted the run: {source}")]
    ToolAborted {
        /// The tool's name.
        tool: String,
        /// The tool's error.
        #[source]
        source: PortError,
    },
}
