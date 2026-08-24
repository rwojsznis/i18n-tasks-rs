//! Keys whose value is the same as in the base locale.
//!
//! ref: lib/i18n/tasks/missing_keys.rb#eq_base_keys
//! ref: lib/i18n/tasks/missing_keys.rb#equal_values_tree

use super::{KeyRow, Outcome, render_table};
use crate::config::{Config, IgnoreType};
use crate::data::load::{Store, Value};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct EqBaseReport {
    pub rows: Vec<KeyRow>,
    #[serde(skip)]
    base_locale: String,
}

pub fn report(cfg: &Config, store: &Store, locales: &[String]) -> EqBaseReport {
    let Some(base) = store.tree(&store.base_locale) else {
        return EqBaseReport {
            rows: Vec::new(),
            base_locale: store.base_locale.clone(),
        };
    };
    let mut rows = Vec::new();
    for locale in locales.iter().filter(|l| l.as_str() != store.base_locale) {
        let Some(tree) = store.tree(locale) else {
            continue;
        };
        let ignore = cfg.ignore_patterns(IgnoreType::EqBase, Some(locale));
        for leaf in tree.sorted_keys() {
            if ignore.is_match(&leaf.key) {
                continue;
            }
            let Some(base_leaf) = base.get(&leaf.key) else {
                continue;
            };
            if values_equal(&leaf.value, &base_leaf.value) {
                rows.push(KeyRow {
                    locale: locale.clone(),
                    key: leaf.key.clone(),
                    value: Some(leaf.value.to_display_string()),
                    reason: None,
                });
            }
        }
    }
    EqBaseReport {
        rows,
        base_locale: store.base_locale.clone(),
    }
}

/// Ruby `Hash#==` ignores insertion order. Mappings can occur inside sequence
/// leaves, where the loader preserves source order for stable emission.
fn values_equal(value: &Value, base: &Value) -> bool {
    match (value, base) {
        (Value::Seq(a), Value::Seq(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| values_equal(x, y))
        }
        (Value::Map(a), Value::Map(b)) => {
            a.len() == b.len()
                && a.iter().all(|(key, value)| {
                    b.iter()
                        .find(|(base_key, _)| base_key == key)
                        .is_some_and(|(_, base_value)| values_equal(value, base_value))
                })
        }
        _ => value == base,
    }
}

impl EqBaseReport {
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
        render_table(
            &format!("Same value as {}", self.base_locale),
            &["Locale", "Key", "Value"],
            &rows,
        )
    }
}
