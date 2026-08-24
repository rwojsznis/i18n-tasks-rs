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
use i18n_tasks_rs::config::{Config, DEFAULT_CONFIG_PATH};
use i18n_tasks_rs::data::load::Store;
use i18n_tasks_rs::report::missing::MissingType;
use i18n_tasks_rs::report::{Outcome, eq_base, interpolations, missing, normalize, unused};
use i18n_tasks_rs::stats::{ForestStats, forest_stats};
use i18n_tasks_rs::used::UsedKeys;
use i18n_tasks_rs::{clean_config, init, migrate};
use serde::ser::SerializeMap;
use serde::{Serialize, Serializer};
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
/// `Session::open`, so `--jobs` belongs to the commands that read a project and
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
}

/// ref: lib/i18n/tasks/command/option_parsers/locale.rb#ListParser
///
/// An empty list, or a lone `all`, means every configured locale. Otherwise
/// `base` stands in for the base locale, and if the base locale lands anywhere
/// but first it is swapped to the front, so a report always starts there.
fn resolve_locales(requested: &[String], store: &Store) -> Result<Vec<String>, String> {
    if requested.is_empty() || requested == ["all"] {
        return Ok(store.locales.clone());
    }
    let mut locales: Vec<String> = requested
        .iter()
        .map(|l| {
            if l == "base" {
                store.base_locale.clone()
            } else {
                l.clone()
            }
        })
        .collect();
    // ref: ListParser#move_base_locale_to_front!. A swap, not a rotation: the
    // locale that held the front takes the base locale's old slot.
    if let Some(pos) = locales
        .iter()
        .position(|l| *l == store.base_locale)
        .filter(|p| *p > 0)
    {
        locales.swap(0, pos);
    }
    for l in &locales {
        // ref: Locale::Validator::VALID_LOCALE_RE. Reported before the
        // membership check so a typo like `-l en,` names the real problem.
        if !valid_locale(l) {
            return Err(format!("invalid locale `{l}`"));
        }
        if !store.locales.contains(l) {
            return Err(format!(
                "unknown locale `{l}`. Configured locales: {}",
                store.locales.join(", ")
            ));
        }
    }
    Ok(locales)
}

/// ref: `/\A\w[\w\-.]*\z/i`. Ruby's `\w` here is ASCII-only.
fn valid_locale(locale: &str) -> bool {
    let mut chars = locale.chars();
    let first_ok = matches!(chars.next(), Some(c) if c.is_ascii_alphanumeric() || c == '_');
    first_ok && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
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

/// Everything the commands share, loaded once.
struct Session {
    cfg: Config,
    store: Store,
    locales: Vec<String>,
    json: bool,
}

impl Session {
    fn open(common: &Common) -> Result<Session, String> {
        install_pool(common.jobs)?;
        let cfg = Config::load(&common.config, common.root.as_deref())?;
        let store = Store::load(&cfg)?;
        for warning in &store.warnings {
            errln!("warning: {warning}");
        }
        let locales = resolve_locales(&common.requested_locales(), &store)?;
        Ok(Session {
            cfg,
            store,
            locales,
            json: common.format == Format::Json,
        })
    }

    fn scan(&self) -> Result<UsedKeys, String> {
        UsedKeys::scan(&self.cfg)
    }
}

/// One check's result, under the name the CLI and the JSON report it by.
///
/// The name belongs to the command, not to the report type: `InterpolationReport`
/// serves two checks, and `NormalizeReport` serves `check-normalized` as well as
/// `normalize`. So an associated const on the report type cannot carry it, and
/// this enum carries it instead. It lives here rather than in `report` because
/// these are the CLI's names for things.
///
/// `run` names a check once, then `emit` and `health` both read the name, the
/// outcome and the text out of it.
#[derive(Serialize)]
#[serde(untagged)]
enum Check {
    Missing(missing::MissingReport),
    Unused(unused::UnusedReport),
    EqBase(eq_base::EqBaseReport),
    ConsistentInterpolations(interpolations::InterpolationReport),
    ReservedInterpolations(interpolations::InterpolationReport),
    Normalized(normalize::NormalizeReport),
}

impl Check {
    /// The `check` field of the JSON envelope, and the field name `health`
    /// nests the report under.
    fn name(&self) -> &'static str {
        match self {
            Check::Missing(_) => "missing",
            Check::Unused(_) => "unused",
            Check::EqBase(_) => "eq_base",
            Check::ConsistentInterpolations(_) => "check_consistent_interpolations",
            Check::ReservedInterpolations(_) => "check_reserved_interpolations",
            Check::Normalized(_) => "check_normalized",
        }
    }

    fn outcome(&self) -> Outcome {
        match self {
            Check::Missing(r) => r.outcome(),
            Check::Unused(r) => r.outcome(),
            Check::EqBase(r) => r.outcome(),
            Check::ConsistentInterpolations(r) | Check::ReservedInterpolations(r) => r.outcome(),
            Check::Normalized(r) => r.outcome(),
        }
    }

    fn to_text(&self) -> String {
        match self {
            Check::Missing(r) => r.to_text(),
            Check::Unused(r) => r.to_text(),
            Check::EqBase(r) => r.to_text(),
            Check::ConsistentInterpolations(r) | Check::ReservedInterpolations(r) => r.to_text(),
            // `normalize` prints the same report differently, so the report has
            // two renderers and this is the read-only one.
            Check::Normalized(r) => r.to_check_text(),
        }
    }
}

fn run() -> Result<u8, String> {
    let cli = Cli::parse();
    match &cli.command {
        Command::Missing { common, types } => {
            let s = Session::open(common)?;
            // No `--types` means all three, which clap cannot express as a
            // default without also accepting an explicitly empty list.
            let types = types.clone().unwrap_or_else(|| MissingType::ALL.to_vec());
            let used = s.scan()?;
            let report = missing::report(&s.cfg, &s.store, &used, &s.locales, &types);
            emit(&s, &Check::Missing(report))
        }
        Command::Unused { common } => {
            let s = Session::open(common)?;
            let used = s.scan()?;
            let report = unused::report(&s.cfg, &s.store, &used, &s.locales);
            emit(&s, &Check::Unused(report))
        }
        Command::EqBase { common } => {
            let s = Session::open(common)?;
            let report = eq_base::report(&s.cfg, &s.store, &s.locales);
            emit(&s, &Check::EqBase(report))
        }
        Command::CheckConsistentInterpolations { common } => {
            let s = Session::open(common)?;
            let report = interpolations::inconsistent(&s.cfg, &s.store, &s.locales);
            emit(&s, &Check::ConsistentInterpolations(report))
        }
        Command::CheckReservedInterpolations { common } => {
            let s = Session::open(common)?;
            let report = interpolations::reserved(&s.store, &s.locales);
            emit(&s, &Check::ReservedInterpolations(report))
        }
        Command::CheckNormalized { common } => {
            let s = Session::open(common)?;
            // `false` is `force_pattern`: the conservative router, which keeps
            // every existing key in the file it is already in, so this reports
            // formatting only and never a move.
            let report = normalize::plan(&s.cfg, &s.store, &s.locales, false)?;
            emit(&s, &Check::Normalized(report))
        }
        Command::Normalize { common, flags } => {
            let s = Session::open(common)?;
            normalize_command(&s, flags)
        }
        Command::CleanConfig { common, write } => clean_config_command(common, *write),
        Command::Health { common } => health(common),
        Command::Find { common } => {
            let s = Session::open(common)?;
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
    let session = Session::open(common)?;
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
        outln!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "check": "clean_config",
                "written": write && !report.is_clean(),
                "config_digest": session.cfg.digest,
                "locales": session.locales,
                "stale_rules": report.stale_rules,
            }))
            .map_err(|e| e.to_string())?
        );
    } else {
        out!("{}", report.diff());
    }
    if report.is_clean() {
        return Ok(EXIT_OK);
    }
    if write {
        std::fs::write(&common.config, &report.cleaned)
            .map_err(|e| format!("cannot write {}: {e}", common.config.display()))?;
        Ok(EXIT_OK)
    } else {
        if !session.json {
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
        #[derive(Serialize)]
        struct Envelope<'a> {
            check: &'a str,
            passed: bool,
            config_digest: &'a str,
            locales: &'a [String],
            #[serde(flatten)]
            report: &'a Check,
        }
        let env = Envelope {
            check: check.name(),
            passed: outcome == Outcome::Clean,
            config_digest: &session.cfg.digest,
            locales: &session.locales,
            report: check,
        };
        outln!(
            "{}",
            serde_json::to_string_pretty(&env).map_err(|e| e.to_string())?
        );
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
    let s = Session::open(common)?;
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

    let found = checks.iter().any(|c| c.outcome() == Outcome::Found);

    if s.json {
        outln!(
            "{}",
            serde_json::to_string_pretty(&Health {
                passed: !found,
                config_digest: &s.cfg.digest,
                locales: &s.locales,
                stats: &stats,
                checks: &checks,
            })
            .map_err(|e| e.to_string())?
        );
    } else {
        outln!("{}", stats.to_text());
        for check in &checks {
            outln!();
            out!("{}", check.to_text());
        }
    }
    Ok(if found { EXIT_FOUND } else { EXIT_OK })
}

/// The `health` envelope: the shared four fields, the statistics header, then
/// one field per check, named by `Check::name`.
///
/// Written by hand rather than derived, because the field names come from the
/// checks at run time. A `serde_json::Map` would not do: without the
/// `preserve_order` feature it is a `BTreeMap`, so the five reports would come
/// out in alphabetical order instead of report order.
struct Health<'a> {
    passed: bool,
    config_digest: &'a str,
    locales: &'a [String],
    stats: &'a ForestStats,
    checks: &'a [Check],
}

impl Serialize for Health<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(5 + self.checks.len()))?;
        map.serialize_entry("check", "health")?;
        map.serialize_entry("passed", &self.passed)?;
        map.serialize_entry("config_digest", self.config_digest)?;
        map.serialize_entry("locales", self.locales)?;
        map.serialize_entry("stats", self.stats)?;
        for check in self.checks {
            map.serialize_entry(check.name(), check)?;
        }
        map.end()
    }
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
        outln!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "check": "normalize",
                "written": flags.write,
                "config_digest": s.cfg.digest,
                "locales": s.locales,
                "changes": report.changes,
                "files_routed": report.files_routed,
            }))
            .map_err(|e| e.to_string())?
        );
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

/// Dumps every used-key occurrence. Exists for diffing this tool against the
/// gem over the same project.
fn find_output(session: &Session, used: &UsedKeys) -> Result<u8, String> {
    if session.json {
        #[derive(Serialize)]
        struct Row<'a> {
            key: &'a str,
            occurrences: Vec<Loc<'a>>,
        }
        #[derive(Serialize)]
        struct Loc<'a> {
            path: String,
            line: usize,
            column: usize,
            raw_key: &'a str,
            candidate_keys: &'a [String],
        }
        #[derive(Serialize)]
        struct Out<'a> {
            check: &'static str,
            config_digest: &'a str,
            files_scanned: usize,
            files_prefiltered: usize,
            keys: Vec<Row<'a>>,
            patterns: Vec<&'a str>,
            opaque: Vec<Loc<'a>>,
        }
        let keys = used
            .keys
            .iter()
            .map(|(key, occs)| Row {
                key,
                occurrences: occs
                    .iter()
                    .map(|o| Loc {
                        path: o.path.display().to_string(),
                        line: o.line_num,
                        column: o.line_pos,
                        raw_key: &o.raw_key,
                        candidate_keys: &o.candidate_keys,
                    })
                    .collect(),
            })
            .collect();
        let mut patterns: Vec<&str> = used
            .pattern_sources
            .iter()
            .map(|(p, _)| p.as_str())
            .collect();
        patterns.sort_unstable();
        patterns.dedup();
        let out = Out {
            check: "find",
            config_digest: &session.cfg.digest,
            files_scanned: used.files_scanned,
            files_prefiltered: used.files_prefiltered,
            keys,
            patterns,
            opaque: used
                .opaque
                .iter()
                .map(|o| Loc {
                    path: o.path.display().to_string(),
                    line: o.line_num,
                    column: o.line_pos,
                    raw_key: &o.raw_key,
                    candidate_keys: &o.candidate_keys,
                })
                .collect(),
        };
        outln!(
            "{}",
            serde_json::to_string_pretty(&out).map_err(|e| e.to_string())?
        );
    } else {
        for (key, occs) in &used.keys {
            outln!("{key}");
            for o in occs {
                outln!("  {}:{}:{}", o.path.display(), o.line_num, o.line_pos);
            }
        }
        for (pattern, occ) in &used.pattern_sources {
            outln!("{pattern}  (pattern)");
            outln!("  {}:{}", occ.path.display(), occ.line_num);
        }
        for o in &used.opaque {
            outln!("(opaque)  {}:{}", o.path.display(), o.line_num);
        }
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
