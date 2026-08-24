pub mod interpolations;
pub mod missing;
pub mod normalize;
pub mod unused;

use serde::Serialize;

/// Whether a check found anything. Exit code 1 means "found something",
/// matching the gem's internal `:exit1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Clean,
    Found,
}

impl Outcome {
    pub fn of(found: bool) -> Outcome {
        if found {
            Outcome::Found
        } else {
            Outcome::Clean
        }
    }
}

/// One reported key, with the locale it belongs to.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct KeyRow {
    pub locale: String,
    pub key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
}

/// Renders rows as an aligned plain-text table.
///
/// The gem uses terminal-table and Rainbow. Both are replaced with a plain
/// reimplementation.
pub fn render_table(title: &str, headers: &[&str], rows: &[Vec<String>]) -> String {
    if rows.is_empty() {
        return format!("{title}: none\n");
    }
    let cols = headers.len();
    let mut width: Vec<usize> = headers.iter().map(|h| h.chars().count()).collect();
    for row in rows {
        for (i, w) in width.iter_mut().enumerate().take(cols) {
            let cell = row.get(i).map_or(0, |c| c.chars().count());
            if cell > *w {
                *w = cell;
            }
        }
    }
    let mut out = format!("{title} ({} found)\n", rows.len());
    let line = |cells: &[String]| -> String {
        let mut s = String::new();
        for (i, w) in width.iter().enumerate().take(cols) {
            let cell = cells.get(i).map(String::as_str).unwrap_or("");
            s.push_str(cell);
            if i + 1 < cols {
                for _ in cell.chars().count()..w + 2 {
                    s.push(' ');
                }
            }
        }
        s.trim_end().to_string()
    };
    out.push_str(&line(
        &headers.iter().map(|h| h.to_string()).collect::<Vec<_>>(),
    ));
    out.push('\n');
    out.push_str(&"-".repeat(width.iter().sum::<usize>() + 2 * (cols - 1)));
    out.push('\n');
    for row in rows {
        out.push_str(&line(row));
        out.push('\n');
    }
    out
}
