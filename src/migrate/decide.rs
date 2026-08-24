//! What happens to one config key.
//!
//! A setting this port has no equivalent for is dropped with its reason, and
//! the reason is what the generated header reports. `data` and `search` are
//! filtered one child at a time, so a section keeps the settings that do exist.

use super::Dropped;
use super::lines::{Extent, extents, slice};
use crate::yaml::{self, Node};
use std::path::Path;

/// What to do with one config key.
pub(super) enum Decision {
    Keep,
    /// Filter the children instead. `data` and `search` only.
    Recurse,
    Drop(String),
}

/// `section` is `None` at the top level, otherwise the parent key.
#[allow(
    clippy::match_same_arms,
    reason = "the arms are grouped by config section, which merging would scramble"
)]
pub(super) fn decide(key: &str, value: &Node, section: Option<&str>) -> Decision {
    // A key with no value is the gem template's way of showing an example, e.g.
    // `external:` followed by commented-out samples. Keeping it would hand the
    // tool a list holding one empty path.
    if yaml::is_null_scalar(value) {
        return Decision::Drop("it had no value".into());
    }
    let drop = |reason: &str| Decision::Drop(reason.to_string());
    match (section, key) {
        (None, "base_locale" | "locales" | "ignore") => Decision::Keep,
        (
            None,
            "ignore_missing"
            | "ignore_unused"
            | "ignore_eq_base"
            | "ignore_inconsistent_interpolations",
        ) => Decision::Keep,
        (None, "data" | "search") => Decision::Recurse,
        (None, "internal_locale") => drop("reports are English only"),
        (None, "translation") => drop("translation backends are out of scope"),

        (Some("data"), "read" | "write" | "external" | "keep_order") => Decision::Keep,
        (Some("data"), "router") => match value.as_str() {
            Some("conservative_router" | "pattern_router") => Decision::Keep,
            Some(other) => Decision::Drop(format!(
                "`{other}` is not one of conservative_router, pattern_router; \
                 the default conservative_router applies"
            )),
            None => drop("the router must be a name"),
        },
        (Some("data"), "adapter") => drop("the only data adapter is the YAML file system"),
        (Some("data"), "yaml") => {
            drop("the emitter has no options; it never folds lines (blocker B1)")
        }
        (Some("data"), "json") => drop("JSON locale files are not supported"),

        (Some("search"), "paths" | "exclude" | "only" | "relative_roots") => Decision::Keep,
        (Some("search"), "relative_exclude_method_name_paths") => Decision::Keep,
        (Some("search"), "scanners") => {
            drop("scanners are built in and picked by file extension; no class names")
        }
        (Some("search"), "prism") => {
            drop("Prism is the only Ruby parser and Rails detection is always on")
        }
        (Some("search"), "strict") => {
            drop("a dynamic key always becomes a pattern; there is no strict mode (blocker B5)")
        }
        (Some("search"), "ast_matchers") => {
            drop("the Rails model and mailer-subject matchers are always on")
        }
        _ => drop("i18n-tasks-rs has no such setting"),
    }
}

/// Renders `data` or `search` with the unsupported children removed. Returns
/// `None` when nothing under it survived.
pub(super) fn nested(
    lines: &[&str],
    section: &str,
    parent_line: usize,
    value: &Node,
    extent: &Extent,
    from: &Path,
    dropped: &mut Vec<Dropped>,
) -> Result<Option<String>, String> {
    let entries = value.as_map().ok_or_else(|| {
        format!(
            "{}:{parent_line}: `{section}` must be a mapping",
            from.display()
        )
    })?;
    let starts: Vec<usize> = entries.iter().map(|(k, _)| k.line()).collect();
    // A flow mapping puts the children on the parent's line, so there are no
    // line ranges to slice. Rare enough to refuse rather than re-serialize.
    if starts.iter().any(|&s| s <= parent_line) {
        return Err(format!(
            "{}:{parent_line}: `{section}` is written in flow style (`{{a: b}}`). \
             Rewrite it as an indented block and run the migration again.",
            from.display()
        ));
    }
    let child_extents = extents(lines, &starts, parent_line + 1, extent.body_end);

    let mut kept_blocks = Vec::new();
    for (i, (k, v)) in entries.iter().enumerate() {
        let name = k
            .as_str()
            .ok_or_else(|| format!("{}: non-string config key", from.display()))?;
        let path = format!("{section}.{name}");
        match decide(name, v, Some(section)) {
            Decision::Keep => kept_blocks.push(slice(
                lines,
                child_extents[i].lead_start,
                child_extents[i].body_end,
            )),
            // Neither section has a third level, so `Recurse` cannot appear.
            Decision::Recurse | Decision::Drop(_) => {
                let Decision::Drop(reason) = decide(name, v, Some(section)) else {
                    unreachable!("no config section nests twice")
                };
                dropped.push(Dropped {
                    key: path,
                    line: k.line(),
                    reason,
                });
            }
        }
    }
    if kept_blocks.is_empty() {
        return Ok(None);
    }
    // The section header keeps its own leading comments, and only those: the
    // comments that sat above a dropped child leave with it.
    let mut out = slice(lines, extent.lead_start, parent_line);
    for block in kept_blocks {
        out.push('\n');
        out.push_str(&block);
    }
    Ok(Some(out))
}
