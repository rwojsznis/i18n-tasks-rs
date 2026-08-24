//! The used-key set, scanned once for every locale.
//!
//! Design decision 2 in `docs/design-notes.md`: the used-key set does not
//! depend on the
//! locale, but the gem recomputes `used_tree` per locale
//! (`unused_keys.rb:16`, `missing_keys.rb:111`), about 37% of `unused`, and
//! parses the whole source tree twice, strict and non-strict
//! (`used_keys.rb:143`), a further 22%.

use crate::config::Config;
use crate::discover::Finder;
use crate::pattern::PatternSet;
use crate::scan::{FileScan, Occurrence, ScanConfig, scan_file};
use rayon::prelude::*;
use std::collections::BTreeMap;

pub struct UsedKeys {
    /// Resolved keys and every place they are used, sorted by key. This is the
    /// only record of which keys are used; `key_used` reads it directly.
    pub keys: BTreeMap<String, Vec<Occurrence>>,
    /// Blocker B5: key patterns built from interpolated keys.
    pub patterns: PatternSet,
    pub pattern_sources: Vec<(String, Occurrence)>,
    /// Calls whose key cannot be determined at all.
    pub opaque: Vec<Occurrence>,
    pub files_scanned: usize,
    pub files_prefiltered: usize,
}

impl UsedKeys {
    /// True when the key itself or any ancestor is used.
    ///
    /// ref: lib/i18n/tasks/unused_keys.rb#key_used? and PR #721 — `t(:section)`
    /// covers `section.item.title`.
    pub fn key_used(&self, key: &str) -> bool {
        if self.keys.contains_key(key) {
            return true;
        }
        let mut k = key;
        while let Some(parent) = crate::keys::parent_key(k) {
            if self.keys.contains_key(parent) {
                return true;
            }
            k = parent;
        }
        false
    }

    pub fn scan(cfg: &Config) -> Result<UsedKeys, String> {
        let finder = Finder::new(cfg)?;
        let found = finder.discover();
        let scan_cfg = ScanConfig::from_config(cfg);
        // One task per file, on whichever thread pool the caller
        // installed. `scan_file` is a pure function of the bytes and the path,
        // so nothing is shared and nothing needs a lock.
        //
        // `collect` into a `Vec` keeps the results in file order, which is what
        // makes `--jobs N` byte-identical to `--jobs 1`: the sorts in
        // `from_scan` are stable, so two occurrences that share a path and a
        // position must still arrive in the same order.
        let per_file: Vec<FileScan> = found
            .files
            .par_iter()
            .map(|path| {
                let Ok(bytes) = std::fs::read(path) else {
                    return FileScan::default();
                };
                // Paths are reported relative to the config root, as the gem does.
                let rel = path.strip_prefix(&cfg.root).unwrap_or(path);
                scan_file(&bytes, rel, &scan_cfg)
            })
            .collect();
        let mut merged = FileScan::default();
        for scan in per_file {
            merged.merge(scan);
        }
        Ok(UsedKeys::from_scan(
            merged,
            found.files.len(),
            found.prefiltered,
        ))
    }

    pub fn from_scan(scan: FileScan, files_scanned: usize, files_prefiltered: usize) -> UsedKeys {
        let mut keys: BTreeMap<String, Vec<Occurrence>> = BTreeMap::new();
        for (key, occ) in scan.keys {
            keys.entry(key).or_default().push(occ);
        }
        for occs in keys.values_mut() {
            occs.sort_by(|a, b| (&a.path, a.pos).cmp(&(&b.path, b.pos)));
        }
        let mut pattern_sources = scan.patterns;
        pattern_sources.sort_by(|a, b| (&a.0, &a.1.path, a.1.pos).cmp(&(&b.0, &b.1.path, b.1.pos)));
        pattern_sources.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);
        let pattern_srcs: Vec<String> = {
            let mut v: Vec<String> = pattern_sources.iter().map(|(p, _)| p.clone()).collect();
            v.sort();
            v.dedup();
            v
        };
        let mut opaque = scan.opaque;
        opaque.sort_by(|a, b| (&a.path, a.pos).cmp(&(&b.path, b.pos)));

        UsedKeys {
            keys,
            patterns: PatternSet::new(&pattern_srcs),
            pattern_sources,
            opaque,
            files_scanned,
            files_prefiltered,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn used_with(keys: &[&str]) -> UsedKeys {
        let mut map: BTreeMap<String, Vec<Occurrence>> = BTreeMap::new();
        for key in keys {
            map.insert((*key).to_string(), Vec::new());
        }
        UsedKeys {
            keys: map,
            patterns: Default::default(),
            pattern_sources: Vec::new(),
            opaque: Vec::new(),
            files_scanned: 0,
            files_prefiltered: 0,
        }
    }

    /// `key_used` is a question about `keys`, so a `UsedKeys` built by hand —
    /// as `missing` and the report tests do — must answer it the same way one
    /// built by `from_scan` does.
    #[test]
    fn key_used_reads_the_key_map() {
        let used = used_with(&["a.b"]);
        assert!(used.key_used("a.b"), "the key itself");
        assert!(used.key_used("a.b.c"), "a descendant of a used key");
        assert!(!used.key_used("a"), "an ancestor is not itself used");
        assert!(!used.key_used("z"), "an unrelated key");
    }
}
