//! The exit codes, as a type.
//!
//! ref: lib/i18n/tasks/cli.rb. 0 means the check passed, 1 means the check
//! found something, 2 means the tool itself failed. The gem signals the middle
//! case with an internal `:exit1`.

use i18n_tasks_rs::report::Outcome;
use std::process::ExitCode;

/// The tool itself failed. `main` produces this one and no command can, which
/// is why it is a bare code and not an `ExitStatus` variant.
pub(crate) const EXIT_FAILURE: u8 = 2;

/// What a command returns: it either passed or it found something.
///
/// An enum rather than the two `u8` constants it used to be, because a check's
/// verdict is the tool's whole output: `Ok(EXIT_OK)` where `Ok(EXIT_FOUND)` was
/// meant compiles and silently inverts it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExitStatus {
    Ok,
    Found,
}

impl ExitStatus {
    /// For the commands that decide on something other than a report's
    /// `Outcome`: a generated config that needs a human, an ignore rule only a
    /// human can remove.
    pub(crate) fn found(found: bool) -> ExitStatus {
        if found {
            ExitStatus::Found
        } else {
            ExitStatus::Ok
        }
    }
}

impl From<Outcome> for ExitStatus {
    fn from(outcome: Outcome) -> ExitStatus {
        ExitStatus::found(outcome == Outcome::Found)
    }
}

impl From<ExitStatus> for ExitCode {
    fn from(status: ExitStatus) -> ExitCode {
        match status {
            ExitStatus::Ok => ExitCode::from(0),
            ExitStatus::Found => ExitCode::from(1),
        }
    }
}
