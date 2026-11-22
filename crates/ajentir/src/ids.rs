//! Validated names and identifiers.
//!
//! Every identifier in AJENTIR is a distinct type so a thread id can never be
//! passed where a channel name is expected. All of them share one validation
//! rule — non-empty, at most 256 bytes, no control characters — and that rule is
//! also enforced when they are deserialized, so persisted data cannot smuggle in
//! a name the constructors would have refused.

use std::{borrow::Borrow, fmt, str::FromStr};

use serde::{Deserialize, Serialize};

const MAX_NAME_BYTES: usize = 256;

/// Why a name was rejected.
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

/// A name or identifier that failed validation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid {what}: {problem}")]
pub struct NameError {
    what: &'static str,
    problem: NameProblem,
}

impl NameError {
    /// What kind of name was being validated, e.g. `"channel name"`.
    #[must_use]
    pub fn what(&self) -> &'static str {
        self.what
    }

    /// The rule the name broke.
    #[must_use]
    pub fn problem(&self) -> NameProblem {
        self.problem
    }
}

fn validate(what: &'static str, value: &str) -> Result<(), NameError> {
    let problem = if value.is_empty() {
        Some(NameProblem::Empty)
    } else if value.len() > MAX_NAME_BYTES {
        Some(NameProblem::TooLong)
    } else if value.chars().any(char::is_control) {
        Some(NameProblem::ControlCharacter)
    } else {
        None
    };
    match problem {
        Some(problem) => Err(NameError { what, problem }),
        None => Ok(()),
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
            /// Returns [`NameError`] if the value is empty, longer than 256 bytes,
            /// or contains a control character.
            pub fn new(value: impl Into<String>) -> Result<Self, NameError> {
                let value = value.into();
                validate($what, &value)?;
                Ok(Self(value))
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
                validate($what, &value)?;
                Ok(Self(value))
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
                f.write_str(&self.0)
            }
        }

        impl Borrow<str> for $name {
            fn borrow(&self) -> &str {
                &self.0
            }
        }
    };
}

name_type! {
    /// Identifies one linear history of checkpoints. A thread is never branched;
    /// forking copies a checkpoint into a new thread.
    ThreadId, "thread id"
}

name_type! {
    /// Names one channel of a [`Schema`](crate::Schema).
    ChannelName, "channel name"
}

name_type! {
    /// Names a unit of work (a node) that a frontier task will run.
    NodeName, "node name"
}

name_type! {
    /// Identifies who produced a write. Writers order writes inside a batch, so
    /// the id must be stable across retries and independent of scheduling.
    WriterId, "writer id"
}

name_type! {
    /// Identifies one task in a [`Frontier`](crate::Frontier).
    TaskId, "task id"
}

name_type! {
    /// Names a [`Schema`](crate::Schema) independently of its version.
    SchemaName, "schema name"
}

impl TaskId {
    /// Wraps a string the crate itself produced (a hex digest), which is known to
    /// satisfy the naming rule.
    pub(crate) fn from_trusted(value: String) -> Self {
        Self(value)
    }
}

impl From<&TaskId> for WriterId {
    /// A task's writes are attributed to the task itself. A valid task id is a
    /// valid writer id because both share one naming rule.
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
        assert_eq!(ThreadId::new("").unwrap_err().problem(), NameProblem::Empty);
        assert_eq!(
            ThreadId::new("x".repeat(257)).unwrap_err().problem(),
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
