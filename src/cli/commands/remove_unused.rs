//! ref: lib/i18n/tasks/command/commands/usages.rb#remove_unused

use crate::cli::args::Common;
use crate::cli::commands::report_deletions;
use crate::cli::exit::ExitStatus;
use crate::cli::out::{out, outln};
use i18n_tasks_rs::check::RemoveUnusedJson;
use i18n_tasks_rs::pattern::Pattern;
use i18n_tasks_rs::report::{normalize, unused};

/// Four `bool`s and a pattern. Same reason as `NormalizeArgs` for keeping them
/// in a struct: every one is read by name and never passed on positionally.
#[derive(clap::Args, Clone, Debug)]
pub(crate) struct RemoveUnusedArgs {
    #[command(flatten)]
    common: Common,
    /// Remove only unused keys that match this key pattern.
    #[arg(long, short = 'p')]
    pattern: Option<String>,
    /// Preserve the order of keys that remain.
    #[arg(long, short = 'k')]
    keep_order: bool,
    /// Write the changes. Without this, print the plan only.
    #[arg(long)]
    write: bool,
    /// Print a unified diff of every change, and write nothing.
    #[arg(long)]
    dry_run: bool,
    /// Allow deleting a file that ends up with no keys.
    #[arg(long)]
    allow_delete: bool,
    /// Allow writes when calls with statically unknown keys were found.
    #[arg(long)]
    allow_opaque: bool,
}

impl RemoveUnusedArgs {
    pub(crate) fn run(&self) -> Result<ExitStatus, String> {
        let s = self.common.open()?;
        if self.write && self.dry_run {
            return Err("`--write` and `--dry-run` contradict each other".into());
        }
        let used = s.scan()?;
        let mut unused = unused::report(&s.cfg, &s.store, &used, &s.locales);
        if let Some(pattern) = self.pattern.as_deref().map(Pattern::compile) {
            unused.select_pattern(&pattern);
        }
        if !unused.has_removable() {
            if s.json {
                outln!("{}", RemoveUnusedJson::nothing_to_remove().to_json(&s)?);
            } else {
                outln!("No unused keys to remove");
            }
            return Ok(ExitStatus::Ok);
        }
        let mut cfg = s.cfg.clone();
        if self.keep_order {
            cfg.data.keep_order = true;
        }
        let report =
            normalize::plan_filtered(&cfg, &s.store, &s.locales, false, &|locale, key| {
                !unused.removable(locale, key)
            })?;
        let deletions = report.deletions();
        report_deletions(&deletions);
        if !s.json {
            out!("{}", unused.to_text());
            out!("{}", report.to_normalize_text(self.dry_run));
        }
        // Every refusal below still prints the plan under `-f json`, so the
        // envelope always says what would have happened and `written` is the
        // only field that changes.
        let planned = RemoveUnusedJson::planned(&unused, &report);
        if !self.write {
            if s.json {
                outln!("{}", planned.to_json(&s)?);
            } else {
                outln!(
                    "Nothing was written. Pass `--write` to apply, `--dry-run` to see the diff."
                );
            }
            return Ok(ExitStatus::Ok);
        }
        if !unused.opaque.is_empty() && !self.allow_opaque {
            if s.json {
                outln!("{}", planned.to_json(&s)?);
            }
            return Err(format!(
                "refusing to remove keys while {} translation call(s) have an unknown key. \
                 Add `# i18n-tasks-use` or `ignore_unused` rules, or pass `--allow-opaque` \
                 after you verify the calls.",
                unused.opaque.len()
            ));
        }
        if !deletions.is_empty() && !self.allow_delete {
            if s.json {
                outln!("{}", planned.to_json(&s)?);
            }
            return Err(
                "refusing to delete the files listed above. Pass `--allow-delete` to allow it."
                    .into(),
            );
        }
        normalize::apply(&report)?;
        if s.json {
            outln!("{}", planned.written().to_json(&s)?);
        }
        Ok(ExitStatus::Ok)
    }
}
