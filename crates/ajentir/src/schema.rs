//! Versioned schemas: the declared shape of an agent's state.
//!
//! A [`Schema`] names its channels and, for each, the JSON type it holds and the
//! [`ReducerKind`] that merges updates. Schemas are immutable once built. Every
//! checkpoint records a [`SchemaRef`] — name, version, and a fingerprint of the
//! full definition — so state is only ever resumed against the schema it was
//! written under, or deliberately moved to a newer one with a
//! [`Migration`](crate::Migration).

use std::{collections::BTreeMap, fmt};

use serde::{Deserialize, Serialize};

use crate::{
    digest::{Digest, DigestBuilder},
    ids::{ChannelName, NameError, SchemaName},
    reducer::ReducerKind,
    value_type::ValueType,
};

/// A schema's version number. Versions of one schema name increase monotonically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SchemaVersion(u32);

impl SchemaVersion {
    /// Wraps a version number.
    #[must_use]
    pub const fn new(version: u32) -> Self {
        Self(version)
    }

    /// The version number.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl fmt::Display for SchemaVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "v{}", self.0)
    }
}

/// A reducer was paired with a value type it cannot operate on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("reducer {reducer:?} cannot be used with value type {value_type}")]
pub struct IncompatibleSpec {
    /// The reducer that was requested.
    pub reducer: ReducerKind,
    /// The value type it was paired with.
    pub value_type: ValueType,
}

/// What a channel holds and how updates to it merge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelSpec {
    value_type: ValueType,
    reducer: ReducerKind,
}

impl ChannelSpec {
    /// Pairs a value type with a reducer.
    ///
    /// # Errors
    ///
    /// Returns [`IncompatibleSpec`] if the reducer cannot operate on the type, for
    /// example [`ReducerKind::SumI64`] on a string channel.
    pub fn new(value_type: ValueType, reducer: ReducerKind) -> Result<Self, IncompatibleSpec> {
        if reducer.supports(value_type) {
            Ok(Self {
                value_type,
                reducer,
            })
        } else {
            Err(IncompatibleSpec {
                reducer,
                value_type,
            })
        }
    }

    /// The JSON type the channel holds.
    #[must_use]
    pub fn value_type(&self) -> ValueType {
        self.value_type
    }

    /// The merge rule for updates.
    #[must_use]
    pub fn reducer(&self) -> ReducerKind {
        self.reducer
    }
}

/// Why a schema could not be built.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SchemaError {
    /// The schema name or a channel name was invalid.
    #[error(transparent)]
    Name(#[from] NameError),
    /// A schema must declare at least one channel.
    #[error("a schema needs at least one channel")]
    NoChannels,
    /// Two channels share a name.
    #[error("channel `{0}` is declared twice")]
    DuplicateChannel(ChannelName),
    /// A channel pairs a reducer with a type it cannot operate on.
    #[error("channel `{channel}`: {source}")]
    Incompatible {
        /// The offending channel.
        channel: ChannelName,
        /// The mismatch.
        #[source]
        source: IncompatibleSpec,
    },
}

/// Identifies the exact schema a checkpoint was written under.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaRef {
    /// The schema's name.
    pub name: SchemaName,
    /// The schema's version.
    pub version: SchemaVersion,
    /// Fingerprint of the full definition; two schemas with equal fingerprints
    /// declare identical channels, types and reducers.
    pub fingerprint: Digest,
}

impl fmt::Display for SchemaRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} ({})", self.name, self.version, self.fingerprint)
    }
}

/// An immutable, validated state definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "SchemaWire")]
pub struct Schema {
    name: SchemaName,
    version: SchemaVersion,
    channels: BTreeMap<ChannelName, ChannelSpec>,
}

#[derive(Deserialize)]
struct SchemaWire {
    name: SchemaName,
    version: SchemaVersion,
    channels: BTreeMap<ChannelName, ChannelSpec>,
}

impl TryFrom<SchemaWire> for Schema {
    type Error = SchemaError;

    fn try_from(wire: SchemaWire) -> Result<Self, SchemaError> {
        Self::from_channels(wire.name, wire.version, wire.channels)
    }
}

impl Schema {
    /// Starts building a schema.
    #[must_use]
    pub fn builder(name: impl Into<String>, version: u32) -> SchemaBuilder {
        SchemaBuilder {
            name: name.into(),
            version: SchemaVersion::new(version),
            channels: Vec::new(),
        }
    }

    /// Builds a schema from an already-validated channel map.
    ///
    /// # Errors
    ///
    /// Returns [`SchemaError::NoChannels`] for an empty map and
    /// [`SchemaError::Incompatible`] if any channel pairs a reducer with a type it
    /// cannot operate on (possible when the map came from untrusted data).
    pub fn from_channels(
        name: SchemaName,
        version: SchemaVersion,
        channels: BTreeMap<ChannelName, ChannelSpec>,
    ) -> Result<Self, SchemaError> {
        if channels.is_empty() {
            return Err(SchemaError::NoChannels);
        }
        for (channel, spec) in &channels {
            if !spec.reducer.supports(spec.value_type) {
                return Err(SchemaError::Incompatible {
                    channel: channel.clone(),
                    source: IncompatibleSpec {
                        reducer: spec.reducer,
                        value_type: spec.value_type,
                    },
                });
            }
        }
        Ok(Self {
            name,
            version,
            channels,
        })
    }

    /// The schema's name.
    #[must_use]
    pub fn name(&self) -> &SchemaName {
        &self.name
    }

    /// The schema's version.
    #[must_use]
    pub fn version(&self) -> SchemaVersion {
        self.version
    }

    /// All channels, ordered by name.
    #[must_use]
    pub fn channels(&self) -> &BTreeMap<ChannelName, ChannelSpec> {
        &self.channels
    }

    /// Looks up one channel's spec.
    #[must_use]
    pub fn channel(&self, name: &str) -> Option<&ChannelSpec> {
        self.channels.get(name)
    }

    /// A digest over the name, version and every channel's name, type and reducer.
    #[must_use]
    pub fn fingerprint(&self) -> Digest {
        let mut builder = DigestBuilder::new("ajentir.schema.v1");
        builder
            .str(self.name.as_str())
            .u64(u64::from(self.version.get()))
            .u64(u64::try_from(self.channels.len()).unwrap_or(u64::MAX));
        for (channel, spec) in &self.channels {
            builder
                .str(channel.as_str())
                .str(spec.value_type.tag())
                .str(spec.reducer.tag());
        }
        builder.finish()
    }

    /// The reference recorded in checkpoints written under this schema.
    #[must_use]
    pub fn reference(&self) -> SchemaRef {
        SchemaRef {
            name: self.name.clone(),
            version: self.version,
            fingerprint: self.fingerprint(),
        }
    }
}

/// Collects channel declarations and validates them all at once in [`build`](Self::build).
#[derive(Debug, Clone)]
pub struct SchemaBuilder {
    name: String,
    version: SchemaVersion,
    channels: Vec<(String, ValueType, ReducerKind)>,
}

impl SchemaBuilder {
    /// Declares a channel.
    #[must_use]
    pub fn channel(
        mut self,
        name: impl Into<String>,
        value_type: ValueType,
        reducer: ReducerKind,
    ) -> Self {
        self.channels.push((name.into(), value_type, reducer));
        self
    }

    /// Validates every declaration and builds the schema.
    ///
    /// # Errors
    ///
    /// Returns [`SchemaError`] for an invalid name, no channels, a duplicate
    /// channel, or an incompatible reducer/type pair.
    pub fn build(self) -> Result<Schema, SchemaError> {
        let name = SchemaName::new(self.name)?;
        let mut channels = BTreeMap::new();
        for (raw_name, value_type, reducer) in self.channels {
            let channel = ChannelName::new(raw_name)?;
            let spec = ChannelSpec::new(value_type, reducer).map_err(|source| {
                SchemaError::Incompatible {
                    channel: channel.clone(),
                    source,
                }
            })?;
            if channels.insert(channel.clone(), spec).is_some() {
                return Err(SchemaError::DuplicateChannel(channel));
            }
        }
        Schema::from_channels(name, self.version, channels)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Schema {
        Schema::builder("agent", 1)
            .channel("messages", ValueType::Array, ReducerKind::Append)
            .channel("turns", ValueType::Integer, ReducerKind::SumI64)
            .build()
            .unwrap()
    }

    #[test]
    fn builds_and_exposes_channels() {
        let schema = sample();
        assert_eq!(schema.name().as_str(), "agent");
        assert_eq!(schema.version(), SchemaVersion::new(1));
        assert_eq!(schema.channels().len(), 2);
        assert_eq!(
            schema.channel("turns").unwrap().reducer(),
            ReducerKind::SumI64
        );
        assert!(schema.channel("nope").is_none());
    }

    #[test]
    fn declaration_order_does_not_change_the_fingerprint() {
        let reversed = Schema::builder("agent", 1)
            .channel("turns", ValueType::Integer, ReducerKind::SumI64)
            .channel("messages", ValueType::Array, ReducerKind::Append)
            .build()
            .unwrap();
        assert_eq!(sample().fingerprint(), reversed.fingerprint());
    }

    #[test]
    fn any_semantic_change_changes_the_fingerprint() {
        let base = sample().fingerprint();
        let other_version = Schema::builder("agent", 2)
            .channel("messages", ValueType::Array, ReducerKind::Append)
            .channel("turns", ValueType::Integer, ReducerKind::SumI64)
            .build()
            .unwrap();
        let other_reducer = Schema::builder("agent", 1)
            .channel("messages", ValueType::Array, ReducerKind::Union)
            .channel("turns", ValueType::Integer, ReducerKind::SumI64)
            .build()
            .unwrap();
        let other_name = Schema::builder("agent2", 1)
            .channel("messages", ValueType::Array, ReducerKind::Append)
            .channel("turns", ValueType::Integer, ReducerKind::SumI64)
            .build()
            .unwrap();
        assert_ne!(base, other_version.fingerprint());
        assert_ne!(base, other_reducer.fingerprint());
        assert_ne!(base, other_name.fingerprint());
    }

    #[test]
    fn rejects_bad_declarations() {
        assert_eq!(
            Schema::builder("agent", 1).build().unwrap_err(),
            SchemaError::NoChannels
        );
        assert!(matches!(
            Schema::builder("", 1)
                .channel("a", ValueType::Any, ReducerKind::Overwrite)
                .build(),
            Err(SchemaError::Name(_))
        ));
        assert!(matches!(
            Schema::builder("agent", 1)
                .channel("a", ValueType::Any, ReducerKind::Overwrite)
                .channel("a", ValueType::Bool, ReducerKind::Overwrite)
                .build(),
            Err(SchemaError::DuplicateChannel(name)) if name.as_str() == "a"
        ));
        assert!(matches!(
            Schema::builder("agent", 1)
                .channel("turns", ValueType::String, ReducerKind::SumI64)
                .build(),
            Err(SchemaError::Incompatible { .. })
        ));
    }

    #[test]
    fn serde_round_trip_and_validation_on_load() {
        let schema = sample();
        let json = serde_json::to_string(&schema).unwrap();
        assert_eq!(serde_json::from_str::<Schema>(&json).unwrap(), schema);

        let tampered = json.replace("\"sum_i64\"", "\"append\"");
        assert!(serde_json::from_str::<Schema>(&tampered).is_err());
        let empty = r#"{"name":"agent","version":1,"channels":{}}"#;
        assert!(serde_json::from_str::<Schema>(empty).is_err());
    }

    #[test]
    fn reference_matches_the_schema() {
        let schema = sample();
        let reference = schema.reference();
        assert_eq!(reference.fingerprint, schema.fingerprint());
        assert_eq!(reference.version, schema.version());
    }
}
