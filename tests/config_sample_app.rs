//! A realistic project config must load.
//!
//! `tests/fixtures/sample_app/i18n-tasks-rs.yml` is modelled on a production
//! Rails config: two overlapping `data.read` globs, a pattern router with a
//! wide alternation, per-type ignore lists and a global `ignore`.

use i18n_tasks_rs::config::{Config, IgnoreType};
use std::path::{Path, PathBuf};

#[test]
fn loads_the_sample_app_config() {
    let path = Path::new("tests/fixtures/sample_app/i18n-tasks-rs.yml");
    let src = std::fs::read_to_string(path).expect("fixture is checked in");
    let cfg = Config::parse(&src, path, PathBuf::from(".")).expect("config parses");

    assert_eq!(cfg.base_locale, "de");
    assert_eq!(cfg.locales.as_ref().unwrap(), &["de", "en", "fr"]);
    assert_eq!(cfg.data.read.len(), 2);

    // The pattern router rule and its fall-through.
    assert_eq!(cfg.data.write.len(), 2);
    let rule = &cfg.data.write[0];
    assert!(
        rule.pattern
            .as_deref()
            .unwrap()
            .starts_with("{activemodel, activerecord,")
    );
    assert_eq!(rule.path, "config/locales/\\1.%{locale}.yml");
    assert_eq!(cfg.data.write[1].path, "config/locales/base.%{locale}.yml");

    // relative_roots includes the two directories the gem's Prism path ignores.
    assert!(cfg.search.relative_roots.contains(&"app/forms".to_string()));
    assert!(
        cfg.search
            .relative_roots
            .contains(&"app/presenters".to_string())
    );

    // Global `ignore` reaches every type.
    let unused = cfg.ignore_patterns(IgnoreType::Unused, Some("de"));
    assert!(unused.is_match("errors.messages.blank"));
    assert!(unused.is_match("simple_form.labels.x"));
    assert!(unused.is_match("activerecord.models.user"));
    assert!(unused.is_match("categories.details.plumbing.footer_text_html"));
    assert!(!unused.is_match("jobs.index.title"));

    let missing = cfg.ignore_patterns(IgnoreType::Missing, Some("fr"));
    assert!(missing.is_match("legal_notices.anything"));
    assert!(missing.is_match("errors.messages.blank"));
    assert!(!missing.is_match("devise.sessions.new.title"));
}
