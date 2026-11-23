# ajentir-run

Durable driver for the AJENTIR agent loop.

The agent loop already exists as the sans-IO `rig_run::AgentRun` state machine. This
crate does not replace it. It makes a run **survive interruption**: every step
boundary of the machine is stored as an [`ajentir`](../ajentir) checkpoint, and
every expensive piece of work done between boundaries — a billed model call, a tool
with side effects — is journaled the moment it finishes.

```rust
let store = Arc::new(InMemoryCheckpointer::new());
let mut run = DurableRun::start(store.clone(), thread, AgentRun::new("What time is it?"))?;
let response = futures::executor::block_on(run.drive(&model, &tools, &FailFast))?;

// Any later session — in this process or another — continues from the store.
let run = DurableRun::resume(store, thread)?;
```

A complete, compiled example is in the crate documentation (`cargo doc -p ajentir-run --open`).

## What it provides

| Item | Purpose |
| --- | --- |
| `DurableRun` | A session over one run: `start`, `resume`, and the step-by-step transitions (`complete_model`, `resolve_invalid_tool_call`, `record_tool_result`, `complete_tools`). |
| `DurableRun::drive` | A convenience async loop that runs the whole thing through the ports below. |
| `ModelPort`, `ToolPort` | The only way the driver reaches a model or a tool. Implemented by adapters outside this crate. |
| `InvalidToolCallPolicy`, `FailFast` | How to recover when the model names a tool that does not exist. Closures work too. |
| `Pending` | What a run needs next, including work already journaled for it. |
| `schema` | The `ajentir` schema runs are stored under, and the frontier/journal vocabulary. |

## What recovery guarantees

| Interrupted while… | After `resume` |
| --- | --- |
| waiting for the model (nothing received) | the model is called again (at-least-once) |
| the model answered, before its checkpoint | the **journaled response is used; the model is not called again** |
| tools running, some finished | **finished tools are not run again**; unfinished ones run again (at-least-once) |
| an invalid tool call awaiting a decision | the response is replayed and the same decision is asked again |

Re-running a tool that was in flight is unavoidable, so a tool with external side
effects must be idempotent or deduplicate on the tool-call id. This crate makes the
*state* of a run recoverable; it cannot make the outside world exactly-once.

Tool results are matched to calls by id, so parallel tools may finish in any order.

## How it is tested

- Behaviour tests cover every transition, the failure modes, and the "not repeated"
  promises (a journaled model response, a finished tool) with call counters.
- A **crash sweep** kills a run at every one of its storage operations, both before a
  write and after a write whose acknowledgement was lost, and requires the recovered
  run to produce the same response and the same checkpoint phases as an
  uninterrupted one. It asserts that every operation was crashed exactly once.
- A SQLite test resumes a run in a second session that shares only the database file.

The sweep alone does not prove that finished tools are never repeated (it bounds
repeats and checks the result); the targeted tests do.

## Limits

- **No hooks or streaming.** Completion-response hooks, per-turn hooks that can retry
  or stop a turn, tool hooks and streamed turns belong to the driver in `rig-agent`
  and are not offered here.
- **Stored size and sensitivity.** A checkpoint embeds the whole conversation and
  every completed call's raw provider response. Stored runs carry whatever the
  conversation contains. The serialized machine has no cross-version stability
  guarantee: resume with the same `rig` version that wrote the run.
- **Synchronous storage.** `Checkpointer` calls block the calling thread, including
  inside `drive`.
- **One writer per thread.** A second session writing the same thread gets a store
  error instead of forking history, and can `resume` to catch up. There are no
  leases, so the work of both may still run.
- **No provider adapter yet.** Connecting a real `rig-core` provider and tool
  registry to `ModelPort` and `ToolPort` is a separate step.

## Verifying

```bash
cargo test -p ajentir-run
cargo clippy -p ajentir-run --all-targets -- -D warnings
cargo fmt -- --check
```
