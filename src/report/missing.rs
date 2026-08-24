//! The `missing` report: types `used`, `diff` and `plural`.
//!
//! ref: lib/i18n/tasks/missing_keys.rb

use super::{KeyRow, Outcome, Reason, render_table};
use crate::config::{Config, IgnoreType};
use crate::data::load::Store;
use crate::plural::{depluralize_key, required_categories};
use crate::used::UsedKeys;
use serde::Serialize;
use std::collections::BTreeSet;

/// The three `--types` values. `ValueEnum` so the valid set is written once:
/// clap parses the flag, lists the values in `--help` and reports a bad one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum MissingType {
    Used,
    Diff,
    Plural,
}

impl MissingType {
    pub const ALL: [MissingType; 3] = [MissingType::Used, MissingType::Diff, MissingType::Plural];
}

#[derive(Debug, Serialize)]
pub struct MissingReport {
    pub rows: Vec<KeyRow>,
}

/// ref: missing_keys.rb#locale_key_missing?
fn locale_key_missing(cfg: &Config, store: &Store, locale: &str, key: &str) -> bool {
    !store.key_value(locale, key)
        && !store.external_has(locale, key)
        && !cfg
            .ignore_patterns(IgnoreType::Missing, Some(locale))
            .is_match(key)
}

pub fn report(
    cfg: &Config,
    store: &Store,
    used: &UsedKeys,
    locales: &[String],
    types: &[MissingType],
) -> MissingReport {
    let mut rows = Vec::new();
    for ty in types {
        match ty {
            MissingType::Used => rows.extend(missing_used(cfg, store, used, locales)),
            MissingType::Diff => rows.extend(missing_diff(cfg, store, locales)),
            MissingType::Plural => rows.extend(missing_plural(cfg, store, locales)),
        }
    }
    MissingReport { rows }
}

/// A scanned key with no value in the locale.
///
/// ref: missing_keys.rb#missing_used_tree (lines 110-130). An occurrence counts
/// as present when *any* of its candidate keys resolves, and the key is
/// reported only when *every* occurrence is missing.
fn missing_used(cfg: &Config, store: &Store, used: &UsedKeys, locales: &[String]) -> Vec<KeyRow> {
    let mut rows = Vec::new();
    // Compile the ignore set once per locale rather than once per key.
    for locale in locales {
        let ignore = cfg.ignore_patterns(IgnoreType::Missing, Some(locale));
        for (key, occurrences) in &used.keys {
            let all_missing = occurrences.iter().all(|occ| {
                let candidates: &[String] = if occ.candidate_keys.is_empty() {
                    std::slice::from_ref(key)
                } else {
                    &occ.candidate_keys
                };
                candidates.iter().all(|c| {
                    !store.key_value(locale, c)
                        && !store.external_has(locale, c)
                        && !ignore.is_match(c)
                })
            });
            if all_missing {
                let first = &occurrences[0];
                rows.push(KeyRow {
                    locale: locale.clone(),
                    key: key.clone(),
                    value: None,
                    reason: Some(Reason::Used {
                        path: first.path.clone(),
                        line: first.line_num,
                    }),
                });
            }
        }
    }
    rows
}

/// Present in the compared locale, absent here.
///
/// ref: missing_keys.rb#missing_diff_forest
fn missing_diff(cfg: &Config, store: &Store, locales: &[String]) -> Vec<KeyRow> {
    let base = &store.base_locale;
    let mut rows = Vec::new();
    let push_diff = |locale: &str, compared_to: &str, rows: &mut Vec<KeyRow>| {
        let Some(source) = store.tree(compared_to) else {
            return;
        };
        let source_base = store.tree(base);
        for leaf in source.sorted_keys() {
            let key = depluralize_key(&leaf.key, Some(source), source_base);
            if locale_key_missing(cfg, store, locale, &key) {
                rows.push(KeyRow {
                    locale: locale.to_string(),
                    key,
                    value: Some(leaf.value.to_display_string()),
                    reason: Some(Reason::Diff {
                        present_in: compared_to.to_string(),
                    }),
                });
            }
        }
    };
    // Present in base but not in the locale.
    for locale in locales.iter().filter(|l| *l != base) {
        push_diff(locale, base, &mut rows);
    }
    // Present in another locale but not in base.
    if locales.iter().any(|l| l == base) {
        for locale in store.locales.iter().filter(|l| *l != base) {
            push_diff(base, locale, &mut rows);
        }
    }
    // The two directions can report the same key twice.
    let mut seen = BTreeSet::new();
    rows.retain(|r| seen.insert((r.locale.clone(), r.key.clone())));
    rows
}

/// A plural node missing a required CLDR category.
///
/// ref: missing_keys.rb#missing_plural_forest
fn missing_plural(cfg: &Config, store: &Store, locales: &[String]) -> Vec<KeyRow> {
    let mut rows = Vec::new();
    for locale in locales {
        let Some(required) = required_categories(locale) else {
            continue;
        };
        let Some(tree) = store.tree(locale) else {
            continue;
        };
        let ignore = cfg.ignore_patterns(IgnoreType::Missing, Some(locale));
        for key in tree.sorted_plural_nodes() {
            if ignore.is_match(key) {
                continue;
            }
            let present: Vec<&str> = tree.children(key).iter().map(String::as_str).collect();
            let absent: Vec<&'static str> = required
                .iter()
                .copied()
                .filter(|r| !present.contains(r))
                .collect();
            if absent.is_empty() {
                continue;
            }
            rows.push(KeyRow {
                locale: locale.clone(),
                key: key.to_string(),
                value: None,
                reason: Some(Reason::Plural { categories: absent }),
            });
        }
    }
    rows
}

impl MissingReport {
    pub fn outcome(&self) -> Outcome {
        Outcome::of(!self.rows.is_empty())
    }

    pub fn to_text(&self) -> String {
        let rows: Vec<Vec<String>> = self
            .rows
            .iter()
            .map(|r| {
                vec![
                    r.locale.clone(),
                    r.key.clone(),
                    r.details(),
                    r.value.clone().unwrap_or_default(),
                ]
            })
            .collect();
        render_table(
            "Missing keys",
            &["Locale", "Key", "Type", "Base value"],
            &rows,
        )
    }
}
