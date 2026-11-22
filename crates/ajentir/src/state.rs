//! Channel state and the pure transition that applies an update batch.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    digest::{Digest, DigestBuilder},
    ids::ChannelName,
    reducer::ReduceError,
    schema::Schema,
    update::UpdateBatch,
    value_type::{ValueType, describe},
};

/// Why a batch could not be applied. Application is all-or-nothing: on error the
/// state the batch was applied to is untouched.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UpdateError {
    /// An update targeted a channel the schema does not declare.
    #[error("channel `{0}` is not declared in the schema")]
    UnknownChannel(ChannelName),
    /// An update carried a value of the wrong JSON type.
    #[error("channel `{channel}` holds {expected} values but an update carried {found}")]
    TypeMismatch {
        /// The channel.
        channel: ChannelName,
        /// The type the schema declares.
        expected: ValueType,
        /// The JSON kind that was supplied.
        found: &'static str,
    },
    /// The channel's reducer rejected the updates.
    #[error("channel `{channel}`: {source}")]
    Reduce {
        /// The channel.
        channel: ChannelName,
        /// The reducer's complaint.
        #[source]
        source: ReduceError,
    },
}

/// Why a state does not conform to a schema.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StateError {
    /// The state stores a value for an undeclared channel.
    #[error("state has a value for channel `{0}`, which the schema does not declare")]
    UnknownChannel(ChannelName),
    /// A stored value has the wrong JSON type.
    #[error("channel `{channel}` holds {expected} values but the state stores {found}")]
    TypeMismatch {
        /// The channel.
        channel: ChannelName,
        /// The type the schema declares.
        expected: ValueType,
        /// The JSON kind that is stored.
        found: &'static str,
    },
    /// A declared channel has no version counter.
    #[error("schema channel `{0}` has no version entry in the state")]
    MissingVersion(ChannelName),
    /// The state tracks a version for an undeclared channel.
    #[error("state tracks a version for channel `{0}`, which the schema does not declare")]
    ExtraVersion(ChannelName),
}

/// What a successful [`State::apply`] changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedBatch {
    changed: BTreeMap<ChannelName, u64>,
    batch: Digest,
}

impl AppliedBatch {
    /// Channels whose value changed, with their new version. A channel written
    /// with a value equal to its current one is *not* listed.
    #[must_use]
    pub fn changed(&self) -> &BTreeMap<ChannelName, u64> {
        &self.changed
    }

    /// Digest of the batch that was applied.
    #[must_use]
    pub fn batch_digest(&self) -> &Digest {
        &self.batch
    }
}

/// A snapshot of every channel's value and version.
///
/// A channel's version is the number of applied batches that changed it. Versions
/// let a scheduler decide which work a change should trigger without comparing
/// values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    values: BTreeMap<ChannelName, Value>,
    versions: BTreeMap<ChannelName, u64>,
}

impl State {
    /// The initial state under `schema`: every channel at version 0, holding its
    /// reducer's identity value where one exists.
    #[must_use]
    pub fn new(schema: &Schema) -> Self {
        let mut values = BTreeMap::new();
        let mut versions = BTreeMap::new();
        for (channel, spec) in schema.channels() {
            versions.insert(channel.clone(), 0);
            if let Some(identity) = spec.reducer().identity() {
                values.insert(channel.clone(), identity);
            }
        }
        Self { values, versions }
    }

    /// Assembles a state from parts; the caller guarantees they conform to the
    /// schema they will be checked against.
    pub(crate) fn from_parts(
        values: BTreeMap<ChannelName, Value>,
        versions: BTreeMap<ChannelName, u64>,
    ) -> Self {
        Self { values, versions }
    }

    /// The current value of a channel, or `None` if it has never been written and
    /// its reducer has no identity.
    #[must_use]
    pub fn value(&self, channel: &str) -> Option<&Value> {
        self.values.get(channel)
    }

    /// The current version of a channel.
    #[must_use]
    pub fn version(&self, channel: &str) -> Option<u64> {
        self.versions.get(channel).copied()
    }

    /// All stored values.
    #[must_use]
    pub fn values(&self) -> &BTreeMap<ChannelName, Value> {
        &self.values
    }

    /// All channel versions.
    #[must_use]
    pub fn versions(&self) -> &BTreeMap<ChannelName, u64> {
        &self.versions
    }

    /// Checks that every stored value and version belongs to `schema` and has the
    /// declared type.
    ///
    /// # Errors
    ///
    /// Returns the first [`StateError`] found.
    pub fn conforms_to(&self, schema: &Schema) -> Result<(), StateError> {
        for (channel, value) in &self.values {
            let spec = schema
                .channel(channel.as_str())
                .ok_or_else(|| StateError::UnknownChannel(channel.clone()))?;
            if !spec.value_type().accepts(value) {
                return Err(StateError::TypeMismatch {
                    channel: channel.clone(),
                    expected: spec.value_type(),
                    found: describe(value),
                });
            }
        }
        for channel in self.versions.keys() {
            if schema.channel(channel.as_str()).is_none() {
                return Err(StateError::ExtraVersion(channel.clone()));
            }
        }
        for channel in schema.channels().keys() {
            if !self.versions.contains_key(channel) {
                return Err(StateError::MissingVersion(channel.clone()));
            }
        }
        Ok(())
    }

    /// Applies `batch` and returns the resulting state; `self` is not modified.
    ///
    /// Writes to each channel are folded with that channel's reducer in batch
    /// order, so the result depends only on the set of writes. Every write is
    /// validated against the schema before any state is produced.
    ///
    /// # Errors
    ///
    /// Returns [`UpdateError`] if a write targets an unknown channel, carries the
    /// wrong type, or is rejected by the channel's reducer.
    pub fn apply(
        &self,
        schema: &Schema,
        batch: &UpdateBatch,
    ) -> Result<(State, AppliedBatch), UpdateError> {
        let mut grouped: BTreeMap<&ChannelName, Vec<&Value>> = BTreeMap::new();
        for write in batch.writes() {
            let update = write.update();
            let spec = schema
                .channel(update.channel().as_str())
                .ok_or_else(|| UpdateError::UnknownChannel(update.channel().clone()))?;
            if !spec.value_type().accepts(update.value()) {
                return Err(UpdateError::TypeMismatch {
                    channel: update.channel().clone(),
                    expected: spec.value_type(),
                    found: describe(update.value()),
                });
            }
            grouped
                .entry(update.channel())
                .or_default()
                .push(update.value());
        }

        let mut next = self.clone();
        let mut changed = BTreeMap::new();
        for (channel, updates) in grouped {
            let spec = schema
                .channel(channel.as_str())
                .ok_or_else(|| UpdateError::UnknownChannel(channel.clone()))?;
            let current = self.values.get(channel);
            let folded =
                spec.reducer()
                    .fold(current, &updates)
                    .map_err(|source| UpdateError::Reduce {
                        channel: channel.clone(),
                        source,
                    })?;
            if folded.as_ref() == current {
                continue;
            }
            match folded {
                Some(value) => {
                    next.values.insert(channel.clone(), value);
                }
                None => {
                    next.values.remove(channel);
                }
            }
            let version = next.versions.entry(channel.clone()).or_insert(0);
            *version = version.saturating_add(1);
            changed.insert(channel.clone(), *version);
        }
        Ok((
            next,
            AppliedBatch {
                changed,
                batch: batch.digest(),
            },
        ))
    }

    /// A digest over every value and version.
    #[must_use]
    pub fn digest(&self) -> Digest {
        let mut builder = DigestBuilder::new("ajentir.state.v1");
        builder.u64(u64::try_from(self.values.len()).unwrap_or(u64::MAX));
        for (channel, value) in &self.values {
            builder.str(channel.as_str()).value(value);
        }
        builder.u64(u64::try_from(self.versions.len()).unwrap_or(u64::MAX));
        for (channel, version) in &self.versions {
            builder.str(channel.as_str()).u64(*version);
        }
        builder.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ids::WriterId,
        reducer::ReducerKind,
        update::{Update, Write},
    };
    use serde_json::json;

    fn schema() -> Schema {
        Schema::builder("agent", 1)
            .channel("messages", ValueType::Array, ReducerKind::Append)
            .channel("turns", ValueType::Integer, ReducerKind::SumI64)
            .channel("status", ValueType::String, ReducerKind::Overwrite)
            .channel("best", ValueType::Integer, ReducerKind::MaxI64)
            .build()
            .unwrap()
    }

    fn writer(name: &str) -> WriterId {
        WriterId::new(name).unwrap()
    }

    fn update(channel: &str, value: Value) -> Update {
        Update::named(channel, value).unwrap()
    }

    #[test]
    fn a_fresh_state_holds_identities_and_zero_versions() {
        let state = State::new(&schema());
        assert_eq!(state.value("messages"), Some(&json!([])));
        assert_eq!(state.value("turns"), Some(&json!(0)));
        assert_eq!(state.value("status"), None);
        assert_eq!(state.value("best"), None);
        assert_eq!(state.version("status"), Some(0));
        assert!(state.conforms_to(&schema()).is_ok());
    }

    #[test]
    fn apply_folds_with_each_channels_reducer() {
        let schema = schema();
        let batch = UpdateBatch::from_writer_outputs([
            (
                writer("a"),
                vec![update("messages", json!(["hi"])), update("turns", json!(1))],
            ),
            (
                writer("b"),
                vec![update("messages", json!(["yo"])), update("turns", json!(2))],
            ),
        ])
        .unwrap();
        let (next, applied) = State::new(&schema).apply(&schema, &batch).unwrap();
        assert_eq!(next.value("messages"), Some(&json!(["hi", "yo"])));
        assert_eq!(next.value("turns"), Some(&json!(3)));
        assert_eq!(next.version("messages"), Some(1));
        assert_eq!(applied.changed().len(), 2);
        assert_eq!(applied.batch_digest(), &batch.digest());
        assert!(next.conforms_to(&schema).is_ok());
    }

    #[test]
    fn arrival_order_cannot_change_the_outcome() {
        let schema = schema();
        let outputs = || {
            vec![
                (writer("w1"), vec![update("messages", json!(["one"]))]),
                (writer("w2"), vec![update("messages", json!(["two"]))]),
                (writer("w3"), vec![update("messages", json!(["three"]))]),
            ]
        };
        let forward = UpdateBatch::from_writer_outputs(outputs()).unwrap();
        let mut reversed_outputs = outputs();
        reversed_outputs.reverse();
        let backward = UpdateBatch::from_writer_outputs(reversed_outputs).unwrap();

        let (state_a, _) = State::new(&schema).apply(&schema, &forward).unwrap();
        let (state_b, _) = State::new(&schema).apply(&schema, &backward).unwrap();
        assert_eq!(state_a, state_b);
        assert_eq!(state_a.digest(), state_b.digest());
        assert_eq!(
            state_a.value("messages"),
            Some(&json!(["one", "two", "three"]))
        );
    }

    #[test]
    fn conflicting_overwrites_fail_instead_of_racing() {
        let schema = schema();
        let batch = UpdateBatch::from_writer_outputs([
            (writer("a"), vec![update("status", json!("done"))]),
            (writer("b"), vec![update("status", json!("failed"))]),
        ])
        .unwrap();
        let err = State::new(&schema).apply(&schema, &batch).unwrap_err();
        assert!(matches!(
            err,
            UpdateError::Reduce {
                source: ReduceError::Conflict { updates: 2 },
                ..
            }
        ));
    }

    #[test]
    fn a_rejected_batch_leaves_the_original_untouched() {
        let schema = schema();
        let before = State::new(&schema);
        let batch = UpdateBatch::from_writer_outputs([(
            writer("a"),
            vec![update("turns", json!(1)), update("ghost", json!(1))],
        )])
        .unwrap();
        let err = before.apply(&schema, &batch).unwrap_err();
        assert_eq!(
            err,
            UpdateError::UnknownChannel(ChannelName::new("ghost").unwrap())
        );
        assert_eq!(before, State::new(&schema));
    }

    #[test]
    fn wrong_types_are_rejected_before_reduction() {
        let schema = schema();
        let batch =
            UpdateBatch::from_writer_outputs([(writer("a"), vec![update("turns", json!("one"))])])
                .unwrap();
        assert!(matches!(
            State::new(&schema).apply(&schema, &batch),
            Err(UpdateError::TypeMismatch {
                found: "string",
                ..
            })
        ));
    }

    #[test]
    fn versions_only_move_when_the_value_changes() {
        let schema = schema();
        let set = |value: &str| {
            UpdateBatch::new(vec![Write::new(
                writer("a"),
                0,
                update("status", json!(value)),
            )])
            .unwrap()
        };
        let (first, applied) = State::new(&schema).apply(&schema, &set("ready")).unwrap();
        assert_eq!(first.version("status"), Some(1));
        assert_eq!(applied.changed().len(), 1);

        let (second, applied) = first.apply(&schema, &set("ready")).unwrap();
        assert_eq!(second.version("status"), Some(1));
        assert!(applied.changed().is_empty());
        assert_eq!(second, first);

        let (third, _) = second.apply(&schema, &set("running")).unwrap();
        assert_eq!(third.version("status"), Some(2));
    }

    #[test]
    fn an_empty_batch_is_a_no_op() {
        let schema = schema();
        let state = State::new(&schema);
        let (next, applied) = state.apply(&schema, &UpdateBatch::empty()).unwrap();
        assert_eq!(next, state);
        assert!(applied.changed().is_empty());
    }

    #[test]
    fn conformance_catches_foreign_and_mistyped_state() {
        let schema = schema();
        let mut values = BTreeMap::new();
        let mut versions = BTreeMap::new();
        for name in ["messages", "turns", "status", "best"] {
            versions.insert(ChannelName::new(name).unwrap(), 0);
        }
        values.insert(ChannelName::new("turns").unwrap(), json!("three"));
        let mistyped = State::from_parts(values.clone(), versions.clone());
        assert!(matches!(
            mistyped.conforms_to(&schema),
            Err(StateError::TypeMismatch { .. })
        ));

        values.clear();
        values.insert(ChannelName::new("ghost").unwrap(), json!(1));
        let foreign = State::from_parts(values, versions.clone());
        assert!(matches!(
            foreign.conforms_to(&schema),
            Err(StateError::UnknownChannel(_))
        ));

        versions.remove("best");
        let missing = State::from_parts(BTreeMap::new(), versions.clone());
        assert!(matches!(
            missing.conforms_to(&schema),
            Err(StateError::MissingVersion(_))
        ));

        versions.insert(ChannelName::new("best").unwrap(), 0);
        versions.insert(ChannelName::new("ghost").unwrap(), 0);
        let extra = State::from_parts(BTreeMap::new(), versions);
        assert!(matches!(
            extra.conforms_to(&schema),
            Err(StateError::ExtraVersion(_))
        ));
    }

    #[test]
    fn digest_covers_values_and_versions() {
        let schema = schema();
        let base = State::new(&schema);
        let batch =
            UpdateBatch::from_writer_outputs([(writer("a"), vec![update("turns", json!(1))])])
                .unwrap();
        let (next, _) = base.apply(&schema, &batch).unwrap();
        assert_ne!(base.digest(), next.digest());
    }
}
