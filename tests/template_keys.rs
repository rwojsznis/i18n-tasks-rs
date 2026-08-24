//! The regex scanner over Slim and JS, driven by the gem's own fixtures.
//!
//! Ported from spec/used_keys_slim_spec.rb, spec/pattern_scanner_spec.rb and
//! spec/pattern_with_scope_scanner_spec.rb. The unit tests in
//! `src/scan/template.rs` cover those two scanner specs case by case; this file
//! covers whole fixture files, which is what the differential harness compares.

use i18n_tasks_rs::scan::{FileScan, ScanConfig, scan_file};
use std::path::Path;

fn cfg() -> ScanConfig {
    ScanConfig {
        relative_roots: vec![
            "app/controllers".into(),
            "app/helpers".into(),
            "app/mailers".into(),
            "app/presenters".into(),
            "app/views".into(),
        ],
        relative_exclude_method_name_paths: vec![],
    }
}

fn scan_fixture(rel: &str) -> FileScan {
    let path = Path::new("tests/fixtures").join(rel);
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    scan_file(&bytes, Path::new(rel), &cfg())
}

fn keys(scan: &FileScan) -> Vec<String> {
    let mut out: Vec<String> = scan.keys.iter().map(|(k, _)| k.clone()).collect();
    out.sort();
    out.dedup();
    out
}

/// ref: spec/used_keys_slim_spec.rb
///
/// The gem finds three leaves, not four: `Siblings.from_key_occurrences`
/// overwrites `c.layer` with `c.layer.underneath_c`. The port keeps both — see
/// accepted diff 5.
#[test]
fn slim_spec_source() {
    let src = concat!(
        "div = t 'a'\n",
        "  p = t 'a'\n",
        "h1 = t 'b'\n",
        "h2 = t 'c.layer'\n",
        "h3 = t 'c.layer.underneath_c'\n",
        "// Do not match non \\w characters before the t\n",
        "// https://github.com/glebm/i18n-tasks/issues/526\n",
        "| À bientôt !\n",
    );
    let scan = scan_file(src.as_bytes(), Path::new("a.html.slim"), &cfg());
    assert_eq!(
        keys(&scan),
        vec!["a", "b", "c.layer", "c.layer.underneath_c"]
    );
    let lines: Vec<usize> = scan
        .keys
        .iter()
        .filter(|(k, _)| k == "a")
        .map(|(_, o)| o.line_num)
        .collect();
    assert_eq!(lines, vec![1, 2]);
    // `bientôt !` must not read as a `t` call. ref: issue #526.
    assert!(scan.opaque.is_empty());
    assert!(scan.patterns.is_empty());
}

/// The differential harness fixture, whose 43 lines are a catalogue of the
/// scanner's edge cases.
#[test]
fn fixture_app_index_slim() {
    let scan = scan_fixture("app/views/index.html.slim");
    assert_eq!(
        keys(&scan),
        vec![
            "array.with_nil",
            "blank_in_es.a",
            "ca.a",
            "ca.b",
            "ca.c",
            "ca.d",
            "ca.e",
            "ca.f",
            "devise.a",
            "emoji.smile",
            "fn_comment",
            "ignore.a",
            "ignore_eq_base_all.a",
            "ignore_eq_base_es.a",
            "ignored_missing_key.a",
            "ignored_pattern.some_key",
            "latin_extra.çüéö",
            "missing-key-question?.key",
            "missing-key-with-a-dash.key",
            "missing_in_es.a",
            "missing_in_es_plural_1.a",
            "missing_in_es_plural_2.a",
            "missing_key_ending_in_colon.key:",
            "missing_symbol.key_three",
            "missing_symbol.key_two",
            "missing_symbol_key",
            "not_a_comment",
            "numeric.a",
            "only_in_es",
            "plural.a",
            "present_in_es_but_not_en.a",
            "reference-missing-target.a",
            "reference-ok-nested.a",
            "reference-ok-plain",
            "same_in_es.a",
            "scope.subscope.a.b",
            "scoped.x",
            "used_but_missing.key",
            "very.scoped.x",
        ]
    );
    for dropped in [
        // A quote or a dash before the `t`. ref: pattern_scanner.rb:15.
        "fp_quote_before",
        "fp_dash_before",
        // A Slim comment line, without a magic comment on it.
        "fp_comment",
        // A receiver that is not `I18n`.
        "not_a_key",
        // `scope: [:scoped, code]` is not an array of literals.
        // `scope: [:search, params[:action]]` holds a nested bracket.
        "ignored_in_strict_mode",
    ] {
        assert!(
            !keys(&scan).iter().any(|k| k.contains(dropped)),
            "{dropped} should be dropped"
        );
    }
}

#[test]
fn fixture_app_relative_slim() {
    let scan = scan_fixture("app/views/relative/index.html.slim");
    assert_eq!(
        keys(&scan),
        vec![
            "relative.index.description",
            "relative.index.missing",
            "relative.index.summary",
            "relative.index.title",
            // `t(".title", scope: "scope")` prepends the scope to the already
            // absolute key. ref: pattern_with_scope_scanner.rb:23-30.
            "scope.relative.index.title",
        ]
    );
}

/// A JS file: the gem scans it with the same regex scanner.
#[test]
fn fixture_app_javascript() {
    let scan = scan_fixture("app/assets/javascripts/application.js");
    // `//= require t` is a comment line, and `Matrix.t(this)` has a receiver.
    assert!(keys(&scan).is_empty());
}

/// A custom `SlimMultilineScanner`, of the kind projects bolt on to the gem,
/// absorbed into the built-in Slim scanner.
#[test]
fn slim_line_continuation_resolves() {
    let src = concat!(
        "div\n",
        "  = t( \\\n",
        "    \"services.restricted_warning_html\",\n",
        "    link: link_to( \\\n",
        "      t(\"services.restricted_warning_link\"),\n",
        "      path,\n",
        "    ),\n",
        "  )\n",
    );
    let scan = scan_file(
        src.as_bytes(),
        Path::new("app/components/x/restricted_warning_component.html.slim"),
        &cfg(),
    );
    assert_eq!(
        keys(&scan),
        vec![
            "services.restricted_warning_html",
            "services.restricted_warning_link"
        ]
    );
    // The occurrence points at the call, as the gem's own scanners do. The
    // custom scanner points at the key literal, one line further down.
    assert_eq!(scan.keys[0].1.line_num, 2);
}

/// The comment-skip table is per extension. ref: pattern_scanner.rb:16-24.
#[test]
fn comment_rules_are_per_extension() {
    for (path, line, scanned) in [
        ("a.js", "// t('x.a')", false),
        ("a.js", "// i18n-tasks-use t('x.a')", true),
        ("a.coffee", "# t('x.a')", false),
        ("a.slim", "-# t('x.a')", false),
        ("a.slim", "/ t('x.a')", false),
        // `.ts` is absent from the gem's table, so its comments are scanned.
        ("a.ts", "// t('x.a')", true),
    ] {
        let scan = scan_file(line.as_bytes(), Path::new(path), &cfg());
        assert_eq!(
            !scan.keys.is_empty(),
            scanned,
            "{path}: {line} should{} be scanned",
            if scanned { "" } else { " not" }
        );
    }
}

/// Blocker B5: a dynamic key in a template is a pattern, which is what the gem
/// reaches through `used_in_expr?`.
#[test]
fn dynamic_template_keys_become_patterns() {
    let src = "p = t(\"about.team.#{person}.name\")\n";
    let scan = scan_file(
        src.as_bytes(),
        Path::new("app/views/static/_about_card.html.slim"),
        &cfg(),
    );
    assert!(scan.keys.is_empty());
    assert_eq!(
        scan.patterns
            .iter()
            .map(|(p, _)| p.as_str())
            .collect::<Vec<_>>(),
        vec!["about.team.*:.name"]
    );
}
