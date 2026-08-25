//! Removing the ERB.
//!
//! Blocker B3: the gem evaluates its config as ERB and then as Ruby, so a real
//! config can `require` a scanner, shell out, or boot Rails. Nothing here
//! evaluates anything. A tag is cut out, and a line whose *value* was computed
//! by one is reported for a human instead.

use super::Manual;
use super::lines::is_comment;

pub(super) struct Stripped {
    /// The source with every ERB line blanked, so line numbers still match.
    pub(super) text: String,
    pub(super) erb_lines: Vec<usize>,
    pub(super) manual: Vec<Manual>,
}

/// Removes the ERB, one line at a time.
///
/// A line that held nothing but an ERB tag is dropped outright — that is the
/// `<% require ... %>` prelude and its kind. A line that mixed ERB into a YAML
/// value cannot be migrated at all, because the value was computed by code, so
/// it is dropped and reported for a human. Comments are left alone unless they
/// contain `<%`, which [`Config::parse`](crate::config::Config::parse) rejects
/// wherever it appears.
///
/// Blanked lines keep their place in the file so that every line number in
/// every message still refers to the original.
pub(super) fn strip_erb(src: &str) -> Stripped {
    let mut text = String::with_capacity(src.len());
    let mut erb_lines = Vec::new();
    let mut manual = Vec::new();
    // Set while an ERB tag opened on an earlier line is still open.
    let mut open = false;

    for (idx, raw) in src.lines().enumerate() {
        let line_no = idx + 1;
        let mut rest = raw;
        let had_erb = open || raw.contains("<%");

        if !had_erb {
            text.push_str(raw);
            text.push('\n');
            continue;
        }

        // Whatever is left of the line once every ERB tag is cut out.
        let mut kept = String::new();
        if open {
            match rest.find("%>") {
                Some(pos) => {
                    rest = &rest[pos + 2..];
                    open = false;
                }
                None => {
                    erb_lines.push(line_no);
                    text.push('\n');
                    continue;
                }
            }
        }
        loop {
            match rest.find("<%") {
                None => {
                    kept.push_str(rest);
                    break;
                }
                Some(pos) => {
                    kept.push_str(&rest[..pos]);
                    match rest[pos..].find("%>") {
                        Some(end) => rest = &rest[pos + end + 2..],
                        None => {
                            open = true;
                            break;
                        }
                    }
                }
            }
        }

        erb_lines.push(line_no);
        text.push('\n');
        // A comment that held ERB simply goes: `Config::parse` rejects `<%`
        // wherever it appears, comments included. A *value* built by ERB is a
        // different matter — no one can guess what the code returned.
        if !kept.trim().is_empty() && !is_comment(raw) {
            manual.push(Manual {
                line: line_no,
                text: raw.trim().to_string(),
            });
        }
    }
    Stripped {
        text,
        erb_lines,
        manual,
    }
}

/// Every ERB tag in `text` becomes `[ERB]`, so the line can be quoted in a
/// config this tool will read back.
pub(super) fn redact_erb(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(pos) = rest.find("<%") {
        out.push_str(&rest[..pos]);
        out.push_str("[ERB]");
        rest = match rest[pos..].find("%>") {
            Some(end) => &rest[pos + end + 2..],
            None => "",
        };
    }
    out.push_str(rest);
    out
}
