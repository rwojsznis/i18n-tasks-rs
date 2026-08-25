//! Magic comments — `# i18n-tasks-use t('a')`.
//!
//! The free functions live here; the `Visitor` methods that drive them stay
//! with the visitor, because they need its scope ranges.
//!
//! ref: lib/i18n/tasks/scanners/ruby_scanner.rb

use super::args::{ArgVal, process_arguments};
use super::nodes::{is_i18n_receiver, is_translation_name};
use ruby_prism as pr;
use ruby_prism::Visit as _;

/// ref: visitor.rb#MAGIC_COMMENT_PREFIX
const MAGIC_COMMENT_MARKER: &str = "i18n-tasks-use";

/// ref: visitor.rb#MAGIC_COMMENT_PREFIX = /\A.\s*i18n-tasks-use\s+/
///
/// The leading `.` matches the comment's own `#`.
pub(super) fn strip_magic_prefix(text: &str) -> Option<&str> {
    let mut chars = text.char_indices();
    let (_, _first) = chars.next()?;
    let rest_start = chars.clone().next().map_or(text.len(), |(i, _)| i);
    let rest = &text[rest_start..];
    let trimmed = rest.trim_start_matches([' ', '\t']);
    let payload = trimmed.strip_prefix(MAGIC_COMMENT_MARKER)?;
    // The prefix requires at least one space after the marker.
    if !payload.starts_with([' ', '\t']) {
        return None;
    }
    Some(payload.trim_start())
}

/// ref: ruby_scanner.rb:87 — `split(/\s+(?=t)/)`
pub(super) fn split_calls(payload: &str) -> Vec<&str> {
    let bytes = payload.as_bytes();
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i].is_ascii_whitespace() {
            let ws_start = i;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b't' && ws_start > start {
                out.push(&payload[start..ws_start]);
                start = i;
            }
        } else {
            i += 1;
        }
    }
    if start < payload.len() {
        out.push(&payload[start..]);
    }
    if out.is_empty() {
        out.push(payload);
    }
    out
}

/// A `t`-family call reduced to owned data.
///
/// Blocker B9: node lifetimes end with the `ParseResult`, so everything is
/// extracted during the visit.
pub(super) struct ExtractedCall {
    pub(super) args: Vec<ArgVal>,
    pub(super) kwargs: Vec<(String, ArgVal)>,
    pub(super) receiver_present: bool,
    pub(super) receiver_is_i18n: bool,
}

/// Collects the `t`-family calls in a nested (magic comment) parse.
pub(super) fn collect_translation_calls(node: &pr::Node) -> Vec<ExtractedCall> {
    struct Collector {
        found: Vec<ExtractedCall>,
    }
    impl<'pr> pr::Visit<'pr> for Collector {
        fn visit_call_node(&mut self, node: &pr::CallNode<'pr>) {
            if is_translation_name(node.name().as_slice()) {
                let (args, kwargs) = process_arguments(node);
                let receiver = node.receiver();
                self.found.push(ExtractedCall {
                    args,
                    kwargs,
                    receiver_present: receiver.is_some(),
                    receiver_is_i18n: receiver.as_ref().is_some_and(is_i18n_receiver),
                });
            }
            pr::visit_call_node(self, node);
        }
    }
    let mut c = Collector { found: Vec::new() };
    c.visit(node);
    c.found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn magic_comment_prefix() {
        // ref: /\A.\s*i18n-tasks-use\s+/
        assert_eq!(
            strip_magic_prefix("# i18n-tasks-use t('a')"),
            Some("t('a')")
        );
        assert_eq!(strip_magic_prefix("#i18n-tasks-use t('a')"), Some("t('a')"));
        assert_eq!(
            strip_magic_prefix("#   i18n-tasks-use   t('a')"),
            Some("t('a')")
        );
        assert_eq!(strip_magic_prefix("# not a magic comment"), None);
        // The marker needs whitespace after it.
        assert_eq!(strip_magic_prefix("# i18n-tasks-uset('a')"), None);
    }
    #[test]
    fn splits_several_calls_in_one_comment() {
        // ref: ruby_scanner.rb:87 — split(/\s+(?=t)/)
        assert_eq!(split_calls("t('a') t('b')"), vec!["t('a')", "t('b')"]);
        assert_eq!(split_calls("t('a')"), vec!["t('a')"]);
        // The fallback: nothing to split, so the payload comes back whole.
        assert_eq!(split_calls(""), vec![""]);
        // Only whitespace directly before a `t` splits, so a `scope:` argument
        // on the same line stays with its call.
        assert_eq!(split_calls("t('a', scope: :x)"), vec!["t('a', scope: :x)"]);
        assert_eq!(
            split_calls("t('a', scope: :x) t('b')"),
            vec!["t('a', scope: :x)", "t('b')"]
        );
    }
}
