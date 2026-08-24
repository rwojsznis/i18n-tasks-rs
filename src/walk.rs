//! The one directory walk.
//!
//! Three of these used to exist — one for source discovery, one for locale
//! glob expansion, one for `init-config` — and they had drifted apart on
//! hidden files, on symlinks and on what they pruned. The policy belongs to
//! the caller and is passed in; the walk itself is here, once.
//!
//! ref: lib/i18n/tasks/scanners/files/file_finder.rb:34-50, which is
//! `Find.find` plus a prune rule, and the shape this follows.

use std::path::{Path, PathBuf};

/// Whether the walk descends into the directory it has just offered.
///
/// `visit` answers for a file as well, where the answer is ignored, so that
/// one closure decides both questions about one entry.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Descend {
    Yes,
    No,
}

/// Walks `dir` depth-first, in path order, and offers every entry to `visit`
/// with a flag saying whether it is a directory.
///
/// `dir` itself is not offered — a caller that wants it has it already. A
/// directory that cannot be read is skipped rather than reported, which is
/// what `Find.find` does.
///
/// A symlink is offered as a file whatever it points at, so a symlinked source
/// or locale file is read like a real one. The walk never descends into one:
/// that is `Find.find`'s rule, and it is what stops a cycle.
pub fn walk(dir: &Path, visit: &mut impl FnMut(&Path, bool) -> Descend) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<(PathBuf, bool)> = entries
        .filter_map(Result::ok)
        .map(|e| {
            // `DirEntry::file_type` does not follow the link, so a symlink is
            // never a directory here, however it resolves.
            let is_dir = e.file_type().is_ok_and(|t| t.is_dir());
            (e.path(), is_dir)
        })
        .collect();
    // Path order, so the walk is deterministic whatever the directory hands
    // back. Callers that sort their own results still rely on this for the
    // order in which files are *read*.
    entries.sort();
    for (path, is_dir) in entries {
        if visit(&path, is_dir) == Descend::Yes && is_dir {
            walk(&path, visit);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a tree with a nested directory, a hidden file, a symlinked file
    /// and a symlinked directory, and returns its root.
    fn tree(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("i18n-tasks-rs-walk-{name}"));
        let _ = std::fs::remove_dir_all(&root);
        for rel in ["a/deep/x.rb", "a/b.rb", "c.rb", ".hidden.rb", "out/real.rb"] {
            let path = root.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "t('k')\n").unwrap();
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.join("out/real.rb"), root.join("a/link.rb")).unwrap();
            std::os::unix::fs::symlink(root.join("out"), root.join("a/linkdir")).unwrap();
        }
        root
    }

    /// Everything, in path order, with the entry kinds the walk reports.
    fn seen(root: &Path) -> Vec<String> {
        let mut out = Vec::new();
        walk(root, &mut |path, is_dir| {
            let rel = path
                .strip_prefix(root)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");
            out.push(format!("{}{rel}", if is_dir { "d " } else { "f " }));
            Descend::Yes
        });
        out
    }

    #[test]
    fn the_walk_is_depth_first_in_path_order() {
        let root = tree("order");
        let mut want = vec![
            "f .hidden.rb",
            "d a",
            "f a/b.rb",
            "d a/deep",
            "f a/deep/x.rb",
        ];
        // A symlink is a file even when it points at a directory, so `linkdir`
        // is offered and not walked into.
        #[cfg(unix)]
        want.extend(["f a/link.rb", "f a/linkdir"]);
        want.extend(["f c.rb", "d out", "f out/real.rb"]);
        assert_eq!(seen(&root), want);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_no_answer_prunes_the_subtree() {
        let root = tree("prune");
        let mut out = Vec::new();
        walk(&root, &mut |path, is_dir| {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if is_dir && name == "a" {
                return Descend::No;
            }
            if !is_dir {
                out.push(name);
            }
            Descend::Yes
        });
        assert_eq!(out, [".hidden.rb", "c.rb", "real.rb"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// An unreadable or absent directory yields nothing, and is not an error.
    #[test]
    fn a_directory_that_is_not_there_is_skipped() {
        let root = std::env::temp_dir().join("i18n-tasks-rs-walk-nowhere");
        let _ = std::fs::remove_dir_all(&root);
        assert!(seen(&root).is_empty());
    }
}
