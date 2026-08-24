// A panicking library is a bug: this crate returns `Result` everywhere and the
// only command that writes does so under `--write`. Declared here rather than in
// `Cargo.toml`, because a manifest `[lints]` table also covers `tests/`, where a
// panic *is* the failure report. `clippy.toml` exempts the unit tests in `src/`.
#![warn(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

pub mod clean_config;
pub mod config;
pub mod data;
pub mod discover;
pub mod init;
pub mod keys;
pub mod lineindex;
pub mod migrate;
pub mod pattern;
pub mod plural;
pub mod report;
pub mod scan;
pub mod stats;
pub mod used;
pub mod walk;
pub mod yaml;
