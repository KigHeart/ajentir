//! The one naming rule every AJENTIR identifier follows.
//!
//! A name is non-empty, at most [`MAX_NAME_BYTES`] bytes of UTF-8, and free of
//! control characters. The rule is checked by constructors *and* when a value is
//! deserialized, so persisted data cannot carry a name the constructors would have
//! refused. Crates that define their own identifiers (a channel name, a task id)
//! call [`validate_name`] instead of restating the rule.

/// Longest permitted name, measured in bytes of UTF-8 (not characters).
pub const MAX_NAME_BYTES: usize = 256;

/// The rule a rejected name broke.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum NameProblem {
    /// The name was empty.
    #[error("must not be empty")]
    Empty,
    /// The name was longer than [`MAX_NAME_BYTES`] bytes.
    #[error("must be at most 256 bytes")]
    TooLong,
    /// The name contained a control character.
    #[error("must not contain control characters")]
    ControlCharacter,
}

/// A name or identifier that failed validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid {what}: {problem}")]
pub struct NameError {
    what: &'static str,
    problem: NameProblem,
}

impl NameError {
    /// What kind of name was being validated, for example `"thread id"`.
    #[must_use]
    pub fn what(&self) -> &'static str {
        self.what
    }

    /// The rule the name broke.
    #[must_use]
    pub fn problem(&self) -> NameProblem {
        self.problem
    }
}

/// Checks `value` against the naming rule. `what` names the kind of identifier
/// and appears in the error message.
///
/// # Errors
///
/// Returns a [`NameError`] carrying the first rule broken: empty, then too long,
/// then control character.
pub fn validate_name(what: &'static str, value: &str) -> Result<(), NameError> {
    let problem = if value.is_empty() {
        NameProblem::Empty
    } else if value.len() > MAX_NAME_BYTES {
        NameProblem::TooLong
    } else if value.chars().any(char::is_control) {
        NameProblem::ControlCharacter
    } else {
        return Ok(());
    };
    Err(NameError { what, problem })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ordinary_name_is_accepted() {
        assert_eq!(validate_name("thread id", "ticket-42"), Ok(()));
    }

    #[test]
    fn an_empty_name_is_refused() {
        let error = validate_name("thread id", "").unwrap_err();
        assert_eq!(error.problem(), NameProblem::Empty);
        assert_eq!(error.what(), "thread id");
    }

    #[test]
    fn the_length_limit_is_in_bytes_and_inclusive() {
        assert_eq!(validate_name("x", &"a".repeat(MAX_NAME_BYTES)), Ok(()));
        assert_eq!(
            validate_name("x", &"a".repeat(MAX_NAME_BYTES + 1))
                .unwrap_err()
                .problem(),
            NameProblem::TooLong
        );
        // 129 two-byte characters are 129 chars but 258 bytes.
        assert_eq!(
            validate_name("x", &"é".repeat(129)).unwrap_err().problem(),
            NameProblem::TooLong
        );
        assert_eq!(validate_name("x", &"é".repeat(128)), Ok(()));
    }

    #[test]
    fn control_characters_are_refused_wherever_they_appear() {
        for value in [
            "\nfoo",
            "foo\n",
            "fo\u{0}o",
            "fo\u{7f}o",
            "a\tb",
            "a\u{85}b",
        ] {
            assert_eq!(
                validate_name("x", value).unwrap_err().problem(),
                NameProblem::ControlCharacter,
                "{value:?}"
            );
        }
    }

    #[test]
    fn spaces_and_non_ascii_text_are_allowed() {
        assert_eq!(validate_name("x", "plan step 3 — résumé"), Ok(()));
    }

    #[test]
    fn the_first_broken_rule_is_reported() {
        // Too long *and* contains a control character: length is checked first.
        let value = format!("{}\n", "a".repeat(MAX_NAME_BYTES));
        assert_eq!(
            validate_name("x", &value).unwrap_err().problem(),
            NameProblem::TooLong
        );
    }

    #[test]
    fn the_message_names_the_kind_and_the_rule() {
        assert_eq!(
            validate_name("run id", "").unwrap_err().to_string(),
            "invalid run id: must not be empty"
        );
    }
}
