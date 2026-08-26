//! Which lines of the source each config entry owns.
//!
//! The migration slices the original lines rather than re-serializing a parsed
//! tree, and this module answers the one question that makes that possible:
//! where does an entry start and end.

/// The line range one config entry owns.
#[derive(Debug, Clone, Copy)]
pub(super) struct Extent {
    /// First line of the comment block above the key, or the key itself.
    pub(super) lead_start: usize,
    /// Last line of the value, comments below it excluded.
    pub(super) body_end: usize,
}

/// Splits a level into one extent per entry. All lines are 1-based.
///
/// Two conventions decide who owns a comment, and between them they cover how
/// config files are actually written:
///
///   * the run of comment lines directly above a key, with no blank line in
///     between, documents that key and moves with it;
///   * anything below a key — a commented-out list entry, a trailing note —
///     stays with the key above it.
///
/// So `## Translation Services` leaves with the `translation:` block it
/// introduces, while a commented-out `# - 'errors.messages.*'` under
/// `ignore_missing` stays under `ignore_missing`.
pub(super) fn extents(
    lines: &[&str],
    starts: &[usize],
    level_start: usize,
    level_end: usize,
) -> Vec<Extent> {
    let at = |n: usize| lines.get(n - 1).copied().unwrap_or("");
    let lead_start = |i: usize| {
        let start = starts[i];
        // Never climb past the previous key, whatever is in between.
        let floor = if i == 0 {
            level_start
        } else {
            starts[i - 1] + 1
        };
        let mut lead = start;
        while lead > floor && is_comment(at(lead - 1)) {
            lead -= 1;
        }
        lead
    };
    (0..starts.len())
        .map(|i| {
            let start = starts[i];
            let mut body_end = if i + 1 < starts.len() {
                lead_start(i + 1).saturating_sub(1)
            } else {
                level_end
            }
            .max(start);
            loop {
                // A blank line at the end of a block is a separator.
                while body_end > start && at(body_end).trim().is_empty() {
                    body_end -= 1;
                }
                // A comment block that a blank line separates from the value
                // below it belongs to neither key. In a gem config that is
                // always the commented-out documentation of a setting nobody
                // enabled, and half of it documents settings this port dropped.
                let mut probe = body_end;
                while probe > start && is_comment(at(probe)) {
                    probe -= 1;
                }
                if probe < body_end && at(probe).trim().is_empty() {
                    body_end = probe;
                } else {
                    break;
                }
            }
            Extent {
                lead_start: lead_start(i),
                body_end,
            }
        })
        .collect()
}

pub(super) fn is_comment(line: &str) -> bool {
    line.trim_start().starts_with('#')
}

pub(super) fn slice(lines: &[&str], from: usize, to: usize) -> String {
    lines[from - 1..to].join("\n")
}
