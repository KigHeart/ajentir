//! Crash sweep: kill the run at every storage operation, in both failure modes,
//! then resume on whatever survived and require the same final result.
//!
//! Two failure modes cover the two ways a process can die around a write:
//! *before* the write reaches the store, and *after* it landed but before the
//! caller heard back. Both are tried at every operation index of a full run.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

mod common;

use std::sync::Arc;

use ajentir::{Checkpointer, InMemoryCheckpointer};
use ajentir_run::{DriveError, DurableRun, DurableRunError, FailFast, InvalidToolCallPolicy};
use common::{FlakyStore, ScriptedModel, ScriptedTools, agent_run, thread, two_tool_script};
use futures::executor::block_on;
use rig_core::message::AssistantContent;
use rig_run::{InvalidToolCallAction, InvalidToolCallContext, PromptResponse};
use serde_json::json;

fn unknown_tool_script() -> Vec<Vec<AssistantContent>> {
    vec![
        vec![
            AssistantContent::tool_call("call-add", "add", json!({"a": 2, "b": 3})),
            AssistantContent::tool_call("call-x", "nope", json!({})),
        ],
        vec![AssistantContent::text("recovered")],
    ]
}

struct SkipInvalid;

impl InvalidToolCallPolicy for SkipInvalid {
    fn resolve(&self, _: &InvalidToolCallContext) -> InvalidToolCallAction {
        InvalidToolCallAction::skip("no such tool")
    }
}

/// Starts the run in thread `t`, or continues it if the store already has it.
fn run_to_completion<P: InvalidToolCallPolicy>(
    store: Arc<dyn Checkpointer>,
    model: &ScriptedModel,
    tools: &ScriptedTools,
    policy: &P,
) -> Result<PromptResponse, DriveError> {
    let mut run = match DurableRun::resume(store.clone(), thread("t")) {
        Err(DurableRunError::NoSuchThread(_)) => {
            DurableRun::start(store, thread("t"), agent_run())?
        }
        other => other?,
    };
    block_on(run.drive(model, tools, policy))
}

fn phases(store: &InMemoryCheckpointer) -> Vec<String> {
    store
        .history(&thread("t"))
        .unwrap()
        .iter()
        .map(|summary| {
            let checkpoint = store.get(&thread("t"), &summary.id).unwrap().unwrap();
            checkpoint.verify().unwrap();
            checkpoint.metadata()["phase"].as_str().unwrap().to_owned()
        })
        .collect()
}

fn sweep<P: InvalidToolCallPolicy>(
    script: fn() -> Vec<Vec<AssistantContent>>,
    policy: &P,
    after_effect: bool,
) {
    // The reference: the same run with no interruption.
    let reference_store = Arc::new(InMemoryCheckpointer::new());
    let counting = Arc::new(FlakyStore::healthy(reference_store.clone()));
    let reference = run_to_completion(
        counting.clone(),
        &ScriptedModel::new(script()),
        &ScriptedTools::arithmetic(),
        policy,
    )
    .unwrap();
    let reference_json = serde_json::to_value(&reference).unwrap();
    let reference_phases = phases(&reference_store);
    let reference_model_calls = script().len();
    // Every storage operation of an uninterrupted run is a crash point.
    let total_operations = counting.operations();

    let mut crashed = 0;
    let mut reached_the_end = false;
    for limit in 0..1000 {
        let inner = Arc::new(InMemoryCheckpointer::new());
        let model = ScriptedModel::new(script());
        let tools = ScriptedTools::arithmetic();
        let flaky = Arc::new(FlakyStore::new(inner.clone(), limit, after_effect));

        match run_to_completion(flaky.clone(), &model, &tools, policy) {
            Ok(response) => {
                // No crash was triggered: the limit lies beyond the whole run.
                assert!(flaky.operations() <= limit);
                assert_eq!(serde_json::to_value(&response).unwrap(), reference_json);
                reached_the_end = true;
                break;
            }
            Err(_) => {
                crashed += 1;
                let recovered = run_to_completion(inner.clone(), &model, &tools, policy)
                    .unwrap_or_else(|error| {
                        panic!("limit {limit} (after_effect={after_effect}): {error}")
                    });
                assert_eq!(
                    serde_json::to_value(&recovered).unwrap(),
                    reference_json,
                    "limit {limit} (after_effect={after_effect}): result differs"
                );
                assert_eq!(
                    phases(&inner),
                    reference_phases,
                    "limit {limit} (after_effect={after_effect}): checkpoint chain differs"
                );
                // At most one model call and one execution per tool are repeated.
                assert!(model.calls() <= reference_model_calls + 1, "limit {limit}");
                for id in ["call-add", "call-mul"] {
                    assert!(tools.executions(id) <= 2, "limit {limit}: {id}");
                }
            }
        }
    }
    assert!(
        reached_the_end,
        "the sweep never reached an uninterrupted run"
    );
    assert_eq!(
        crashed, total_operations,
        "every one of the run's {total_operations} storage operations must be crashed exactly once"
    );
}

#[test]
fn parallel_tools_survive_a_crash_at_any_storage_operation_before_the_write() {
    sweep(two_tool_script, &FailFast, false);
}

#[test]
fn parallel_tools_survive_a_crash_at_any_storage_operation_after_the_write() {
    sweep(two_tool_script, &FailFast, true);
}

#[test]
fn invalid_call_recovery_survives_a_crash_before_the_write() {
    sweep(unknown_tool_script, &SkipInvalid, false);
}

#[test]
fn invalid_call_recovery_survives_a_crash_after_the_write() {
    sweep(unknown_tool_script, &SkipInvalid, true);
}
