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
//! The AJENTIR typed state and checkpoint substrate.
//!
//! This crate is the foundation an agent runtime stands on: it decides what an
//! agent's state *is*, how concurrent updates to it combine, and how a run is
//! saved so it can be resumed. It has no model, tool or network dependencies and
//! does not run agents itself.
//!
//! # The pieces
//!
//! - [`Schema`] declares named channels, each with a [`ValueType`] and a
//!   [`ReducerKind`] that merges concurrent updates. A schema has a name, a
//!   version and a content fingerprint ([`SchemaRef`]).
//! - [`State`] holds each channel's value and version. [`State::apply`] folds an
//!   [`UpdateBatch`] through the reducers as a pure function.
//! - [`UpdateBatch`] orders writes by `(writer, sequence)`, never by completion
//!   time, so task scheduling cannot change the result.
//! - [`Frontier`] records the tasks still to run, with ids derived from their
//!   content.
//! - [`Checkpoint`] bundles state, frontier, schema reference and provenance under
//!   a content-derived [`CheckpointId`]; altering any stored byte is detectable.
//! - [`Checkpointer`] stores checkpoints in linear threads:
//!   [`InMemoryCheckpointer`] (not durable) and, with the default `sqlite`
//!   feature, [`SqliteCheckpointer`] (durable, multi-process safe).
//! - [`fork`] resumes from an earlier checkpoint in a *new* thread, optionally
//!   with an edit, leaving the source history intact. [`Migration`] carries a
//!   checkpoint to a newer schema version.
//!
//! # Example
//!
//! ```
//! use std::collections::BTreeMap;
//!
//! use aj_state::{
//!     Channel, Checkpoint, Checkpointer, ForkSpec, Frontier, InMemoryCheckpointer, NodeName,
//!     ReducerKind, Schema, ThreadId, UpdateBatch, ValueType, WriterId, fork_thread,
//! };
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let schema = Schema::builder("support-agent", 1)
//!     .channel("messages", ValueType::Array, ReducerKind::Append)
//!     .channel("turns", ValueType::Integer, ReducerKind::SumI64)
//!     .build()?;
//! let messages: Channel<Vec<String>> = Channel::new("messages")?;
//! let turns: Channel<i64> = Channel::new("turns")?;
//! let store = InMemoryCheckpointer::new();
//! let thread = ThreadId::new("ticket-42")?;
//!
//! // 1. Start a thread from the user's first message; the `respond` node runs next.
//! let input = UpdateBatch::from_writer_outputs([(
//!     WriterId::new("user")?,
//!     vec![messages.update(&vec!["My invoice is wrong".to_owned()])?],
//! )])?;
//! let first = Checkpoint::initial(
//!     thread.clone(),
//!     &schema,
//!     &input,
//!     Frontier::plan(0, vec![(NodeName::new("respond")?, serde_json::json!({}))]),
//!     BTreeMap::new(),
//! )?;
//! store.put(&first)?;
//!
//! // 2. The node finished: commit what it wrote as step 1 and mark the run done.
//! let step = UpdateBatch::from_writer_outputs([(
//!     WriterId::new("respond")?,
//!     vec![
//!         messages.update(&vec!["Let me check that.".to_owned()])?,
//!         turns.update(&1)?,
//!     ],
//! )])?;
//! let second = first.advance(&schema, &step, Frontier::empty(), BTreeMap::new())?;
//! store.put(&second)?;
//! assert_eq!(turns.read(second.state())?, Some(1));
//!
//! // 3. Try a different continuation from the first checkpoint, in a new thread.
//! let retry = ForkSpec::new(ThreadId::new("ticket-42-retry")?);
//! let forked = fork_thread(&store, &thread, &first.id(), &schema, retry)?;
//! assert_eq!(forked.state(), first.state());
//! assert_eq!(store.history(&thread)?.len(), 2);
//! # Ok(())
//! # }
//! ```
//!
//! # What this crate does not do
//!
//! Checkpointing makes *state* recoverable; it does not make external side effects
//! (an email sent, a payment made) exactly-once. There are no distributed leases
//! or fencing tokens, no approval workflow and no execution ledger here; those
//! belong to the runtime layers built on top. The in-memory checkpointer loses
//! everything when dropped.

pub mod canonical;
pub mod channel;
pub mod checkpoint;
pub mod checkpointer;
pub mod digest;
pub mod fork;
pub mod frontier;
pub mod ids;
pub mod migration;
pub mod reducer;
pub mod schema;
pub mod state;
pub mod update;
pub mod value_type;

pub use channel::{Channel, ChannelError};
pub use checkpoint::{Checkpoint, CheckpointError, CheckpointId, Provenance};
#[cfg(feature = "sqlite")]
pub use checkpointer::SqliteCheckpointer;
pub use checkpointer::{CheckpointSummary, Checkpointer, InMemoryCheckpointer, StoreError};
pub use digest::{Digest, DigestParseError};
pub use fork::{ForkSpec, fork_checkpoint, fork_thread};
pub use frontier::{Frontier, FrontierError, FrontierTask};
pub use ids::{
    ChannelName, NameError, NameProblem, NodeName, SchemaName, TaskId, ThreadId, WriterId,
};
pub use migration::{Migration, MigrationError, MigrationStep};
pub use reducer::{ReduceError, ReducerKind};
pub use schema::{
    ChannelSpec, IncompatibleSpec, Schema, SchemaBuilder, SchemaError, SchemaRef, SchemaVersion,
};
pub use state::{AppliedBatch, State, StateError, UpdateError};
pub use update::{BatchError, TaskWrites, Update, UpdateBatch, Write};
pub use value_type::ValueType;
