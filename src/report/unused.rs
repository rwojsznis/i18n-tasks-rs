//! The `unused` report.
//!
//! ref: lib/i18n/tasks/unused_keys.rb

use super::{KeyRow, Outcome, render_table};
use crate::config::{Config, IgnoreType};
use crate::data::load::Store;
use crate::plural::depluralize_key;
use crate::scan::Occurrence;
use crate::used::UsedKeys;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct UnusedReport {
    pub rows: Vec<KeyRow>,
    /// Blocker B5: calls whose key cannot be verified at all.
    pub opaque: Vec<Occurrence>,
}

pub fn report(cfg: &Config, store: &Store, used: &UsedKeys, locales: &[String]) -> UnusedReport {
    let base = store.tree(&store.base_locale);
    let ignore = cfg.ignore_patterns(IgnoreType::Unused, None);
    let mut rows = Vec::new();
    for locale in locales {
        let Some(tree) = store.tree(locale) else {
            continue;
        };
        let mut hits: Vec<&str> = Vec::new();
        for leaf in tree.sorted_keys() {
            let key = leaf.key.as_str();
            if ignore.is_match(key) {
                continue;
            }
            // Blocker B5. The gem only consults its dynamic-key patterns when
            // `search.strict` is false, and the default is true, so by default
            // it never protects a dynamically built key at all.
            if used.patterns.is_match(key) {
                continue;
            }
            let dep = depluralize_key(key, Some(tree), base);
            if used.key_used(&dep) {
                continue;
            }
            // External keys are never unused.
            if store.external_has(locale, key) {
                continue;
            }
            hits.push(key);
        }
        for key in collapse_plural_nodes(tree, hits) {
            let value = tree.get(&key).map(|l| l.value.to_display_string());
            rows.push(KeyRow {
                locale: locale.clone(),
                key,
                value,
                details: None,
            });
        }
    }
    UnusedReport {
        rows,
        opaque: used.opaque.clone(),
    }
}

/// Replaces a complete set of unused plural children with their parent.
///
/// ref: lib/i18n/tasks/plural_keys.rb#collapse_plural_nodes!
fn collapse_plural_nodes(tree: &crate::data::load::LocaleTree, hits: Vec<&str>) -> Vec<String> {
    use std::collections::HashSet;
    let hit_set: HashSet<&str> = hits.iter().copied().collect();
    let mut collapsed: Vec<String> = Vec::with_capacity(hits.len());
    let mut done: HashSet<String> = HashSet::new();
    for key in hits {
        let Some(parent) = crate::keys::parent_key(key) else {
            collapsed.push(key.to_string());
            continue;
        };
        if !tree.is_plural_node(parent) {
            collapsed.push(key.to_string());
            continue;
        }
        // Collapse only when every plural child is unused.
        let all_unused = tree
            .children(parent)
            .iter()
            .all(|c| hit_set.contains(format!("{parent}.{c}").as_str()));
        if !all_unused {
            collapsed.push(key.to_string());
        } else if done.insert(parent.to_string()) {
            collapsed.push(parent.to_string());
        }
    }
    collapsed
}

impl UnusedReport {
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
                    r.value.clone().unwrap_or_default(),
                ]
            })
            .collect();
        let mut out = render_table("Unused keys", &["Locale", "Key", "Value"], &rows);
        if !self.opaque.is_empty() {
            out.push('\n');
            out.push_str(&format!(
                "{} translation call(s) have a key that cannot be determined statically. \
                 Keys they reach cannot be verified as used. Add an `# i18n-tasks-use` \
                 comment or an `ignore_unused` rule for each.\n",
                self.opaque.len()
            ));
            for occ in &self.opaque {
                out.push_str(&format!(
                    "  {}:{} {}\n",
                    occ.path.display(),
                    occ.line_num,
                    occ.snippet.lines().next().unwrap_or("")
                ));
            }
        }
        out
    }
}
