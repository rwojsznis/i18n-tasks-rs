//! `clean-config`: the ignore rules that suppress nothing.

use crate::cli::args::Common;
use crate::cli::exit::ExitStatus;
use crate::cli::out::{out, outln};
use i18n_tasks_rs::clean_config;

#[derive(clap::Args, Clone, Debug)]
pub(crate) struct CleanConfigArgs {
    #[command(flatten)]
    common: Common,
    /// Write the cleaned config. Without this, print a diff only.
    #[arg(long)]
    write: bool,
}

impl CleanConfigArgs {
    pub(crate) fn run(&self) -> Result<ExitStatus, String> {
        let path = self.common.config_path();
        // The config's own bytes as well as the parsed config: the cleaned
        // output is an edit of the file, so it keeps the comments and the
        // formatting the parse throws away.
        let source = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read config {}: {e}", path.display()))?;
        let session = self.common.open()?;
        let used = session.scan()?;
        let report = clean_config::plan(
            &session.cfg,
            &session.store,
            &used,
            &session.locales,
            &source,
            path,
        )?;
        if session.json {
            outln!(
                "{}",
                report.to_json(&session, self.write && report.has_edit())?
            );
        } else {
            out!("{}", report.diff());
            out!("{}", report.manual_note());
        }
        if report.is_clean() {
            return Ok(ExitStatus::Ok);
        }
        if self.write {
            if report.has_edit() {
                std::fs::write(path, &report.cleaned)
                    .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
            }
            // A rule that only a human can remove leaves the config unclean, so
            // the run still reports a finding.
            Ok(ExitStatus::found(report.has_manual()))
        } else {
            if !session.json && report.has_edit() {
                outln!("Nothing was written. Pass `--write` to apply.");
            }
            Ok(ExitStatus::Found)
        }
    }
}
