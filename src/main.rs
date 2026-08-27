//! The `i18n-tasks-rs` CLI.
//!
//! Everything is under `cli`: the flags, one module per command, the exit codes
//! and the four printing macros. This file is the entry point and the place the
//! third exit code comes from.

// A panicking library is a bug: this crate returns `Result` everywhere and the
// only command that writes does so under `--write`. Declared here rather than in
// `Cargo.toml`, because a manifest `[lints]` table also covers `tests/`, where a
// panic *is* the failure report. `clippy.toml` exempts the unit tests in `src/`.
#![warn(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod cli;

use crate::cli::exit::EXIT_FAILURE;
use crate::cli::out::errln;
use std::process::ExitCode;

fn main() -> ExitCode {
    match cli::run() {
        Ok(status) => status.into(),
        Err(message) => {
            errln!("i18n-tasks-rs: {message}");
            ExitCode::from(EXIT_FAILURE)
        }
    }
}
