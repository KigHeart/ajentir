//! The driver loop: runs a [`DurableRun`] to completion through the ports.

use futures::future::join_all;
use rig_core::message::{ToolResultContent, UserContent};
use rig_run::PromptResponse;

use crate::{
    error::{DriveError, DurableRunError},
    pending::{Pending, ToolSlot},
    ports::{InvalidToolCallPolicy, ModelCall, ModelPort, ToolError, ToolPort},
    session::{DurableRun, ModelOutcome},
};

impl DurableRun {
    /// Drives the run until it finishes, resuming wherever it currently stands.
    ///
    /// Each pass does whatever the run needs next:
    ///
    /// - **Model call:** if a response was already journaled (the process died after
    ///   the provider answered), it is used and the model is *not* called again;
    ///   otherwise `model` is called. An invalid tool call in the response is
    ///   settled by `policy`.
    /// - **Tool step:** every call that has no result yet runs concurrently on the
    ///   current task. Each result is journaled the moment its call finishes, so a
    ///   crash or a failing sibling never discards completed work. When all calls
    ///   have results the step is completed and the next boundary checkpointed.
    ///
    /// Calling `drive` on a finished run returns its response again.
    ///
    /// # Errors
    ///
    /// Returns [`DriveError`]. Every error leaves the run intact at its last
    /// checkpoint plus its journal, so calling `drive` again — or `resume` in a new
    /// process — carries on. Errors from the agent state machine itself poison this
    /// in-memory session; `resume` to continue.
    pub async fn drive<M, T, P>(
        &mut self,
        model: &M,
        tools: &T,
        policy: &P,
    ) -> Result<PromptResponse, DriveError>
    where
        M: ModelPort,
        T: ToolPort,
        P: InvalidToolCallPolicy,
    {
        loop {
            match self.pending() {
                Pending::Done(response) => return Ok(response),
                Pending::Model(step) => {
                    let turn = match step.journaled {
                        Some(turn) => turn,
                        None => model
                            .complete(ModelCall {
                                prompt: &step.prompt,
                                history: &step.history,
                                turn: step.turn,
                            })
                            .await
                            .map_err(DriveError::Model)?,
                    };
                    let mut outcome = self.complete_model(turn)?;
                    while let ModelOutcome::NeedsResolution(context) = outcome {
                        outcome = self.resolve_invalid_tool_call(policy.resolve(&context))?;
                    }
                }
                Pending::Tools(slots) => self.run_tools(tools, &slots).await?,
            }
        }
    }

    /// Executes the unanswered calls of a tool step, then completes the step.
    async fn run_tools<T: ToolPort>(
        &mut self,
        tools: &T,
        slots: &[ToolSlot],
    ) -> Result<(), DriveError> {
        let executions = slots
            .iter()
            .filter(|slot| slot.result.is_none())
            .map(|slot| self.execute_tool(tools, slot));
        let finished = join_all(executions).await;

        // Every call has now either finished (and been journaled) or failed. Keep the
        // finished results in memory first, then surface the first abort.
        let mut first_abort = None;
        for outcome in finished {
            match outcome {
                Ok((index, result)) => self.note_tool_result(index, result),
                Err(error) => {
                    first_abort.get_or_insert(error);
                }
            }
        }
        if let Some(error) = first_abort {
            return Err(error);
        }
        Ok(self.complete_tools()?)
    }

    /// Runs one tool call and journals its result.
    async fn execute_tool<T: ToolPort>(
        &self,
        tools: &T,
        slot: &ToolSlot,
    ) -> Result<(usize, UserContent), DriveError> {
        let call = &slot.call.tool_call;
        let content = match tools.call(call).await {
            Ok(content) => content,
            Err(ToolError::Failed(message)) => vec![ToolResultContent::text(message)],
            Err(ToolError::Abort(source)) => {
                return Err(DriveError::ToolAborted {
                    tool: call.function.name.clone(),
                    source,
                });
            }
        };
        let result = UserContent::tool_result_for(
            call.id.clone(),
            call.provider.clone(),
            call.function.name.clone(),
            content,
        );
        self.journal_tool_result(slot.index, &result)?;
        Ok((slot.index, result))
    }
}
