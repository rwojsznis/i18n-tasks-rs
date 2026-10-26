//! ref: lib/i18n/tasks/command/commands/data.rb:14-22. Writes, as the gem's
//! does, and prints no text on success; an exception to blocker B8.
//! `--dry-run` prints the diff instead, a deletion still needs
//! `--allow-delete`, and a change outside the project root needs
//! `--allow-outside-root`.

use crate::cli::args::Common;
use crate::cli::commands::{OUTSIDE_ROOT_REFUSAL, report_deletions, report_outside_root};
use crate::cli::exit::ExitStatus;
use crate::cli::out::{out, outln};
use i18n_tasks_rs::report::normalize;

#[derive(clap::Args, Clone, Debug)]
pub(crate) struct NormalizeArgs {
    #[command(flatten)]
    common: Common,
    /// Route every key through `data.write`, so keys physically move.
    // The gem spells it `--pattern_router`. ref: lib/i18n/tasks/command/commands/data.rb:9-12
    #[arg(long, short = 'p', alias = "pattern_router")]
    pattern_router: bool,
    /// Print a unified diff of every change, and write nothing.
    #[arg(long)]
    dry_run: bool,
    /// Allow deleting a file that ends up with no keys.
    #[arg(long)]
    allow_delete: bool,
    /// Allow writing or deleting a file outside the project root.
    #[arg(long)]
    allow_outside_root: bool,
}

impl NormalizeArgs {
    pub(crate) fn run(&self) -> Result<ExitStatus, String> {
        let s = self.common.open()?;
        let report = normalize::plan(&s.cfg, &s.store, &s.locales, self.pattern_router)?;
        let deletions = report.deletions();
        let outside = report.outside_root(&s.cfg.root);
        report_deletions(&deletions);
        report_outside_root(&outside);
        if self.dry_run {
            if s.json {
                outln!("{}", report.to_normalize_json(&s, false)?);
            } else {
                out!("{}", report.to_normalize_text(true));
            }
            return Ok(ExitStatus::Ok);
        }
        if !outside.is_empty() && !self.allow_outside_root {
            return Err(OUTSIDE_ROOT_REFUSAL.into());
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
