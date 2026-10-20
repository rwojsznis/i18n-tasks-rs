//! ref: lib/i18n/tasks/command/commands/data.rb:14-22. Writes, as the gem's
//! does, and prints no text on success; an exception to blocker B8.
//! `--dry-run` prints the diff instead, and a deletion still needs
//! `--allow-delete`.

use crate::cli::args::Common;
use crate::cli::commands::report_deletions;
use crate::cli::exit::ExitStatus;
use crate::cli::out::{out, outln};
use i18n_tasks_rs::report::normalize;

#[derive(clap::Args, Clone, Debug)]
pub(crate) struct NormalizeArgs {
    #[command(flatten)]
    common: Common,
    /// Route every key through `data.write`, so keys physically move.
    #[arg(long, short = 'p', alias = "pattern_router")]
    pattern_router: bool,
    /// Print a unified diff of every change, and write nothing.
    #[arg(long)]
    dry_run: bool,
    /// Allow deleting a file that ends up with no keys.
    #[arg(long)]
    allow_delete: bool,
}

impl NormalizeArgs {
    pub(crate) fn run(&self) -> Result<ExitStatus, String> {
        let s = self.common.open()?;
        let report = normalize::plan(&s.cfg, &s.store, &s.locales, self.pattern_router)?;
        let deletions = report.deletions();
        report_deletions(&deletions);
        if self.dry_run {
            if s.json {
                outln!("{}", report.to_normalize_json(&s, false)?);
            } else {
                out!("{}", report.to_normalize_text(true));
            }
            return Ok(ExitStatus::Ok);
        }
        if !deletions.is_empty() && !self.allow_delete {
            return Err(
                "refusing to delete the files listed above. Pass `--allow-delete` to allow it."
                    .into(),
            );
        }
        normalize::apply(&report)?;
        if s.json {
            outln!("{}", report.to_normalize_json(&s, true)?);
        }
        Ok(ExitStatus::Ok)
    }
}
