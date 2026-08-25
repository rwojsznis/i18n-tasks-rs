//! `find`: every used-key occurrence, in text and in JSON.
//!
//! Not a check — it reports no finding and always exits 0. It exists so this
//! tool and the gem can be compared over the same project, which is why the
//! JSON is an interface and this module is tested rather than inlined in the
//! CLI.

use crate::scan::Occurrence;
use crate::used::UsedKeys;
use serde::Serialize;

/// The key, then one indented `path:line:column` per occurrence, then the key
/// patterns and the opaque calls.
pub fn to_text(used: &UsedKeys) -> String {
    let mut out = String::new();
    for (key, occs) in &used.keys {
        out.push_str(&format!("{key}\n"));
        for o in occs {
            out.push_str(&format!(
                "  {}:{}:{}\n",
                o.path.display(),
                o.line_num,
                o.line_pos
            ));
        }
    }
    for (pattern, occ) in &used.pattern_sources {
        out.push_str(&format!("{pattern}  (pattern)\n"));
        out.push_str(&format!("  {}:{}\n", occ.path.display(), occ.line_num));
    }
    for o in &used.opaque {
        out.push_str(&format!("(opaque)  {}:{}\n", o.path.display(), o.line_num));
    }
    out
}

/// # Errors
///
/// The occurrence set does not serialize.
pub fn to_json(used: &UsedKeys, config_digest: &str) -> Result<String, String> {
    let keys = used
        .keys
        .iter()
        .map(|(key, occs)| Row {
            key,
            occurrences: occs.iter().map(Loc::of).collect(),
        })
        .collect();
    // One entry per distinct pattern: several interpolated calls can build the
    // same one, and the list is the set, not the sources.
    let mut patterns: Vec<&str> = used
        .pattern_sources
        .iter()
        .map(|(p, _)| p.as_str())
        .collect();
    patterns.sort_unstable();
    patterns.dedup();
    let out = Out {
        check: "find",
        config_digest,
        files_scanned: used.files_scanned,
        files_prefiltered: used.files_prefiltered,
        keys,
        patterns,
        opaque: used.opaque.iter().map(Loc::of).collect(),
    };
    serde_json::to_string_pretty(&out).map_err(|e| e.to_string())
}

#[derive(Serialize)]
struct Out<'a> {
    check: &'static str,
    config_digest: &'a str,
    files_scanned: usize,
    files_prefiltered: usize,
    keys: Vec<Row<'a>>,
    patterns: Vec<&'a str>,
    opaque: Vec<Loc<'a>>,
}

#[derive(Serialize)]
struct Row<'a> {
    key: &'a str,
    occurrences: Vec<Loc<'a>>,
}

/// One occurrence. `path` is a `String` rather than the `Arc<Path>` it comes
/// from, because the JSON is an interface and `Path` has no one serialization.
#[derive(Serialize)]
struct Loc<'a> {
    path: String,
    line: usize,
    column: usize,
    raw_key: &'a str,
    candidate_keys: &'a [String],
}

impl<'a> Loc<'a> {
    fn of(o: &'a Occurrence) -> Loc<'a> {
        Loc {
            path: o.path.display().to_string(),
            line: o.line_num,
            column: o.line_pos,
            raw_key: &o.raw_key,
            candidate_keys: &o.candidate_keys,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{to_json, to_text};
    use crate::pattern::PatternSet;
    use crate::scan::Occurrence;
    use crate::used::UsedKeys;
    use std::collections::BTreeMap;
    use std::path::Path;
    use std::sync::Arc;

    fn occurrence(path: &str, line: usize, key: &str) -> Occurrence {
        Occurrence {
            path: Arc::from(Path::new(path)),
            snippet: format!("t('{key}')"),
            pos: 0,
            line_pos: 3,
            line_num: line,
            raw_key: key.to_string(),
            candidate_keys: vec![key.to_string()],
        }
    }

    fn used() -> UsedKeys {
        let mut keys = BTreeMap::new();
        keys.insert(
            "a.b".to_string(),
            vec![
                occurrence("app/a.rb", 2, "a.b"),
                occurrence("app/b.rb", 7, "a.b"),
            ],
        );
        UsedKeys {
            keys,
            patterns: PatternSet::new(&["p.*".to_string()]),
            pattern_sources: vec![("p.*".to_string(), occurrence("app/c.rb", 4, "p.x"))],
            opaque: vec![occurrence("app/d.rb", 9, "")],
            files_scanned: 4,
            files_prefiltered: 1,
        }
    }

    #[test]
    fn the_text_form_indents_the_occurrences_under_the_key() {
        assert_eq!(
            to_text(&used()),
            "a.b\n  app/a.rb:2:3\n  app/b.rb:7:3\np.*  (pattern)\n  app/c.rb:4\n(opaque)  app/d.rb:9\n"
        );
    }

    #[test]
    fn the_json_form_lists_every_pattern_once_and_sorted() {
        let mut u = used();
        u.pattern_sources
            .push(("b.*".to_string(), occurrence("app/e.rb", 1, "b.x")));
        u.pattern_sources
            .push(("p.*".to_string(), occurrence("app/f.rb", 1, "p.y")));
        let json = to_json(&u, "digest").expect("the envelope serializes");
        let patterns = json
            .split("\"patterns\": [")
            .nth(1)
            .and_then(|s| s.split(']').next())
            .expect("the patterns array is there");
        assert_eq!(
            patterns.matches('"').count() / 2,
            2,
            "`p.*` appears twice in the sources and once here: {patterns}"
        );
        assert!(
            patterns.find("\"b.*\"") < patterns.find("\"p.*\""),
            "sorted: {patterns}"
        );
    }
}
