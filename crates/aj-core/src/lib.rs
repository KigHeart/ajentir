#![forbid(unsafe_code)]
#![warn(missing_docs)]
//! Validated identities and primitive contracts shared by every AJENTIR crate.
//!
//! `aj-core` sits at the bottom of the dependency graph. It knows nothing about
//! models, tools, storage or scheduling; it only defines the vocabulary the other
//! crates use to refer to the same things.
//!
//! - **The naming rule** ([`validate_name`], [`NameError`], [`NameProblem`]): one
//!   definition of what a legal name is, so every crate refuses the same input.
//! - **Identifiers** ([`RunId`], [`ThreadId`], [`NodeName`], [`ToolCallId`],
//!   [`MessageId`], [`ModelId`], [`ProviderId`]): distinct types, each validated on construction and again on
//!   deserialization.
//! - **The run clock** ([`SuperStep`]).
//!
//! ```
//! use aj_core::{NameProblem, ThreadId};
//!
//! let thread: ThreadId = "ticket-42".parse()?;
//! assert_eq!(thread.as_str(), "ticket-42");
//!
//! let error = ThreadId::new("").unwrap_err();
//! assert_eq!(error.problem(), NameProblem::Empty);
//! assert_eq!(error.to_string(), "invalid thread id: must not be empty");
//! # Ok::<(), aj_core::NameError>(())
//! ```

mod id;
mod name;
mod step;

pub use id::{MessageId, ModelId, NodeName, ProviderId, RunId, ThreadId, ToolCallId};
pub use name::{MAX_NAME_BYTES, NameError, NameProblem, validate_name};
pub use step::SuperStep;
