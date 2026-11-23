//! Resuming across a real storage boundary: a SQLite file written by one session and
//! finished by another that shares nothing with it but the file.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

mod common;

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use ajentir::{Checkpointer, SqliteCheckpointer};
use ajentir_run::{DriveError, DurableRun, FailFast, Pending, ToolError};
use assert_fs::TempDir;
use common::{ScriptedModel, ScriptedTools, agent_run, thread, two_tool_script};
use futures::executor::block_on;
use rig_core::message::ToolResultContent;

#[test]
fn a_run_interrupted_in_one_session_is_finished_by_another_from_the_file() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("runs.sqlite");
    let model = ScriptedModel::new(two_tool_script());

    // Session one: `add` finishes, `mul` aborts, so the process "dies" mid tool step.
    let mul_is_down = Arc::new(AtomicBool::new(true));
    let down = mul_is_down.clone();
    let tools = ScriptedTools::with(move |call| match call.function.name.as_str() {
        "add" => Ok(vec![ToolResultContent::text("5")]),
        _ if down.load(Ordering::SeqCst) => Err(ToolError::Abort("backend down".into())),
        _ => Ok(vec![ToolResultContent::text("20")]),
    });
    {
        let store: Arc<dyn Checkpointer> = Arc::new(SqliteCheckpointer::open(&path).unwrap());
        let mut run = DurableRun::start(store, thread("job-1"), agent_run()).unwrap();
        let error = block_on(run.drive(&model, &tools, &FailFast)).unwrap_err();
        assert!(matches!(error, DriveError::ToolAborted { .. }));
    }

    // Session two: a fresh connection to the same file.
    mul_is_down.store(false, Ordering::SeqCst);
    let store: Arc<dyn Checkpointer> = Arc::new(SqliteCheckpointer::open(&path).unwrap());
    let mut run = DurableRun::resume(store.clone(), thread("job-1")).unwrap();
    let Pending::Tools(slots) = run.pending() else {
        panic!("expected the interrupted tool step");
    };
    assert!(slots[0].result.is_some());
    assert!(slots[1].result.is_none());

    let response = block_on(run.drive(&model, &tools, &FailFast)).unwrap();
    assert_eq!(response.output, "5 and 20");
    assert_eq!(
        model.calls(),
        2,
        "the journaled model turn must not be re-requested"
    );
    assert_eq!(tools.executions("call-add"), 1);
    assert_eq!(tools.executions("call-mul"), 2);

    // The chain on disk is complete and every checkpoint verifies.
    let history = store.history(&thread("job-1")).unwrap();
    assert_eq!(history.len(), 4);
    for summary in history {
        store
            .get(&thread("job-1"), &summary.id)
            .unwrap()
            .unwrap()
            .verify()
            .unwrap();
    }
}

#[test]
fn several_runs_share_one_database_without_interfering() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("runs.sqlite");
    let store: Arc<dyn Checkpointer> = Arc::new(SqliteCheckpointer::open(&path).unwrap());

    for name in ["alpha", "beta", "gamma"] {
        let model = ScriptedModel::new(two_tool_script());
        let tools = ScriptedTools::arithmetic();
        let mut run = DurableRun::start(store.clone(), thread(name), agent_run()).unwrap();
        let response = block_on(run.drive(&model, &tools, &FailFast)).unwrap();
        assert_eq!(response.output, "5 and 20");
    }
    assert_eq!(store.threads().unwrap().len(), 3);
    for name in ["alpha", "beta", "gamma"] {
        let run = DurableRun::resume(store.clone(), thread(name)).unwrap();
        assert!(run.is_done());
    }
}
