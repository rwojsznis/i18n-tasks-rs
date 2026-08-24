pub mod erb;
pub mod ruby;
pub mod template;

use crate::lineindex::LineIndex;
use serde::Serialize;
use std::path::{Path, PathBuf};

/// One place in the source where a key is used.
///
/// ref: lib/i18n/tasks/scanners/results/occurrence.rb
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Occurrence {
    pub path: PathBuf,
    /// The source slice of the call node, which is what the gem's Prism path
    /// stores in `Occurrence#line`.
    pub snippet: String,
    pub pos: usize,
    pub line_pos: usize,
    pub line_num: usize,
    pub raw_key: String,
    /// All keys this occurrence may resolve to. Only `missing` looks at more
    /// than the first. ref: lib/i18n/tasks/missing_keys.rb:110-130
    pub candidate_keys: Vec<String>,
}

/// The result of scanning one file. A pure function of the file bytes and path.
#[derive(Debug, Clone, Default, Serialize)]
pub struct FileScan {
    /// Fully resolved keys.
    pub keys: Vec<(String, Occurrence)>,
    /// Key patterns derived from interpolated keys. Blocker B5.
    pub patterns: Vec<(String, Occurrence)>,
    /// Calls whose key cannot be determined at all, such as `t(some_var)`.
    pub opaque: Vec<Occurrence>,
}

impl FileScan {
    pub fn merge(&mut self, other: FileScan) {
        self.keys.extend(other.keys);
        self.patterns.extend(other.patterns);
        self.opaque.extend(other.opaque);
    }
}

/// The parts of the config a scanner needs.
#[derive(Debug, Clone)]
pub struct ScanConfig {
    /// Longest match wins. ref: lib/i18n/tasks/scanners/relative_keys.rb#path_root
    pub relative_roots: Vec<String>,
    /// ref: lib/i18n/tasks/scanners/relative_keys.rb:26-28
    pub relative_exclude_method_name_paths: Vec<String>,
}

impl ScanConfig {
    pub fn from_config(cfg: &crate::config::Config) -> ScanConfig {
        ScanConfig {
            relative_roots: cfg.search.relative_roots.clone(),
            relative_exclude_method_name_paths: cfg
                .search
                .relative_exclude_method_name_paths
                .clone(),
        }
    }

    /// The longest configured relative root the file lies under.
    pub fn matching_root(&self, path: &Path) -> Option<&str> {
        let p = path.to_string_lossy().replace('\\', "/");
        self.relative_roots
            .iter()
            .filter(|root| {
                let root = root.trim_end_matches('/');
                p.contains(&format!("{root}/"))
            })
            .max_by_key(|root| root.len())
            .map(String::as_str)
    }

    pub fn skips_method_name(&self, root: &str) -> bool {
        self.relative_exclude_method_name_paths
            .iter()
            .any(|p| p.trim_end_matches('/') == root.trim_end_matches('/'))
    }
}

/// Maps a byte offset in a synthetic buffer back to the file it came from.
///
/// The ERB scanner concatenates every code tag of a file into one Ruby buffer
/// and parses that once (design decision 3), so every position Prism reports
/// has to be translated back before it reaches an `Occurrence`.
#[derive(Debug, Default, Clone)]
pub struct SourceMap {
    segs: Vec<Seg>,
}

#[derive(Debug, Clone)]
struct Seg {
    buf_start: u32,
    len: u32,
    file_start: u32,
}

impl SourceMap {
    /// Records that `len` bytes at `buf_start` in the buffer are a verbatim copy
    /// of the file bytes at `file_start`. Segments must be pushed in order.
    /// The offsets are packed as `u32`, as in `LineIndex`: no template this
    /// scans is 4 GiB, and a longer one saturates rather than wrapping.
    pub fn push(&mut self, buf_start: usize, len: usize, file_start: usize) {
        self.segs.push(Seg {
            buf_start: u32::try_from(buf_start).unwrap_or(u32::MAX),
            len: u32::try_from(len).unwrap_or(u32::MAX),
            file_start: u32::try_from(file_start).unwrap_or(u32::MAX),
        });
    }

    /// A buffer offset as a file offset. An offset that lands in filler the
    /// scanner inserted resolves to the start of the next real segment, which
    /// is what a rewritten comment marker needs.
    pub fn translate(&self, buf_pos: usize) -> usize {
        if self.segs.is_empty() {
            return buf_pos;
        }
        let pos = u32::try_from(buf_pos).unwrap_or(u32::MAX);
        let i = self.segs.partition_point(|s| s.buf_start <= pos);
        if i == 0 {
            return self.segs[0].file_start as usize;
        }
        let seg = &self.segs[i - 1];
        if pos < seg.buf_start + seg.len {
            (seg.file_start + (pos - seg.buf_start)) as usize
        } else if let Some(next) = self.segs.get(i) {
            next.file_start as usize
        } else {
            (seg.file_start + seg.len) as usize
        }
    }
}

/// Turns a parser offset into the file position, line and column an
/// `Occurrence` reports.
#[derive(Debug)]
pub struct Locator<'a> {
    index: &'a LineIndex,
    map: Option<&'a SourceMap>,
}

impl<'a> Locator<'a> {
    pub fn direct(index: &'a LineIndex) -> Locator<'a> {
        Locator { index, map: None }
    }

    pub fn mapped(index: &'a LineIndex, map: &'a SourceMap) -> Locator<'a> {
        Locator {
            index,
            map: Some(map),
        }
    }

    /// `(file position, one-based line, zero-based column)`.
    pub fn locate(&self, pos: usize) -> (usize, usize, usize) {
        let pos = self.map.map_or(pos, |m| m.translate(pos));
        let (line, col) = self.index.locate(pos);
        (pos, line, col)
    }
}

thread_local! {
    /// Prism parses of a whole source buffer, counted per thread.
    ///
    /// Design decision 3: one parse per file, however many ERB tags
    /// the file holds. `tests/erb_keys.rs` asserts it. A nested parse of a
    /// magic-comment payload is not counted; the gem parses those too.
    ///
    /// Per thread, not global, so that the assertion stays meaningful while
    /// files are scanned in parallel and while test threads share the process.
    static SOURCE_PARSES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Prism parses this thread has done. See `SOURCE_PARSES`.
pub fn source_parses() -> u64 {
    SOURCE_PARSES.get()
}

pub(crate) fn count_source_parse() {
    SOURCE_PARSES.set(SOURCE_PARSES.get() + 1);
}

/// Scans one file, dispatching on the extension.
///
/// The gem dispatches the same way, through `search.scanners`
/// (`used_keys.rb:22-26`): `*.rb` to the Ruby scanner, `*.erb` to the ERB
/// scanner, and everything else to the regex scanner.
pub fn scan_file(bytes: &[u8], path: &Path, cfg: &ScanConfig) -> FileScan {
    match path.extension().and_then(|e| e.to_str()) {
        Some("rb") => ruby::scan(bytes, path, cfg),
        Some("erb") => erb::scan(bytes, path, cfg),
        _ => template::scan(bytes, path, cfg),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_map_translates_inside_and_between_segments() {
        let mut map = SourceMap::default();
        map.push(0, 5, 100); // buffer 0..5   -> file 100..105
        map.push(6, 4, 200); // buffer 6..10  -> file 200..204
        assert_eq!(map.translate(0), 100);
        assert_eq!(map.translate(4), 104);
        // The filler byte between the two segments points at the next segment.
        assert_eq!(map.translate(5), 200);
        assert_eq!(map.translate(7), 201);
        // Past the last segment, the end of its file range.
        assert_eq!(map.translate(10), 204);
        assert_eq!(map.translate(99), 204);
    }

    #[test]
    fn empty_source_map_is_the_identity() {
        assert_eq!(SourceMap::default().translate(42), 42);
    }
}
