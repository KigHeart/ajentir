#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic
    )
)]
//! Shared, provider-independent identities and primitive contracts.
//!
//! This crate owns AJENTIR's canonical identifier validation and primitive
//! identity types. Domain crates should reuse these types rather than implement
//! parallel validation rules for the same identity.

use std::borrow::Borrow;
use std::fmt;
use std::str::FromStr;

/// Why an identifier was rejected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IdentityError {
    /// The identifier was empty.
    Empty,
    /// The identifier exceeded its type's maximum byte length.
    TooLong { maximum: usize },
    /// The identifier contained a control character.
    ControlCharacter,
}

impl fmt::Display for IdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("identifier must not be empty"),
            Self::TooLong { maximum } => write!(f, "identifier exceeds {maximum} bytes"),
            Self::ControlCharacter => f.write_str("identifier contains a control character"),
        }
    }
}

impl std::error::Error for IdentityError {}

/// A validated string used as the representation of a typed identity.
///
/// The maximum is expressed in bytes, not Unicode scalar values. This keeps
/// identifier limits stable across serialization and storage boundaries.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Identifier(String);

impl Identifier {
    /// Validates and constructs an identifier with a byte-length limit.
    pub fn new(value: impl Into<String>, maximum: usize) -> Result<Self, IdentityError> {
        let value = value.into();
        if value.is_empty() {
            return Err(IdentityError::Empty);
        }
        if value.len() > maximum {
            return Err(IdentityError::TooLong { maximum });
        }
        if value.chars().any(char::is_control) {
            return Err(IdentityError::ControlCharacter);
        }
        Ok(Self(value))
    }

    /// Returns the identifier as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consumes the identifier and returns its string representation.
    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Display for Identifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

macro_rules! identifier_type {
    ($name:ident, $maximum:expr, $documentation:literal) => {
        #[doc = $documentation]
        #[derive(
            Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize
        )]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(Identifier);

        impl $name {
            /// Validates and constructs this identity.
            pub fn new(value: impl Into<String>) -> Result<Self, IdentityError> {
                Identifier::new(value, $maximum).map(Self)
            }

            /// Returns the identity as text.
            pub fn as_str(&self) -> &str {
                self.0.as_str()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl FromStr for $name {
            type Err = IdentityError;
            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::new(value)
            }
        }

        impl TryFrom<String> for $name {
            type Error = IdentityError;
            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0.into_string()
            }
        }

        impl Borrow<str> for $name {
            fn borrow(&self) -> &str {
                self.as_str()
            }
        }
    };
}

identifier_type!(RunId, 256, "Identifies one durable runtime execution.");
identifier_type!(
    ThreadId,
    256,
    "Identifies one linear history of checkpoints."
);
identifier_type!(
    NodeId,
    256,
    "Identifies one logical node in a runtime graph."
);
identifier_type!(CheckpointId, 256, "Identifies one immutable checkpoint.");

/// The zero-based BSP super-step number.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SuperStep(pub u64);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_reject_empty_control_and_oversized_values() {
        assert_eq!(NodeId::new(""), Err(IdentityError::Empty));
        assert_eq!(NodeId::new("node\n1"), Err(IdentityError::ControlCharacter));
        assert_eq!(
            NodeId::new("x".repeat(257)),
            Err(IdentityError::TooLong { maximum: 256 })
        );
    }

    #[test]
    fn identifiers_have_stable_order_and_display() {
        let alpha = NodeId::new("alpha");
        let beta = NodeId::new("beta");
        assert!(alpha.is_ok() && beta.is_ok());
        if let (Ok(alpha), Ok(beta)) = (alpha, beta) {
            assert!(alpha < beta);
            assert_eq!(alpha.to_string(), "alpha");
        }
    }

    #[test]
    fn thread_identity_round_trips_as_a_validated_string() {
        let thread = ThreadId::new("ticket-42").expect("valid test identity");
        let encoded = serde_json::to_string(&thread).expect("serialize identity");
        assert_eq!(encoded, "\"ticket-42\"");
        let decoded: ThreadId = serde_json::from_str(&encoded).expect("deserialize identity");
        assert_eq!(decoded, thread);
        assert!(serde_json::from_str::<ThreadId>("\"\"").is_err());
        assert!(serde_json::from_str::<ThreadId>("\"a\\u0000b\"").is_err());
    }

    #[test]
    fn typed_identities_support_borrowed_map_lookup() {
        let mut values = std::collections::BTreeMap::new();
        values.insert(ThreadId::new("thread-a").expect("valid id"), 7);
        assert_eq!(values.get("thread-a"), Some(&7));
    }
}
