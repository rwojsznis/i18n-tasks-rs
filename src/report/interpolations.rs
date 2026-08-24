//! The two interpolation checks. Both share one scanner.
//!
//! ref: lib/i18n/tasks/interpolations.rb
//!
//! Blocker B2: the gem's `/(?<!%)%{[^}]+}/` needs a negative lookbehind, which
//! the `regex` crate does not have, so the scan is hand-rolled over the bytes.

use super::{KeyRow, Outcome, Reason, render_table};
use crate::config::{Config, IgnoreType};
use crate::data::load::Store;
use serde::Serialize;
use std::collections::BTreeSet;

/// `I18n.reserved_keys_pattern`, as a static list.
///
/// The `i18n` gem is not a dependency, so the 15 names are inlined.
pub const RESERVED_KEYS: &[&str] = &[
    "cascade",
    "deep_interpolation",
    "skip_interpolation",
    "default",
    "exception_handler",
    "fallback",
    "fallback_in_progress",
    "fallback_original_locale",
    "format",
    "object",
    "raise",
    "resolve",
    "scope",
    "separator",
    "throw",
];

/// Every `%{...}` in `value`, as the full match including the braces.
///
/// `%%{scope}` is an escaped `%` followed by a literal `{scope}`, so it must not
/// match. That is the negative lookbehind in `/(?<!%)%{[^}]+}/`.
pub fn variables(value: &str) -> Vec<&str> {
    let b = value.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + 2 < b.len() {
        if b[i] == b'%' && (i == 0 || b[i - 1] != b'%') && b[i + 1] == b'{' {
            // `[^}]+` needs at least one character.
            if let Some(rel) = b[i + 2..].iter().position(|&c| c == b'}')
                && rel > 0
            {
                let end = i + 2 + rel + 1;
                out.push(&value[i..end]);
                i = end;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// The variable name inside `%{name}`.
pub fn variable_name(matched: &str) -> &str {
    matched
        .trim_start_matches('%')
        .trim_start_matches('{')
        .trim_end_matches('}')
}

#[derive(Debug, Serialize)]
pub struct InterpolationReport {
    pub rows: Vec<KeyRow>,
    title: String,
}

impl InterpolationReport {
    pub fn outcome(&self) -> Outcome {
        Outcome::of(!self.rows.is_empty())
    }

    pub fn to_text(&self) -> String {
        let rows: Vec<Vec<String>> = self
            .rows
            .iter()
            .map(|r| vec![r.locale.clone(), r.key.clone(), r.details()])
            .collect();
        render_table(&self.title, &["Locale", "Key", "Detail"], &rows)
    }
}

/// Compares the variable set per key, base locale against each other locale.
///
/// ref: interpolations.rb#inconsistent_interpolations
pub fn inconsistent(cfg: &Config, store: &Store, locales: &[String]) -> InterpolationReport {
    let base = &store.base_locale;
    let ignore = cfg.ignore_patterns(IgnoreType::InconsistentInterpolations, None);
    let mut rows = Vec::new();
    let Some(base_tree) = store.tree(base) else {
        return InterpolationReport {
            rows,
            title: "Inconsistent interpolations".into(),
        };
    };
    for leaf in base_tree.sorted_keys() {
        let Some(value) = leaf.value.as_str() else {
            continue;
        };
        if !matches!(leaf.value, crate::data::load::Value::Str(_)) {
            continue;
        }
        if ignore.is_match(&leaf.key) {
            continue;
        }
        let base_vars: BTreeSet<&str> = variables(value).into_iter().collect();
        for locale in locales.iter().filter(|l| *l != base) {
            let Some(tree) = store.tree(locale) else {
                continue;
            };
            let Some(other) = tree.get(&leaf.key) else {
                continue;
            };
            let Some(other_value) = other.value.as_str() else {
                continue;
            };
            if !matches!(other.value, crate::data::load::Value::Str(_)) {
                continue;
            }
            let other_vars: BTreeSet<&str> = variables(other_value).into_iter().collect();
            if base_vars != other_vars {
                rows.push(KeyRow {
                    locale: locale.clone(),
                    key: leaf.key.clone(),
                    value: Some(other_value.to_string()),
                    reason: Some(Reason::Interpolations {
                        variables: owned(&other_vars),
                        base_locale: base.clone(),
                        base_variables: owned(&base_vars),
                    }),
                });
            }
        }
    }
    InterpolationReport {
        rows,
        title: "Inconsistent interpolations".into(),
    }
}

/// Values whose variables hit the reserved name list.
///
/// ref: interpolations.rb#reserved_interpolations
pub fn reserved(store: &Store, locales: &[String]) -> InterpolationReport {
    let mut rows = Vec::new();
    for locale in locales {
        let Some(tree) = store.tree(locale) else {
            continue;
        };
        for leaf in tree.sorted_keys() {
            let Some(value) = leaf.value.as_str() else {
                continue;
            };
            if !matches!(leaf.value, crate::data::load::Value::Str(_)) {
                continue;
            }
            let hits: Vec<&str> = variables(value)
                .into_iter()
                .map(variable_name)
                .filter(|n| RESERVED_KEYS.contains(n))
                .collect();
            if hits.is_empty() {
                continue;
            }
            rows.push(KeyRow {
                locale: locale.clone(),
                key: leaf.key.clone(),
                value: Some(value.to_string()),
                reason: Some(Reason::Reserved {
                    names: hits.iter().map(ToString::to_string).collect(),
                }),
            });
        }
    }
    InterpolationReport {
        rows,
        title: "Reserved interpolations".into(),
    }
}

/// The set as an owned, sorted list. `BTreeSet` already orders it, and the
/// text report joins it in that order.
fn owned(set: &BTreeSet<&str>) -> Vec<String> {
    set.iter().map(|v| (*v).to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_variables() {
        assert_eq!(variables("Hello %{name}!"), vec!["%{name}"]);
        assert_eq!(variables("%{a} and %{b}"), vec!["%{a}", "%{b}"]);
        assert_eq!(variables("no variables"), Vec::<&str>::new());
    }

    #[test]
    fn escaped_percent_does_not_match() {
        // `%%{scope}` is an escaped percent, so it is not an interpolation.
        assert_eq!(variables("%%{scope}"), Vec::<&str>::new());
        assert_eq!(variables("100%%{a}"), Vec::<&str>::new());
        assert_eq!(variables("%{a}%%{b}"), vec!["%{a}"]);
    }

    #[test]
    fn empty_braces_do_not_match() {
        assert_eq!(variables("%{}"), Vec::<&str>::new());
    }

    #[test]
    fn reserved_names_are_recognised() {
        let names: Vec<&str> = variables("%{scope} %{name} %{format}")
            .into_iter()
            .map(variable_name)
            .filter(|n| RESERVED_KEYS.contains(n))
            .collect();
        assert_eq!(names, vec!["scope", "format"]);
        assert_eq!(RESERVED_KEYS.len(), 15);
    }
}
