//! The statistics header.
//!
//! ref: lib/i18n/tasks/stats.rb
//!
//! The integer divisions and the `%.1f` are deliberate: the numbers must be
//! comparable against the gem's output, so they are not "improved" to floats.

use crate::data::load::Store;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct ForestStats {
    /// Locale names, comma-joined, base locale first.
    pub locales: String,
    /// Total leaf count across all locales.
    pub key_count: usize,
    pub locale_count: usize,
    /// `key_count / locale_count`, integer division.
    pub per_locale_avg: usize,
    /// Mean segment count per leaf, formatted `%.1f`.
    pub key_segments_avg: String,
    /// Mean value length in characters, integer division.
    pub value_chars_avg: usize,
}

#[allow(
    clippy::cast_precision_loss,
    reason = "the counts are leaf counts; 2^53 of them do not fit in memory"
)]
pub fn forest_stats(store: &Store, locales: &[String]) -> ForestStats {
    let mut key_count = 0usize;
    let mut segments = 0usize;
    let mut chars = 0usize;
    for locale in locales {
        let Some(tree) = store.tree(locale) else {
            continue;
        };
        for leaf in &tree.leaves {
            key_count += 1;
            // ref: `node.walk_to_root.count - 1`, which is the key depth
            // without the locale root.
            segments += leaf.depth as usize;
            // `value.to_s.length` counts characters, not bytes.
            chars += leaf.value.to_display_string().chars().count();
        }
    }
    let locale_count = locales.len().max(1);
    ForestStats {
        locales: locales.join(", "),
        key_count,
        locale_count,
        per_locale_avg: if key_count == 0 {
            0
        } else {
            key_count / locale_count
        },
        key_segments_avg: if key_count == 0 {
            "0.0".into()
        } else {
            format!("{:.1}", segments as f64 / key_count as f64)
        },
        value_chars_avg: chars.checked_div(key_count).unwrap_or(0),
    }
}

impl ForestStats {
    /// The gem prints this through `terminal_report.forest_stats`.
    pub fn to_text(&self) -> String {
        format!(
            "{} keys in {} locales ({}), {} keys per locale, \
             {} segments per key, {} characters per value",
            self.key_count,
            self.locale_count,
            self.locales,
            self.per_locale_avg,
            self.key_segments_avg,
            self.value_chars_avg
        )
    }
}
