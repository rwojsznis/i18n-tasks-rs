//! Glob expansion for the `data.read` and `data.write` patterns.
//!
//! Only `*` and `**` are supported, which is all the gem's `Dir.glob` patterns
//! use. Nothing here knows about locales: it turns a pattern into the concrete
//! paths that exist.

use crate::walk::{Descend, walk};
use std::path::{Path, PathBuf};

/// Expands a glob relative to `root`.
pub(super) fn glob_paths(root: &Path, pattern: &str) -> Vec<PathBuf> {
    let pattern = pattern.replace('\\', "/");
    let parts: Vec<&str> = pattern.split('/').filter(|p| !p.is_empty()).collect();
    let mut current = vec![root.to_path_buf()];
    for (i, part) in parts.iter().enumerate() {
        let last = i + 1 == parts.len();
        let mut next = Vec::new();
        if *part == "**" {
            for dir in &current {
                collect_dirs(dir, &mut next);
            }
        } else if part.contains('*') {
            let glob = globset::Glob::new(part).ok().map(|g| g.compile_matcher());
            for dir in &current {
                let Ok(entries) = std::fs::read_dir(dir) else {
                    continue;
                };
                for e in entries.filter_map(Result::ok) {
                    let name = e.file_name();
                    let name = name.to_string_lossy();
                    if !glob.as_ref().is_some_and(|g| g.is_match(name.as_ref())) {
                        continue;
                    }
                    let p = e.path();
                    if fits_segment(&p, last) {
                        next.push(p);
                    }
                }
            }
        } else {
            for dir in &current {
                let p = dir.join(part);
                if fits_segment(&p, last) {
                    next.push(p);
                }
            }
        }
        current = next;
        if current.is_empty() {
            break;
        }
    }
    current.sort();
    current.dedup();
    current
}

/// `dir` and every directory under it, which is what `**` expands to.
fn collect_dirs(dir: &Path, out: &mut Vec<PathBuf>) {
    out.push(dir.to_path_buf());
    walk(dir, &mut |path, is_dir| {
        if !is_dir {
            return Descend::No;
        }
        out.push(path.to_path_buf());
        Descend::Yes
    });
}

/// Whether an expanded path can stand for one segment of the pattern.
///
/// Only the last segment names a locale file; every earlier one has to be a
/// directory for the next segment to be joined onto it.
fn fits_segment(path: &Path, is_last: bool) -> bool {
    if is_last {
        path.is_file()
    } else {
        path.is_dir()
    }
}
