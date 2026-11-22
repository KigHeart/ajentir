//! The execution frontier: the work a checkpoint says remains to be done.
//!
//! State alone is not enough to resume an interrupted run — the run also needs to
//! know which tasks were about to execute and with what input. A [`Frontier`]
//! records exactly that and is stored in every checkpoint. Task ids are derived
//! from the step, the task's position in canonical order, its node and its input,
//! so planning the same work always yields the same ids regardless of the order
//! the caller lists it in.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    canonical,
    digest::{Digest, DigestBuilder},
    ids::{NodeName, TaskId},
};

/// One unit of pending work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrontierTask {
    id: TaskId,
    node: NodeName,
    input: Value,
}

impl FrontierTask {
    /// The task's identity within its frontier.
    #[must_use]
    pub fn id(&self) -> &TaskId {
        &self.id
    }

    /// The node that will run.
    #[must_use]
    pub fn node(&self) -> &NodeName {
        &self.node
    }

    /// The input the node will receive.
    #[must_use]
    pub fn input(&self) -> &Value {
        &self.input
    }
}

/// A frontier could not be assembled from existing tasks.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FrontierError {
    /// Two tasks share an id.
    #[error("task `{0}` appears twice in the frontier")]
    DuplicateTask(TaskId),
}

/// The set of tasks pending at a checkpoint, ordered by task id.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(try_from = "Vec<FrontierTask>", into = "Vec<FrontierTask>")]
pub struct Frontier {
    tasks: Vec<FrontierTask>,
}

impl Frontier {
    /// A frontier with no pending work; the run it belongs to has finished.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Plans the tasks for the checkpoint at `step`, deriving their ids.
    ///
    /// Tasks are put into canonical order — by node, then by the canonical
    /// encoding of their input — before ids are assigned, so the listing order is
    /// irrelevant. Identical `(node, input)` pairs are allowed and receive
    /// distinct ids.
    #[must_use]
    pub fn plan(step: u64, tasks: Vec<(NodeName, Value)>) -> Self {
        let mut keyed: Vec<(NodeName, Vec<u8>, Value)> = tasks
            .into_iter()
            .map(|(node, input)| {
                let encoded = canonical::to_vec(&input);
                (node, encoded, input)
            })
            .collect();
        keyed.sort_by(|left, right| (&left.0, &left.1).cmp(&(&right.0, &right.1)));

        let mut planned: Vec<FrontierTask> = keyed
            .into_iter()
            .enumerate()
            .map(|(index, (node, encoded, input))| {
                let mut builder = DigestBuilder::new("ajentir.task.v1");
                builder
                    .u64(step)
                    .u64(u64::try_from(index).unwrap_or(u64::MAX))
                    .str(node.as_str())
                    .bytes(&encoded);
                FrontierTask {
                    id: TaskId::from_trusted(builder.finish().to_hex()),
                    node,
                    input,
                }
            })
            .collect();
        planned.sort_by(|left, right| left.id.cmp(&right.id));
        Self { tasks: planned }
    }

    /// Builds a frontier from tasks that already carry ids, for example ones read
    /// back from storage.
    ///
    /// # Errors
    ///
    /// Returns [`FrontierError::DuplicateTask`] if two tasks share an id.
    pub fn from_tasks(mut tasks: Vec<FrontierTask>) -> Result<Self, FrontierError> {
        tasks.sort_by(|left, right| left.id.cmp(&right.id));
        for pair in tasks.windows(2) {
            if let [first, second] = pair
                && first.id == second.id
            {
                return Err(FrontierError::DuplicateTask(first.id.clone()));
            }
        }
        Ok(Self { tasks })
    }

    /// Pending tasks, ordered by id.
    #[must_use]
    pub fn tasks(&self) -> &[FrontierTask] {
        &self.tasks
    }

    /// Number of pending tasks.
    #[must_use]
    pub fn len(&self) -> usize {
        self.tasks.len()
    }

    /// Whether nothing is pending.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    /// Looks up a task by id.
    #[must_use]
    pub fn get(&self, id: &TaskId) -> Option<&FrontierTask> {
        self.tasks.iter().find(|task| &task.id == id)
    }

    /// Whether the frontier contains a task with this id.
    #[must_use]
    pub fn contains(&self, id: &TaskId) -> bool {
        self.get(id).is_some()
    }

    /// A digest over every task in order.
    #[must_use]
    pub fn digest(&self) -> Digest {
        let mut builder = DigestBuilder::new("ajentir.frontier.v1");
        builder.u64(u64::try_from(self.tasks.len()).unwrap_or(u64::MAX));
        for task in &self.tasks {
            builder
                .str(task.id.as_str())
                .str(task.node.as_str())
                .value(&task.input);
        }
        builder.finish()
    }
}

impl TryFrom<Vec<FrontierTask>> for Frontier {
    type Error = FrontierError;

    fn try_from(tasks: Vec<FrontierTask>) -> Result<Self, FrontierError> {
        Self::from_tasks(tasks)
    }
}

impl From<Frontier> for Vec<FrontierTask> {
    fn from(frontier: Frontier) -> Self {
        frontier.tasks
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn node(name: &str) -> NodeName {
        NodeName::new(name).unwrap()
    }

    #[test]
    fn planning_is_independent_of_listing_order() {
        let one = Frontier::plan(
            3,
            vec![
                (node("search"), json!({"q": "rust"})),
                (node("summarize"), json!({})),
                (node("search"), json!({"q": "wasm"})),
            ],
        );
        let two = Frontier::plan(
            3,
            vec![
                (node("search"), json!({"q": "wasm"})),
                (node("search"), json!({"q": "rust"})),
                (node("summarize"), json!({})),
            ],
        );
        assert_eq!(one, two);
        assert_eq!(one.digest(), two.digest());
        assert_eq!(one.len(), 3);
    }

    #[test]
    fn ids_depend_on_the_step_and_are_unique() {
        let tasks = || vec![(node("a"), json!(1)), (node("a"), json!(1))];
        let at_three = Frontier::plan(3, tasks());
        let at_four = Frontier::plan(4, tasks());
        assert_ne!(at_three, at_four);
        let ids: Vec<&TaskId> = at_three.tasks().iter().map(FrontierTask::id).collect();
        assert_eq!(ids.len(), 2);
        assert_ne!(ids.first(), ids.get(1));
    }

    #[test]
    fn lookup_by_id() {
        let frontier = Frontier::plan(0, vec![(node("only"), json!("x"))]);
        let task = frontier.tasks().first().unwrap();
        assert_eq!(frontier.get(task.id()).unwrap().node().as_str(), "only");
        assert!(frontier.contains(task.id()));
        assert!(!frontier.contains(&TaskId::new("other").unwrap()));
        assert_eq!(task.input(), &json!("x"));
    }

    #[test]
    fn empty_frontier() {
        let frontier = Frontier::empty();
        assert!(frontier.is_empty());
        assert_eq!(frontier.len(), 0);
    }

    #[test]
    fn duplicate_ids_are_rejected_on_load() {
        let planned = Frontier::plan(0, vec![(node("a"), json!(1))]);
        let task = planned.tasks().first().unwrap().clone();
        assert!(matches!(
            Frontier::from_tasks(vec![task.clone(), task]),
            Err(FrontierError::DuplicateTask(_))
        ));
    }

    #[test]
    fn serde_round_trip() {
        let frontier = Frontier::plan(1, vec![(node("a"), json!({"k": [1, 2]}))]);
        let json = serde_json::to_string(&frontier).unwrap();
        assert_eq!(serde_json::from_str::<Frontier>(&json).unwrap(), frontier);
    }
}
