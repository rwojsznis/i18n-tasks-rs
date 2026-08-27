//! One module per command, each holding its own flags, how they resolve, and
//! what the command does. Only what more than one command needs lives here.

pub(crate) mod checks;
pub(crate) mod clean_config;
pub(crate) mod find;
pub(crate) mod health;
pub(crate) mod init_config;
pub(crate) mod migrate_config;
pub(crate) mod normalize;
pub(crate) mod remove_unused;

use crate::cli::exit::ExitStatus;
use crate::cli::out::{errln, out, outln};
use i18n_tasks_rs::check::Check;
use i18n_tasks_rs::report::normalize::FileChange;
use i18n_tasks_rs::session::Session;
use std::path::Path;

/// Prints one check and returns its exit code. The JSON form wraps the report
/// in the shared envelope; the text form is the report's own.
pub(crate) fn emit(session: &Session, check: &Check) -> Result<ExitStatus, String> {
    if session.json {
        outln!("{}", check.to_json(session)?);
    } else {
        out!("{}", check.to_text());
    }
    Ok(ExitStatus::from(check.outcome()))
}

/// Names the files a write plan empties. Both writing commands print this
/// whether or not the run may act on it, and before they decide.
pub(crate) fn report_deletions(deletions: &[&FileChange]) {
    if deletions.is_empty() {
        return;
    }
    errln!("{} file(s) end up with no keys:", deletions.len());
    for deletion in deletions {
        errln!("  {}", deletion.display);
    }
}

/// Shared by the two commands that produce a config. Neither replaces a file
/// someone may have edited without being told to.
pub(crate) fn write_config(to: &Path, contents: &str, force: bool) -> Result<(), String> {
    if to.exists() && !force {
        return Err(format!(
            "{} already exists. Pass `--force` to overwrite it.",
            to.display()
        ));
    }
    if let Some(dir) = to.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    std::fs::write(to, contents).map_err(|e| format!("cannot write {}: {e}", to.display()))
}
