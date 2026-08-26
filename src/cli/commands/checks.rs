//! The read-only checks: open the project, build one report, print it.
//!
//! Six commands over two flag sets, so one method per check rather than one
//! module each. Every one of them ends in `emit`, which is where the exit code
//! comes from.

use crate::cli::args::Common;
use crate::cli::commands::emit;
use crate::cli::exit::ExitStatus;
use i18n_tasks_rs::check::Check;
use i18n_tasks_rs::report::missing::MissingType;
use i18n_tasks_rs::report::{eq_base, interpolations, missing, normalize, unused};

/// A check that takes no flags of its own.
#[derive(clap::Args, Clone, Debug)]
pub(crate) struct CheckArgs {
    #[command(flatten)]
    common: Common,
}

impl CheckArgs {
    pub(crate) fn unused(&self) -> Result<ExitStatus, String> {
        let s = self.common.open()?;
        let used = s.scan()?;
        let report = unused::report(&s.cfg, &s.store, &used, &s.locales);
        emit(&s, &Check::Unused(report))
    }

    pub(crate) fn eq_base(&self) -> Result<ExitStatus, String> {
        let s = self.common.open()?;
        let report = eq_base::report(&s.cfg, &s.store, &s.locales);
        emit(&s, &Check::EqBase(report))
    }

    pub(crate) fn consistent_interpolations(&self) -> Result<ExitStatus, String> {
        let s = self.common.open()?;
        let report = interpolations::inconsistent(&s.cfg, &s.store, &s.locales);
        emit(&s, &Check::ConsistentInterpolations(report))
    }

    pub(crate) fn reserved_interpolations(&self) -> Result<ExitStatus, String> {
        let s = self.common.open()?;
        let report = interpolations::reserved(&s.store, &s.locales);
        emit(&s, &Check::ReservedInterpolations(report))
    }

    pub(crate) fn normalized(&self) -> Result<ExitStatus, String> {
        let s = self.common.open()?;
        // `false` is `force_pattern`: the conservative router, which keeps
        // every existing key in the file it is already in, so this reports
        // formatting only and never a move.
        let report = normalize::plan(&s.cfg, &s.store, &s.locales, false)?;
        emit(&s, &Check::Normalized(report))
    }
}

#[derive(clap::Args, Clone, Debug)]
pub(crate) struct MissingArgs {
    #[command(flatten)]
    common: Common,
    /// Subset of used, diff, plural. Defaults to all three.
    #[arg(long, value_delimiter = ',', value_parser = TrimmedMissingType)]
    types: Option<Vec<MissingType>>,
}

impl MissingArgs {
    pub(crate) fn run(&self) -> Result<ExitStatus, String> {
        let s = self.common.open()?;
        // No `--types` means all three, which clap cannot express as a default
        // without also accepting an explicitly empty list.
        let types = self
            .types
            .clone()
            .unwrap_or_else(|| MissingType::ALL.to_vec());
        let used = s.scan()?;
        let report = missing::report(&s.cfg, &s.store, &used, &s.locales, &types);
        emit(&s, &Check::Missing(report))
    }
}

/// `MissingType`'s `ValueEnum` parser with each item trimmed first.
///
/// ref: lib/i18n/tasks/command/option_parsers/enum.rb. The gem splits a list
/// option on `/\s*,\s*/`, so `--types "used, diff"` is valid there and was
/// valid here before `--types` became a `ValueEnum`. The trim is the only thing
/// this adds: the valid set still lives once, on the enum, and `--help` and the
/// error message both still read it from there.
#[derive(Clone)]
struct TrimmedMissingType;

impl clap::builder::TypedValueParser for TrimmedMissingType {
    type Value = MissingType;

    fn parse_ref(
        &self,
        cmd: &clap::Command,
        arg: Option<&clap::Arg>,
        value: &std::ffi::OsStr,
    ) -> Result<MissingType, clap::Error> {
        let inner = clap::builder::EnumValueParser::<MissingType>::new();
        // A non-UTF-8 value has no whitespace to trim that we can see. Hand it
        // over untouched and let the inner parser produce the error.
        match value.to_str() {
            Some(text) => inner.parse_ref(cmd, arg, std::ffi::OsStr::new(text.trim())),
            None => inner.parse_ref(cmd, arg, value),
        }
    }

    fn possible_values(
        &self,
    ) -> Option<Box<dyn Iterator<Item = clap::builder::PossibleValue> + '_>> {
        use clap::ValueEnum;
        Some(Box::new(
            MissingType::value_variants()
                .iter()
                .filter_map(MissingType::to_possible_value),
        ))
    }
}
