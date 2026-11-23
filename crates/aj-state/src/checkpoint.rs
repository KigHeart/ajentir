//! Checkpoints: validated state plus the frontier that continues from it.
//!
//! A [`Checkpoint`] is a complete, self-describing resume point: the state, the
//! tasks that were about to run, the schema it is valid under, where it came
//! from, and its position in its thread. Its [`CheckpointId`] is a digest of all
//! of that, so the id both names the checkpoint and proves its content: a stored
//! checkpoint whose bytes were altered no longer matches its id, and
//! [`Checkpoint::verify`] says so.

use std::{collections::BTreeMap, fmt};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    digest::{Digest, DigestBuilder, DigestParseError},
    frontier::Frontier,
    ids::ThreadId,
    schema::{Schema, SchemaRef},
    state::{State, StateError, UpdateError},
    update::UpdateBatch,
};

/// The content-derived identity of a [`Checkpoint`].
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CheckpointId(Digest);

impl CheckpointId {
    /// Parses the form produced by [`CheckpointId::to_hex`].
    ///
    /// # Errors
    ///
    /// Returns [`DigestParseError`] unless the text is 64 lowercase hex characters.
    pub fn from_hex(text: &str) -> Result<Self, DigestParseError> {
        Digest::from_hex(text).map(Self)
    }

    /// The 64-character lowercase hex form.
    #[must_use]
    pub fn to_hex(&self) -> String {
        self.0.to_hex()
    }

    /// The underlying digest.
    #[must_use]
    pub fn digest(&self) -> &Digest {
        &self.0
    }
}

impl fmt::Display for CheckpointId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl fmt::Debug for CheckpointId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CheckpointId({})", self.0)
    }
}

/// How a checkpoint came to exist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Provenance {
    /// The first checkpoint of a thread, built from the initial input batch.
    Input {
        /// Digest of the input batch.
        batch: Digest,
    },
    /// Produced by applying a superstep's batch to the previous checkpoint.
    Step {
        /// Digest of the applied batch.
        batch: Digest,
    },
    /// Copied from another thread, optionally with an edit applied through the
    /// schema's reducers. The edit itself is recorded in full so the divergence
    /// from the source is auditable.
    Fork {
        /// The thread forked from.
        source_thread: ThreadId,
        /// The checkpoint forked from.
        source_checkpoint: CheckpointId,
        /// The edit applied on top of the source state, if any.
        edit: Option<UpdateBatch>,
    },
    /// Produced by migrating the previous checkpoint to a newer schema.
    Migration {
        /// The schema the state was migrated from.
        from: SchemaRef,
    },
}

impl Provenance {
    fn feed(&self, builder: &mut DigestBuilder) {
        match self {
            Self::Input { batch } => {
                builder.tag(0).digest(batch);
            }
            Self::Step { batch } => {
                builder.tag(1).digest(batch);
            }
            Self::Fork {
                source_thread,
                source_checkpoint,
                edit,
            } => {
                builder
                    .tag(2)
                    .str(source_thread.as_str())
                    .digest(source_checkpoint.digest());
                match edit {
                    Some(batch) => {
                        builder.tag(1).digest(&batch.digest());
                    }
                    None => {
                        builder.tag(0);
                    }
                }
            }
            Self::Migration { from } => {
                builder
                    .tag(3)
                    .str(from.name.as_str())
                    .u64(u64::from(from.version.get()))
                    .digest(&from.fingerprint);
            }
        }
    }
}

/// Why a checkpoint could not be built or does not hold up.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CheckpointError {
    /// Applying a batch to the state failed.
    #[error(transparent)]
    Update(#[from] UpdateError),
    /// The stored state does not conform to the schema.
    #[error(transparent)]
    State(#[from] StateError),
    /// The checkpoint was written under a different schema than the one supplied.
    #[error("checkpoint was written under schema {checkpoint} but {supplied} was supplied")]
    SchemaMismatch {
        /// The schema recorded in the checkpoint.
        checkpoint: Box<SchemaRef>,
        /// The schema the caller tried to use.
        supplied: Box<SchemaRef>,
    },
    /// The checkpoint's content no longer hashes to its id.
    #[error("checkpoint id {claimed} does not match its content (computed {computed})")]
    IntegrityMismatch {
        /// The id the checkpoint carries.
        claimed: CheckpointId,
        /// The id its content actually hashes to.
        computed: CheckpointId,
    },
    /// The thread's step counter cannot advance further.
    #[error("step counter overflow")]
    StepOverflow,
    /// A fork's target must be a different thread from its source.
    #[error("cannot fork thread `{0}` into itself")]
    ForkIntoSameThread(ThreadId),
}

/// The fields of a checkpoint other than its id, gathered so [`Checkpoint::seal`]
/// can derive the id from them.
pub(crate) struct Parts {
    pub(crate) thread: ThreadId,
    pub(crate) parent: Option<CheckpointId>,
    pub(crate) step: u64,
    pub(crate) schema: SchemaRef,
    pub(crate) state: State,
    pub(crate) frontier: Frontier,
    pub(crate) provenance: Provenance,
    pub(crate) metadata: BTreeMap<String, Value>,
}

/// A durable resume point. Immutable once sealed.
///
/// Deserializing a checkpoint does **not** trust its id; call
/// [`Checkpoint::verify`] (every [`Checkpointer`](crate::Checkpointer) does) before
/// relying on stored data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    id: CheckpointId,
    thread: ThreadId,
    parent: Option<CheckpointId>,
    step: u64,
    schema: SchemaRef,
    state: State,
    frontier: Frontier,
    provenance: Provenance,
    metadata: BTreeMap<String, Value>,
}

impl Checkpoint {
    /// Creates the first checkpoint of `thread` (step 0) by applying `input` to a
    /// fresh state.
    ///
    /// `frontier` is the work to run next; plan it with `Frontier::plan(0, …)`.
    /// `metadata` is free-form and is covered by the checkpoint id.
    ///
    /// # Errors
    ///
    /// Returns [`CheckpointError::Update`] if the input batch is invalid under
    /// `schema`.
    pub fn initial(
        thread: ThreadId,
        schema: &Schema,
        input: &UpdateBatch,
        frontier: Frontier,
        metadata: BTreeMap<String, Value>,
    ) -> Result<Self, CheckpointError> {
        let (state, applied) = State::new(schema).apply(schema, input)?;
        Ok(Self::seal(Parts {
            thread,
            parent: None,
            step: 0,
            schema: schema.reference(),
            state,
            frontier,
            provenance: Provenance::Input {
                batch: *applied.batch_digest(),
            },
            metadata,
        }))
    }

    /// Creates the next checkpoint of this thread by applying a superstep's
    /// `batch`. The new checkpoint's parent is this one and its step is
    /// [`next_step`](Self::next_step).
    ///
    /// `frontier` is the work to run after the step; plan it with
    /// `Frontier::plan(checkpoint.next_step(), …)`, or pass an empty frontier to
    /// mark the run finished.
    ///
    /// # Errors
    ///
    /// Returns [`CheckpointError::SchemaMismatch`] if `schema` is not the schema
    /// this checkpoint was written under, [`CheckpointError::Update`] if the batch
    /// is invalid, and [`CheckpointError::StepOverflow`] at `u64::MAX` steps.
    pub fn advance(
        &self,
        schema: &Schema,
        batch: &UpdateBatch,
        frontier: Frontier,
        metadata: BTreeMap<String, Value>,
    ) -> Result<Self, CheckpointError> {
        self.check_schema(schema)?;
        let step = self
            .step
            .checked_add(1)
            .ok_or(CheckpointError::StepOverflow)?;
        let (state, applied) = self.state.apply(schema, batch)?;
        Ok(Self::seal(Parts {
            thread: self.thread.clone(),
            parent: Some(self.id),
            step,
            schema: self.schema.clone(),
            state,
            frontier,
            provenance: Provenance::Step {
                batch: *applied.batch_digest(),
            },
            metadata,
        }))
    }

    /// Computes the id from `parts` and seals them into a checkpoint.
    pub(crate) fn seal(parts: Parts) -> Self {
        let mut checkpoint = Self {
            id: CheckpointId(Digest::ZERO),
            thread: parts.thread,
            parent: parts.parent,
            step: parts.step,
            schema: parts.schema,
            state: parts.state,
            frontier: parts.frontier,
            provenance: parts.provenance,
            metadata: parts.metadata,
        };
        checkpoint.id = checkpoint.compute_id();
        checkpoint
    }

    fn compute_id(&self) -> CheckpointId {
        let mut builder = DigestBuilder::new("ajentir.checkpoint.v1");
        builder.str(self.thread.as_str());
        match &self.parent {
            Some(parent) => {
                builder.tag(1).digest(parent.digest());
            }
            None => {
                builder.tag(0);
            }
        }
        builder
            .u64(self.step)
            .str(self.schema.name.as_str())
            .u64(u64::from(self.schema.version.get()))
            .digest(&self.schema.fingerprint)
            .digest(&self.state.digest())
            .digest(&self.frontier.digest());
        self.provenance.feed(&mut builder);
        builder.u64(u64::try_from(self.metadata.len()).unwrap_or(u64::MAX));
        for (key, value) in &self.metadata {
            builder.str(key).value(value);
        }
        CheckpointId(builder.finish())
    }

    /// Confirms the checkpoint's content still hashes to its id.
    ///
    /// # Errors
    ///
    /// Returns [`CheckpointError::IntegrityMismatch`] if it does not.
    pub fn verify(&self) -> Result<(), CheckpointError> {
        let computed = self.compute_id();
        if computed == self.id {
            Ok(())
        } else {
            Err(CheckpointError::IntegrityMismatch {
                claimed: self.id,
                computed,
            })
        }
    }

    pub(crate) fn check_schema(&self, schema: &Schema) -> Result<(), CheckpointError> {
        if self.schema.fingerprint == schema.fingerprint() {
            Ok(())
        } else {
            Err(CheckpointError::SchemaMismatch {
                checkpoint: Box::new(self.schema.clone()),
                supplied: Box::new(schema.reference()),
            })
        }
    }

    /// Confirms the checkpoint was written under exactly `schema` and that its
    /// state conforms to it.
    ///
    /// # Errors
    ///
    /// Returns [`CheckpointError::SchemaMismatch`] or [`CheckpointError::State`].
    pub fn validate_against(&self, schema: &Schema) -> Result<(), CheckpointError> {
        self.check_schema(schema)?;
        self.state.conforms_to(schema)?;
        Ok(())
    }

    /// The content-derived id.
    #[must_use]
    pub fn id(&self) -> CheckpointId {
        self.id
    }

    /// The thread this checkpoint belongs to.
    #[must_use]
    pub fn thread(&self) -> &ThreadId {
        &self.thread
    }

    /// The previous checkpoint in the same thread; `None` for a thread's first.
    #[must_use]
    pub fn parent(&self) -> Option<CheckpointId> {
        self.parent
    }

    /// Position in the thread; each stored checkpoint is exactly one past its parent.
    #[must_use]
    pub fn step(&self) -> u64 {
        self.step
    }

    /// The step number the next checkpoint will carry.
    #[must_use]
    pub fn next_step(&self) -> u64 {
        self.step.saturating_add(1)
    }

    /// The schema the state is valid under.
    #[must_use]
    pub fn schema(&self) -> &SchemaRef {
        &self.schema
    }

    /// The channel state.
    #[must_use]
    pub fn state(&self) -> &State {
        &self.state
    }

    /// The work that remains.
    #[must_use]
    pub fn frontier(&self) -> &Frontier {
        &self.frontier
    }

    /// How this checkpoint came to exist.
    #[must_use]
    pub fn provenance(&self) -> &Provenance {
        &self.provenance
    }

    /// Free-form metadata supplied when the checkpoint was created.
    #[must_use]
    pub fn metadata(&self) -> &BTreeMap<String, Value> {
        &self.metadata
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ids::{NodeName, WriterId},
        reducer::ReducerKind,
        update::Update,
        value_type::ValueType,
    };
    use serde_json::json;

    fn schema() -> Schema {
        Schema::builder("agent", 1)
            .channel("messages", ValueType::Array, ReducerKind::Append)
            .channel("turns", ValueType::Integer, ReducerKind::SumI64)
            .build()
            .unwrap()
    }

    fn batch(channel: &str, value: Value) -> UpdateBatch {
        UpdateBatch::from_writer_outputs([(
            WriterId::new("w").unwrap(),
            vec![Update::named(channel, value).unwrap()],
        )])
        .unwrap()
    }

    fn first() -> Checkpoint {
        Checkpoint::initial(
            ThreadId::new("t").unwrap(),
            &schema(),
            &batch("messages", json!(["hello"])),
            Frontier::plan(0, vec![(NodeName::new("respond").unwrap(), json!({}))]),
            BTreeMap::new(),
        )
        .unwrap()
    }

    #[test]
    fn initial_checkpoint_shape() {
        let cp = first();
        assert_eq!(cp.step(), 0);
        assert_eq!(cp.next_step(), 1);
        assert_eq!(cp.parent(), None);
        assert_eq!(cp.thread().as_str(), "t");
        assert!(matches!(cp.provenance(), Provenance::Input { .. }));
        assert_eq!(cp.state().value("messages"), Some(&json!(["hello"])));
        assert_eq!(cp.frontier().len(), 1);
        assert!(cp.verify().is_ok());
        assert!(cp.validate_against(&schema()).is_ok());
    }

    #[test]
    fn identical_inputs_give_identical_ids() {
        assert_eq!(first().id(), first().id());
    }

    #[test]
    fn advance_chains_to_the_parent() {
        let one = first();
        let two = one
            .advance(
                &schema(),
                &batch("turns", json!(1)),
                Frontier::empty(),
                BTreeMap::new(),
            )
            .unwrap();
        assert_eq!(two.step(), 1);
        assert_eq!(two.parent(), Some(one.id()));
        assert_eq!(two.state().value("turns"), Some(&json!(1)));
        assert_eq!(two.state().value("messages"), Some(&json!(["hello"])));
        assert_ne!(two.id(), one.id());
        assert!(matches!(two.provenance(), Provenance::Step { .. }));
        assert!(two.frontier().is_empty());
        assert!(two.verify().is_ok());
    }

    #[test]
    fn id_covers_state_frontier_metadata_and_parentage() {
        let base = first();
        let mut with_meta = BTreeMap::new();
        with_meta.insert("run".to_owned(), json!("a"));
        let tagged = Checkpoint::initial(
            ThreadId::new("t").unwrap(),
            &schema(),
            &batch("messages", json!(["hello"])),
            Frontier::plan(0, vec![(NodeName::new("respond").unwrap(), json!({}))]),
            with_meta,
        )
        .unwrap();
        let other_state = Checkpoint::initial(
            ThreadId::new("t").unwrap(),
            &schema(),
            &batch("messages", json!(["goodbye"])),
            Frontier::plan(0, vec![(NodeName::new("respond").unwrap(), json!({}))]),
            BTreeMap::new(),
        )
        .unwrap();
        let other_frontier = Checkpoint::initial(
            ThreadId::new("t").unwrap(),
            &schema(),
            &batch("messages", json!(["hello"])),
            Frontier::empty(),
            BTreeMap::new(),
        )
        .unwrap();
        let other_thread = Checkpoint::initial(
            ThreadId::new("u").unwrap(),
            &schema(),
            &batch("messages", json!(["hello"])),
            Frontier::plan(0, vec![(NodeName::new("respond").unwrap(), json!({}))]),
            BTreeMap::new(),
        )
        .unwrap();
        for different in [&tagged, &other_state, &other_frontier, &other_thread] {
            assert_ne!(base.id(), different.id());
        }
    }

    #[test]
    fn tampering_is_detected() {
        let cp = first();
        let mut json = serde_json::to_value(&cp).unwrap();
        json["state"]["values"]["messages"] = json!(["forged"]);
        let forged: Checkpoint = serde_json::from_value(json).unwrap();
        assert!(matches!(
            forged.verify(),
            Err(CheckpointError::IntegrityMismatch { .. })
        ));
    }

    #[test]
    fn a_different_schema_is_refused() {
        let other = Schema::builder("agent", 2)
            .channel("messages", ValueType::Array, ReducerKind::Append)
            .channel("turns", ValueType::Integer, ReducerKind::SumI64)
            .build()
            .unwrap();
        let cp = first();
        assert!(matches!(
            cp.validate_against(&other),
            Err(CheckpointError::SchemaMismatch { .. })
        ));
        assert!(matches!(
            cp.advance(
                &other,
                &UpdateBatch::empty(),
                Frontier::empty(),
                BTreeMap::new()
            ),
            Err(CheckpointError::SchemaMismatch { .. })
        ));
    }

    #[test]
    fn invalid_batches_surface_the_update_error() {
        assert!(matches!(
            first().advance(
                &schema(),
                &batch("ghost", json!(1)),
                Frontier::empty(),
                BTreeMap::new()
            ),
            Err(CheckpointError::Update(UpdateError::UnknownChannel(_)))
        ));
    }

    #[test]
    fn id_hex_round_trip() {
        let id = first().id();
        assert_eq!(CheckpointId::from_hex(&id.to_hex()).unwrap(), id);
        assert!(CheckpointId::from_hex("short").is_err());
        assert_eq!(id.to_string(), id.to_hex());
    }
}
