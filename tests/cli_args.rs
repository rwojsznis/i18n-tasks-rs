//! Where the shared flags go on the command line.
//!
//! ref: lib/i18n/tasks/cli.rb. The gem's flags belong to the task, as in
//! `i18n-tasks missing -c config/i18n-tasks.yml`, so `-c`, `-f` and `--root`
//! are per-subcommand here too. They are *not* global flags of the binary:
//! `i18n-tasks-rs -c … missing` is an error, and this test keeps it one, so
//! that lifting the flags up to the top level stays a deliberate change with a
//! test to update rather than a silent one.

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_i18n-tasks-rs");

struct Project {
    root: PathBuf,
}

impl Project {
    fn new(name: &str) -> Project {
        let root = std::env::temp_dir().join(format!("i18n-tasks-rs-args-{name}"));
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
        Project { root }
    }

    fn config(&self) -> PathBuf {
        self.root.join("config/i18n-tasks.yml")
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn run(args: &[&str]) -> (i32, String) {
    let out = Command::new(BIN).args(args).output().expect("binary runs");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.code().unwrap_or(-1), text)
}

#[test]
fn shared_flags_are_accepted_after_the_subcommand() {
    let p = Project::new("after");
    let cfg = p.config().to_str().unwrap().to_string();
    let root = p.root.to_str().unwrap().to_string();
    for flags in [
        vec!["find", "-c", &cfg, "--root", &root],
        vec!["find", "-c", &cfg, "--root", &root, "-f", "json"],
    ] {
        let (code, text) = run(&flags);
        assert_eq!(code, 0, "{flags:?} failed: {text}");
        assert!(text.contains('a'), "{flags:?} printed nothing: {text}");
    }
}

#[test]
fn shared_flags_are_rejected_before_the_subcommand() {
    let p = Project::new("before");
    let cfg = p.config().to_str().unwrap().to_string();
    let root = p.root.to_str().unwrap().to_string();
    for (flags, arg) in [
        (vec!["-c", &cfg as &str, "find"], "-c"),
        (vec!["-f", "json", "find"], "-f"),
        (vec!["--root", &root, "find"], "--root"),
    ] {
        let (code, text) = run(&flags);
        assert_eq!(code, 2, "{flags:?} was accepted: {text}");
        assert!(
            text.contains(&format!("unexpected argument '{arg}' found")),
            "{flags:?} gave an unexpected message: {text}"
        );
    }
}

/// `--types` and `--format` are `ValueEnum`s, so the valid set is written once
/// and clap lists it. The gem splits a list option on `/\s*,\s*/`, so a space
/// after the comma is still accepted.
///
/// ref: lib/i18n/tasks/command/option_parsers/enum.rb
#[test]
fn the_enum_flags_list_their_values_and_tolerate_spaces() {
    let p = Project::new("enums");
    let cfg = p.config().to_str().unwrap().to_string();
    let root = p.root.to_str().unwrap().to_string();
    let missing = |extra: &[&str]| {
        let mut args = vec!["missing", "-c", &cfg, "--root", &root];
        args.extend_from_slice(extra);
        run(&args)
    };

    let (code, text) = missing(&["--types", "used, diff"]);
    assert_eq!(code, 0, "{text}");

    let (code, text) = missing(&["--types", "bogus"]);
    assert_eq!(code, 2, "{text}");
    assert!(
        text.contains("[possible values: used, diff, plural]"),
        "{text}"
    );

    let (code, text) = missing(&["-f", "yaml"]);
    assert_eq!(code, 2, "{text}");
    assert!(text.contains("[possible values: text, json]"), "{text}");
}
