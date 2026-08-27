//! The flags every project-reading command shares, and the project they open.

use crate::cli::out::errln;
use crate::cli::pool::install_pool;
use i18n_tasks_rs::config::DEFAULT_CONFIG_PATH;
use i18n_tasks_rs::session::{Session, SessionOptions};
use std::path::{Path, PathBuf};

/// ref: lib/i18n/tasks/cli.rb. These are flattened into each subcommand, not
/// declared on `Cli`, because the gem's flags belong to the task:
/// `i18n-tasks missing -c config/i18n-tasks.yml`. So they are per-subcommand
/// and `i18n-tasks-rs -c … missing` is an error, which `tests/cli_args.rs`
/// pins down.
#[derive(clap::Args, Clone, Debug)]
pub(crate) struct Common {
    /// Locale(s) to process. Special: base. Default: all.
    ///
    /// Comma-separated, repeatable, and concatenated with any trailing
    /// positional locales the same way the gem's `consume_positional` does.
    #[arg(long, short = 'l', value_delimiter = ',')]
    locales: Vec<String>,
    /// Trailing locales, equivalent to `--locales`.
    #[arg(value_name = "LOCALES", value_delimiter = ',')]
    positional_locales: Vec<String>,
    #[arg(long, short = 'c', default_value = DEFAULT_CONFIG_PATH)]
    config: PathBuf,
    #[arg(long, short = 'f', value_enum, default_value = "text")]
    format: Format,
    /// Directory every config path is relative to. Defaults to the working
    /// directory, which is what the gem uses.
    #[arg(long)]
    root: Option<PathBuf>,
    /// Worker threads for the source scan. Defaults to the core count.
    ///
    /// `--jobs 1` scans on one thread, for debugging. The output is identical
    /// either way; a parallel run that reorders anything is a bug.
    #[arg(long, short = 'j')]
    jobs: Option<usize>,
}

/// The two output forms. A `ValueEnum` rather than a `String`, so the valid set
/// is written once: clap validates the flag against the same list the commands
/// then match on, and `--help` lists it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum Format {
    Text,
    Json,
}

impl Common {
    /// ref: lib/i18n/tasks/cli.rb#parse_option, `consume_positional: true`.
    ///
    /// The flag values come first, then the positional ones. The gem
    /// concatenates rather than letting either win, so `-l es missing en`
    /// asks for both.
    fn requested_locales(&self) -> Vec<String> {
        self.locales
            .iter()
            .chain(&self.positional_locales)
            // Ruby's `String#split(",")` drops trailing empties, so the gem
            // reads `-l en,` as `-l en`. Skipping every empty entry covers
            // that and the `-l ,en` typo the gem would reject.
            .filter(|l| !l.is_empty())
            .cloned()
            .collect()
    }

    /// The config file, for `clean-config`, which reads its own bytes as well
    /// as the parsed config.
    pub(crate) fn config_path(&self) -> &Path {
        &self.config
    }

    /// Sizes the pool, then loads the project.
    ///
    /// The pool is installed here rather than in `Session::open` because it is
    /// a process-global side effect: the library reads a project, and the
    /// binary decides how many threads the process runs on.
    pub(crate) fn open(&self) -> Result<Session, String> {
        install_pool(self.jobs)?;
        let opts = SessionOptions {
            config: &self.config,
            root: self.root.as_deref(),
            locales: self.requested_locales(),
            json: self.format == Format::Json,
        };
        Session::open(&opts, |warning| errln!("warning: {warning}"))
    }
}
