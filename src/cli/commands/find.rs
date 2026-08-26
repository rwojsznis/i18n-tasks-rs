//! Dumps every used-key occurrence. Exists for diffing this tool against the
//! gem over the same project.

use crate::cli::args::Common;
use crate::cli::exit::ExitStatus;
use crate::cli::out::{out, outln};
use i18n_tasks_rs::report::find;

#[derive(clap::Args, Clone, Debug)]
pub(crate) struct FindArgs {
    #[command(flatten)]
    common: Common,
}

impl FindArgs {
    pub(crate) fn run(&self) -> Result<ExitStatus, String> {
        let s = self.common.open()?;
        let used = s.scan()?;
        if s.json {
            outln!("{}", find::to_json(&used, &s.cfg.digest)?);
        } else {
            out!("{}", find::to_text(&used));
        }
        Ok(ExitStatus::Ok)
    }
}
