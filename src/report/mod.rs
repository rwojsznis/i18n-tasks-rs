pub mod interpolations;
pub mod missing;
pub mod normalize;
pub mod unused;

use serde::Serialize;
use std::path::PathBuf;

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

/// Why a row is in the report.
///
/// This is a tagged enum rather than a sentence because `-f json` is an
/// interface: `find -f json` exists so this tool and the gem can be compared
/// mechanically, and a consumer of `missing -f json` should not have to regex
/// English apart to recover the type, the path and the line. The prose lives in
/// `to_text`, which the plain-text table calls and nothing else does.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Reason {
    /// `missing --types used`: scanned in the source, no value in the locale.
    /// The first occurrence of the key, which is the one the table names.
    Used { path: PathBuf, line: usize },
    /// `missing --types diff`: present in another locale, absent in this one.
    Diff { present_in: String },
    /// `missing --types plural`: a plural node short of a CLDR category.
    Plural { categories: Vec<&'static str> },
    /// `check-consistent-interpolations`: the variables here against the base
    /// locale's. Both lists are the full `%{name}` match, sorted.
    Interpolations {
        variables: Vec<String>,
        base_locale: String,
        base_variables: Vec<String>,
    },
    /// `check-reserved-interpolations`: variable *names*, without the braces,
    /// that collide with `RESERVED_KEYS`.
    Reserved { names: Vec<String> },
}

impl Reason {
    /// The prose the plain-text table prints. `locale` is the row's own, which
    /// the enum does not repeat: `KeyRow::details` supplies it.
    pub fn to_text(&self, locale: &str) -> String {
        match self {
            Reason::Used { path, line } => format!("used: {}:{line}", path.display()),
            Reason::Diff { present_in } => format!("diff: present in {present_in}"),
            Reason::Plural { categories } => {
                format!("plural: missing {}", categories.join(", "))
            }
            Reason::Interpolations {
                variables,
                base_locale,
                base_variables,
            } => format!(
                "{locale} has {}, {base_locale} has {}",
                variable_list(variables),
                variable_list(base_variables)
            ),
            Reason::Reserved { names } => names.join(", "),
        }
    }
}

/// ref: interpolations.rb#inconsistent_interpolations, which prints
/// `"no variables"` for an empty set rather than an empty cell.
fn variable_list(variables: &[String]) -> String {
    if variables.is_empty() {
        "no variables".into()
    } else {
        variables.join(", ")
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
    pub reason: Option<Reason>,
}

impl KeyRow {
    /// The reason as prose, for the text table. Empty when there is none.
    pub fn details(&self) -> String {
        self.reason
            .as_ref()
            .map_or_else(String::new, |r| r.to_text(&self.locale))
    }
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
