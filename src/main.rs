//! The `i18n-tasks-rs` CLI.
//!
//! Exit codes match the gem: 0 means the check passed, 1 means the check found
//! something, 2 means the tool itself failed. The gem signals the middle case
//! with an internal `:exit1`.

// A panicking library is a bug: this crate returns `Result` everywhere and the
// only command that writes does so under `--write`. Declared here rather than in
// `Cargo.toml`, because a manifest `[lints]` table also covers `tests/`, where a
// panic *is* the failure report. `clippy.toml` exempts the unit tests in `src/`.
#![warn(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use clap::{Parser, Subcommand};
use i18n_tasks_rs::check::{Check, any_found, health_json};
use i18n_tasks_rs::config::DEFAULT_CONFIG_PATH;
use i18n_tasks_rs::pattern::Pattern;
use i18n_tasks_rs::report::missing::MissingType;
use i18n_tasks_rs::report::{Outcome, eq_base, find, interpolations, missing, normalize, unused};
use i18n_tasks_rs::session::{Session, SessionOptions};
use i18n_tasks_rs::stats::forest_stats;
use i18n_tasks_rs::used::UsedKeys;
use i18n_tasks_rs::{clean_config, init, migrate};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Every write the CLI makes, with a closed pipe treated as the end of the run.
///
/// `println!` panics when the write fails, and a reader that quits early
/// (`i18n-tasks-rs unused | head`, or `| more` and then `q`) closes the pipe
/// under the writer. Rust ignores `SIGPIPE`, so that write comes back as
/// `ErrorKind::BrokenPipe` rather than killing the process, and restoring the
/// default handler needs `libc` and an `unsafe` block the crate forbids. So the
/// output goes through here instead: a closed pipe ends the output quietly and
/// leaves the exit code alone, and `unused | head` still says 1.
fn write_out(args: std::fmt::Arguments) {
    write_to(&mut std::io::stdout().lock(), args, "stdout");
}

fn write_err(args: std::fmt::Arguments) {
    write_to(&mut std::io::stderr().lock(), args, "stderr");
}

fn write_to(w: &mut impl std::io::Write, args: std::fmt::Arguments, name: &str) {
    match w.write_fmt(args) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {}
        // A full disk, say. Say so once, on the other stream, and carry on.
        Err(e) => {
            let _ = std::io::Write::write_fmt(
                &mut std::io::stderr(),
                format_args!("i18n-tasks-rs: cannot write to {name}: {e}\n"),
            );
        }
    }
}

/// `print!`, `println!`, `eprint!` and `eprintln!`, routed through `write_out`
/// and `write_err`. The CLI uses these four and never the standard ones.
macro_rules! out {
    ($($arg:tt)*) => { crate::write_out(format_args!($($arg)*)) };
}

macro_rules! outln {
    () => { crate::write_out(format_args!("\n")) };
    ($($arg:tt)*) => { crate::write_out(format_args!("{}\n", format_args!($($arg)*))) };
}

macro_rules! err {
    ($($arg:tt)*) => { crate::write_err(format_args!($($arg)*)) };
}

macro_rules! errln {
    ($($arg:tt)*) => { crate::write_err(format_args!("{}\n", format_args!($($arg)*))) };
}

const EXIT_OK: u8 = 0;
const EXIT_FOUND: u8 = 1;
const EXIT_FAILURE: u8 = 2;

#[derive(Parser)]
#[command(
    name = "i18n-tasks-rs",
    about = "Manage translations in Ruby applications. A Rust port of i18n-tasks.",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// Sizes the `rayon` pool the scan fans out over. Called once, from
/// `Common::open`, so `--jobs` belongs to the commands that read a project and
/// not to `migrate-config`, which scans nothing.
///
/// Without `--jobs`, `rayon` uses the core count.
fn install_pool(jobs: Option<usize>) -> Result<(), String> {
    let mut builder = rayon::ThreadPoolBuilder::new();
    if let Some(n) = jobs {
        if n == 0 {
            return Err("--jobs must be at least 1".to_string());
        }
        builder = builder.num_threads(n);
    }
    // The Prism visitor recurses over the AST, and a worker thread's default
    // stack is a quarter of the main thread's. Match the main thread instead of
    // failing on one deeply nested file.
    builder
        .stack_size(8 * 1024 * 1024)
        .build_global()
        .map_err(|e| match jobs {
            Some(n) => format!("cannot start {n} worker threads: {e}"),
            // Without `--jobs` the count is rayon's, not ours, so name no
            // number rather than a made-up one.
            None => format!("cannot start the worker thread pool: {e}"),
        })
}

/// The flags every project-reading command shares.
///
/// ref: lib/i18n/tasks/cli.rb. These are flattened into each subcommand, not
/// declared on `Cli`, because the gem's flags belong to the task:
/// `i18n-tasks missing -c config/i18n-tasks.yml`. So they are per-subcommand
/// and `i18n-tasks-rs -c … missing` is an error, which `tests/cli_args.rs`
/// pins down.
#[derive(clap::Args, Clone, Debug)]
struct Common {
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
/// is written once: clap validates the flag against the same list that `run`
/// then matches on, and `--help` lists it.
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

    /// Sizes the pool, then loads the project.
    ///
    /// The pool is installed here rather than in `Session::open` because it is
    /// a process-global side effect: the library reads a project, and the
    /// binary decides how many threads the process runs on.
    fn open(&self) -> Result<Session, String> {
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

#[derive(Subcommand)]
enum Command {
    /// Report keys used in the source that have no translation.
    Missing {
        #[command(flatten)]
        common: Common,
        /// Subset of used, diff, plural. Defaults to all three.
        #[arg(long, value_delimiter = ',', value_parser = TrimmedMissingType)]
        types: Option<Vec<MissingType>>,
    },
    /// Report translations that the source never uses.
    Unused {
        #[command(flatten)]
        common: Common,
    },
    /// Remove translations that the source never uses.
    RemoveUnused {
        #[command(flatten)]
        common: Common,
        #[command(flatten)]
        flags: RemoveUnusedFlags,
    },
    /// Report translations whose value is the same as in the base locale.
    EqBase {
        #[command(flatten)]
        common: Common,
    },
    /// Report keys whose interpolation variables differ from the base locale.
    CheckConsistentInterpolations {
        #[command(flatten)]
        common: Common,
    },
    /// Report values that use a variable name I18n reserves.
    CheckReservedInterpolations {
        #[command(flatten)]
        common: Common,
    },
    /// Report files whose emitted bytes differ from disk. Never writes.
    CheckNormalized {
        #[command(flatten)]
        common: Common,
    },
    /// Rewrite the locale files in the normalized form.
    ///
    /// Blocker B8: writing is opt-in. Without `--write` nothing is touched.
    Normalize {
        #[command(flatten)]
        common: Common,
        #[command(flatten)]
        flags: NormalizeFlags,
    },
    /// Remove ignore rules that suppress no current issue.
    CleanConfig {
        #[command(flatten)]
        common: Common,
        /// Write the cleaned config. Without this, print a diff only.
        #[arg(long)]
        write: bool,
    },
    /// Run every check and print the statistics header.
    Health {
        #[command(flatten)]
        common: Common,
    },
    /// Print every used key with its occurrences.
    Find {
        #[command(flatten)]
        common: Common,
    },
    /// Generate a config from the project's own layout.
    ///
    /// Detects the locale files, the base locale, the search paths and the
    /// relative roots, then reads the result back before offering to write it.
    /// Exits 1 when the generated config still needs a human.
    InitConfig {
        #[command(flatten)]
        flags: InitFlags,
    },
    /// Convert a gem config (YAML or ERB) into the config this tool reads.
    ///
    /// Unsupported settings are dropped, each with the reason recorded in the
    /// output header. Exits 1 when the result still needs a human.
    MigrateConfig {
        #[command(flatten)]
        flags: MigrateFlags,
    },
}

/// `normalize`'s own flags.
///
/// A struct rather than four `bool` parameters on `normalize_command`: to the
/// type checker the four are the same thing, so a transposed pair compiles and
/// silently changes what the one command that writes to disk does. Clippy names
/// that `fn_params_excessive_bools`.
///
/// The count itself is the CLI's, not ours, so pedantic clippy now says
/// `struct_excessive_bools` here instead. That one is harmless: a field is named
/// at every use, and clap needs a `bool` per flag.
#[derive(clap::Args, Clone, Debug)]
struct NormalizeFlags {
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

#[derive(clap::Args, Clone, Debug)]
struct RemoveUnusedFlags {
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
}

/// `init-config`'s flags, plus the two defaults they resolve to.
///
/// `to` and `root` are both `Option<PathBuf>` and `write` and `force` are both
/// `bool`, so every pair here is transposable without a compile error. The
/// resolution lives next to the declarations, so `init_config` reads the flags
/// by name and never by position.
#[derive(clap::Args, Clone, Debug)]
struct InitFlags {
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

impl InitFlags {
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
}

/// `migrate-config`'s flags. Same shape as `InitFlags` and the same reason for
/// it, with `--from` added.
#[derive(clap::Args, Clone, Debug)]
struct MigrateFlags {
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

impl MigrateFlags {
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
}

/// `MissingType`'s `ValueEnum` parser with each item trimmed first.
///
/// ref: lib/i18n/tasks/command/option_parsers/enum.rb. The gem splits a list
/// option on `/\s*,\s*/`, so `--types "used, diff"` is valid there and was
/// valid here before `--types` became a `ValueEnum`. The trim is the only thing
/// this adds: the valid set still lives once, on the enum, and `--help` and the
/// error message both still read it from there.
#[derive(Clone)]
struct TrimmedMissingType;

impl clap::builder::TypedValueParser for TrimmedMissingType {
    type Value = MissingType;

    fn parse_ref(
        &self,
        cmd: &clap::Command,
        arg: Option<&clap::Arg>,
        value: &std::ffi::OsStr,
    ) -> Result<MissingType, clap::Error> {
        let inner = clap::builder::EnumValueParser::<MissingType>::new();
        // A non-UTF-8 value has no whitespace to trim that we can see. Hand it
        // over untouched and let the inner parser produce the error.
        match value.to_str() {
            Some(text) => inner.parse_ref(cmd, arg, std::ffi::OsStr::new(text.trim())),
            None => inner.parse_ref(cmd, arg, value),
        }
    }

    fn possible_values(
        &self,
    ) -> Option<Box<dyn Iterator<Item = clap::builder::PossibleValue> + '_>> {
        use clap::ValueEnum;
        Some(Box::new(
            MissingType::value_variants()
                .iter()
                .filter_map(MissingType::to_possible_value),
        ))
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(message) => {
            errln!("i18n-tasks-rs: {message}");
            ExitCode::from(EXIT_FAILURE)
        }
    }
}

fn run() -> Result<u8, String> {
    let cli = Cli::parse();
    match &cli.command {
        Command::Missing { common, types } => {
            let s = common.open()?;
            // No `--types` means all three, which clap cannot express as a
            // default without also accepting an explicitly empty list.
            let types = types.clone().unwrap_or_else(|| MissingType::ALL.to_vec());
            let used = s.scan()?;
            let report = missing::report(&s.cfg, &s.store, &used, &s.locales, &types);
            emit(&s, &Check::Missing(report))
        }
        Command::Unused { common } => {
            let s = common.open()?;
            let used = s.scan()?;
            let report = unused::report(&s.cfg, &s.store, &used, &s.locales);
            emit(&s, &Check::Unused(report))
        }
        Command::RemoveUnused { common, flags } => {
            let s = common.open()?;
            remove_unused_command(&s, flags)
        }
        Command::EqBase { common } => {
            let s = common.open()?;
            let report = eq_base::report(&s.cfg, &s.store, &s.locales);
            emit(&s, &Check::EqBase(report))
        }
        Command::CheckConsistentInterpolations { common } => {
            let s = common.open()?;
            let report = interpolations::inconsistent(&s.cfg, &s.store, &s.locales);
            emit(&s, &Check::ConsistentInterpolations(report))
        }
        Command::CheckReservedInterpolations { common } => {
            let s = common.open()?;
            let report = interpolations::reserved(&s.store, &s.locales);
            emit(&s, &Check::ReservedInterpolations(report))
        }
        Command::CheckNormalized { common } => {
            let s = common.open()?;
            // `false` is `force_pattern`: the conservative router, which keeps
            // every existing key in the file it is already in, so this reports
            // formatting only and never a move.
            let report = normalize::plan(&s.cfg, &s.store, &s.locales, false)?;
            emit(&s, &Check::Normalized(report))
        }
        Command::Normalize { common, flags } => {
            let s = common.open()?;
            normalize_command(&s, flags)
        }
        Command::CleanConfig { common, write } => clean_config_command(common, *write),
        Command::Health { common } => health(common),
        Command::Find { common } => {
            let s = common.open()?;
            let used = s.scan()?;
            find_output(&s, &used)
        }
        Command::InitConfig { flags } => init_config(flags),
        Command::MigrateConfig { flags } => migrate_config(flags),
    }
}

fn clean_config_command(common: &Common, write: bool) -> Result<u8, String> {
    let source = std::fs::read_to_string(&common.config)
        .map_err(|e| format!("cannot read config {}: {e}", common.config.display()))?;
    let session = common.open()?;
    let used = session.scan()?;
    let report = clean_config::plan(
        &session.cfg,
        &session.store,
        &used,
        &session.locales,
        &source,
        &common.config,
    )?;
    if session.json {
        outln!("{}", report.to_json(&session, write && report.has_edit())?);
    } else {
        out!("{}", report.diff());
        out!("{}", report.manual_note());
    }
    if report.is_clean() {
        return Ok(EXIT_OK);
    }
    if write {
        if report.has_edit() {
            std::fs::write(&common.config, &report.cleaned)
                .map_err(|e| format!("cannot write {}: {e}", common.config.display()))?;
        }
        // A rule that only a human can remove leaves the config unclean, so
        // the run still reports a finding.
        Ok(if report.has_manual() {
            EXIT_FOUND
        } else {
            EXIT_OK
        })
    } else {
        if !session.json && report.has_edit() {
            outln!("Nothing was written. Pass `--write` to apply.");
        }
        Ok(EXIT_FOUND)
    }
}

/// The gem's answer here is `cp $(bundle exec i18n-tasks gem-path)/templates/...`,
/// which is the same file for every project. This one is generated from the
/// project. Writing is opt-in all the same (blocker B8).
fn init_config(flags: &InitFlags) -> Result<u8, String> {
    let to = flags.target();
    let generated = init::generate(flags.root(), &to)?;

    if flags.write {
        write_config(&to, &generated.output, flags.force)?;
    } else {
        out!("{}", generated.output);
    }
    err!("{}", init::to_text(&generated, &to, flags.write));
    Ok(if generated.needs_attention() {
        EXIT_FOUND
    } else {
        EXIT_OK
    })
}

/// Shared by the two commands that produce a config. Neither replaces a file
/// someone may have edited without being told to.
fn write_config(to: &Path, contents: &str, force: bool) -> Result<(), String> {
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

/// ref: blocker B3. The gem config is ERB over Ruby, so it is translated, not
/// renamed. Writing is opt-in here as it is everywhere else (blocker B8).
fn migrate_config(flags: &MigrateFlags) -> Result<u8, String> {
    let from = flags.source()?;
    let to = flags.target();
    if from == to {
        return Err("`--from` and `--to` are the same file".into());
    }
    let src = std::fs::read_to_string(&from)
        .map_err(|e| format!("cannot read {}: {e}", from.display()))?;
    let migration = migrate::migrate(&src, &from, &to)?;

    if flags.write {
        write_config(&to, &migration.output, flags.force)?;
    } else {
        out!("{}", migration.output);
    }
    err!("{}", migrate::to_text(&migration, &from, &to, flags.write));
    Ok(if migration.needs_attention() {
        EXIT_FOUND
    } else {
        EXIT_OK
    })
}

/// Prints one check and returns its exit code. The JSON form wraps the report
/// in the shared envelope; the text form is the report's own.
fn emit(session: &Session, check: &Check) -> Result<u8, String> {
    let outcome = check.outcome();
    if session.json {
        outln!("{}", check.to_json(session)?);
    } else {
        out!("{}", check.to_text());
    }
    Ok(if outcome == Outcome::Clean {
        EXIT_OK
    } else {
        EXIT_FOUND
    })
}

/// ref: lib/i18n/tasks/command/commands/health.rb
///
/// Every check runs, even after one fails: the gem builds the result array
/// eagerly and only then calls `.detect`, so there is no short-circuit.
fn health(common: &Common) -> Result<u8, String> {
    let s = common.open()?;
    let stats = forest_stats(&s.store, &s.locales);
    // A silent pass on an empty data set is the worst possible outcome.
    if stats.key_count == 0 {
        return Err("no keys detected. Check `data.read` and the working directory.".into());
    }
    let used = s.scan()?;
    let checks = vec![
        Check::Missing(missing::report(
            &s.cfg,
            &s.store,
            &used,
            &s.locales,
            &MissingType::ALL,
        )),
        Check::Unused(unused::report(&s.cfg, &s.store, &used, &s.locales)),
        Check::ConsistentInterpolations(interpolations::inconsistent(&s.cfg, &s.store, &s.locales)),
        Check::ReservedInterpolations(interpolations::reserved(&s.store, &s.locales)),
        // `health` never writes. This step only compares the emitted bytes
        // against the file on disk.
        Check::Normalized(normalize::plan(&s.cfg, &s.store, &s.locales, false)?),
    ];

    let found = any_found(&checks);

    if s.json {
        outln!("{}", health_json(&s, &stats, &checks)?);
    } else {
        outln!("{}", stats.to_text());
        for check in &checks {
            outln!();
            out!("{}", check.to_text());
        }
    }
    Ok(if found { EXIT_FOUND } else { EXIT_OK })
}

/// ref: blocker B8. `--write` is required, `--dry-run` prints the diff, and a
/// deletion always needs `--allow-delete` on top of `--write`.
fn normalize_command(s: &Session, flags: &NormalizeFlags) -> Result<u8, String> {
    if flags.write && flags.dry_run {
        return Err("`--write` and `--dry-run` contradict each other".into());
    }
    let report = normalize::plan(&s.cfg, &s.store, &s.locales, flags.pattern_router)?;
    let deletions = report.deletions();
    // Always print the deletion list, whether or not the run may act on it.
    if !deletions.is_empty() {
        errln!("{} file(s) end up with no keys:", deletions.len());
        for d in &deletions {
            errln!("  {}", d.display);
        }
    }
    if s.json {
        outln!("{}", report.to_normalize_json(s, flags.write)?);
    } else {
        out!("{}", report.to_normalize_text(flags.dry_run));
    }
    if !flags.write {
        if !s.json {
            outln!("Nothing was written. Pass `--write` to apply, `--dry-run` to see the diff.");
        }
        return Ok(EXIT_OK);
    }
    if !deletions.is_empty() && !flags.allow_delete {
        return Err(
            "refusing to delete the files listed above. Pass `--allow-delete` to allow it.".into(),
        );
    }
    normalize::apply(&report)?;
    Ok(EXIT_OK)
}

/// ref: lib/i18n/tasks/command/commands/usages.rb#remove_unused
fn remove_unused_command(s: &Session, flags: &RemoveUnusedFlags) -> Result<u8, String> {
    if flags.write && flags.dry_run {
        return Err("`--write` and `--dry-run` contradict each other".into());
    }
    let used = s.scan()?;
    let mut unused = unused::report(&s.cfg, &s.store, &used, &s.locales);
    if let Some(pattern) = flags.pattern.as_deref().map(Pattern::compile) {
        unused.select_pattern(&pattern);
    }
    if !unused.has_removable() {
        if s.json {
            outln!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "check": "remove_unused",
                    "written": false,
                    "config_digest": s.cfg.digest,
                    "locales": s.locales,
                    "changes": [],
                    "files_routed": 0,
                }))
                .map_err(|e| e.to_string())?
            );
        } else {
            outln!("No unused keys to remove");
        }
        return Ok(EXIT_OK);
    }
    let mut cfg = s.cfg.clone();
    if flags.keep_order {
        cfg.data.keep_order = true;
    }
    let report = normalize::plan_filtered(&cfg, &s.store, &s.locales, false, &|locale, key| {
        !unused.removable(locale, key)
    })?;
    let deletions = report.deletions();
    if !deletions.is_empty() {
        errln!("{} file(s) end up with no keys:", deletions.len());
        for deletion in &deletions {
            errln!("  {}", deletion.display);
        }
    }
    if s.json {
        outln!("{}", report.to_write_json(s, "remove_unused", flags.write)?);
    } else {
        out!("{}", unused.to_text());
        out!("{}", report.to_normalize_text(flags.dry_run));
    }
    if !flags.write {
        if !s.json {
            outln!("Nothing was written. Pass `--write` to apply, `--dry-run` to see the diff.");
        }
        return Ok(EXIT_OK);
    }
    if !deletions.is_empty() && !flags.allow_delete {
        return Err(
            "refusing to delete the files listed above. Pass `--allow-delete` to allow it.".into(),
        );
    }
    normalize::apply(&report)?;
    Ok(EXIT_OK)
}

/// Dumps every used-key occurrence. Exists for diffing this tool against the
/// gem over the same project.
fn find_output(session: &Session, used: &UsedKeys) -> Result<u8, String> {
    if session.json {
        outln!("{}", find::to_json(used, &session.cfg.digest)?);
    } else {
        out!("{}", find::to_text(used));
    }
    Ok(EXIT_OK)
}

#[cfg(test)]
mod tests {
    use super::install_pool;

    /// The second `build_global` in a process always fails, which is the only
    /// way to reach the error arm without a resource limit. Keep this the only
    /// test in this binary: it installs a process-global pool.
    #[test]
    fn pool_error_does_not_invent_a_thread_count() {
        install_pool(Some(2)).expect("first install builds the global pool");
        let err = install_pool(None).expect_err("second install must fail");
        assert!(
            !err.contains('0'),
            "no --jobs, so the message must not name a thread count: {err}"
        );
        let err = install_pool(Some(4)).expect_err("second install must fail");
        assert!(
            err.contains("4 worker threads"),
            "with --jobs the message names the asked-for count: {err}"
        );
    }
}
