//! Typed handles onto channels.
//!
//! State is stored as JSON so schemas can be persisted, fingerprinted and
//! migrated, but application code should not shuttle `serde_json::Value`s by hand.
//! A [`Channel<T>`] pairs a channel name with a Rust type: it serializes values
//! into [`Update`]s and reads typed values back out of a [`State`].
//!
//! The handle does not know the schema, so it cannot prove that `T` matches the
//! channel's declared [`ValueType`](crate::ValueType); a mismatch surfaces as an
//! [`UpdateError::TypeMismatch`](crate::UpdateError) when the update is applied,
//! or as [`ChannelError::Decode`] when a value is read.

use std::marker::PhantomData;

use serde::{Serialize, de::DeserializeOwned};

use crate::{
    ids::{ChannelName, NameError},
    state::State,
    update::Update,
};

/// A typed value could not be converted to or from JSON.
#[derive(Debug, thiserror::Error)]
pub enum ChannelError {
    /// The value could not be serialized.
    #[error("channel `{channel}`: cannot encode value: {source}")]
    Encode {
        /// The channel.
        channel: ChannelName,
        /// The serializer's error.
        #[source]
        source: serde_json::Error,
    },
    /// The stored value does not deserialize into the handle's type.
    #[error("channel `{channel}`: stored value does not decode: {source}")]
    Decode {
        /// The channel.
        channel: ChannelName,
        /// The deserializer's error.
        #[source]
        source: serde_json::Error,
    },
}

/// A channel name tied to the Rust type `T` its values are read and written as.
///
/// For channels with the [`Append`](crate::ReducerKind::Append) or
/// [`Union`](crate::ReducerKind::Union) reducers, `T` is the collection type
/// (for example `Vec<String>`) and an update carries only the *new* items.
#[derive(Debug)]
pub struct Channel<T> {
    name: ChannelName,
    marker: PhantomData<fn() -> T>,
}

impl<T> Clone for Channel<T> {
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            marker: PhantomData,
        }
    }
}

impl<T> Channel<T> {
    /// Creates a handle for the channel called `name`.
    ///
    /// # Errors
    ///
    /// Returns [`NameError`] if `name` is not a valid channel name.
    pub fn new(name: impl Into<String>) -> Result<Self, NameError> {
        Ok(Self {
            name: ChannelName::new(name)?,
            marker: PhantomData,
        })
    }

    /// The channel's name.
    #[must_use]
    pub fn name(&self) -> &ChannelName {
        &self.name
    }
}

impl<T: Serialize + DeserializeOwned> Channel<T> {
    /// Serializes `value` into an update for this channel.
    ///
    /// # Errors
    ///
    /// Returns [`ChannelError::Encode`] if `value` cannot be serialized.
    pub fn update(&self, value: &T) -> Result<Update, ChannelError> {
        let encoded = serde_json::to_value(value).map_err(|source| ChannelError::Encode {
            channel: self.name.clone(),
            source,
        })?;
        Ok(Update::new(self.name.clone(), encoded))
    }

    /// Reads this channel from `state`; `None` if it holds no value yet.
    ///
    /// # Errors
    ///
    /// Returns [`ChannelError::Decode`] if the stored value does not deserialize
    /// into `T`.
    pub fn read(&self, state: &State) -> Result<Option<T>, ChannelError> {
        state
            .value(self.name.as_str())
            .map(|stored| {
                T::deserialize(stored).map_err(|source| ChannelError::Decode {
                    channel: self.name.clone(),
                    source,
                })
            })
            .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ids::WriterId, reducer::ReducerKind, schema::Schema, update::UpdateBatch,
        value_type::ValueType,
    };
    use serde::Deserialize;

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Note {
        author: String,
        text: String,
    }

    fn schema() -> Schema {
        Schema::builder("agent", 1)
            .channel("notes", ValueType::Array, ReducerKind::Append)
            .channel("turns", ValueType::Integer, ReducerKind::SumI64)
            .channel("owner", ValueType::String, ReducerKind::Overwrite)
            .build()
            .unwrap()
    }

    #[test]
    fn typed_round_trip_through_a_batch() {
        let schema = schema();
        let notes: Channel<Vec<Note>> = Channel::new("notes").unwrap();
        let turns: Channel<i64> = Channel::new("turns").unwrap();
        let owner: Channel<String> = Channel::new("owner").unwrap();

        let note = Note {
            author: "ada".into(),
            text: "hello".into(),
        };
        let batch = UpdateBatch::from_writer_outputs([(
            WriterId::new("w").unwrap(),
            vec![
                notes.update(&vec![note]).unwrap(),
                turns.update(&2).unwrap(),
                owner.update(&"ada".to_owned()).unwrap(),
            ],
        )])
        .unwrap();
        let (state, _) = State::new(&schema).apply(&schema, &batch).unwrap();

        assert_eq!(turns.read(&state).unwrap(), Some(2));
        assert_eq!(owner.read(&state).unwrap().as_deref(), Some("ada"));
        let read_notes = notes.read(&state).unwrap().unwrap();
        assert_eq!(read_notes.len(), 1);
        assert_eq!(read_notes.first().unwrap().text, "hello");
    }

    #[test]
    fn unwritten_channels_read_as_none() {
        let schema = schema();
        let owner: Channel<String> = Channel::new("owner").unwrap();
        assert_eq!(owner.read(&State::new(&schema)).unwrap(), None);
    }

    #[test]
    fn decoding_into_the_wrong_type_is_an_error() {
        let schema = schema();
        let as_string: Channel<String> = Channel::new("turns").unwrap();
        assert!(matches!(
            as_string.read(&State::new(&schema)),
            Err(ChannelError::Decode { .. })
        ));
    }

    #[test]
    fn invalid_names_are_rejected() {
        assert!(Channel::<i64>::new("").is_err());
    }
}
