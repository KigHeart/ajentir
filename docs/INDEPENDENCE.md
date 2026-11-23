# Independence and implementation status

## Decision

AJENTIR is an independent Rust runtime. Rig is not its base framework, runtime dependency, public API, compatibility layer, or required source of agent/model/tool abstractions.

The active default-branch Cargo workspace has been replaced with an AJENTIR-owned workspace. Earlier branches and commits are retained as historical source material and are not active workspace members.

## Active workspace boundary

The root `Cargo.toml` explicitly lists only these packages:

- `aj-core`
- `aj-state`
- `aj-kernel`
- `aj-sim`

The root manifest contains no inherited Rig workspace members, provider matrix, or Rig package aliases. The four active package manifests use AJENTIR-owned local dependencies and selected general-purpose Rust dependencies.

The intended dependency direction is:

```text
aj-core

aj-state

aj-kernel -> aj-state
aj-sim    -> aj-kernel + aj-state
```

This is a small foundation, not yet the complete v0.2 tiered topology. The current source audit also identifies a follow-up design issue: `aj-core` defines primitive identifiers while `aj-state` defines its own related identifier types. Those boundaries need to be reconciled deliberately before more crates depend on them.

## Preserved implementation

The active state/checkpoint substrate includes:

- versioned schemas and typed channel handles;
- deterministic update batches and reducers;
- content-addressed checkpoints and frontier state;
- in-memory and SQLite checkpointers;
- schema migration and fork-with-edit behavior;
- integrity and storage conformance tests.

The active BSP kernel validates exact completion coverage of a frozen frontier, rejects duplicate results, builds deterministic task-write batches, and produces a new immutable checkpoint. It does not schedule or execute asynchronous tasks.

The active simulator enumerates bounded task-completion permutations and checks that they produce the same checkpoint. It is not a distributed-systems simulator, fault injector, or formal model checker.

## Not implemented yet

The following v0.2 capabilities remain separate implementation work:

- task execution records and durable recovery of completed work;
- effect intents/results and idempotency/reconciliation;
- compare-and-swap commits, worker leases and fencing;
- one storage contract for checkpoints, tasks and effects;
- model/provider manifests and request journaling;
- tool registry, effect classification and dispatch;
- context admission, compaction and model routing;
- durable approvals, budgets and execution control;
- fault-injection simulation and comparative evaluation.

Checkpoint persistence alone does not establish exactly-once external effects, distributed commit safety, or crash recovery of task execution.

## GitHub repository lineage

The code and active Cargo workspace are independent of Rig. The GitHub repository may still carry fork-network metadata from its earlier origin. That metadata is not changed by editing manifests or source files; detaching a repository from a fork network requires GitHub-side action/support. No historical branches or commits have been deleted to create the independent workspace.

## Verification gates

The active workspace must pass:

```powershell
cargo fmt --all -- --check
cargo test --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

A successful CI run is the evidence for a pass. A configured workflow or a command listed in documentation is not itself evidence that the check ran.

## Independence exit criteria

- [x] Active Cargo workspace contains only AJENTIR-owned packages.
- [x] No Rig dependency or Rig re-export in the active package manifests and public crate roots inspected for this status update.
- [x] AJENTIR state/checkpoint work retained.
- [x] BSP kernel and deterministic simulator are active workspace members.
- [ ] Shared primitive identity boundary reconciled between `aj-core` and `aj-state`.
- [ ] Full active source tree audited for stale Rig imports, names and documentation.
- [ ] GitHub fork-network metadata detached, if required for complete repository-level independence.
- [ ] Task/effect journal, fenced commit protocol and fault-injection verification implemented.

This status records the current code boundary. It does not claim the full runtime architecture is complete.
