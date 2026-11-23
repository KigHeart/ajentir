# Independence and implementation status

## Decision

AJENTIR is an independent Rust runtime. Rig is not its base framework, runtime dependency, public API, or required source of agent/model/tool abstractions.

The repository's prior default branch was an upstream Rig snapshot. This branch replaces that inherited workspace tree with an AJENTIR-owned workspace while retaining the original branches as historical source material.

## Workspace boundary

The active Cargo workspace contains only AJENTIR-owned packages:

- `aj-core`
- `aj-state`
- `aj-kernel`
- `aj-sim`
- `aj-trading` (financial-domain types and pure pre-trade risk gate; no broker execution)

The selected crates contain no Rig dependency or Rig re-export. The root manifest contains no inherited provider matrix, upstream workspace members, or Rig aliases.

## Preserved implementation

The state/checkpoint substrate is retained with its schemas, typed channel handles, reducers, deterministic update batches, content-addressed checkpoints, in-memory and SQLite checkpointers, conformance tests, durability tests, migrations, and fork-with-edit behavior.

The AJENTIR v0.2 architecture document and supplied PDF are retained as the design baseline.

## Deliberately not claimed

The P0 BSP kernel and bounded completion-order simulator are active workspace members and integrated against the retained typed state/checkpoint API. The `aj-trading` crate is the first domain application slice; it contains deterministic order/portfolio contracts and a pure risk gate, not a live trading system.

The model interface, tool interface, durable scheduler, effect journal, approval/control plane, cost ledger, context admission, router, and evaluation harness remain implementation work. The simulator currently checks bounded completion-order permutations; it is not yet a full fault-injection or distributed-systems simulator.

## Exit checks for this slice

- The workspace is AJENTIR-owned and explicit.
- No active workspace package depends on Rig.
- No active public Rust API re-exports Rig types.
- The state/checkpoint tests remain part of the workspace.
- CI checks formatting, tests, and Clippy.

Passing these checks establishes an independent codebase foundation, not completion of the runtime architecture.
