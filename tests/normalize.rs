//! The write path: `normalize`, `check-normalized`, the routers and the
//! emitter, end to end.
//!
//! ref: blocker B1 in `docs/design-notes.md`. The correctness properties are
//! value preservation and idempotence, not byte equality with Psych.

use i18n_tasks_rs::config::Config;
use i18n_tasks_rs::data::load::{Store, Value};
use i18n_tasks_rs::report::normalize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_i18n-tasks-rs");

struct Project {
    root: PathBuf,
}

impl Project {
    fn new(name: &str, config: &str) -> Project {
        let root = std::env::temp_dir().join(format!("i18n-tasks-rs-norm-{name}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("config/locales")).unwrap();
        std::fs::create_dir_all(root.join("app")).unwrap();
        std::fs::write(root.join("config/i18n-tasks.yml"), config).unwrap();
        Project { root }
    }

    fn write(&self, rel: &str, contents: &str) -> &Project {
        let path = self.root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
        self
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.root.join(rel)).unwrap_or_default()
    }

    fn exists(&self, rel: &str) -> bool {
        self.root.join(rel).is_file()
    }

    fn config(&self) -> Config {
        Config::load(&self.root.join("config/i18n-tasks.yml"), Some(&self.root)).expect("config")
    }

    fn store(&self) -> Store {
        Store::load(&self.config()).expect("store")
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

const SIMPLE: &str = "base_locale: en\nlocales: [en]\ndata:\n  read:\n    - config/locales/%{locale}.yml\nsearch:\n  paths: [app/]\n";

/// Every key of every locale, flattened, for the value-preservation check.
fn values(store: &Store) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    for locale in &store.locales {
        let Some(tree) = store.tree(locale) else {
            continue;
        };
        for leaf in &tree.leaves {
            out.insert(format!("{locale}.{}", leaf.key), leaf.value.clone());
        }
    }
    out
}

#[test]
fn golden_kitchen_sink() {
    let p = Project::new("golden", SIMPLE);
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden");
    let input = std::fs::read_to_string(dir.join("kitchen_sink.in.yml")).unwrap();
    let expected = std::fs::read_to_string(dir.join("kitchen_sink.out.yml")).unwrap();
    p.write("config/locales/en.yml", &input);

    let before = values(&p.store());
    let (code, text) = p.run(&["normalize", "--write"]);
    assert_eq!(code, 0, "{text}");
    assert_eq!(p.read("config/locales/en.yml"), expected);

    // Value preservation: the same keys map to the same values afterwards.
    assert_eq!(values(&p.store()), before);

    // Idempotence: a second run changes nothing.
    let (code, text) = p.run(&["normalize", "--write"]);
    assert_eq!(code, 0, "{text}");
    assert_eq!(p.read("config/locales/en.yml"), expected);
    assert_eq!(p.run(&["check-normalized"]).0, 0);
}

#[test]
fn check_normalized_never_writes() {
    let p = Project::new("nowrite", SIMPLE);
    p.write("config/locales/en.yml", "en:\n  b: B\n  a: A\n");
    let (code, text) = p.run(&["check-normalized"]);
    assert_eq!(code, 1);
    assert!(text.contains("config/locales/en.yml"), "{text}");
    assert_eq!(p.read("config/locales/en.yml"), "en:\n  b: B\n  a: A\n");
}

#[test]
fn normalize_writes_nothing_without_the_write_flag() {
    // ref: blocker B8.
    let p = Project::new("optin", SIMPLE);
    p.write("config/locales/en.yml", "en:\n  b: B\n  a: A\n");
    let (code, text) = p.run(&["normalize"]);
    assert_eq!(code, 0);
    assert!(text.contains("Nothing was written"), "{text}");
    assert_eq!(p.read("config/locales/en.yml"), "en:\n  b: B\n  a: A\n");
}

#[test]
fn remove_unused_writes_nothing_without_the_write_flag() {
    let p = Project::new("remove-unused-optin", SIMPLE);
    p.write(
        "config/locales/en.yml",
        "en:\n  used: Used\n  unused: Unused\n",
    )
    .write("app/use.rb", "I18n.t('used')\n");

    let (code, text) = p.run(&["remove-unused"]);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("unused"), "{text}");
    assert!(text.contains("Nothing was written"), "{text}");
    assert_eq!(
        p.read("config/locales/en.yml"),
        "en:\n  used: Used\n  unused: Unused\n"
    );
}

#[test]
fn remove_unused_removes_only_unused_keys_selected_by_pattern() {
    let p = Project::new("remove-unused-pattern", SIMPLE);
    p.write(
        "config/locales/en.yml",
        "en:\n  used: Used\n  stale:\n    first: One\n    second: Two\n",
    )
    .write("app/use.rb", "I18n.t('used')\n");

    let (code, text) = p.run(&["remove-unused", "--pattern", "stale.first", "--write"]);
    assert_eq!(code, 0, "{text}");
    assert_eq!(
        p.read("config/locales/en.yml"),
        "---\nen:\n  stale:\n    second: Two\n  used: Used\n"
    );
}

#[test]
fn remove_unused_pattern_noop_does_not_normalize_the_file() {
    let p = Project::new("remove-unused-pattern-noop", SIMPLE);
    p.write("config/locales/en.yml", "en:\n  z: Z\n  a: A\n");

    let (code, text) = p.run(&["remove-unused", "--pattern", "not.there", "--write"]);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("No unused keys"), "{text}");
    assert_eq!(p.read("config/locales/en.yml"), "en:\n  z: Z\n  a: A\n");
}

#[test]
fn remove_unused_pattern_can_select_a_collapsed_plural_node() {
    let p = Project::new("remove-unused-plural", SIMPLE);
    p.write(
        "config/locales/en.yml",
        "en:\n  used: Used\n  apples:\n    one: One\n    other: Other\n",
    )
    .write("app/use.rb", "I18n.t('used')\n");

    let (code, text) = p.run(&["remove-unused", "--pattern", "apples", "--write"]);
    assert_eq!(code, 0, "{text}");
    assert_eq!(p.read("config/locales/en.yml"), "---\nen:\n  used: Used\n");
}

#[test]
fn remove_unused_does_not_expand_a_partial_plural_to_an_ignored_child() {
    let p = Project::new(
        "remove-unused-partial-plural",
        "base_locale: en\n\
         locales: [en]\n\
         data:\n\
         \x20 read:\n\
         \x20   - config/locales/%{locale}.yml\n\
         search:\n\
         \x20 paths: [app/]\n\
         ignore_unused: [apples.one]\n",
    );
    p.write(
        "config/locales/en.yml",
        "en:\n  apples:\n    one: One\n    other: Other\n",
    );

    let (code, text) = p.run(&["remove-unused", "--pattern", "apples", "--write"]);
    assert_eq!(code, 0, "{text}");
    assert_eq!(
        p.read("config/locales/en.yml"),
        "---\nen:\n  apples:\n    one: One\n"
    );
}

#[test]
fn remove_unused_pattern_can_select_an_unused_subtree() {
    let p = Project::new("remove-unused-subtree", SIMPLE);
    p.write(
        "config/locales/en.yml",
        "en:\n  used: Used\n  stale:\n    first: One\n    second: Two\n",
    )
    .write("app/use.rb", "I18n.t('used')\n");

    let (code, text) = p.run(&["remove-unused", "--pattern", "stale", "--write"]);
    assert_eq!(code, 0, "{text}");
    assert_eq!(p.read("config/locales/en.yml"), "---\nen:\n  used: Used\n");
}

#[test]
fn remove_unused_keep_order_preserves_the_remaining_order() {
    let p = Project::new("remove-unused-order", SIMPLE);
    p.write(
        "config/locales/en.yml",
        "en:\n  z_used: Z\n  stale: S\n  a_used: A\n",
    )
    .write("app/use.rb", "I18n.t('z_used')\nI18n.t('a_used')\n");

    let (code, text) = p.run(&["remove-unused", "--keep-order", "--write"]);
    assert_eq!(code, 0, "{text}");
    assert_eq!(
        p.read("config/locales/en.yml"),
        "---\nen:\n  z_used: Z\n  a_used: A\n"
    );
}

#[test]
fn remove_unused_needs_permission_to_delete_an_empty_file() {
    let p = Project::new("remove-unused-delete", SIMPLE);
    p.write("config/locales/en.yml", "en:\n  stale: Stale\n");

    let (code, text) = p.run(&["remove-unused", "--write"]);
    assert_eq!(code, 2, "{text}");
    assert!(text.contains("--allow-delete"), "{text}");
    assert!(p.exists("config/locales/en.yml"));

    let (code, text) = p.run(&["remove-unused", "--write", "--allow-delete"]);
    assert_eq!(code, 0, "{text}");
    assert!(!p.exists("config/locales/en.yml"));
}

#[test]
fn remove_unused_accepts_duplicate_locale_arguments() {
    let p = Project::new("remove-unused-duplicate-locale", SIMPLE);
    p.write(
        "config/locales/en.yml",
        "en:\n  used: Used\n  stale: Stale\n",
    )
    .write("app/use.rb", "I18n.t('used')\n");

    let (code, text) = p.run(&["remove-unused", "en", "en"]);
    assert_eq!(code, 0, "{text}");
    assert!(!text.contains("destination for both"), "{text}");
}

#[test]
fn remove_unused_refuses_opaque_calls_without_an_override() {
    let p = Project::new("remove-unused-opaque", SIMPLE);
    p.write("config/locales/en.yml", "en:\n  live: Live\n")
        .write("app/use.rb", "I18n.t(key_from_database)\n");

    let (code, text) = p.run(&["remove-unused", "--write", "--allow-delete"]);
    assert_eq!(code, 2, "{text}");
    assert!(text.contains("--allow-opaque"), "{text}");
    assert!(p.exists("config/locales/en.yml"));

    let (code, text) = p.run(&[
        "remove-unused",
        "--write",
        "--allow-delete",
        "--allow-opaque",
    ]);
    assert_eq!(code, 0, "{text}");
    assert!(!p.exists("config/locales/en.yml"));
}

#[test]
fn remove_unused_dry_run_prints_a_diff_and_does_not_write() {
    let p = Project::new("remove-unused-dry-run", SIMPLE);
    p.write(
        "config/locales/en.yml",
        "en:\n  used: Used\n  stale: Stale\n",
    )
    .write("app/use.rb", "I18n.t('used')\n");

    let (code, text) = p.run(&["remove-unused", "--dry-run"]);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("--- a/config/locales/en.yml"), "{text}");
    assert!(text.contains("-  stale: Stale"), "{text}");
    assert_eq!(
        p.read("config/locales/en.yml"),
        "en:\n  used: Used\n  stale: Stale\n"
    );
}

#[test]
fn remove_unused_rejects_write_with_dry_run() {
    let p = Project::new("remove-unused-contradict", SIMPLE);
    p.write("config/locales/en.yml", "en:\n  stale: Stale\n");

    let (code, text) = p.run(&["remove-unused", "--write", "--dry-run"]);
    assert_eq!(code, 2, "{text}");
    assert!(text.contains("contradict"), "{text}");
    assert!(p.exists("config/locales/en.yml"));
}

#[test]
fn remove_unused_json_does_not_claim_a_refused_write() {
    let p = Project::new("remove-unused-json-refusal", SIMPLE);
    p.write("config/locales/en.yml", "en:\n  stale: Stale\n");

    let out = Command::new(BIN)
        .args(["remove-unused", "--write", "--format", "json"])
        .arg("-c")
        .arg(p.root.join("config/i18n-tasks.yml"))
        .arg("--root")
        .arg(&p.root)
        .output()
        .expect("binary runs");
    assert_eq!(out.status.code(), Some(2));
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    assert_eq!(json["written"], false);
    assert_eq!(json["unused"]["rows"][0]["key"], "stale");
    assert!(p.exists("config/locales/en.yml"));
}

#[test]
fn remove_unused_json_reports_a_successful_write() {
    let p = Project::new("remove-unused-json-success", SIMPLE);
    p.write(
        "config/locales/en.yml",
        "en:\n  used: Used\n  stale: Stale\n",
    )
    .write("app/use.rb", "I18n.t('used')\n");

    let out = Command::new(BIN)
        .args(["remove-unused", "--write", "--format", "json"])
        .arg("-c")
        .arg(p.root.join("config/i18n-tasks.yml"))
        .arg("--root")
        .arg(&p.root)
        .output()
        .expect("binary runs");
    assert_eq!(out.status.code(), Some(0));
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    assert_eq!(json["written"], true);
    assert_eq!(json["unused"]["rows"][0]["key"], "stale");
    assert_eq!(p.read("config/locales/en.yml"), "---\nen:\n  used: Used\n");
}

#[test]
fn remove_unused_json_noop_has_an_empty_plan() {
    let p = Project::new("remove-unused-json-noop", SIMPLE);
    p.write("config/locales/en.yml", "en:\n  used: Used\n")
        .write("app/use.rb", "I18n.t('used')\n");

    let out = Command::new(BIN)
        .args(["remove-unused", "--format", "json"])
        .arg("-c")
        .arg(p.root.join("config/i18n-tasks.yml"))
        .arg("--root")
        .arg(&p.root)
        .output()
        .expect("binary runs");
    assert_eq!(out.status.code(), Some(0));
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    assert_eq!(json["written"], false);
    assert_eq!(json["changes"], serde_json::json!([]));
    assert_eq!(json["files_routed"], 0);
}

#[test]
fn remove_unused_changes_only_the_selected_locale() {
    let p = Project::new(
        "remove-unused-locale",
        "base_locale: en\n\
         locales: [en, de]\n\
         data:\n\
         \x20 read:\n\
         \x20   - config/locales/%{locale}.yml\n\
         search:\n\
         \x20 paths: [app/]\n",
    );
    p.write("config/locales/en.yml", "en:\n  stale: English\n")
        .write("config/locales/de.yml", "de:\n  stale: German\n");

    let (code, text) = p.run(&["remove-unused", "en", "--write", "--allow-delete"]);
    assert_eq!(code, 0, "{text}");
    assert!(!p.exists("config/locales/en.yml"));
    assert_eq!(p.read("config/locales/de.yml"), "de:\n  stale: German\n");
}

#[test]
fn remove_unused_refuses_to_delete_a_file_that_holds_an_unselected_locale() {
    let p = Project::new(
        "remove-unused-shared-unselected",
        "base_locale: en\n\
         locales: [en, de]\n\
         data:\n\
         \x20 read:\n\
         \x20   - config/locales/all.yml\n\
         search:\n\
         \x20 paths: [app/]\n",
    );
    let contents = "en:\n  stale: English\nde:\n  stale: German\n";
    p.write("config/locales/all.yml", contents);

    let (code, text) = p.run(&["remove-unused", "en", "--write", "--allow-delete"]);
    assert_eq!(code, 2, "{text}");
    assert!(text.contains("holds the locale(s) de"), "{text}");
    assert_eq!(p.read("config/locales/all.yml"), contents);
}

#[test]
fn remove_unused_does_not_apply_duplicate_deletes_for_a_shared_file() {
    let p = Project::new(
        "remove-unused-shared-selected",
        "base_locale: en\n\
         locales: [en, de]\n\
         data:\n\
         \x20 read:\n\
         \x20   - config/locales/all.yml\n\
         search:\n\
         \x20 paths: [app/]\n",
    );
    let contents = "en:\n  stale: English\nde:\n  stale: German\n";
    p.write("config/locales/all.yml", contents);

    let (code, text) = p.run(&["remove-unused", "--write", "--allow-delete"]);
    assert_eq!(code, 2, "{text}");
    assert!(text.contains("holds the locale(s) de"), "{text}");
    assert_eq!(p.read("config/locales/all.yml"), contents);
}

#[test]
fn dry_run_prints_a_unified_diff_and_writes_nothing() {
    let p = Project::new("dryrun", SIMPLE);
    p.write("config/locales/en.yml", "en:\n  b: B\n  a: A\n");
    let (code, text) = p.run(&["normalize", "--dry-run"]);
    assert_eq!(code, 0);
    assert!(text.contains("--- a/config/locales/en.yml"), "{text}");
    assert!(text.contains("+++ b/config/locales/en.yml"), "{text}");
    assert!(text.contains("+  b: B"), "{text}");
    assert_eq!(p.read("config/locales/en.yml"), "en:\n  b: B\n  a: A\n");
}

#[test]
fn write_and_dry_run_contradict_each_other() {
    let p = Project::new("contradict", SIMPLE);
    p.write("config/locales/en.yml", "---\nen:\n  a: A\n");
    let (code, text) = p.run(&["normalize", "--write", "--dry-run"]);
    assert_eq!(code, 2);
    assert!(text.contains("contradict"), "{text}");
}

const PATTERN_CONFIG: &str = "base_locale: en\n\
     locales: [en]\n\
     data:\n\
     \x20 read:\n\
     \x20   - config/locales/*.%{locale}.yml\n\
     \x20 write:\n\
     \x20   - [\"{activerecord, views}.*\", 'config/locales/\\1.%{locale}.yml']\n\
     \x20   - config/locales/base.%{locale}.yml\n\
     search:\n\
     \x20 paths: [app/]\n";

#[test]
fn the_conservative_router_keeps_a_key_in_its_own_file() {
    let p = Project::new("conservative", PATTERN_CONFIG);
    // `views.home.title` lives in the wrong file for `data.write`, and the
    // conservative router leaves it there.
    p.write(
        "config/locales/base.en.yml",
        "en:\n  views:\n    home:\n      title: T\n",
    );
    let (code, text) = p.run(&["normalize", "--write"]);
    assert_eq!(code, 0, "{text}");
    assert_eq!(
        p.read("config/locales/base.en.yml"),
        "---\nen:\n  views:\n    home:\n      title: T\n"
    );
    assert!(!p.exists("config/locales/views.en.yml"));
}

#[test]
fn the_pattern_router_moves_keys_and_needs_allow_delete() {
    let p = Project::new("pattern", PATTERN_CONFIG);
    p.write(
        "config/locales/base.en.yml",
        "en:\n  views:\n    home:\n      title: T\n",
    );
    // Every key moves out, so `base.en.yml` ends up empty.
    let (code, text) = p.run(&["normalize", "--pattern-router", "--write"]);
    assert_eq!(code, 2, "{text}");
    assert!(text.contains("--allow-delete"), "{text}");
    assert!(text.contains("config/locales/base.en.yml"), "{text}");
    assert!(p.exists("config/locales/base.en.yml"));

    let (code, text) = p.run(&["normalize", "--pattern-router", "--write", "--allow-delete"]);
    assert_eq!(code, 0, "{text}");
    assert_eq!(
        p.read("config/locales/views.en.yml"),
        "---\nen:\n  views:\n    home:\n      title: T\n"
    );
    assert!(!p.exists("config/locales/base.en.yml"));
}

#[test]
fn the_pattern_router_falls_through_to_the_catch_all() {
    let p = Project::new("fallthrough", PATTERN_CONFIG);
    p.write("config/locales/other.en.yml", "en:\n  misc:\n    a: A\n");
    let (code, text) = p.run(&["normalize", "--pattern-router", "--write", "--allow-delete"]);
    assert_eq!(code, 0, "{text}");
    assert_eq!(
        p.read("config/locales/base.en.yml"),
        "---\nen:\n  misc:\n    a: A\n"
    );
}

#[test]
fn keep_order_leaves_the_key_order_alone() {
    let p = Project::new(
        "keeporder",
        "base_locale: en\n\
         locales: [en]\n\
         data:\n\
         \x20 read:\n\
         \x20   - config/locales/%{locale}.yml\n\
         \x20 keep_order: true\n\
         search:\n\
         \x20 paths: [app/]\n",
    );
    p.write("config/locales/en.yml", "en:\n  z: Z\n  a: A\n");
    let (code, text) = p.run(&["normalize", "--write"]);
    assert_eq!(code, 0, "{text}");
    assert_eq!(
        p.read("config/locales/en.yml"),
        "---\nen:\n  z: Z\n  a: A\n"
    );
}

#[test]
fn a_file_holding_two_locales_is_refused() {
    let p = Project::new("twolocales", SIMPLE);
    p.write("config/locales/en.yml", "en:\n  a: A\nde:\n  a: A\n");
    let (code, text) = p.run(&["normalize", "--write"]);
    assert_eq!(code, 2, "{text}");
    assert!(text.contains("holds the locale(s) de"), "{text}");
    // Nothing was touched.
    assert_eq!(
        p.read("config/locales/en.yml"),
        "en:\n  a: A\nde:\n  a: A\n"
    );
}

#[test]
fn two_locales_routed_to_one_file_are_refused() {
    let p = Project::new(
        "clash",
        "base_locale: en\n\
         locales: [en, de]\n\
         data:\n\
         \x20 read:\n\
         \x20   - config/locales/%{locale}.yml\n\
         \x20 write:\n\
         \x20   - config/locales/all.yml\n\
         \x20 router: pattern_router\n\
         search:\n\
         \x20 paths: [app/]\n",
    );
    p.write("config/locales/en.yml", "en:\n  a: A\n");
    p.write("config/locales/de.yml", "de:\n  a: A\n");
    let (code, text) = p.run(&["normalize", "--write"]);
    assert_eq!(code, 2, "{text}");
    assert!(text.contains("destination for both"), "{text}");
}

#[test]
fn health_runs_check_normalized_without_writing() {
    let p = Project::new("health", SIMPLE);
    p.write("config/locales/en.yml", "en:\n  a: A\n")
        .write("app/a.rb", "t('a')\n");
    let (code, text) = p.run(&["health"]);
    assert_eq!(code, 1, "{text}");
    assert!(text.contains("requires normalization"), "{text}");
    assert_eq!(p.read("config/locales/en.yml"), "en:\n  a: A\n");
}

#[test]
fn a_new_key_from_another_locale_lands_in_the_matching_file() {
    // The conservative router rewrites the locale part of the other locale's
    // path. ref: LocalePathname#replace_locale.
    let p = Project::new(
        "infer",
        "base_locale: en\n\
         locales: [en, de]\n\
         data:\n\
         \x20 read:\n\
         \x20   - config/locales/*.%{locale}.yml\n\
         search:\n\
         \x20 paths: [app/]\n",
    );
    p.write(
        "config/locales/models.en.yml",
        "en:\n  user: User\n  role: Role\n",
    );
    p.write("config/locales/models.de.yml", "de:\n  user: Benutzer\n");
    let cfg = p.config();
    let store = p.store();
    let fallback = i18n_tasks_rs::data::route::PatternRouter::new(&cfg);
    let router = i18n_tasks_rs::data::route::ConservativeRouter::new(&fallback, &store);
    // `user` is in both locales, so `de` keeps its own file.
    assert_eq!(
        router.route_key("de", "user").unwrap(),
        p.root.join("config/locales/models.de.yml")
    );
    // `role` is only in `en`, so its `en` path has its locale part rewritten.
    assert_eq!(
        router.route_key("de", "role").unwrap(),
        p.root.join("config/locales/models.de.yml")
    );
    // A key no locale has yet falls through to `data.write`.
    assert_eq!(
        router.route_key("de", "brand.new").unwrap(),
        PathBuf::from("config/locales/de.yml")
    );
}

#[test]
fn plan_reports_but_does_not_write() {
    let p = Project::new("planonly", SIMPLE);
    p.write("config/locales/en.yml", "en:\n  b: B\n  a: A\n");
    let cfg = p.config();
    let store = p.store();
    let report = normalize::plan(&cfg, &store, &store.locales, false).unwrap();
    assert_eq!(report.changes.len(), 1);
    assert_eq!(report.changes[0].action, normalize::Action::Update);
    assert_eq!(p.read("config/locales/en.yml"), "en:\n  b: B\n  a: A\n");
}

/// A routed key whose destination directory does not exist yet gets it created.
#[test]
fn writing_creates_a_missing_directory() {
    let p = Project::new(
        "mkdir",
        "base_locale: en\nlocales: [en]\ndata:\n  read:\n    - config/locales/**/*%{locale}.yml\n  write:\n    - [\"views.*\", 'config/locales/nested/deeper/views.%{locale}.yml']\n    - config/locales/base.%{locale}.yml\nsearch:\n  paths: [app/]\n",
    );
    p.write(
        "config/locales/base.en.yml",
        "---\nen:\n  views:\n    home: Home\n  other: O\n",
    );
    let (code, text) = p.run(&["normalize", "--pattern-router", "--write", "--allow-delete"]);
    assert_eq!(code, 0, "{text}");
    assert!(
        p.exists("config/locales/nested/deeper/views.en.yml"),
        "{text}"
    );
    assert_eq!(
        p.read("config/locales/nested/deeper/views.en.yml"),
        "---\nen:\n  views:\n    home: Home\n"
    );
    // The values survived the move.
    assert!(p.store().key_value("en", "views.home"));
    assert!(p.store().key_value("en", "other"));
}

/// `normalize --format json` reports the plan instead of the diff.
#[test]
fn normalize_emits_json() {
    let p = Project::new("normjson", SIMPLE);
    p.write("config/locales/en.yml", "en:\n  b: B\n  a: A\n");
    let (code, text) = p.run(&["normalize", "-f", "json"]);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("\"check\": \"normalize\""), "{text}");
    assert!(text.contains("\"written\": false"), "{text}");
    assert!(text.contains("\"files_routed\""), "{text}");
    // Nothing was touched.
    assert_eq!(p.read("config/locales/en.yml"), "en:\n  b: B\n  a: A\n");

    let (code, text) = p.run(&["normalize", "-f", "json", "--write"]);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("\"written\": true"), "{text}");
    assert_eq!(
        p.read("config/locales/en.yml"),
        "---\nen:\n  a: A\n  b: B\n"
    );
}

/// The deletion list is printed whether or not the run may act on it, so a
/// `--dry-run` shows what a `--write` would remove.
#[test]
fn a_dry_run_still_lists_the_deletions() {
    let p = Project::new(
        "dryrundelete",
        "base_locale: en\nlocales: [en]\ndata:\n  read:\n    - config/locales/*.%{locale}.yml\n  write:\n    - config/locales/base.%{locale}.yml\nsearch:\n  paths: [app/]\n",
    );
    p.write("config/locales/base.en.yml", "---\nen:\n  a: A\n")
        .write("config/locales/extra.en.yml", "---\nen:\n  b: B\n");
    let (code, text) = p.run(&["normalize", "--pattern-router", "--dry-run"]);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("1 file(s) end up with no keys"), "{text}");
    assert!(text.contains("extra.en.yml"), "{text}");
    // Nothing was removed.
    assert!(p.exists("config/locales/extra.en.yml"));
}

/// A destination whose directory cannot be created says so, rather than
/// leaving the tree half-written.
#[test]
fn a_destination_directory_that_cannot_be_created_is_an_error() {
    let p = Project::new(
        "blocked-dir",
        "base_locale: en\nlocales: [en]\ndata:\n  read:\n    - config/locales/*%{locale}.yml\n  write:\n    - config/locales/blocker/out.%{locale}.yml\nsearch:\n  paths: [app/]\n",
    );
    p.write("config/locales/en.yml", "---\nen:\n  a: A\n")
        // `blocker` is a file, so `blocker/out.en.yml` has no directory.
        .write("config/locales/blocker", "not a directory\n");
    let (code, text) = p.run(&["normalize", "--pattern-router", "--write", "--allow-delete"]);
    assert_eq!(code, 2, "{text}");
    assert!(text.contains("cannot create"), "{text}");
}

/// `apply` must write the path that `plan` chose, not a re-parse of the
/// string the report prints. A destination component holding a `\` is legal on
/// Unix, and `display_path` turns that byte into a separator.
#[test]
fn a_destination_holding_a_backslash_is_written_where_it_was_planned() {
    let p = Project::new(
        "backslash",
        "base_locale: en\nlocales: [en]\ndata:\n  read:\n    - config/locales/%{locale}.yml\n  write:\n    - \"config/back\\\\slash/%{locale}.yml\"\nsearch:\n  paths: [app/]\n",
    );
    p.write("config/locales/en.yml", "---\nen:\n  a: A\n");
    let (code, text) = p.run(&["normalize", "--pattern-router", "--write", "--allow-delete"]);
    assert_eq!(code, 0, "{text}");
    assert!(
        p.exists("config/back\\slash/en.yml"),
        "the planned path was not written: {text}"
    );
    assert!(
        !p.exists("config/back/slash/en.yml"),
        "the `\\` was read back as a separator: {text}"
    );
}

/// The other half of the same guarantee: a `data.write` path that leaves the
/// root, and one that is absolute, both have to survive the trip from `plan`
/// to `apply`.
#[test]
fn a_destination_outside_the_root_is_written_where_it_was_planned() {
    let p = Project::new("escape-root", SIMPLE);
    let root = p.root.join("proj");
    let abs = p.root.join("absolute").join("%{locale}.yml");
    std::fs::create_dir_all(root.join("config/locales")).unwrap();
    let cfg_path = root.join("config/i18n-tasks.yml");
    std::fs::write(
        &cfg_path,
        format!(
            "base_locale: en\nlocales: [en]\ndata:\n  read:\n    - config/locales/%{{locale}}.yml\n  write:\n    - ['out.*', '../outside/%{{locale}}.yml']\n    - ['abs.*', '{}']\n    - config/locales/%{{locale}}.yml\nsearch:\n  paths: [app/]\n",
            abs.display()
        ),
    )
    .unwrap();
    std::fs::write(
        root.join("config/locales/en.yml"),
        "---\nen:\n  out:\n    a: A\n  abs:\n    b: B\n  keep:\n    c: C\n",
    )
    .unwrap();

    let cfg = Config::load(&cfg_path, Some(&root)).expect("config");
    let store = Store::load(&cfg).expect("store");
    let report = normalize::plan(&cfg, &store, &store.locales, true).unwrap();
    normalize::apply(&report).unwrap();

    assert_eq!(
        std::fs::read_to_string(p.root.join("outside/en.yml")).unwrap_or_default(),
        "---\nen:\n  out:\n    a: A\n"
    );
    assert_eq!(
        std::fs::read_to_string(p.root.join("absolute/en.yml")).unwrap_or_default(),
        "---\nen:\n  abs:\n    b: B\n"
    );
    // Nothing landed under the root pretending to be either of them.
    assert!(!root.join("outside/en.yml").exists());
    assert!(!root.join("absolute").exists());
}

/// Every plain form YAML 1.1 reads as a number keeps the bytes it was written
/// with, so the Ruby type the app sees does not change.
/// ref: docs/accepted-diffs.md §29.
#[test]
fn every_number_form_round_trips_unquoted() {
    let p = Project::new("numbers", SIMPLE);
    let input = "---\nen:\n  date: 2026-08-24\n  float: 1.5\n  hexy: 0x1f\n  inf: .inf\n  \
                 int: 42\n  octal: 017\n  sexa: 1:30\n";
    p.write("config/locales/en.yml", input);
    let (code, text) = p.run(&["normalize", "--write"]);
    assert_eq!(code, 0, "{text}");
    assert_eq!(p.read("config/locales/en.yml"), input);
    assert_eq!(p.run(&["check-normalized"]).0, 0);
}

/// Psych resolves `yes`, `no`, `on` and `off` to booleans, in any casing. The
/// loader has to agree, or the emitter quotes them and the app reads a String
/// where it read `true`.
#[test]
fn a_yaml_boolean_stays_a_boolean() {
    let p = Project::new("booleans", SIMPLE);
    p.write(
        "config/locales/en.yml",
        "en:\n  a: yes\n  b: Off\n  c: nO\n  d: TRUE\n",
    );
    let (code, text) = p.run(&["normalize", "--write"]);
    assert_eq!(code, 0, "{text}");
    assert_eq!(
        p.read("config/locales/en.yml"),
        "---\nen:\n  a: true\n  b: false\n  c: false\n  d: true\n"
    );
    assert_eq!(p.run(&["check-normalized"]).0, 0);
}

/// `normalize --write` goes through a sibling temp file, so the run has to
/// leave the destination directory holding the locale files and nothing else.
#[test]
fn a_written_run_leaves_no_temp_file_behind() {
    let p = Project::new(
        "no-temp",
        "base_locale: en\nlocales: [en, de]\ndata:\n  read:\n    - config/locales/%{locale}.yml\nsearch:\n  paths: [app/]\n",
    );
    p.write("config/locales/en.yml", "en:\n  b: B\n  a: A\n")
        .write("config/locales/de.yml", "de:\n  b: B\n  a: A\n");
    let (code, text) = p.run(&["normalize", "--write"]);
    assert_eq!(code, 0, "{text}");
    let mut names: Vec<String> = std::fs::read_dir(p.root.join("config/locales"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["de.yml", "en.yml"]);
    assert_eq!(
        p.read("config/locales/en.yml"),
        "---\nen:\n  a: A\n  b: B\n"
    );
}
