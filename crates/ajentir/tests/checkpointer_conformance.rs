//! One behavioural specification, run against every [`Checkpointer`] backend.
//!
//! Each check is written once against `&dyn Checkpointer`; the macro at the bottom
//! instantiates it for the in-memory and SQLite implementations, so a backend
//! cannot drift from the contract without a test failing.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

mod common;

#[cfg(feature = "sqlite")]
use ajentir::SqliteCheckpointer;
use ajentir::{
    Checkpoint, CheckpointError, CheckpointId, Checkpointer, InMemoryCheckpointer, StoreError,
    TaskId, TaskWrites, Update,
};
use common::{batch, child, schema, start, thread};
use serde_json::json;

fn put_chain(store: &dyn Checkpointer, length: usize) -> Vec<Checkpoint> {
    let mut chain = vec![start("main")];
    store.put(&chain[0]).unwrap();
    for turn in 1..length {
        let next = child(&chain[turn - 1], i64::try_from(turn).unwrap());
        store.put(&next).unwrap();
        chain.push(next);
    }
    chain
}

fn round_trip(store: &dyn Checkpointer) {
    let chain = put_chain(store, 3);
    for checkpoint in &chain {
        let loaded = store
            .get(checkpoint.thread(), &checkpoint.id())
            .unwrap()
            .unwrap();
        assert_eq!(&loaded, checkpoint);
    }
    let head = store.head(&thread("main")).unwrap().unwrap();
    assert_eq!(head, chain[2]);

    let history = store.history(&thread("main")).unwrap();
    assert_eq!(history.len(), 3);
    for (position, summary) in history.iter().enumerate() {
        assert_eq!(summary.id, chain[position].id());
        assert_eq!(summary.step, u64::try_from(position).unwrap());
        assert_eq!(summary.parent, chain[position].parent());
        assert_eq!(&summary.schema, chain[position].schema());
        assert_eq!(summary.pending_tasks, chain[position].frontier().len());
    }
    assert_eq!(history[0].pending_tasks, 2);
    assert_eq!(history[1].pending_tasks, 1);
}

fn unknown_threads_and_checkpoints_are_empty_not_errors(store: &dyn Checkpointer) {
    let stranger = start("stranger");
    assert!(store.head(&thread("nobody")).unwrap().is_none());
    assert!(store.history(&thread("nobody")).unwrap().is_empty());
    assert!(
        store
            .get(&thread("nobody"), &stranger.id())
            .unwrap()
            .is_none()
    );
    assert!(store.threads().unwrap().is_empty());
    assert!(
        store
            .task_writes(&thread("nobody"), &stranger.id())
            .unwrap()
            .is_empty()
    );
    assert!(!store.delete_thread(&thread("nobody")).unwrap());

    store.put(&stranger).unwrap();
    let elsewhere = start("elsewhere");
    assert!(
        store
            .get(&thread("stranger"), &elsewhere.id())
            .unwrap()
            .is_none()
    );
}

fn putting_the_head_again_is_a_no_op(store: &dyn Checkpointer) {
    let chain = put_chain(store, 2);
    store.put(&chain[1]).unwrap();
    assert_eq!(store.history(&thread("main")).unwrap().len(), 2);
    // A checkpoint that is neither the head nor its child is refused, even if it
    // was once stored.
    assert!(matches!(
        store.put(&chain[0]),
        Err(StoreError::HeadMismatch { .. })
    ));
}

fn a_stale_writer_loses_the_race(store: &dyn Checkpointer) {
    let root = start("main");
    store.put(&root).unwrap();
    let winner = child(&root, 1);
    let loser = child(&root, 2);
    store.put(&winner).unwrap();
    match store.put(&loser) {
        Err(StoreError::HeadMismatch {
            head: Some(head),
            expected_parent: Some(parent),
            ..
        }) => {
            assert_eq!(head, winner.id());
            assert_eq!(parent, root.id());
        }
        other => panic!("expected HeadMismatch, got {other:?}"),
    }
    assert_eq!(
        store.head(&thread("main")).unwrap().unwrap().id(),
        winner.id()
    );
}

fn a_thread_cannot_start_with_a_parent(store: &dyn Checkpointer) {
    let root = start("main");
    let orphan = child(&root, 1);
    assert!(matches!(
        store.put(&orphan),
        Err(StoreError::HeadMismatch { head: None, .. })
    ));
    assert!(store.threads().unwrap().is_empty());
}

fn a_second_root_cannot_replace_an_existing_thread(store: &dyn Checkpointer) {
    let root = start("main");
    store.put(&root).unwrap();
    let other_root = ajentir::Checkpoint::initial(
        thread("main"),
        &schema(),
        &batch("messages", json!(["different"])),
        ajentir::Frontier::empty(),
        std::collections::BTreeMap::new(),
    )
    .unwrap();
    assert!(matches!(
        store.put(&other_root),
        Err(StoreError::HeadMismatch { .. })
    ));
}

fn tampered_checkpoints_are_refused(store: &dyn Checkpointer) {
    let genuine = start("main");
    let mut value = serde_json::to_value(&genuine).unwrap();
    value["state"]["values"]["messages"] = json!(["forged"]);
    let forged: Checkpoint = serde_json::from_value(value).unwrap();
    assert!(matches!(
        store.put(&forged),
        Err(StoreError::Checkpoint(
            CheckpointError::IntegrityMismatch { .. }
        ))
    ));
    assert!(store.threads().unwrap().is_empty());
}

fn threads_are_listed_in_order_and_isolated(store: &dyn Checkpointer) {
    for name in ["delta", "alpha", "charlie", "bravo"] {
        store.put(&start(name)).unwrap();
    }
    assert_eq!(
        store.threads().unwrap(),
        vec![
            thread("alpha"),
            thread("bravo"),
            thread("charlie"),
            thread("delta")
        ]
    );
    // Identical content in two threads yields two distinct checkpoints.
    assert_ne!(start("alpha").id(), start("bravo").id());
    assert!(store.delete_thread(&thread("bravo")).unwrap());
    assert!(store.head(&thread("bravo")).unwrap().is_none());
    assert!(store.head(&thread("alpha")).unwrap().is_some());
    assert_eq!(store.threads().unwrap().len(), 3);
    assert!(!store.delete_thread(&thread("bravo")).unwrap());
}

fn task_writes_are_recorded_idempotent_and_ordered(store: &dyn Checkpointer) {
    let root = start("main");
    store.put(&root).unwrap();
    let ids: Vec<TaskId> = root
        .frontier()
        .tasks()
        .iter()
        .map(|t| t.id().clone())
        .collect();
    assert_eq!(ids.len(), 2);

    let writes = |task: &TaskId, turns: i64| {
        TaskWrites::new(
            task.clone(),
            vec![Update::named("turns", json!(turns)).unwrap()],
        )
    };
    // Record out of id order; reading back is ordered by task id.
    store
        .put_task_writes(&thread("main"), &root.id(), &writes(&ids[1], 5))
        .unwrap();
    store
        .put_task_writes(&thread("main"), &root.id(), &writes(&ids[0], 3))
        .unwrap();
    let stored = store.task_writes(&thread("main"), &root.id()).unwrap();
    assert_eq!(stored, vec![writes(&ids[0], 3), writes(&ids[1], 5)]);

    // Same writes again: fine. Different writes for the same task: conflict.
    store
        .put_task_writes(&thread("main"), &root.id(), &writes(&ids[0], 3))
        .unwrap();
    assert!(matches!(
        store.put_task_writes(&thread("main"), &root.id(), &writes(&ids[0], 99)),
        Err(StoreError::TaskWritesConflict(task)) if task == ids[0]
    ));
    assert_eq!(
        store
            .task_writes(&thread("main"), &root.id())
            .unwrap()
            .len(),
        2
    );
}

fn task_writes_are_validated_against_the_head(store: &dyn Checkpointer) {
    let root = start("main");
    store.put(&root).unwrap();
    let task = root.frontier().tasks()[0].id().clone();
    let writes = TaskWrites::new(task.clone(), vec![]);

    assert!(matches!(
        store.put_task_writes(
            &thread("main"),
            &root.id(),
            &TaskWrites::new(TaskId::new("not-in-frontier").unwrap(), vec![])
        ),
        Err(StoreError::UnknownTask(_))
    ));
    assert!(matches!(
        store.put_task_writes(&thread("ghost"), &root.id(), &writes),
        Err(StoreError::NotFound { .. })
    ));

    let next = child(&root, 1);
    store.put(&next).unwrap();
    assert!(matches!(
        store.put_task_writes(&thread("main"), &root.id(), &writes),
        Err(StoreError::NotHead { .. })
    ));
    let bogus = CheckpointId::from_hex(&"ab".repeat(32)).unwrap();
    assert!(matches!(
        store.put_task_writes(&thread("main"), &bogus, &writes),
        Err(StoreError::NotHead { .. })
    ));
}

fn deleting_a_thread_deletes_its_task_writes(store: &dyn Checkpointer) {
    let root = start("main");
    store.put(&root).unwrap();
    let task = root.frontier().tasks()[0].id().clone();
    store
        .put_task_writes(&thread("main"), &root.id(), &TaskWrites::new(task, vec![]))
        .unwrap();
    assert!(store.delete_thread(&thread("main")).unwrap());
    assert!(
        store
            .task_writes(&thread("main"), &root.id())
            .unwrap()
            .is_empty()
    );
    // The thread name is reusable afterwards.
    store.put(&root).unwrap();
    assert!(
        store
            .task_writes(&thread("main"), &root.id())
            .unwrap()
            .is_empty()
    );
}

fn a_long_chain_keeps_exact_order(store: &dyn Checkpointer) {
    let chain = put_chain(store, 25);
    let history = store.history(&thread("main")).unwrap();
    assert_eq!(history.len(), 25);
    for (position, summary) in history.iter().enumerate() {
        assert_eq!(summary.id, chain[position].id());
    }
    let head = store.head(&thread("main")).unwrap().unwrap();
    assert_eq!(head.step(), 24);
    assert_eq!(
        head.state().value("turns"),
        Some(&json!((1..25).sum::<i64>()))
    );
}

macro_rules! conformance {
    ($($check:ident),+ $(,)?) => {
        mod in_memory {
            $(
                #[test]
                fn $check() {
                    super::$check(&super::InMemoryCheckpointer::new());
                }
            )+
        }

        #[cfg(feature = "sqlite")]
        mod sqlite {
            $(
                #[test]
                fn $check() {
                    super::$check(&super::SqliteCheckpointer::open_in_memory().unwrap());
                }
            )+
        }
    };
}

conformance!(
    round_trip,
    unknown_threads_and_checkpoints_are_empty_not_errors,
    putting_the_head_again_is_a_no_op,
    a_stale_writer_loses_the_race,
    a_thread_cannot_start_with_a_parent,
    a_second_root_cannot_replace_an_existing_thread,
    tampered_checkpoints_are_refused,
    threads_are_listed_in_order_and_isolated,
    task_writes_are_recorded_idempotent_and_ordered,
    task_writes_are_validated_against_the_head,
    deleting_a_thread_deletes_its_task_writes,
    a_long_chain_keeps_exact_order,
);
