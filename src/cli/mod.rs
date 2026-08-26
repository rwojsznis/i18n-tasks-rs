//! The command line: the flags, the dispatch, and the printing macros.
//!
//! `Command` is the whole map of the CLI — one variant, one module, one `run`.
//! Everything a command owns, its flags included, lives in that module, so
//! adding a flag is one file and adding a command is one file and one arm.

pub(crate) mod args;
pub(crate) mod commands;
pub(crate) mod exit;
pub(crate) mod out;
pub(crate) mod pool;

use crate::cli::commands::checks::{CheckArgs, MissingArgs};
use crate::cli::commands::clean_config::CleanConfigArgs;
use crate::cli::commands::find::FindArgs;
use crate::cli::commands::health::HealthArgs;
use crate::cli::commands::init_config::InitConfigArgs;
use crate::cli::commands::migrate_config::MigrateConfigArgs;
use crate::cli::commands::normalize::NormalizeArgs;
use crate::cli::commands::remove_unused::RemoveUnusedArgs;
use crate::cli::exit::ExitStatus;
use clap::{Parser, Subcommand};

const VERSION: &str = match option_env!("I18N_TASKS_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};

#[derive(Parser)]
#[command(
    name = "i18n-tasks-rs",
    about = "Manage translations in Ruby applications. A Rust port of i18n-tasks.",
    version = VERSION
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// Parses the command line and runs the one command it names.
pub(crate) fn run() -> Result<ExitStatus, String> {
    Cli::parse().command.run()
}

#[derive(Subcommand)]
enum Command {
    /// Report keys used in the source that have no translation.
    Missing(MissingArgs),
    /// Report translations that the source never uses.
    Unused(CheckArgs),
    /// Remove translations that the source never uses.
    RemoveUnused(RemoveUnusedArgs),
    /// Report translations whose value is the same as in the base locale.
    EqBase(CheckArgs),
    /// Report keys whose interpolation variables differ from the base locale.
    CheckConsistentInterpolations(CheckArgs),
    /// Report values that use a variable name I18n reserves.
    CheckReservedInterpolations(CheckArgs),
    /// Report files whose emitted bytes differ from disk. Never writes.
    CheckNormalized(CheckArgs),
    /// Rewrite the locale files in the normalized form.
    ///
    /// Blocker B8: writing is opt-in. Without `--write` nothing is touched.
    Normalize(NormalizeArgs),
    /// Remove ignore rules that suppress no current issue.
    CleanConfig(CleanConfigArgs),
    /// Run every check and print the statistics header.
    Health(HealthArgs),
    /// Print every used key with its occurrences.
    Find(FindArgs),
    /// Generate a config from the project's own layout.
    ///
    /// Detects the locale files, the base locale, the search paths and the
    /// relative roots, then reads the result back before offering to write it.
    /// Exits 1 when the generated config still needs a human.
    InitConfig(InitConfigArgs),
    /// Convert a gem config (YAML or ERB) into the config this tool reads.
    ///
    /// Unsupported settings are dropped, each with the reason recorded in the
    /// output header. Exits 1 when the result still needs a human.
    MigrateConfig(MigrateConfigArgs),
}

impl Command {
    fn run(&self) -> Result<ExitStatus, String> {
        match self {
            Command::Missing(a) => a.run(),
            Command::Unused(a) => a.unused(),
            Command::RemoveUnused(a) => a.run(),
            Command::EqBase(a) => a.eq_base(),
            Command::CheckConsistentInterpolations(a) => a.consistent_interpolations(),
            Command::CheckReservedInterpolations(a) => a.reserved_interpolations(),
            Command::CheckNormalized(a) => a.normalized(),
            Command::Normalize(a) => a.run(),
            Command::CleanConfig(a) => a.run(),
            Command::Health(a) => a.run(),
            Command::Find(a) => a.run(),
            Command::InitConfig(a) => a.run(),
            Command::MigrateConfig(a) => a.run(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Cli, VERSION};
    use clap::CommandFactory;

    #[test]
    fn version_falls_back_to_the_cargo_package_version() {
        assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));
    }

    /// clap's own consistency check over the whole tree: a duplicated flag, a
    /// short option claimed twice, a subcommand name that collides. It panics
    /// on a fault, which is why one test asks it once.
    #[test]
    fn the_command_tree_is_well_formed() {
        Cli::command().debug_assert();
    }
}
