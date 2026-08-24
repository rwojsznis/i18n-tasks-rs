//! `normalize` and `check-normalized`.
//!
//! ref: lib/i18n/tasks/data/file_system_base.rb#set
//! ref: lib/i18n/tasks/data/file_formats.rb#normalized?
//! ref: lib/i18n/tasks/command/commands/data.rb
//!
//! Both commands share one plan. `check-normalized` reports it and stops;
//! `normalize` applies it. Nothing here writes: `apply` is a separate call
//! that the CLI makes only after `--write`. See blocker B8.

use super::Outcome;
use crate::config::Config;
use crate::data::emit::{Tree, emit_locale};
use crate::data::load::Store;
use crate::data::route;
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Create,
    Update,
    /// The file held only keys that have moved elsewhere, so it ends up empty.
    /// ref: `(paths_before - paths_after).each { FileUtils.remove_file ... }`
    Delete,
}

#[derive(Debug, Serialize)]
pub struct FileChange {
    /// The path as reports print it: relative to the root where it can be,
    /// slashes normalised. Rendering only — never the target of a write.
    #[serde(rename = "path")]
    pub display: String,
    /// The path `apply` writes, exactly as the router produced it. Kept apart
    /// from `display` because that rendering is lossy and this is the only
    /// code path in the tool that destroys data.
    #[serde(skip)]
    pub path: PathBuf,
    pub locale: String,
    pub action: Action,
    /// The bytes to write. Empty for a deletion.
    #[serde(skip)]
    pub after: String,
    #[serde(skip)]
    pub before: String,
}

#[derive(Debug, Serialize)]
pub struct NormalizeReport {
    pub changes: Vec<FileChange>,
    /// Every destination the router produced, changed or not.
    pub files_routed: usize,
}

impl NormalizeReport {
    pub fn outcome(&self) -> Outcome {
        Outcome::of(!self.changes.is_empty())
    }

    pub fn deletions(&self) -> Vec<&FileChange> {
        self.changes
            .iter()
            .filter(|c| c.action == Action::Delete)
            .collect()
    }

    /// `check-normalized`. ref: `terminal_report.check_normalized_results`.
    pub fn to_check_text(&self) -> String {
        if self.changes.is_empty() {
            return "All data is normalized\n".to_string();
        }
        let mut out = format!(
            "The following data requires normalization ({} found)\n",
            self.changes.len()
        );
        for c in &self.changes {
            out.push_str(&c.display);
            out.push('\n');
        }
        out.push_str("Run `i18n-tasks-rs normalize --write` to fix\n");
        out
    }

    /// The `normalize` summary. `diff` adds a unified diff per file.
    pub fn to_normalize_text(&self, diff: bool) -> String {
        if self.changes.is_empty() {
            return "All data is normalized\n".to_string();
        }
        let mut out = format!("{} file(s) to change\n", self.changes.len());
        for c in &self.changes {
            let verb = match c.action {
                Action::Create => "create",
                Action::Update => "update",
                Action::Delete => "delete",
            };
            out.push_str(&format!("  {verb} {}\n", c.display));
        }
        if diff {
            for c in &self.changes {
                out.push('\n');
                out.push_str(&unified_diff(&c.display, &c.before, &c.after));
            }
        }
        out
    }
}

/// Works out what every destination file should hold.
///
/// Two guards that the gem does not have, both of which protect data that
/// `normalize` would otherwise drop without a word:
///
/// * a destination claimed by two locales in one run, where the second write
///   would erase the first;
/// * a destination that already holds a locale outside this run, because a
///   file is written from one locale's keys only.
///
/// # Errors
///
/// A key cannot be routed, a locale in `locales` has no data, or either guard
/// above fires. Nothing is written either way — `plan` does not touch disk.
pub fn plan(
    cfg: &Config,
    store: &Store,
    locales: &[String],
    force_pattern: bool,
) -> Result<NormalizeReport, String> {
    let mut changes = Vec::new();
    let mut files_routed = 0usize;
    let mut claimed: HashMap<PathBuf, String> = HashMap::new();

    for locale in locales {
        let destinations = route::route(cfg, store, locale, force_pattern)?;
        let tree = store
            .tree(locale)
            .ok_or_else(|| format!("locale `{locale}` has no data"))?;
        let mut written: Vec<PathBuf> = Vec::new();

        for dest in &destinations {
            files_routed += 1;
            if let Some(other) = claimed.get(&dest.path) {
                return Err(format!(
                    "{} is the destination for both `{other}` and `{locale}`. \
                     A locale file holds one locale, so the second write would erase \
                     the first. Fix `data.write` so each locale routes to its own file.",
                    dest.path.display()
                ));
            }
            claimed.insert(dest.path.clone(), locale.clone());
            check_foreign_locales(tree, &dest.path, locale)?;

            let mut out = Tree::new();
            for key in &dest.keys {
                let leaf = tree
                    .get(key)
                    .ok_or_else(|| format!("`{locale}.{key}` vanished between routing and emit"))?;
                out.insert_segments(&leaf.segments(), leaf.value.clone());
            }
            // ref: `config[:sort] = !config[:keep_order]`
            if !cfg.data.keep_order {
                out.sort();
            }
            let after = emit_locale(locale, &out);
            written.push(dest.path.clone());

            let before = std::fs::read_to_string(&dest.path).unwrap_or_default();
            let exists = dest.path.is_file();
            // ref: `write_tree` returns early when the bytes already match.
            if exists && before == after {
                continue;
            }
            changes.push(FileChange {
                display: display_path(cfg, &dest.path),
                path: dest.path.clone(),
                locale: locale.clone(),
                action: if exists {
                    Action::Update
                } else {
                    Action::Create
                },
                after,
                before,
            });
        }

        for path in route::origin_paths(store, locale) {
            if written.contains(&path) {
                continue;
            }
            changes.push(FileChange {
                display: display_path(cfg, &path),
                locale: locale.clone(),
                action: Action::Delete,
                after: String::new(),
                before: std::fs::read_to_string(&path).unwrap_or_default(),
                path,
            });
        }
    }

    // On the printed path, so the report order does not depend on how a
    // destination outside the root spells itself.
    changes.sort_by(|a, b| a.display.cmp(&b.display));
    Ok(NormalizeReport {
        changes,
        files_routed,
    })
}

fn check_foreign_locales(
    tree: &crate::data::load::LocaleTree,
    path: &Path,
    locale: &str,
) -> Result<(), String> {
    let Some(found) = tree.file_locales.get(path) else {
        return Ok(());
    };
    let foreign: Vec<&str> = found
        .iter()
        .map(String::as_str)
        .filter(|l| *l != locale)
        .collect();
    if foreign.is_empty() {
        return Ok(());
    }
    Err(format!(
        "{} holds the locale(s) {} as well as `{locale}`. `normalize` writes one \
         locale per file, so it would drop them. Split the file by locale first.",
        path.display(),
        foreign.join(", ")
    ))
}

/// Paths are reported relative to the config root, which is what the gem
/// prints and what a diff reads best. Lossy, and one-way: `apply` writes
/// `FileChange.path`, never this.
fn display_path(cfg: &Config, path: &Path) -> String {
    path.strip_prefix(&cfg.root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Applies the plan. Only the CLI calls this, and only after `--write`.
///
/// The planned `path` is written verbatim. It is deliberately not rebuilt from
/// `display`, which cannot be reversed for a non-UTF-8 component or for a name
/// holding a `\`.
///
/// # Errors
///
/// A file cannot be written, a parent directory cannot be created, or an
/// emptied file cannot be deleted. The plan is applied in order and stops at
/// the first failure, so a partial write is possible.
pub fn apply(report: &NormalizeReport) -> Result<(), String> {
    for change in &report.changes {
        let path = change.path.as_path();
        match change.action {
            Action::Delete => std::fs::remove_file(path)
                .map_err(|e| format!("cannot delete {}: {e}", path.display()))?,
            _ => {
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir)
                        .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
                }
                std::fs::write(path, &change.after)
                    .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
            }
        }
    }
    Ok(())
}

/// A unified diff with three lines of context.
pub fn unified_diff(path: &str, before: &str, after: &str) -> String {
    use similar::TextDiff;
    let diff = TextDiff::from_lines(before, after);
    let mut out = format!("--- a/{path}\n+++ b/{path}\n");
    for hunk in diff.unified_diff().context_radius(3).iter_hunks() {
        out.push_str(&hunk.to_string());
    }
    out
}
