//! Canonical JSON encoding.
//!
//! Content hashes (schema fingerprints, state digests, checkpoint ids) must not
//! depend on how a `serde_json::Value` happened to be built. `serde_json` keeps
//! object keys sorted by default, but the `preserve_order` feature — which any
//! crate in a dependency graph may switch on — changes that. This module encodes
//! values itself so the bytes are identical regardless of that feature:
//!
//! - object keys are sorted by their UTF-8 bytes,
//! - no insignificant whitespace is emitted,
//! - strings use the shortest valid JSON escapes,
//! - numbers use `serde_json`'s own formatting (shortest round-trip for floats).
//!
//! The output is valid JSON and parses back to an equal `Value`.

use serde_json::Value;

/// Encodes `value` as canonical JSON bytes.
#[must_use]
pub fn to_vec(value: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    write_value(value, &mut out);
    out
}

fn write_value(value: &Value, out: &mut Vec<u8>) {
    match value {
        Value::Null => out.extend_from_slice(b"null"),
        Value::Bool(true) => out.extend_from_slice(b"true"),
        Value::Bool(false) => out.extend_from_slice(b"false"),
        Value::Number(number) => out.extend_from_slice(number.to_string().as_bytes()),
        Value::String(text) => write_string(text, out),
        Value::Array(items) => {
            out.push(b'[');
            for (position, item) in items.iter().enumerate() {
                if position > 0 {
                    out.push(b',');
                }
                write_value(item, out);
            }
            out.push(b']');
        }
        Value::Object(map) => {
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            entries.sort_by(|left, right| left.0.cmp(right.0));
            out.push(b'{');
            for (position, (key, item)) in entries.into_iter().enumerate() {
                if position > 0 {
                    out.push(b',');
                }
                write_string(key, out);
                out.push(b':');
                write_value(item, out);
            }
            out.push(b'}');
        }
    }
}

fn write_string(text: &str, out: &mut Vec<u8>) {
    out.push(b'"');
    for ch in text.chars() {
        match ch {
            '"' => out.extend_from_slice(b"\\\""),
            '\\' => out.extend_from_slice(b"\\\\"),
            '\n' => out.extend_from_slice(b"\\n"),
            '\r' => out.extend_from_slice(b"\\r"),
            '\t' => out.extend_from_slice(b"\\t"),
            '\u{08}' => out.extend_from_slice(b"\\b"),
            '\u{0c}' => out.extend_from_slice(b"\\f"),
            control if u32::from(control) < 0x20 => {
                out.extend_from_slice(format!("\\u{:04x}", u32::from(control)).as_bytes());
            }
            other => {
                let mut buffer = [0u8; 4];
                out.extend_from_slice(other.encode_utf8(&mut buffer).as_bytes());
            }
        }
    }
    out.push(b'"');
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn object_key_order_does_not_change_the_bytes() {
        let left: Value = serde_json::from_str(r#"{"b":1,"a":{"y":true,"x":null}}"#).unwrap();
        let right: Value = serde_json::from_str(r#"{"a":{"x":null,"y":true},"b":1}"#).unwrap();
        assert_eq!(to_vec(&left), to_vec(&right));
        assert_eq!(to_vec(&left), br#"{"a":{"x":null,"y":true},"b":1}"#);
    }

    #[test]
    fn output_is_valid_json_that_round_trips() {
        let original = json!({
            "text": "quote \" backslash \\ newline \n tab \t bell \u{07} snowman \u{2603}",
            "numbers": [0, -7, 3.5, 1e21, u64::MAX],
            "nested": {"empty_array": [], "empty_object": {}, "flag": false}
        });
        let parsed: Value = serde_json::from_slice(&to_vec(&original)).unwrap();
        assert_eq!(parsed, original);
    }

    #[test]
    fn array_order_is_significant() {
        assert_ne!(to_vec(&json!([1, 2])), to_vec(&json!([2, 1])));
    }
}
