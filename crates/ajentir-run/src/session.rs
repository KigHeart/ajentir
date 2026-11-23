//! [`DurableRun`]: an [`AgentRun`] whose every step boundary is a checkpoint.

use std::sync::Arc;

use ajentir::{Checkpoint, Checkpointer, Schema, StoreError, ThreadId};
use rig_core::message::UserContent;
use rig_run::{
    AgentRun, AgentRunStep, InvalidToolCallAction, InvalidToolCallContext, ModelTurn,
    ModelTurnOutcome, PromptError,
};
use serde_json::Value;

use crate::{
    boundary,
    error::DurableRunError,
    journal::{self, Journal, decode},
    pending::{ModelStep, PHASE_DONE, PHASE_MODEL, PHASE_TOOLS, Pending, ToolSlot},
    schema::{RUN_CHANNEL, STEP_CHANNEL, schema},
};

/// What happened to a model turn that was submitted to a [`DurableRun`].
#[derive(Debug, Clone)]
pub enum ModelOutcome {
    /// The turn was accepted and the run advanced to its next step, which is now
    /// checkpointed. Inspect it with [`DurableRun::pending`].
    Committed,
    /// The model emitted a tool call that is unknown or not allowed for the turn.
    /// Answer it with [`DurableRun::resolve_invalid_tool_call`] before anything else.
    ///
    /// Nothing is checkpointed yet: the turn itself is journaled, so after an
    /// interruption `resume` offers it again and the same question is asked again.
    NeedsResolution(InvalidToolCallContext),
}

/// A single agent run that survives interruption.
///
/// The run's decisions — turn counting, tool-call validation, history threading,
/// usage and the final response — are made by the sans-IO [`AgentRun`] state
/// machine; this type adds durability around it. At every *step boundary* (the run
/// has just produced its next step and nobody has acted on it) the machine and the
/// step are stored as a checkpoint in a [`Checkpointer`], with the required work as
/// the checkpoint's frontier. Between boundaries, finished work is journaled as it
/// completes. See the crate documentation for the full recovery semantics.
///
/// The type performs no IO besides storage; the caller (or
/// [`DurableRun::drive`]) calls the model and tools.
///
/// Storage calls are synchronous and run on the calling thread. On an async runtime
/// with a slow store, run `drive` where blocking is acceptable.
pub struct DurableRun {
    store: Arc<dyn Checkpointer>,
    schema: Schema,
    head: Checkpoint,
    run: AgentRun,
    step: AgentRunStep,
    journal: Journal,
    resolving: Option<InvalidToolCallContext>,
    poisoned: bool,
}

impl std::fmt::Debug for DurableRun {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DurableRun")
            .field("thread", self.head.thread())
            .field("checkpoint", &self.head.id())
            .field("waiting_for", &self.phase())
            .field("poisoned", &self.poisoned)
            .finish_non_exhaustive()
    }
}

impl DurableRun {
    /// Begins a new durable run in `thread`.
    ///
    /// Asks `run` for its first step and stores the first checkpoint. Configure the
    /// run (history, `max_turns`, output validation, …) before passing it in.
    ///
    /// # Errors
    ///
    /// Returns [`DurableRunError::ThreadExists`] if the thread already holds
    /// checkpoints, [`DurableRunError::Run`] if the machine rejects the first step,
    /// or a store error.
    pub fn start(
        store: Arc<dyn Checkpointer>,
        thread: ThreadId,
        mut run: AgentRun,
    ) -> Result<Self, DurableRunError> {
        if store.head(&thread)?.is_some() {
            return Err(DurableRunError::ThreadExists(thread));
        }
        let schema = schema()?;
        let step = run.next_step()?;
        let boundary = boundary::build(&run, &step, 0)?;
        let head = Checkpoint::initial(
            thread.clone(),
            &schema,
            &boundary.batch,
            boundary.frontier,
            boundary.metadata,
        )?;
        match store.put(&head) {
            Ok(()) => {}
            Err(StoreError::HeadMismatch { .. }) => {
                return Err(DurableRunError::ThreadExists(thread));
            }
            Err(other) => return Err(other.into()),
        }
        Ok(Self {
            store,
            schema,
            head,
            run,
            step,
            journal: Journal::default(),
            resolving: None,
            poisoned: false,
        })
    }

    /// Continues the run stored in `thread` from its latest checkpoint.
    ///
    /// Everything journaled against that checkpoint is loaded, so work that had
    /// finished before the interruption is not repeated.
    ///
    /// # Errors
    ///
    /// Returns [`DurableRunError::NoSuchThread`] for an empty thread,
    /// [`DurableRunError::Checkpoint`] if the checkpoint was not written under this
    /// crate's schema or fails verification, [`DurableRunError::Corrupt`] or
    /// [`DurableRunError::Codec`] if its contents do not decode, or a store error.
    pub fn resume(store: Arc<dyn Checkpointer>, thread: ThreadId) -> Result<Self, DurableRunError> {
        let head = store
            .head(&thread)?
            .ok_or(DurableRunError::NoSuchThread(thread))?;
        let schema = schema()?;
        head.validate_against(&schema)?;
        let run: AgentRun = decode(stored(&head, RUN_CHANNEL)?, "agent run")?;
        let step: AgentRunStep = decode(stored(&head, STEP_CHANNEL)?, "pending step")?;
        let journal = journal::load(store.as_ref(), &head)?;
        Ok(Self {
            store,
            schema,
            head,
            run,
            step,
            journal,
            resolving: None,
            poisoned: false,
        })
    }

    /// The thread this run is stored in.
    #[must_use]
    pub fn thread(&self) -> &ThreadId {
        self.head.thread()
    }

    /// The latest checkpoint.
    #[must_use]
    pub fn checkpoint(&self) -> &Checkpoint {
        &self.head
    }

    /// The agent state machine as of the latest checkpoint.
    #[must_use]
    pub fn run(&self) -> &AgentRun {
        &self.run
    }

    /// Whether the run has finished.
    #[must_use]
    pub fn is_done(&self) -> bool {
        matches!(self.step, AgentRunStep::Done(_))
    }

    /// What the run needs next, including any work already journaled for it.
    #[must_use]
    pub fn pending(&self) -> Pending {
        match &self.step {
            AgentRunStep::CallModel {
                prompt,
                history,
                turn,
            } => Pending::Model(ModelStep {
                prompt: prompt.clone(),
                history: history.clone(),
                turn: *turn,
                journaled: self.journal.model_turn.clone(),
            }),
            AgentRunStep::CallTools { calls } => Pending::Tools(
                calls
                    .iter()
                    .enumerate()
                    .map(|(index, call)| ToolSlot {
                        index,
                        call: call.clone(),
                        result: call
                            .preresolved_result
                            .clone()
                            .or_else(|| self.journal.tool_results.get(&index).cloned()),
                    })
                    .collect(),
            ),
            AgentRunStep::Done(response) => Pending::Done(response.clone()),
        }
    }

    fn phase(&self) -> &'static str {
        match self.step {
            AgentRunStep::CallModel { .. } => PHASE_MODEL,
            AgentRunStep::CallTools { .. } => PHASE_TOOLS,
            AgentRunStep::Done(_) => PHASE_DONE,
        }
    }

    fn ensure_usable(&self) -> Result<(), DurableRunError> {
        if self.poisoned {
            Err(DurableRunError::Poisoned)
        } else {
            Ok(())
        }
    }

    /// Runs an operation on the state machine, poisoning the session if it fails
    /// because the machine may then be partly advanced.
    fn guard<T>(
        &mut self,
        operation: impl FnOnce(&mut AgentRun) -> Result<T, PromptError>,
    ) -> Result<T, DurableRunError> {
        match operation(&mut self.run) {
            Ok(value) => Ok(value),
            Err(error) => {
                self.poisoned = true;
                Err(error.into())
            }
        }
    }

    /// Submits the model's response for the pending model call.
    ///
    /// The response is journaled first — so a crash right after cannot lose a billed
    /// model call — and then applied. If the run accepts it, the next step is
    /// checkpointed.
    ///
    /// # Errors
    ///
    /// Returns [`DurableRunError::WrongPhase`] unless the run is waiting for a model
    /// call, [`DurableRunError::ResolutionPending`] while an invalid tool call is
    /// unresolved, [`DurableRunError::Store`] (nothing recorded, safe to retry) if
    /// journaling fails or a *different* response was already journaled for this
    /// step, and [`DurableRunError::Run`] if the machine rejects the turn.
    pub fn complete_model(&mut self, turn: ModelTurn) -> Result<ModelOutcome, DurableRunError> {
        self.ensure_usable()?;
        if self.resolving.is_some() {
            return Err(DurableRunError::ResolutionPending);
        }
        self.expect(PHASE_MODEL, |step| {
            matches!(step, AgentRunStep::CallModel { .. })
        })?;
        journal::write_model_turn(self.store.as_ref(), &self.head, &turn)?;
        let outcome = self.guard(|run| run.model_response(turn))?;
        self.after_outcome(outcome)
    }

    /// Answers the invalid tool call reported by [`ModelOutcome::NeedsResolution`].
    ///
    /// The action may itself lead to another invalid call, which is reported the
    /// same way.
    ///
    /// # Errors
    ///
    /// Returns [`DurableRunError::NothingToResolve`] if no invalid call is
    /// outstanding and [`DurableRunError::Run`] if the machine rejects the action
    /// (for example [`InvalidToolCallAction::Fail`], which fails the run).
    pub fn resolve_invalid_tool_call(
        &mut self,
        action: InvalidToolCallAction,
    ) -> Result<ModelOutcome, DurableRunError> {
        self.ensure_usable()?;
        if self.resolving.is_none() {
            return Err(DurableRunError::NothingToResolve);
        }
        let outcome = self.guard(|run| run.resolve_invalid_tool_call(action))?;
        self.after_outcome(outcome)
    }

    fn after_outcome(
        &mut self,
        outcome: ModelTurnOutcome,
    ) -> Result<ModelOutcome, DurableRunError> {
        match outcome {
            ModelTurnOutcome::Continue { .. } | ModelTurnOutcome::TurnRetried => {
                self.resolving = None;
                self.advance()?;
                Ok(ModelOutcome::Committed)
            }
            ModelTurnOutcome::NeedsResolution(context) => {
                self.resolving = Some(context.clone());
                Ok(ModelOutcome::NeedsResolution(context))
            }
        }
    }

    /// Records the result of the tool call at `index` of the pending tool step.
    ///
    /// The result is journaled immediately. Recording the same result again is
    /// harmless.
    ///
    /// # Errors
    ///
    /// Returns [`DurableRunError::WrongPhase`] unless the run is waiting for tool
    /// calls, [`DurableRunError::NoSuchToolCall`] for a bad position, and
    /// [`DurableRunError::ToolResultConflict`] if the call already has a different
    /// result (including one supplied by invalid-call recovery).
    pub fn record_tool_result(
        &mut self,
        index: usize,
        result: UserContent,
    ) -> Result<(), DurableRunError> {
        self.ensure_usable()?;
        self.check_tool_slot(index, &result)?;
        if self.slot_is_preresolved(index) {
            return Ok(());
        }
        self.journal_tool_result(index, &result)?;
        self.note_tool_result(index, result);
        Ok(())
    }

    /// Writes a tool result to the journal without touching in-memory state, so
    /// several calls can be journaled concurrently from `&self`.
    pub(crate) fn journal_tool_result(
        &self,
        index: usize,
        result: &UserContent,
    ) -> Result<(), DurableRunError> {
        journal::write_tool_result(self.store.as_ref(), &self.head, index, result)
    }

    /// Remembers an already-journaled result in memory.
    pub(crate) fn note_tool_result(&mut self, index: usize, result: UserContent) {
        self.journal.tool_results.insert(index, result);
    }

    fn check_tool_slot(&self, index: usize, result: &UserContent) -> Result<(), DurableRunError> {
        let AgentRunStep::CallTools { calls } = &self.step else {
            return Err(DurableRunError::WrongPhase {
                expected: PHASE_TOOLS,
                actual: self.phase(),
            });
        };
        let call = calls
            .get(index)
            .ok_or(DurableRunError::NoSuchToolCall(index))?;
        match &call.preresolved_result {
            Some(existing) if existing != result => Err(DurableRunError::ToolResultConflict(index)),
            _ => Ok(()),
        }
    }

    fn slot_is_preresolved(&self, index: usize) -> bool {
        match &self.step {
            AgentRunStep::CallTools { calls } => calls
                .get(index)
                .is_some_and(|call| call.preresolved_result.is_some()),
            _ => false,
        }
    }

    /// Hands every call's result to the state machine and checkpoints the step that
    /// follows.
    ///
    /// # Errors
    ///
    /// Returns [`DurableRunError::WrongPhase`] unless the run is waiting for tool
    /// calls, [`DurableRunError::ToolsIncomplete`] if any call still lacks a result,
    /// and [`DurableRunError::Run`] if the machine rejects the results.
    pub fn complete_tools(&mut self) -> Result<(), DurableRunError> {
        self.ensure_usable()?;
        let results = self.gather_tool_results()?;
        self.guard(|run| run.tool_results(results))?;
        self.advance()
    }

    fn gather_tool_results(&self) -> Result<Vec<UserContent>, DurableRunError> {
        let AgentRunStep::CallTools { calls } = &self.step else {
            return Err(DurableRunError::WrongPhase {
                expected: PHASE_TOOLS,
                actual: self.phase(),
            });
        };
        let mut results = Vec::with_capacity(calls.len());
        let mut missing = Vec::new();
        for (index, call) in calls.iter().enumerate() {
            let known = call
                .preresolved_result
                .clone()
                .or_else(|| self.journal.tool_results.get(&index).cloned());
            match known {
                Some(result) => results.push(result),
                None => missing.push(index),
            }
        }
        if missing.is_empty() {
            Ok(results)
        } else {
            Err(DurableRunError::ToolsIncomplete { missing })
        }
    }

    fn expect(
        &self,
        expected: &'static str,
        is_right_phase: impl FnOnce(&AgentRunStep) -> bool,
    ) -> Result<(), DurableRunError> {
        if is_right_phase(&self.step) {
            Ok(())
        } else {
            Err(DurableRunError::WrongPhase {
                expected,
                actual: self.phase(),
            })
        }
    }

    /// Asks the machine for its next step and commits the boundary as a checkpoint.
    ///
    /// By the time this runs the machine has already moved on, so any failure here
    /// poisons the session: the stored checkpoint is still valid, the in-memory
    /// machine no longer matches it.
    fn advance(&mut self) -> Result<(), DurableRunError> {
        let outcome = self.advance_inner();
        if outcome.is_err() {
            self.poisoned = true;
        }
        outcome
    }

    fn advance_inner(&mut self) -> Result<(), DurableRunError> {
        let next = self.run.next_step()?;
        let boundary = boundary::build(&self.run, &next, self.head.next_step())?;
        let checkpoint = self.head.advance(
            &self.schema,
            &boundary.batch,
            boundary.frontier,
            boundary.metadata,
        )?;
        self.store.put(&checkpoint)?;
        self.head = checkpoint;
        self.step = next;
        self.journal = Journal::default();
        self.resolving = None;
        Ok(())
    }
}

/// Reads a channel of a stored checkpoint, which must be present.
fn stored<'a>(head: &'a Checkpoint, channel: &str) -> Result<&'a Value, DurableRunError> {
    head.state().value(channel).ok_or_else(|| {
        DurableRunError::Corrupt(format!("checkpoint has no `{channel}` channel value"))
    })
}
