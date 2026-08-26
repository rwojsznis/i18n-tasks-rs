//! `init-config`: a config generated from the project's own layout.
//!
//! The gem's answer here is `cp $(bundle exec i18n-tasks gem-path)/templates/...`,
//! which is the same file for every project. This one is generated from the
//! project. Writing is opt-in all the same (blocker B8).

use crate::cli::commands::write_config;
use crate::cli::exit::ExitStatus;
use crate::cli::out::{err, out};
use i18n_tasks_rs::init;
use std::path::{Path, PathBuf};

/// `to` and `root` are both `Option<PathBuf>` and `write` and `force` are both
/// `bool`, so every pair here is transposable without a compile error. The
/// resolution lives next to the declarations, so `run` reads the flags by name
/// and never by position.
#[derive(clap::Args, Clone, Debug)]
pub(crate) struct InitConfigArgs {
    /// Where to write. Default: config/i18n-tasks-rs.yml.
    #[arg(long, short = 'o')]
    to: Option<PathBuf>,
    /// Write the file. Without this the config goes to stdout.
    #[arg(long)]
    write: bool,
    /// Overwrite the destination when it already exists.
    #[arg(long)]
    force: bool,
    /// The project directory to inspect. Default: the working directory.
    #[arg(long)]
    root: Option<PathBuf>,
}

impl InitConfigArgs {
    /// The working directory, which is the root the gem uses.
    fn root(&self) -> &Path {
        self.root.as_deref().unwrap_or(Path::new("."))
    }

    /// `--to` is taken verbatim, so the destination is independent of the
    /// project; only the default sits under `--root`.
    fn target(&self) -> PathBuf {
        match &self.to {
            Some(to) => to.clone(),
            None => self.root().join(init::INIT_TARGET),
        }
    }

    pub(crate) fn run(&self) -> Result<ExitStatus, String> {
        let to = self.target();
        let generated = init::generate(self.root(), &to)?;

        if self.write {
            write_config(&to, &generated.output, self.force)?;
        } else {
            out!("{}", generated.output);
        }
        err!("{}", init::to_text(&generated, &to, self.write));
        Ok(ExitStatus::found(generated.needs_attention()))
    }
}
