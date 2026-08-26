//! Per-file line-offset index.
//!
//! The gem recomputes the line number for every occurrence with
//! `contents[0..position].count("\n")`
//! (`lib/i18n/tasks/scanners/occurrence_from_position.rb:20`), which is
//! quadratic in the number of occurrences. One index per file replaces it.

/// Line starts, packed as `u32`: four bytes per line beats a `usize`'s eight,
/// and no source file this scans is 4 GiB. A longer buffer saturates rather
/// than wraps, so the offsets stay ordered and `locate` keeps answering the
/// last line instead of some arbitrary earlier one.
#[derive(Debug)]
pub struct LineIndex {
    /// Byte offset of the first character of each line. Always starts with 0.
    starts: Vec<u32>,
}

impl LineIndex {
    pub fn new(bytes: &[u8]) -> LineIndex {
        let mut starts = Vec::with_capacity(bytes.len() / 32 + 1);
        starts.push(0u32);
        for (i, &b) in bytes.iter().enumerate() {
            if b == b'\n' {
                starts.push(u32::try_from(i + 1).unwrap_or(u32::MAX));
            }
        }
        LineIndex { starts }
    }

    /// One-based line number and zero-based column for a byte offset.
    pub fn locate(&self, offset: usize) -> (usize, usize) {
        let off = u32::try_from(offset).unwrap_or(u32::MAX);
        let line = match self.starts.binary_search(&off) {
            Ok(i) => i,
            Err(i) => i - 1,
        };
        (line + 1, offset - self.starts[line] as usize)
    }

    /// The line containing `offset`, without its trailing newline.
    pub fn line_text<'a>(&self, bytes: &'a [u8], offset: usize) -> &'a [u8] {
        let (line, _) = self.locate(offset);
        let start = self.starts[line - 1] as usize;
        let end = self.starts.get(line).map_or(bytes.len(), |&e| e as usize);
        let mut end = end.min(bytes.len());
        while end > start && (bytes[end - 1] == b'\n' || bytes[end - 1] == b'\r') {
            end -= 1;
        }
        &bytes[start..end]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locates_offsets() {
        let src = b"one\ntwo\nthree";
        let ix = LineIndex::new(src);
        assert_eq!(ix.locate(0), (1, 0));
        assert_eq!(ix.locate(3), (1, 3));
        assert_eq!(ix.locate(4), (2, 0));
        assert_eq!(ix.locate(9), (3, 1));
        assert_eq!(ix.line_text(src, 5), b"two");
        assert_eq!(ix.line_text(src, 12), b"three");
    }
}
