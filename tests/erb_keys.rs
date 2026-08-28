//! The ERB scanner, driven by the gem's own fixtures.
//!
//! Ported from spec/used_keys_erb_prism_spec.rb. The Rails inference keys that
//! spec expects — `activerecord.*` from `model_name.human` and
//! `human_attribute_name` — are out of scope (accepted diff 4),
//! so they are absent here by design.

use i18n_tasks_rs::scan::{FileScan, ScanConfig, scan_file};
use std::path::Path;

fn cfg() -> ScanConfig {
    ScanConfig {
        relative_roots: vec![
            "app/components".into(),
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

fn patterns(scan: &FileScan) -> Vec<String> {
    let mut out: Vec<String> = scan.patterns.iter().map(|(k, _)| k.clone()).collect();
    out.sort();
    out.dedup();
    out
}

fn line_of(scan: &FileScan, key: &str) -> Vec<usize> {
    let mut lines: Vec<usize> = scan
        .keys
        .iter()
        .filter(|(k, _)| k == key)
        .map(|(_, o)| o.line_num)
        .collect();
    lines.sort_unstable();
    lines
}

/// ref: spec/used_keys_erb_prism_spec.rb ".html.erb"
#[test]
fn show_html_erb_matches_the_gem_spec() {
    let scan = scan_fixture("used_keys/app/views/application/show.html.erb");
    assert_eq!(
        keys(&scan),
        vec![
            "a",
            "application.show.edit",
            "application.show.nested_call",
            "blacklight.tools.citation",
            "comment.absolute.attribute",
            "scope_a.scope_b.with_scope",
            "with_parameter",
        ]
    );
    // Two tags on two lines, one of them a bare `<% what = t 'a' %>`.
    assert_eq!(line_of(&scan, "a"), vec![1, 2]);
    // `I18n.t("this_should_not")` on line 3 is outside every ERB tag.
    assert!(!keys(&scan).iter().any(|k| k == "this_should_not"));
    // `theme_t` is a different method.
    assert!(!keys(&scan).iter().any(|k| k.starts_with("ignore.this")));
}

/// ref: spec/used_keys_erb_prism_spec.rb ".text.erb"
#[test]
fn index_text_erb_matches_the_gem_spec() {
    let scan = scan_fixture("used_keys/app/views/application/index.text.erb");
    assert_eq!(
        keys(&scan),
        vec![
            "application.index.edit",
            "application.index.nested_call",
            "blacklight.tools.citation",
            "comment.absolute.attribute",
            "scope_a.scope_b.with_scope",
            "text.a",
            "with_parameter",
        ]
    );
    assert_eq!(line_of(&scan, "text.a"), vec![1, 2]);
    assert_eq!(line_of(&scan, "blacklight.tools.citation"), vec![22]);
}

/// A `t` call inside a block that spans two ERB tags. The gem needs its
/// `ignore_blocks` hack for this; one concatenated buffer does not.
#[test]
fn a_block_spanning_two_tags_parses() {
    let scan = scan_fixture("used_keys/app/views/application/show.html.erb");
    assert_eq!(line_of(&scan, "application.show.edit"), vec![15]);
    assert_eq!(line_of(&scan, "blacklight.tools.citation"), vec![21]);
}

/// ref: spec/used_keys_erb_prism_spec.rb "comments"
#[test]
fn comments_html_erb_matches_the_gem_spec() {
    let scan = scan_fixture("used_keys/app/views/application/comments.html.erb");
    assert_eq!(
        keys(&scan),
        vec![
            "erb.comment.works",
            "erb_multi.comment.line1",
            "erb_multi.comment.line2",
            "erb_multi_dash.comment.line1",
            "erb_multi_dash.comment.line2",
            "ruby.comment.works",
            "ruby_multi.comment.line1",
            "ruby_multi.comment.line2",
        ]
    );
    // An HTML comment is not an ERB tag, so the gem does not see this one either.
    assert!(!keys(&scan).iter().any(|k| k == "ignore.html.comments"));
    // Each magic comment reports its own line. The gem puts both `ruby_multi`
    // keys on line 20; see accepted diff 6.
    assert_eq!(line_of(&scan, "erb_multi.comment.line1"), vec![10]);
    assert_eq!(line_of(&scan, "erb_multi.comment.line2"), vec![11]);
    assert_eq!(line_of(&scan, "erb_multi_dash.comment.line1"), vec![15]);
    assert_eq!(line_of(&scan, "erb_multi_dash.comment.line2"), vec![16]);
    assert_eq!(line_of(&scan, "ruby_multi.comment.line1"), vec![20]);
    assert_eq!(line_of(&scan, "ruby_multi.comment.line2"), vec![21]);
    // Blocker B5: the three interpolated keys the gem drops become patterns.
    assert_eq!(
        patterns(&scan),
        vec![
            "erb_multi.comment.*:",
            "erb_multi_dash.comment.*:",
            "ruby_multi.comment.*:",
        ]
    );
}

/// Only a magic comment is read out of a comment tag. The gem parses the tag
/// body as live Ruby and reports every call in it.
///
/// ref: erb_ast_scanner.rb#process_comments
#[test]
fn a_comment_tag_without_a_magic_comment_yields_nothing() {
    let scan = scan_fixture("used_keys/app/views/application/commented_out.html.erb");
    assert_eq!(keys(&scan), vec!["live.key"]);
    // Blocker B5 must not turn the commented interpolated key into a pattern.
    assert!(patterns(&scan).is_empty(), "{:?}", patterns(&scan));
    assert!(scan.opaque.is_empty(), "{:?}", scan.opaque);
}

/// ref: spec/used_keys_erb_prism_spec.rb "partials"
#[test]
fn partials_resolve_relative_keys() {
    let scan = scan_fixture("used_keys/app/views/application/_event.html.erb");
    assert_eq!(
        keys(&scan),
        vec![
            "application.event.relative_key",
            "comment.absolute.attribute"
        ]
    );
}

/// ref: spec/used_keys_erb_prism_spec.rb "ViewComponent"
///
/// The gem hardcodes `app/components/`; the port needs it in
/// `search.relative_roots`. See accepted diff 3 (blocker B6).
#[test]
fn view_component_templates_resolve_relative_keys() {
    let scan = scan_fixture("used_keys/app/components/example_component.html.erb");
    assert_eq!(keys(&scan), vec!["example_component.header"]);
    let scan = scan_fixture("used_keys/app/components/namespaced/example_component.html.erb");
    assert_eq!(keys(&scan), vec!["namespaced.example_component.header"]);
}

/// The gem's own `spec/fixtures/app` views, used by the differential harness.
#[test]
fn fixture_app_erb_views() {
    let scan = scan_fixture("app/views/test.html.erb");
    assert_eq!(keys(&scan), vec!["scope.key_in_erb"]);
    // A `<% # ... %>` code tag holding a magic comment, in a `.js.erb` file.
    let scan = scan_fixture("app/views/show.js.erb");
    assert_eq!(keys(&scan), vec!["hello.world.from_javascript"]);
}

/// Design decision 3: one Prism parse per file, however many tags it has.
#[test]
fn one_parse_per_file_however_many_tags() {
    let mut src = String::new();
    for i in 0..200 {
        src.push_str(&format!("<div><%= t('key.k{i}') %></div>\n"));
    }
    // The counter is per thread, and each test runs on its own thread.
    let before = i18n_tasks_rs::scan::source_parses();
    let scan = scan_file(
        src.as_bytes(),
        Path::new("app/views/application/many.html.erb"),
        &cfg(),
    );
    assert_eq!(scan.keys.len(), 200);
    assert_eq!(i18n_tasks_rs::scan::source_parses() - before, 1);
}
