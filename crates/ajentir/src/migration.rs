//! Moving a checkpoint to a newer schema version.
//!
//! A checkpoint is only ever resumed under the exact schema it was written under.
//! When a schema evolves, a [`Migration`] carries a checkpoint across: it lists,
//! step by step, which channels are renamed, dropped or added, and the result must
//! then fit the target schema exactly. Anything the steps do not account for — a
//! target channel with no counterpart, a carried value of the wrong type — is an
//! error, never a silent default.
//!
//! Migrating does not rewrite history. It appends one new checkpoint to the same
//! thread, whose [`Provenance::Migration`] names the schema it came from, so the
//! thread stays linear and the pre-migration checkpoints remain readable.

use serde_json::Value;

use crate::{
    checkpoint::{Checkpoint, CheckpointError, Parts, Provenance},
    ids::{ChannelName, SchemaName},
    schema::{Schema, SchemaRef, SchemaVersion},
    state::{State, StateError},
};

/// One change between two schema versions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrationStep {
    /// Moves a channel's value and version counter to a new name.
    RenameChannel {
        /// The existing channel.
        from: ChannelName,
        /// Its new name; must not already exist.
        to: ChannelName,
    },
    /// Removes a channel and its value.
    DropChannel {
        /// The channel to remove.
        channel: ChannelName,
    },
    /// Introduces a channel that the old schema did not have.
    AddChannel {
        /// The new channel; must not already exist.
        channel: ChannelName,
        /// Its starting value (the channel then begins at version 1). With `None`
        /// the channel starts at version 0 holding the target reducer's identity
        /// value, or absent if that reducer has none.
        initial: Option<Value>,
    },
}

/// A migration could not be defined or applied.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MigrationError {
    /// A migration keeps the schema's name; only the version changes.
    #[error("a migration keeps the schema name `{from}`, but the target is `{to}`")]
    DifferentSchema {
        /// The source schema's name.
        from: SchemaName,
        /// The target schema's name.
        to: SchemaName,
    },
    /// The target version must be strictly newer.
    #[error("target version {to} is not newer than {from}")]
    NotAnUpgrade {
        /// The source version.
        from: SchemaVersion,
        /// The target version.
        to: SchemaVersion,
    },
    /// The checkpoint is not at the schema this migration starts from.
    #[error("checkpoint is at schema {checkpoint}, but this migration starts from {expected}")]
    WrongSource {
        /// The schema the checkpoint carries.
        checkpoint: Box<SchemaRef>,
        /// The schema the migration expects.
        expected: Box<SchemaRef>,
    },
    /// A step refers to a channel the state does not have.
    #[error("step {step}: channel `{channel}` does not exist")]
    UnknownChannel {
        /// Zero-based index of the step.
        step: usize,
        /// The missing channel.
        channel: ChannelName,
    },
    /// A step would create a channel that already exists.
    #[error("step {step}: channel `{channel}` already exists")]
    ChannelExists {
        /// Zero-based index of the step.
        step: usize,
        /// The clashing channel.
        channel: ChannelName,
    },
    /// After all steps the state does not fit the target schema.
    #[error("migrated state does not fit the target schema: {0}")]
    DoesNotFit(#[from] StateError),
    /// The migrated checkpoint could not be sealed.
    #[error(transparent)]
    Checkpoint(#[from] CheckpointError),
}

/// A validated plan for moving checkpoints from one schema version to a newer one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Migration {
    from: SchemaRef,
    to: Schema,
    steps: Vec<MigrationStep>,
}

impl Migration {
    /// Defines a migration from the schema identified by `from` to `to`.
    ///
    /// # Errors
    ///
    /// Returns [`MigrationError::DifferentSchema`] if the names differ and
    /// [`MigrationError::NotAnUpgrade`] if `to` is not a newer version.
    pub fn new(
        from: SchemaRef,
        to: Schema,
        steps: Vec<MigrationStep>,
    ) -> Result<Self, MigrationError> {
        if from.name != *to.name() {
            return Err(MigrationError::DifferentSchema {
                from: from.name,
                to: to.name().clone(),
            });
        }
        if to.version() <= from.version {
            return Err(MigrationError::NotAnUpgrade {
                from: from.version,
                to: to.version(),
            });
        }
        Ok(Self { from, to, steps })
    }

    /// The schema checkpoints must be at to be migrated.
    #[must_use]
    pub fn from(&self) -> &SchemaRef {
        &self.from
    }

    /// The schema migrated checkpoints are written under.
    #[must_use]
    pub fn to(&self) -> &Schema {
        &self.to
    }

    /// The steps, in application order.
    #[must_use]
    pub fn steps(&self) -> &[MigrationStep] {
        &self.steps
    }

    /// Produces the checkpoint that follows `checkpoint` under the target schema.
    ///
    /// The result has `checkpoint` as its parent, the next step number, the same
    /// frontier and metadata, and the migrated state. `checkpoint` itself is not
    /// modified. The caller stores the result with `Checkpointer::put`.
    ///
    /// # Errors
    ///
    /// Returns [`MigrationError`] if the checkpoint fails verification, is not at
    /// the source schema, a step is inapplicable, or the migrated state does not
    /// fit the target schema.
    pub fn apply(&self, checkpoint: &Checkpoint) -> Result<Checkpoint, MigrationError> {
        checkpoint.verify()?;
        if checkpoint.schema() != &self.from {
            return Err(MigrationError::WrongSource {
                checkpoint: Box::new(checkpoint.schema().clone()),
                expected: Box::new(self.from.clone()),
            });
        }

        let mut values = checkpoint.state().values().clone();
        let mut versions = checkpoint.state().versions().clone();
        for (index, step) in self.steps.iter().enumerate() {
            match step {
                MigrationStep::RenameChannel { from, to } => {
                    let Some(version) = versions.remove(from) else {
                        return Err(MigrationError::UnknownChannel {
                            step: index,
                            channel: from.clone(),
                        });
                    };
                    if versions.contains_key(to) {
                        return Err(MigrationError::ChannelExists {
                            step: index,
                            channel: to.clone(),
                        });
                    }
                    versions.insert(to.clone(), version);
                    if let Some(value) = values.remove(from) {
                        values.insert(to.clone(), value);
                    }
                }
                MigrationStep::DropChannel { channel } => {
                    if versions.remove(channel).is_none() {
                        return Err(MigrationError::UnknownChannel {
                            step: index,
                            channel: channel.clone(),
                        });
                    }
                    values.remove(channel);
                }
                MigrationStep::AddChannel { channel, initial } => {
                    if versions.contains_key(channel) {
                        return Err(MigrationError::ChannelExists {
                            step: index,
                            channel: channel.clone(),
                        });
                    }
                    match initial {
                        Some(value) => {
                            values.insert(channel.clone(), value.clone());
                            versions.insert(channel.clone(), 1);
                        }
                        None => {
                            let identity = self
                                .to
                                .channel(channel.as_str())
                                .and_then(|spec| spec.reducer().identity());
                            if let Some(identity) = identity {
                                values.insert(channel.clone(), identity);
                            }
                            versions.insert(channel.clone(), 0);
                        }
                    }
                }
            }
        }

        let state = State::from_parts(values, versions);
        state.conforms_to(&self.to)?;
        let step = checkpoint
            .step()
            .checked_add(1)
            .ok_or(CheckpointError::StepOverflow)?;
        Ok(Checkpoint::seal(Parts {
            thread: checkpoint.thread().clone(),
            parent: Some(checkpoint.id()),
            step,
            schema: self.to.reference(),
            state,
            frontier: checkpoint.frontier().clone(),
            provenance: Provenance::Migration {
                from: checkpoint.schema().clone(),
            },
            metadata: checkpoint.metadata().clone(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        checkpointer::{Checkpointer, InMemoryCheckpointer},
        frontier::Frontier,
        ids::{NodeName, ThreadId, WriterId},
        reducer::ReducerKind,
        update::{Update, UpdateBatch},
        value_type::ValueType,
    };
    use serde_json::json;
    use std::collections::BTreeMap;

    fn v1() -> Schema {
        Schema::builder("agent", 1)
            .channel("messages", ValueType::Array, ReducerKind::Append)
            .channel("turns", ValueType::Integer, ReducerKind::SumI64)
            .channel("scratch", ValueType::String, ReducerKind::Overwrite)
            .build()
            .unwrap()
    }

    fn v2() -> Schema {
        Schema::builder("agent", 2)
            .channel("history", ValueType::Array, ReducerKind::Append)
            .channel("turns", ValueType::Integer, ReducerKind::SumI64)
            .channel("tags", ValueType::Array, ReducerKind::Union)
            .build()
            .unwrap()
    }

    fn name(text: &str) -> ChannelName {
        ChannelName::new(text).unwrap()
    }

    fn steps() -> Vec<MigrationStep> {
        vec![
            MigrationStep::RenameChannel {
                from: name("messages"),
                to: name("history"),
            },
            MigrationStep::DropChannel {
                channel: name("scratch"),
            },
            MigrationStep::AddChannel {
                channel: name("tags"),
                initial: None,
            },
        ]
    }

    fn checkpoint_v1() -> Checkpoint {
        let input = UpdateBatch::from_writer_outputs([(
            WriterId::new("w").unwrap(),
            vec![
                Update::named("messages", json!(["hello"])).unwrap(),
                Update::named("turns", json!(2)).unwrap(),
                Update::named("scratch", json!("x")).unwrap(),
            ],
        )])
        .unwrap();
        let mut metadata = BTreeMap::new();
        metadata.insert("run".to_owned(), json!("r1"));
        Checkpoint::initial(
            ThreadId::new("t").unwrap(),
            &v1(),
            &input,
            Frontier::plan(0, vec![(NodeName::new("respond").unwrap(), json!({}))]),
            metadata,
        )
        .unwrap()
    }

    fn migration() -> Migration {
        Migration::new(v1().reference(), v2(), steps()).unwrap()
    }

    #[test]
    fn migrates_renames_drops_and_additions() {
        let before = checkpoint_v1();
        let after = migration().apply(&before).unwrap();

        assert_eq!(after.parent(), Some(before.id()));
        assert_eq!(after.step(), before.step() + 1);
        assert_eq!(after.thread(), before.thread());
        assert_eq!(after.schema(), &v2().reference());
        assert_eq!(after.frontier(), before.frontier());
        assert_eq!(after.metadata(), before.metadata());
        assert!(
            matches!(after.provenance(), Provenance::Migration { from } if from == before.schema())
        );

        let state = after.state();
        assert_eq!(state.value("history"), Some(&json!(["hello"])));
        assert_eq!(state.value("turns"), Some(&json!(2)));
        assert_eq!(state.value("tags"), Some(&json!([])));
        assert_eq!(state.value("messages"), None);
        assert_eq!(state.value("scratch"), None);
        assert_eq!(state.version("history"), before.state().version("messages"));
        assert_eq!(state.version("tags"), Some(0));
        assert!(after.validate_against(&v2()).is_ok());
        assert!(after.verify().is_ok());
    }

    #[test]
    fn the_original_is_untouched_and_the_result_is_storable() {
        let before = checkpoint_v1();
        let snapshot = before.clone();
        let after = migration().apply(&before).unwrap();
        assert_eq!(before, snapshot);

        let store = InMemoryCheckpointer::new();
        store.put(&before).unwrap();
        store.put(&after).unwrap();
        assert_eq!(
            store.head(before.thread()).unwrap().unwrap().id(),
            after.id()
        );
        assert_eq!(store.history(before.thread()).unwrap().len(), 2);
    }

    #[test]
    fn the_migrated_thread_can_continue_under_the_new_schema() {
        let after = migration().apply(&checkpoint_v1()).unwrap();
        let batch = UpdateBatch::from_writer_outputs([(
            WriterId::new("w").unwrap(),
            vec![Update::named("tags", json!(["urgent"])).unwrap()],
        )])
        .unwrap();
        let next = after
            .advance(&v2(), &batch, Frontier::empty(), BTreeMap::new())
            .unwrap();
        assert_eq!(next.state().value("tags"), Some(&json!(["urgent"])));
    }

    #[test]
    fn add_with_an_initial_value_starts_at_version_one() {
        let target = Schema::builder("agent", 2)
            .channel("messages", ValueType::Array, ReducerKind::Append)
            .channel("turns", ValueType::Integer, ReducerKind::SumI64)
            .channel("owner", ValueType::String, ReducerKind::Overwrite)
            .build()
            .unwrap();
        let migration = Migration::new(
            v1().reference(),
            target,
            vec![
                MigrationStep::DropChannel {
                    channel: name("scratch"),
                },
                MigrationStep::AddChannel {
                    channel: name("owner"),
                    initial: Some(json!("ada")),
                },
            ],
        )
        .unwrap();
        let after = migration.apply(&checkpoint_v1()).unwrap();
        assert_eq!(after.state().value("owner"), Some(&json!("ada")));
        assert_eq!(after.state().version("owner"), Some(1));
    }

    #[test]
    fn definition_errors() {
        assert!(matches!(
            Migration::new(
                v1().reference(),
                Schema::builder("other", 2)
                    .channel("a", ValueType::Any, ReducerKind::Overwrite)
                    .build()
                    .unwrap(),
                vec![]
            ),
            Err(MigrationError::DifferentSchema { .. })
        ));
        assert!(matches!(
            Migration::new(v2().reference(), v1(), vec![]),
            Err(MigrationError::NotAnUpgrade { .. })
        ));
        assert!(matches!(
            Migration::new(v1().reference(), v1(), vec![]),
            Err(MigrationError::NotAnUpgrade { .. })
        ));
    }

    #[test]
    fn a_checkpoint_at_another_schema_is_refused() {
        let wrong = Migration::new(
            v2().reference(),
            {
                Schema::builder("agent", 3)
                    .channel("history", ValueType::Array, ReducerKind::Append)
                    .build()
                    .unwrap()
            },
            vec![],
        )
        .unwrap();
        assert!(matches!(
            wrong.apply(&checkpoint_v1()),
            Err(MigrationError::WrongSource { .. })
        ));
    }

    #[test]
    fn inapplicable_steps_name_the_step_and_channel() {
        let missing = Migration::new(
            v1().reference(),
            v2(),
            vec![MigrationStep::RenameChannel {
                from: name("ghost"),
                to: name("history"),
            }],
        )
        .unwrap();
        assert!(matches!(
            missing.apply(&checkpoint_v1()),
            Err(MigrationError::UnknownChannel { step: 0, .. })
        ));

        let clash = Migration::new(
            v1().reference(),
            v2(),
            vec![MigrationStep::RenameChannel {
                from: name("messages"),
                to: name("turns"),
            }],
        )
        .unwrap();
        assert!(matches!(
            clash.apply(&checkpoint_v1()),
            Err(MigrationError::ChannelExists { step: 0, .. })
        ));

        let duplicate_add = Migration::new(
            v1().reference(),
            v2(),
            vec![MigrationStep::AddChannel {
                channel: name("turns"),
                initial: None,
            }],
        )
        .unwrap();
        assert!(matches!(
            duplicate_add.apply(&checkpoint_v1()),
            Err(MigrationError::ChannelExists { step: 0, .. })
        ));

        let drop_missing = Migration::new(
            v1().reference(),
            v2(),
            vec![MigrationStep::DropChannel {
                channel: name("ghost"),
            }],
        )
        .unwrap();
        assert!(matches!(
            drop_missing.apply(&checkpoint_v1()),
            Err(MigrationError::UnknownChannel { step: 0, .. })
        ));
    }

    #[test]
    fn unaccounted_channels_are_errors_not_defaults() {
        // Missing the rename: `history` has no counterpart and `messages` is extra.
        let incomplete = Migration::new(
            v1().reference(),
            v2(),
            vec![
                MigrationStep::DropChannel {
                    channel: name("scratch"),
                },
                MigrationStep::AddChannel {
                    channel: name("tags"),
                    initial: None,
                },
            ],
        )
        .unwrap();
        assert!(matches!(
            incomplete.apply(&checkpoint_v1()),
            Err(MigrationError::DoesNotFit(_))
        ));
    }

    #[test]
    fn carried_values_must_fit_the_target_type() {
        let retyped = Schema::builder("agent", 2)
            .channel("messages", ValueType::String, ReducerKind::Overwrite)
            .channel("turns", ValueType::Integer, ReducerKind::SumI64)
            .build()
            .unwrap();
        let migration = Migration::new(
            v1().reference(),
            retyped,
            vec![MigrationStep::DropChannel {
                channel: name("scratch"),
            }],
        )
        .unwrap();
        assert!(matches!(
            migration.apply(&checkpoint_v1()),
            Err(MigrationError::DoesNotFit(StateError::TypeMismatch { .. }))
        ));
    }

    #[test]
    fn accessors_expose_the_definition() {
        let migration = migration();
        assert_eq!(migration.from(), &v1().reference());
        assert_eq!(migration.to(), &v2());
        assert_eq!(migration.steps().len(), 3);
    }
}
