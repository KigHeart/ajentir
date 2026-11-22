//! Reducers: how concurrent updates to one channel are combined.
//!
//! A reducer folds the updates that reach a channel in a single batch into the
//! channel's current value. The set of reducers is closed and serializable on
//! purpose: a schema records which reducer each channel uses, and that record is
//! part of the schema fingerprint stored in every checkpoint, so a checkpoint can
//! never be resumed under different merge semantics without anyone noticing.
//!
//! Updates arrive already ordered by `(writer, sequence)` (see
//! [`UpdateBatch`](crate::UpdateBatch)), never by completion time, so the result
//! of a fold depends only on *what* was written and *by whom*.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{canonical, value_type::ValueType};

/// A channel's merge rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReducerKind {
    /// The channel holds the last value written. At most one update per batch is
    /// allowed; two or more are a [`ReduceError::Conflict`] rather than a silent
    /// race. Works with any [`ValueType`].
    Overwrite,
    /// The channel is an array; each update is an array whose items are appended
    /// in writer order. Requires [`ValueType::Array`].
    Append,
    /// The channel is an array treated as a set: items from the current value and
    /// every update are merged, deduplicated by canonical JSON, and kept sorted by
    /// that encoding. Order-independent by construction. Requires
    /// [`ValueType::Array`].
    Union,
    /// The channel is an integer; updates are added with overflow checking.
    /// Requires [`ValueType::Integer`].
    SumI64,
    /// The channel keeps the largest integer seen. Requires [`ValueType::Integer`].
    MaxI64,
    /// The channel keeps the smallest integer seen. Requires [`ValueType::Integer`].
    MinI64,
}

/// A fold that could not be completed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReduceError {
    /// More than one update reached an [`ReducerKind::Overwrite`] channel in a batch.
    #[error(
        "{updates} updates reached an overwrite channel in one batch; only one writer may set it per step"
    )]
    Conflict {
        /// How many updates were in the batch for this channel.
        updates: usize,
    },
    /// An array was required.
    #[error("expected a JSON array")]
    NotArray,
    /// An integer representable as `i64` was required.
    #[error("expected an integer representable as i64")]
    NotInteger,
    /// Summing exceeded the `i64` range.
    #[error("integer overflow while summing")]
    Overflow,
}

impl ReducerKind {
    /// A stable lowercase name, also used when hashing schemas.
    #[must_use]
    pub fn tag(self) -> &'static str {
        match self {
            Self::Overwrite => "overwrite",
            Self::Append => "append",
            Self::Union => "union",
            Self::SumI64 => "sum_i64",
            Self::MaxI64 => "max_i64",
            Self::MinI64 => "min_i64",
        }
    }

    /// Whether this reducer can operate on a channel declared as `value_type`.
    #[must_use]
    pub fn supports(self, value_type: ValueType) -> bool {
        match self {
            Self::Overwrite => true,
            Self::Append | Self::Union => value_type == ValueType::Array,
            Self::SumI64 | Self::MaxI64 | Self::MinI64 => value_type == ValueType::Integer,
        }
    }

    /// The value a fresh channel starts with, if the reducer has a neutral element.
    ///
    /// `Overwrite`, `MaxI64` and `MinI64` have none, so those channels start absent.
    #[must_use]
    pub fn identity(self) -> Option<Value> {
        match self {
            Self::Append | Self::Union => Some(Value::Array(Vec::new())),
            Self::SumI64 => Some(Value::from(0_i64)),
            Self::Overwrite | Self::MaxI64 | Self::MinI64 => None,
        }
    }

    /// Folds `updates` (already in deterministic order) into `current`.
    ///
    /// With no updates the current value is returned unchanged.
    ///
    /// # Errors
    ///
    /// Returns [`ReduceError`] if the updates conflict, have the wrong shape, or
    /// overflow.
    pub fn fold(
        self,
        current: Option<&Value>,
        updates: &[&Value],
    ) -> Result<Option<Value>, ReduceError> {
        if updates.is_empty() {
            return Ok(current.cloned());
        }
        match self {
            Self::Overwrite => match updates {
                [only] => Ok(Some((*only).clone())),
                many => Err(ReduceError::Conflict {
                    updates: many.len(),
                }),
            },
            Self::Append => {
                let mut items: Vec<Value> = match current {
                    Some(value) => items_of(value)?.clone(),
                    None => Vec::new(),
                };
                for update in updates {
                    items.extend(items_of(update)?.iter().cloned());
                }
                Ok(Some(Value::Array(items)))
            }
            Self::Union => {
                let mut keyed: Vec<(Vec<u8>, Value)> = Vec::new();
                for source in current.into_iter().chain(updates.iter().copied()) {
                    for item in items_of(source)? {
                        keyed.push((canonical::to_vec(item), item.clone()));
                    }
                }
                keyed.sort_by(|left, right| left.0.cmp(&right.0));
                keyed.dedup_by(|left, right| left.0 == right.0);
                Ok(Some(Value::Array(
                    keyed.into_iter().map(|(_, item)| item).collect(),
                )))
            }
            Self::SumI64 => {
                let mut total = match current {
                    Some(value) => integer_of(value)?,
                    None => 0,
                };
                for update in updates {
                    total = total
                        .checked_add(integer_of(update)?)
                        .ok_or(ReduceError::Overflow)?;
                }
                Ok(Some(Value::from(total)))
            }
            Self::MaxI64 => extremum(current, updates, i64::max),
            Self::MinI64 => extremum(current, updates, i64::min),
        }
    }
}

fn items_of(value: &Value) -> Result<&Vec<Value>, ReduceError> {
    value.as_array().ok_or(ReduceError::NotArray)
}

fn integer_of(value: &Value) -> Result<i64, ReduceError> {
    value.as_i64().ok_or(ReduceError::NotInteger)
}

fn extremum(
    current: Option<&Value>,
    updates: &[&Value],
    pick: fn(i64, i64) -> i64,
) -> Result<Option<Value>, ReduceError> {
    let mut best: Option<i64> = None;
    for value in current.into_iter().chain(updates.iter().copied()) {
        let number = integer_of(value)?;
        best = Some(match best {
            Some(existing) => pick(existing, number),
            None => number,
        });
    }
    Ok(best.map(Value::from))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fold(
        kind: ReducerKind,
        current: Option<Value>,
        updates: &[Value],
    ) -> Result<Option<Value>, ReduceError> {
        let refs: Vec<&Value> = updates.iter().collect();
        kind.fold(current.as_ref(), &refs)
    }

    #[test]
    fn no_updates_leaves_the_value_alone() {
        for kind in [
            ReducerKind::Overwrite,
            ReducerKind::Append,
            ReducerKind::Union,
            ReducerKind::SumI64,
            ReducerKind::MaxI64,
            ReducerKind::MinI64,
        ] {
            assert_eq!(fold(kind, None, &[]), Ok(None));
            assert_eq!(fold(kind, Some(json!(5)), &[]), Ok(Some(json!(5))));
        }
    }

    #[test]
    fn overwrite_takes_a_single_update_and_rejects_two() {
        assert_eq!(
            fold(ReducerKind::Overwrite, Some(json!("old")), &[json!("new")]),
            Ok(Some(json!("new")))
        );
        assert_eq!(
            fold(ReducerKind::Overwrite, None, &[json!(1), json!(2)]),
            Err(ReduceError::Conflict { updates: 2 })
        );
    }

    #[test]
    fn append_concatenates_in_the_order_given() {
        assert_eq!(
            fold(
                ReducerKind::Append,
                Some(json!(["a"])),
                &[json!(["b", "c"]), json!(["d"])]
            ),
            Ok(Some(json!(["a", "b", "c", "d"])))
        );
        assert_eq!(
            fold(ReducerKind::Append, None, &[json!(["x"])]),
            Ok(Some(json!(["x"])))
        );
        assert_eq!(
            fold(
                ReducerKind::Append,
                Some(json!(["a"])),
                &[json!("not-array")]
            ),
            Err(ReduceError::NotArray)
        );
    }

    #[test]
    fn union_is_order_independent_and_deduplicated() {
        let forward = fold(
            ReducerKind::Union,
            Some(json!(["b"])),
            &[json!(["c", "a"]), json!(["a", "d"])],
        );
        let backward = fold(
            ReducerKind::Union,
            Some(json!(["b"])),
            &[json!(["a", "d"]), json!(["a", "c"])],
        );
        assert_eq!(forward, backward);
        assert_eq!(forward, Ok(Some(json!(["a", "b", "c", "d"]))));
    }

    #[test]
    fn union_treats_object_key_order_as_the_same_item() {
        let first: Value = serde_json::from_str(r#"{"a":1,"b":2}"#).unwrap();
        let second: Value = serde_json::from_str(r#"{"b":2,"a":1}"#).unwrap();
        let merged = fold(ReducerKind::Union, None, &[json!([first]), json!([second])])
            .unwrap()
            .unwrap();
        assert_eq!(merged.as_array().map(Vec::len), Some(1));
    }

    #[test]
    fn sum_adds_and_detects_overflow() {
        assert_eq!(
            fold(ReducerKind::SumI64, Some(json!(10)), &[json!(5), json!(-3)]),
            Ok(Some(json!(12)))
        );
        assert_eq!(
            fold(ReducerKind::SumI64, None, &[json!(4)]),
            Ok(Some(json!(4)))
        );
        assert_eq!(
            fold(ReducerKind::SumI64, Some(json!(i64::MAX)), &[json!(1)]),
            Err(ReduceError::Overflow)
        );
        assert_eq!(
            fold(ReducerKind::SumI64, None, &[json!(1.5)]),
            Err(ReduceError::NotInteger)
        );
    }

    #[test]
    fn max_and_min_consider_the_current_value() {
        assert_eq!(
            fold(ReducerKind::MaxI64, Some(json!(7)), &[json!(3), json!(9)]),
            Ok(Some(json!(9)))
        );
        assert_eq!(
            fold(ReducerKind::MaxI64, Some(json!(7)), &[json!(3)]),
            Ok(Some(json!(7)))
        );
        assert_eq!(
            fold(ReducerKind::MinI64, None, &[json!(3), json!(-2)]),
            Ok(Some(json!(-2)))
        );
    }

    #[test]
    fn support_matrix_matches_the_documentation() {
        assert!(ReducerKind::Overwrite.supports(ValueType::Any));
        assert!(ReducerKind::Overwrite.supports(ValueType::String));
        assert!(ReducerKind::Append.supports(ValueType::Array));
        assert!(!ReducerKind::Append.supports(ValueType::Object));
        assert!(ReducerKind::Union.supports(ValueType::Array));
        assert!(ReducerKind::SumI64.supports(ValueType::Integer));
        assert!(!ReducerKind::SumI64.supports(ValueType::Number));
        assert!(!ReducerKind::MaxI64.supports(ValueType::Any));
    }

    #[test]
    fn identity_elements() {
        assert_eq!(ReducerKind::Append.identity(), Some(json!([])));
        assert_eq!(ReducerKind::Union.identity(), Some(json!([])));
        assert_eq!(ReducerKind::SumI64.identity(), Some(json!(0)));
        assert_eq!(ReducerKind::Overwrite.identity(), None);
        assert_eq!(ReducerKind::MaxI64.identity(), None);
    }
}
