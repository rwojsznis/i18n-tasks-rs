//! ref: lib/i18n/tasks/command/commands/health.rb

use crate::cli::args::Common;
use crate::cli::exit::ExitStatus;
use crate::cli::out::{out, outln};
use i18n_tasks_rs::check::{Check, any_found, health_json};
use i18n_tasks_rs::report::missing::MissingType;
use i18n_tasks_rs::report::{interpolations, missing, normalize, unused};
use i18n_tasks_rs::stats::forest_stats;

#[derive(clap::Args, Clone, Debug)]
pub(crate) struct HealthArgs {
    #[command(flatten)]
    common: Common,
}

impl HealthArgs {
    /// Every check runs, even after one fails: the gem builds the result array
    /// eagerly and only then calls `.detect`, so there is no short-circuit.
    pub(crate) fn run(&self) -> Result<ExitStatus, String> {
        let s = self.common.open()?;
        let stats = forest_stats(&s.store, &s.locales);
        // A silent pass on an empty data set is the worst possible outcome.
        if stats.key_count == 0 {
            return Err("no keys detected. Check `data.read` and the working directory.".into());
        }
        let used = s.scan()?;
        let checks = vec![
            Check::Missing(missing::report(
                &s.cfg,
                &s.store,
                &used,
                &s.locales,
                &MissingType::ALL,
            )),
            Check::Unused(unused::report(&s.cfg, &s.store, &used, &s.locales)),
            Check::ConsistentInterpolations(interpolations::inconsistent(
                &s.cfg, &s.store, &s.locales,
            )),
            Check::ReservedInterpolations(interpolations::reserved(&s.store, &s.locales)),
            // `health` never writes. This step only compares the emitted bytes
            // against the file on disk.
            Check::Normalized(normalize::plan(&s.cfg, &s.store, &s.locales, false)?),
        ];

        let found = any_found(&checks);

        if s.json {
            outln!("{}", health_json(&s, &stats, &checks)?);
        } else {
            outln!("{}", stats.to_text());
            for check in &checks {
                outln!();
                out!("{}", check.to_text());
            }
        }
        Ok(ExitStatus::found(found))
    }
}
