//! The `i18n-tasks-rs` CLI.
//!
//! Exit codes match the gem: 0 means the check passed, 1 means the check found
//! something, 2 means the tool itself failed. The gem signals the middle case
//! with an internal `:exit1`.

use clap::{Parser, Subcommand};
use i18n_tasks_rs::config::{Config, DEFAULT_CONFIG_PATH};
use i18n_tasks_rs::data::load::Store;
use i18n_tasks_rs::report::missing::MissingType;
use i18n_tasks_rs::report::{Outcome, interpolations, missing, normalize, unused};
use i18n_tasks_rs::stats::{ForestStats, forest_stats};
use i18n_tasks_rs::used::UsedKeys;
use i18n_tasks_rs::{init, migrate};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

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
    #[arg(long, short = 'f', value_parser = ["text", "json"], default_value = "text")]
    format: String,
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
        #[arg(long, value_delimiter = ',')]
        types: Option<Vec<String>>,
    },
    /// Report translations that the source never uses.
    Unused {
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
    },
    /// Convert a gem config (YAML or ERB) into the config this tool reads.
    ///
    /// Unsupported settings are dropped, each with the reason recorded in the
    /// output header. Exits 1 when the result still needs a human.
    MigrateConfig {
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
    },
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(message) => {
            eprintln!("i18n-tasks-rs: {message}");
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
            eprintln!("warning: {warning}");
        }
        let locales = resolve_locales(&common.requested_locales(), &store)?;
        Ok(Session {
            cfg,
            store,
            locales,
            json: common.format == "json",
        })
    }

    fn scan(&self) -> Result<UsedKeys, String> {
        UsedKeys::scan(&self.cfg)
    }
}

fn run() -> Result<u8, String> {
    let cli = Cli::parse();
    match &cli.command {
        Command::Missing { common, types } => {
            let s = Session::open(common)?;
            let types = parse_types(types.as_deref())?;
            let used = s.scan()?;
            let report = missing::report(&s.cfg, &s.store, &used, &s.locales, &types);
            emit(&s, "missing", &report, report.outcome(), || {
                report.to_text()
            })
        }
        Command::Unused { common } => {
            let s = Session::open(common)?;
            let used = s.scan()?;
            let report = unused::report(&s.cfg, &s.store, &used, &s.locales);
            emit(&s, "unused", &report, report.outcome(), || report.to_text())
        }
        Command::CheckConsistentInterpolations { common } => {
            let s = Session::open(common)?;
            let report = interpolations::inconsistent(&s.cfg, &s.store, &s.locales);
            emit(
                &s,
                "check_consistent_interpolations",
                &report,
                report.outcome(),
                || report.to_text(),
            )
        }
        Command::CheckReservedInterpolations { common } => {
            let s = Session::open(common)?;
            let report = interpolations::reserved(&s.store, &s.locales);
            emit(
                &s,
                "check_reserved_interpolations",
                &report,
                report.outcome(),
                || report.to_text(),
            )
        }
        Command::CheckNormalized { common } => {
            let s = Session::open(common)?;
            let report = normalize::plan(&s.cfg, &s.store, &s.locales, false)?;
            emit(&s, "check_normalized", &report, report.outcome(), || {
                report.to_check_text()
            })
        }
        Command::Normalize {
            common,
            pattern_router,
            write,
            dry_run,
            allow_delete,
        } => {
            let s = Session::open(common)?;
            normalize_command(&s, *pattern_router, *write, *dry_run, *allow_delete)
        }
        Command::Health { common } => health(common),
        Command::Find { common } => {
            let s = Session::open(common)?;
            let used = s.scan()?;
            find_output(&s, &used)
        }
        Command::InitConfig {
            to,
            write,
            force,
            root,
        } => init_config(to.as_deref(), *write, *force, root.as_deref()),
        Command::MigrateConfig {
            from,
            to,
            write,
            force,
            root,
        } => migrate_config(
            from.as_deref(),
            to.as_deref(),
            *write,
            *force,
            root.as_deref(),
        ),
    }
}

/// The gem's answer here is `cp $(bundle exec i18n-tasks gem-path)/templates/...`,
/// which is the same file for every project. This one is generated from the
/// project. Writing is opt-in all the same (blocker B8).
fn init_config(
    to: Option<&Path>,
    write: bool,
    force: bool,
    root: Option<&Path>,
) -> Result<u8, String> {
    let root = root.unwrap_or(Path::new("."));
    let to = match to {
        Some(p) => p.to_path_buf(),
        None => root.join(init::INIT_TARGET),
    };
    let generated = init::generate(root, &to)?;

    if write {
        write_config(&to, &generated.output, force)?;
    } else {
        print!("{}", generated.output);
    }
    eprint!("{}", init::to_text(&generated, &to, write));
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
fn migrate_config(
    from: Option<&Path>,
    to: Option<&Path>,
    write: bool,
    force: bool,
    root: Option<&Path>,
) -> Result<u8, String> {
    let root = root.unwrap_or(Path::new("."));
    let from = match from {
        Some(p) => p.to_path_buf(),
        None => migrate::find_gem_config(root).ok_or_else(|| {
            format!(
                "no gem config found. Looked for {} under {}. Name one with `--from`.",
                migrate::GEM_CONFIG_CANDIDATES.join(" and "),
                root.display()
            )
        })?,
    };
    let to = match to {
        Some(p) => p.to_path_buf(),
        None => root.join(migrate::MIGRATION_TARGET),
    };
    if from == to {
        return Err("`--from` and `--to` are the same file".into());
    }
    let src = std::fs::read_to_string(&from)
        .map_err(|e| format!("cannot read {}: {e}", from.display()))?;
    let migration = migrate::migrate(&src, &from, &to)?;

    if write {
        write_config(&to, &migration.output, force)?;
    } else {
        print!("{}", migration.output);
    }
    eprint!("{}", migrate::to_text(&migration, &from, &to, write));
    Ok(if migration.needs_attention() {
        EXIT_FOUND
    } else {
        EXIT_OK
    })
}

fn parse_types(types: Option<&[String]>) -> Result<Vec<MissingType>, String> {
    match types {
        None => Ok(MissingType::ALL.to_vec()),
        Some(names) => names.iter().map(|n| MissingType::parse(n.trim())).collect(),
    }
}

fn emit<T: Serialize>(
    session: &Session,
    name: &str,
    report: &T,
    outcome: Outcome,
    text: impl Fn() -> String,
) -> Result<u8, String> {
    if session.json {
        #[derive(Serialize)]
        struct Envelope<'a, T> {
            check: &'a str,
            passed: bool,
            config_digest: &'a str,
            locales: &'a [String],
            #[serde(flatten)]
            report: &'a T,
        }
        let env = Envelope {
            check: name,
            passed: outcome == Outcome::Clean,
            config_digest: &session.cfg.digest,
            locales: &session.locales,
            report,
        };
        println!(
            "{}",
            serde_json::to_string_pretty(&env).map_err(|e| e.to_string())?
        );
    } else {
        print!("{}", text());
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
    let missing_report = missing::report(&s.cfg, &s.store, &used, &s.locales, &MissingType::ALL);
    let unused_report = unused::report(&s.cfg, &s.store, &used, &s.locales);
    let consistent = interpolations::inconsistent(&s.cfg, &s.store, &s.locales);
    let reserved = interpolations::reserved(&s.store, &s.locales);
    // `health` never writes. This step only compares the emitted bytes against
    // the file on disk.
    let normalized = normalize::plan(&s.cfg, &s.store, &s.locales, false)?;

    let found = [
        missing_report.outcome(),
        unused_report.outcome(),
        consistent.outcome(),
        reserved.outcome(),
        normalized.outcome(),
    ]
    .contains(&Outcome::Found);

    if s.json {
        #[derive(Serialize)]
        struct Health<'a> {
            check: &'static str,
            passed: bool,
            config_digest: &'a str,
            locales: &'a [String],
            stats: &'a ForestStats,
            missing: &'a missing::MissingReport,
            unused: &'a unused::UnusedReport,
            check_consistent_interpolations: &'a interpolations::InterpolationReport,
            check_reserved_interpolations: &'a interpolations::InterpolationReport,
            check_normalized: &'a normalize::NormalizeReport,
        }
        let out = Health {
            check: "health",
            passed: !found,
            config_digest: &s.cfg.digest,
            locales: &s.locales,
            stats: &stats,
            missing: &missing_report,
            unused: &unused_report,
            check_consistent_interpolations: &consistent,
            check_reserved_interpolations: &reserved,
            check_normalized: &normalized,
        };
        println!(
            "{}",
            serde_json::to_string_pretty(&out).map_err(|e| e.to_string())?
        );
    } else {
        println!("{}", stats.to_text());
        println!();
        print!("{}", missing_report.to_text());
        println!();
        print!("{}", unused_report.to_text());
        println!();
        print!("{}", consistent.to_text());
        println!();
        print!("{}", reserved.to_text());
        println!();
        print!("{}", normalized.to_check_text());
    }
    Ok(if found { EXIT_FOUND } else { EXIT_OK })
}

/// ref: blocker B8. `--write` is required, `--dry-run` prints the diff, and a
/// deletion always needs `--allow-delete` on top of `--write`.
fn normalize_command(
    s: &Session,
    pattern_router: bool,
    write: bool,
    dry_run: bool,
    allow_delete: bool,
) -> Result<u8, String> {
    if write && dry_run {
        return Err("`--write` and `--dry-run` contradict each other".into());
    }
    let report = normalize::plan(&s.cfg, &s.store, &s.locales, pattern_router)?;
    let deletions = report.deletions();
    // Always print the deletion list, whether or not the run may act on it.
    if !deletions.is_empty() {
        eprintln!("{} file(s) end up with no keys:", deletions.len());
        for d in &deletions {
            eprintln!("  {}", d.display);
        }
    }
    if s.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "check": "normalize",
                "written": write,
                "config_digest": s.cfg.digest,
                "locales": s.locales,
                "changes": report.changes,
                "files_routed": report.files_routed,
            }))
            .map_err(|e| e.to_string())?
        );
    } else {
        print!("{}", report.to_normalize_text(dry_run));
    }
    if !write {
        if !s.json {
            println!("Nothing was written. Pass `--write` to apply, `--dry-run` to see the diff.");
        }
        return Ok(EXIT_OK);
    }
    if !deletions.is_empty() && !allow_delete {
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
        println!(
            "{}",
            serde_json::to_string_pretty(&out).map_err(|e| e.to_string())?
        );
    } else {
        for (key, occs) in &used.keys {
            println!("{key}");
            for o in occs {
                println!("  {}:{}:{}", o.path.display(), o.line_num, o.line_pos);
            }
        }
        for (pattern, occ) in &used.pattern_sources {
            println!("{pattern}  (pattern)");
            println!("  {}:{}", occ.path.display(), occ.line_num);
        }
        for o in &used.opaque {
            println!("(opaque)  {}:{}", o.path.display(), o.line_num);
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
