//! Validated state-domain names and identifiers.
//!
//! The canonical runtime thread identity is owned by aj-core. State keeps
//! distinct newtypes for concepts that are not interchangeable, while
//! delegating their shared validation policy to aj_core::Identifier.

use std::{borrow::Borrow, fmt, str::FromStr};

use aj_core::{Identifier, IdentityError};
use serde::{Deserialize, Serialize};

pub use aj_core::{NodeId as NodeName, ThreadId};

/// Why a state-domain name was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum NameProblem {
    /// The name was empty.
    #[error("must not be empty")]
    Empty,
    /// The name was longer than 256 bytes.
    #[error("must be at most 256 bytes")]
    TooLong,
    /// The name contained a control character.
    #[error("must not contain control characters")]
    ControlCharacter,
}

/// A state-domain name or identifier that failed validation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid {what}: {problem}")]
pub struct NameError {
    what: &'static str,
    problem: NameProblem,
}

impl NameError {
    /// What kind of name was being validated.
    #[must_use]
    pub fn what(&self) -> &'static str {
        self.what
    }

    /// The rule the name broke.
    #[must_use]
    pub fn problem(&self) -> NameProblem {
        self.problem
    }

    fn from_identity(what: &'static str, error: IdentityError) -> Self {
        let problem = match error {
            IdentityError::Empty => NameProblem::Empty,
            IdentityError::TooLong { .. } => NameProblem::TooLong,
            IdentityError::ControlCharacter => NameProblem::ControlCharacter,
        };
        Self { what, problem }
    }
}

macro_rules! name_type {
    ($(#[$meta:meta])* $name:ident, $what:literal) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            #[doc = concat!("Validates and wraps a ", $what, ".")]
            ///
            /// # Errors
            ///
            /// Returns NameError if the value is empty, longer than 256 bytes,
            /// or contains a control character.
            pub fn new(value: impl Into<String>) -> Result<Self, NameError> {
                Identifier::new(value, 256)
                    .map(Identifier::into_string)
                    .map(Self)
                    .map_err(|error| NameError::from_identity($what, error))
            }

            /// The name as text.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = NameError;
            fn try_from(value: String) -> Result<Self, NameError> {
                Self::new(value)
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> String {
                value.0
            }
        }

        impl FromStr for $name {
            type Err = NameError;
            fn from_str(value: &str) -> Result<Self, NameError> {
                Self::new(value)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl Borrow<str> for $name {
            fn borrow(&self) -> &str {
                self.as_str()
            }
        }
    };
}

name_type! {
    /// Names one channel of a Schema.
    ChannelName, "channel name"
}

name_type! {
    /// Identifies who produced a write.
    WriterId, "writer id"
}

name_type! {
    /// Identifies one task in a Frontier.
    TaskId, "task id"
}

name_type! {
    /// Names a Schema independently of its version.
    SchemaName, "schema name"
}

impl TaskId {
    /// Wraps a string the crate itself produced (a hex digest).
    pub(crate) fn from_trusted(value: String) -> Self {
        Self(value)
    }
}

impl From<&TaskId> for WriterId {
    /// A task's writes are attributed to the task itself.
    fn from(task: &TaskId) -> Self {
        Self(task.0.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_ordinary_names() {
        assert_eq!(ThreadId::new("ticket-42").unwrap().as_str(), "ticket-42");
        assert!(ChannelName::new("messages").is_ok());
        assert!(NodeName::new("plan → act").is_ok());
    }

    #[test]
    fn rejects_empty_long_and_control_names() {
        assert_eq!(ThreadId::new("").unwrap_err(), IdentityError::Empty);
        assert_eq!(
            ThreadId::new("x".repeat(257)).unwrap_err(),
            IdentityError::TooLong { maximum: 256 }
        );
        assert_eq!(
            ThreadId::new("a\nb").unwrap_err(),
            IdentityError::ControlCharacter
        );
        assert_eq!(
            ChannelName::new("").unwrap_err().problem(),
            NameProblem::Empty
        );
        assert_eq!(
            ChannelName::new("x".repeat(257)).unwrap_err().problem(),
            NameProblem::TooLong
        );
        assert!(ThreadId::new("x".repeat(256)).is_ok());
        assert_eq!(
            ChannelName::new("a\nb").unwrap_err().problem(),
            NameProblem::ControlCharacter
        );
        assert_eq!(ChannelName::new("").unwrap_err().what(), "channel name");
    }

    #[test]
    fn deserialization_enforces_the_same_rule() {
        assert!(serde_json::from_str::<ThreadId>("\"ok\"").is_ok());
        assert!(serde_json::from_str::<ThreadId>("\"\"").is_err());
        assert!(serde_json::from_str::<ThreadId>("\"a\\u0000b\"").is_err());
        assert!(serde_json::from_str::<ChannelName>("\"\"").is_err());
    }

    #[test]
    fn serializes_as_a_plain_string() {
        let id = ThreadId::new("t").unwrap();
        assert_eq!(serde_json::to_string(&id).unwrap(), "\"t\"");
    }

    #[test]
    fn map_lookup_by_str_works() {
        let mut map = std::collections::BTreeMap::new();
        map.insert(ChannelName::new("messages").unwrap(), 1);
        assert_eq!(map.get("messages"), Some(&1));
        assert_eq!(map.get("missing"), None);
    }

    #[test]
    fn a_task_is_a_writer() {
        let task = TaskId::new("abc").unwrap();
        assert_eq!(WriterId::from(&task).as_str(), "abc");
    }
}
