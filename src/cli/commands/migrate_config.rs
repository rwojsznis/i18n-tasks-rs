//! `migrate-config`: a gem config translated into the config this tool reads.
//!
//! ref: blocker B3. The gem config is ERB over Ruby, so it is translated, not
//! renamed. Writing is opt-in here as it is everywhere else (blocker B8).

use crate::cli::commands::write_config;
use crate::cli::exit::ExitStatus;
use crate::cli::out::{err, out};
use i18n_tasks_rs::migrate;
use std::path::{Path, PathBuf};

/// Same shape as `InitConfigArgs` and the same reason for it, with `--from`
/// added.
#[derive(clap::Args, Clone, Debug)]
pub(crate) struct MigrateConfigArgs {
    /// The gem config to read. Default: config/i18n-tasks.yml, then
    /// config/i18n-tasks.yml.erb.
    #[arg(long, short = 'i')]
    from: Option<PathBuf>,
    /// Where to write. Default: config/i18n-tasks-rs.yml.
    #[arg(long, short = 'o')]
    to: Option<PathBuf>,
    /// Write the file. Without this the migrated config goes to stdout.
    #[arg(long)]
    write: bool,
    /// Overwrite the destination when it already exists.
    #[arg(long)]
    force: bool,
    /// Directory the default paths are looked up in.
    #[arg(long)]
    root: Option<PathBuf>,
}

impl MigrateConfigArgs {
    fn root(&self) -> &Path {
        self.root.as_deref().unwrap_or(Path::new("."))
    }

    fn target(&self) -> PathBuf {
        match &self.to {
            Some(to) => to.clone(),
            None => self.root().join(migrate::MIGRATION_TARGET),
        }
    }

    /// Without `--from`, the gem config is looked for under `--root`. Naming
    /// both candidates and the directory is the whole error message here: there
    /// is nothing else to go on.
    fn source(&self) -> Result<PathBuf, String> {
        match &self.from {
            Some(from) => Ok(from.clone()),
            None => migrate::find_gem_config(self.root()).ok_or_else(|| {
                format!(
                    "no gem config found. Looked for {} under {}. Name one with `--from`.",
                    migrate::GEM_CONFIG_CANDIDATES.join(" and "),
                    self.root().display()
                )
            }),
        }
    }

    pub(crate) fn run(&self) -> Result<ExitStatus, String> {
        let from = self.source()?;
        let to = self.target();
        if from == to {
            return Err("`--from` and `--to` are the same file".into());
        }
        let src = std::fs::read_to_string(&from)
            .map_err(|e| format!("cannot read {}: {e}", from.display()))?;
        let migration = migrate::migrate(&src, &from, &to)?;

        if self.write {
            write_config(&to, &migration.output, self.force)?;
        } else {
            out!("{}", migration.output);
        }
        err!("{}", migrate::to_text(&migration, &from, &to, self.write));
        Ok(ExitStatus::found(migration.needs_attention()))
    }
}
