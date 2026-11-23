//! Identifiers shared across crates.
//!
//! Each identifier is its own type, so a thread id can never be passed where a run
//! id is expected. All of them follow the rule in [`crate::name`] and re-check it on
//! deserialization.

use std::{borrow::Borrow, fmt, str::FromStr};

use serde::{Deserialize, Serialize};

use crate::name::{NameError, validate_name};

macro_rules! identifier {
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
                validate_name($what, &value)?;
                Ok(Self(value))
            }

            /// The identifier as text.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = NameError;

            fn try_from(value: String) -> Result<Self, NameError> {
                validate_name($what, &value)?;
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

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl Borrow<str> for $name {
            fn borrow(&self) -> &str {
                &self.0
            }
        }
    };
}

identifier! {
    /// Identifies one execution of an agent from start to finish.
    RunId, "run id"
}

identifier! {
    /// Identifies one linear history of checkpoints. A thread is never branched;
    /// forking copies a checkpoint into a new thread.
    ThreadId, "thread id"
}

identifier! {
    /// Names a unit of work in an agent's graph.
    NodeName, "node name"
}

identifier! {
    /// Identifies one tool call. Providers assign these, and a tool result refers
    /// back to the call it answers by this id.
    ToolCallId, "tool call id"
}

identifier! {
    /// Identifies one message in a conversation.
    MessageId, "message id"
}

identifier! {
    /// Names a model as its provider knows it, for example `"claude-sonnet-5-5"`.
    ModelId, "model id"
}

identifier! {
    /// Names a model provider, for example `"anthropic"`.
    ProviderId, "provider id"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::name::NameProblem;

    #[test]
    fn a_valid_value_round_trips_through_text() {
        let id = ThreadId::new("ticket-42").unwrap();
        assert_eq!(id.as_str(), "ticket-42");
        assert_eq!(id.to_string(), "ticket-42");
        assert_eq!("ticket-42".parse::<ThreadId>().unwrap(), id);
        assert_eq!(String::from(id), "ticket-42");
    }

    #[test]
    fn every_identifier_applies_the_shared_rule_and_names_itself() {
        let cases: [(Result<String, NameError>, &str); 7] = [
            (RunId::new("").map(String::from), "run id"),
            (ThreadId::new("").map(String::from), "thread id"),
            (NodeName::new("").map(String::from), "node name"),
            (ToolCallId::new("").map(String::from), "tool call id"),
            (MessageId::new("").map(String::from), "message id"),
            (ModelId::new("").map(String::from), "model id"),
            (ProviderId::new("").map(String::from), "provider id"),
        ];
        for (result, what) in cases {
            let error = result.unwrap_err();
            assert_eq!(error.what(), what);
            assert_eq!(error.problem(), NameProblem::Empty);
        }
    }

    #[test]
    fn identifiers_order_and_hash_by_their_text() {
        let alpha = NodeName::new("alpha").unwrap();
        let beta = NodeName::new("beta").unwrap();
        assert!(alpha < beta);
        let mut set = std::collections::BTreeSet::new();
        set.insert(beta.clone());
        set.insert(alpha.clone());
        set.insert(alpha.clone());
        assert_eq!(set.into_iter().collect::<Vec<_>>(), vec![alpha, beta]);
    }

    #[test]
    fn a_map_keyed_by_an_identifier_can_be_looked_up_by_text() {
        let mut map = std::collections::BTreeMap::new();
        map.insert(ToolCallId::new("call-1").unwrap(), 1);
        assert_eq!(map.get("call-1"), Some(&1));
    }

    #[test]
    fn serialization_is_a_plain_json_string() {
        let id = RunId::new("run-7").unwrap();
        assert_eq!(serde_json::to_string(&id).unwrap(), "\"run-7\"");
        assert_eq!(serde_json::from_str::<RunId>("\"run-7\"").unwrap(), id);
    }

    #[test]
    fn deserialization_refuses_what_the_constructor_refuses() {
        assert!(serde_json::from_str::<MessageId>("\"\"").is_err());
        assert!(serde_json::from_str::<MessageId>("\"a\\u0000b\"").is_err());
        let long = format!("\"{}\"", "a".repeat(257));
        assert!(serde_json::from_str::<MessageId>(&long).is_err());
        assert!(serde_json::from_str::<MessageId>("7").is_err());
    }

    #[test]
    fn a_failed_parse_reports_the_kind_of_identifier() {
        let error = "".parse::<ToolCallId>().unwrap_err();
        assert_eq!(error.to_string(), "invalid tool call id: must not be empty");
    }
}
