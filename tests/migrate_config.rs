//! `migrate-config` against the two configs that matter: the gem's own
//! template, and a realistic project's ERB config.
//!
//! The second one is the stronger test.
//! `tests/fixtures/sample_app/i18n-tasks-rs.yml` was hand-translated from
//! `tests/fixtures/sample_app/i18n-tasks.yml.erb` before this command existed,
//! so migrating the ERB has to arrive at the same place a human did.

use i18n_tasks_rs::config::{Config, IgnoreSpec, Router};
use i18n_tasks_rs::migrate;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_i18n-tasks-rs");
const TARGET: &str = "config/i18n-tasks-rs.yml";

fn migrate_file(path: &str) -> (migrate::Migration, Config) {
    let src = std::fs::read_to_string(path).expect("fixture is checked in");
    let m = migrate::migrate(&src, Path::new(path), Path::new(TARGET)).expect("migration succeeds");
    let cfg =
        Config::parse(&m.output, Path::new(TARGET), PathBuf::from(".")).expect("output loads");
    (m, cfg)
}

#[test]
fn the_erb_config_migrates_to_what_a_human_wrote_by_hand() {
    let (m, migrated) = migrate_file("tests/fixtures/sample_app/i18n-tasks.yml.erb");
    let hand = std::fs::read_to_string("tests/fixtures/sample_app/i18n-tasks-rs.yml").unwrap();
    let hand = Config::parse(&hand, Path::new(TARGET), PathBuf::from(".")).unwrap();

    // Nothing in that file needs a human: every ERB tag in it is either the
    // `require` prelude or sits inside a comment.
    assert!(!m.needs_attention(), "{:?}", m.manual);

    assert_eq!(migrated.base_locale, hand.base_locale);
    assert_eq!(migrated.locales, hand.locales);
    assert_eq!(
        format!("{:?}", migrated.data),
        format!("{:?}", hand.data),
        "data"
    );
    assert_eq!(
        format!("{:?}", migrated.search),
        format!("{:?}", hand.search),
        "search"
    );
    assert_eq!(migrated.ignore, hand.ignore);
    for (a, b) in [
        (&migrated.ignore_missing, &hand.ignore_missing),
        (&migrated.ignore_unused, &hand.ignore_unused),
    ] {
        assert_eq!(format!("{a:?}"), format!("{b:?}"));
    }
    // The one difference, and it is the hand translation's: the ERB has these
    // patterns under an `all:` group, and the human flattened them to a list.
    // Both mean the same thing.
    match (&migrated.ignore_eq_base, &hand.ignore_eq_base) {
        (IgnoreSpec::PerLocale(groups), IgnoreSpec::All(flat)) => {
            assert_eq!(groups.len(), 1);
            assert_eq!(groups[0].0, ["all"]);
            assert_eq!(&groups[0].1, flat);
        }
        (a, b) => panic!("unexpected shapes: {a:?} / {b:?}"),
    }

    // The five settings the gem config carried that this port has no answer for.
    let dropped: Vec<&str> = m.dropped.iter().map(|d| d.key.as_str()).collect();
    assert_eq!(
        dropped,
        [
            "data.external",
            "data.yaml",
            "search.prism",
            "search.scanners",
            "translation"
        ]
    );
    // Every reason is recorded in the file itself, not just on the terminal.
    for d in &m.dropped {
        assert!(m.output.contains(&d.reason), "{} unexplained", d.key);
    }
}

#[test]
fn the_comment_that_explains_an_ignored_key_survives() {
    let (m, _) = migrate_file("tests/fixtures/sample_app/i18n-tasks.yml.erb");
    // These comments are the only record of why the key is ignored, so losing
    // them in a migration would be worse than losing the setting.
    for comment in [
        "# Rendered from contact request status filters.",
        "# Category names are resolved through Category#localized_name from category codes.",
        "# NOTE: prism right now doesn't detect a lot of dynamic interpolations",
        "# used in new admin only",
    ] {
        assert!(m.output.contains(comment), "lost {comment}");
    }
    // The commented-out entries under `ignore_missing` stay where they were.
    assert!(
        m.output
            .contains("# - 'errors.messages.{accepted,blank,invalid,too_short,too_long}'")
    );
}

#[test]
fn the_gem_template_migrates() {
    // Every supported setting in the gem's template is commented out, so what
    // is left is the four `search.exclude` paths.
    let (m, cfg) = migrate_file("tests/fixtures/gem/i18n-tasks.yml");
    assert!(!m.needs_attention(), "{:?}", m.manual);
    assert_eq!(cfg.base_locale, "en");
    assert_eq!(cfg.search.exclude.len(), 4);
    assert_eq!(cfg.data.router, Router::Conservative);
    // The template documents the whole gem in comments, including the ERB
    // `add_scanner` examples at the end. None of it may reach the output: a
    // single `<%` anywhere makes the config unreadable.
    assert!(!m.output.contains("<%"));
}

struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    fn new(name: &str, config: &str) -> Sandbox {
        let root = std::env::temp_dir().join(format!("i18n-tasks-rs-migrate-{name}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("config")).unwrap();
        std::fs::write(root.join("config/i18n-tasks.yml"), config).unwrap();
        Sandbox { root }
    }

    fn run(&self, args: &[&str]) -> (i32, String) {
        let out = Command::new(BIN)
            .arg("migrate-config")
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

#[test]
fn the_command_finds_the_gem_config_and_writes_once_asked() {
    let s = Sandbox::new(
        "write",
        "base_locale: de\nlocales: [de, en]\ninternal_locale: ru\n",
    );

    // Blocker B8: writing is opt-in here too.
    let (code, out) = s.run(&[]);
    assert_eq!(code, 0);
    assert!(out.contains("nothing written"), "{out}");
    assert!(!s.target().exists());
    assert!(out.contains("dropped internal_locale"), "{out}");

    let (code, _) = s.run(&["--write"]);
    assert_eq!(code, 0);
    let written = std::fs::read_to_string(s.target()).unwrap();
    assert!(written.contains("base_locale: de"));
    assert!(!written.contains("internal_locale: ru"));

    // A second run does not quietly replace a file someone may have edited.
    let (code, out) = s.run(&["--write"]);
    assert_eq!(code, 2);
    assert!(out.contains("--force"), "{out}");
    let (code, _) = s.run(&["--write", "--force"]);
    assert_eq!(code, 0);
}

#[test]
fn a_config_that_computes_a_value_exits_one() {
    let s = Sandbox::new(
        "erb",
        "base_locale: de\ndata:\n  read:\n    - \"<%= Rails.root %>/config/locales/%{locale}.yml\"\n",
    );
    let (code, out) = s.run(&["--write"]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("NEEDS ATTENTION line 4"), "{out}");
    // Written all the same, so the rest of the migration is not lost, and the
    // line that needs a human is named in the file.
    let written = std::fs::read_to_string(s.target()).unwrap();
    assert!(written.contains("NEEDS ATTENTION"));
    assert!(written.contains("[ERB]/config/locales/%{locale}.yml"));
    assert!(!written.contains("<%"));
}

#[test]
fn without_a_gem_config_the_command_says_where_it_looked() {
    let root = std::env::temp_dir().join("i18n-tasks-rs-migrate-empty");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let out = Command::new(BIN)
        .args(["migrate-config", "--root"])
        .arg(&root)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("config/i18n-tasks.yml"), "{err}");
    assert!(err.contains("--from"), "{err}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_missing_config_points_at_the_gem_config_next_door() {
    let s = Sandbox::new("hint", "base_locale: de\n");
    // `unused` with the default `-c`, which does not exist yet.
    let out = Command::new(BIN)
        .args(["unused", "--root"])
        .arg(&s.root)
        .current_dir(&s.root)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("migrate-config"), "{err}");
}

/// `--from` and `--to` name the two files explicitly, which is how a project
/// with a config outside `config/` migrates.
#[test]
fn from_and_to_can_be_named_explicitly() {
    let s = Sandbox::new("explicit", "base_locale: de\nlocales: [de]\n");
    std::fs::write(
        s.root.join("elsewhere.yml"),
        "base_locale: fr\nlocales: [fr]\n",
    )
    .unwrap();
    let out = Command::new(BIN)
        .args(["migrate-config", "--from"])
        .arg(s.root.join("elsewhere.yml"))
        .arg("--to")
        .arg(s.root.join("out/new.yml"))
        .arg("--write")
        .arg("--root")
        .arg(&s.root)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    // The named source was read, not the one the default search would find,
    // and the destination directory was created.
    let written = std::fs::read_to_string(s.root.join("out/new.yml")).unwrap();
    assert!(written.contains("base_locale: fr"), "{written}");
    // The default target was left alone.
    assert!(!s.target().exists());
}

/// Reading and writing the same file would destroy the source.
#[test]
fn from_and_to_may_not_be_the_same_file() {
    let s = Sandbox::new("same", "base_locale: de\n");
    let out = Command::new(BIN)
        .args(["migrate-config", "--from"])
        .arg(s.root.join("config/i18n-tasks.yml"))
        .arg("--to")
        .arg(s.root.join("config/i18n-tasks.yml"))
        .arg("--root")
        .arg(&s.root)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("the same file"), "{err}");
}

/// A `--from` that does not exist says so, rather than falling back to the
/// default search.
#[test]
fn a_named_source_that_does_not_exist_is_an_error() {
    let s = Sandbox::new("nosource", "base_locale: de\n");
    let out = Command::new(BIN)
        .args(["migrate-config", "--from"])
        .arg(s.root.join("nope.yml"))
        .arg("--root")
        .arg(&s.root)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("cannot read"), "{err}");
}

/// The ERB config is preferred only when the plain one is absent, which is the
/// opposite of the gem's order and is why the target has its own name.
#[test]
fn the_plain_config_wins_over_the_erb_one() {
    let s = Sandbox::new("both", "base_locale: de\nlocales: [de]\n");
    std::fs::write(
        s.root.join("config/i18n-tasks.yml.erb"),
        "base_locale: fr\nlocales: [fr]\n",
    )
    .unwrap();
    let (code, out) = s.run(&[]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("base_locale: de"), "{out}");
    // With only the ERB file present, that one is used.
    std::fs::remove_file(s.root.join("config/i18n-tasks.yml")).unwrap();
    let (code, out) = s.run(&[]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("base_locale: fr"), "{out}");
}

/// A `--to` whose parent directory cannot be created says so, rather than
/// failing later on the write.
#[test]
fn a_destination_directory_that_cannot_be_created_is_an_error() {
    let s = Sandbox::new("blocked-dir", "base_locale: de\nlocales: [de]\n");
    // `blocker` is a file, so `blocker/new.yml` has no directory to create.
    std::fs::write(s.root.join("blocker"), "not a directory\n").unwrap();
    let out = Command::new(BIN)
        .args(["migrate-config", "--to"])
        .arg(s.root.join("blocker/new.yml"))
        .arg("--write")
        .arg("--root")
        .arg(&s.root)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("cannot create"), "{err}");
}
