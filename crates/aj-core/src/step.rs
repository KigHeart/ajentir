//! The logical clock of a run.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Position of a super-step within one run, counted from zero.
///
/// A super-step is one round of work whose results are merged together. Ordering
/// follows the count, so a later step compares greater.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct SuperStep(u64);

impl SuperStep {
    /// The first super-step of a run.
    pub const FIRST: Self = Self(0);

    /// Wraps a step number.
    #[must_use]
    pub const fn new(number: u64) -> Self {
        Self(number)
    }

    /// The step number.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// The step after this one, or `None` if the counter would overflow.
    #[must_use]
    pub const fn next(self) -> Option<Self> {
        match self.0.checked_add(1) {
            Some(number) => Some(Self(number)),
            None => None,
        }
    }
}

impl fmt::Display for SuperStep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_starts_at_step_zero_and_counts_up() {
        assert_eq!(SuperStep::default(), SuperStep::FIRST);
        assert_eq!(SuperStep::FIRST.get(), 0);
        assert_eq!(SuperStep::FIRST.next(), Some(SuperStep::new(1)));
        assert!(SuperStep::new(2) > SuperStep::new(1));
    }

    #[test]
    fn the_counter_refuses_to_wrap() {
        assert_eq!(SuperStep::new(u64::MAX).next(), None);
        assert_eq!(
            SuperStep::new(u64::MAX - 1).next(),
            Some(SuperStep::new(u64::MAX))
        );
    }

    #[test]
    fn it_serializes_as_a_bare_number() {
        assert_eq!(serde_json::to_string(&SuperStep::new(7)).unwrap(), "7");
        assert_eq!(
            serde_json::from_str::<SuperStep>("7").unwrap(),
            SuperStep::new(7)
        );
        assert_eq!(SuperStep::new(7).to_string(), "7");
    }
}
