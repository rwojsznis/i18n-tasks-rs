//! ref: blocker B8. `--write` is required, `--dry-run` prints the diff, and a
//! deletion always needs `--allow-delete` on top of `--write`.

use crate::cli::args::Common;
use crate::cli::commands::report_deletions;
use crate::cli::exit::ExitStatus;
use crate::cli::out::{out, outln};
use i18n_tasks_rs::report::normalize;

/// The flags of the one command that writes.
///
/// Four `bool`s, so to the type checker they are the same thing and a
/// transposed pair compiles. Every one is read by name, off `self`, and never
/// passed on positionally.
#[derive(clap::Args, Clone, Debug)]
pub(crate) struct NormalizeArgs {
    #[command(flatten)]
    common: Common,
    /// Route every key through `data.write`, so keys physically move.
    #[arg(long, short = 'p')]
    pattern_router: bool,
    /// Write the changes. Required before anything is touched on disk.
    #[arg(long)]
    write: bool,
    /// Print a unified diff of every change, and write nothing.
    #[arg(long)]
    dry_run: bool,
    /// Allow deleting a file that ends up with no keys.
    #[arg(long)]
    allow_delete: bool,
}

impl NormalizeArgs {
    pub(crate) fn run(&self) -> Result<ExitStatus, String> {
        // Before the flags are judged, so a broken config is reported ahead of
        // a flag contradiction, as it was when the dispatch opened the project.
        let s = self.common.open()?;
        if self.write && self.dry_run {
            return Err("`--write` and `--dry-run` contradict each other".into());
        }
        let report = normalize::plan(&s.cfg, &s.store, &s.locales, self.pattern_router)?;
        let deletions = report.deletions();
        report_deletions(&deletions);
        if s.json {
            outln!("{}", report.to_normalize_json(&s, self.write)?);
        } else {
            out!("{}", report.to_normalize_text(self.dry_run));
        }
        if !self.write {
            if !s.json {
                outln!(
                    "Nothing was written. Pass `--write` to apply, `--dry-run` to see the diff."
                );
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
        Ok(ExitStatus::Ok)
    }
}
