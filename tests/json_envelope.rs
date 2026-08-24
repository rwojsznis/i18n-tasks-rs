//! The shape of `-f json`: which top-level fields each check emits, and in
//! which order.
//!
//! `find -f json` exists so this tool and the gem can be compared over the same
//! project, so the JSON is an interface, not a debug dump. These tests are
//! characterization tests — they record what the binary emits today, so that a
//! refactor of the envelope has to be deliberate. They deliberately assert the
//! *order* as well as the set, because `health` composes five reports and a
//! reordering there is a visible change to every consumer.

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_i18n-tasks-rs");

struct Project {
    root: PathBuf,
}

impl Project {
    /// A project with something wrong for every check: a used key with no
    /// translation, a translation nothing uses, an interpolation that differs
    /// between locales, a reserved variable name, and a file that is not in the
    /// emitter's form.
    fn new(name: &str) -> Project {
        let root = std::env::temp_dir().join(format!("i18n-tasks-rs-envelope-{name}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("config/locales")).unwrap();
        std::fs::create_dir_all(root.join("app/controllers")).unwrap();
        std::fs::write(
            root.join("config/locales/en.yml"),
            "en:\n  a: A\n  unusedkey: U\n  inter: \"%{count} of %{total}\"\n  reserved_one: \"%{scope} here\"\n",
        )
        .unwrap();
        std::fs::write(
            root.join("config/locales/de.yml"),
            "de:\n  a: \"A de\"\n  inter: \"%{count}\"\n",
        )
        .unwrap();
        std::fs::write(
            root.join("config/i18n-tasks.yml"),
            "base_locale: en\nlocales: [en, de]\ndata:\n  read:\n    - config/locales/%{locale}.yml\nsearch:\n  paths: [app/]\n",
        )
        .unwrap();
        std::fs::write(
            root.join("app/controllers/a_controller.rb"),
            "t('a')\nt('missing.one')\nt(some_var)\n",
        )
        .unwrap();
        Project { root }
    }

    fn run(&self, args: &[&str]) -> String {
        let out = Command::new(BIN)
            .args(args)
            .arg("-c")
            .arg(self.root.join("config/i18n-tasks.yml"))
            .arg("--root")
            .arg(&self.root)
            .output()
            .expect("binary runs");
        String::from_utf8(out.stdout).expect("stdout is UTF-8")
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The top-level field names of a pretty-printed JSON object, in order. Every
/// top-level key sits at exactly two spaces of indentation.
fn top_level_keys(json: &str) -> Vec<String> {
    json.lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("  \"")?;
            let (name, _) = rest.split_once("\":")?;
            (!name.contains('"')).then(|| name.to_string())
        })
        .collect()
}

#[test]
fn every_check_emits_the_same_envelope_fields_first() {
    let p = Project::new("envelope");
    for (args, expected) in [
        (
            vec!["missing", "-f", "json"],
            vec!["check", "passed", "config_digest", "locales", "rows"],
        ),
        (
            vec!["unused", "-f", "json"],
            vec![
                "check",
                "passed",
                "config_digest",
                "locales",
                "rows",
                "opaque",
            ],
        ),
        (
            vec!["check-consistent-interpolations", "-f", "json"],
            vec![
                "check",
                "passed",
                "config_digest",
                "locales",
                "rows",
                "title",
            ],
        ),
        (
            vec!["check-reserved-interpolations", "-f", "json"],
            vec![
                "check",
                "passed",
                "config_digest",
                "locales",
                "rows",
                "title",
            ],
        ),
        (
            vec!["check-normalized", "-f", "json"],
            vec![
                "check",
                "passed",
                "config_digest",
                "locales",
                "changes",
                "files_routed",
            ],
        ),
    ] {
        let out = p.run(&args);
        assert_eq!(top_level_keys(&out), expected, "{args:?} emitted: {out}");
    }
}

/// `health` runs the five checks and nests each report under its own check
/// name, after the shared envelope and the statistics header. The order is the
/// order the text report prints them in.
#[test]
fn health_nests_the_five_reports_in_report_order() {
    let p = Project::new("health");
    let out = p.run(&["health", "-f", "json"]);
    assert_eq!(
        top_level_keys(&out),
        [
            "check",
            "passed",
            "config_digest",
            "locales",
            "stats",
            "missing",
            "unused",
            "check_consistent_interpolations",
            "check_reserved_interpolations",
            "check_normalized",
        ],
        "{out}"
    );
    assert!(out.contains("\"check\": \"health\""), "{out}");
    // The nested reports carry no `check` or `passed` of their own: the five
    // names are the field names, and `passed` is the whole run's.
    assert_eq!(out.matches("\"passed\"").count(), 1, "{out}");
}

/// `health` on a clean project passes, and still emits all five reports.
#[test]
fn a_clean_health_run_still_emits_every_report() {
    let root = std::env::temp_dir().join("i18n-tasks-rs-envelope-clean");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("config/locales")).unwrap();
    std::fs::create_dir_all(root.join("app/controllers")).unwrap();
    std::fs::write(root.join("config/locales/en.yml"), "---\nen:\n  a: A\n").unwrap();
    std::fs::write(
        root.join("config/i18n-tasks.yml"),
        "base_locale: en\nlocales: [en]\ndata:\n  read:\n    - config/locales/%{locale}.yml\nsearch:\n  paths: [app/]\n",
    )
    .unwrap();
    std::fs::write(root.join("app/controllers/a_controller.rb"), "t('a')\n").unwrap();
    let p = Project { root };
    let out = p.run(&["health", "-f", "json"]);
    assert!(out.contains("\"passed\": true"), "{out}");
    for check in [
        "missing",
        "unused",
        "check_consistent_interpolations",
        "check_reserved_interpolations",
        "check_normalized",
    ] {
        assert!(out.contains(&format!("\"{check}\":")), "{check}: {out}");
    }
}
