#![forbid(unsafe_code)]
//! Bounded schedule simulator for deterministic AJENTIR super-step commits.
//!
//! This first simulator enumerates task completion orders and verifies that all
//! permutations produce the same content-addressed checkpoint. It is not yet a
//! distributed scheduler, fault injector or full model checker.

use aj_kernel::{KernelError, commit_superstep};
use aj_state::{Checkpoint, Frontier, Schema, TaskWrites};
use serde_json::Value;
use std::collections::BTreeMap;
use thiserror::Error;

/// Why a simulation could not complete.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SimulationError {
    /// The factorial number of completion orders exceeds the caller's limit.
    #[error("completion-order space exceeds configured limit {limit}")]
    PermutationLimit { limit: usize },
    /// A simulated execution failed kernel validation or commit.
    #[error(transparent)]
    Kernel(#[from] KernelError),
    /// Two completion orders produced different checkpoint identities.
    #[error("completion order changed the resulting checkpoint identity")]
    NondeterministicOutcome,
}

/// Evidence from one bounded completion-order determinism run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimulationReport {
    permutations_checked: usize,
    checkpoint: Checkpoint,
}

impl SimulationReport {
    /// Number of completion orders checked.
    #[must_use]
    pub fn permutations_checked(&self) -> usize { self.permutations_checked }

    /// The common checkpoint produced by every checked completion order.
    #[must_use]
    pub fn checkpoint(&self) -> &Checkpoint { &self.checkpoint }
}

/// Enumerates every completion order up to max_permutations and checks that
/// committing the same task outputs always yields the same checkpoint identity.
///
/// # Errors
///
/// Returns a limit error before simulation if n! exceeds max_permutations.
pub fn verify_completion_order_independence(
    checkpoint: &Checkpoint,
    schema: &Schema,
    completed: &[TaskWrites],
    next_frontier: &Frontier,
    metadata: &BTreeMap<String, Value>,
    max_permutations: usize,
) -> Result<SimulationReport, SimulationError> {
    let count = factorial_bounded(completed.len(), max_permutations)
        .ok_or(SimulationError::PermutationLimit { limit: max_permutations })?;
    let mut permutations = Vec::with_capacity(count);
    enumerate(completed, &mut vec![false; completed.len()], &mut Vec::new(), &mut permutations);
    let mut canonical_id = None;
    let mut canonical_checkpoint = None;
    for order in &permutations {
        let committed = commit_superstep(checkpoint, schema, order, next_frontier.clone(), metadata.clone())?;
        let id = committed.checkpoint().id();
        if canonical_id.is_some_and(|expected| expected != id) {
            return Err(SimulationError::NondeterministicOutcome);
        }
        canonical_id = Some(id);
        canonical_checkpoint = Some(committed.checkpoint().clone());
    }
    let checkpoint = canonical_checkpoint.unwrap_or_else(|| checkpoint.clone());
    Ok(SimulationReport { permutations_checked: count, checkpoint })
}

fn factorial_bounded(n: usize, limit: usize) -> Option<usize> {
    if limit == 0 { return None; }
    let mut result = 1usize;
    for factor in 2..=n {
        result = result.checked_mul(factor)?;
        if result > limit { return None; }
    }
    (result <= limit).then_some(result)
}

fn enumerate<T: Clone>(items: &[T], used: &mut [bool], current: &mut Vec<T>, out: &mut Vec<Vec<T>>) {
    if current.len() == items.len() {
        out.push(current.clone());
        return;
    }
    for index in 0..items.len() {
        if !used[index] {
            used[index] = true;
            if let Some(item) = items.get(index) { current.push(item.clone()); }
            enumerate(items, used, current, out);
            drop(current.pop());
            used[index] = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aj_state::{NodeName, ReducerKind, ThreadId, Update, UpdateBatch, ValueType};
    use serde_json::json;

    #[test]
    fn all_completion_orders_converge_to_one_checkpoint() {
        let schema = Schema::builder("sim-test", 1)
            .channel("total", ValueType::Integer, ReducerKind::SumI64)
            .build().unwrap();
        let frontier = Frontier::plan(0, vec![
            (NodeName::new("worker").unwrap(), json!(1)),
            (NodeName::new("worker").unwrap(), json!(2)),
            (NodeName::new("worker").unwrap(), json!(3)),
        ]);
        let checkpoint = Checkpoint::initial(ThreadId::new("sim-thread").unwrap(),
            &schema, &UpdateBatch::empty(), frontier, BTreeMap::new()).unwrap();
        let completed: Vec<TaskWrites> = checkpoint.frontier().tasks().iter().map(|task| {
            TaskWrites::new(task.id().clone(), vec![
                Update::named("total", task.input().clone()).unwrap()
            ])
        }).collect();
        let report = verify_completion_order_independence(&checkpoint, &schema, &completed,
            &Frontier::empty(), &BTreeMap::new(), 6).unwrap();
        assert_eq!(report.permutations_checked(), 6);
        assert_eq!(report.checkpoint().state().value("total"), Some(&json!(6)));
    }

    #[test]
    fn factorial_bound_is_checked_before_enumeration() {
        assert_eq!(factorial_bounded(4, 24), Some(24));
        assert!(factorial_bounded(5, 100).is_none());
        assert!(factorial_bounded(0, 0).is_none());
    }
}
