//! Exit codes, which must match the gem.
//!
//! 0 means the check passed, 1 means the check found something, 2 means the
//! tool itself failed. The gem signals the middle case with an internal
//! `:exit1`.

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_i18n-tasks-rs");

struct Project {
    root: PathBuf,
}

impl Project {
    fn new(name: &str, locale_yml: &str, source: Option<&str>) -> Project {
        let root = std::env::temp_dir().join(format!("i18n-tasks-rs-exit-{name}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("config/locales")).unwrap();
        std::fs::create_dir_all(root.join("app/controllers")).unwrap();
        std::fs::write(root.join("config/locales/en.yml"), locale_yml).unwrap();
        std::fs::write(
            root.join("config/i18n-tasks.yml"),
            "base_locale: en\nlocales: [en]\ndata:\n  read:\n    - config/locales/%{locale}.yml\nsearch:\n  paths: [app/]\n",
        )
        .unwrap();
        if let Some(src) = source {
            std::fs::write(root.join("app/controllers/a_controller.rb"), src).unwrap();
        }
        Project { root }
    }

    fn run(&self, args: &[&str]) -> (i32, String) {
        let out = Command::new(BIN)
            .args(args)
            .arg("-c")
            .arg(self.root.join("config/i18n-tasks.yml"))
            .arg("--root")
            .arg(&self.root)
            .output()
            .expect("binary runs");
        let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        (out.status.code().unwrap_or(-1), text)
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn a_clean_check_exits_zero() {
    // Already in the emitter's own form, so `check-normalized` passes too.
    let p = Project::new("clean", "---\nen:\n  a: A\n", Some("t('a')\n"));
    assert_eq!(p.run(&["unused"]).0, 0);
    assert_eq!(p.run(&["missing"]).0, 0);
    assert_eq!(p.run(&["check-consistent-interpolations"]).0, 0);
    assert_eq!(p.run(&["check-reserved-interpolations"]).0, 0);
    assert_eq!(p.run(&["health"]).0, 0);
    assert_eq!(p.run(&["find"]).0, 0);
}

#[test]
fn a_check_that_found_something_exits_one() {
    let p = Project::new(
        "dirty",
        "en:\n  a: A\n  b: B\n",
        Some("t('a')\nt('nope')\n"),
    );
    assert_eq!(p.run(&["unused"]).0, 1);
    assert_eq!(p.run(&["missing"]).0, 1);
    assert_eq!(p.run(&["health"]).0, 1);
    // `find` only reports, so it never fails a build.
    assert_eq!(p.run(&["find"]).0, 0);
}

#[test]
fn a_reserved_interpolation_exits_one() {
    let p = Project::new("reserved", "en:\n  a: \"%{scope} x\"\n", Some("t('a')\n"));
    assert_eq!(p.run(&["check-reserved-interpolations"]).0, 1);
}

#[test]
fn a_tool_failure_exits_two() {
    let p = Project::new("broken", "en:\n  a: &x A\n  b: *x\n", Some("t('a')\n"));
    let (code, text) = p.run(&["unused"]);
    assert_eq!(code, 2);
    assert!(text.contains("anchors"), "{text}");

    let p2 = Project::new("badlocale", "en:\n  a: A\n", Some("t('a')\n"));
    let (code, text) = p2.run(&["unused", "zz"]);
    assert_eq!(code, 2);
    assert!(text.contains("unknown locale"), "{text}");

    // `normalize` cannot write two locales into one file.
    let p3 = Project::new("twolocales", "---\nen:\n  a: A\nde:\n  a: A\n", None);
    let (code, text) = p3.run(&["check-normalized"]);
    assert_eq!(code, 2);
    assert!(text.contains("holds the locale(s) de"), "{text}");
}

#[test]
fn health_runs_all_five_checks() {
    let p = Project::new("healthtext", "---\nen:\n  a: A\n", Some("t('a')\n"));
    let (code, text) = p.run(&["health"]);
    assert_eq!(code, 0);
    assert!(text.contains("All data is normalized"), "{text}");
    // The statistics header comes first.
    assert!(text.starts_with("1 keys in 1 locales (en)"), "{text}");
}

#[test]
fn json_output_carries_the_config_digest() {
    let p = Project::new("json", "en:\n  a: A\n  b: B\n", Some("t('a')\n"));
    let (code, text) = p.run(&["unused", "-f", "json"]);
    assert_eq!(code, 1);
    assert!(text.contains("\"config_digest\""), "{text}");
    assert!(text.contains("\"passed\": false"), "{text}");
    assert!(text.contains("\"key\": \"b\""), "{text}");
}

#[test]
fn an_unknown_missing_type_is_a_tool_failure() {
    let p = Project::new("types", "en:\n  a: A\n", Some("t('a')\n"));
    let (code, text) = p.run(&["missing", "--types", "bogus"]);
    assert_eq!(code, 2);
    assert!(text.contains("unknown missing type"), "{text}");
}

/// A trailing locale list restricts every command to those locales.
#[test]
fn an_explicit_locale_list_restricts_the_run() {
    let root = std::env::temp_dir().join("i18n-tasks-rs-exit-locales");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("config/locales")).unwrap();
    std::fs::create_dir_all(root.join("app")).unwrap();
    std::fs::write(root.join("app/a.rb"), "t('a')\n").unwrap();
    std::fs::write(root.join("config/locales/en.yml"), "en:\n  a: A\n").unwrap();
    std::fs::write(
        root.join("config/locales/es.yml"),
        "es:\n  a: A\n  extra: E\n",
    )
    .unwrap();
    std::fs::write(
        root.join("config/i18n-tasks.yml"),
        "base_locale: en\nlocales: [en, es]\ndata:\n  read:\n    - config/locales/%{locale}.yml\nsearch:\n  paths: [app/]\n",
    )
    .unwrap();
    let run = |args: &[&str]| {
        let out = std::process::Command::new(BIN)
            .args(args)
            .arg("-c")
            .arg(root.join("config/i18n-tasks.yml"))
            .arg("--root")
            .arg(&root)
            .arg("-f")
            .arg("json")
            .output()
            .expect("binary runs");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
        )
    };
    // Both locales: `es.extra` is unused and `en.extra` is missing.
    let (_, both) = run(&["unused"]);
    assert!(
        both.contains("\"locales\": [\n    \"en\",\n    \"es\"\n  ]"),
        "{both}"
    );
    // Only `en`: `es.extra` is out of scope, so `unused` is clean.
    let (code, only_en) = run(&["unused", "en"]);
    assert_eq!(code, 0, "{only_en}");
    assert!(
        only_en.contains("\"locales\": [\n    \"en\"\n  ]"),
        "{only_en}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// `-l/--locales` on every check and on `health`.
///
/// ref: lib/i18n/tasks/command/option_parsers/locale.rb#ListParser. `base`
/// resolves to the base locale, a bare `all` means every configured locale,
/// the flag and the trailing positionals concatenate, and the base locale is
/// swapped to the front whenever it appears later in the list.
#[test]
fn the_locales_flag_matches_the_gems_list_parser() {
    let root = std::env::temp_dir().join("i18n-tasks-rs-exit-locales-flag");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("config/locales")).unwrap();
    std::fs::create_dir_all(root.join("app")).unwrap();
    std::fs::write(root.join("app/a.rb"), "t('a')\n").unwrap();
    for locale in ["en", "es", "ru"] {
        std::fs::write(
            root.join(format!("config/locales/{locale}.yml")),
            format!("{locale}:\n  a: A\n"),
        )
        .unwrap();
    }
    // Base is `es`, so the default order is base-first: es, en, ru.
    std::fs::write(
        root.join("config/i18n-tasks.yml"),
        "base_locale: es\nlocales: [en, es, ru]\ndata:\n  read:\n    - config/locales/%{locale}.yml\nsearch:\n  paths: [app/]\n",
    )
    .unwrap();
    let run = |args: &[&str]| {
        let out = std::process::Command::new(BIN)
            .args(args)
            .arg("-c")
            .arg(root.join("config/i18n-tasks.yml"))
            .arg("--root")
            .arg(&root)
            .arg("-f")
            .arg("json")
            .output()
            .expect("binary runs");
        let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        (out.status.code().unwrap_or(-1), text)
    };
    // The JSON `locales` array, flattened onto one line.
    let locales_of = |text: &str| -> String {
        let start = text.find("\"locales\": [").expect("a locales key");
        let rest = &text[start..];
        let end = rest.find(']').expect("a closing bracket") + 1;
        rest[..end]
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == ',')
            .collect::<String>()
            .trim_start_matches("locales")
            .trim_start_matches(',')
            .to_string()
    };

    // Every check and `health` reads the same flag.
    for cmd in [
        "missing",
        "unused",
        "check-consistent-interpolations",
        "check-reserved-interpolations",
        "check-normalized",
        "normalize",
        "health",
    ] {
        let (_, text) = run(&[cmd, "-l", "en,ru"]);
        assert_eq!(locales_of(&text), "en,ru", "{cmd}: {text}");
    }

    // No flag, and an explicit `all`, both mean every configured locale.
    let (_, text) = run(&["missing"]);
    assert_eq!(locales_of(&text), "es,en,ru", "{text}");
    let (_, text) = run(&["missing", "-l", "all"]);
    assert_eq!(locales_of(&text), "es,en,ru", "{text}");

    // `base` stands in for the base locale.
    let (_, text) = run(&["missing", "-l", "base"]);
    assert_eq!(locales_of(&text), "es", "{text}");
    let (_, text) = run(&["missing", "-l", "base,en"]);
    assert_eq!(locales_of(&text), "es,en", "{text}");

    // move_base_locale_to_front! is a swap, not a rotation: `es` at index 2
    // trades places with `en` at index 0.
    let (_, text) = run(&["missing", "-l", "en,ru,es"]);
    assert_eq!(locales_of(&text), "es,ru,en", "{text}");

    // Repeating the flag, and mixing it with the trailing positionals, both
    // concatenate. ref: cli.rb#parse_option, `consume_positional: true`.
    let (_, text) = run(&["missing", "-l", "en", "-l", "ru"]);
    assert_eq!(locales_of(&text), "en,ru", "{text}");
    let (_, text) = run(&["missing", "-l", "ru", "en"]);
    assert_eq!(locales_of(&text), "ru,en", "{text}");

    // A trailing comma is the gem's `String#split` behaviour, not an error.
    let (_, text) = run(&["missing", "-l", "en,"]);
    assert_eq!(locales_of(&text), "en", "{text}");

    // A locale that is not configured, and one that is not a locale at all,
    // are both tool failures rather than an empty report.
    let (code, text) = run(&["missing", "-l", "fr"]);
    assert_eq!(code, 2, "{text}");
    assert!(text.contains("unknown locale `fr`"), "{text}");
    let (code, text) = run(&["missing", "-l", "e!n"]);
    assert_eq!(code, 2, "{text}");
    assert!(text.contains("invalid locale `e!n`"), "{text}");

    let _ = std::fs::remove_dir_all(&root);
}

/// A key whose *name* is nil is dropped with a warning, and the warning reaches
/// the terminal rather than only the report structure.
/// ref: file_system_base.rb#filter_nil_keys!
#[test]
fn a_nil_key_warns_on_stderr() {
    let p = Project::new("nilkey", "en:\n  a: A\n  ~: dropped\n", Some("t('a')\n"));
    let (code, text) = p.run(&["unused"]);
    // The nil key never enters the tree, so nothing is unused.
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("warning:"), "{text}");
    assert!(text.contains("nil key"), "{text}");
}

/// `health` on an empty data set is a tool failure, not a silent pass.
#[test]
fn health_refuses_an_empty_data_set_from_the_cli() {
    let p = Project::new("healthempty", "en: {}\n", Some("t('a')\n"));
    let (code, text) = p.run(&["health"]);
    assert_eq!(code, 2);
    assert!(text.contains("no keys detected"), "{text}");
}

/// Every check supports `--format json`, including the four `health` runs.
#[test]
fn every_check_emits_json() {
    let p = Project::new(
        "jsonall",
        "---\nen:\n  a: \"%{scope}\"\n  b: B\n",
        Some("t('a')\n"),
    );
    for (args, check) in [
        (vec!["missing", "-f", "json"], "missing"),
        (vec!["unused", "-f", "json"], "unused"),
        (
            vec!["check-consistent-interpolations", "-f", "json"],
            "check_consistent_interpolations",
        ),
        (
            vec!["check-reserved-interpolations", "-f", "json"],
            "check_reserved_interpolations",
        ),
        (vec!["check-normalized", "-f", "json"], "check_normalized"),
        (vec!["health", "-f", "json"], "health"),
        (vec!["find", "-f", "json"], "find"),
    ] {
        let (_, text) = p.run(&args);
        assert!(
            text.contains(&format!("\"check\": \"{check}\"")),
            "{check}: {text}"
        );
    }
}

/// `find` in text form prints keys, derived patterns and opaque calls.
#[test]
fn find_prints_keys_patterns_and_opaque_calls() {
    let p = Project::new(
        "findtext",
        "en:\n  a: A\n",
        Some("t('a')\nt(\"pre.#{x}\")\nt(some_var)\n"),
    );
    let (code, text) = p.run(&["find"]);
    assert_eq!(code, 0);
    assert!(
        text.contains("a\n  app/controllers/a_controller.rb:1:0"),
        "{text}"
    );
    assert!(text.contains("pre.*:  (pattern)"), "{text}");
    assert!(text.contains("(opaque)"), "{text}");
}

/// The opaque-call note is part of the `unused` text report.
#[test]
fn the_unused_report_explains_an_opaque_call() {
    let p = Project::new("opaquetext", "en:\n  a: A\n", Some("t(some_var)\n"));
    let (code, text) = p.run(&["unused"]);
    assert_eq!(code, 1);
    assert!(text.contains("cannot be determined statically"), "{text}");
    assert!(text.contains("t(some_var)"), "{text}");
}

/// A config file that does not exist is a tool failure, and the message points
/// at the gem config next door when there is one.
#[test]
fn a_missing_config_is_a_tool_failure() {
    let root = std::env::temp_dir().join("i18n-tasks-rs-exit-noconfig");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let out = std::process::Command::new(BIN)
        .args(["unused", "-c"])
        .arg(root.join("nope.yml"))
        .arg("--root")
        .arg(&root)
        .output()
        .expect("binary runs");
    assert_eq!(out.status.code(), Some(2));
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(text.contains("cannot read config"), "{text}");
    let _ = std::fs::remove_dir_all(&root);
}

/// Without `--root` every config path is resolved against the working
/// directory, which is what the gem does.
#[test]
fn the_working_directory_is_the_default_root() {
    let root = std::env::temp_dir().join("i18n-tasks-rs-exit-cwd");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("config/locales")).unwrap();
    std::fs::create_dir_all(root.join("app")).unwrap();
    std::fs::write(root.join("app/a.rb"), "t('a')\n").unwrap();
    std::fs::write(root.join("config/locales/en.yml"), "---\nen:\n  a: A\n").unwrap();
    std::fs::write(
        root.join("config/i18n-tasks-rs.yml"),
        "base_locale: en\nlocales: [en]\ndata:\n  read:\n    - config/locales/%{locale}.yml\nsearch:\n  paths: [app/]\n",
    )
    .unwrap();
    // No `-c` and no `--root`: both defaults come from the working directory.
    let out = std::process::Command::new(BIN)
        .arg("health")
        .current_dir(&root)
        .output()
        .expect("binary runs");
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{text}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(text.starts_with("1 keys in 1 locales (en)"), "{text}");
    let _ = std::fs::remove_dir_all(&root);
}
