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
use crate::session::Session;
use serde::Serialize;
use std::collections::{BTreeSet, HashMap};
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

    /// The `normalize` envelope. `check-normalized` reports the same plan
    /// through the shared check envelope instead, so this one is separate:
    /// it names the command that writes, and `written` is what it adds.
    ///
    /// # Errors
    ///
    /// The plan does not serialize.
    pub fn to_normalize_json(&self, session: &Session, written: bool) -> Result<String, String> {
        serde_json::to_string_pretty(&serde_json::json!({
            "check": "normalize",
            "written": written,
            "config_digest": session.cfg.digest,
            "locales": session.locales,
            "changes": self.changes,
            "files_routed": self.files_routed,
        }))
        .map_err(|e| e.to_string())
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
    plan_filtered(cfg, store, locales, force_pattern, &|_, _| true)
}

/// Works out the destination files after rejected locale/key pairs are removed.
///
/// # Errors
///
/// A selected key cannot be routed, a locale has no data, or a write-safety
/// guard fails. Nothing is written.
pub fn plan_filtered(
    cfg: &Config,
    store: &Store,
    locales: &[String],
    force_pattern: bool,
    keep: &impl Fn(&str, &str) -> bool,
) -> Result<NormalizeReport, String> {
    let mut changes = Vec::new();
    let mut files_routed = 0usize;
    let mut claimed: HashMap<PathBuf, String> = HashMap::new();

    for locale in locales {
        let destinations =
            route::route_filtered(cfg, store, locale, force_pattern, &|key| keep(locale, key))?;
        let tree = store
            .tree(locale)
            .ok_or_else(|| format!("locale `{locale}` has no data"))?;
        let mut written: BTreeSet<PathBuf> = BTreeSet::new();

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
            written.insert(dest.path.clone());

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
            check_foreign_locales(tree, &path, locale)?;
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

    // Ordered by the printed path, so the report order does not depend on how
    // a destination outside the root spells itself.
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
        "{} holds the locale(s) {} as well as `{locale}`. Writing one locale per \
         file would drop them. Split the file by locale first.",
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
/// the first failure, so some files may be left unwritten — but no single file
/// is left half-written, because `write_atomic` renames a finished temp file
/// into place.
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
                write_atomic(path, change.after.as_bytes())
                    .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
            }
        }
    }
    Ok(())
}

/// Replaces `path`'s bytes with `bytes` through a sibling temp file and one
/// `rename`. `std::fs::write` truncates first, so a failure part-way through
/// (a full disk) leaves a truncated locale file, and this is the only code
/// path in the tool that destroys data. Here the old file stays whole until
/// the rename, which is atomic on the same filesystem.
///
/// The temp file is `sync_all`ed before the rename on purpose: without it a
/// full disk is reported at close, after the rename has already put the short
/// file in place.
///
/// Three things the plain write gave for free, which the rename has to keep:
///
/// - a symlinked destination is written through, not replaced with a plain
///   file;
/// - the destination keeps the mode it had, because the rename brings a new
///   file with it;
/// - a destination the process cannot write is an error. `rename` asks the
///   directory for permission, not the file, so the file is probed first by
///   opening it for append — the one mode that neither truncates nor creates.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let target = through_symlink(path);
    let dir = target.parent().unwrap_or_else(|| Path::new("."));
    let name = target.file_name().unwrap_or(std::ffi::OsStr::new("locale"));
    let existing = std::fs::metadata(&target).ok();
    if existing.is_some() {
        std::fs::OpenOptions::new().append(true).open(&target)?;
    }

    // The name holds the pid, so two runs over one project cannot share a
    // temp file. Inside a run the destinations are distinct already.
    let mut temp_name = std::ffi::OsString::from(".");
    temp_name.push(name);
    temp_name.push(format!(".{}.i18n-tasks-rs.tmp", std::process::id()));
    let temp = dir.join(temp_name);

    let written = (|| {
        let mut file = std::fs::File::create(&temp)?;
        file.write_all(bytes)?;
        if let Some(meta) = &existing {
            file.set_permissions(meta.permissions())?;
        }
        file.sync_all()
    })()
    .and_then(|()| std::fs::rename(&temp, &target));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&temp);
        return Err(e);
    }
    Ok(())
}

/// The path the write lands on: a symlink is followed, so the rename replaces
/// what the link points at rather than the link. A broken link has nothing to
/// follow, and is written as itself.
fn through_symlink(path: &Path) -> PathBuf {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
        }
        _ => path.to_path_buf(),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory of its own per test, so one test's leftovers cannot reach
    /// another.
    fn dir(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("i18n-tasks-rs-atomic-{name}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut out: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        out.sort();
        out
    }

    #[test]
    fn a_write_replaces_the_bytes_and_leaves_no_temp_file() {
        let root = dir("replace");
        let path = root.join("en.yml");
        std::fs::write(&path, "old\n").unwrap();
        write_atomic(&path, b"new\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new\n");
        assert_eq!(names(&root), ["en.yml"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_write_creates_a_file_that_is_not_there() {
        let root = dir("create");
        let path = root.join("en.yml");
        write_atomic(&path, b"new\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new\n");
        assert_eq!(names(&root), ["en.yml"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The rename brings a new file with it, so the mode of the file it
    /// replaces has to be carried over. `std::fs::write` kept it for free.
    #[cfg(unix)]
    #[test]
    fn a_write_keeps_the_mode_of_the_file_it_replaces() {
        use std::os::unix::fs::PermissionsExt;
        let root = dir("mode");
        let path = root.join("en.yml");
        std::fs::write(&path, "old\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        write_atomic(&path, b"new\n").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o640, "the mode was not carried over");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// `rename` asks the directory for permission, not the file, so a
    /// read-only destination would be replaced without the probe. Skipped when
    /// the chmod does not take, which is what happens when the tests run as
    /// root.
    #[cfg(unix)]
    #[test]
    fn a_read_only_destination_is_an_error_and_keeps_its_bytes() {
        use std::os::unix::fs::PermissionsExt;
        let root = dir("read-only");
        let path = root.join("en.yml");
        std::fs::write(&path, "old\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
        let writable = std::fs::OpenOptions::new().append(true).open(&path).is_ok();
        let result = write_atomic(&path, b"new\n");
        let bytes = std::fs::read_to_string(&path).unwrap();
        let left = names(&root);
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644));
        let _ = std::fs::remove_dir_all(&root);
        if writable {
            eprintln!("skipped: the chmod did not take effect");
            return;
        }
        assert!(result.is_err(), "a read-only file was replaced");
        assert_eq!(bytes, "old\n");
        assert_eq!(left, ["en.yml"], "a temp file was left behind");
    }

    /// A symlinked destination is written through, as `std::fs::write` did.
    /// The rename would otherwise put a plain file where the link was.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_destination_is_written_through() {
        let root = dir("symlink");
        let real = root.join("shared.yml");
        let link = root.join("en.yml");
        std::fs::write(&real, "old\n").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        write_atomic(&link, b"new\n").unwrap();
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "new\n");
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the link was replaced by a plain file"
        );
        assert_eq!(names(&root), ["en.yml", "shared.yml"]);
        let _ = std::fs::remove_dir_all(&root);
    }
}
