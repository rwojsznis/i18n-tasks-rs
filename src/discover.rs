//! Source file discovery.
//!
//! ref: lib/i18n/tasks/scanners/files/file_finder.rb:34-50

use crate::config::{ALWAYS_EXCLUDE, Config};
use aho_corasick::AhoCorasick;
use globset::{Glob, GlobSet, GlobSetBuilder};
use rayon::prelude::*;
use std::path::{Path, PathBuf};

/// Substrings that any `t`-family call or magic comment must contain.
///
/// A file with no hit cannot hold a translation call, so it is never parsed.
const NEEDLES: &[&str] = &["t(", "t ", "t!", "translate", "I18n.", "i18n-tasks-use"];

pub struct Discovery {
    pub files: Vec<PathBuf>,
    /// Files that matched the globs but held none of the needles.
    pub prefiltered: usize,
}

pub struct Finder {
    only: Option<GlobSet>,
    exclude: GlobSet,
    prefilter: AhoCorasick,
    paths: Vec<PathBuf>,
    /// What `match_path` strips, so the globs see the path a config author
    /// wrote them against.
    root: PathBuf,
}

impl Finder {
    pub fn new(cfg: &Config) -> Result<Finder, String> {
        // `search.only` takes priority over `exclude`, but `exclude` still
        // applies, and ALWAYS_EXCLUDE applies whatever the config says.
        let only = match &cfg.search.only {
            Some(globs) if !globs.is_empty() => Some(build_globs(globs)?),
            _ => None,
        };
        let mut excludes: Vec<String> = cfg.search.exclude.clone();
        excludes.extend(ALWAYS_EXCLUDE.iter().map(|s| s.to_string()));
        let exclude = build_globs(&excludes)?;
        let paths = cfg.search.paths.iter().map(|p| cfg.root.join(p)).collect();
        Ok(Finder {
            only,
            exclude,
            prefilter: AhoCorasick::new(NEEDLES).expect("static needles compile"),
            paths,
            root: cfg.root.clone(),
        })
    }

    pub fn discover(&self) -> Discovery {
        let mut files = Vec::new();
        for root in &self.paths {
            if !root.exists() {
                continue;
            }
            // ref: file_finder.rb — `Find.find` yields a path given to it
            // directly, so a `search.paths` entry may name one file.
            if root.is_dir() {
                self.walk(root, &mut files);
            } else {
                self.consider(root.clone(), &mut files);
            }
        }
        files.sort();
        files.dedup();
        // Reading every candidate once here also warms the OS page cache for
        // the scan that follows. The reads are fanned out too, because they are
        // the larger half of the scan path once the parse is parallel.
        //
        // `Some(false)` is a file with no needle in it, `None` is a file that
        // could not be read; only the first is reported. Collecting a verdict
        // per file, in file order, keeps the surviving list identical at every
        // `--jobs` setting.
        let verdicts: Vec<Option<bool>> = files
            .par_iter()
            .map(|p| {
                std::fs::read(p)
                    .ok()
                    .map(|bytes| self.prefilter.is_match(&bytes[..]))
            })
            .collect();
        let prefiltered = verdicts.iter().filter(|v| **v == Some(false)).count();
        let files = files
            .into_iter()
            .zip(&verdicts)
            .filter(|(_, v)| **v == Some(true))
            .map(|(p, _)| p)
            .collect();
        Discovery { files, prefiltered }
    }

    fn walk(&self, dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<_> = entries.filter_map(Result::ok).collect();
        entries.sort_by_key(std::fs::DirEntry::path);
        for entry in entries {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let hidden = name.starts_with('.');
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            let rel = self.match_path(&path);
            let excluded = self.exclude.is_match(rel.as_str());
            if is_dir {
                // The gem prunes a directory that is hidden or excluded, and
                // descends into every other one.
                if !hidden && !excluded {
                    self.walk(&path, out);
                }
            } else if !hidden && !excluded {
                self.consider(path, out);
            }
        }
    }

    /// The include/exclude decision for one file, shared by the walk and by a
    /// `search.paths` entry that names a file rather than a directory.
    fn consider(&self, path: PathBuf, out: &mut Vec<PathBuf>) {
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            return;
        };
        if name.starts_with('.') {
            return;
        }
        let rel = self.match_path(&path);
        if self.exclude.is_match(rel.as_str()) {
            return;
        }
        if self
            .only
            .as_ref()
            .is_some_and(|g| !g.is_match(rel.as_str()))
        {
            return;
        }
        out.push(path);
    }

    /// The path the globs are matched against: root-relative, and `/`-separated
    /// whatever the platform separator is.
    ///
    /// The gem matches the path exactly as `Find.find` produced it. Its
    /// `search.paths` entries are relative and it runs from the project root,
    /// so that path is root-relative too, and a config glob is written against
    /// it. Matching the absolute path instead cannot work: a pattern holding a
    /// `*` never sees the leading `/Users/…` it would have to cover.
    ///
    /// A `search.paths` entry that is absolute, or that escapes the root, has
    /// no root-relative form. Such a path is matched whole, which is what a
    /// glob for it has to be written against anyway.
    ///
    /// ref: lib/i18n/tasks/scanners/files/file_finder.rb:34-50, and accepted
    /// difference 26 for where this parts company with the gem.
    fn match_path(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/")
    }
}

/// `File.fnmatch` without `FNM_PATHNAME`, where `*` also crosses `/`. That is
/// `globset`'s default, so no builder options are needed.
fn build_globs(globs: &[String]) -> Result<GlobSet, String> {
    let mut b = GlobSetBuilder::new();
    for g in globs {
        b.add(Glob::new(g).map_err(|e| format!("bad glob `{g}`: {e}"))?);
        // A wildcard-free `app/webpack` names a directory, and pruning in
        // `Find.find` drops what is under it as well as the directory itself.
        // One extra variant covers that. A pattern holding a `*` needs none:
        // `*` crosses `/`, so it already reaches down.
        if !g.contains('*') {
            b.add(Glob::new(&format!("{g}/**")).map_err(|e| format!("bad glob `{g}`: {e}"))?);
        }
    }
    b.build().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    /// The file tree from spec/scanners/files/file_finder_spec.rb.
    fn project(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("i18n-tasks-rs-finder-{name}"));
        let _ = std::fs::remove_dir_all(&root);
        for rel in [
            "a/a/a/a.rb",
            "a/a/a.rb",
            "a/a/b.rb",
            "a/b/a.rb",
            "a/b/b.rb",
            "a.rb",
            ".hidden/a.rb",
            ".dotfile.rb",
        ] {
            let path = root.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            // Every file holds a needle, so none is dropped by the prefilter.
            std::fs::write(path, "t('key')\n").unwrap();
        }
        root
    }

    fn found(root: &Path, config_body: &str) -> Vec<String> {
        let path = root.join("i18n-tasks.yml");
        std::fs::write(&path, config_body).unwrap();
        let cfg = Config::parse(config_body, &path, root.to_path_buf()).expect("config parses");
        let mut out: Vec<String> = Finder::new(&cfg)
            .expect("globs compile")
            .discover()
            .files
            .iter()
            .map(|p| {
                p.strip_prefix(root)
                    .unwrap_or(p)
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        out.sort();
        out
    }

    /// ref: spec/scanners/files/file_finder_spec.rb "finds all the files"
    #[test]
    fn finds_every_file_under_the_search_paths() {
        let root = project("all");
        assert_eq!(
            found(&root, "search:\n  paths: [.]\n"),
            vec![
                "a.rb",
                "a/a/a.rb",
                "a/a/a/a.rb",
                "a/a/b.rb",
                "a/b/a.rb",
                "a/b/b.rb",
            ]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// ref: "finds only the files in paths"
    #[test]
    fn search_paths_restrict_the_walk() {
        let root = project("paths");
        assert_eq!(
            found(&root, "search:\n  paths: [a/a, a/b/a.rb]\n"),
            vec!["a/a/a.rb", "a/a/a/a.rb", "a/a/b.rb", "a/b/a.rb"]
        );
        // A configured path that does not exist is skipped, not an error.
        assert!(found(&root, "search:\n  paths: [nowhere]\n").is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// ref: "find only the files specified by the inclusion patterns"
    #[test]
    fn search_only_selects_a_subset() {
        let root = project("only");
        assert_eq!(
            found(&root, "search:\n  paths: [a]\n  only: ['a/a/**']\n"),
            vec!["a/a/a.rb", "a/a/a/a.rb", "a/a/b.rb"]
        );
        // An empty `only` list is the same as none at all.
        assert_eq!(found(&root, "search:\n  paths: [a]\n  only: []\n").len(), 5);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A glob holding a `*` is matched against the root-relative path, the way
    /// the gem matches the path `Find.find` yielded. Handing the globs an
    /// absolute path instead makes `exclude` silently do nothing and `only`
    /// silently match nothing, and an empty `only` makes `unused` report every
    /// key in the project.
    #[test]
    fn a_wildcard_glob_is_matched_relative_to_the_root() {
        let root = project("relative-globs");
        assert_eq!(
            found(&root, "search:\n  paths: [.]\n  exclude: ['a/*/b.rb']\n"),
            vec!["a.rb", "a/a/a.rb", "a/a/a/a.rb", "a/b/a.rb"]
        );
        assert_eq!(
            found(&root, "search:\n  paths: [.]\n  only: ['a/b/**']\n"),
            vec!["a/b/a.rb", "a/b/b.rb"]
        );
        // The other half of the same change: an absolute glob no longer
        // matches, because the target it is matched against is relative.
        let absolute = format!(
            "search:\n  paths: [.]\n  only: ['{}/a/b/**']\n",
            root.display()
        );
        assert!(found(&root, &absolute).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A wildcard-free glob still has to prune the directory it names and
    /// everything under it, which is what pruning in `Find.find` does.
    #[test]
    fn a_wildcard_free_glob_prunes_the_directory_it_names() {
        let root = project("prune-dir");
        assert_eq!(
            found(&root, "search:\n  paths: [.]\n  exclude: ['a/a']\n"),
            vec!["a.rb", "a/b/a.rb", "a/b/b.rb"]
        );
        // At the place it names, though, and not at every depth: `a/a` is not
        // `**/a/a`, and neither is what the gem's `fnmatch` would match.
        assert_eq!(
            found(&root, "search:\n  paths: [.]\n  exclude: ['a']\n"),
            vec!["a.rb"]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// ref: "finds only the files not specified by the exclusion patterns"
    #[test]
    fn search_exclude_prunes_a_subtree() {
        let root = project("exclude");
        assert_eq!(
            found(&root, "search:\n  paths: [.]\n  exclude: ['a/a']\n"),
            vec!["a.rb", "a/b/a.rb", "a/b/b.rb"]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A file named directly in `search.paths` is still subject to the hidden,
    /// `only` and `exclude` rules, because `Find.find` yields it like any other.
    #[test]
    fn a_file_named_in_search_paths_obeys_the_same_rules() {
        let root = project("named-file");
        // Named directly: found.
        assert_eq!(
            found(&root, "search:\n  paths: [a/b/a.rb]\n"),
            vec!["a/b/a.rb"]
        );
        // A hidden *directory* is pruned by the walk, but a file named
        // directly is reached, exactly as `Find.find` reaches it.
        assert_eq!(
            found(&root, "search:\n  paths: ['.hidden/a.rb']\n"),
            vec![".hidden/a.rb"]
        );
        // A hidden file is not, because its own basename opens with a dot.
        assert!(found(&root, "search:\n  paths: ['.dotfile.rb']\n").is_empty());
        // Named directly but excluded.
        assert!(
            found(
                &root,
                "search:\n  paths: [a/b/a.rb]\n  exclude: ['a/b/a.rb']\n"
            )
            .is_empty()
        );
        // Named directly but outside `only`.
        assert!(found(&root, "search:\n  paths: [a/b/a.rb]\n  only: ['a/a/**']\n").is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A file name that is not valid UTF-8 cannot be matched against a glob, so
    /// it is skipped rather than matched by accident. The name never has to
    /// exist for this: the check is on the name, not on the file.
    #[test]
    fn a_file_name_that_is_not_utf8_is_skipped() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let root = project("non-utf8");
        let body = "search:\n  paths: [.]\n";
        let cfg = Config::parse(body, &root.join("i18n-tasks.yml"), root.clone()).unwrap();
        let finder = Finder::new(&cfg).unwrap();
        let mut out = Vec::new();
        finder.consider(root.join(OsStr::from_bytes(b"bad\xff.rb")), &mut out);
        assert!(out.is_empty());
        // A name that is valid UTF-8 goes through.
        finder.consider(root.join("a.rb"), &mut out);
        assert_eq!(out.len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_bad_glob_is_reported_rather_than_ignored() {
        let root = project("badglob");
        let body = "search:\n  paths: [.]\n  exclude: ['a[']\n";
        let cfg = Config::parse(body, &root.join("i18n-tasks.yml"), root.clone()).unwrap();
        let e = Finder::new(&cfg)
            .err()
            .expect("a malformed glob must not compile");
        assert!(e.contains("bad glob `a[`"), "{e}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The prefilter drops a file with no translation call in it, and counts it.
    #[test]
    fn the_prefilter_counts_what_it_skipped() {
        let root = project("prefilter");
        std::fs::write(root.join("plain.rb"), "puts 1\n").unwrap();
        let body = "search:\n  paths: [.]\n";
        let cfg = Config::parse(body, &root.join("i18n-tasks.yml"), root.clone()).unwrap();
        let d = Finder::new(&cfg).unwrap().discover();
        assert_eq!(d.prefiltered, 1);
        assert!(!d.files.iter().any(|p| p.ends_with("plain.rb")));
        let _ = std::fs::remove_dir_all(&root);
    }
}
