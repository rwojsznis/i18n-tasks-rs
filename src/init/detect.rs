//! What the project looks like: the locale files, the base locale, the search
//! paths, and the directories that use relative keys.
//!
//! Everything here reads. Nothing is executed — blocker B3 — so a setting the
//! project computes in Ruby is not detected, and [`detect`] says so in a note
//! rather than guessing.

use crate::config::{DEFAULT_RELATIVE_ROOTS, interpolate_locale};
use crate::data::load::locale_for_path;
use crate::walk::{Descend, walk};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Where locale data is looked for, in order of preference.
const LOCALE_DIR_CANDIDATES: &[&str] = &["config/locales", "locales", "app/locales"];

/// ref: lib/i18n/tasks/used_keys.rb#SEARCH_DEFAULTS is `app/` alone. `lib/` is
/// added when it exists, and that is deliberate: a key used only from a rake
/// task or a service object under `lib/` would otherwise be reported unused,
/// and acting on that report deletes a live translation. The cost is the other
/// direction — a vendored blob under `lib/` can invent a used key, which shows
/// up in `missing` where a human sees it. Accepted difference 25.
const SEARCH_PATH_CANDIDATES: &[&str] = &["app/", "lib/"];

/// Build output that lives under a search path. `ALWAYS_EXCLUDE` covers the
/// asset *extensions*; these are whole directories of generated source.
const EXCLUDE_CANDIDATES: &[&str] = &[
    "app/assets/builds",
    "app/assets/build",
    "app/webpack",
    "app/javascript/node_modules",
    "lib/assets",
    "lib/node_modules",
];

/// Extensions read when looking for evidence of a relative key.
const SOURCE_EXTS: &[&str] = &[
    "rb", "rake", "erb", "haml", "slim", "js", "jsx", "mjs", "es6", "ts", "tsx", "vue",
];

/// Never walked, whether looking for locale data or for source.
const SKIP_DIRS: &[&str] = &["node_modules", "tmp", "coverage", ".git", ".yardoc"];

/// The layouts a locale file can have, in the order the patterns are emitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Shape {
    /// `config/locales/en.yml`
    Flat,
    /// `config/locales/devise.en.yml`
    Namespaced,
    /// `config/locales/en/models.yml`
    LocaleDir,
    /// `config/locales/en/admin/panel.yml`
    LocaleDirDeep,
    /// `config/locales/admin/en.yml`
    SubdirFlat,
    /// `config/locales/admin/devise.en.yml`
    SubdirNamespaced,
}

impl Shape {
    /// The `data.read` pattern this shape needs. `dir` has no trailing slash.
    ///
    /// `LocaleDir` and `LocaleDirDeep` are two patterns and not one because the
    /// loader's `**` becomes `.*` between two slashes: `%{locale}/**/*.yml`
    /// never matches `en/models.yml`. ref: data/load/locale_path.rs#locale_pattern_re.
    fn pattern(self, dir: &str, ext: &str) -> String {
        match self {
            Shape::Flat => format!("{dir}/%{{locale}}.{ext}"),
            Shape::Namespaced => format!("{dir}/*.%{{locale}}.{ext}"),
            Shape::LocaleDir => format!("{dir}/%{{locale}}/*.{ext}"),
            Shape::LocaleDirDeep => format!("{dir}/%{{locale}}/**/*.{ext}"),
            Shape::SubdirFlat => format!("{dir}/**/%{{locale}}.{ext}"),
            Shape::SubdirNamespaced => format!("{dir}/**/*.%{{locale}}.{ext}"),
        }
    }
}

/// What the project turned out to look like.
#[derive(Debug, Clone)]
pub struct Detected {
    /// The locale directory, relative to the root, or `None` when none held
    /// any data.
    pub locale_dir: Option<String>,
    /// Locale files found under it, relative to the root.
    pub files_seen: usize,
    /// Files no emitted pattern reads. Always empty in a clean detection.
    pub unmatched: Vec<String>,
    pub read: Vec<String>,
    pub write: String,
    pub locales: Vec<String>,
    pub base_locale: String,
    /// `path:line` of the `default_locale` assignment, when there was one.
    pub base_locale_from: Option<String>,
    pub search_paths: Vec<String>,
    pub exclude: Vec<String>,
    pub relative_roots: Vec<String>,
    /// The subset of `relative_roots` that is not a gem default and was added
    /// because a file under it uses a relative key.
    pub relative_roots_detected: Vec<String>,
    /// A gem config in the project. `migrate-config` is the better command
    /// there, because it keeps the ignore lists.
    pub gem_config: Option<PathBuf>,
    /// Everything a human should look at before trusting the file.
    pub notes: Vec<String>,
}

/// Everything is read, nothing is executed.
pub fn detect(root: &Path) -> Detected {
    let mut notes = Vec::new();

    let locale_dir = LOCALE_DIR_CANDIDATES
        .iter()
        .find(|c| !locale_files(root, c).is_empty())
        .map(|c| (*c).to_string());

    let files: Vec<String> = match &locale_dir {
        Some(dir) => locale_files(root, dir),
        None => Vec::new(),
    };
    let dir = locale_dir
        .clone()
        .unwrap_or_else(|| "config/locales".into());

    // A pattern per shape and extension seen, so a project mixing `.yml` and
    // `.yaml`, or flat and per-locale-directory files, gets both.
    let mut shapes: BTreeSet<(Shape, String)> = BTreeSet::new();
    let mut ext_counts: BTreeMap<String, usize> = BTreeMap::new();
    for rel in &files {
        let under = rel[dir.len() + 1..].to_string();
        let ext = under.rsplit_once('.').map_or("yml", |(_, e)| e).to_string();
        *ext_counts.entry(ext.clone()).or_default() += 1;
        if let Some(shape) = classify(&under) {
            shapes.insert((shape, ext));
        }
    }
    let read: Vec<String> = if shapes.is_empty() {
        // Nothing recognisable, or nothing at all. The gem's default is still
        // the right starting point.
        vec![format!("{dir}/%{{locale}}.yml")]
    } else {
        shapes.iter().map(|(s, e)| s.pattern(&dir, e)).collect()
    };

    if files.is_empty() {
        notes.push(format!(
            "no locale files under {}. `data.read` below is the gem's default; \
             point it at the real data.",
            LOCALE_DIR_CANDIDATES.join(", ")
        ));
    }

    // The property the whole command rests on, checked with the loader's rule.
    let mut locales: BTreeSet<String> = BTreeSet::new();
    let mut unmatched = Vec::new();
    for rel in &files {
        let mut matched = false;
        for pattern in &read {
            if let Some(locale) = locale_for_path(pattern, rel) {
                locales.insert(locale);
                matched = true;
            }
        }
        if !matched {
            unmatched.push(rel.clone());
        }
    }
    if !unmatched.is_empty() {
        notes.push(format!(
            "{} locale file(s) name no locale under the patterns below, so they are \
             never read: {}",
            unmatched.len(),
            preview(&unmatched)
        ));
    }
    let locales: Vec<String> = locales.into_iter().collect();

    let ruby_default = detect_base_locale(root);
    let base_locale = match &ruby_default {
        Some((locale, _)) => locale.clone(),
        None => match locales.as_slice() {
            [only] => only.clone(),
            many if many.iter().any(|l| l == "en") => "en".into(),
            [] => "en".into(),
            many => {
                notes.push(format!(
                    "no `config.i18n.default_locale` in the project's Ruby, and none of \
                     {} is obviously the base. Assumed {}.",
                    many.join(", "),
                    many[0]
                ));
                many[0].clone()
            }
        },
    };
    if !locales.is_empty() && !locales.contains(&base_locale) {
        notes.push(format!(
            "base_locale is {base_locale}, but the locale files hold {}. \
             One of the two is wrong.",
            locales.join(", ")
        ));
    }

    let ext = ext_counts
        .iter()
        .max_by_key(|(_, n)| **n)
        .map_or("yml", |(e, _)| e.as_str());
    let write = write_target(&dir, ext, &base_locale, &read).unwrap_or_else(|| {
        notes.push(format!(
            "no obvious place to write new keys: no candidate target under {dir} is read \
             back by the patterns below, so `normalize --write` would put keys where \
             nothing looks for them. Set `data.write` by hand."
        ));
        format!("{dir}/%{{locale}}.{ext}")
    });

    let search_paths: Vec<String> = SEARCH_PATH_CANDIDATES
        .iter()
        .filter(|p| root.join(p.trim_end_matches('/')).is_dir())
        .map(|p| (*p).to_string())
        .collect();
    let search_paths = if search_paths.is_empty() {
        notes.push(
            "neither app/ nor lib/ exists, so `search.paths` is the whole project. \
             Narrow it."
                .into(),
        );
        vec!["./".to_string()]
    } else {
        search_paths
    };

    let exclude: Vec<String> = EXCLUDE_CANDIDATES
        .iter()
        .filter(|c| root.join(c).is_dir())
        .filter(|c| {
            search_paths
                .iter()
                .any(|p| p.as_str() == "./" || c.starts_with(p.trim_end_matches('/')))
        })
        .map(|c| (*c).to_string())
        .collect();

    let (relative_roots, relative_roots_detected) = relative_roots(root, &search_paths, &exclude);

    Detected {
        locale_dir,
        files_seen: files.len(),
        unmatched,
        read,
        write,
        locales,
        base_locale,
        base_locale_from: ruby_default.map(|(_, at)| at),
        search_paths,
        exclude,
        relative_roots,
        relative_roots_detected,
        // Kept project-relative, like every other path in the file.
        gem_config: crate::migrate::find_gem_config(root)
            .map(|p| p.strip_prefix(root).unwrap_or(&p).to_path_buf()),
        notes,
    }
}

/// The first candidate target the read patterns read back.
///
/// Writing to a path nothing reads is the one failure a generated config can
/// hide until `normalize --write` has already moved the keys.
fn write_target(dir: &str, ext: &str, base: &str, read: &[String]) -> Option<String> {
    let candidates = [
        format!("{dir}/%{{locale}}.{ext}"),
        format!("{dir}/common.%{{locale}}.{ext}"),
        format!("{dir}/%{{locale}}/common.{ext}"),
    ];
    candidates.into_iter().find(|c| {
        let concrete = interpolate_locale(c, base);
        read.iter()
            .any(|p| locale_for_path(p, &concrete).as_deref() == Some(base))
    })
}

/// The shape of one locale file, named relative to the locale directory.
fn classify(under_dir: &str) -> Option<Shape> {
    let (dirs, file) = match under_dir.rsplit_once('/') {
        Some((d, f)) => (d, f),
        None => ("", under_dir),
    };
    let stem = file.rsplit_once('.').map_or(file, |(s, _)| s);
    let namespaced = || {
        stem.rsplit_once('.')
            .is_some_and(|(_, last)| looks_like_locale(last))
    };
    if dirs.is_empty() {
        if looks_like_locale(stem) {
            Some(Shape::Flat)
        } else if namespaced() {
            Some(Shape::Namespaced)
        } else {
            None
        }
    } else if looks_like_locale(dirs.split('/').next().unwrap_or_default()) {
        Some(if dirs.contains('/') {
            Shape::LocaleDirDeep
        } else {
            Shape::LocaleDir
        })
    } else if looks_like_locale(stem) {
        Some(Shape::SubdirFlat)
    } else if namespaced() {
        Some(Shape::SubdirNamespaced)
    } else {
        None
    }
}

/// An i18n locale code: `en`, `pt-BR`, `zh-Hans-CN`, and the `_` spelling.
///
/// Deliberately narrow. A false positive here invents a locale out of a file
/// named after something else, and `unused` would then report every key in it.
fn looks_like_locale(s: &str) -> bool {
    let mut parts = s.split(['-', '_']);
    let Some(first) = parts.next() else {
        return false;
    };
    if !(2..=3).contains(&first.len()) || !first.bytes().all(|b| b.is_ascii_lowercase()) {
        return false;
    }
    parts.all(|p| (2..=8).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_alphanumeric()))
}

/// `config.i18n.default_locale`, read as text. Blocker B3: no Ruby is run, so
/// a computed value is not detected, and the fallback reports itself.
fn detect_base_locale(root: &Path) -> Option<(String, String)> {
    let mut candidates = vec![PathBuf::from("config/application.rb")];
    for dir in ["config/environments", "config/initializers"] {
        let Ok(entries) = std::fs::read_dir(root.join(dir)) else {
            continue;
        };
        let mut rbs: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "rb"))
            .filter_map(|p| p.strip_prefix(root).ok().map(Path::to_path_buf))
            .collect();
        rbs.sort();
        candidates.extend(rbs);
    }
    for rel in candidates {
        let Ok(src) = std::fs::read_to_string(root.join(&rel)) else {
            continue;
        };
        for (i, line) in src.lines().enumerate() {
            // A commented-out assignment is the most common thing in these
            // files, and it is never the answer.
            let code = line.split_once('#').map_or(line, |(before, _)| before);
            if let Some(locale) = default_locale_in(code) {
                return Some((
                    locale,
                    format!("{}:{}", rel.display().to_string().replace('\\', "/"), i + 1),
                ));
            }
        }
    }
    None
}

/// `... .default_locale = :de` / `= "de"` / `= 'de'`. A value built from
/// anything else is not a literal and is left alone.
fn default_locale_in(code: &str) -> Option<String> {
    let at = code.find(".default_locale")?;
    let rest = code[at + ".default_locale".len()..].trim_start();
    let rest = rest.strip_prefix('=')?.trim_start();
    // `==` is a comparison, not an assignment.
    let (quote, rest) = match rest.as_bytes().first()? {
        b':' => (None, &rest[1..]),
        b'"' => (Some('"'), &rest[1..]),
        b'\'' => (Some('\''), &rest[1..]),
        _ => return None,
    };
    let end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
        .unwrap_or(rest.len());
    let locale = &rest[..end];
    if locale.is_empty() || quote.is_some_and(|q| !rest[end..].starts_with(q)) {
        return None;
    }
    Some(locale.to_string())
}

/// The gem defaults that exist, plus every directory holding a relative key.
///
/// Returns the merged list and the subset that detection added.
fn relative_roots(
    root: &Path,
    search_paths: &[String],
    exclude: &[String],
) -> (Vec<String>, Vec<String>) {
    let mut all: BTreeSet<String> = DEFAULT_RELATIVE_ROOTS
        .iter()
        .filter(|d| root.join(d).is_dir())
        .map(|d| (*d).to_string())
        .collect();

    let mut found: BTreeSet<String> = BTreeSet::new();
    for path in search_paths {
        let base = root.join(path.trim_end_matches('/'));
        let mut files = Vec::new();
        walk_files(&base, &mut |p| {
            if p.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| SOURCE_EXTS.contains(&e))
            {
                files.push(p.to_path_buf());
            }
        });
        for file in files {
            let Ok(bytes) = std::fs::read(&file) else {
                continue;
            };
            if !uses_relative_key(&bytes) {
                continue;
            }
            let Ok(rel) = file.strip_prefix(root) else {
                continue;
            };
            let rel = rel.to_string_lossy().replace('\\', "/");
            if exclude.iter().any(|e| rel.starts_with(e.as_str())) {
                continue;
            }
            if let Some(rr) = enclosing_root(&rel) {
                found.insert(rr);
            }
        }
    }
    let detected: Vec<String> = found.difference(&all).cloned().collect();
    all.extend(found);
    (all.into_iter().collect(), detected)
}

/// The relative root a source file belongs to: the first two path components,
/// which is the shape of every gem default.
fn enclosing_root(rel: &str) -> Option<String> {
    let parts: Vec<&str> = rel.split('/').collect();
    match parts.len() {
        0 | 1 => None,
        2 => Some(parts[0].to_string()),
        _ => Some(format!("{}/{}", parts[0], parts[1])),
    }
}

/// True when the bytes hold a `t`, `t!` or `translate` call whose first
/// argument is a string literal opening with a dot.
///
/// The identifier boundary is the point of doing this by hand: `format(".2f")`
/// ends in `t(".`, and a substring search would call it a relative key.
fn uses_relative_key(bytes: &[u8]) -> bool {
    let ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b't' || (i > 0 && ident(bytes[i - 1])) {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        if bytes[j..].starts_with(b"ranslate") {
            j += "ranslate".len();
        }
        if bytes.get(j) == Some(&b'!') {
            j += 1;
        }
        if bytes.get(j).is_some_and(|b| ident(*b)) {
            i += 1;
            continue;
        }
        while bytes.get(j) == Some(&b' ') {
            j += 1;
        }
        if bytes.get(j) == Some(&b'(') {
            j += 1;
            while bytes.get(j) == Some(&b' ') {
                j += 1;
            }
        }
        if matches!(bytes.get(j), Some(b'"' | b'\'')) && bytes.get(j + 1) == Some(&b'.') {
            return true;
        }
        i += 1;
    }
    false
}

/// Every `.yml`/`.yaml` file under the candidate directory, as project-relative
/// slash paths, sorted.
fn locale_files(root: &Path, candidate: &str) -> Vec<String> {
    let dir = root.join(candidate);
    let mut out = Vec::new();
    walk_files(&dir, &mut |p| {
        if p.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e == "yml" || e == "yaml")
        {
            out.push(p.to_path_buf());
        }
    });
    let mut rels: Vec<String> = out
        .iter()
        .filter_map(|p| {
            let tail = p
                .strip_prefix(&dir)
                .ok()?
                .to_string_lossy()
                .replace('\\', "/");
            Some(format!("{candidate}/{tail}"))
        })
        .collect();
    rels.sort();
    rels
}

/// Every file under `dir`, minus the directories no project keeps its own
/// source or data in.
fn walk_files(dir: &Path, visit: &mut impl FnMut(&Path)) {
    walk(dir, &mut |path, is_dir| {
        if !is_dir {
            visit(path);
            return Descend::No;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if SKIP_DIRS.contains(&name) {
            Descend::No
        } else {
            Descend::Yes
        }
    });
}

fn preview(items: &[String]) -> String {
    let shown: Vec<&str> = items.iter().take(3).map(String::as_str).collect();
    if items.len() > shown.len() {
        format!(
            "{}, and {} more",
            shown.join(", "),
            items.len() - shown.len()
        )
    } else {
        shown.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locale_codes_are_recognised_narrowly() {
        for yes in ["en", "de", "fra", "pt-BR", "zh-Hans", "zh-Hans-CN", "pt_BR"] {
            assert!(looks_like_locale(yes), "{yes}");
        }
        // A file named after its content, which is the common case in a
        // namespaced layout, must not be read as a locale.
        for no in ["devise", "base", "e", "EN", "en-", "en2", "1en", ""] {
            assert!(!looks_like_locale(no), "{no}");
        }
    }

    #[test]
    fn every_layout_has_a_shape() {
        assert_eq!(classify("en.yml"), Some(Shape::Flat));
        assert_eq!(classify("pt-BR.yaml"), Some(Shape::Flat));
        assert_eq!(classify("devise.en.yml"), Some(Shape::Namespaced));
        assert_eq!(classify("en/models.yml"), Some(Shape::LocaleDir));
        assert_eq!(classify("en/admin/panel.yml"), Some(Shape::LocaleDirDeep));
        assert_eq!(classify("admin/en.yml"), Some(Shape::SubdirFlat));
        assert_eq!(
            classify("admin/devise.en.yml"),
            Some(Shape::SubdirNamespaced)
        );
        // Nothing in the name is a locale.
        assert_eq!(classify("defaults.yml"), None);
        assert_eq!(classify("admin/defaults.yml"), None);
    }

    /// Every shape's pattern has to read back the file that produced it, under
    /// the loader's rule and not this module's idea of one.
    #[test]
    fn each_pattern_reads_the_file_it_came_from() {
        for (file, shape) in [
            ("en.yml", Shape::Flat),
            ("devise.en.yml", Shape::Namespaced),
            ("en/models.yml", Shape::LocaleDir),
            ("en/admin/panel.yml", Shape::LocaleDirDeep),
            ("admin/en.yml", Shape::SubdirFlat),
            ("admin/devise.en.yml", Shape::SubdirNamespaced),
        ] {
            assert_eq!(classify(file), Some(shape), "{file}");
            let pattern = shape.pattern("config/locales", "yml");
            assert_eq!(
                locale_for_path(&pattern, &format!("config/locales/{file}")).as_deref(),
                Some("en"),
                "{pattern} does not read {file}"
            );
        }
    }

    /// The reason `LocaleDir` and `LocaleDirDeep` are two patterns: `**`
    /// becomes `.*` *between two slashes*, so it never matches nothing.
    #[test]
    fn the_deep_pattern_alone_misses_the_shallow_file() {
        let deep = Shape::LocaleDirDeep.pattern("config/locales", "yml");
        assert_eq!(locale_for_path(&deep, "config/locales/en/models.yml"), None);
        let shallow = Shape::LocaleDir.pattern("config/locales", "yml");
        assert_eq!(
            locale_for_path(&shallow, "config/locales/en/models.yml").as_deref(),
            Some("en")
        );
    }

    #[test]
    fn the_write_target_is_one_the_read_patterns_read() {
        let flat = vec!["config/locales/%{locale}.yml".to_string()];
        assert_eq!(
            write_target("config/locales", "yml", "en", &flat).as_deref(),
            Some("config/locales/%{locale}.yml")
        );
        // A namespaced layout does not read `config/locales/en.yml` at all.
        let namespaced = vec!["config/locales/*.%{locale}.yml".to_string()];
        assert_eq!(
            write_target("config/locales", "yml", "en", &namespaced).as_deref(),
            Some("config/locales/common.%{locale}.yml")
        );
        let dirs = vec!["config/locales/%{locale}/*.yml".to_string()];
        assert_eq!(
            write_target("config/locales", "yml", "en", &dirs).as_deref(),
            Some("config/locales/%{locale}/common.yml")
        );
        // Nothing fits: the caller has to say so rather than invent a path.
        let odd = vec!["config/locales/deep/**/%{locale}.yml".to_string()];
        assert_eq!(write_target("config/locales", "yml", "en", &odd), None);
    }

    #[test]
    fn default_locale_is_read_from_the_assignment_only() {
        assert_eq!(
            default_locale_in("    config.i18n.default_locale = :de"),
            Some("de".into())
        );
        assert_eq!(
            default_locale_in("I18n.default_locale = \"pt-BR\""),
            Some("pt-BR".into())
        );
        assert_eq!(
            default_locale_in("config.i18n.default_locale='fr'"),
            Some("fr".into())
        );
        // Not an assignment to a literal.
        assert_eq!(default_locale_in("x = I18n.default_locale"), None);
        assert_eq!(
            default_locale_in("config.i18n.default_locale = ENV['LOCALE']"),
            None
        );
        assert_eq!(default_locale_in("if I18n.default_locale == :de"), None);
        // An unterminated quote is not a locale either.
        assert_eq!(default_locale_in("I18n.default_locale = \"de"), None);
        // A different setting that merely contains the word.
        assert_eq!(
            default_locale_in("config.i18n.available_locales = [:de]"),
            None
        );
    }

    #[test]
    fn relative_keys_are_found_without_a_parser() {
        for yes in [
            "t('.title')",
            "t(\".title\")",
            "<%= t \".title\" %>",
            "t!('.title')",
            "translate('.title')",
            "= t '.title'",
        ] {
            assert!(uses_relative_key(yes.as_bytes()), "{yes}");
        }
        for no in [
            // The trap a substring search falls into.
            "format(\".2f\", x)",
            "t('title')",
            "assert(\".x\")",
            "I18n.t(\"a.b\")",
            "attr(\".x\")",
        ] {
            assert!(!uses_relative_key(no.as_bytes()), "{no}");
        }
    }

    #[test]
    fn the_relative_root_is_the_first_two_components() {
        assert_eq!(
            enclosing_root("app/views/users/index.html.erb").as_deref(),
            Some("app/views")
        );
        assert_eq!(enclosing_root("app/a.rb").as_deref(), Some("app"));
        assert_eq!(enclosing_root("a.rb"), None);
    }

    /// The note that lists unreadable files quotes a few of them and counts the
    /// rest, because a project can have hundreds and the header is read by a
    /// human.
    #[test]
    fn a_note_previews_three_items_and_counts_the_rest() {
        let items: Vec<String> = ["a.yml", "b.yml", "c.yml", "d.yml", "e.yml"]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        assert_eq!(preview(&items), "a.yml, b.yml, c.yml, and 2 more");
        assert_eq!(preview(&items[..3]), "a.yml, b.yml, c.yml");
        assert_eq!(preview(&items[..1]), "a.yml");
        assert_eq!(preview(&[]), "");
    }
}
