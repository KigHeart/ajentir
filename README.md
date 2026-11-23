# AJENTIR

**An independent Rust-native agent runtime.**

AJENTIR is being built as a durable, cost-aware, model-agnostic runtime for coordinating agents and their work. Its central execution abstraction is a bulk-synchronous-parallel (BSP) super-step: plan, admit, execute, barrier, merge, route, and commit.

This repository is now being reconstituted as an AJENTIR-owned Rust workspace. It does not use Rig as a runtime dependency, public API, workspace member, compatibility layer, or implementation requirement.

## Current workspace

| Crate | Responsibility | Status |
|---|---|---|
| `aj-core` | Validated runtime identifiers and primitive types | Initial foundation |
| `aj-state` | Typed channels, versioned schemas, reducers, deterministic updates, checkpoints, SQLite persistence, migrations, and fork-with-edit | Implemented substrate; tests are included |

The P0 BSP kernel and simulator are being reintegrated against the richer `aj-state` API as the next implementation step. They are not represented as part of this workspace until their contracts are reconciled and the workspace builds cleanly.

## Independence rule

A crate may depend only on AJENTIR-owned crates and explicitly selected general-purpose Rust dependencies. No Rig package, import, type alias, re-export, fork-specific adapter, or inherited workspace configuration is permitted.

## Build and test

Requires a Rust toolchain supporting Edition 2024.

```powershell
cargo fmt --all -- --check
cargo test --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

These commands are verification gates; consult CI for their actual status.

## Architecture

- [AJENTIR v0.2 architecture baseline](docs/architecture/AJENTIR-V0.2-BASELINE.md)
- [Independence and implementation status](docs/INDEPENDENCE.md)

## License

MIT. See [LICENSE](LICENSE).
