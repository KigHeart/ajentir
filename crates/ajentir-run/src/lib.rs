#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic
    )
)]
//! Durable driver for the AJENTIR agent loop.
//!
//! The agent loop itself — turn counting, tool-call validation, history threading,
//! usage and the final response — already exists as the sans-IO
//! [`rig_run::AgentRun`] state machine. This crate does not replace it. It makes a
//! run **survive interruption**: every step boundary of the machine is stored as an
//! [`ajentir`] checkpoint, and every expensive piece of work done between boundaries
//! is journaled the moment it finishes.
//!
//! # How a run is stored
//!
//! At a *step boundary* — the machine has just produced its next step and nobody
//! has acted on it yet — one checkpoint holds the serialized machine, the pending
//! step, and, as its frontier, the work that step needs: one `model` task, one
//! `tool` task per call still to execute, or nothing when the run is done (see the
//! [`schema`] module). A finished model response or tool result is written as the
//! task write of its task. The next boundary replaces the checkpoint; the journal
//! of the previous one is no longer needed.
//!
//! # What recovery guarantees
//!
//! | Interrupted while… | After `resume` |
//! | --- | --- |
//! | waiting for the model (nothing received) | the model is called again — at-least-once |
//! | the model answered, before its checkpoint | the **journaled response is used; the model is not called again** |
//! | tools running, some finished | **finished tools are not run again**; unfinished ones run again — at-least-once |
//! | between a finished step and its checkpoint | the same, because the finished work is journaled |
//! | an invalid tool call awaiting a decision | the response is replayed and the same decision is asked again |
//!
//! Re-running a tool that was in flight is unavoidable, so a tool with external
//! side effects must itself be idempotent or deduplicate on the tool-call id. This
//! crate makes the *state* of a run recoverable; it cannot make the outside world
//! exactly-once.
//!
//! The recovery tests kill a run at every one of its storage operations — both
//! before a write and after a write whose acknowledgement was lost — and require the
//! recovered run to produce the same response, and a checkpoint chain with the same
//! phases, as an uninterrupted one. Targeted tests separately pin down which work is
//! *not* repeated (a journaled model response, a finished tool).
//!
//! # Example
//!
//! ```
//! use std::{collections::BTreeSet, sync::Arc};
//!
//! use ajentir::{InMemoryCheckpointer, ThreadId};
//! use ajentir_run::{DurableRun, FailFast, ModelCall, ModelPort, PortError, ToolError, ToolPort};
//! use rig_core::{
//!     completion::Usage,
//!     message::{AssistantContent, ToolCall, ToolResultContent},
//! };
//! use rig_run::{AgentRun, ModelTurn};
//!
//! /// A model that asks for the clock once, then answers.
//! struct Model;
//!
//! impl ModelPort for Model {
//!     async fn complete(&self, call: ModelCall<'_>) -> Result<ModelTurn, PortError> {
//!         let tools: BTreeSet<String> = ["clock".to_owned()].into();
//!         let choice = if call.turn == 1 {
//!             vec![AssistantContent::tool_call("c1", "clock", serde_json::json!({}))]
//!         } else {
//!             vec![AssistantContent::text("It is noon.")]
//!         };
//!         Ok(ModelTurn::new(None, choice, Usage::new(), tools.clone(), tools))
//!     }
//! }
//!
//! struct Tools;
//!
//! impl ToolPort for Tools {
//!     async fn call(&self, _call: &ToolCall) -> Result<Vec<ToolResultContent>, ToolError> {
//!         Ok(vec![ToolResultContent::text("12:00")])
//!     }
//! }
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let store = Arc::new(InMemoryCheckpointer::new());
//! let thread = ThreadId::new("job-1")?;
//!
//! let agent = AgentRun::new("What time is it?").max_turns(4);
//! let mut run = DurableRun::start(store.clone(), thread.clone(), agent)?;
//! let response = futures::executor::block_on(run.drive(&Model, &Tools, &FailFast))?;
//! assert_eq!(response.output, "It is noon.");
//!
//! // Any later session — in this process or another — picks the run up from the store.
//! assert!(DurableRun::resume(store, thread)?.is_done());
//! # Ok(())
//! # }
//! ```
//!
//! # Driving by hand
//!
//! [`DurableRun::drive`] is a convenience loop. Everything it does is available
//! step by step — [`DurableRun::pending`], [`DurableRun::complete_model`],
//! [`DurableRun::resolve_invalid_tool_call`], [`DurableRun::record_tool_result`],
//! [`DurableRun::complete_tools`] — for hosts that schedule model and tool work
//! themselves.
//!
//! # Limits
//!
//! - **No hooks or streaming.** Completion-response hooks, per-turn hooks that can
//!   retry or stop a turn, tool hooks and streamed turns belong to the driver in
//!   `rig-agent`; they are not offered here. A model turn is accepted as soon as the
//!   state machine accepts it.
//! - **Stored size and sensitivity.** A checkpoint embeds the whole conversation and
//!   every completed call's raw provider response ([`rig_run::AgentRun`] documents
//!   this). Stored runs carry whatever the conversation contains. The serialized
//!   machine has no cross-version stability guarantee: resume with the same `rig`
//!   version that wrote the run.
//! - **Synchronous storage.** [`ajentir::Checkpointer`] calls block the calling
//!   thread, including inside `drive`.
//! - **One writer per thread.** Two sessions resuming the same thread race; the one
//!   that records or commits second gets a store error (`NotHead` or `HeadMismatch`)
//!   instead of forking history, and can `resume` to catch up. There are no leases,
//!   so the *work* of both may still run.

mod boundary;
mod drive;
pub mod error;
mod journal;
pub mod pending;
pub mod ports;
pub mod schema;
mod session;

pub use error::{DriveError, DurableRunError, PortError};
pub use pending::{ModelStep, Pending, ToolSlot};
pub use ports::{FailFast, InvalidToolCallPolicy, ModelCall, ModelPort, ToolError, ToolPort};
pub use session::{DurableRun, ModelOutcome};
