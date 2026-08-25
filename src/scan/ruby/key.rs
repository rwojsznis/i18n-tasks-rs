//! Key resolution: the key a `t` call names, given its scope.
//!
//! ref: lib/i18n/tasks/scanners/prism_scanners/nodes.rb

use super::CallCtx;

/// ref: nodes.rb TranslationCall#full_key
///
/// Returns the candidate list, most specific first, or `None` when the call
/// resolves to nothing.
pub(super) fn full_key(
    key: &str,
    scope: Option<&str>,
    ctx: &CallCtx,
    receiver_present: bool,
) -> Option<Vec<String>> {
    // ref: nodes.rb#relative_key?
    let relative = key.starts_with('.') && !receiver_present;
    if relative && !ctx.caps.relative {
        return None;
    }
    let base: Vec<String> = scope.map(|s| vec![s.to_string()]).unwrap_or_default();

    if relative && ctx.caps.candidate {
        // ref: nodes.rb:133-150 — progressively strip trailing path segments,
        // and never emit a bare unscoped key.
        let rel = &key[1..];
        let mut out = Vec::new();
        for keep in (1..=ctx.path.len()).rev() {
            let mut parts = base.clone();
            parts.extend(ctx.path[..keep].iter().cloned());
            parts.push(rel.to_string());
            out.push(join_key(&parts));
        }
        if out.is_empty() {
            return None;
        }
        Some(out)
    } else if relative {
        let mut parts = base;
        parts.extend(ctx.path.iter().cloned());
        parts.push(key[1..].to_string());
        Some(vec![join_key(&parts)])
    } else if let Some(stripped) = key.strip_prefix('.') {
        // A leading dot with an explicit receiver is not relative.
        let mut parts = base;
        parts.push(stripped.to_string());
        Some(vec![join_key(&parts)])
    } else {
        let mut parts = base;
        parts.push(key.to_string());
        Some(vec![join_key(&parts)])
    }
}

/// ref: nodes.rb — `.flatten.compact.join(".").gsub("..", ".")`
pub(in crate::scan) fn join_key(parts: &[String]) -> String {
    parts
        .iter()
        .filter(|p| !p.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join(".")
        .replace("..", ".")
}

/// A pattern with no static content would mark every key used.
///
/// ref: used_keys.rb#expr_key_re — `ignore_pattern_re = /\A[.*:]*\z/`
pub(in crate::scan) fn is_all_wildcard(pattern: &str) -> bool {
    pattern.chars().all(|c| c == '.' || c == '*' || c == ':')
}

/// ref: nodes.rb Root#path, generalised to any configured relative root (B6).
///
/// Strips the root, drops every extension from the file name and removes one
/// leading underscore from a partial.
pub(in crate::scan) fn template_path(posix_path: &str, root: &str) -> Vec<String> {
    let root = root.trim_end_matches('/');
    let marker = format!("{root}/");
    let Some(idx) = posix_path.rfind(&marker) else {
        return Vec::new();
    };
    let rest = &posix_path[idx + marker.len()..];
    let mut parts: Vec<String> = rest.split('/').map(str::to_string).collect();
    if let Some(name) = parts.pop() {
        let stem = name.split('.').next().unwrap_or("");
        let stem = stem.strip_prefix('_').unwrap_or(stem);
        parts.push(stem.to_string());
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::super::Caps;
    use super::*;
    use std::rc::Rc;

    /// `full_key` never returns an empty candidate list. With a context that
    /// has no path at all there is nothing to build a candidate from, so a
    /// relative key resolves to nothing rather than to a bare key.
    #[test]
    fn a_candidate_context_with_no_path_resolves_to_nothing() {
        let ctx = CallCtx {
            path: Rc::from([]),
            caps: Caps {
                relative: true,
                candidate: true,
            },
        };
        assert_eq!(full_key(".rel", None, &ctx, false), None);
        // An absolute key still resolves, path or no path.
        assert_eq!(
            full_key("abs", None, &ctx, false),
            Some(vec!["abs".to_string()])
        );
        // With a path, the candidates run from the most specific down, and
        // never as far as a bare key.
        let ctx = CallCtx {
            path: Rc::from(["events".to_string(), "create".to_string()]),
            caps: Caps {
                relative: true,
                candidate: true,
            },
        };
        assert_eq!(
            full_key(".rel", None, &ctx, false),
            Some(vec![
                "events.create.rel".to_string(),
                "events.rel".to_string()
            ])
        );
    }
    #[test]
    fn template_paths_drop_every_extension_and_the_partial_underscore() {
        // The view cases from spec/relative_keys_spec.rb.
        assert_eq!(
            template_path("app/views/movies/show.html.slim", "app/views"),
            vec!["movies", "show"]
        );
        assert_eq!(
            template_path("app/views-mobile/movies/show.html.slim", "app/views-mobile"),
            vec!["movies", "show"]
        );
        // A leading underscore marks a partial and is stripped once.
        assert_eq!(
            template_path("app/views/application/_event.html.erb", "app/views"),
            vec!["application", "event"]
        );
        assert_eq!(
            template_path("app/views/index.html.erb", "app/views"),
            vec!["index"]
        );
    }

    /// A root that is nowhere in the path yields no template path at all.
    #[test]
    fn a_path_outside_the_root_has_no_template_path() {
        assert!(template_path("lib/tasks/thing.rb", "app/views").is_empty());
        assert!(template_path("app/view/x.html.erb", "app/views").is_empty());
    }

    #[test]
    fn all_wildcard_patterns_are_rejected() {
        // ref: used_keys.rb#expr_key_re — /\A[.*:]*\z/
        assert!(is_all_wildcard("*:"));
        assert!(is_all_wildcard("*:.*:"));
        assert!(is_all_wildcard(".*:."));
        assert!(!is_all_wildcard("hash.*:"));
    }
    #[test]
    fn join_key_collapses_double_dots() {
        assert_eq!(join_key(&["a".into(), "b".into()]), "a.b");
        assert_eq!(join_key(&["a.".into(), "b".into()]), "a.b");
    }
}
