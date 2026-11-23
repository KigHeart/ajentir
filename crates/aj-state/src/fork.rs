//! Fork-with-edit: resuming from an earlier point without rewriting history.
//!
//! A thread is a linear chain and is never rewound or branched. To "go back and
//! try something different", a checkpoint is **forked**: its state and frontier are
//! copied into the first checkpoint of a *new* thread, optionally with an edit
//! applied on top. The edit goes through the schema's reducers like any other
//! update — a forked `Append` channel gains items, it is not replaced — and the
//! complete edit is recorded in the new checkpoint's [`Provenance::Fork`], so the
//! divergence from the source is always auditable. The source thread is not
//! touched.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::{
    checkpoint::{Checkpoint, CheckpointError, CheckpointId, Parts, Provenance},
    checkpointer::{Checkpointer, StoreError},
    frontier::Frontier,
    ids::ThreadId,
    schema::Schema,
    update::UpdateBatch,
};

/// What to do differently in the forked thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkSpec {
    /// The new thread. Must differ from the source thread, and must not already
    /// hold checkpoints when the fork is stored.
    pub target: ThreadId,
    /// An edit applied to the copied state through the schema's reducers.
    pub edit: Option<UpdateBatch>,
    /// The work to run first in the new thread. `None` keeps the source
    /// checkpoint's frontier. Plan a replacement with `Frontier::plan(0, …)`.
    pub frontier: Option<Frontier>,
    /// Metadata for the new checkpoint.
    pub metadata: BTreeMap<String, Value>,
}

impl ForkSpec {
    /// A fork into `target` that copies state and frontier unchanged.
    #[must_use]
    pub fn new(target: ThreadId) -> Self {
        Self {
            target,
            edit: None,
            frontier: None,
            metadata: BTreeMap::new(),
        }
    }

    /// Applies `edit` to the copied state.
    #[must_use]
    pub fn with_edit(mut self, edit: UpdateBatch) -> Self {
        self.edit = Some(edit);
        self
    }

    /// Replaces the frontier the forked thread starts with.
    #[must_use]
    pub fn with_frontier(mut self, frontier: Frontier) -> Self {
        self.frontier = Some(frontier);
        self
    }

    /// Sets the new checkpoint's metadata.
    #[must_use]
    pub fn with_metadata(mut self, metadata: BTreeMap<String, Value>) -> Self {
        self.metadata = metadata;
        self
    }
}

/// Builds the first checkpoint of `spec.target` from `source`.
///
/// The result has step 0 and no parent, as the first checkpoint of any thread
/// does; its provenance names the source thread and checkpoint. Nothing is stored.
///
/// # Errors
///
/// Returns [`CheckpointError::ForkIntoSameThread`] if the target is the source's
/// thread, [`CheckpointError::IntegrityMismatch`] if `source` fails verification,
/// [`CheckpointError::SchemaMismatch`] or [`CheckpointError::State`] if `source`
/// does not belong to `schema`, and [`CheckpointError::Update`] if the edit is
/// invalid under `schema`.
pub fn fork_checkpoint(
    source: &Checkpoint,
    schema: &Schema,
    spec: ForkSpec,
) -> Result<Checkpoint, CheckpointError> {
    if spec.target == *source.thread() {
        return Err(CheckpointError::ForkIntoSameThread(spec.target));
    }
    source.verify()?;
    source.validate_against(schema)?;

    let state = match &spec.edit {
        Some(edit) => source.state().apply(schema, edit)?.0,
        None => source.state().clone(),
    };
    Ok(Checkpoint::seal(Parts {
        thread: spec.target,
        parent: None,
        step: 0,
        schema: source.schema().clone(),
        state,
        frontier: spec.frontier.unwrap_or_else(|| source.frontier().clone()),
        provenance: Provenance::Fork {
            source_thread: source.thread().clone(),
            source_checkpoint: source.id(),
            edit: spec.edit,
        },
        metadata: spec.metadata,
    }))
}

/// Forks a stored checkpoint into a new thread and stores the result.
///
/// Loads `source_checkpoint` from `source_thread`, builds the fork with
/// [`fork_checkpoint`] and appends it to `store`. Returns the stored checkpoint.
///
/// # Errors
///
/// Returns [`StoreError::NotFound`] if the source checkpoint does not exist,
/// [`StoreError::Checkpoint`] for the failures listed on [`fork_checkpoint`], and
/// [`StoreError::HeadMismatch`] if the target thread already holds checkpoints.
pub fn fork_thread(
    store: &dyn Checkpointer,
    source_thread: &ThreadId,
    source_checkpoint: &CheckpointId,
    schema: &Schema,
    spec: ForkSpec,
) -> Result<Checkpoint, StoreError> {
    let source = store
        .get(source_thread, source_checkpoint)?
        .ok_or_else(|| StoreError::NotFound {
            thread: source_thread.clone(),
            checkpoint: *source_checkpoint,
        })?;
    let forked = fork_checkpoint(&source, schema, spec)?;
    store.put(&forked)?;
    Ok(forked)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        checkpointer::InMemoryCheckpointer,
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

    fn thread(name: &str) -> ThreadId {
        ThreadId::new(name).unwrap()
    }

    fn batch(channel: &str, value: Value) -> UpdateBatch {
        UpdateBatch::from_writer_outputs([(
            WriterId::new("editor").unwrap(),
            vec![Update::named(channel, value).unwrap()],
        )])
        .unwrap()
    }

    /// Two checkpoints in thread `main`; returns the store and both.
    fn populated() -> (InMemoryCheckpointer, Checkpoint, Checkpoint) {
        let schema = schema();
        let first = Checkpoint::initial(
            thread("main"),
            &schema,
            &batch("messages", json!(["q1"])),
            Frontier::plan(0, vec![(NodeName::new("answer").unwrap(), json!({"n": 1}))]),
            BTreeMap::new(),
        )
        .unwrap();
        let second = first
            .advance(
                &schema,
                &batch("messages", json!(["a1"])),
                Frontier::plan(1, vec![(NodeName::new("answer").unwrap(), json!({"n": 2}))]),
                BTreeMap::new(),
            )
            .unwrap();
        let store = InMemoryCheckpointer::new();
        store.put(&first).unwrap();
        store.put(&second).unwrap();
        (store, first, second)
    }

    #[test]
    fn a_plain_fork_copies_state_and_frontier_into_a_new_thread() {
        let (_, first, _) = populated();
        let forked = fork_checkpoint(&first, &schema(), ForkSpec::new(thread("alt"))).unwrap();
        assert_eq!(forked.thread(), &thread("alt"));
        assert_eq!(forked.step(), 0);
        assert_eq!(forked.parent(), None);
        assert_eq!(forked.state(), first.state());
        assert_eq!(forked.frontier(), first.frontier());
        assert_eq!(forked.schema(), first.schema());
        assert_ne!(forked.id(), first.id());
        assert!(forked.verify().is_ok());
        assert!(matches!(
            forked.provenance(),
            Provenance::Fork { source_thread, source_checkpoint, edit: None }
                if source_thread == &thread("main") && *source_checkpoint == first.id()
        ));
    }

    #[test]
    fn the_edit_goes_through_reducers_and_is_recorded() {
        let (_, first, _) = populated();
        let edit = batch("messages", json!(["q1-edited"]));
        let forked = fork_checkpoint(
            &first,
            &schema(),
            ForkSpec::new(thread("alt")).with_edit(edit.clone()),
        )
        .unwrap();
        assert_eq!(
            forked.state().value("messages"),
            Some(&json!(["q1", "q1-edited"]))
        );
        assert!(matches!(
            forked.provenance(),
            Provenance::Fork { edit: Some(recorded), .. } if recorded == &edit
        ));
    }

    #[test]
    fn the_frontier_and_metadata_can_be_replaced() {
        let (_, first, _) = populated();
        let replacement = Frontier::plan(0, vec![(NodeName::new("retry").unwrap(), json!({}))]);
        let mut metadata = BTreeMap::new();
        metadata.insert("reason".to_owned(), json!("try again"));
        let forked = fork_checkpoint(
            &first,
            &schema(),
            ForkSpec::new(thread("alt"))
                .with_frontier(replacement.clone())
                .with_metadata(metadata.clone()),
        )
        .unwrap();
        assert_eq!(forked.frontier(), &replacement);
        assert_eq!(forked.metadata(), &metadata);
    }

    #[test]
    fn forking_is_deterministic() {
        let (_, first, _) = populated();
        let spec = || ForkSpec::new(thread("alt")).with_edit(batch("turns", json!(5)));
        let one = fork_checkpoint(&first, &schema(), spec()).unwrap();
        let two = fork_checkpoint(&first, &schema(), spec()).unwrap();
        assert_eq!(one.id(), two.id());
    }

    #[test]
    fn invalid_forks_are_refused() {
        let (_, first, _) = populated();
        assert!(matches!(
            fork_checkpoint(&first, &schema(), ForkSpec::new(thread("main"))),
            Err(CheckpointError::ForkIntoSameThread(_))
        ));
        assert!(matches!(
            fork_checkpoint(
                &first,
                &schema(),
                ForkSpec::new(thread("alt")).with_edit(batch("ghost", json!(1)))
            ),
            Err(CheckpointError::Update(_))
        ));
        let newer = Schema::builder("agent", 2)
            .channel("messages", ValueType::Array, ReducerKind::Append)
            .channel("turns", ValueType::Integer, ReducerKind::SumI64)
            .build()
            .unwrap();
        assert!(matches!(
            fork_checkpoint(&first, &newer, ForkSpec::new(thread("alt"))),
            Err(CheckpointError::SchemaMismatch { .. })
        ));
    }

    #[test]
    fn fork_thread_stores_the_fork_and_leaves_the_source_alone() {
        let (store, first, second) = populated();
        let forked = fork_thread(
            &store,
            &thread("main"),
            &first.id(),
            &schema(),
            ForkSpec::new(thread("alt")).with_edit(batch("messages", json!(["q1-v2"]))),
        )
        .unwrap();

        assert_eq!(
            store.head(&thread("alt")).unwrap().unwrap().id(),
            forked.id()
        );
        let main = store.history(&thread("main")).unwrap();
        assert_eq!(main.len(), 2);
        assert_eq!(
            store.head(&thread("main")).unwrap().unwrap().id(),
            second.id()
        );
        assert_eq!(
            store.threads().unwrap(),
            vec![thread("alt"), thread("main")]
        );
    }

    #[test]
    fn a_forked_thread_continues_independently() {
        let (store, first, _) = populated();
        let forked = fork_thread(
            &store,
            &thread("main"),
            &first.id(),
            &schema(),
            ForkSpec::new(thread("alt")),
        )
        .unwrap();
        let next = forked
            .advance(
                &schema(),
                &batch("messages", json!(["a1-alt"])),
                Frontier::empty(),
                BTreeMap::new(),
            )
            .unwrap();
        store.put(&next).unwrap();
        assert_eq!(
            store
                .head(&thread("alt"))
                .unwrap()
                .unwrap()
                .state()
                .value("messages"),
            Some(&json!(["q1", "a1-alt"]))
        );
        assert_eq!(
            store
                .head(&thread("main"))
                .unwrap()
                .unwrap()
                .state()
                .value("messages"),
            Some(&json!(["q1", "a1"]))
        );
    }

    #[test]
    fn fork_thread_errors() {
        let (store, first, _) = populated();
        let missing = CheckpointId::from_hex(&"0".repeat(64)).unwrap();
        assert!(matches!(
            fork_thread(
                &store,
                &thread("main"),
                &missing,
                &schema(),
                ForkSpec::new(thread("alt"))
            ),
            Err(StoreError::NotFound { .. })
        ));
        assert!(matches!(
            fork_thread(
                &store,
                &thread("main"),
                &first.id(),
                &schema(),
                ForkSpec::new(thread("main"))
            ),
            Err(StoreError::Checkpoint(CheckpointError::ForkIntoSameThread(
                _
            )))
        ));

        fork_thread(
            &store,
            &thread("main"),
            &first.id(),
            &schema(),
            ForkSpec::new(thread("alt")),
        )
        .unwrap();
        let different = ForkSpec::new(thread("alt")).with_edit(batch("turns", json!(1)));
        assert!(matches!(
            fork_thread(&store, &thread("main"), &first.id(), &schema(), different),
            Err(StoreError::HeadMismatch { .. })
        ));
    }
}
