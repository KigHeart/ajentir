//! Behaviour specific to the SQLite checkpointer: what survives a restart, what is
//! detected when the file is damaged, and what happens when several connections
//! write at once.

#![cfg(feature = "sqlite")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

mod common;

use std::{path::PathBuf, sync::Arc, thread as os_thread};

use aj_state::{CheckpointError, Checkpointer, SqliteCheckpointer, StoreError, TaskWrites, Update};
use assert_fs::TempDir;
use common::{child, start, thread};
use rusqlite::Connection;
use serde_json::json;

fn database() -> (TempDir, PathBuf) {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("checkpoints.sqlite");
    (directory, path)
}

#[test]
fn everything_survives_closing_and_reopening() {
    let (_directory, path) = database();
    let root = start("main");
    let next = child(&root, 4);
    let task = next.frontier().tasks()[0].id().clone();
    let writes = TaskWrites::new(task, vec![Update::named("turns", json!(9)).unwrap()]);
    {
        let store = SqliteCheckpointer::open(&path).unwrap();
        store.put(&root).unwrap();
        store.put(&next).unwrap();
        store
            .put_task_writes(&thread("main"), &next.id(), &writes)
            .unwrap();
    }

    let reopened = SqliteCheckpointer::open(&path).unwrap();
    assert_eq!(reopened.head(&thread("main")).unwrap().unwrap(), next);
    assert_eq!(
        reopened.get(&thread("main"), &root.id()).unwrap().unwrap(),
        root
    );
    assert_eq!(reopened.history(&thread("main")).unwrap().len(), 2);
    assert_eq!(
        reopened.task_writes(&thread("main"), &next.id()).unwrap(),
        vec![writes]
    );
    // The chain can keep growing after a restart.
    let third = child(&next, 1);
    reopened.put(&third).unwrap();
    assert_eq!(reopened.head(&thread("main")).unwrap().unwrap().step(), 2);
}

#[test]
fn an_edited_row_is_detected_not_resumed() {
    let (_directory, path) = database();
    let root = start("main");
    {
        let store = SqliteCheckpointer::open(&path).unwrap();
        store.put(&root).unwrap();
    }
    let raw = Connection::open(&path).unwrap();
    let changed = raw
        .execute(
            "UPDATE ajentir_checkpoints SET body = replace(body, 'hello', 'forged')",
            [],
        )
        .unwrap();
    assert_eq!(changed, 1);
    let body: String = raw
        .query_row("SELECT body FROM ajentir_checkpoints", [], |row| row.get(0))
        .unwrap();
    assert!(body.contains("forged") && !body.contains("hello"));
    drop(raw);

    let store = SqliteCheckpointer::open(&path).unwrap();
    assert!(matches!(
        store.get(&thread("main"), &root.id()),
        Err(StoreError::Checkpoint(
            CheckpointError::IntegrityMismatch { .. }
        ))
    ));
    assert!(matches!(
        store.head(&thread("main")),
        Err(StoreError::Checkpoint(
            CheckpointError::IntegrityMismatch { .. }
        ))
    ));
}

#[test]
fn a_row_holding_a_different_valid_checkpoint_is_detected() {
    let (_directory, path) = database();
    let root = start("main");
    let other = start("other");
    {
        let store = SqliteCheckpointer::open(&path).unwrap();
        store.put(&root).unwrap();
    }
    let raw = Connection::open(&path).unwrap();
    raw.execute(
        "UPDATE ajentir_checkpoints SET body = ?1",
        [serde_json::to_string(&other).unwrap()],
    )
    .unwrap();
    drop(raw);

    let store = SqliteCheckpointer::open(&path).unwrap();
    assert!(matches!(
        store.get(&thread("main"), &root.id()),
        Err(StoreError::Corrupt(_))
    ));
}

#[test]
fn an_undecodable_row_is_reported_as_corrupt() {
    let (_directory, path) = database();
    let root = start("main");
    {
        let store = SqliteCheckpointer::open(&path).unwrap();
        store.put(&root).unwrap();
    }
    let raw = Connection::open(&path).unwrap();
    raw.execute("UPDATE ajentir_checkpoints SET body = 'not json'", [])
        .unwrap();
    drop(raw);

    let store = SqliteCheckpointer::open(&path).unwrap();
    assert!(matches!(
        store.get(&thread("main"), &root.id()),
        Err(StoreError::Corrupt(_))
    ));
}

#[test]
fn a_database_from_a_newer_layout_is_refused() {
    let (_directory, path) = database();
    drop(SqliteCheckpointer::open(&path).unwrap());
    let raw = Connection::open(&path).unwrap();
    raw.execute_batch("PRAGMA user_version = 99;").unwrap();
    drop(raw);

    match SqliteCheckpointer::open(&path) {
        Err(StoreError::Storage(message)) => assert!(message.contains("99")),
        other => panic!("expected a layout-version error, got {other:?}"),
    }
}

#[test]
fn deleting_a_thread_removes_task_writes_from_the_file() {
    let (_directory, path) = database();
    let root = start("main");
    let task = root.frontier().tasks()[0].id().clone();
    let store = SqliteCheckpointer::open(&path).unwrap();
    store.put(&root).unwrap();
    store
        .put_task_writes(&thread("main"), &root.id(), &TaskWrites::new(task, vec![]))
        .unwrap();
    assert!(store.delete_thread(&thread("main")).unwrap());

    let raw = Connection::open(&path).unwrap();
    for table in ["ajentir_checkpoints", "ajentir_task_writes"] {
        let rows: i64 = raw
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(rows, 0, "{table} should be empty");
    }
}

#[test]
fn concurrent_connections_cannot_fork_a_thread() {
    let (_directory, path) = database();
    let root = start("main");
    SqliteCheckpointer::open(&path).unwrap().put(&root).unwrap();

    let root = Arc::new(root);
    let path = Arc::new(path);
    let workers: Vec<_> = (1..=8_i64)
        .map(|turns| {
            let root = Arc::clone(&root);
            let path = Arc::clone(&path);
            os_thread::spawn(move || {
                let store = SqliteCheckpointer::open(path.as_path()).unwrap();
                store.put(&child(&root, turns))
            })
        })
        .collect();
    let outcomes: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();

    let winners = outcomes.iter().filter(|outcome| outcome.is_ok()).count();
    assert_eq!(
        winners, 1,
        "exactly one writer may extend the root: {outcomes:?}"
    );
    for outcome in outcomes.iter().filter(|outcome| outcome.is_err()) {
        assert!(
            matches!(outcome, Err(StoreError::HeadMismatch { .. })),
            "losers must see a head mismatch, got {outcome:?}"
        );
    }
    let store = SqliteCheckpointer::open(path.as_path()).unwrap();
    assert_eq!(store.history(&thread("main")).unwrap().len(), 2);
}

#[test]
fn concurrent_connections_extend_independent_threads() {
    let (_directory, path) = database();
    let path = Arc::new(path);
    let workers: Vec<_> = (0..6)
        .map(|index| {
            let path = Arc::clone(&path);
            os_thread::spawn(move || {
                let store = SqliteCheckpointer::open(path.as_path()).unwrap();
                let name = format!("worker-{index}");
                let mut tip = start(&name);
                store.put(&tip).unwrap();
                for turn in 1..=10 {
                    tip = child(&tip, turn);
                    store.put(&tip).unwrap();
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }

    let store = SqliteCheckpointer::open(path.as_path()).unwrap();
    assert_eq!(store.threads().unwrap().len(), 6);
    for index in 0..6 {
        let history = store.history(&thread(&format!("worker-{index}"))).unwrap();
        assert_eq!(history.len(), 11);
    }
}
