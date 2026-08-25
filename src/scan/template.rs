//! The regex scanner: Haml, Slim, JS, TS, and every extension that is not `.rb`
//! or `.erb`.
//!
//! ref: lib/i18n/tasks/scanners/pattern_scanner.rb
//! ref: lib/i18n/tasks/scanners/pattern_with_scope_scanner.rb
//! ref: lib/i18n/tasks/scanners/ruby_key_literals.rb
//!
//! The gem uses a regex here too, so a regex is parity rather than a
//! compromise. This scanner is the **union** of two things: the gem's
//! `PatternWithScopeScanner`, which the gem applies to everything except
//! `*.erb` and `*.rb`, and a custom `SlimMultilineScanner` of the kind projects
//! bolt on to the gem. That scanner's one distinctive
//! feature is `\s*\(\s*\\?\s*`: it tolerates a Slim line-continuation
//! backslash between the call and its argument.
//!
//! Departures from the gem, all recorded in `docs/accepted-diffs.md`:
//!
//! * `LITERAL_RE` is fixed. The gem's `:?".+?"` stops at the first quote, so an
//!   escaped quote inside a key truncates it.
//! * A dynamic key becomes a key pattern (blocker B5), the same treatment the
//!   Ruby scanner gives `t("a.#{b}")`. The gem reaches the same keys through
//!   `used_in_expr?`, built from a second, non-strict scan of every file.
//! * The enclosing method name is never used. The gem re-reads the whole file
//!   from disk per occurrence to grep backwards for `def`
//!   (`pattern_scanner.rb:83-91`), which is quadratic, and it only ever applies
//!   to a path matching `controllers|mailers` — never a template.

use super::{FileScan, Occurrence, ScanConfig, ruby};
use crate::lineindex::LineIndex;
use regex::bytes::Regex;
use std::path::Path;
use std::sync::Arc;
use std::sync::LazyLock;

/// The call, its first argument and an optional `scope:`, in one pass.
///
/// ref: `pattern_scanner.rb:96-106` and `pattern_with_scope_scanner.rb:12-16`,
/// with the argument separator widened to the union described above.
#[allow(
    clippy::expect_used,
    reason = "a static pattern that fails to compile is a bug here, not a run-time condition"
)]
static CALL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?x)
        (?P<i18n> I18n\. )?
        t (?: ! | ranslate!? )?
        (?:
            \x20 \s*                      # ref: pattern_scanner.rb `[( ]`
          | \s* \( \s* (?: \\ \s* )?      # union: Slim line continuation
        )
        (?P<arg>
            :?" (?: \\ [\s\S] | [^"\\\n] )*"
          | :?' (?: \\ [\s\S] | [^'\\\n] )*'
          | : \w+
          | [\w@.&|\s?!]+                 # ref: pattern_with_scope_scanner.rb#expr_re
        )
        (?:
          \s* , \s*
          (?: :scope \s* => \s* | scope: \s* )
          (?P<scope> \[ [^\n)%\#]* \] | [^\n)%\#,]* )
        )?
        "#,
    )
    .expect("static pattern compiles")
});

/// ref: ruby_key_literals.rb:5 `LITERAL_RE`, used to decide whether a scope
/// fragment is a literal at all.
#[allow(
    clippy::expect_used,
    reason = "a static pattern that fails to compile is a bug here, not a run-time condition"
)]
static LITERAL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?x) :?" (?: \\ [\s\S] | [^"\\\n] )*" | :?' (?: \\ [\s\S] | [^'\\\n] )*' | : \w+ "#,
    )
    .expect("static pattern compiles")
});

pub fn scan(bytes: &[u8], path: &Path, cfg: &ScanConfig) -> FileScan {
    let index = LineIndex::new(bytes);
    // One allocation for the file, cloned into every occurrence it produces.
    let shared: Arc<Path> = Arc::from(path);
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default();
    let mut out = FileScan::default();
    let mut at = 0;
    while let Some(caps) = CALL_RE.captures_at(bytes, at) {
        // Group 0 always participates, so the `else` arms are unreachable;
        // they keep a regex change from turning into a panic.
        let Some(whole) = caps.get(0) else { break };
        // The gem's `t` is inside the match and `I18n.` is inside the
        // lookbehind, so the position it reports is the `t`.
        let call_pos = caps.name("i18n").map_or(whole.start(), |m| m.end());
        at = whole.end();
        if !preceded_ok(bytes, whole.start(), caps.name("i18n").is_some()) {
            // Rejected calls resume one byte on, not past the whole match, so a
            // real call inside the rejected span is still found.
            at = call_pos + 1;
            continue;
        }
        let Some(arg) = caps.name("arg") else {
            continue;
        };
        let arg = arg.as_bytes();
        let arg = String::from_utf8_lossy(arg).into_owned();
        let scope = caps
            .name("scope")
            .map(|m| String::from_utf8_lossy(m.as_bytes()).into_owned());

        // ref: pattern_scanner.rb:45 — the comment check comes before the key
        // is resolved, and it tests the whole line.
        if is_comment_line(ext, index.line_text(bytes, call_pos)) {
            continue;
        }
        let Some(key) = match_to_key(&arg, scope.as_deref(), path, cfg) else {
            continue;
        };
        // ref: pattern_scanner.rb:50 — a key built up from a prefix ending in a
        // dot becomes a single-segment wildcard.
        let key = if key.ends_with('.') {
            format!("{key}:")
        } else {
            key
        };
        if !valid_key(&key) {
            continue;
        }
        let (line_num, line_pos) = index.locate(call_pos);
        let occ = Occurrence {
            path: Arc::clone(&shared),
            snippet: String::from_utf8_lossy(index.line_text(bytes, call_pos)).into_owned(),
            pos: call_pos,
            line_pos,
            line_num,
            raw_key: strip_literal(&arg),
            candidate_keys: vec![key.clone()],
        };
        push_key(&mut out, key, occ);
    }
    out
}

/// Blocker B5, and the gem's `used_in_expr?` for the same input: a key with an
/// interpolation in it is a pattern, not a key.
fn push_key(out: &mut FileScan, key: String, occ: Occurrence) {
    if key.contains("#{") {
        let pattern = replace_interpolations(&key);
        if ruby::is_all_wildcard(&pattern) {
            // ref: used_keys.rb#expr_key_re `ignore_pattern_re` — a pattern with
            // no static content would mark every key used.
            out.opaque.push(occ);
        } else {
            out.patterns.push((pattern, occ));
        }
        return;
    }
    // A key that came from a `foo.` prefix keeps the gem's literal form, and
    // also protects the keys below it, which the gem's trailing-dot rule does
    // not: `key += ":"` runs before `expr_key_re` selects keys ending in a dot,
    // so the gem never derives a pattern from one.
    if let Some(prefix) = key.strip_suffix(':')
        && prefix.ends_with('.')
    {
        out.patterns.push((format!("{prefix}*:"), occ.clone()));
    }
    out.keys.push((key, occ));
}

/// ref: pattern_with_scope_scanner.rb:23-34
fn match_to_key(arg: &str, scope: Option<&str>, path: &Path, cfg: &ScanConfig) -> Option<String> {
    let key = absolute_key(&strip_literal(arg), path, cfg)?;
    match scope {
        Some(scope) => {
            let parts = extract_literal_or_array_of_literals(scope)?;
            if parts.is_empty() {
                return None;
            }
            Some(format!("{}.{}", parts.join("."), key))
        }
        // Without a scope, an expression argument is dropped: only a literal
        // starts with something other than a word character. Ruby's `\w` is
        // ASCII-only. ref: pattern_with_scope_scanner.rb:32
        None if arg.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_') => None,
        None => Some(key),
    }
}

/// ref: relative_keys.rb#absolute_key
///
/// The gem raises a `CommandError` when no relative root matches. Dropping the
/// occurrence keeps one unresolvable key in a stray template from failing the
/// whole run, and matches what the Ruby scanner does in the same situation.
fn absolute_key(key: &str, path: &Path, cfg: &ScanConfig) -> Option<String> {
    let Some(relative) = key.strip_prefix('.') else {
        return Some(key.to_string());
    };
    let root = cfg.matching_root(path)?;
    let posix = path.to_string_lossy().replace('\\', "/");
    let mut parts = ruby::template_path(&posix, root);
    parts.push(relative.to_string());
    Some(ruby::join_key(&parts))
}

/// ref: ruby_key_literals.rb:17-21, and `pattern_with_scope_scanner.rb:42-48`
/// for the expression case, which the gem turns into an interpolation so the
/// key is recognisably dynamic.
fn strip_literal(literal: &str) -> String {
    // `\A[\w@]`, and Ruby's `\w` is ASCII-only.
    if literal.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_' || c == '@') {
        return format!("#{{{literal}}}");
    }
    let literal = literal.strip_prefix(':').unwrap_or(literal);
    let bytes = literal.as_bytes();
    if bytes.len() >= 2
        && (bytes[0] == b'"' || bytes[0] == b'\'')
        && bytes[bytes.len() - 1] == bytes[0]
    {
        return literal[1..literal.len() - 1].to_string();
    }
    literal.to_string()
}

/// ref: pattern_with_scope_scanner.rb:66-97
///
/// Returns `None` for anything that is not a literal or an array of literals,
/// which drops the occurrence.
fn extract_literal_or_array_of_literals(s: &str) -> Option<Vec<String>> {
    let mut literals: Vec<String> = Vec::new();
    let mut in_brackets = false;
    let mut acc = String::new();
    for c in s.chars() {
        match c {
            '[' => {
                if in_brackets {
                    return None;
                }
                in_brackets = true;
            }
            ']' => break,
            ',' => {
                consume_literal(&mut acc, &mut literals)?;
                if !in_brackets {
                    break;
                }
            }
            _ if is_valid_key_char(c) || c == '\'' || c == '"' || c == ':' => acc.push(c),
            ' ' => {}
            _ => return None,
        }
    }
    if !acc.is_empty() {
        consume_literal(&mut acc, &mut literals)?;
    }
    Some(literals)
}

fn consume_literal(acc: &mut String, literals: &mut Vec<String>) -> Option<()> {
    if !LITERAL_RE.is_match(acc.as_bytes()) {
        return None;
    }
    literals.push(strip_literal(acc));
    acc.clear();
    Some(())
}

/// The gem's `t` is preceded by a lookbehind, `pattern_scanner.rb:15`. The
/// custom Slim scanner uses `(?<![\p{L}_.'-])`, the same character set, so the
/// union is: any `I18n.t`, or a bare `t` that no word character, quote, hyphen
/// or dot precedes.
fn preceded_ok(bytes: &[u8], start: usize, i18n_prefix: bool) -> bool {
    if i18n_prefix {
        return true;
    }
    match prev_char(bytes, start) {
        None => true,
        Some(c) => !(c.is_alphabetic() || c == '_' || c == '\'' || c == '-' || c == '.'),
    }
}

fn prev_char(bytes: &[u8], offset: usize) -> Option<char> {
    if offset == 0 {
        return None;
    }
    let mut start = offset - 1;
    // Walk back over UTF-8 continuation bytes.
    while start > 0 && bytes[start] & 0xC0 == 0x80 {
        start -= 1;
    }
    std::str::from_utf8(&bytes[start..offset])
        .ok()
        .and_then(|s| s.chars().next())
}

/// ref: pattern_scanner.rb:16-24 `IGNORE_LINES`
///
/// A comment line is skipped unless it carries a magic comment. The gem writes
/// this as a negative lookahead, `(?!\si18n-tasks-use)`; restructuring it that
/// way needs no lookaround. `.jsx`, `.ts` and `.tsx` are absent from the gem's
/// table, so a `//` comment in one of those is scanned, here as there.
fn is_comment_line(ext: &str, line: &[u8]) -> bool {
    let markers: &[&str] = match ext {
        "coffee" | "opal" => &["#"],
        "es6" | "js" => &["//"],
        "haml" => &[],
        "slim" => &["-#", "/"],
        // The gem's table has an `erb` entry, but its regex scanner never sees
        // an `.erb` file: `ErbAstScanner` handles those, and so does `erb.rs`.
        _ => return false,
    };
    let line = String::from_utf8_lossy(line);
    let trimmed = line.trim_start();
    // ref: pattern_scanner.rb:20 `^\s*-\s*#(?!\si18n-tasks-use)`
    if ext == "haml"
        && let Some(rest) = trimmed.strip_prefix('-')
        && let Some(rest) = rest.trim_start().strip_prefix('#')
    {
        let keeps_magic = rest
            .strip_prefix(char::is_whitespace)
            .is_some_and(|rest| rest.starts_with("i18n-tasks-use"));
        return !keeps_magic;
    }
    for marker in markers {
        if let Some(rest) = trimmed.strip_prefix(marker) {
            let keeps_magic = rest
                .strip_prefix(char::is_whitespace)
                .is_some_and(|r| r.starts_with("i18n-tasks-use"));
            return !keeps_magic;
        }
    }
    false
}

/// ref: pattern_scanner.rb:73 `VALID_KEY_RE_DYNAMIC`, which is the non-strict
/// gate. The port is always non-strict, because a dynamic key becomes a pattern
/// instead of being dropped.
fn valid_key(key: &str) -> bool {
    if key.is_empty() {
        return false;
    }
    let chars: Vec<char> = key.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if is_valid_key_char(c) || matches!(c, ':' | '#' | '{' | '@' | '}' | '[' | ']') {
            continue;
        }
        // ref: ruby_key_literals.rb:23 — whitespace only between alphanumerics.
        if c.is_whitespace()
            && i > 0
            && chars[i - 1].is_alphanumeric()
            && chars.get(i + 1).is_some_and(|c| c.is_alphanumeric())
        {
            continue;
        }
        return false;
    }
    true
}

/// ref: ruby_key_literals.rb:23 `VALID_KEY_CHARS`, without the whitespace rule,
/// which needs the surrounding characters.
fn is_valid_key_char(c: char) -> bool {
    c.is_alphanumeric()
        || c == '_'
        || matches!(c, '-' | '.' | '?' | '!' | ':' | ';' | '\\' | '/')
        || ('\u{C0}'..='\u{17E}').contains(&c)
}

/// ref: used_keys.rb#replace_key_exp — every top-level `#{...}` becomes `*:`,
/// with nested braces counted so `#{h[:k]}` is one replacement.
fn replace_interpolations(key: &str) -> String {
    let mut out = String::new();
    let mut depth = 0usize;
    let mut chars = key.char_indices().peekable();
    while let Some((_, c)) = chars.next() {
        match c {
            '#' if chars.peek().map(|(_, c)| *c) == Some('{') => {
                chars.next();
                depth += 1;
                if depth == 1 {
                    out.push_str("*:");
                }
            }
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn cfg() -> ScanConfig {
        ScanConfig {
            relative_roots: vec!["app/views".into(), "app/controllers".into()],
            relative_exclude_method_name_paths: vec![],
        }
    }

    fn keys(src: &str) -> Vec<String> {
        let out = scan(
            src.as_bytes(),
            &PathBuf::from("app/views/x/index.html.slim"),
            &cfg(),
        );
        out.keys.into_iter().map(|(k, _)| k).collect()
    }

    fn patterns(src: &str) -> Vec<String> {
        let out = scan(
            src.as_bytes(),
            &PathBuf::from("app/views/x/index.html.slim"),
            &cfg(),
        );
        out.patterns.into_iter().map(|(k, _)| k).collect()
    }

    // ref: spec/pattern_scanner_spec.rb#default_pattern
    #[test]
    fn matches_the_gems_pattern_cases() {
        for src in [
            r#"t(".a.b")"#,
            r#"t "a.b""#,
            "t 'a.b'",
            r#"t("a.b")"#,
            "t('a.b')",
            "t('a.b', :arg => val)",
            "t('a.b', arg: val)",
            "t :a_b",
            "t :'a.b'",
            r#"t :"a.b""#,
            "t(:ab)",
            "t(:'a.b')",
            r#"t(:"a.b")"#,
            r#"I18n.t("a.b")"#,
            r#"I18n.translate("a.b")"#,
        ] {
            assert!(!keys(src).is_empty(), "expected a key from {src}");
        }
    }

    #[test]
    fn rejects_what_the_gem_rejects() {
        assert!(keys("t \"a.b'").is_empty());
        assert!(keys("t a.b").is_empty());
        assert!(keys(r#"theme_t "a.b.""#).is_empty());
        assert!(keys("Spree.t 'not_a_key'").is_empty());
        assert!(keys("| x't :fp_quote_before").is_empty());
        assert!(keys("| x-t :fp_dash_before").is_empty());
    }

    // ref: spec/pattern_with_scope_scanner_spec.rb
    #[test]
    fn resolves_scopes() {
        assert_eq!(keys(r#"= t :key, scope: "scope""#), ["scope.key"]);
        assert_eq!(keys("= t :key, scope: :scope"), ["scope.key"]);
        assert_eq!(keys("= t :key, :scope => :scope"), ["scope.key"]);
        assert_eq!(
            keys(r#"= t :key, :scope => :scope, default: "Default""#),
            ["scope.key"]
        );
        assert_eq!(keys("= t :key, :scope => [:a, :b]"), ["a.b.key"]);
        assert_eq!(keys("= t :key, :scope => [:a, :b] :c"), ["a.b.key"]);
        assert_eq!(keys("= t :key, :scope => [:a, :b], :c"), ["a.b.key"]);
        assert_eq!(
            keys("= t :key, scope: :a, name: t(:key, scope: :b)"),
            ["a.key", "b.key"]
        );
    }

    #[test]
    fn drops_a_scope_that_is_not_a_literal() {
        for src in [
            "= t :key, scope: a",
            "= t :key, scope: []",
            "= t :key, scope: [a]",
            "= t :key, scope: [:x, [:y]]",
            "= t :key, scope: (a)",
            "= t key, scope: (a)",
            "= t key",
        ] {
            assert!(keys(src).is_empty(), "expected no key from {src}");
        }
    }

    // An expression argument with a literal scope is dynamic, so it is a
    // pattern here and a `used_in_expr?` entry in the gem.
    #[test]
    fn an_expression_with_a_scope_is_a_pattern() {
        assert_eq!(patterns(r#"= t key, scope: "scope""#), ["scope.*:"]);
        assert_eq!(patterns(r#"= t @key.m, scope: "scope""#), ["scope.*:"]);
        assert!(keys(r#"= t key, scope: "scope""#).is_empty());
    }

    #[test]
    fn slim_line_continuation_is_tolerated() {
        // A custom SlimMultilineScanner exists for exactly this.
        assert_eq!(keys("= t(\\\n  'multiline.key')"), ["multiline.key"]);
        assert_eq!(keys("= t (\n  'spaced.key')"), ["spaced.key"]);
    }

    #[test]
    fn skips_comment_lines_but_keeps_magic_comments() {
        assert!(keys("/ t(:fp_comment)").is_empty());
        assert_eq!(keys("/ i18n-tasks-use t(:fn_comment)"), ["fn_comment"]);
        assert_eq!(keys("#x = t 'not_a_comment'"), ["not_a_comment"]);
        assert_eq!(keys("-# i18n-tasks-use t(:kept)"), ["kept"]);
        assert!(keys("-# t(:dropped)").is_empty());
    }

    #[test]
    fn resolves_relative_keys_against_the_template_path() {
        assert_eq!(keys("p = t '.title'"), ["x.index.title"]);
        let out = scan(
            b"= t '.title'",
            &PathBuf::from("app/views/x/_partial.html.slim"),
            &cfg(),
        );
        assert_eq!(out.keys[0].0, "x.partial.title");
    }

    #[test]
    fn a_trailing_dot_becomes_a_wildcard() {
        assert_eq!(keys("= t 'foo.'"), ["foo.:"]);
        assert_eq!(patterns("= t 'foo.'"), ["foo.*:"]);
    }

    #[test]
    fn interpolated_keys_become_patterns() {
        assert_eq!(patterns(r#"p #{t "a.#{b}.c"}"#), ["a.*:.c"]);
        assert!(keys(r#"p #{t "a.#{b}.c"}"#).is_empty());
        // No static content at all, so it is an opaque call, never a pattern.
        let out = scan(
            r##"= t "#{b}""##.as_bytes(),
            &PathBuf::from("app/views/x/index.html.slim"),
            &cfg(),
        );
        assert!(out.patterns.is_empty());
        assert_eq!(out.opaque.len(), 1);
    }

    #[test]
    fn escaped_quotes_do_not_truncate_the_key() {
        // The gem's non-greedy `:?".+?"` stops at the escaped quote, so the key
        // becomes `say \`. Matching the whole literal drops it instead, because
        // a quote is not a valid key character, and the call after it is still
        // found.
        assert_eq!(keys(r#"= t("say \"hi\"") + t('real.key')"#), ["real.key"]);
    }

    #[test]
    fn several_calls_in_one_line() {
        assert_eq!(
            keys(r#"p #{t('ca.a')} #{t 'ca.b'} #{t "ca.c"}"#),
            ["ca.a", "ca.b", "ca.c"]
        );
    }

    #[test]
    fn occurrence_reports_the_call_position() {
        let src = "x\np = I18n.t 'a.b'\n";
        let out = scan(
            src.as_bytes(),
            &PathBuf::from("app/views/x/index.html.slim"),
            &cfg(),
        );
        let occ = &out.keys[0].1;
        assert_eq!(occ.line_num, 2);
        assert_eq!(occ.pos, src.find("t 'a.b'").unwrap());
        assert_eq!(occ.snippet, "p = I18n.t 'a.b'");
    }

    /// Full port of spec/scanners/ruby_key_literals_spec.rb `#valid_key?`.
    /// The gem allows whitespace inside a key, but only between alphanumerics.
    #[test]
    fn keys_may_hold_spaces_slashes_and_any_script() {
        for key in [
            "category/product",
            "key with spaces",
            "привет мир 你好 世界",
            "product カテゴリー with スペース",
            "項目123 テスト 456",
            "مرحبا بالعالم",
            "Hello مرحبا 世界",
        ] {
            assert_eq!(keys(&format!("t('{key}')")), vec![key], "{key}");
        }
    }

    #[test]
    fn a_key_the_gem_would_reject_yields_nothing() {
        // An empty literal is not a key.
        assert!(keys("t('')").is_empty());
        // A space that does not sit between two alphanumerics fails the rule,
        // so the whole occurrence is dropped.
        assert!(keys("t('a .b')").is_empty());
        assert!(keys("t('a. b')").is_empty());
        assert!(keys("t(' a')").is_empty());
    }

    /// ref: spec/pattern_with_scope_scanner_spec.rb "matches only the scope
    /// argument". The scope list ends at the first `,` outside brackets, and at
    /// the `]` inside them, so a later argument is never swallowed.
    #[test]
    fn only_the_scope_argument_is_read() {
        assert_eq!(keys("= t :key, :scope => [:a, :b] :c"), vec!["a.b.key"]);
        assert_eq!(keys("= t :key, :scope => [:a, :b], :c"), vec!["a.b.key"]);
        assert_eq!(keys("t('key', scope: 'a.b', count: 1)"), vec!["a.b.key"]);
        // ref: the scanner's own caveat — a scope is only read when it is the
        // argument right after the key, so this one is not a scope at all.
        assert_eq!(keys("t('key', count: 1, scope: :x)"), vec!["key"]);
    }

    /// A `#{}` payload may itself hold braces, and the whole thing still
    /// collapses to one single-segment wildcard.
    #[test]
    fn braces_inside_an_interpolation_do_not_end_it() {
        assert_eq!(patterns("t(\"a.#{h{k}}.z\")"), vec!["a.*:.z"]);
        assert_eq!(patterns("t(\"a.#{h[:k]}\")"), vec!["a.*:"]);
    }

    /// ref: spec/pattern_with_scope_scanner_spec.rb "matches nested calls"
    #[test]
    fn nested_calls_each_keep_their_own_scope() {
        assert_eq!(
            keys("= t :key, scope: :a, name: t(:key, scope: :b)"),
            vec!["a.key", "b.key"]
        );
        assert_eq!(
            keys("= t :key, scope: [:a, :a], name: t(:key, scope: :b)"),
            vec!["a.a.key", "b.key"]
        );
    }

    #[test]
    fn replaces_nested_interpolations() {
        assert_eq!(replace_interpolations("a.#{h[:k]}.b"), "a.*:.b");
        assert_eq!(replace_interpolations("a.#{x}#{y}"), "a.*:*:");
        assert_eq!(replace_interpolations("plain"), "plain");
    }
}
