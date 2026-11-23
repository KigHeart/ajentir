# AJENTIR

**An independent Rust-native agent runtime and multi-agent systems foundation.**

AJENTIR coordinates durable, cost-aware, model-agnostic agent work through a bulk-synchronous-parallel (BSP) super-step: plan, admit, execute, barrier, merge, route and commit.

AJENTIR is independent. Rig is not a runtime dependency, public API, workspace member, compatibility layer or implementation requirement.

## Active workspace

| Crate | Responsibility |
|---|---|
| aj-core | Validated runtime identities and primitive contracts |
| aj-state | Typed channels, deterministic reducers, content-addressed checkpoints, SQLite persistence and fork-with-edit |
| aj-kernel | Deterministic BSP frontier validation and atomic state/checkpoint commit |
| aj-sim | Bounded completion-order determinism simulator |
| aj-trading | Financial-domain contracts and pure pre-trade risk evaluation; no broker execution |

## AJENTIR Trading

AJENTIR Trading is the first domain application built on the runtime. Its direction is multi-agent market research, hypothesis generation, strategy evaluation, portfolio analysis and independently controlled execution.

The initial aj-trading crate contains validated whole-unit limit-order and portfolio types plus a deterministic pre-trade risk gate. It does not connect to brokers, submit orders, predict prices or claim profitability.

See [Trading architecture](docs/trading/ARCHITECTURE.md).

## Independence rule

A crate may depend only on AJENTIR-owned crates and explicitly selected general-purpose Rust dependencies. No Rig package, import, type alias, re-export or inherited workspace configuration is permitted.

## Build and test

Requires a Rust toolchain supporting Edition 2024.

```powershell
cargo fmt --all -- --check
cargo test --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

These are verification gates; consult CI for their actual status.

## Architecture

- [AJENTIR v0.2 architecture baseline](docs/architecture/AJENTIR-V0.2-BASELINE.md)
- [Independence and implementation status](docs/INDEPENDENCE.md)
- [Trading architecture](docs/trading/ARCHITECTURE.md)

## License

MIT. See [LICENSE](LICENSE).
