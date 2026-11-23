# AJENTIR

**An independent Rust-native agent runtime.**

AJENTIR is being built as a durable, cost-aware, model-agnostic runtime for coordinating agents and their work. Its central execution abstraction is a bulk-synchronous-parallel (BSP) super-step: plan, admit, execute, barrier, merge, route, and commit.

The active Cargo workspace is AJENTIR-owned. It does not use Rig as a runtime dependency, public API, compatibility layer, or active workspace member. Historical branches preserve earlier development material; they are not part of the active build.

## Current workspace

| Crate | Responsibility | Current boundary |
|---|---|---|
| `aj-core` | Validated runtime identifiers and primitive types | Initial foundation |
| `aj-state` | Typed channels, versioned schemas, reducers, deterministic updates, content-addressed checkpoints, SQLite persistence, task-write recording, migrations, and fork-with-edit | Implemented state and partial task-journal substrate |
| `aj-kernel` | Validates complete task results for a frozen frontier and creates deterministic next checkpoints | Synchronous BSP commit boundary; no scheduler |
| `aj-sim` | Enumerates bounded task-completion permutations and checks checkpoint determinism | Determinism harness; no fault injection or distributed simulation |

### Current dependency direction

```text
aj-core

aj-state

aj-kernel -> aj-state
aj-sim    -> aj-kernel + aj-state
```

The dependency graph is intentionally small. The next architecture work is to reconcile shared primitive identities, then complete the task/effect journal and storage contracts without turning the kernel into a provider-specific framework.

## Independence rule

A crate may depend only on AJENTIR-owned crates and explicitly selected general-purpose Rust dependencies. No Rig package, import, type alias, re-export, fork-specific adapter, or inherited workspace configuration is permitted in the active implementation.

The repository retains historical branches and GitHub lineage from its earlier development. Code/workspace independence and GitHub's fork-network metadata are separate matters; see [the independence status](docs/INDEPENDENCE.md).

## Build and test

Requires a Rust toolchain supporting Edition 2024.

```powershell
cargo fmt --all -- --check
cargo test --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

These are the required verification gates. Do not infer that a check passed unless its CI run or local output confirms it.

## Architecture

- [AJENTIR v0.2 architecture baseline](docs/architecture/AJENTIR-V0.2-BASELINE.md)
- [Independence and implementation status](docs/INDEPENDENCE.md)

## License

MIT. See [LICENSE](LICENSE).
