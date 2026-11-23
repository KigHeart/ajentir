# aj-state

The AJENTIR typed state and checkpoint substrate.

This crate decides what an agent's state *is*, how concurrent updates to it
combine, and how a run is saved so it can be resumed. It has no model, tool or
network dependencies and does not run agents itself; runtimes are built on top.

## What it provides

| Piece | Module | Purpose |
| --- | --- | --- |
| Schemas | `schema`, `value_type`, `reducer` | Named channels, each with a JSON type and a merge rule. Schemas are versioned and fingerprinted. |
| State | `state`, `update`, `channel` | Channel values and per-channel version counters; a pure `apply` that folds an update batch through the reducers. Typed `Channel<T>` handles avoid hand-written JSON. |
| Frontier | `frontier` | The tasks that remain to run, with ids derived from their content. |
| Checkpoints | `checkpoint` | State + frontier + schema reference + provenance under a content-derived id. |
| Storage | `checkpointer` | `Checkpointer` trait; `InMemoryCheckpointer` and `SqliteCheckpointer`. |
| Forking | `fork` | Resume from an earlier checkpoint in a new thread, optionally with an edit. |
| Migration | `migration` | Carry a checkpoint to a newer schema version. |

## Guarantees

- **Deterministic updates.** An `UpdateBatch` orders writes by `(writer, sequence)`,
  never by completion time. The same writes produce the same state and digest in
  any arrival order.
- **No silent races.** Two updates reaching an `Overwrite` channel in one batch are
  a `Conflict` error, not a last-writer-wins accident.
- **All-or-nothing application.** `State::apply` validates every write before
  producing a new state; on error the original state is untouched.
- **Tamper evidence.** A checkpoint id is a SHA-256 digest of its full content.
  Both checkpointers verify on write and on read, so an edited or swapped row is
  reported (`IntegrityMismatch`, `Corrupt`) instead of resumed.
- **Linear threads.** A checkpoint is accepted only as the child of its thread's
  head. Concurrent writers cannot fork a thread; the loser gets `HeadMismatch`.
  Going back is a fork into a new thread, never a rewind.
- **Explicit schema evolution.** A checkpoint resumes only under the exact schema
  it was written under. Moving to a new version needs a `Migration` that accounts
  for every channel.
- **Canonical hashing.** Digests use their own canonical JSON encoding, so they do
  not depend on `serde_json`'s `preserve_order` feature or on key insertion order.

## Features

| Feature | Default | Effect |
| --- | --- | --- |
| `sqlite` | yes | Enables `SqliteCheckpointer` (bundled SQLite, WAL mode, `synchronous = FULL`). |

## Not covered

Checkpointing makes state recoverable; it does not make external side effects
exactly-once. There are no distributed leases or fencing tokens, no approval
workflow and no execution ledger in this crate. `InMemoryCheckpointer` is not
durable. The `Checkpointer` interface is synchronous; call it from a blocking task
on an async runtime.

## Verifying

```bash
cargo test -p aj-state
cargo test -p aj-state --no-default-features
cargo clippy -p aj-state --all-targets --all-features -- -D warnings
cargo fmt -- --check
```

See the crate-level documentation (`cargo doc -p aj-state --open`) for a complete
worked example.
