//! The JSON type a channel is declared to hold.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The kind of JSON value a channel accepts, both as an update and as stored state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValueType {
    /// Any JSON value, including `null`.
    Any,
    /// `true` or `false`.
    Bool,
    /// A JSON number representable as `i64`.
    Integer,
    /// Any JSON number.
    Number,
    /// A JSON string.
    String,
    /// A JSON array.
    Array,
    /// A JSON object.
    Object,
}

impl ValueType {
    /// A stable lowercase name, also used when hashing schemas.
    #[must_use]
    pub fn tag(self) -> &'static str {
        match self {
            Self::Any => "any",
            Self::Bool => "bool",
            Self::Integer => "integer",
            Self::Number => "number",
            Self::String => "string",
            Self::Array => "array",
            Self::Object => "object",
        }
    }

    /// Whether `value` is acceptable for a channel of this type.
    #[must_use]
    pub fn accepts(self, value: &Value) -> bool {
        match self {
            Self::Any => true,
            Self::Bool => value.is_boolean(),
            Self::Integer => value.is_i64(),
            Self::Number => value.is_number(),
            Self::String => value.is_string(),
            Self::Array => value.is_array(),
            Self::Object => value.is_object(),
        }
    }
}

impl fmt::Display for ValueType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.tag())
    }
}

/// Names the JSON kind of `value` for error messages.
#[must_use]
pub fn describe(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(number) if number.is_i64() => "integer",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn each_type_accepts_exactly_its_kind() {
        assert!(ValueType::Any.accepts(&json!(null)));
        assert!(ValueType::Bool.accepts(&json!(true)));
        assert!(!ValueType::Bool.accepts(&json!(1)));
        assert!(ValueType::Integer.accepts(&json!(-3)));
        assert!(!ValueType::Integer.accepts(&json!(1.5)));
        assert!(!ValueType::Integer.accepts(&json!(u64::MAX)));
        assert!(ValueType::Number.accepts(&json!(1.5)));
        assert!(ValueType::Number.accepts(&json!(2)));
        assert!(ValueType::String.accepts(&json!("s")));
        assert!(ValueType::Array.accepts(&json!([])));
        assert!(!ValueType::Array.accepts(&json!({})));
        assert!(ValueType::Object.accepts(&json!({})));
    }

    #[test]
    fn describe_names_the_kind() {
        assert_eq!(describe(&json!(null)), "null");
        assert_eq!(describe(&json!(1)), "integer");
        assert_eq!(describe(&json!(1.5)), "number");
        assert_eq!(describe(&json!([1])), "array");
    }
}
