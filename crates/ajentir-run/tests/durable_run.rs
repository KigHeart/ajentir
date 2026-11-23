//! Behaviour of a durable run: checkpoints at every boundary, journaling, resume,
//! invalid tool calls, and the documented failure modes.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

mod common;

use std::sync::Arc;

use ajentir::{Checkpointer, InMemoryCheckpointer, TaskWrites, Update};
use ajentir_run::{
    DriveError, DurableRun, DurableRunError, FailFast, ModelOutcome, Pending, ToolError,
};
use common::{
    FlakyStore, ScriptedModel, ScriptedTools, agent_run, thread, tool_names, two_tool_script,
};
use futures::executor::block_on;
use rig_core::{
    completion::Usage,
    message::{AssistantContent, ToolResultContent, UserContent},
};
use rig_run::{InvalidToolCallAction, ModelTurn};
use serde_json::json;

fn store() -> Arc<InMemoryCheckpointer> {
    Arc::new(InMemoryCheckpointer::new())
}

fn phases(store: &InMemoryCheckpointer, name: &str) -> Vec<String> {
    let thread = thread(name);
    let mut phases = Vec::new();
    for summary in store.history(&thread).unwrap() {
        let checkpoint = store.get(&thread, &summary.id).unwrap().unwrap();
        phases.push(checkpoint.metadata()["phase"].as_str().unwrap().to_owned());
    }
    phases
}

#[test]
fn a_run_without_tools_finishes_with_two_checkpoints() {
    let store = store();
    let model = ScriptedModel::new(vec![vec![AssistantContent::text("hello")]]);
    let tools = ScriptedTools::arithmetic();
    let mut run = DurableRun::start(store.clone(), thread("t"), agent_run()).unwrap();
    assert!(matches!(run.pending(), Pending::Model(_)));
    assert!(!run.is_done());

    let response = block_on(run.drive(&model, &tools, &FailFast)).unwrap();
    assert_eq!(response.output, "hello");
    assert!(run.is_done());
    assert_eq!(phases(&store, "t"), vec!["model", "done"]);
    assert!(run.checkpoint().frontier().is_empty());
    assert_eq!(tools.total_executions(), 0);
}

#[test]
fn tool_calls_execute_once_and_their_results_reach_the_model() {
    let store = store();
    let model = ScriptedModel::new(two_tool_script());
    let tools = ScriptedTools::arithmetic();
    let mut run = DurableRun::start(store.clone(), thread("t"), agent_run()).unwrap();

    let response = block_on(run.drive(&model, &tools, &FailFast)).unwrap();
    assert_eq!(response.output, "5 and 20");
    assert_eq!(model.calls(), 2);
    assert_eq!(tools.executions("call-add"), 1);
    assert_eq!(tools.executions("call-mul"), 1);
    assert_eq!(phases(&store, "t"), vec!["model", "tools", "model", "done"]);

    let history = serde_json::to_string(&response.messages).unwrap();
    assert!(history.contains(r#""text":"5""#), "{history}");
    assert!(history.contains(r#""text":"20""#), "{history}");
}

#[test]
fn the_tool_step_checkpoint_has_one_task_per_call() {
    let store = store();
    let mut run = DurableRun::start(store.clone(), thread("t"), agent_run()).unwrap();
    let turn = ModelTurn::new(
        None,
        two_tool_script().remove(0),
        Usage::new(),
        tool_names(),
        tool_names(),
    );
    assert!(matches!(
        run.complete_model(turn).unwrap(),
        ModelOutcome::Committed
    ));
    let frontier = run.checkpoint().frontier();
    assert_eq!(frontier.len(), 2);
    assert!(
        frontier
            .tasks()
            .iter()
            .all(|task| task.node().as_str() == "tool")
    );
    let Pending::Tools(slots) = run.pending() else {
        panic!("expected a tool step");
    };
    assert_eq!(slots.len(), 2);
    assert!(slots.iter().all(|slot| slot.result.is_none()));
}

#[test]
fn start_refuses_a_thread_that_already_exists() {
    let store = store();
    DurableRun::start(store.clone(), thread("t"), agent_run()).unwrap();
    assert!(matches!(
        DurableRun::start(store, thread("t"), agent_run()),
        Err(DurableRunError::ThreadExists(_))
    ));
}

#[test]
fn resume_needs_an_existing_thread_under_this_crates_schema() {
    let store = store();
    assert!(matches!(
        DurableRun::resume(store.clone(), thread("missing")),
        Err(DurableRunError::NoSuchThread(_))
    ));

    // A checkpoint written under some other schema is not a durable run.
    let foreign = ajentir::Schema::builder("something-else", 1)
        .channel(
            "x",
            ajentir::ValueType::Any,
            ajentir::ReducerKind::Overwrite,
        )
        .build()
        .unwrap();
    let checkpoint = ajentir::Checkpoint::initial(
        thread("foreign"),
        &foreign,
        &ajentir::UpdateBatch::empty(),
        ajentir::Frontier::empty(),
        Default::default(),
    )
    .unwrap();
    store.put(&checkpoint).unwrap();
    assert!(matches!(
        DurableRun::resume(store, thread("foreign")),
        Err(DurableRunError::Checkpoint(_))
    ));
}

#[test]
fn resume_continues_from_the_stored_boundary() {
    let store = store();
    let model = ScriptedModel::new(two_tool_script());
    let tools = ScriptedTools::arithmetic();
    {
        let mut run = DurableRun::start(store.clone(), thread("t"), agent_run()).unwrap();
        let Pending::Model(step) = run.pending() else {
            panic!("expected a model step");
        };
        assert_eq!(step.turn, 1);
        let turn = block_on(ajentir_run::ModelPort::complete(
            &model,
            ajentir_run::ModelCall {
                prompt: &step.prompt,
                history: &step.history,
                turn: step.turn,
            },
        ))
        .unwrap();
        run.complete_model(turn).unwrap();
        // The process "dies" here: the session is dropped with tools pending.
    }
    let mut resumed = DurableRun::resume(store.clone(), thread("t")).unwrap();
    assert!(matches!(resumed.pending(), Pending::Tools(_)));
    assert_eq!(resumed.checkpoint().step(), 1);

    let response = block_on(resumed.drive(&model, &tools, &FailFast)).unwrap();
    assert_eq!(response.output, "5 and 20");
    assert_eq!(model.calls(), 2, "turn 1 must not be requested again");
}

#[test]
fn a_journaled_model_response_is_never_requested_again() {
    let inner = store();
    let model = ScriptedModel::new(two_tool_script());
    let tools = ScriptedTools::arithmetic();
    let flaky = Arc::new(FlakyStore::healthy(inner.clone()));

    // The provider answers, the response is journaled, then the checkpoint write
    // fails — the process effectively dies between the two.
    {
        let mut run = DurableRun::start(flaky.clone(), thread("t"), agent_run()).unwrap();
        flaky.fail_checkpoint_writes(true);
        let error = block_on(run.drive(&model, &tools, &FailFast)).unwrap_err();
        assert!(
            matches!(error, DriveError::Run(DurableRunError::Store(_))),
            "{error}"
        );
        assert_eq!(model.calls(), 1);
        assert!(matches!(
            run.complete_tools().unwrap_err(),
            DurableRunError::Poisoned
        ));
    }

    let mut resumed = DurableRun::resume(inner.clone(), thread("t")).unwrap();
    let Pending::Model(step) = resumed.pending() else {
        panic!("expected the model step to be pending again");
    };
    assert!(
        step.journaled.is_some(),
        "the response must have been journaled"
    );

    let response = block_on(resumed.drive(&model, &tools, &FailFast)).unwrap();
    assert_eq!(response.output, "5 and 20");
    assert_eq!(
        model.calls(),
        2,
        "turn 1 was served from the journal, turn 2 called once"
    );
}

#[test]
fn finished_tool_results_survive_a_failing_sibling_and_are_not_rerun() {
    let inner = store();
    let model = ScriptedModel::new(two_tool_script());
    let abort_mul = std::sync::atomic::AtomicBool::new(true);
    let abort_mul = Arc::new(abort_mul);
    let flag = abort_mul.clone();
    let tools = ScriptedTools::with(move |call| match call.function.name.as_str() {
        "add" => Ok(vec![ToolResultContent::text("5")]),
        _ if flag.load(std::sync::atomic::Ordering::SeqCst) => {
            Err(ToolError::Abort("tool backend is down".into()))
        }
        _ => Ok(vec![ToolResultContent::text("20")]),
    });

    let mut run = DurableRun::start(inner.clone(), thread("t"), agent_run()).unwrap();
    let error = block_on(run.drive(&model, &tools, &FailFast)).unwrap_err();
    assert!(matches!(error, DriveError::ToolAborted { ref tool, .. } if tool == "mul"));
    assert_eq!(tools.executions("call-add"), 1);
    assert_eq!(tools.executions("call-mul"), 1);

    // The aborted call recorded nothing; the finished one is journaled.
    let mut resumed = DurableRun::resume(inner.clone(), thread("t")).unwrap();
    let Pending::Tools(slots) = resumed.pending() else {
        panic!("expected the tool step");
    };
    assert!(
        slots[0].result.is_some(),
        "add finished and must be journaled"
    );
    assert!(slots[1].result.is_none(), "mul aborted and must be pending");

    abort_mul.store(false, std::sync::atomic::Ordering::SeqCst);
    let response = block_on(resumed.drive(&model, &tools, &FailFast)).unwrap();
    assert_eq!(response.output, "5 and 20");
    assert_eq!(
        tools.executions("call-add"),
        1,
        "a journaled result must not be recomputed"
    );
    assert_eq!(tools.executions("call-mul"), 2);
}

#[test]
fn completion_order_of_parallel_tools_does_not_matter() {
    // `add` finishes last; results are matched to calls by id, not by arrival.
    let tools = ScriptedTools::arithmetic().yielding("call-add", 3);
    let model = ScriptedModel::new(two_tool_script());
    let mut run = DurableRun::start(store(), thread("t"), agent_run()).unwrap();
    let response = block_on(run.drive(&model, &tools, &FailFast)).unwrap();
    assert_eq!(response.output, "5 and 20");
}

#[test]
fn a_tool_failure_is_reported_to_the_model_and_the_run_continues() {
    let tools = ScriptedTools::with(|_| Err(ToolError::Failed("division by zero".to_owned())));
    let model = ScriptedModel::new(two_tool_script());
    let mut run = DurableRun::start(store(), thread("t"), agent_run()).unwrap();
    let response = block_on(run.drive(&model, &tools, &FailFast)).unwrap();
    assert_eq!(response.output, "5 and 20");
    let history = serde_json::to_string(&response.messages).unwrap();
    assert!(history.contains("division by zero"), "{history}");
}

#[test]
fn driving_a_finished_run_returns_the_response_again() {
    let model = ScriptedModel::new(vec![vec![AssistantContent::text("hi")]]);
    let tools = ScriptedTools::arithmetic();
    let mut run = DurableRun::start(store(), thread("t"), agent_run()).unwrap();
    let first = block_on(run.drive(&model, &tools, &FailFast)).unwrap();
    let second = block_on(run.drive(&model, &tools, &FailFast)).unwrap();
    assert_eq!(first.output, second.output);
    assert_eq!(model.calls(), 1);
}

fn unknown_tool_script() -> Vec<Vec<AssistantContent>> {
    vec![
        vec![AssistantContent::tool_call("call-x", "nope", json!({}))],
        vec![AssistantContent::text("recovered")],
    ]
}

#[test]
fn fail_fast_turns_an_invalid_tool_call_into_a_failed_run_that_stays_resumable() {
    let inner = store();
    let model = ScriptedModel::new(unknown_tool_script());
    let tools = ScriptedTools::arithmetic();
    let mut run = DurableRun::start(inner.clone(), thread("t"), agent_run()).unwrap();
    let error = block_on(run.drive(&model, &tools, &FailFast)).unwrap_err();
    assert!(
        matches!(error, DriveError::Run(DurableRunError::Run(_))),
        "{error}"
    );
    assert!(matches!(
        run.complete_tools().unwrap_err(),
        DurableRunError::Poisoned
    ));

    // The stored run is untouched and the model's answer is journaled.
    let resumed = DurableRun::resume(inner, thread("t")).unwrap();
    let Pending::Model(step) = resumed.pending() else {
        panic!("expected the model step");
    };
    assert!(step.journaled.is_some());
    assert_eq!(model.calls(), 1);
}

#[test]
fn a_policy_can_skip_an_invalid_tool_call() {
    let model = ScriptedModel::new(unknown_tool_script());
    let tools = ScriptedTools::arithmetic();
    let skip = |_: &rig_run::InvalidToolCallContext| InvalidToolCallAction::skip("no such tool");
    let mut run = DurableRun::start(store(), thread("t"), agent_run()).unwrap();
    let response = block_on(run.drive(&model, &tools, &skip)).unwrap();
    assert_eq!(response.output, "recovered");
    assert_eq!(
        tools.total_executions(),
        0,
        "a skipped call is never executed"
    );
}

#[test]
fn the_manual_resolution_flow_reports_the_invalid_call_first() {
    let mut run = DurableRun::start(store(), thread("t"), agent_run()).unwrap();
    let turn = ModelTurn::new(
        None,
        unknown_tool_script().remove(0),
        Usage::new(),
        tool_names(),
        tool_names(),
    );
    let ModelOutcome::NeedsResolution(context) = run.complete_model(turn.clone()).unwrap() else {
        panic!("expected an invalid tool call");
    };
    assert_eq!(context.tool_name, "nope");

    // Nothing else may proceed until it is answered.
    assert!(matches!(
        run.complete_model(turn).unwrap_err(),
        DurableRunError::ResolutionPending
    ));
    let outcome = run
        .resolve_invalid_tool_call(InvalidToolCallAction::skip("unknown"))
        .unwrap();
    assert!(matches!(outcome, ModelOutcome::Committed));
    assert!(matches!(
        run.resolve_invalid_tool_call(InvalidToolCallAction::fail())
            .unwrap_err(),
        DurableRunError::NothingToResolve
    ));
}

#[test]
fn operations_in_the_wrong_phase_are_refused_without_harm() {
    let mut run = DurableRun::start(store(), thread("t"), agent_run()).unwrap();
    assert!(matches!(
        run.complete_tools().unwrap_err(),
        DurableRunError::WrongPhase { .. }
    ));
    let result = UserContent::tool_result("c", "add", vec![ToolResultContent::text("1")]);
    assert!(matches!(
        run.record_tool_result(0, result).unwrap_err(),
        DurableRunError::WrongPhase { .. }
    ));
    // Still fully usable afterwards.
    let model = ScriptedModel::new(vec![vec![AssistantContent::text("ok")]]);
    let response = block_on(run.drive(&model, &ScriptedTools::arithmetic(), &FailFast)).unwrap();
    assert_eq!(response.output, "ok");
}

#[test]
fn tool_results_can_be_recorded_by_hand_and_must_be_complete() {
    let mut run = DurableRun::start(store(), thread("t"), agent_run()).unwrap();
    let turn = ModelTurn::new(
        None,
        two_tool_script().remove(0),
        Usage::new(),
        tool_names(),
        tool_names(),
    );
    run.complete_model(turn).unwrap();
    let Pending::Tools(slots) = run.pending() else {
        panic!("expected the tool step");
    };
    let answer = |index: usize, text: &str| {
        let call = &slots[index].call.tool_call;
        UserContent::tool_result_for(
            call.id.clone(),
            call.provider.clone(),
            call.function.name.clone(),
            vec![ToolResultContent::text(text)],
        )
    };

    assert!(matches!(
        run.record_tool_result(9, answer(0, "5")).unwrap_err(),
        DurableRunError::NoSuchToolCall(9)
    ));
    run.record_tool_result(0, answer(0, "5")).unwrap();
    run.record_tool_result(0, answer(0, "5")).unwrap();
    assert!(matches!(
        run.record_tool_result(0, answer(0, "6")).unwrap_err(),
        DurableRunError::ToolResultConflict(0)
    ));
    assert!(matches!(
        run.complete_tools().unwrap_err(),
        DurableRunError::ToolsIncomplete { ref missing } if missing == &vec![1]
    ));
    run.record_tool_result(1, answer(1, "20")).unwrap();
    run.complete_tools().unwrap();
    assert!(matches!(run.pending(), Pending::Model(_)));
    assert_eq!(run.checkpoint().step(), 2);
}

#[test]
fn a_different_model_response_for_a_journaled_step_is_refused_without_harm() {
    let inner = store();
    let model = ScriptedModel::new(two_tool_script());
    let tools = ScriptedTools::arithmetic();
    let flaky = Arc::new(FlakyStore::healthy(inner.clone()));

    // Journal the real response, then lose the checkpoint write.
    {
        let mut run = DurableRun::start(flaky.clone(), thread("t"), agent_run()).unwrap();
        flaky.fail_checkpoint_writes(true);
        let _ = block_on(run.drive(&model, &tools, &FailFast));
    }

    let mut resumed = DurableRun::resume(inner.clone(), thread("t")).unwrap();
    let different = ModelTurn::new(
        None,
        vec![AssistantContent::text("a different answer")],
        Usage::new(),
        tool_names(),
        tool_names(),
    );
    let error = resumed.complete_model(different).unwrap_err();
    assert!(
        matches!(
            error,
            DurableRunError::Store(ajentir::StoreError::TaskWritesConflict(_))
        ),
        "{error}"
    );

    // Refusing it changed nothing: the session is not poisoned and the journaled
    // response still drives the run to the right answer.
    let response = block_on(resumed.drive(&model, &tools, &FailFast)).unwrap();
    assert_eq!(response.output, "5 and 20");
}

#[test]
fn a_second_session_on_the_same_thread_cannot_fork_history() {
    let inner = store();
    DurableRun::start(inner.clone(), thread("t"), agent_run()).unwrap();
    let mut first = DurableRun::resume(inner.clone(), thread("t")).unwrap();
    let mut second = DurableRun::resume(inner.clone(), thread("t")).unwrap();
    let turn = || {
        ModelTurn::new(
            None,
            two_tool_script().remove(0),
            Usage::new(),
            tool_names(),
            tool_names(),
        )
    };

    first.complete_model(turn()).unwrap();
    let error = second.complete_model(turn()).unwrap_err();
    assert!(
        matches!(
            error,
            DurableRunError::Store(ajentir::StoreError::NotHead { .. })
        ),
        "{error}"
    );

    // History stayed linear: initial + the first session's step, nothing forked.
    assert_eq!(inner.history(&thread("t")).unwrap().len(), 2);
    // The loser is not poisoned and can catch up by resuming.
    let caught_up = DurableRun::resume(inner, thread("t")).unwrap();
    assert!(matches!(caught_up.pending(), Pending::Tools(_)));
}

#[test]
fn journal_entries_use_only_the_journal_channel_and_one_update() {
    let inner = store();
    let model = ScriptedModel::new(two_tool_script());
    let tools = ScriptedTools::with(|_| Err(ToolError::Abort("down".into())));
    let mut run = DurableRun::start(inner.clone(), thread("t"), agent_run()).unwrap();
    let _ = block_on(run.drive(&model, &tools, &FailFast));

    // The model turn of step 0 was committed; its journal belongs to checkpoint 0.
    let history = inner.history(&thread("t")).unwrap();
    let first: &TaskWrites = &inner.task_writes(&thread("t"), &history[0].id).unwrap()[0];
    let update: &Update = &first.updates()[0];
    assert_eq!(update.channel().as_str(), "journal");
    assert_eq!(first.updates().len(), 1);
}
