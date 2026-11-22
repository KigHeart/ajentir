//! Fixtures shared by the integration tests: a small agent schema and helpers that
//! build checkpoints under it.

#![allow(dead_code)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::collections::BTreeMap;

use ajentir::{
    Checkpoint, Frontier, NodeName, ReducerKind, Schema, ThreadId, Update, UpdateBatch, ValueType,
    WriterId,
};
use serde_json::Value;

pub fn schema() -> Schema {
    Schema::builder("agent", 1)
        .channel("messages", ValueType::Array, ReducerKind::Append)
        .channel("turns", ValueType::Integer, ReducerKind::SumI64)
        .build()
        .unwrap()
}

pub fn thread(name: &str) -> ThreadId {
    ThreadId::new(name).unwrap()
}

pub fn batch(channel: &str, value: Value) -> UpdateBatch {
    UpdateBatch::from_writer_outputs([(
        WriterId::new("writer").unwrap(),
        vec![Update::named(channel, value).unwrap()],
    )])
    .unwrap()
}

/// The first checkpoint of `thread_name`, with two pending tasks.
pub fn start(thread_name: &str) -> Checkpoint {
    Checkpoint::initial(
        thread(thread_name),
        &schema(),
        &batch("messages", serde_json::json!(["hello"])),
        Frontier::plan(
            0,
            vec![
                (NodeName::new("plan").unwrap(), serde_json::json!({"i": 1})),
                (NodeName::new("act").unwrap(), serde_json::json!({"i": 2})),
            ],
        ),
        BTreeMap::new(),
    )
    .unwrap()
}

/// A child of `parent` that adds `turns` to the turn counter and has one pending task.
pub fn child(parent: &Checkpoint, turns: i64) -> Checkpoint {
    parent
        .advance(
            &schema(),
            &batch("turns", serde_json::json!(turns)),
            Frontier::plan(
                parent.next_step(),
                vec![(
                    NodeName::new("act").unwrap(),
                    serde_json::json!({"turns": turns}),
                )],
            ),
            BTreeMap::new(),
        )
        .unwrap()
}
