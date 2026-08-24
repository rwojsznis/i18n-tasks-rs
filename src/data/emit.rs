//! The hand-written YAML emitter.
//!
//! ref: blocker B1 in `docs/design-notes.md`.
//!
//! Psych *defines* "normalized" for the gem, because `FileFormats#normalized?`
//! is exact string equality against Psych output. Psych cannot be reproduced
//! byte for byte from Rust, so this emitter aims at a different, stronger pair
//! of properties:
//!
//! * **value preservation** — parse, emit, parse again, and every key maps to
//!   the same value;
//! * **idempotence** — emitting twice produces the same bytes.
//!
//! Two Psych behaviours are dropped on purpose. Lines are never folded, which
//! removes the whole `line_width` class of bugs and keeps diffs stable. Non-BMP
//! characters are written literally, never as `\Uxxxxxxxx`, so the gem's
//! `EMOJI_REGEX` post-processing has no counterpart here.

use crate::data::load::Value;
use std::collections::HashMap;

// Counts the sibling keys a lookup had to examine, so a test can pin the
// per-insert cost of `insert_segments`. Thread-local, because the harness runs
// each test on its own thread.
#[cfg(test)]
thread_local! {
    static SIBLINGS_EXAMINED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn note_siblings_examined(n: usize) {
    SIBLINGS_EXAMINED.with(|c| c.set(c.get() + n));
}

/// A nested mapping, rebuilt from the flat key map for one output file.
///
/// `entries` holds the children in insertion order, which is what
/// `data.keep_order` preserves and what `sort` rewrites. `index` gives each
/// child's position in it, so an insert does not scan the siblings already
/// there — a parent with a few thousand keys is ordinary, and a scan makes the
/// build quadratic. The two are kept in step by `push_entry` and by `sort`,
/// which rebuilds the index after it moves the entries.
#[derive(Debug, Default, Clone)]
pub struct Tree {
    entries: Vec<(String, Entry)>,
    index: HashMap<String, usize>,
}

#[derive(Debug, Clone)]
enum Entry {
    Leaf(Value),
    Map(Tree),
}

impl Tree {
    pub fn new() -> Tree {
        Tree::default()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Inserts a dotted key.
    ///
    /// A key segment that holds a dot cannot be told apart from a nesting
    /// level here, the same as everywhere else in the gem's CLI, so the split
    /// is on every dot.
    ///
    /// When a leaf and a mapping claim the same path, the mapping wins. That
    /// is what `Tree::Siblings#merge!` does: merging a node that has children
    /// into a node that has a value keeps the children.
    pub fn insert(&mut self, key: &str, value: Value) {
        let segments: Vec<&str> = key.split('.').collect();
        self.insert_segments(&segments, value);
    }

    /// Inserts by real segments, which is what a key holding a dot needs.
    pub fn insert_segments(&mut self, segments: &[&str], value: Value) {
        let mut node = self;
        let mut parts = segments.iter().copied().peekable();
        while let Some(seg) = parts.next() {
            let last = parts.peek().is_none();
            let pos = node.position_of(seg);
            if last {
                match pos {
                    // The mapping wins, so an existing map is left alone.
                    Some(i) if matches!(node.entries[i].1, Entry::Map(_)) => {}
                    Some(i) => node.entries[i].1 = Entry::Leaf(value),
                    None => {
                        node.push_entry(seg, Entry::Leaf(value));
                    }
                }
                return;
            }
            let i = match pos {
                Some(i) => {
                    if matches!(node.entries[i].1, Entry::Leaf(_)) {
                        node.entries[i].1 = Entry::Map(Tree::new());
                    }
                    i
                }
                None => node.push_entry(seg, Entry::Map(Tree::new())),
            };
            let Entry::Map(child) = &mut node.entries[i].1 else {
                unreachable!("the branch above turned this into a map");
            };
            node = child;
        }
    }

    /// Finds a child by name.
    fn position_of(&self, seg: &str) -> Option<usize> {
        #[cfg(test)]
        note_siblings_examined(1);
        self.index.get(seg).copied()
    }

    /// Appends a child and records where it went. Returns its position.
    fn push_entry(&mut self, seg: &str, entry: Entry) -> usize {
        let i = self.entries.len();
        self.entries.push((seg.to_string(), entry));
        self.index.insert(seg.to_string(), i);
        i
    }

    /// Sorts every level, recursively.
    ///
    /// ref: blocker B10 — Ruby `String#<=>` and Rust `str` `Ord` are both
    /// byte-wise over UTF-8, so the two agree and no special handling is
    /// needed. `data.keep_order` skips this call.
    pub fn sort(&mut self) {
        self.entries.sort_by(|a, b| a.0.cmp(&b.0));
        self.index.clear();
        for (i, (key, entry)) in self.entries.iter_mut().enumerate() {
            self.index.insert(key.clone(), i);
            if let Entry::Map(child) = entry {
                child.sort();
            }
        }
    }
}

/// Emits one locale file: `---`, the locale root, then the tree.
pub fn emit_locale(locale: &str, tree: &Tree) -> String {
    let mut out = String::from("---\n");
    out.push_str(&format_key(locale));
    if tree.is_empty() {
        out.push_str(": {}\n");
        return out;
    }
    out.push_str(":\n");
    write_map(&mut out, tree, 1);
    out
}

fn write_map(out: &mut String, tree: &Tree, indent: usize) {
    for (key, entry) in &tree.entries {
        push_indent(out, indent);
        out.push_str(&format_key(key));
        match entry {
            Entry::Map(child) if child.is_empty() => out.push_str(": {}\n"),
            Entry::Map(child) => {
                out.push_str(":\n");
                write_map(out, child, indent + 1);
            }
            Entry::Leaf(value) => write_value(out, value, indent),
        }
    }
}

/// Writes the part after a `key`, newline included.
fn write_value(out: &mut String, value: &Value, indent: usize) {
    match value {
        // Psych writes a nil value as a bare `key:`.
        Value::Nil => out.push_str(":\n"),
        Value::Seq(items) if items.is_empty() => out.push_str(": []\n"),
        Value::Map(entries) if entries.is_empty() => out.push_str(": {}\n"),
        // A block sequence sits at the indent of its own key, which is what
        // Psych does with its default indentation.
        Value::Seq(items) => {
            out.push_str(":\n");
            write_seq(out, items, indent);
        }
        Value::Map(entries) => {
            out.push_str(":\n");
            for (k, v) in entries {
                push_indent(out, indent + 1);
                out.push_str(&format_key(k));
                write_value(out, v, indent + 1);
            }
        }
        _ => {
            out.push_str(": ");
            out.push_str(&format_scalar(value, indent));
            out.push('\n');
        }
    }
}

fn write_seq(out: &mut String, items: &[Value], indent: usize) {
    for item in items {
        // Render the item one level in, then splice the `- ` over the last two
        // spaces of its first line. That keeps a nested map or sequence lined
        // up under the dash without a second code path.
        let mut block = String::new();
        match item {
            Value::Nil => {
                push_indent(out, indent);
                out.push_str("-\n");
                continue;
            }
            Value::Seq(inner) if inner.is_empty() => {
                push_indent(out, indent);
                out.push_str("- []\n");
                continue;
            }
            Value::Map(inner) if inner.is_empty() => {
                push_indent(out, indent);
                out.push_str("- {}\n");
                continue;
            }
            Value::Seq(inner) => write_seq(&mut block, inner, indent + 1),
            Value::Map(inner) => {
                for (k, v) in inner {
                    push_indent(&mut block, indent + 1);
                    block.push_str(&format_key(k));
                    write_value(&mut block, v, indent + 1);
                }
            }
            _ => {
                push_indent(&mut block, indent + 1);
                // The dash sits at `indent`, so block content goes one deeper.
                block.push_str(&format_scalar(item, indent));
                block.push('\n');
            }
        }
        let dash_at = (indent + 1) * 2 - 2;
        block.replace_range(dash_at..dash_at + 2, "- ");
        out.push_str(&block);
    }
}

fn push_indent(out: &mut String, indent: usize) {
    for _ in 0..indent {
        out.push_str("  ");
    }
}

/// A mapping key. A key is always one line, so it never becomes a block scalar.
pub fn format_key(key: &str) -> String {
    match style_of(key) {
        Style::Plain => key.to_string(),
        Style::Single => single_quoted(key),
        Style::Double => double_quoted(key),
    }
}

/// A leaf value, including the block-scalar forms. `indent` is the indent of
/// the line that carries the key or the dash; block content goes one deeper.
pub fn format_scalar(value: &Value, indent: usize) -> String {
    let s = match value {
        Value::Str(s) => s.as_str(),
        // A number, a `true`, or a Symbol inside a sequence keeps the form it
        // was written in, so the file round-trips unchanged.
        Value::Plain(s) => return s.clone(),
        Value::Bool(b) => return b.to_string(),
        Value::Nil => return String::new(),
        Value::Seq(_) | Value::Map(_) => unreachable!("handled by write_value"),
    };
    if block_safe(s) {
        return block_scalar(s, indent + 1);
    }
    match style_of(s) {
        Style::Plain => s.to_string(),
        Style::Single => single_quoted(s),
        Style::Double => double_quoted(s),
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Style {
    Plain,
    Single,
    Double,
}

/// Decides how a one-line scalar is written.
///
/// "Required" is defined by these rules, and every one of them has a test:
///
/// * **Q1** the empty string, which has no plain form;
/// * **Q2** the value opens with a character that is not a word character and
///   holds no `"`. The YAML grammar needs only part of this: no plain scalar
///   may open with an indicator (`- ? : , [ ] { } # & * ! | > ' " % @ \``) or a
///   space, which is why `indicator_start` is checked on its own. The rest of
///   it — every other non-word character, and the `"` exception — is Psych's
///   rule copied verbatim (`o =~ /^[^[:word:]][^"]*$/` in
///   `Psych::Visitors::YAMLTree#visit_String`). Copying it keeps the output
///   identical to the gem's for the ordinary case, and that is what makes the
///   one-time reformat reviewable by hand: 15 files on the reference project
///   instead of several hundred;
/// * **Q3** a trailing space;
/// * **Q4** `": "` inside the value, or a trailing `:`, either of which would
///   read back as a mapping;
/// * **Q5** `" #"` inside the value, which would start a comment;
/// * **Q6** YAML 1.1 resolves the text to something that is not a string — a
///   null, a boolean, a number, a timestamp, `<<` or `=`;
/// * **Q7** a control character, a tab included, which no plain scalar can
///   carry legibly. This one forces the double-quoted form.
///
/// Between the two quoted forms: double quotes when escapes are needed and
/// when Q2 applies, single quotes otherwise. That is Psych's choice too.
fn style_of(s: &str) -> Style {
    if needs_escapes(s) {
        return Style::Double;
    }
    let first = s.chars().next();
    let non_word_start =
        first.is_some_and(|c| !c.is_alphanumeric() && c != '_') && !s.contains('"');
    let indicator_start = first.is_some_and(|c| c == ' ' || "-?:,[]{}#&*!|>'\"%@`".contains(c));
    let required = s.is_empty()                                     // Q1
        || non_word_start || indicator_start                        // Q2
        || s.ends_with(' ')                                         // Q3
        || s.contains(": ") || s.ends_with(':')                     // Q4
        || s.contains(" #")                                         // Q5
        || resolves_to_non_string(s); // Q6
    if !required {
        return Style::Plain;
    }
    if non_word_start {
        Style::Double
    } else {
        Style::Single
    }
}

/// Q7. `char::is_control` covers `\t`, `\n`, `\r` and the rest of C0 and C1.
fn needs_escapes(s: &str) -> bool {
    s.chars().any(char::is_control)
}

/// Q6. A plain scalar that YAML 1.1 does not read back as a string.
///
/// The answer comes from `yaml::resolve_plain`, the same resolver the loader
/// classifies with, so the two cannot disagree about a value's type. The four
/// extras are values Psych reads as strings but that are worth a pair of
/// quotes anyway: `y` and `n`, which a less strict YAML 1.1 reader takes for
/// booleans, and `<<` and `=`, Psych's merge and value keys.
fn resolves_to_non_string(s: &str) -> bool {
    !matches!(crate::yaml::resolve_plain(s), crate::yaml::Resolved::Str)
        || matches!(s, "y" | "Y" | "n" | "N" | "<<" | "=")
}

fn single_quoted(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// Non-BMP characters stay literal. The gem has to undo Psych's `\Uxxxxxxxx`
/// with a regex; there is nothing to undo here.
fn double_quoted(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Can this value be written as a `|` block scalar without changing it?
///
/// The first line has to carry the indentation, so it must not be empty and
/// must not open with a space; otherwise Psych needs an explicit indicator
/// such as `|2`, and this emitter double-quotes instead. A line that ends in a
/// space, or holds a tab or any other control character, is also refused,
/// because block content is copied verbatim and the whitespace would be easy
/// to lose in an editor.
fn block_safe(s: &str) -> bool {
    if !s.contains('\n') {
        return false;
    }
    let body = s.trim_end_matches('\n');
    let Some(first) = body.split('\n').next() else {
        return false;
    };
    if first.is_empty() || first.starts_with(' ') {
        return false;
    }
    body.split('\n')
        .all(|line| !line.ends_with(' ') && !line.chars().any(char::is_control))
}

/// ref: spec/yaml_spec.rb — Psych normalizes `|+` to `|` and every folded
/// style to a literal one, so only three chomping indicators ever appear.
fn block_scalar(s: &str, indent: usize) -> String {
    let trailing = s.len() - s.trim_end_matches('\n').len();
    let header = match trailing {
        0 => "|-",
        1 => "|",
        _ => "|+",
    };
    let mut lines: Vec<&str> = s.split('\n').collect();
    if trailing > 0 {
        lines.pop();
    }
    let mut out = String::from(header);
    for line in lines {
        out.push('\n');
        // An empty line is written empty, never as indentation alone, so the
        // file holds no trailing spaces.
        if !line.is_empty() {
            push_indent(&mut out, indent);
            out.push_str(line);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(pairs: &[(&str, Value)]) -> Tree {
        let mut t = Tree::new();
        for (k, v) in pairs {
            t.insert(k, v.clone());
        }
        t
    }

    fn emit(pairs: &[(&str, Value)]) -> String {
        let mut t = tree(pairs);
        t.sort();
        emit_locale("en", &t)
    }

    fn s(v: &str) -> Value {
        Value::Str(v.into())
    }

    fn scalar(v: &str) -> String {
        format_scalar(&s(v), 1)
    }

    /// H17: a lookup must not cost one comparison per sibling already there.
    /// A locale file with a few thousand keys under one parent is ordinary, and
    /// a linear scan makes that quadratic.
    #[test]
    fn inserting_a_sibling_does_not_scan_the_siblings_before_it() {
        const N: usize = 500;
        let mut t = Tree::new();
        SIBLINGS_EXAMINED.with(|c| c.set(0));
        for i in 0..N {
            t.insert(&format!("parent.key{i:04}"), s("v"));
        }
        let examined = SIBLINGS_EXAMINED.with(std::cell::Cell::get);
        // Two lookups per key — `parent`, then the leaf — so the floor is 2N.
        assert!(
            examined < 4 * N,
            "{examined} sibling comparisons for {N} inserts; want O(N), not O(N^2)"
        );
    }

    /// `sort` moves the entries, so the index it left behind has to be the
    /// new one. A stale index would place a second entry under a name that is
    /// already there, and the emitter would write the key twice.
    #[test]
    fn an_insert_after_a_sort_finds_the_entries_the_sort_moved() {
        let mut t = tree(&[("b", s("B")), ("a", s("A")), ("m.q", s("Q"))]);
        t.sort();
        t.insert("b", s("B2"));
        t.insert("m.p", s("P"));
        assert_eq!(
            emit_locale("en", &t),
            "---\nen:\n  a: A\n  b: B2\n  m:\n    q: Q\n    p: P\n"
        );
    }

    #[test]
    fn emits_a_nested_document() {
        assert_eq!(
            emit(&[("b.c", s("x")), ("a", s("hi"))]),
            "---\nen:\n  a: hi\n  b:\n    c: x\n"
        );
    }

    #[test]
    fn sorts_every_level_byte_wise() {
        let out = emit(&[("a.z", s("1")), ("a.B", s("2")), ("a.a", s("3"))]);
        // Uppercase sorts before lowercase, the same as Ruby `String#<=>`.
        assert_eq!(out, "---\nen:\n  a:\n    B: '2'\n    a: '3'\n    z: '1'\n");
    }

    #[test]
    fn keep_order_skips_the_sort() {
        let t = tree(&[("z", s("1")), ("a", s("2"))]);
        assert_eq!(emit_locale("en", &t), "---\nen:\n  z: '1'\n  a: '2'\n");
    }

    #[test]
    fn a_map_beats_a_leaf_at_the_same_path() {
        let mut t = tree(&[("a", s("leaf")), ("a.b", s("child"))]);
        t.sort();
        assert_eq!(emit_locale("en", &t), "---\nen:\n  a:\n    b: child\n");
        let mut t = tree(&[("a.b", s("child")), ("a", s("leaf"))]);
        t.sort();
        assert_eq!(emit_locale("en", &t), "---\nen:\n  a:\n    b: child\n");
    }

    // One test per quoting rule.

    #[test]
    fn q1_empty_string() {
        assert_eq!(scalar(""), "''");
    }

    #[test]
    fn q2_non_word_first_character() {
        assert_eq!(scalar("%{count} items"), "\"%{count} items\"");
        assert_eq!(scalar("-dash"), "\"-dash\"");
        assert_eq!(scalar("#hash"), "\"#hash\"");
        assert_eq!(scalar(" lead"), "\" lead\"");
        assert_eq!(scalar("«quoted»"), "\"«quoted»\"");
        // A word character opens a plain scalar, accents included.
        assert_eq!(scalar("Ärzte"), "Ärzte");
        assert_eq!(scalar("5% off"), "5% off");
        // A `"` inside takes the value out of Q2, the same as in Psych, but an
        // opening indicator still forces quotes.
        assert_eq!(scalar("«a\"b»"), "«a\"b»");
        assert_eq!(scalar("<a href=\"x\">y</a>"), "<a href=\"x\">y</a>");
        assert_eq!(scalar("#a\"b"), "'#a\"b'");
    }

    #[test]
    fn q3_trailing_space_or_tab() {
        assert_eq!(scalar("trail "), "'trail '");
        assert_eq!(scalar("a\tb"), "\"a\\tb\"");
    }

    #[test]
    fn q4_colon_space_or_trailing_colon() {
        assert_eq!(scalar("a: b"), "'a: b'");
        assert_eq!(scalar("note:"), "'note:'");
        // A colon that is not followed by a space is plain.
        assert_eq!(scalar("a:b"), "a:b");
    }

    #[test]
    fn q5_space_hash() {
        assert_eq!(scalar("a #b"), "'a #b'");
        assert_eq!(scalar("a#b"), "a#b");
    }

    #[test]
    fn q6_values_yaml_would_not_read_as_strings() {
        for v in [
            "true",
            "false",
            "yes",
            "no",
            "on",
            "off",
            "y",
            "n",
            "null",
            "123",
            "1.5",
            "0x1f",
            "010",
            "089",
            "1_000",
            "1:30",
            ".inf",
            "2020-01-01",
            "<<",
            "=",
        ] {
            assert_ne!(scalar(v), v, "{v} must be quoted");
        }
        // These only look numeric.
        for v in ["1.2.3", "e5", "12a", "0x", "1-2"] {
            assert_eq!(scalar(v), v, "{v} must stay plain");
        }
    }

    #[test]
    fn q7_control_characters() {
        assert_eq!(scalar("a\u{7}b"), "\"a\\x07b\"");
        assert_eq!(scalar("a\rb"), "\"a\\rb\"");
    }

    #[test]
    fn non_bmp_characters_stay_literal() {
        assert_eq!(scalar("😀 emoji"), "\"😀 emoji\"");
        assert_eq!(scalar("emoji 😀"), "emoji 😀");
    }

    #[test]
    fn single_quotes_double_up() {
        assert_eq!(scalar("'quoted'"), "\"'quoted'\"");
        assert_eq!(scalar("it's: here"), "'it''s: here'");
    }

    // ref: spec/yaml_spec.rb
    #[test]
    fn multi_line_values_use_block_scalars() {
        assert_eq!(scalar("hello\nworld\n"), "|\n    hello\n    world");
        assert_eq!(scalar("hello\nworld"), "|-\n    hello\n    world");
        assert_eq!(scalar("hello\nworld\n\n"), "|+\n    hello\n    world\n");
        assert_eq!(scalar("hi\n"), "|\n    hi");
    }

    #[test]
    fn a_blank_line_inside_a_block_carries_no_indentation() {
        let out = emit(&[("a", s("one\n\ntwo\n"))]);
        assert_eq!(out, "---\nen:\n  a: |\n    one\n\n    two\n");
        assert!(!out.lines().any(|l| l.ends_with(' ')));
    }

    #[test]
    fn unsafe_multi_line_values_are_double_quoted() {
        // A leading space would need Psych's `|2` indicator.
        assert_eq!(scalar("  indented\nnext\n"), "\"  indented\\nnext\\n\"");
        // A trailing space would be invisible in the block form.
        assert_eq!(scalar("tail \nnext\n"), "\"tail \\nnext\\n\"");
    }

    #[test]
    fn sequences_sit_at_the_indent_of_their_key() {
        assert_eq!(
            emit(&[(
                "order",
                Value::Seq(vec![
                    Value::Plain(":day".into()),
                    Value::Plain(":month".into())
                ])
            )]),
            "---\nen:\n  order:\n  - :day\n  - :month\n"
        );
    }

    #[test]
    fn a_map_inside_a_sequence_lines_up_under_the_dash() {
        let item = Value::Map(vec![
            ("title".into(), s("Roof")),
            ("cost".into(), Value::Plain("4".into())),
        ]);
        assert_eq!(
            emit(&[("list", Value::Seq(vec![item]))]),
            "---\nen:\n  list:\n  - title: Roof\n    cost: 4\n"
        );
    }

    #[test]
    fn a_sequence_inside_a_sequence() {
        let inner = Value::Seq(vec![Value::Plain("1".into()), Value::Plain("2".into())]);
        assert_eq!(
            emit(&[("m", Value::Seq(vec![inner]))]),
            "---\nen:\n  m:\n  - - 1\n    - 2\n"
        );
    }

    #[test]
    fn a_block_scalar_inside_a_sequence() {
        assert_eq!(
            emit(&[("m", Value::Seq(vec![s("a\nb\n")]))]),
            "---\nen:\n  m:\n  - |\n    a\n    b\n"
        );
    }

    /// A sequence item can itself be `nil` or an empty collection, and each
    /// gets its own inline form on the dash line.
    #[test]
    fn nil_and_empty_collections_inside_a_sequence() {
        assert_eq!(
            emit(&[(
                "m",
                Value::Seq(vec![
                    Value::Nil,
                    Value::Seq(vec![]),
                    Value::Map(vec![]),
                    s("x"),
                ])
            )]),
            "---\nen:\n  m:\n  -\n  - []\n  - {}\n  - x\n"
        );
    }

    /// A mapping two levels down inside a sequence. `write_value` renders the
    /// inner mapping, `write_seq` only lines up the dash.
    #[test]
    fn a_map_nested_inside_a_map_inside_a_sequence() {
        let inner = Value::Map(vec![("b".into(), s("B")), ("c".into(), s("C"))]);
        let item = Value::Map(vec![("a".into(), inner)]);
        assert_eq!(
            emit(&[("list", Value::Seq(vec![item]))]),
            "---\nen:\n  list:\n  - a:\n      b: B\n      c: C\n"
        );
    }

    /// A sequence nested under a mapping key inside a sequence item.
    #[test]
    fn a_sequence_under_a_key_inside_a_sequence_item() {
        let item = Value::Map(vec![("tags".into(), Value::Seq(vec![s("a"), s("b")]))]);
        assert_eq!(
            emit(&[("list", Value::Seq(vec![item]))]),
            "---\nen:\n  list:\n  - tags:\n    - a\n    - b\n"
        );
    }

    #[test]
    fn a_double_quoted_value_escapes_the_quote_and_the_backslash() {
        // A `"` in the value takes it out of Q2 but the newline still forces
        // the double-quoted form, which then has to escape both characters.
        // A leading space on the first line rules out the block form, so the
        // double-quoted form has to escape the quote, the backslash and the
        // newline itself.
        assert_eq!(scalar(" say \"hi\"\nx"), "\" say \\\"hi\\\"\\nx\"");
        assert_eq!(scalar(" a\\b\nc"), "\" a\\\\b\\nc\"");
        // A tab anywhere rules out the block form too.
        assert_eq!(scalar("tab\there\nx"), "\"tab\\there\\nx\"");
    }

    /// A key follows the same quoting rules as a value, including the double
    /// quoted form.
    #[test]
    fn a_key_that_needs_double_quotes_gets_them() {
        assert_eq!(format_key("a\tb"), "\"a\\tb\"");
        assert_eq!(format_key("plain"), "plain");
        assert_eq!(format_key("has space"), "has space");
    }

    #[test]
    fn a_bool_and_a_nil_leaf_keep_their_written_form() {
        assert_eq!(format_scalar(&Value::Bool(true), 0), "true");
        assert_eq!(format_scalar(&Value::Bool(false), 0), "false");
        assert_eq!(format_scalar(&Value::Nil, 0), "");
        assert_eq!(format_scalar(&Value::Plain("1.50".into()), 0), "1.50");
    }

    /// A deeper key wins over a shallower leaf whichever order they arrive in,
    /// and a leaf at a path an earlier leaf already claimed replaces it.
    #[test]
    fn a_later_leaf_replaces_an_earlier_one_at_the_same_path() {
        assert_eq!(
            emit(&[("a", s("first")), ("a", s("second"))]),
            "---\nen:\n  a: second\n"
        );
    }

    #[test]
    fn empty_collections_and_nil() {
        assert_eq!(
            emit(&[
                ("a", Value::Nil),
                ("b", Value::Seq(vec![])),
                ("c", Value::Map(vec![])),
            ]),
            "---\nen:\n  a:\n  b: []\n  c: {}\n"
        );
    }

    #[test]
    fn an_empty_locale_emits_an_empty_mapping() {
        assert_eq!(emit_locale("en", &Tree::new()), "---\nen: {}\n");
    }

    #[test]
    fn keys_follow_the_same_quoting_rules() {
        assert_eq!(
            emit(&[("true", s("a")), ("123", s("b")), ("ok", s("c"))]),
            "---\nen:\n  '123': b\n  ok: c\n  'true': a\n"
        );
    }

    #[test]
    fn deep_nesting() {
        let out = emit(&[("a.b.c.d.e.f.g.h.i.j", s("deep"))]);
        assert!(out.ends_with("                    j: deep\n"), "{out}");
    }
}
