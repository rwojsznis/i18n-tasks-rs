//! The ERB scanner.
//!
//! ref: lib/i18n/tasks/scanners/erb_ast_scanner.rb
//!
//! Every code tag of a file is concatenated into one synthetic Ruby buffer,
//! which Prism parses **once** (design decision 3). The gem parses
//! each tag on its own (`erb_ast_scanner.rb:107`), so a view with 150 tags costs
//! 150 parses, and a block that spans two tags cannot parse at all — which is
//! why the gem needs the `ignore_blocks` hack in `local_ruby_parser.rb`. A
//! concatenated buffer keeps `<% if %>...<% end %>` intact, so the hack is
//! dropped.
//!
//! Two further departures, both recorded in `docs/accepted-diffs.md`:
//!
//! * The occurrence position comes from the source map, not from the gem's
//!   `code.index(key)` fallback at `erb_ast_scanner.rb:130`.
//! * The tag offset is the real start of the code group. The gem computes
//!   `match.begin(0) + 2 + character.size` with only the **first** character of
//!   the indicator, so `<%==` and `<%#-` tags are off by one.

use super::{FileScan, ScanConfig, SourceMap, ruby};
use crate::lineindex::LineIndex;
use regex::bytes::Regex;
use std::path::Path;
use std::sync::LazyLock;

/// ref: erb_ast_scanner.rb:13 `DEFAULT_REGEXP`
static TAG_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?s)<%(={1,2}|-|#-?|%)?(.*?)([-=])?%>").expect("static pattern compiles")
});

pub fn scan(bytes: &[u8], path: &Path, cfg: &ScanConfig) -> FileScan {
    let (buffer, map) = build_buffer(bytes);
    if buffer.is_empty() {
        return FileScan::default();
    }
    let index = LineIndex::new(bytes);
    ruby::scan_synthetic(&buffer, path, cfg, &index, &map)
}

/// One Ruby buffer for the whole file, plus the map back to file offsets.
///
/// A code tag is copied verbatim. A comment tag becomes a run of Ruby comment
/// lines, so the magic-comment path in the Ruby scanner sees it — the same job
/// the gem does with `code.gsub("i18n-tasks-use ", "#i18n-tasks-use ")` at
/// `erb_ast_scanner.rb:124`, but per line, so a multi-line comment tag keeps
/// every `i18n-tasks-use` in it.
fn build_buffer(bytes: &[u8]) -> (Vec<u8>, SourceMap) {
    let mut buf: Vec<u8> = Vec::with_capacity(bytes.len() / 2);
    let mut map = SourceMap::default();
    for caps in TAG_RE.captures_iter(bytes) {
        let indicator = caps.get(1).map(|m| m.as_bytes()[0]);
        let Some(code) = caps.get(2) else { continue };
        if code.as_bytes().iter().all(u8::is_ascii_whitespace) {
            // ref: erb_ast_scanner.rb:105 — an empty tag is skipped.
            continue;
        }
        match indicator {
            // ref: erb_ast_scanner.rb:94 — `"="`, `nil` and `"-"` are code.
            None | Some(b'=') | Some(b'-') => {
                map.push(buf.len(), code.len(), code.start());
                buf.extend_from_slice(code.as_bytes());
                buf.push(b'\n');
            }
            // A `#` indicator, `<%#` or `<%#-`, is a comment.
            Some(b'#') => push_comment(&mut buf, &mut map, code.as_bytes(), code.start()),
            // `<%%` is a literal `<%` in the output, never code.
            _ => {}
        }
    }
    (buf, map)
}

fn push_comment(buf: &mut Vec<u8>, map: &mut SourceMap, code: &[u8], file_start: usize) {
    let mut offset = 0;
    for line in code.split_inclusive(|b| *b == b'\n') {
        let text = line.strip_suffix(b"\n").unwrap_or(line);
        let text = text.strip_suffix(b"\r").unwrap_or(text);
        if !text.iter().all(u8::is_ascii_whitespace) {
            buf.push(b'#');
            map.push(buf.len(), text.len(), file_start + offset);
            buf.extend_from_slice(text);
            buf.push(b'\n');
        }
        offset += line.len();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buffer(src: &str) -> String {
        String::from_utf8(build_buffer(src.as_bytes()).0).unwrap()
    }

    #[test]
    fn concatenates_code_tags_into_one_buffer() {
        let src = "<%= link_to(x) do %>\n  <i></i>\n<% end %>\n";
        assert_eq!(buffer(src), " link_to(x) do \n end \n");
    }

    #[test]
    fn skips_literal_and_empty_tags() {
        assert_eq!(buffer("<%% not code %>"), "");
        assert_eq!(buffer("<%   %>"), "");
    }

    #[test]
    fn comment_tags_become_ruby_comment_lines() {
        let src = "<%# i18n-tasks-use t('a')\ni18n-tasks-use t('b') %>";
        assert_eq!(
            buffer(src),
            "# i18n-tasks-use t('a')\n#i18n-tasks-use t('b') \n"
        );
    }

    #[test]
    fn positions_map_back_to_the_file() {
        let src = "<div><%= t('a') %></div>\n<% t('b') %>\n";
        let (buf, map) = build_buffer(src.as_bytes());
        let buf = String::from_utf8(buf).unwrap();
        let a = buf.find("t('a')").unwrap();
        let b = buf.find("t('b')").unwrap();
        assert_eq!(map.translate(a), src.find("t('a')").unwrap());
        assert_eq!(map.translate(b), src.find("t('b')").unwrap());
    }

    /// An ERB file with no code tag at all costs no Prism parse.
    #[test]
    fn a_file_with_no_code_tag_is_skipped() {
        use crate::scan::ScanConfig;
        let cfg = ScanConfig {
            relative_roots: vec!["app/views".into()],
            relative_exclude_method_name_paths: vec![],
        };
        let out = scan(
            b"<h1>Hello</h1>\n<%% literal %>\n",
            Path::new("app/views/x/index.html.erb"),
            &cfg,
        );
        assert!(out.keys.is_empty());
        assert!(out.patterns.is_empty());
        assert!(out.opaque.is_empty());
    }

    #[test]
    fn comment_marker_maps_to_the_start_of_the_comment_text() {
        let src = "x\n<%# i18n-tasks-use t('a') %>\n";
        let (buf, map) = build_buffer(src.as_bytes());
        // The inserted `#` is filler, so it resolves to the first real byte.
        assert_eq!(String::from_utf8(buf).unwrap().as_bytes()[0], b'#');
        assert_eq!(map.translate(0), src.find(" i18n-tasks-use").unwrap());
    }
}
