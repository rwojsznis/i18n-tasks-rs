//! `init-config` against the three locale layouts a real project uses.
//!
//! The property that matters is coverage: every locale file the project has
//! must be matched by a `data.read` pattern the command emitted, judged by the
//! loader's own rules. A generated config that reads none of the data is worse
//! than no config at all, because the reports look clean.

use i18n_tasks_rs::config::Config;
use i18n_tasks_rs::init;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_i18n-tasks-rs");
const TARGET: &str = "config/i18n-tasks-rs.yml";

fn generate(fixture: &str) -> init::Generated {
    init::generate(Path::new(fixture), Path::new(TARGET)).expect("init succeeds")
}

fn parse(g: &init::Generated, fixture: &str) -> Config {
    Config::parse(&g.output, Path::new(TARGET), PathBuf::from(fixture)).expect("output loads")
}

/// The ordinary Rails layout, and the one case where every detection runs:
/// a `default_locale` in `config/application.rb`, three locale files, and a
/// relative key in a directory the gem's defaults do not list.
#[test]
fn the_flat_layout_is_detected_whole() {
    let fixture = "tests/fixtures/init/flat";
    let g = generate(fixture);
    let cfg = parse(&g, fixture);

    assert_eq!(cfg.data.read, ["config/locales/%{locale}.yml"]);
    assert_eq!(cfg.data.write.len(), 1);
    assert_eq!(cfg.data.write[0].path, "config/locales/%{locale}.yml");

    // `config.i18n.default_locale = :de`, and not the commented-out `:en`
    // three lines above it.
    assert_eq!(cfg.base_locale, "de");
    assert_eq!(
        g.detected.base_locale_from.as_deref(),
        Some("config/application.rb:8")
    );
    assert_eq!(g.detected.locales, ["de", "en", "fr"]);

    // Only the search paths that exist.
    assert_eq!(cfg.search.paths, ["app/", "lib/"]);
    // A build directory under a search path is excluded up front.
    assert_eq!(cfg.search.exclude, ["app/assets/builds"]);

    // `app/controllers` and `app/views` are gem defaults that exist here.
    // `app/components` is not a default: it earned its place by holding a
    // relative key. `app/helpers` and the rest are defaults that do not exist.
    // `lib/tasks` has a `t` call, but an absolute one.
    assert_eq!(
        cfg.search.relative_roots,
        ["app/components", "app/controllers", "app/views"]
    );
    assert!(
        g.detected
            .relative_roots_detected
            .contains(&"app/components".to_string())
    );

    // The generated config was read back and the data loaded before the
    // command offered to write anything.
    assert_eq!(g.verified.locales, ["de", "en", "fr"]);
    assert!(g.verified.key_count > 0, "{:?}", g.verified);
    assert_eq!(g.verified.error, None);
    assert!(!g.needs_attention(), "{:?}", g.detected.notes);
}

/// One file per top-level key: `devise.en.yml`. The write target cannot be
/// `config/locales/%{locale}.yml` here — nothing would read it back.
#[test]
fn the_namespaced_layout_gets_a_write_target_its_own_patterns_read() {
    let fixture = "tests/fixtures/init/namespaced";
    let g = generate(fixture);
    let cfg = parse(&g, fixture);

    assert_eq!(cfg.data.read, ["config/locales/*.%{locale}.yml"]);
    assert_eq!(
        cfg.data.write[0].path,
        "config/locales/common.%{locale}.yml"
    );
    assert_eq!(g.detected.locales, ["en", "fr"]);
    // No Ruby names a default locale, and `en` is among the locales found.
    assert_eq!(cfg.base_locale, "en");
    assert_eq!(g.detected.base_locale_from, None);
    assert!(!g.needs_attention(), "{:?}", g.detected.notes);
}

/// A directory per locale. Two patterns are needed, not one: the loader's
/// `**` crosses at least one directory, so `%{locale}/**/*.yml` alone never
/// matches `en/models.yml`.
#[test]
fn a_directory_per_locale_needs_both_depths() {
    let fixture = "tests/fixtures/init/locale_dirs";
    let g = generate(fixture);
    let cfg = parse(&g, fixture);

    assert_eq!(
        cfg.data.read,
        [
            "config/locales/%{locale}/*.yml",
            "config/locales/%{locale}/**/*.yml"
        ]
    );
    assert_eq!(
        cfg.data.write[0].path,
        "config/locales/%{locale}/common.yml"
    );
    assert_eq!(g.detected.locales, ["en", "fr"]);
    assert_eq!(g.verified.key_count, 3);
}

/// The one property worth asserting for every layout.
#[test]
fn every_locale_file_is_read_by_the_generated_config() {
    for fixture in [
        "tests/fixtures/init/flat",
        "tests/fixtures/init/namespaced",
        "tests/fixtures/init/locale_dirs",
    ] {
        let g = generate(fixture);
        assert!(
            g.detected.unmatched.is_empty(),
            "{fixture}: {:?}",
            g.detected.unmatched
        );
        assert!(g.detected.files_seen > 0, "{fixture}");
    }
}

struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    fn new(name: &str) -> Sandbox {
        let root = std::env::temp_dir().join(format!("i18n-tasks-rs-init-{name}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("config/locales")).unwrap();
        std::fs::create_dir_all(root.join("app")).unwrap();
        std::fs::write(root.join("config/locales/en.yml"), "---\nen:\n  a: A\n").unwrap();
        std::fs::write(root.join("app/a.rb"), "t('a')\n").unwrap();
        Sandbox { root }
    }

    fn run(&self, args: &[&str]) -> (i32, String) {
        let out = Command::new(BIN)
            .arg("init-config")
            .args(args)
            .arg("--root")
            .arg(&self.root)
            .output()
            .expect("binary runs");
        let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        (out.status.code().unwrap_or(-1), text)
    }

    fn target(&self) -> PathBuf {
        self.root.join(TARGET)
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Blocker B8: writing is opt-in here as everywhere else.
#[test]
fn the_command_writes_only_when_asked() {
    let s = Sandbox::new("write");

    let (code, out) = s.run(&[]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("base_locale: en"), "{out}");
    assert!(out.contains("nothing written"), "{out}");
    assert!(!s.target().exists());

    let (code, out) = s.run(&["--write"]);
    assert_eq!(code, 0, "{out}");
    assert!(
        std::fs::read_to_string(s.target())
            .unwrap()
            .contains("base_locale: en")
    );

    // A second run does not quietly replace a file someone may have edited.
    let (code, out) = s.run(&["--write"]);
    assert_eq!(code, 2);
    assert!(out.contains("--force"), "{out}");
    let (code, out) = s.run(&["--write", "--force"]);
    assert_eq!(code, 0, "{out}");
}

/// `-o` names the destination, `--root` names the project it is generated from.
/// The two are independent paths, and both are optional, so this pins which one
/// is which: the destination is used verbatim, and the default target under the
/// root stays untouched.
#[test]
fn the_destination_is_independent_of_the_project_root() {
    let s = Sandbox::new("dest");
    let dest = s.root.join("elsewhere/custom.yml");
    let (code, out) = s.run(&["--write", "-o", dest.to_str().unwrap()]);
    assert_eq!(code, 0, "{out}");
    // The project under `--root` is what was read: it holds `en.yml` only.
    assert!(
        std::fs::read_to_string(&dest)
            .unwrap()
            .contains("base_locale: en"),
        "{out}"
    );
    assert!(!s.target().exists(), "the default target was written too");

    // `--force` is about the destination, not the default target.
    let (code, out) = s.run(&["--write", "-o", dest.to_str().unwrap()]);
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("--force"), "{out}");
}

/// The generated config is the one the tool then reads, so the whole point is
/// that the commands work straight afterwards.
#[test]
fn the_generated_config_runs_the_checks() {
    let s = Sandbox::new("runs");
    let (code, out) = s.run(&["--write"]);
    assert_eq!(code, 0, "{out}");
    let health = Command::new(BIN)
        .args(["health", "--root"])
        .arg(&s.root)
        .arg("-c")
        .arg(s.target())
        .output()
        .expect("binary runs");
    let text = String::from_utf8_lossy(&health.stdout).into_owned()
        + &String::from_utf8_lossy(&health.stderr);
    assert_eq!(health.status.code(), Some(0), "{text}");
    assert!(text.contains("1 keys"), "{text}");
}

/// A project that still has a gem config wants `migrate-config`: generating a
/// fresh config there silently loses every `ignore_unused` entry.
#[test]
fn a_gem_config_in_the_project_is_reported() {
    let s = Sandbox::new("gem");
    std::fs::write(
        s.root.join("config/i18n-tasks.yml"),
        "base_locale: en\nignore_unused:\n  - 'devise.*'\n",
    )
    .unwrap();
    let (code, out) = s.run(&["--write"]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("migrate-config"), "{out}");
    // Written all the same: the note is in the file, and the choice is the
    // reader's.
    assert!(
        std::fs::read_to_string(s.target())
            .unwrap()
            .contains("migrate-config")
    );
}

/// Nothing to detect. The command still produces a usable starting point, and
/// says the data was not found rather than pretending.
#[test]
fn an_empty_project_is_told_so() {
    let root = std::env::temp_dir().join("i18n-tasks-rs-init-empty");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let out = Command::new(BIN)
        .args(["init-config", "--root"])
        .arg(&root)
        .output()
        .expect("binary runs");
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{text}");
    assert!(text.contains("no locale files"), "{text}");
    // The gem's own defaults, so the file is a starting point and not empty.
    assert!(text.contains("config/locales/%{locale}.yml"), "{text}");
    let _ = std::fs::remove_dir_all(&root);
}

/// The error a fresh project hits first is the missing config. It has to name
/// the command that makes one.
#[test]
fn the_missing_config_error_points_at_init() {
    let root = std::env::temp_dir().join("i18n-tasks-rs-init-pointer");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let out = Command::new(BIN)
        .args(["unused", "--root"])
        .arg(&root)
        .output()
        .expect("binary runs");
    assert_eq!(out.status.code(), Some(2));
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(text.contains("init-config"), "{text}");
    let _ = std::fs::remove_dir_all(&root);
}
