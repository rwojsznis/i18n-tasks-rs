//! The used-key set, scanned once for every locale.
//!
//! Design decision 2 in `docs/design-notes.md`: the used-key set does not
//! depend on the locale, but the gem recomputes `used_tree` per locale
//! (`unused_keys.rb:16`, `missing_keys.rb:111`), about 37% of `unused`, and
//! parses the whole source tree twice, strict and non-strict
//! (`used_keys.rb:143`), a further 22%.

use crate::config::Config;
use crate::discover::{Finder, read_source};
use crate::pattern::PatternSet;
use crate::scan::{FileScan, Occurrence, ScanConfig, scan_file};
use rayon::prelude::*;
use std::collections::BTreeMap;

#[derive(Debug)]
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

/// What one candidate file turned out to be.
///
/// Collected in file order, so the counts and the merge order do not depend on
/// how the work was spread over the pool.
enum Verdict {
    Scanned(FileScan),
    /// Held none of the needles, so it cannot hold a translation call.
    Prefiltered,
    /// Could not be read, so it is no evidence either way and is counted as
    /// neither scanned nor prefiltered.
    Unreadable,
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

    /// # Errors
    ///
    /// A `search.only` or `search.exclude` entry is not a valid glob. A file
    /// that cannot be read is skipped, not an error: the gem's `Find.find`
    /// walk does the same.
    pub fn scan(cfg: &Config) -> Result<UsedKeys, String> {
        let finder = Finder::new(cfg)?;
        let candidates = finder.discover();
        let scan_cfg = ScanConfig::from_config(cfg);
        // One task per file, on whichever thread pool the caller
        // installed. `scan_file` is a pure function of the bytes and the path,
        // so nothing is shared and nothing needs a lock.
        //
        // The needle prefilter runs here rather than in `discover`, so that a
        // candidate is read once and the same bytes answer both questions.
        //
        // `collect` into a `Vec` keeps the results in file order, which is what
        // makes `--jobs N` byte-identical to `--jobs 1`: the sorts in
        // `from_scan` are stable, so two occurrences that share a path and a
        // position must still arrive in the same order.
        let per_file: Vec<Verdict> = candidates
            .par_iter()
            .map(|path| {
                let Some(bytes) = read_source(path) else {
                    return Verdict::Unreadable;
                };
                if !finder.prefilter_matches(&bytes) {
                    return Verdict::Prefiltered;
                }
                // Paths are reported relative to the config root, as the gem does.
                let rel = path.strip_prefix(&cfg.root).unwrap_or(path);
                Verdict::Scanned(scan_file(&bytes, rel, &scan_cfg))
            })
            .collect();
        let mut merged = FileScan::default();
        let mut files_scanned = 0;
        let mut files_prefiltered = 0;
        for verdict in per_file {
            match verdict {
                Verdict::Scanned(scan) => {
                    files_scanned += 1;
                    merged.merge(scan);
                }
                Verdict::Prefiltered => files_prefiltered += 1,
                Verdict::Unreadable => {}
            }
        }
        Ok(UsedKeys::from_scan(
            merged,
            files_scanned,
            files_prefiltered,
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
            patterns: PatternSet::default(),
            pattern_sources: Vec::new(),
            opaque: Vec::new(),
            files_scanned: 0,
            files_prefiltered: 0,
        }
    }

    /// Each candidate file is read once.
    ///
    /// The prefilter used to read every candidate itself and the scan then read
    /// the survivors again, so a file holding a translation call cost two
    /// syscalls and two copies of its bytes.
    #[test]
    fn a_candidate_file_is_read_once() {
        let root = std::env::temp_dir().join("i18n-tasks-rs-used-one-read");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("app")).unwrap();
        std::fs::write(root.join("app/loud.rb"), "t('a.b')\n").unwrap();
        std::fs::write(root.join("app/quiet.rb"), "1 + 1\n").unwrap();
        let body = "search:\n  paths: [app]\n";
        let cfg = Config::parse(body, &root.join("i18n-tasks-rs.yml"), root.clone()).unwrap();

        let used = UsedKeys::scan(&cfg).unwrap();

        assert_eq!(used.files_scanned, 1, "loud.rb");
        assert_eq!(used.files_prefiltered, 1, "quiet.rb");
        assert!(used.key_used("a.b"));
        assert_eq!(
            crate::discover::read_log::count_under(&root),
            2,
            "one read per candidate, not one per pass"
        );
        let _ = std::fs::remove_dir_all(&root);
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
