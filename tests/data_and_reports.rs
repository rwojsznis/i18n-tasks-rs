//! Locale data loading and the read-only reports, end to end.
//!
//! Ported from spec/file_system_data_spec.rb and the report specs.

use i18n_tasks_rs::config::Config;
use i18n_tasks_rs::data::load::{Store, Value};
use i18n_tasks_rs::pattern::PatternSet;
use i18n_tasks_rs::report::missing::MissingType;
use i18n_tasks_rs::report::{Outcome, Reason, eq_base, interpolations, missing, unused};
use i18n_tasks_rs::stats::forest_stats;
use i18n_tasks_rs::used::UsedKeys;
use std::path::{Path, PathBuf};

/// A throwaway project tree. Each test gets its own directory.
struct Project {
    root: PathBuf,
}

impl Project {
    fn new(name: &str) -> Project {
        let root = std::env::temp_dir().join(format!("i18n-tasks-rs-test-{name}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("config/locales")).unwrap();
        std::fs::create_dir_all(root.join("app/controllers")).unwrap();
        Project { root }
    }

    fn write(&self, rel: &str, contents: &str) -> &Project {
        let path = self.root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
        self
    }

    fn config(&self, body: &str) -> Config {
        let path = self.root.join("config/i18n-tasks.yml");
        std::fs::write(&path, body).unwrap();
        Config::load(&path, Some(&self.root)).expect("config loads")
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

const BASIC_CONFIG: &str = "base_locale: en\nlocales: [en, es]\ndata:\n  read:\n    - config/locales/%{locale}.yml\nsearch:\n  paths: [app/]\n";

#[test]
fn eq_base_matches_the_gem_and_honours_per_locale_ignores() {
    // ref: spec/i18n_tasks_spec.rb:226-229
    // ref: lib/i18n/tasks/missing_keys.rb:31-36,132-137
    let p = Project::new("eqbase");
    p.write(
        "config/locales/en.yml",
        "en:\n  same: Same\n  different: EN\n  sequence: [{one: 1, two: 2}]\n  ignored_all: Same\n  ignored_es: Same\n  only_base: Same\n",
    )
    .write(
        "config/locales/es.yml",
        "es:\n  same: Same\n  different: ES\n  sequence: [{two: 2, one: 1}]\n  ignored_all: Same\n  ignored_es: Same\n  only_es: Same\n",
    );
    let cfg = p.config(
        "base_locale: en\nlocales: [en, es]\ndata:\n  read:\n    - config/locales/%{locale}.yml\nignore_eq_base:\n  all: [ignored_all]\n  es: [ignored_es]\n",
    );
    let store = Store::load(&cfg).unwrap();
    let report = eq_base::report(&cfg, &store, &store.locales);
    assert_eq!(
        report.rows,
        vec![
            i18n_tasks_rs::report::KeyRow {
                locale: "es".into(),
                key: "same".into(),
                value: Some("Same".into()),
                reason: None,
            },
            i18n_tasks_rs::report::KeyRow {
                locale: "es".into(),
                key: "sequence".into(),
                value: Some("[{\"two\" => 2, \"one\" => 1}]".into()),
                reason: None,
            },
        ]
    );
    assert_eq!(report.outcome(), Outcome::Found);
}

#[test]
fn overlapping_read_globs_are_deduplicated_and_merge_in_order() {
    // A real-world config has exactly this shape: the second glob matches every
    // file the first one does.
    let p = Project::new("overlap");
    p.write(
        "config/locales/base.en.yml",
        "en:\n  a: from base\n  shared: base wins?\n",
    )
    .write(
        "config/locales/other.en.yml",
        "en:\n  b: from other\n  shared: other wins\n",
    );
    let cfg = p.config(
        "base_locale: en\nlocales: [en]\ndata:\n  read:\n    - config/locales/base.%{locale}.yml\n    - config/locales/*.%{locale}.yml\n",
    );
    let store = Store::load(&cfg).unwrap();
    let tree = store.tree("en").unwrap();
    // Three keys, not five: `base.en.yml` is read once even though both globs
    // match it.
    assert_eq!(tree.leaves.len(), 3);
    assert_eq!(tree.get("a").unwrap().value, Value::Str("from base".into()));
    assert_eq!(
        tree.get("b").unwrap().value,
        Value::Str("from other".into())
    );
    // A later file wins, matching the gem's `reduce(:merge!)`.
    assert_eq!(
        tree.get("shared").unwrap().value,
        Value::Str("other wins".into())
    );
    // Every key remembers its origin file, which the conservative router needs.
    assert!(tree.get("a").unwrap().path.ends_with("base.en.yml"));
    assert!(tree.get("b").unwrap().path.ends_with("other.en.yml"));
}

#[test]
fn locales_are_inferred_from_the_data_when_unset() {
    let p = Project::new("infer");
    p.write("config/locales/en.yml", "en:\n  a: A\n")
        .write("config/locales/fr.yml", "fr:\n  a: A\n")
        .write("config/locales/de.yml", "de:\n  a: A\n");
    let cfg = p.config("base_locale: fr\ndata:\n  read:\n    - config/locales/%{locale}.yml\n");
    let store = Store::load(&cfg).unwrap();
    // Base locale first, the rest sorted. ref: lib/i18n/tasks/locale_list.rb
    assert_eq!(store.locales, vec!["fr", "de", "en"]);
}

#[test]
fn nil_keys_are_skipped_with_a_warning() {
    // ref: file_system_base.rb#filter_nil_keys!
    let p = Project::new("nilkeys");
    p.write(
        "config/locales/en.yml",
        "en:\n  a: A\n  ~: dropped\n  null: also dropped\n",
    );
    let cfg = p.config(BASIC_CONFIG.replace("[en, es]", "[en]").as_str());
    let store = Store::load(&cfg).unwrap();
    assert_eq!(store.tree("en").unwrap().leaves.len(), 1);
    assert_eq!(store.warnings.len(), 2);
    assert!(store.warnings[0].contains("nil key"));
}

#[test]
fn an_empty_mapping_contributes_no_leaves() {
    // `from_key_value` gives an empty Hash empty children, so the node is not a
    // leaf and never appears in a report.
    let p = Project::new("emptymap");
    p.write("config/locales/en.yml", "en:\n  a: A\n  b: {}\n");
    let cfg = p.config(BASIC_CONFIG.replace("[en, es]", "[en]").as_str());
    let store = Store::load(&cfg).unwrap();
    assert_eq!(store.tree("en").unwrap().leaves.len(), 1);
}

#[test]
fn a_reference_value_is_an_error_but_a_sequence_symbol_is_not() {
    // Blocker B4.
    let p = Project::new("refs");
    p.write("config/locales/en.yml", "en:\n  a: :other.key\n");
    let cfg = p.config(BASIC_CONFIG.replace("[en, es]", "[en]").as_str());
    let err = Store::load(&cfg).unwrap_err();
    assert!(err.contains("reference value"), "{err}");

    // Rails writes `date.order: [:day, :month, :year]`, which is data.
    let p2 = Project::new("seqsymbols");
    p2.write(
        "config/locales/en.yml",
        "en:\n  date:\n    order:\n    - :day\n    - :month\n",
    );
    let cfg2 = p2.config(BASIC_CONFIG.replace("[en, es]", "[en]").as_str());
    let store = Store::load(&cfg2).unwrap();
    assert_eq!(
        store.tree("en").unwrap().get("date.order").unwrap().value,
        Value::Seq(vec![
            Value::Plain(":day".into()),
            Value::Plain(":month".into())
        ])
    );
}

#[test]
fn anchors_are_rejected_with_file_and_line() {
    let p = Project::new("anchors");
    p.write("config/locales/en.yml", "en:\n  a: &x A\n  b: *x\n");
    let cfg = p.config(BASIC_CONFIG.replace("[en, es]", "[en]").as_str());
    let err = Store::load(&cfg).unwrap_err();
    assert!(err.contains("en.yml:2"), "{err}");
    assert!(err.contains("anchors"), "{err}");
}

#[test]
fn external_keys_are_never_unused_and_never_missing() {
    // ref: lib/i18n/tasks/data.rb#external_key?
    let p = Project::new("external");
    p.write("config/locales/en.yml", "en:\n  own: Own\n")
        .write("config/locales/external/en.yml", "en:\n  ext: Ext\n")
        .write(
            "app/controllers/a_controller.rb",
            "class AController\n  def x\n    t('ext')\n  end\nend\n",
        );
    let cfg = p.config(
        "base_locale: en\nlocales: [en]\ndata:\n  read:\n    - config/locales/%{locale}.yml\n  external:\n    - config/locales/external/%{locale}.yml\nsearch:\n  paths: [app/]\n",
    );
    let store = Store::load(&cfg).unwrap();
    let used = UsedKeys::scan(&cfg).unwrap();
    // `ext` lives only in external data, so it is not missing.
    let m = missing::report(&cfg, &store, &used, &store.locales, &[MissingType::Used]);
    assert!(m.rows.is_empty(), "{:?}", m.rows);
    // And external data is never reported unused.
    let u = unused::report(&cfg, &store, &used, &store.locales);
    assert_eq!(
        u.rows.iter().map(|r| r.key.as_str()).collect::<Vec<_>>(),
        vec!["own"]
    );
}

#[test]
fn unused_covers_descendants_of_a_used_ancestor() {
    // ref: unused_keys.rb#key_used? and PR #721 — `t(:section)` covers
    // `section.item.title`.
    let p = Project::new("ancestor");
    p.write(
        "config/locales/en.yml",
        "en:\n  section:\n    item:\n      title: T\n  lonely: L\n",
    )
    .write(
        "app/controllers/a_controller.rb",
        "class AController\n  def x\n    t(:section)\n  end\nend\n",
    );
    let cfg = p.config(BASIC_CONFIG.replace("[en, es]", "[en]").as_str());
    let store = Store::load(&cfg).unwrap();
    let used = UsedKeys::scan(&cfg).unwrap();
    let u = unused::report(&cfg, &store, &used, &store.locales);
    assert_eq!(
        u.rows.iter().map(|r| r.key.as_str()).collect::<Vec<_>>(),
        vec!["lonely"]
    );
}

#[test]
fn unused_collapses_a_fully_unused_plural_node() {
    // ref: plural_keys.rb#collapse_plural_nodes!
    let p = Project::new("collapse");
    p.write(
        "config/locales/en.yml",
        "en:\n  apple:\n    one: one\n    other: many\n",
    );
    let cfg = p.config(BASIC_CONFIG.replace("[en, es]", "[en]").as_str());
    let store = Store::load(&cfg).unwrap();
    let used = UsedKeys::scan(&cfg).unwrap();
    let u = unused::report(&cfg, &store, &used, &store.locales);
    assert_eq!(
        u.rows.iter().map(|r| r.key.as_str()).collect::<Vec<_>>(),
        vec!["apple"]
    );
}

#[test]
fn missing_used_accepts_any_resolving_candidate_key() {
    // ref: missing_keys.rb:110-130. `t('.success')` in a controller yields the
    // candidates ["events.create.success", "events.success"]; either one being
    // present means the usage is not missing.
    let p = Project::new("candidates");
    p.write("config/locales/en.yml", "en:\n  events:\n    success: S\n")
        .write(
            "app/controllers/events_controller.rb",
            "class EventsController\n  def create\n    t('.success')\n  end\nend\n",
        );
    let cfg = p.config(
        "base_locale: en\nlocales: [en]\ndata:\n  read:\n    - config/locales/%{locale}.yml\nsearch:\n  paths: [app/]\n  relative_roots: [app/controllers]\n",
    );
    let store = Store::load(&cfg).unwrap();
    let used = UsedKeys::scan(&cfg).unwrap();
    let m = missing::report(&cfg, &store, &used, &store.locales, &[MissingType::Used]);
    assert!(m.rows.is_empty(), "{:?}", m.rows);
}

#[test]
fn missing_diff_runs_both_directions_when_base_is_in_scope() {
    // ref: missing_keys.rb#missing_diff_forest
    let p = Project::new("diff");
    p.write("config/locales/en.yml", "en:\n  only_en: E\n  both: B\n")
        .write("config/locales/es.yml", "es:\n  only_es: E\n  both: B\n");
    let cfg = p.config(BASIC_CONFIG);
    let store = Store::load(&cfg).unwrap();
    let used = UsedKeys::scan(&cfg).unwrap();
    let m = missing::report(&cfg, &store, &used, &store.locales, &[MissingType::Diff]);
    let mut got: Vec<String> = m
        .rows
        .iter()
        .map(|r| format!("{}.{}", r.locale, r.key))
        .collect();
    got.sort();
    assert_eq!(got, vec!["en.only_es", "es.only_en"]);
}

#[test]
fn missing_plural_uses_the_static_cldr_table() {
    // Blocker B7: no rails-i18n gem and no `eval`.
    let p = Project::new("plural");
    p.write(
        "config/locales/en.yml",
        "en:\n  apple:\n    one: one\n  pear:\n    one: one\n    other: many\n",
    );
    let cfg = p.config(BASIC_CONFIG.replace("[en, es]", "[en]").as_str());
    let store = Store::load(&cfg).unwrap();
    let used = UsedKeys::scan(&cfg).unwrap();
    let m = missing::report(&cfg, &store, &used, &store.locales, &[MissingType::Plural]);
    assert_eq!(m.rows.len(), 1);
    assert_eq!(m.rows[0].key, "apple");
    assert_eq!(
        m.rows[0].reason,
        Some(Reason::Plural {
            categories: vec!["other"]
        })
    );
}

#[test]
fn inconsistent_and_reserved_interpolations() {
    let p = Project::new("interp");
    p.write(
        "config/locales/en.yml",
        "en:\n  greet: \"Hi %{name}\"\n  same: \"%{a} %{b}\"\n  reserved: \"%{scope} here\"\n",
    )
    .write(
        "config/locales/es.yml",
        "es:\n  greet: \"Hola %{nombre}\"\n  same: \"%{b} %{a}\"\n  reserved: \"%{scope} aqui\"\n",
    );
    let cfg = p.config(BASIC_CONFIG);
    let store = Store::load(&cfg).unwrap();

    let inconsistent = interpolations::inconsistent(&cfg, &store, &store.locales);
    // Only `greet` differs. `same` uses the same variable set in both, just
    // reordered, and the check compares sets.
    assert_eq!(
        inconsistent
            .rows
            .iter()
            .map(|r| r.key.as_str())
            .collect::<Vec<_>>(),
        vec!["greet"]
    );
    assert_eq!(inconsistent.outcome(), Outcome::Found);

    let reserved = interpolations::reserved(&store, &store.locales);
    let mut keys: Vec<String> = reserved
        .rows
        .iter()
        .map(|r| format!("{}.{}", r.locale, r.key))
        .collect();
    keys.sort();
    assert_eq!(keys, vec!["en.reserved", "es.reserved"]);
}

#[test]
fn stats_match_the_gems_integer_division() {
    // ref: lib/i18n/tasks/stats.rb
    let p = Project::new("stats");
    p.write(
        "config/locales/en.yml",
        "en:\n  a:\n    b: \"1234\"\n  c: \"12345\"\n",
    )
    .write("config/locales/es.yml", "es:\n  a:\n    b: \"1\"\n");
    let cfg = p.config(BASIC_CONFIG);
    let store = Store::load(&cfg).unwrap();
    let stats = forest_stats(&store, &store.locales);
    assert_eq!(stats.key_count, 3);
    assert_eq!(stats.locale_count, 2);
    // 3 / 2 with integer division.
    assert_eq!(stats.per_locale_avg, 1);
    // Segments: 2 + 1 + 2 = 5, over 3 keys.
    assert_eq!(stats.key_segments_avg, "1.7");
    // Characters: 4 + 5 + 1 = 10, over 3 keys, integer division.
    assert_eq!(stats.value_chars_avg, 3);
    assert_eq!(stats.locales, "en, es");
}

#[test]
fn the_prefilter_skips_files_with_no_translation_call() {
    let p = Project::new("prefilter");
    p.write("config/locales/en.yml", "en:\n  a: A\n")
        .write(
            "app/controllers/a_controller.rb",
            "class AController\n  def x\n    t('a')\n  end\nend\n",
        )
        .write(
            "app/models/quiet.rb",
            "class Quiet\n  def x\n    1 + 1\n  end\nend\n",
        );
    let cfg = p.config(BASIC_CONFIG.replace("[en, es]", "[en]").as_str());
    let used = UsedKeys::scan(&cfg).unwrap();
    assert_eq!(used.files_prefiltered, 1);
    assert_eq!(used.files_scanned, 1);
}

#[test]
fn search_exclude_prunes_a_directory() {
    // ref: file_finder.rb:34-50
    let p = Project::new("exclude");
    p.write("config/locales/en.yml", "en:\n  a: A\n  b: B\n")
        .write("app/controllers/a_controller.rb", "t('a')\n")
        .write("app/webpack/thing.rb", "t('b')\n");
    let cfg = p.config(
        "base_locale: en\nlocales: [en]\ndata:\n  read:\n    - config/locales/%{locale}.yml\nsearch:\n  paths: [app/]\n  exclude: [app/webpack]\n",
    );
    let used = UsedKeys::scan(&cfg).unwrap();
    assert!(used.key_used("a"));
    assert!(!used.key_used("b"));
}

#[test]
fn health_refuses_an_empty_data_set() {
    // The gem raises `i18n_tasks.health.no_keys_detected`. A silent pass on an
    // empty data set is the worst possible outcome.
    let p = Project::new("empty");
    p.write("config/locales/en.yml", "en: {}\n");
    let cfg = p.config(BASIC_CONFIG.replace("[en, es]", "[en]").as_str());
    let store = Store::load(&cfg).unwrap();
    assert_eq!(forest_stats(&store, &store.locales).key_count, 0);
}

#[test]
fn a_missing_config_file_is_a_tool_failure() {
    let err = Config::load(Path::new("does/not/exist.yml"), None).unwrap_err();
    assert!(err.contains("cannot read config"));
}

/// Full port of the `#depluralize_key` group in spec/plural_keys_spec.rb.
#[test]
fn depluralize_key_matches_the_gem_spec() {
    use i18n_tasks_rs::plural::depluralize_key;
    let p = Project::new("depluralize");
    p.write(
        "config/locales/en.yml",
        "en:\n\
         \x20 regular_key: a\n\
         \x20 plural_key:\n\
         \x20   one: one\n\
         \x20   other: \"%{count}\"\n\
         \x20 not_really_plural:\n\
         \x20   one: a\n\
         \x20   green: b\n\
         \x20 explicit_0_1_rules:\n\
         \x20   '0': explicit zero\n\
         \x20   '1': explicit one\n\
         \x20   one: one\n\
         \x20   other: \"%{count}\"\n",
    );
    let cfg = p.config(BASIC_CONFIG.replace("[en, es]", "[en]").as_str());
    let store = Store::load(&cfg).unwrap();
    let en = store.tree("en").unwrap();
    let dep = |key: &str| depluralize_key(key, Some(en), Some(en));

    assert_eq!(dep("plural_key.one"), "plural_key");
    assert_eq!(dep("regular_key"), "regular_key");
    // A node whose children are not all plural categories is not a plural node.
    assert_eq!(dep("not_really_plural.one"), "not_really_plural.one");
    // `0` and `1` are plural suffixes but not CLDR categories, so a key that
    // ends in one is never depluralized.
    assert_eq!(dep("explicit_0_1_rules.0"), "explicit_0_1_rules.0");
    assert_eq!(dep("explicit_0_1_rules.1"), "explicit_0_1_rules.1");
    // They do count towards `plural_forms?`, so their siblings depluralize.
    assert_eq!(dep("explicit_0_1_rules.other"), "explicit_0_1_rules");
    // A category name with no parent at all stays as it is.
    assert_eq!(dep("one"), "one");
    // A parent no tree knows about stays as it is.
    assert_eq!(dep("absent.one"), "absent.one");
    // The base locale is consulted when the locale itself has nothing there.
    let empty = Store::load(&p.config(BASIC_CONFIG.replace("[en, es]", "[en]").as_str())).unwrap();
    assert_eq!(
        depluralize_key("plural_key.one", None, empty.tree("en")),
        "plural_key"
    );
}

/// Port of the `#missing_plural_forest` example in spec/plural_keys_spec.rb,
/// with `ar` for its six categories and `ignore_missing` for the filter.
#[test]
fn missing_plural_matches_the_gem_spec() {
    let base = r#"  regular_key: a
  plural_key:
    one: one
    other: "%{count}"
  not_really_plural:
    one: a
    green: b
  nested:
    plural_key:
      zero: none
      one: one
      other: "%{count}"
  ignored_pattern:
    plural_key:
      other: "%{count}"
  explicit_0_1_rules:
    '0': explicit zero
    '1': explicit one
    one: one
    other: "%{count}"
"#;
    let p = Project::new("missing-plural");
    p.write("config/locales/en.yml", &format!("en:\n{base}"))
        .write("config/locales/ar.yml", &format!("ar:\n{base}"));
    let cfg = p.config(
        "base_locale: en\nlocales: [en, ar]\ndata:\n  read:\n    - config/locales/%{locale}.yml\nsearch:\n  paths: [app/]\nignore_missing: [\"ignored_pattern.*\"]\n",
    );
    let store = Store::load(&cfg).unwrap();
    let used = UsedKeys::scan(&cfg).unwrap();
    let m = missing::report(&cfg, &store, &used, &store.locales, &[MissingType::Plural]);
    let rows: Vec<(String, String, String)> = m
        .rows
        .iter()
        .map(|r| (r.locale.clone(), r.key.clone(), r.details()))
        .collect();
    // `en` needs only one/other and has both everywhere, so nothing is
    // reported for it. `ignored_pattern.plural_key` is filtered out, and
    // `not_really_plural` is not a plural node at all.
    assert_eq!(
        rows,
        vec![
            (
                "ar".into(),
                "explicit_0_1_rules".into(),
                "plural: missing zero, two, few, many".into()
            ),
            (
                "ar".into(),
                "nested.plural_key".into(),
                "plural: missing two, few, many".into()
            ),
            (
                "ar".into(),
                "plural_key".into(),
                "plural: missing zero, two, few, many".into()
            ),
        ]
    );
}

/// A locale rails-i18n ships no pluralization for gets no plural check, which
/// is how the gem behaves when the file is missing.
#[test]
fn an_unknown_locale_gets_no_plural_check() {
    let p = Project::new("plural-unknown");
    p.write("config/locales/en.yml", "en:\n  apple:\n    one: one\n")
        .write("config/locales/zz.yml", "zz:\n  apple:\n    one: one\n");
    let cfg = p.config(
        "base_locale: en\nlocales: [en, zz]\ndata:\n  read:\n    - config/locales/%{locale}.yml\nsearch:\n  paths: [app/]\n",
    );
    let store = Store::load(&cfg).unwrap();
    let used = UsedKeys::scan(&cfg).unwrap();
    let m = missing::report(&cfg, &store, &used, &store.locales, &[MissingType::Plural]);
    assert!(m.rows.iter().all(|r| r.locale == "en"), "{:?}", m.rows);
}

/// ref: spec/ignore_missing_spec.rb, end to end through the report.
#[test]
fn ignore_missing_groups_apply_per_locale() {
    let p = Project::new("ignore-missing");
    p.write(
        "config/locales/en.yml",
        "en:\n\
         \x20 common:\n\
         \x20   ignored_for_all: Text\n\
         \x20   not_ignored: Text\n\
         \x20 specific:\n\
         \x20   ignored_for_es: Text\n\
         \x20   ignored_for_es_and_fr: Text\n",
    )
    .write("config/locales/es.yml", "es: {}\n")
    .write("config/locales/fr.yml", "fr: {}\n")
    .write("config/locales/de.yml", "de: {}\n");
    let cfg = p.config(
        "base_locale: en\n\
         locales: [en, es, fr, de]\n\
         data:\n\
         \x20 read:\n\
         \x20   - config/locales/%{locale}.yml\n\
         search:\n\
         \x20 paths: [app/]\n\
         ignore_missing:\n\
         \x20 all: [\"common.ignored_for_all\"]\n\
         \x20 es: [\"specific.ignored_for_es\"]\n\
         \x20 \"es,fr\": [\"specific.ignored_for_es_and_fr\"]\n",
    );
    let store = Store::load(&cfg).unwrap();
    let used = UsedKeys::scan(&cfg).unwrap();
    let m = missing::report(&cfg, &store, &used, &store.locales, &[MissingType::Diff]);
    let has = |locale: &str, key: &str| m.rows.iter().any(|r| r.locale == locale && r.key == key);
    // `all` applies to every locale.
    assert!(!has("es", "common.ignored_for_all"));
    assert!(!has("fr", "common.ignored_for_all"));
    assert!(has("es", "common.not_ignored"));
    assert!(has("fr", "common.not_ignored"));
    // A single-locale group applies to that locale only.
    assert!(!has("es", "specific.ignored_for_es"));
    assert!(has("fr", "specific.ignored_for_es"));
    // A comma group is split and matched exactly, so `de` is unaffected.
    assert!(!has("es", "specific.ignored_for_es_and_fr"));
    assert!(!has("fr", "specific.ignored_for_es_and_fr"));
    assert!(has("de", "specific.ignored_for_es_and_fr"));
}

/// `ignore_unused` and the derived key patterns both take a key out of the
/// `unused` report, and each does so for its own reason.
#[test]
fn unused_honours_ignore_unused_and_the_derived_patterns() {
    let p = Project::new("unused-filters");
    p.write(
        "config/locales/en.yml",
        "en:\n\
         \x20 devise:\n\
         \x20   sessions: Sign in\n\
         \x20 categories:\n\
         \x20   details:\n\
         \x20     roofing:\n\
         \x20       footer: Roofs\n\
         \x20 plain: Plain\n\
         \x20 used: Used\n",
    )
    .write(
        "app/controllers/a_controller.rb",
        "t('used')\nI18n.t(\"categories.details.#{code}.footer\")\n",
    );
    let cfg = p.config(
        "base_locale: en\nlocales: [en]\ndata:\n  read:\n    - config/locales/%{locale}.yml\nsearch:\n  paths: [app/]\nignore_unused: [\"devise.*\"]\n",
    );
    let store = Store::load(&cfg).unwrap();
    let used = UsedKeys::scan(&cfg).unwrap();
    let report = unused::report(&cfg, &store, &used, &store.locales);
    let keys: Vec<&str> = report.rows.iter().map(|r| r.key.as_str()).collect();
    // `devise.sessions` is ignored, the interpolated key protects
    // `categories.details.roofing.footer`, and `used` is used outright.
    assert_eq!(keys, vec!["plain"]);
}

/// `ignore_inconsistent_interpolations` takes a key out of that check.
#[test]
fn inconsistent_interpolations_honours_its_ignore_list() {
    let p = Project::new("ignore-interp");
    p.write(
        "config/locales/en.yml",
        "en:\n  ignored: \"Hi %{name}\"\n  reported: \"Hi %{name}\"\n  novars: plain\n",
    )
    .write(
        "config/locales/es.yml",
        "es:\n  ignored: \"Hola\"\n  reported: \"Hola\"\n  novars: \"%{x}\"\n",
    );
    let cfg = p.config(
        "base_locale: en\nlocales: [en, es]\ndata:\n  read:\n    - config/locales/%{locale}.yml\nsearch:\n  paths: [app/]\nignore_inconsistent_interpolations: [\"ignored\"]\n",
    );
    let store = Store::load(&cfg).unwrap();
    let r = interpolations::inconsistent(&cfg, &store, &store.locales);
    let rows: Vec<(&str, String)> = r
        .rows
        .iter()
        .map(|row| (row.key.as_str(), row.details()))
        .collect();
    assert_eq!(
        rows,
        vec![
            ("novars", "es has %{x}, en has no variables".to_string()),
            (
                "reported",
                "es has no variables, en has %{name}".to_string()
            ),
        ]
    );
    // The prose above is rendered from a tagged reason, not stored as one.
    assert_eq!(
        r.rows[0].reason,
        Some(Reason::Interpolations {
            variables: vec!["%{x}".into()],
            base_locale: "en".into(),
            base_variables: vec![],
        })
    );
}

/// A value that is not a String is skipped by both interpolation checks: the
/// gem tests `value.is_a?(String)`, so a number or a list never matches.
#[test]
fn interpolation_checks_skip_non_string_values() {
    let p = Project::new("interp-nonstring");
    p.write(
        "config/locales/en.yml",
        "en:\n  n: 1\n  seq:\n    - \"%{scope}\"\n  nil_value:\n  s: \"%{scope}\"\n",
    )
    .write(
        "config/locales/es.yml",
        "es:\n  n: 2\n  seq:\n    - \"%{other}\"\n  nil_value:\n  s: \"%{scope}\"\n",
    );
    let cfg = p.config(BASIC_CONFIG);
    let store = Store::load(&cfg).unwrap();
    let reserved = interpolations::reserved(&store, &store.locales);
    // Only the plain String value is checked; the sequence is not.
    let keys: Vec<&str> = reserved.rows.iter().map(|r| r.key.as_str()).collect();
    assert_eq!(keys, vec!["s", "s"]);
    let inconsistent = interpolations::inconsistent(&cfg, &store, &store.locales);
    assert!(inconsistent.rows.is_empty(), "{:?}", inconsistent.rows);
}

/// A locale the store holds no tree for is skipped by every report rather than
/// crashing or being counted. A configured locale always gets a tree, even an
/// empty one, so this is the case of a report asked for a locale outside the
/// configured set.
#[test]
fn a_locale_with_no_tree_is_skipped_by_every_report() {
    let p = Project::new("no-tree");
    p.write("config/locales/en.yml", "en:\n  a: A\n");
    let cfg = p.config(BASIC_CONFIG.replace("[en, es]", "[en]").as_str());
    let store = Store::load(&cfg).unwrap();
    assert!(store.tree("zz").is_none());
    let locales = vec!["zz".to_string()];
    let used = UsedKeys::scan(&cfg).unwrap();
    assert!(
        unused::report(&cfg, &store, &used, &locales)
            .rows
            .is_empty()
    );
    assert!(interpolations::reserved(&store, &locales).rows.is_empty());
    assert_eq!(forest_stats(&store, &locales).key_count, 0);
    let m = missing::report(&cfg, &store, &used, &locales, &[MissingType::Plural]);
    assert!(m.rows.is_empty());
    // `missing --types diff` still reports what the base locale holds, because
    // that side reads the base tree, not the locale's.
    let m = missing::report(&cfg, &store, &used, &locales, &[MissingType::Diff]);
    assert_eq!(m.rows.len(), 1, "{:?}", m.rows);
    assert_eq!(m.rows[0].key, "a");
}

/// A configured locale with no file on disk gets an empty tree, and `diff`
/// reports every base key against it.
#[test]
fn a_configured_locale_with_no_file_gets_an_empty_tree() {
    let p = Project::new("empty-locale");
    p.write("config/locales/en.yml", "en:\n  a: A\n");
    let cfg = p.config(
        "base_locale: en\nlocales: [en, zz]\ndata:\n  read:\n    - config/locales/%{locale}.yml\nsearch:\n  paths: [app/]\n",
    );
    let store = Store::load(&cfg).unwrap();
    assert_eq!(store.tree("zz").unwrap().sorted_keys().len(), 0);
    let used = UsedKeys::scan(&cfg).unwrap();
    let m = missing::report(&cfg, &store, &used, &store.locales, &[MissingType::Diff]);
    assert_eq!(m.rows.len(), 1, "{:?}", m.rows);
    assert_eq!(
        (m.rows[0].locale.as_str(), m.rows[0].key.as_str()),
        ("zz", "a")
    );
}

/// A base locale with no data at all leaves the consistency check empty.
#[test]
fn inconsistent_interpolations_needs_a_base_tree() {
    let p = Project::new("no-base");
    p.write("config/locales/es.yml", "es:\n  a: \"%{x}\"\n");
    let cfg = p.config(
        "base_locale: en\nlocales: [en, es]\ndata:\n  read:\n    - config/locales/%{locale}.yml\nsearch:\n  paths: [app/]\n",
    );
    let store = Store::load(&cfg).unwrap();
    let r = interpolations::inconsistent(&cfg, &store, &store.locales);
    assert!(r.rows.is_empty());
    assert_eq!(r.outcome(), Outcome::Clean);
}

/// Only a plural node whose every child is unused collapses to the parent.
///
/// A *used* plural child cannot keep the node alive: `depluralize_key` turns
/// `apple.one` into `apple`, and the used set holds `apple.one`, so the gem
/// reports the node unused too. An `ignore_unused` rule is what leaves one
/// child standing.
#[test]
fn a_partly_ignored_plural_node_does_not_collapse() {
    let p = Project::new("partial-plural");
    p.write(
        "config/locales/en.yml",
        "en:\n  apple:\n    one: one\n    other: many\n  pear: Pear\n",
    )
    .write("app/controllers/a_controller.rb", "t('apple.one')\n");
    let cfg = p.config(
        "base_locale: en\nlocales: [en]\ndata:\n  read:\n    - config/locales/%{locale}.yml\nsearch:\n  paths: [app/]\nignore_unused: [\"apple.one\"]\n",
    );
    let store = Store::load(&cfg).unwrap();
    let used = UsedKeys::scan(&cfg).unwrap();
    let report = unused::report(&cfg, &store, &used, &store.locales);
    let keys: Vec<&str> = report.rows.iter().map(|r| r.key.as_str()).collect();
    // `apple.other` is reported on its own, not as `apple`, because `apple.one`
    // never entered the hit list. `pear` has no plural parent at all.
    assert_eq!(keys, vec!["apple.other", "pear"]);
}

/// Without the ignore rule the whole node collapses, even though one form is
/// used: this is the gem's behaviour, not a port artefact.
#[test]
fn a_used_plural_form_does_not_keep_the_node_alive() {
    let p = Project::new("plural-used-form");
    p.write(
        "config/locales/en.yml",
        "en:\n  apple:\n    one: one\n    other: many\n",
    )
    .write("app/controllers/a_controller.rb", "t('apple.one')\n");
    let cfg = p.config(BASIC_CONFIG.replace("[en, es]", "[en]").as_str());
    let store = Store::load(&cfg).unwrap();
    let used = UsedKeys::scan(&cfg).unwrap();
    let report = unused::report(&cfg, &store, &used, &store.locales);
    let keys: Vec<&str> = report.rows.iter().map(|r| r.key.as_str()).collect();
    assert_eq!(keys, vec!["apple"]);
}

/// An interior node is a key for `key_value?` but never a leaf.
#[test]
fn an_interior_node_is_a_key_without_being_a_leaf() {
    let p = Project::new("interior");
    p.write("config/locales/en.yml", "en:\n  a:\n    b: B\n");
    let cfg = p.config(BASIC_CONFIG.replace("[en, es]", "[en]").as_str());
    let store = Store::load(&cfg).unwrap();
    let en = store.tree("en").unwrap();
    assert!(en.is_interior("a"));
    assert!(!en.is_interior("a.b"));
    assert!(en.has_key("a"));
    assert!(en.has_key("a.b"));
    assert!(!en.has_key("a.c"));
    assert!(en.get("a").is_none());
    assert!(en.get("a.b").is_some());
    // ref: missing_keys.rb#locale_key_missing? goes through `key_value?`,
    // which is true for an interior node too.
    assert!(store.key_value("en", "a"));
}

/// ref: spec/file_system_data_spec.rb "#get" — a broken file names itself.
#[test]
fn a_broken_locale_file_names_itself() {
    let p = Project::new("broken-yaml");
    p.write("config/locales/en.yml", "en:\n  a: 1\n  %bad\n");
    let cfg = p.config(BASIC_CONFIG.replace("[en, es]", "[en]").as_str());
    let e = Store::load(&cfg).unwrap_err();
    assert!(e.contains("config/locales/en.yml"), "{e}");
    assert!(e.contains("YAML syntax error"), "{e}");
}

/// A locale file the parser reads but that holds no mapping at the top.
#[test]
fn a_locale_file_that_is_not_a_mapping_is_an_error() {
    let p = Project::new("not-a-mapping");
    p.write("config/locales/en.yml", "- a\n- b\n");
    let cfg = p.config(BASIC_CONFIG.replace("[en, es]", "[en]").as_str());
    let e = Store::load(&cfg).unwrap_err();
    assert!(e.contains("expected a mapping at the top level"), "{e}");
}

/// An empty locale file contributes nothing and is not an error, because the
/// gem's `load_file(path) || {}` does the same.
#[test]
fn an_empty_locale_file_is_skipped() {
    let p = Project::new("empty-file");
    p.write("config/locales/base.en.yml", "en:\n  a: A\n")
        .write("config/locales/other.en.yml", "# only a comment\n");
    let cfg = p.config(
        "base_locale: en\nlocales: [en]\ndata:\n  read:\n    - config/locales/*.%{locale}.yml\nsearch:\n  paths: [app/]\n",
    );
    let store = Store::load(&cfg).unwrap();
    assert_eq!(store.tree("en").unwrap().sorted_keys().len(), 1);
}

/// A YAML key that is not a scalar cannot be a key segment.
#[test]
fn a_non_scalar_yaml_key_is_an_error() {
    let p = Project::new("odd-key");
    p.write("config/locales/en.yml", "en:\n  ? [a, b]\n  : value\n");
    let cfg = p.config(BASIC_CONFIG.replace("[en, es]", "[en]").as_str());
    let e = Store::load(&cfg).unwrap_err();
    assert!(e.contains("non-scalar YAML key"), "{e}");
    // The same inside a sequence, which `to_value` walks separately.
    let p = Project::new("odd-key-seq");
    p.write(
        "config/locales/en.yml",
        "en:\n  list:\n    - ? [a, b]\n      : value\n",
    );
    let cfg = p.config(BASIC_CONFIG.replace("[en, es]", "[en]").as_str());
    let e = Store::load(&cfg).unwrap_err();
    assert!(e.contains("non-scalar YAML key"), "{e}");
}

/// A `data.read` entry with no `%{locale}` in it is read for every locale, and
/// contributes nothing to locale inference. ref: file_system_base.rb:120.
#[test]
fn a_read_pattern_without_the_locale_placeholder_names_no_locale() {
    let p = Project::new("no-placeholder");
    p.write("config/locales/en.yml", "en:\n  a: A\n")
        .write("config/locales/global.defaults.yml", "en:\n  shared: S\n");
    let cfg = p.config(
        "base_locale: en\ndata:\n  read:\n    - config/locales/global.defaults.yml\n    - config/locales/%{locale}.yml\nsearch:\n  paths: [app/]\n",
    );
    let store = Store::load(&cfg).unwrap();
    // The placeholder-less pattern names no locale, so only `en` is inferred,
    // and the keys it holds still land in the `en` tree.
    assert_eq!(store.locales, vec!["en"]);
    assert!(store.key_value("en", "shared"));
}

/// A `**` in a `data.read` glob crosses directories.
#[test]
fn a_double_star_read_glob_crosses_directories() {
    let p = Project::new("double-star");
    p.write("config/locales/a/b/deep.en.yml", "en:\n  deep: D\n")
        .write("config/locales/top.en.yml", "en:\n  top: T\n");
    let cfg = p.config(
        "base_locale: en\ndata:\n  read:\n    - config/locales/**/*.%{locale}.yml\nsearch:\n  paths: [app/]\n",
    );
    let store = Store::load(&cfg).unwrap();
    assert_eq!(store.locales, vec!["en"]);
    assert!(store.key_value("en", "deep"));
    assert!(store.key_value("en", "top"));
}

/// With no `locales` in the config and nothing on disk, the tool says so
/// rather than reporting a clean run over an empty data set.
#[test]
fn no_locale_data_at_all_is_an_error() {
    let p = Project::new("no-data");
    let cfg = p.config(
        "base_locale: en\ndata:\n  read:\n    - config/locales/%{locale}.yml\nsearch:\n  paths: [app/]\n",
    );
    let e = Store::load(&cfg).unwrap_err();
    assert!(e.contains("no locale data found"), "{e}");
    assert!(e.contains("config/locales/%{locale}.yml"), "{e}");
}

/// A key that lives in both the main data and the external data is never
/// reported unused, even though the main tree holds it.
#[test]
fn a_key_that_is_also_external_is_never_unused() {
    let p = Project::new("external-overlap");
    p.write("config/locales/en.yml", "en:\n  shared: S\n  own: Own\n")
        .write("config/locales/external/en.yml", "en:\n  shared: S\n");
    let cfg = p.config(
        "base_locale: en\nlocales: [en]\ndata:\n  read:\n    - config/locales/%{locale}.yml\n  external:\n    - config/locales/external/%{locale}.yml\nsearch:\n  paths: [app/]\n",
    );
    let store = Store::load(&cfg).unwrap();
    let used = UsedKeys::scan(&cfg).unwrap();
    let u = unused::report(&cfg, &store, &used, &store.locales);
    assert_eq!(
        u.rows.iter().map(|r| r.key.as_str()).collect::<Vec<_>>(),
        vec!["own"]
    );
}

/// A nested key whose parent is an ordinary node is reported as it stands.
#[test]
fn a_nested_unused_key_keeps_its_full_name() {
    let p = Project::new("nested-unused");
    p.write("config/locales/en.yml", "en:\n  a:\n    b: B\n    c: C\n");
    let cfg = p.config(BASIC_CONFIG.replace("[en, es]", "[en]").as_str());
    let store = Store::load(&cfg).unwrap();
    let used = UsedKeys::scan(&cfg).unwrap();
    let u = unused::report(&cfg, &store, &used, &store.locales);
    assert_eq!(
        u.rows.iter().map(|r| r.key.as_str()).collect::<Vec<_>>(),
        vec!["a.b", "a.c"]
    );
}

/// A used key with no candidate keys — the ordinary absolute-key case — is
/// looked up under its own name.
#[test]
fn missing_used_reports_a_key_with_no_candidates() {
    let p = Project::new("missing-plain");
    p.write("config/locales/en.yml", "en:\n  present: P\n")
        .write("app/a.rb", "t('present')\nt('absent')\n");
    let cfg = p.config(BASIC_CONFIG.replace("[en, es]", "[en]").as_str());
    let store = Store::load(&cfg).unwrap();
    let used = UsedKeys::scan(&cfg).unwrap();
    let m = missing::report(&cfg, &store, &used, &store.locales, &[MissingType::Used]);
    assert_eq!(
        m.rows.iter().map(|r| r.key.as_str()).collect::<Vec<_>>(),
        vec!["absent"]
    );
    assert_eq!(
        m.rows[0].reason,
        Some(Reason::Used {
            path: "app/a.rb".into(),
            line: 2
        })
    );
}

/// A store with no tree for the base locale. `Store::load` always builds one,
/// because `normalize_locale_list` prepends the base, so this is the defensive
/// path: a caller that hands the reports a locale set the store does not cover
/// gets empty output rather than a panic.
#[test]
fn reports_survive_a_store_with_no_base_tree() {
    use std::collections::HashMap;
    let p = Project::new("no-base-tree");
    let cfg = p.config(BASIC_CONFIG);
    let store = Store {
        base_locale: "de".into(),
        locales: vec!["de".into(), "es".into()],
        trees: HashMap::new(),
        external: HashMap::new(),
        warnings: Vec::new(),
    };
    let used = UsedKeys::scan(&cfg).unwrap();
    let r = interpolations::inconsistent(&cfg, &store, &store.locales);
    assert!(r.rows.is_empty());
    assert_eq!(r.outcome(), Outcome::Clean);
    for ty in MissingType::ALL {
        let m = missing::report(&cfg, &store, &used, &store.locales, &[ty]);
        assert!(m.rows.is_empty(), "{ty:?}: {:?}", m.rows);
    }
    assert!(
        unused::report(&cfg, &store, &used, &store.locales)
            .rows
            .is_empty()
    );
    assert_eq!(forest_stats(&store, &store.locales).key_count, 0);
}

/// The consistency check skips a locale it has no tree for, a key the locale
/// does not hold, and a value that is not a String on either side.
#[test]
fn the_consistency_check_skips_what_it_cannot_compare() {
    let p = Project::new("interp-skips");
    p.write(
        "config/locales/en.yml",
        "en:\n  only_here: \"%{a}\"\n  differs: \"%{a}\"\n  nonstring: \"%{a}\"\n  nil_here: \"%{a}\"\n",
    )
    .write(
        "config/locales/es.yml",
        "es:\n  differs: \"%{b}\"\n  nonstring: 1\n  nil_here:\n",
    );
    let cfg = p.config(
        "base_locale: en\nlocales: [en, es]\ndata:\n  read:\n    - config/locales/%{locale}.yml\nsearch:\n  paths: [app/]\n",
    );
    let store = Store::load(&cfg).unwrap();
    // A locale with no tree at all is skipped.
    let locales = vec!["en".to_string(), "es".to_string(), "zz".to_string()];
    let r = interpolations::inconsistent(&cfg, &store, &locales);
    assert_eq!(
        r.rows
            .iter()
            .map(|row| row.key.as_str())
            .collect::<Vec<_>>(),
        vec!["differs"]
    );
}

/// A locale rails-i18n knows, but that the store holds no tree for, gets no
/// plural check.
#[test]
fn missing_plural_skips_a_locale_with_no_tree() {
    let p = Project::new("plural-no-tree");
    p.write("config/locales/en.yml", "en:\n  apple:\n    one: one\n");
    let cfg = p.config(BASIC_CONFIG.replace("[en, es]", "[en]").as_str());
    let store = Store::load(&cfg).unwrap();
    assert!(store.tree("fr").is_none());
    let used = UsedKeys::scan(&cfg).unwrap();
    let m = missing::report(
        &cfg,
        &store,
        &used,
        &["fr".to_string()],
        &[MissingType::Plural],
    );
    assert!(m.rows.is_empty());
}

/// A locale whose value is a scalar rather than a mapping contributes nothing.
#[test]
fn a_locale_whose_value_is_a_scalar_contributes_nothing() {
    let p = Project::new("scalar-locale");
    p.write("config/locales/base.en.yml", "en: just a string\n")
        .write("config/locales/other.en.yml", "en:\n  a: A\n");
    let cfg = p.config(
        "base_locale: en\nlocales: [en]\ndata:\n  read:\n    - config/locales/*.%{locale}.yml\nsearch:\n  paths: [app/]\n",
    );
    let store = Store::load(&cfg).unwrap();
    assert_eq!(
        store
            .tree("en")
            .unwrap()
            .sorted_keys()
            .iter()
            .map(|l| l.key.as_str())
            .collect::<Vec<_>>(),
        vec!["a"]
    );
}

/// A `data.read` pattern the gem's escaping cannot turn into a regex is skipped
/// for locale inference, and the patterns beside it still work.
#[test]
fn an_uncompilable_read_pattern_does_not_stop_locale_inference() {
    let p = Project::new("bad-read-pattern");
    p.write("config/locales/en.yml", "en:\n  a: A\n");
    let cfg = p.config(
        "base_locale: en\ndata:\n  read:\n    - \"config/locales/[%{locale}.yml\"\n    - config/locales/%{locale}.yml\nsearch:\n  paths: [app/]\n",
    );
    let store = Store::load(&cfg).unwrap();
    assert_eq!(store.locales, vec!["en"]);
}

/// A directory the process cannot read is stepped over rather than aborting
/// the glob or the walk. Skipped when the chmod does not take, which is what
/// happens when the tests run as root.
#[test]
fn an_unreadable_directory_is_stepped_over() {
    use std::os::unix::fs::PermissionsExt;
    let p = Project::new("unreadable");
    p.write("config/locales/ok/a.en.yml", "en:\n  a: A\n")
        .write("config/locales/locked/b.en.yml", "en:\n  b: B\n")
        .write("app/ok/a.rb", "t('a')\n")
        .write("app/locked/b.rb", "t('b')\n");
    let locked_data = p.root.join("config/locales/locked");
    let locked_src = p.root.join("app/locked");
    for dir in [&locked_data, &locked_src] {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o000)).unwrap();
    }
    let readable = std::fs::read_dir(&locked_data).is_ok();
    let cfg = p.config(
        "base_locale: en\nlocales: [en]\ndata:\n  read:\n    - config/locales/**/*.%{locale}.yml\nsearch:\n  paths: [app/]\n",
    );
    let store = Store::load(&cfg);
    let used = UsedKeys::scan(&cfg);
    // Restore the permissions before any assertion, so the temp dir can be
    // removed however this ends.
    for dir in [&locked_data, &locked_src] {
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755));
    }
    if readable {
        eprintln!("skipped: the chmod did not take effect");
        return;
    }
    let store = store.expect("the readable half still loads");
    assert!(store.key_value("en", "a"));
    assert!(!store.key_value("en", "b"));
    let used = used.expect("the readable half still scans");
    assert!(used.key_used("a"));
    assert!(!used.key_used("b"));
}

/// A file the process cannot read contributes no keys, and the scan of the rest
/// carries on.
#[test]
fn an_unreadable_source_file_is_stepped_over() {
    use std::os::unix::fs::PermissionsExt;
    let p = Project::new("unreadable-file");
    p.write("config/locales/en.yml", "en:\n  a: A\n  b: B\n")
        .write("app/ok.rb", "t('a')\n")
        .write("app/locked.rb", "t('b')\n");
    let locked = p.root.join("app/locked.rb");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let readable = std::fs::read(&locked).is_ok();
    let cfg = p.config(BASIC_CONFIG.replace("[en, es]", "[en]").as_str());
    let used = UsedKeys::scan(&cfg);
    let _ = std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o644));
    if readable {
        eprintln!("skipped: the chmod did not take effect");
        return;
    }
    let used = used.expect("the readable half still scans");
    assert!(used.key_used("a"));
    assert!(!used.key_used("b"));
}

/// An occurrence with no candidate keys is looked up under the key itself.
/// Both scanners always fill the candidate list in, so this documents the
/// fallback in `missing_used` rather than a shape the scanners produce.
#[test]
fn missing_used_falls_back_to_the_key_itself() {
    use i18n_tasks_rs::scan::Occurrence;
    use i18n_tasks_rs::used::UsedKeys;
    use std::collections::BTreeMap;
    let p = Project::new("no-candidates");
    p.write("config/locales/en.yml", "en:\n  present: P\n");
    let cfg = p.config(BASIC_CONFIG.replace("[en, es]", "[en]").as_str());
    let store = Store::load(&cfg).unwrap();
    let occ = |key: &str| Occurrence {
        path: std::sync::Arc::from(Path::new("app/a.rb")),
        snippet: format!("t('{key}')"),
        pos: 0,
        line_pos: 0,
        line_num: 7,
        raw_key: key.to_string(),
        candidate_keys: Vec::new(),
    };
    let mut keys = BTreeMap::new();
    keys.insert("present".to_string(), vec![occ("present")]);
    keys.insert("absent".to_string(), vec![occ("absent")]);
    let used = UsedKeys {
        keys,
        patterns: PatternSet::default(),
        pattern_sources: Vec::new(),
        opaque: Vec::new(),
        files_scanned: 1,
        files_prefiltered: 0,
    };
    let m = missing::report(&cfg, &store, &used, &store.locales, &[MissingType::Used]);
    assert_eq!(
        m.rows.iter().map(|r| r.key.as_str()).collect::<Vec<_>>(),
        vec!["absent"]
    );
    assert_eq!(
        m.rows[0].reason,
        Some(Reason::Used {
            path: "app/a.rb".into(),
            line: 7
        })
    );
}

/// ref: accepted difference 4b. There is no JSON adapter, so a `.json` locale
/// file is parsed as YAML. Flat JSON is YAML flow style, so reading works, but
/// the emitter would write YAML back into the `.json` name.
#[test]
fn a_json_locale_file_is_read_as_yaml() {
    use i18n_tasks_rs::report::normalize;
    let p = Project::new("json-data");
    p.write(
        "config/locales/en.json",
        "{\"en\": {\"a\": \"A\", \"nested\": {\"b\": \"B\"}}}",
    );
    let cfg = p.config(
        "base_locale: en\nlocales: [en]\ndata:\n  read:\n    - config/locales/%{locale}.json\nsearch:\n  paths: [app/]\n",
    );
    let store = Store::load(&cfg).unwrap();
    assert!(store.key_value("en", "a"));
    assert!(store.key_value("en", "nested.b"));

    // The write path would convert the file rather than re-emit JSON.
    let plan = normalize::plan(&cfg, &store, &store.locales, false).unwrap();
    assert_eq!(plan.changes.len(), 1);
    assert_eq!(
        plan.changes[0].after,
        "---\nen:\n  a: A\n  nested:\n    b: B\n"
    );
    assert_eq!(plan.outcome(), Outcome::Found);
}
