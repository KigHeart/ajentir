# aj-core

Tier 0 of the AJENTIR workspace: the vocabulary every other crate shares.

`aj-core` has no knowledge of models, tools, storage or scheduling. It defines:

| Item | Purpose |
| --- | --- |
| `validate_name`, `NameError`, `NameProblem`, `MAX_NAME_BYTES` | The single naming rule: non-empty, at most 256 bytes, no control characters. |
| `RunId`, `ThreadId`, `NodeName`, `ToolCallId`, `MessageId`, `ModelId`, `ProviderId` | Distinct identifier types, so one kind can never be passed for another. |
| `SuperStep` | The logical step counter of a run, with checked increment. |

Every identifier is validated when built **and** when deserialized, so stored data
cannot contain a name the constructors would refuse. Identifiers serialize as plain
JSON strings, and `SuperStep` as a bare number.

Crates that need their own kinds of name (a channel name, a task id) call
`validate_name` rather than restating the rule.

## Not in this crate

Messages, the model interface and the tool interface belong to later crates (`aj-model`,
`aj-tools`) per `docs/architecture/AJENTIR-V0.2-BASELINE.md`. A checkpoint id is a
content digest and lives with checkpoints in `aj-state`.

## Verifying

```bash
cargo test -p aj-core
cargo clippy -p aj-core --all-targets -- -D warnings
```
