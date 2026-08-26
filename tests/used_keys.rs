//! Scanner behaviour, driven by the gem's own fixtures.
//!
//! Ported from spec/prism_scanner_spec.rb and spec/used_keys_spec.rb.

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
    // The scanner sees the project-relative path, as the gem does.
    scan_file(&bytes, Path::new(rel), &cfg())
}

fn sorted_unique_keys(scan: &FileScan) -> Vec<String> {
    let mut out: Vec<String> = scan.keys.iter().map(|(k, _)| k.clone()).collect();
    out.sort();
    out.dedup();
    out
}

fn scan_source(src: &str, path: &str) -> FileScan {
    scan_file(src.as_bytes(), Path::new(path), &cfg())
}

/// ref: spec/prism_scanner_spec.rb:130-142 "handles more syntax"
#[test]
fn prism_controller_fixture_matches_the_gem_spec() {
    let scan = scan_fixture("prism_controller.rb");
    assert_eq!(
        sorted_unique_keys(&scan),
        vec![
            "prism.prism.index.label",
            "prism.prism.show.relative_key",
            "prism.show.assign",
            "prism.show.multiple",
        ]
    );
}

/// ref: spec/fixtures/used_keys/a.rb
#[test]
fn scope_arguments_that_are_not_static_drop_the_occurrence() {
    let scan = scan_fixture("used_keys/a.rb");
    // A `scope:` that resolves to a constant, an array holding a constant, a
    // shorthand `scope:` or a chained call is a ScopeError, and a ScopeError
    // drops the occurrence. ref: nodes.rb#scope, nodes.rb:208
    let keys = sorted_unique_keys(&scan);
    assert_eq!(
        keys,
        vec![
            "a",
            "activerecord.attributes.absolute.attribute",
            "service.what"
        ]
    );
    for dropped in [
        "ignore_a",
        "ignore_b",
        "ignore_array",
        "shorthand_scope_key",
        "chained_scope_key",
    ] {
        assert!(
            !keys.iter().any(|k| k.contains(dropped)),
            "{dropped} should be dropped"
        );
    }
    // The magic comment `# i18n-tasks-use t('service.what')` is what makes
    // `Service.translate(:what)` visible.
    assert!(keys.contains(&"service.what".to_string()));
}

#[test]
fn a_static_scope_is_prepended() {
    // ref: nodes.rb#scope — joined with "."
    let scan = scan_source(
        "t('a', scope: [:x, :y])\nt('b', scope: 'x.y')\nt('c', scope: :x)\n",
        "app/models/m.rb",
    );
    assert_eq!(sorted_unique_keys(&scan), vec!["x.c", "x.y.a", "x.y.b"]);
}

#[test]
fn a_falsey_scope_differs_from_an_absent_scope() {
    // See PR #731. `Array(nil)` is empty, and an empty scope is a ScopeError.
    assert!(
        scan_source("t('a', scope: nil)\n", "app/models/m.rb")
            .keys
            .is_empty()
    );
    assert_eq!(
        sorted_unique_keys(&scan_source("t('a')\n", "app/models/m.rb")),
        vec!["a"]
    );
}

#[test]
fn every_call_name_is_matched() {
    // ref: visitor.rb:91-120
    let src = "t('a')\nt!('b')\ntranslate('c')\ntranslate!('d')\nI18n.t('e')\n::I18n.t('f')\n";
    assert_eq!(
        sorted_unique_keys(&scan_source(src, "app/models/m.rb")),
        vec!["a", "b", "c", "d", "e", "f"]
    );
}

#[test]
fn magic_comment_before_end_still_resolves() {
    // The gem cannot do this: Prism leaves the comment unattached and
    // `ruby_scanner.rb:193-211` loses the enclosing scope.
    let src = "class UsersController < ApplicationController\n  def create\n    render\n    # i18n-tasks-use t('.late')\n  end\nend\n";
    assert_eq!(
        sorted_unique_keys(&scan_source(src, "app/controllers/users_controller.rb")),
        vec!["users.create.late"]
    );
}

#[test]
fn magic_comment_with_two_calls_on_one_line() {
    // The gem splits on /\s+(?=t)/ and joins with "; ".
    let src = "# i18n-tasks-use t('one') t('two')\nputs 1\n";
    assert_eq!(
        sorted_unique_keys(&scan_source(src, "app/models/m.rb")),
        vec!["one", "two"]
    );
}

#[test]
fn magic_comment_in_a_private_method_keeps_absolute_keys() {
    let src = "class C\n  private\n  # i18n-tasks-use t('abs.key')\n  def helper\n  end\nend\n";
    assert_eq!(
        sorted_unique_keys(&scan_source(src, "app/models/m.rb")),
        vec!["abs.key"]
    );
}

/// Blocker B5.
#[test]
fn interpolated_keys_become_patterns() {
    let scan = scan_source(
        "t(\"foo.#{bar}.title\")\nt(:\"a.#{b}\")\nt(\"#{x}\")\n",
        "app/models/m.rb",
    );
    let mut patterns: Vec<String> = scan.patterns.iter().map(|(p, _)| p.clone()).collect();
    patterns.sort();
    assert_eq!(patterns, vec!["a.*:", "foo.*:.title"]);
    // `t("#{x}")` alone would match every key, so it is reported as opaque
    // instead. ref: used_keys.rb#expr_key_re
    assert_eq!(scan.opaque.len(), 1);
}

/// Blocker B5.
#[test]
fn opaque_calls_are_reported() {
    let scan = scan_source(
        "t(some_var)\nt(SOME_CONST)\nt(build_key)\nt(:fine)\n",
        "app/models/m.rb",
    );
    assert_eq!(sorted_unique_keys(&scan), vec!["fine"]);
    assert_eq!(scan.opaque.len(), 3, "{:?}", scan.opaque);
}

#[test]
fn a_magic_comment_covers_an_opaque_call_on_the_next_line() {
    let scan = scan_source(
        "# i18n-tasks-use t('known.key')\nreturn unless ready? && t(dynamic_key).present?\nt(other_key)\n",
        "app/models/m.rb",
    );
    assert_eq!(sorted_unique_keys(&scan), vec!["known.key"]);
    assert_eq!(scan.opaque.len(), 1, "{:?}", scan.opaque);
    assert_eq!(scan.opaque[0].line_num, 3);
}

#[test]
fn a_literal_key_ending_in_a_dot_also_yields_a_pattern() {
    // ref: used_keys.rb#expr_key_re — a key ending in `.` is dynamic.
    let scan = scan_source("t('category.')\n", "app/models/m.rb");
    assert_eq!(sorted_unique_keys(&scan), vec!["category."]);
    assert_eq!(scan.patterns[0].0, "category.*:");
}

#[test]
fn occurrence_positions_are_correct() {
    let src = "class C\n  def m\n    t('a.b')\n  end\nend\n";
    let scan = scan_source(src, "app/models/m.rb");
    let (_, occ) = &scan.keys[0];
    assert_eq!(occ.line_num, 3);
    assert_eq!(occ.line_pos, 4);
    assert_eq!(occ.snippet, "t('a.b')");
    assert_eq!(occ.raw_key, "a.b");
}

/// The extension picks the scanner. ref: used_keys.rb:22-26. The other two
/// scanners are covered by `tests/erb_keys.rs` and `tests/template_keys.rs`.
#[test]
fn the_extension_picks_the_scanner() {
    let erb = scan_file(
        b"<%= t('.title') %>",
        Path::new("app/views/x.html.erb"),
        &cfg(),
    );
    assert_eq!(erb.keys[0].0, "x.title");
    let slim = scan_file(b"= t '.title'", Path::new("app/views/x.html.slim"), &cfg());
    assert_eq!(slim.keys[0].0, "x.title");
    // A binary-ish extension the walk would exclude anyway holds no key.
    let none = scan_file(b"nothing here", Path::new("app/views/x.txt"), &cfg());
    assert!(none.keys.is_empty());
}

/// ref: spec/prism_scanner_spec.rb "i18n-tasks-use - malformed payload does not
/// raise". The nested parse fails, so the comment contributes nothing and the
/// scan of the rest of the file carries on.
#[test]
fn a_malformed_magic_comment_is_ignored() {
    let scan = scan_source(
        "# i18n-tasks-use t('a'\nt('after')\n",
        "app/controllers/events_controller.rb",
    );
    assert_eq!(sorted_unique_keys(&scan), vec!["after"]);
}

/// A magic comment with nothing after the marker, which the gem's `strip`
/// leaves empty.
#[test]
fn an_empty_magic_comment_payload_is_ignored() {
    let scan = scan_source(
        "# i18n-tasks-use   \nt('after')\n",
        "app/controllers/events_controller.rb",
    );
    assert_eq!(sorted_unique_keys(&scan), vec!["after"]);
    // `#` characters inside the payload are deleted, so a payload of nothing
    // but hashes is empty too. ref: ruby_scanner.rb:203.
    let scan = scan_source(
        "# i18n-tasks-use ###\nt('after')\n",
        "app/controllers/events_controller.rb",
    );
    assert_eq!(sorted_unique_keys(&scan), vec!["after"]);
}

/// A magic comment goes through the same gates as real code: a foreign
/// receiver, an unusable scope, or a key that is not a literal all drop it.
#[test]
fn a_magic_comment_obeys_the_same_rules_as_a_call() {
    let src = "\
# i18n-tasks-use Foo.t('foreign')
# i18n-tasks-use t('bad_scope', scope: some_var)
# i18n-tasks-use t(some_var)
# i18n-tasks-use I18n.t('kept')
";
    let scan = scan_source(src, "app/controllers/events_controller.rb");
    assert_eq!(sorted_unique_keys(&scan), vec!["kept"]);
    // A comment is never an opaque call: the gem drops it silently, and there
    // is no call site to point a reader at.
    assert!(scan.opaque.is_empty(), "{:?}", scan.opaque);
}

/// A relative key in a magic comment needs a context that resolves it, and a
/// file that has none drops the comment.
#[test]
fn a_relative_key_in_a_magic_comment_needs_a_relative_context() {
    let scan = scan_source("# i18n-tasks-use t('.relative')\n", "lib/tasks/thing.rb");
    assert!(scan.keys.is_empty(), "{:?}", scan.keys);
    let scan = scan_source(
        "class EventsController\n  def create\n    # i18n-tasks-use t('.relative')\n  end\nend\n",
        "app/controllers/events_controller.rb",
    );
    assert_eq!(sorted_unique_keys(&scan), vec!["events.create.relative"]);
}

/// ref: arguments_visitor.rb — an integer, an array or a hash reduces to
/// something that is not a key, and the gem produces nothing for each.
#[test]
fn a_first_argument_that_is_not_a_key_yields_nothing() {
    for src in ["t(1)\n", "t([:a, :b])\n"] {
        let scan = scan_source(src, "app/models/m.rb");
        assert!(scan.keys.is_empty(), "{src}: {:?}", scan.keys);
        assert!(scan.patterns.is_empty(), "{src}");
        assert!(scan.opaque.is_empty(), "{src}: {:?}", scan.opaque);
    }
    // A call with no arguments at all is not a translation call either.
    let scan = scan_source("t\nt()\n", "app/models/m.rb");
    assert!(scan.keys.is_empty());
    assert!(scan.opaque.is_empty(), "{:?}", scan.opaque);
    // A braced hash literal is not a key, but it is not "no keys used"
    // either, so it is reported. ref: accepted difference 2.
    for src in ["t({a: 1})\n", "t(nil)\n"] {
        let scan = scan_source(src, "app/models/m.rb");
        assert!(scan.keys.is_empty(), "{src}");
        assert_eq!(scan.opaque.len(), 1, "{src}");
    }
}

/// ref: arguments_visitor.rb:14 — a `**splat` in the keyword hash is skipped,
/// and a non-literal keyword name with it, so neither hides the key.
#[test]
fn a_splat_or_an_odd_keyword_name_does_not_hide_the_key() {
    let scan = scan_source("t('a', **opts)\n", "app/models/m.rb");
    assert_eq!(sorted_unique_keys(&scan), vec!["a"]);
    let scan = scan_source("t('b', **opts, scope: :s)\n", "app/models/m.rb");
    assert_eq!(sorted_unique_keys(&scan), vec!["s.b"]);
    // A computed keyword name reduces to nothing and is dropped.
    let scan = scan_source("t('c', some_var => 1)\n", "app/models/m.rb");
    assert_eq!(sorted_unique_keys(&scan), vec!["c"]);
}

/// A module is not a relative-key context. ref: nodes.rb ParsedModule.
#[test]
fn a_module_body_does_not_resolve_a_relative_key() {
    let scan = scan_source(
        "module Admin\n  t('.rel')\n  t('absolute')\nend\n",
        "app/controllers/admin.rb",
    );
    assert_eq!(sorted_unique_keys(&scan), vec!["absolute"]);
}

/// A dynamic key that needs a relative context it cannot get is dropped, the
/// same as a literal relative key would be.
#[test]
fn a_relative_pattern_without_a_context_is_dropped() {
    let scan = scan_source("t(\".#{x}.title\")\n", "lib/tasks/thing.rb");
    assert!(scan.patterns.is_empty(), "{:?}", scan.patterns);
    assert!(scan.keys.is_empty());
}

/// An interpolation nested inside an interpolation still collapses to one
/// single-segment wildcard per `#{}`.
#[test]
fn a_nested_interpolation_becomes_one_wildcard() {
    // The whole `#{...}` is one wildcard, however much is nested inside it.
    let scan = scan_source("t(\"a.#{\"b#{c}\"}.z\")\n", "app/models/m.rb");
    assert_eq!(
        scan.patterns
            .iter()
            .map(|(p, _)| p.as_str())
            .collect::<Vec<_>>(),
        vec!["a.*:.z"]
    );
    // Adjacent string literals concatenate, and one of them may itself be
    // interpolated, so the parts list holds a nested interpolated string.
    let scan = scan_source("t(\"a.\" \"b#{c}.z\")\n", "app/models/m.rb");
    assert_eq!(
        scan.patterns
            .iter()
            .map(|(p, _)| p.as_str())
            .collect::<Vec<_>>(),
        vec!["a.b*:.z"]
    );
    // An interpolated symbol takes the same path as an interpolated string.
    let scan = scan_source("t(:\"a.#{x}\")\n", "app/models/m.rb");
    assert_eq!(
        scan.patterns
            .iter()
            .map(|(p, _)| p.as_str())
            .collect::<Vec<_>>(),
        vec!["a.*:"]
    );
}

/// ref: visitor.rb#i18n_receiver? — `::I18n` is the same receiver as `I18n`,
/// and any other constant path is not.
#[test]
fn a_top_level_i18n_receiver_is_recognised() {
    let scan = scan_source("::I18n.t('a')\nMy::I18n.t('b')\n", "app/models/m.rb");
    assert_eq!(sorted_unique_keys(&scan), vec!["a"]);
}

/// ref: spec/prism_scanner_spec.rb "empty controller", "handles empty method"
/// and "handles call with same name". None of the three holds a `t` call, and
/// `User.new` must not be mistaken for one.
#[test]
fn a_file_with_no_translation_call_yields_nothing() {
    for src in [
        "class ApplicationController < ActionController::Base\nend\n",
        "class EventsController < ApplicationController\n  def create\n  end\nend\n",
        "class EventsController < ApplicationController\n  def new\n    @user = User.new\n  end\nend\n",
    ] {
        let scan = scan_source(src, "app/controllers/events_controller.rb");
        assert!(scan.keys.is_empty(), "{src}");
        assert!(scan.opaque.is_empty(), "{src}: {:?}", scan.opaque);
    }
}

/// ref: spec/prism_scanner_spec.rb "handles translation as argument",
/// "handles translation inside block" and "handles translation inside proc".
/// A `t` call anywhere inside a method body belongs to that method.
#[test]
fn a_call_nested_in_an_argument_a_block_or_a_proc_is_found() {
    let scan = scan_source(
        "class EventsController < ApplicationController\n  def show\n    link_to(path, title: t(\".edit\"))\n  end\nend\n",
        "app/controllers/events_controller.rb",
    );
    assert_eq!(sorted_unique_keys(&scan), vec!["events.show.edit"]);

    let scan = scan_source(
        "class EventsController < ApplicationController\n  def show\n    component.title { t('.edit') }\n  end\nend\n",
        "app/controllers/events_controller.rb",
    );
    assert_eq!(sorted_unique_keys(&scan), vec!["events.show.edit"]);

    // A proc assigned to a constant in a class body: no method, so the key has
    // to be absolute, and it is.
    let scan = scan_source(
        "class Parser\n  DEFAULT_ERROR = proc do |invalid, valid|\n    I18n.t(\"i18n_tasks.cmd.enum_opt.invalid\", invalid: invalid)\n  end\nend\n",
        "lib/i18n/tasks/command/option_parsers/enum.rb",
    );
    assert_eq!(
        sorted_unique_keys(&scan),
        vec!["i18n_tasks.cmd.enum_opt.invalid"]
    );
}

/// ref: spec/prism_scanner_spec.rb "class" — a plain class outside every
/// configured relative root resolves absolute keys and drops relative ones.
#[test]
fn a_plain_class_keeps_only_its_absolute_keys() {
    let src =
        "class Event\n  def what\n    t('a')\n    t('.relative')\n    I18n.t('b')\n  end\nend\n";
    let scan = scan_source(src, "app/models/event.rb");
    assert_eq!(sorted_unique_keys(&scan), vec!["a", "b"]);
    let first = &scan.keys[0].1;
    assert_eq!(first.line_num, 3);
    assert_eq!(first.snippet, "t('a')");
    let last = &scan.keys[scan.keys.len() - 1].1;
    assert_eq!(last.line_num, 5);
    assert_eq!(last.snippet, "I18n.t('b')");
}

/// ref: spec/prism_scanner_spec.rb "file without class" — a `t` call nested in
/// the keyword arguments of another `t` call is found too.
#[test]
fn a_call_inside_another_calls_arguments_is_found() {
    let scan = scan_source(
        "t(\"what.is.this\", parameter: I18n.translate(\"other.thing\"))\n",
        "file_without_class.rb",
    );
    assert_eq!(
        sorted_unique_keys(&scan),
        vec!["other.thing", "what.is.this"]
    );
}

/// ref: spec/prism_scanner_spec.rb "syntax - calling model_name with safe
/// navigation". Rails inference is dropped, so the chain contributes nothing,
/// but it must not stop the scan or crash it.
#[test]
fn a_safe_navigation_chain_does_not_stop_the_scan() {
    let src = "class Blueprint < Blueprinter\n  field(:type) do |history, _opts|\n    (history.residence || history.residence_type.safe_constantize)&.model_name&.human\n    t('a')\n  end\nend\n";
    let scan = scan_source(src, "app/blueprints/blueprint.rb");
    assert_eq!(sorted_unique_keys(&scan), vec!["a"]);
    let occ = &scan.keys[0].1;
    assert_eq!(occ.line_num, 4);
    assert_eq!(occ.snippet, "t('a')");
}

/// Full port of spec/prism_scanner_spec.rb "translation options - handles
/// scope", including every form the gem cannot resolve.
#[test]
fn every_scope_form_from_the_gem_spec() {
    let src = r#"scope = 'special.events'
# These should be detected
t('scope_string', scope: 'events.descriptions')
I18n.t('scope_array', scope: ['events', 'titles'])
I18n.t("scope_array_symbol", scope: %i[events descriptions])
I18n.t("scope_array_words", scope: %w[events descriptions])

# Cannot handle, should ignore
I18n.t("scope_with_known_variable", scope: ["this", "that", scope])
I18n.t("scope_with_unknown", scope: ["this", "that", unknown, "other"])
I18n.t(model.key, **translation_options(model))
I18n.t("success", scope: scope)
"#;
    let scan = scan_source(src, "scope.rb");
    assert_eq!(
        sorted_unique_keys(&scan),
        vec![
            "events.descriptions.scope_array_symbol",
            "events.descriptions.scope_array_words",
            "events.descriptions.scope_string",
            "events.titles.scope_array",
        ]
    );
    // `I18n.t(model.key, ...)` has no static key, so it is reported rather
    // than dropped. ref: accepted difference 2.
    assert_eq!(scan.opaque.len(), 1, "{:?}", scan.opaque);
    // An empty scope list is a ScopeError, the same as a non-literal one.
    assert!(
        scan_source("t('a', scope: [])\n", "app/models/m.rb")
            .keys
            .is_empty()
    );
}

/// ref: accepted difference 4. The gem re-parents a `before_action` lambda's
/// relative keys onto every action it applies to; Rails inference is dropped,
/// so those keys have no method scope and are left behind.
#[test]
fn a_before_action_lambda_contributes_no_relative_key() {
    let src = "class EventsController < ApplicationController\n\
               \x20 before_action -> { t('.before_action') }, only: :create\n\
               \x20 before_action { non_existent if what? }\n\
               \x20 before_action do\n\
               \x20   t('.before_action2')\n\
               \x20 end\n\
               \n\
               \x20 def create\n\
               \x20   t('.relative_key')\n\
               \x20 end\n\
               end\n";
    let scan = scan_source(src, "app/controllers/events_controller.rb");
    // The gem also reports `events.create.before_action` and
    // `events.create.before_action2`.
    assert_eq!(
        sorted_unique_keys(&scan),
        vec!["events.create.relative_key"]
    );
}

/// ref: accepted difference 4. The gem attributes a callee's relative keys to
/// the calling action as well; without that re-parenting each method keeps only
/// its own.
#[test]
fn a_method_call_does_not_re_parent_the_callees_keys() {
    let src = "class EventsController\n\
               \x20 def create\n\
               \x20   t('.relative_key')\n\
               \x20   I18n.t(\"absolute_key\")\n\
               \x20   method_b\n\
               \x20 end\n\
               \n\
               \x20 def method_b\n\
               \x20   t('.error')\n\
               \x20   t(\"absolute_error\")\n\
               \x20 end\n\
               end\n";
    let scan = scan_source(src, "app/controllers/events_controller.rb");
    // The gem also reports `events.create.error`.
    assert_eq!(
        sorted_unique_keys(&scan),
        vec![
            "absolute_error",
            "absolute_key",
            "events.create.relative_key",
            "events.method_b.error",
        ]
    );
}

/// A `def` nested inside a `def`, and a `def` inside a bare module: both are
/// scopes in their own right, and neither resolves a relative key.
#[test]
fn a_nested_def_and_a_module_def_are_their_own_scopes() {
    let scan = scan_source(
        "class EventsController\n  def create\n    def inner\n      t('.rel')\n      t('abs')\n    end\n  end\nend\n",
        "app/controllers/events_controller.rb",
    );
    // ref: nodes.rb — the enclosing *definition* of a method is the class, not
    // the method around it, so the inner method hangs off `events`, not off
    // `events.create`.
    assert_eq!(sorted_unique_keys(&scan), vec!["abs", "events.inner.rel"]);
    // A module is not a relative context, so the relative key is dropped.
    let scan = scan_source(
        "module Admin\n  def helper\n    t('.rel')\n    t('abs')\n  end\nend\n",
        "app/controllers/admin.rb",
    );
    assert_eq!(sorted_unique_keys(&scan), vec!["abs"]);
}

/// A `def` at the top level of a file, with no class or module around it. A
/// `.rb` file is never a template, so it supports no relative key at all.
#[test]
fn a_top_level_def_has_no_relative_context() {
    let scan = scan_source(
        "def helper\n  t('abs')\n  t('.rel')\nend\n",
        "app/controllers/helper.rb",
    );
    assert_eq!(sorted_unique_keys(&scan), vec!["abs"]);
}

/// A receiver that is not a constant at all is not `I18n`, so the call is
/// skipped rather than treated as a translation.
#[test]
fn a_non_constant_receiver_is_skipped() {
    let scan = scan_source("obj.t('a')\nobj.thing.t('b')\nt('c')\n", "app/models/m.rb");
    assert_eq!(sorted_unique_keys(&scan), vec!["c"]);
}

/// A class name written as a constant path keeps only the names in it, so a
/// `self::` prefix contributes nothing.
#[test]
fn a_constant_path_class_name_keeps_only_its_constants() {
    let scan = scan_source(
        "class self::Thing\n  def m\n    t('.rel')\n    t('abs')\n  end\nend\n",
        "app/controllers/events_controller.rb",
    );
    assert_eq!(sorted_unique_keys(&scan), vec!["abs", "thing.m.rel"]);
}

/// A class defined inside a method: the method is the innermost scope when the
/// class opens, so the class path grows from the method path.
#[test]
fn a_class_inside_a_method_hangs_off_the_method_path() {
    let src = "class EventsController\n\
               \x20 def create\n\
               \x20   klass = Class.new\n\
               \x20   class Inner\n\
               \x20     def deep\n\
               \x20       t('.rel')\n\
               \x20       t('abs')\n\
               \x20     end\n\
               \x20   end\n\
               \x20 end\n\
               end\n";
    let scan = scan_source(src, "app/controllers/events_controller.rb");
    assert_eq!(
        sorted_unique_keys(&scan),
        vec!["abs", "events.create.inner.deep.rel"]
    );
}

/// A comment that is not valid UTF-8 is skipped, and the rest of the file is
/// still scanned. A locale file may hold any bytes, so this must not panic.
#[test]
fn a_comment_that_is_not_utf8_is_skipped() {
    let mut src: Vec<u8> = b"# i18n-tasks-use t('from_comment') ".to_vec();
    src.extend_from_slice(&[0xff, 0xfe]);
    src.extend_from_slice(b"\nt('after')\n");
    let scan = scan_file(&src, Path::new("app/models/m.rb"), &cfg());
    assert_eq!(sorted_unique_keys(&scan), vec!["after"]);
}
