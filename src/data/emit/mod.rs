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
//!
//! This module holds the [`Tree`] and the writer that walks it. How one scalar
//! is written — the quoting rules and the block forms — is `scalar`.

use crate::data::load::Value;
use std::collections::HashMap;

mod scalar;

use scalar::push_indent;
pub use scalar::{format_key, format_scalar};

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

    #[test]
    fn a_blank_line_inside_a_block_carries_no_indentation() {
        let out = emit(&[("a", s("one\n\ntwo\n"))]);
        assert_eq!(out, "---\nen:\n  a: |\n    one\n\n    two\n");
        assert!(!out.lines().any(|l| l.ends_with(' ')));
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
