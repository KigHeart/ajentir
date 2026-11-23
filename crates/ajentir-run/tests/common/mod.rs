//! Fixtures shared by the integration tests: scripted model and tools with call
//! counters, and a store wrapper that simulates a process dying mid-run.

#![allow(dead_code)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context, Poll},
};

use ajentir::{
    Checkpoint, CheckpointId, CheckpointSummary, Checkpointer, InMemoryCheckpointer, StoreError,
    TaskWrites, ThreadId,
};
use ajentir_run::{ModelCall, ModelPort, PortError, ToolError, ToolPort};
use rig_core::{
    completion::Usage,
    message::{AssistantContent, ToolCall, ToolResultContent},
};
use rig_run::{AgentRun, ModelTurn};
use serde_json::json;

pub fn thread(name: &str) -> ThreadId {
    ThreadId::new(name).unwrap()
}

pub fn agent_run() -> AgentRun {
    AgentRun::new("What is 2+3 and 4*5?").max_turns(6)
}

pub fn tool_names() -> BTreeSet<String> {
    ["add", "mul"]
        .iter()
        .map(|name| (*name).to_owned())
        .collect()
}

/// Turn 1: two parallel tool calls. Turn 2: the final answer.
pub fn two_tool_script() -> Vec<Vec<AssistantContent>> {
    vec![
        vec![
            AssistantContent::tool_call("call-add", "add", json!({"a": 2, "b": 3})),
            AssistantContent::tool_call("call-mul", "mul", json!({"a": 4, "b": 5})),
        ],
        vec![AssistantContent::text("5 and 20")],
    ]
}

/// A model that answers each turn from a fixed script, keyed by turn number so a
/// repeated call gets the same answer.
pub struct ScriptedModel {
    script: Vec<Vec<AssistantContent>>,
    names: BTreeSet<String>,
    pub calls: AtomicUsize,
}

impl ScriptedModel {
    pub fn new(script: Vec<Vec<AssistantContent>>) -> Self {
        Self {
            script,
            names: tool_names(),
            calls: AtomicUsize::new(0),
        }
    }

    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl ModelPort for ScriptedModel {
    async fn complete(&self, call: ModelCall<'_>) -> Result<ModelTurn, PortError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let choice = self
            .script
            .get(call.turn - 1)
            .cloned()
            .ok_or_else(|| PortError::from("script exhausted"))?;
        Ok(ModelTurn::new(
            None,
            choice,
            Usage::new(),
            self.names.clone(),
            self.names.clone(),
        ))
    }
}

type Behaviour = dyn Fn(&ToolCall) -> Result<Vec<ToolResultContent>, ToolError> + Send + Sync;

/// Tools with a configurable outcome, an execution counter per call id, and an
/// optional number of times to yield to the executor first (to reorder completion).
pub struct ScriptedTools {
    behaviour: Box<Behaviour>,
    yields: BTreeMap<String, usize>,
    executions: Mutex<BTreeMap<String, usize>>,
}

impl ScriptedTools {
    /// Tools that compute `add` and `mul` from `{"a": .., "b": ..}`.
    pub fn arithmetic() -> Self {
        Self::with(|call| {
            let a = call.function.arguments["a"].as_i64().unwrap_or(0);
            let b = call.function.arguments["b"].as_i64().unwrap_or(0);
            let value = match call.function.name.as_str() {
                "add" => a + b,
                "mul" => a * b,
                other => return Err(ToolError::Failed(format!("no tool {other}"))),
            };
            Ok(vec![ToolResultContent::text(value.to_string())])
        })
    }

    pub fn with(
        behaviour: impl Fn(&ToolCall) -> Result<Vec<ToolResultContent>, ToolError>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self {
            behaviour: Box::new(behaviour),
            yields: BTreeMap::new(),
            executions: Mutex::new(BTreeMap::new()),
        }
    }

    /// Makes the call with this id yield `times` times before finishing.
    pub fn yielding(mut self, call_id: &str, times: usize) -> Self {
        self.yields.insert(call_id.to_owned(), times);
        self
    }

    /// How many times the call with this id has been executed.
    pub fn executions(&self, call_id: &str) -> usize {
        self.executions
            .lock()
            .unwrap()
            .get(call_id)
            .copied()
            .unwrap_or(0)
    }

    pub fn total_executions(&self) -> usize {
        self.executions.lock().unwrap().values().sum()
    }
}

impl ToolPort for ScriptedTools {
    async fn call(&self, call: &ToolCall) -> Result<Vec<ToolResultContent>, ToolError> {
        let id = call.id.as_str().to_owned();
        *self
            .executions
            .lock()
            .unwrap()
            .entry(id.clone())
            .or_insert(0) += 1;
        if let Some(times) = self.yields.get(&id) {
            for _ in 0..*times {
                YieldNow(false).await;
            }
        }
        (self.behaviour)(call)
    }
}

/// Returns `Pending` once, then `Ready`, letting other futures run in between.
struct YieldNow(bool);

impl Future for YieldNow {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.0 {
            Poll::Ready(())
        } else {
            self.0 = true;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    }
}

/// A store that behaves like the in-memory one until told to "crash".
///
/// Operations are counted. Once the count reaches `limit`, that operation and every
/// later one fails. In `after_effect` mode the operation at `limit` is still
/// *performed* before failing — a write that landed but whose acknowledgement was
/// lost. `fail_puts` makes only checkpoint writes fail, leaving the journal usable.
pub struct FlakyStore {
    inner: Arc<InMemoryCheckpointer>,
    operations: AtomicUsize,
    limit: usize,
    after_effect: bool,
    fail_puts: AtomicBool,
}

impl FlakyStore {
    pub fn new(inner: Arc<InMemoryCheckpointer>, limit: usize, after_effect: bool) -> Self {
        Self {
            inner,
            operations: AtomicUsize::new(0),
            limit,
            after_effect,
            fail_puts: AtomicBool::new(false),
        }
    }

    /// A store that never crashes by itself (only `fail_puts` can break it).
    pub fn healthy(inner: Arc<InMemoryCheckpointer>) -> Self {
        Self::new(inner, usize::MAX, false)
    }

    pub fn fail_checkpoint_writes(&self, fail: bool) {
        self.fail_puts.store(fail, Ordering::SeqCst);
    }

    pub fn operations(&self) -> usize {
        self.operations.load(Ordering::SeqCst)
    }

    fn run<T>(
        &self,
        operation: impl FnOnce(&InMemoryCheckpointer) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let position = self.operations.fetch_add(1, Ordering::SeqCst);
        if position >= self.limit {
            if self.after_effect && position == self.limit {
                let _ = operation(&self.inner);
            }
            return Err(StoreError::Storage("simulated crash".to_owned()));
        }
        operation(&self.inner)
    }
}

impl Checkpointer for FlakyStore {
    fn put(&self, checkpoint: &Checkpoint) -> Result<(), StoreError> {
        if self.fail_puts.load(Ordering::SeqCst) {
            return Err(StoreError::Storage("checkpoint writes disabled".to_owned()));
        }
        self.run(|store| store.put(checkpoint))
    }

    fn get(&self, thread: &ThreadId, id: &CheckpointId) -> Result<Option<Checkpoint>, StoreError> {
        self.run(|store| store.get(thread, id))
    }

    fn head(&self, thread: &ThreadId) -> Result<Option<Checkpoint>, StoreError> {
        self.run(|store| store.head(thread))
    }

    fn history(&self, thread: &ThreadId) -> Result<Vec<CheckpointSummary>, StoreError> {
        self.run(|store| store.history(thread))
    }

    fn threads(&self) -> Result<Vec<ThreadId>, StoreError> {
        self.run(|store| store.threads())
    }

    fn put_task_writes(
        &self,
        thread: &ThreadId,
        checkpoint: &CheckpointId,
        writes: &TaskWrites,
    ) -> Result<(), StoreError> {
        self.run(|store| store.put_task_writes(thread, checkpoint, writes))
    }

    fn task_writes(
        &self,
        thread: &ThreadId,
        checkpoint: &CheckpointId,
    ) -> Result<Vec<TaskWrites>, StoreError> {
        self.run(|store| store.task_writes(thread, checkpoint))
    }

    fn delete_thread(&self, thread: &ThreadId) -> Result<bool, StoreError> {
        self.run(|store| store.delete_thread(thread))
    }
}
