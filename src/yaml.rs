//! A small YAML reader over the `saphyr-parser` event stream.
//!
//! The gem reads locale data with Psych
//! (`lib/i18n/tasks/data/adapter/yaml_adapter.rb`), passing
//! `permitted_classes: [Symbol], aliases: true`. Two of those behaviours are
//! deliberately not reproduced. See blocker B4 in `docs/design-notes.md`:
//!
//! * an anchor, an alias or a merge key is an error, because `normalize` would
//!   inline it permanently;
//! * a value that Psych would turn into a Ruby Symbol is an error, because the
//!   reference subsystem is dropped.
//!
//! Both errors name the file and the line.

use saphyr_parser::{Event, Parser, ScalarStyle, Span};
use std::fmt;
use std::path::Path;

#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Scalar {
        value: String,
        style: ScalarStyle,
        line: usize,
    },
    Seq {
        items: Vec<Node>,
        line: usize,
    },
    Map {
        entries: Vec<(Node, Node)>,
        line: usize,
    },
}

impl Node {
    pub fn line(&self) -> usize {
        match self {
            Node::Scalar { line, .. } | Node::Seq { line, .. } | Node::Map { line, .. } => *line,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Node::Scalar { value, .. } => Some(value),
            _ => None,
        }
    }

    pub fn as_map(&self) -> Option<&[(Node, Node)]> {
        match self {
            Node::Map { entries, .. } => Some(entries),
            _ => None,
        }
    }

    pub fn as_seq(&self) -> Option<&[Node]> {
        match self {
            Node::Seq { items, .. } => Some(items),
            _ => None,
        }
    }

    /// True when the scalar is unquoted, which is what makes Psych resolve it
    /// to something other than a String.
    pub fn is_plain(&self) -> bool {
        matches!(
            self,
            Node::Scalar {
                style: ScalarStyle::Plain,
                ..
            }
        )
    }

    pub fn map_get(&self, key: &str) -> Option<&Node> {
        self.as_map()?
            .iter()
            .find(|(k, _)| k.as_str() == Some(key))
            .map(|(_, v)| v)
    }
}

#[derive(Debug)]
pub struct YamlError {
    pub path: String,
    pub line: usize,
    pub message: String,
}

impl fmt::Display for YamlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: {}", self.path, self.line, self.message)
    }
}

impl std::error::Error for YamlError {}

/// Parses the first document of `src`. Returns `None` for an empty document.
///
/// # Errors
///
/// The source is not valid YAML, or it uses an anchor, an alias or a tag. All
/// three are refused rather than resolved, because `normalize` would write the
/// resolved form back and lose them.
pub fn parse(src: &str, path: &Path) -> Result<Option<Node>, YamlError> {
    let disp = path.display().to_string();
    let err = |line: usize, message: String| YamlError {
        path: disp.clone(),
        line,
        message,
    };

    let mut events: Vec<(Event, Span)> = Vec::new();
    for item in Parser::new_from_str(src) {
        let (ev, span) = item.map_err(|e| {
            err(
                e.marker().line(),
                format!("YAML syntax error: {}", e.info()),
            )
        })?;
        let line = span.start.line();
        match &ev {
            Event::Alias(_) => {
                return Err(err(
                    line,
                    "YAML aliases are not supported. `normalize` would inline the alias \
                     permanently, so the tool refuses to read it. Write the value out instead."
                        .into(),
                ));
            }
            // An anchor or a tag is refused wherever it appears, so a scalar
            // and the start of a collection are the same case here.
            Event::Scalar(_, _, anchor_id, tag)
            | Event::SequenceStart(anchor_id, tag)
            | Event::MappingStart(anchor_id, tag) => {
                if *anchor_id != 0 {
                    return Err(err(line, anchor_msg()));
                }
                if let Some(t) = tag {
                    return Err(err(line, tag_msg(&t.to_string())));
                }
            }
            _ => {}
        }
        events.push((ev, span));
    }

    let mut i = 0usize;
    while i < events.len() {
        match events[i].0 {
            Event::StreamStart | Event::DocumentStart(_) => i += 1,
            Event::StreamEnd | Event::DocumentEnd => return Ok(None),
            _ => break,
        }
    }
    if i >= events.len() {
        return Ok(None);
    }
    let (node, _) = build(&events, i, &disp)?;
    // Psych turns a document that holds only `~`, `null` or nothing at all into
    // `nil`, and the gem then treats it as `{}`. ref: `load_file(path) || {}`.
    if is_null_scalar(&node) {
        return Ok(None);
    }
    Ok(Some(node))
}

/// A plain scalar that Psych resolves to `nil`.
pub fn is_null_scalar(node: &Node) -> bool {
    match node {
        Node::Scalar {
            value,
            style: ScalarStyle::Plain,
            ..
        } => matches!(resolve_plain(value), Resolved::Null),
        _ => false,
    }
}

/// What Psych reads a **plain** (unquoted) scalar as, in the subset of YAML 1.1
/// locale data can hold. ref: `psych/lib/psych/scalar_scanner.rb#tokenize`.
///
/// One answer, two callers: `to_value` in `src/data/load/mod.rs` classifies a
/// scalar with it, and `style_of` in `src/data/emit/scalar.rs` decides from
/// the same answer whether the value needs quotes. Two predicates for the one
/// question drifted apart once already — the loader read `0x1f` as a string
/// and the emitter then quoted it, so `normalize --write` turned the integer
/// 31 into the string `"0x1f"`.
///
/// The two halves are not equally strict, and deliberately so:
///
/// * `Null` and `Bool` are exact. Naming a string as one of them would rewrite
///   the value, because the emitter writes those two from the tag, not from
///   the source text.
/// * `Number` and `Timestamp` may name a few values Psych calls strings —
///   `1e5` is one. That is safe in both directions: the loader keeps the
///   written form, the emitter writes those bytes back unquoted, and Psych
///   reads the same string it read before.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolved {
    Null,
    Bool(bool),
    Number,
    Timestamp,
    Str,
}

pub fn resolve_plain(s: &str) -> Resolved {
    if s.is_empty() || s == "~" || s.eq_ignore_ascii_case("null") {
        return Resolved::Null;
    }
    // Psych's `/^(yes|true|on)$/i` and `/^(no|false|off)$/i`. Bare `y` and `n`
    // are strings there, whatever other YAML 1.1 readers do with them.
    if ["true", "yes", "on"]
        .iter()
        .any(|w| s.eq_ignore_ascii_case(w))
    {
        return Resolved::Bool(true);
    }
    if ["false", "no", "off"]
        .iter()
        .any(|w| s.eq_ignore_ascii_case(w))
    {
        return Resolved::Bool(false);
    }
    if is_yaml_number(s) {
        return Resolved::Number;
    }
    if is_timestamp(s) {
        return Resolved::Timestamp;
    }
    Resolved::Str
}

fn is_yaml_number(s: &str) -> bool {
    let body = s.strip_prefix(['-', '+']).unwrap_or(s);
    if body.is_empty() {
        return false;
    }
    if body.eq_ignore_ascii_case(".inf") || body.eq_ignore_ascii_case(".nan") {
        return true;
    }
    // `0x1f`, `0b1010` and `0o17`, underscores included.
    for (prefix, radix) in [
        ("0x", 16u32),
        ("0X", 16),
        ("0b", 2),
        ("0B", 2),
        ("0o", 8),
        ("0O", 8),
    ] {
        if let Some(digits) = body.strip_prefix(prefix) {
            let digits = digits.replace('_', "");
            return !digits.is_empty() && digits.chars().all(|c| c.is_digit(radix));
        }
    }
    let plain = body.replace('_', "");
    if plain.is_empty() {
        return false;
    }
    // Bare octal: a leading zero and nothing but octal digits.
    if plain.len() > 1 && plain.starts_with('0') && plain.chars().all(|c| c.is_digit(8)) {
        return true;
    }
    // Sexagesimal, which YAML 1.1 reads as a number: `1:30`, `1:30:15.5`.
    if plain.contains(':')
        && plain
            .split(':')
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit() || c == '.'))
    {
        return true;
    }
    // Rust's float parser accepts `inf`, `infinity` and `nan`, which YAML 1.1
    // reads as plain strings — only the dotted forms above are floats. Keep
    // letters out of the base-10 branch, `e` and `E` excepted.
    if !plain
        .chars()
        .all(|c| c.is_ascii_digit() || matches!(c, '.' | '+' | '-' | 'e' | 'E'))
    {
        return false;
    }
    plain.parse::<i64>().is_ok() || plain.parse::<f64>().is_ok()
}

/// A date, with or without a time after it. Psych reads `2020-01-01` as a
/// `Date` and `2020-01-01 10:00:00` as a `Time`.
fn is_timestamp(s: &str) -> bool {
    let mut parts = s.splitn(3, '-');
    let (Some(y), Some(m), Some(rest)) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    let day: String = rest.chars().take_while(char::is_ascii_digit).collect();
    y.len() == 4
        && y.chars().all(|c| c.is_ascii_digit())
        && (1..=2).contains(&m.len())
        && m.chars().all(|c| c.is_ascii_digit())
        && (1..=2).contains(&day.len())
}

fn anchor_msg() -> String {
    "YAML anchors are not supported. `normalize` would inline the anchor permanently, \
     so the tool refuses to read it."
        .into()
}

fn tag_msg(tag: &str) -> String {
    format!(
        "YAML tag `{tag}` is not supported. Dates, times and `!ruby/*` types are rejected; \
         quote the value if you meant a string."
    )
}

fn build(events: &[(Event, Span)], mut i: usize, path: &str) -> Result<(Node, usize), YamlError> {
    let line = events[i].1.start.line();
    match &events[i].0 {
        Event::Scalar(v, style, _, _) => Ok((
            Node::Scalar {
                value: v.to_string(),
                style: *style,
                line,
            },
            i + 1,
        )),
        Event::SequenceStart(..) => {
            i += 1;
            let mut items = Vec::new();
            while i < events.len() && !matches!(events[i].0, Event::SequenceEnd) {
                let (n, next) = build(events, i, path)?;
                items.push(n);
                i = next;
            }
            Ok((Node::Seq { items, line }, i + 1))
        }
        Event::MappingStart(..) => {
            i += 1;
            let mut entries = Vec::new();
            while i < events.len() && !matches!(events[i].0, Event::MappingEnd) {
                let (k, next) = build(events, i, path)?;
                // ref: the gem relies on Psych rejecting `<<` merge keys only
                // when aliases are off. The tool rejects the key outright.
                if k.as_str() == Some("<<") {
                    return Err(YamlError {
                        path: path.to_string(),
                        line: k.line(),
                        message: "YAML merge keys (`<<`) are not supported.".into(),
                    });
                }
                let (v, next2) = build(events, next, path)?;
                entries.push((k, v));
                i = next2;
            }
            Ok((Node::Map { entries, line }, i + 1))
        }
        ev => Err(YamlError {
            path: path.to_string(),
            line,
            message: format!("unexpected YAML event {ev:?}"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(src: &str) -> Result<Option<Node>, YamlError> {
        parse(src, Path::new("t.yml"))
    }

    #[test]
    fn reads_nested_maps() {
        let n = p("en:\n  a:\n    b: hello\n").unwrap().unwrap();
        let en = n.map_get("en").unwrap();
        let a = en.map_get("a").unwrap();
        assert_eq!(a.map_get("b").unwrap().as_str(), Some("hello"));
    }

    #[test]
    fn empty_document_is_none() {
        assert!(p("").unwrap().is_none());
        assert!(p("---\n").unwrap().is_none());
    }

    #[test]
    fn rejects_anchors_and_aliases() {
        let e = p("en:\n  a: &x 1\n  b: *x\n").unwrap_err();
        assert_eq!(e.line, 2);
        assert!(e.message.contains("anchors"));
    }

    #[test]
    fn rejects_merge_keys() {
        let e = p("a: {}\nen:\n  <<: 1\n").unwrap_err();
        assert!(e.message.contains("merge keys"));
    }

    #[test]
    fn rejects_tags() {
        let e = p("en:\n  a: !ruby/object:Foo {}\n").unwrap_err();
        assert!(e.message.contains("not supported"));
    }

    /// A tag on a scalar, which is the `!ruby/symbol` and date/time case.
    #[test]
    fn rejects_a_tag_on_a_scalar() {
        let e = p("en:\n  a: !ruby/symbol foo\n").unwrap_err();
        assert_eq!(e.line, 2);
        assert!(e.message.contains("ruby/symbol"), "{}", e.message);
        let e = p("en:\n  a: !!timestamp 2001-12-14\n").unwrap_err();
        assert!(e.message.contains("not supported"), "{}", e.message);
    }

    /// An anchor on a collection, rather than on a scalar.
    #[test]
    fn rejects_an_anchor_on_a_collection() {
        // The mapping event starts at its first key, so the anchor on the
        // parent is reported on line 2.
        let e = p("en: &x\n  a: 1\n").unwrap_err();
        assert_eq!(e.line, 2);
        assert!(e.message.contains("anchors"), "{}", e.message);
        let e = p("en:\n  a: &s\n    - 1\n").unwrap_err();
        assert!(e.message.contains("anchors"), "{}", e.message);
    }

    /// A tag on a sequence.
    #[test]
    fn rejects_a_tag_on_a_sequence() {
        let e = p("en:\n  a: !!set\n    - 1\n").unwrap_err();
        assert!(e.message.contains("not supported"), "{}", e.message);
    }

    /// ref: spec/file_system_data_spec.rb "includes problematic YAML file path
    /// in exception message" — the gem appends `(file: invalid.yml)`, this
    /// prefixes `path:line:`.
    #[test]
    fn a_syntax_error_names_the_file_and_the_line() {
        let e = p("en:\n  a: 1\n  %bad\n").unwrap_err();
        assert_eq!(e.path, "t.yml");
        assert!(e.message.contains("YAML syntax error"), "{}", e.message);
        assert_eq!(e.to_string(), format!("t.yml:{}: {}", e.line, e.message));
    }

    #[test]
    fn reads_sequences() {
        let n = p("en:\n  order:\n    - day\n    - month\n")
            .unwrap()
            .unwrap();
        let items = n.map_get("en").unwrap().map_get("order").unwrap();
        let items = items.as_seq().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].as_str(), Some("day"));
    }

    /// The accessors return `None` for the wrong shape rather than panicking.
    #[test]
    fn accessors_reject_the_wrong_shape() {
        let n = p("en:\n  a: hello\n  b: [1]\n").unwrap().unwrap();
        let en = n.map_get("en").unwrap();
        let a = en.map_get("a").unwrap();
        let b = en.map_get("b").unwrap();
        assert!(a.as_map().is_none());
        assert!(a.as_seq().is_none());
        assert!(en.as_str().is_none());
        assert!(b.as_str().is_none());
        assert!(b.as_map().is_none());
        assert!(a.map_get("x").is_none());
        assert_eq!(en.line(), 2);
        assert_eq!(b.line(), 3);
    }

    #[test]
    fn a_null_document_is_none() {
        assert!(p("~\n").unwrap().is_none());
        assert!(p("null\n").unwrap().is_none());
        assert!(p("NULL\n").unwrap().is_none());
        assert!(p("\"null\"\n").unwrap().is_some());
        assert!(p("# nothing here\n").unwrap().is_none());
    }

    /// ref: Psych's `ScalarScanner`. `""` is `Null`, not a number, so it is
    /// left out of both lists.
    #[test]
    fn yaml_number_recognition() {
        for v in [
            "1", "-1", "+1", "1.5", "1_000", ".inf", ".Inf", ".INF", ".nan", ".NaN", ".NAN",
            "0x1f", "0X1F", "0b1010", "0B1010", "0o17", "0O17", "010", "1e5", "1.0e-5", "1:30",
            // Not octal, but Ruby still reads it as the integer 89.
            "089",
        ] {
            assert_eq!(resolve_plain(v), Resolved::Number, "{v} is a number");
        }
        for v in [
            "-", "+", "_", "0x", "0b", "0xzz", "1.2.3", "e5", "12a", "1-2", ".in",
            // Rust parses these three as floats. YAML 1.1 does not.
            "inf", "infinity", "nan",
        ] {
            assert_eq!(resolve_plain(v), Resolved::Str, "{v} is not a number");
        }
    }

    /// The null and boolean halves are exact, so a casing Psych does not
    /// accept has to stay a string.
    #[test]
    fn null_and_boolean_recognition() {
        for v in ["", "~", "null", "Null", "NULL", "nUlL"] {
            assert_eq!(resolve_plain(v), Resolved::Null, "{v} is null");
        }
        for v in ["true", "True", "TRUE", "yes", "Yes", "nO", "off", "Off"] {
            let want = Resolved::Bool(matches!(v.to_ascii_lowercase().as_str(), "true" | "yes"));
            assert_eq!(resolve_plain(v), want, "{v} is a boolean");
        }
        // `y` and `n` are strings for Psych; the emitter quotes them anyway.
        for v in ["y", "n", "nope", "onward", "~~", "<<", "="] {
            assert_eq!(resolve_plain(v), Resolved::Str, "{v} is a string");
        }
    }

    #[test]
    fn timestamp_recognition() {
        for v in ["2020-01-01", "2020-1-1", "2020-01-01 10:00:00"] {
            assert_eq!(resolve_plain(v), Resolved::Timestamp, "{v} is a date");
        }
        for v in ["20-01-01", "2020-01-011", "2020-011-01"] {
            assert_eq!(resolve_plain(v), Resolved::Str, "{v} is not a date");
        }
    }

    #[test]
    fn plain_style_is_distinguished_from_quoted() {
        let n = p("en:\n  plain: 1\n  quoted: \"1\"\n").unwrap().unwrap();
        let en = n.map_get("en").unwrap();
        assert!(en.map_get("plain").unwrap().is_plain());
        assert!(!en.map_get("quoted").unwrap().is_plain());
    }
}
