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
    // Skip to the first document.
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
        } => {
            matches!(value.as_str(), "" | "~" | "null" | "Null" | "NULL")
        }
        _ => false,
    }
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
        // A scalar is not a map and not a sequence.
        assert!(a.as_map().is_none());
        assert!(a.as_seq().is_none());
        // A map and a sequence are not scalars.
        assert!(en.as_str().is_none());
        assert!(b.as_str().is_none());
        assert!(b.as_map().is_none());
        // `map_get` on a non-map is `None`, not a panic.
        assert!(a.map_get("x").is_none());
        assert_eq!(en.line(), 2);
        assert_eq!(b.line(), 3);
    }

    #[test]
    fn a_null_document_is_none() {
        assert!(p("~\n").unwrap().is_none());
        assert!(p("null\n").unwrap().is_none());
        assert!(p("NULL\n").unwrap().is_none());
        // A quoted `null` is a string, so it is a real document.
        assert!(p("\"null\"\n").unwrap().is_some());
        // A comment-only file holds no document at all.
        assert!(p("# nothing here\n").unwrap().is_none());
    }

    #[test]
    fn plain_style_is_distinguished_from_quoted() {
        let n = p("en:\n  plain: 1\n  quoted: \"1\"\n").unwrap().unwrap();
        let en = n.map_get("en").unwrap();
        assert!(en.map_get("plain").unwrap().is_plain());
        assert!(!en.map_get("quoted").unwrap().is_plain());
    }
}
