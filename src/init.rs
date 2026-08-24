//! `init-config`: a config generated from the project's own layout.
//!
//! The gem's answer to "how do I start" is to copy a template
//! (`templates/config/i18n-tasks.yml`), which is the same file for every
//! project and therefore right about nothing except the defaults. This command
//! looks at the project instead:
//!
//!   * `data.read` is derived from the locale files that are actually there,
//!     and every one of them must be matched by a pattern that was emitted —
//!     checked here with [`locale_for_path`], the loader's own rule, not with a
//!     second implementation of it;
//!   * `data.write` is the first candidate target that those same patterns read
//!     back, so a later `normalize --write` cannot put keys where nothing looks
//!     for them;
//!   * `base_locale` comes from `config.i18n.default_locale` in the project's
//!     Ruby, read as text — blocker B3 applies here as everywhere else;
//!   * `search.relative_roots` keeps the gem defaults that exist, and adds a
//!     directory only when a file under it uses a relative key. That is exactly
//!     the condition under which a relative root does anything.
//!
//! Everything that cannot be detected is written out commented, so the file
//! still documents the supported surface the way the gem's template does.
//!
//! The result is parsed back with [`Config::parse`] and loaded with
//! [`Store::load`] before the command offers to write it, so the header can
//! report what the settings actually read.

use crate::config::{Config, DEFAULT_RELATIVE_ROOTS, interpolate_locale};
use crate::data::load::{Store, locale_for_path};
use crate::stats::forest_stats;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// The file the generated config is written to.
pub const INIT_TARGET: &str = "config/i18n-tasks-rs.yml";

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
    /// never matches `en/models.yml`. ref: data/load.rs#locale_pattern_re.
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

/// What the generated config read back.
#[derive(Debug, Clone, PartialEq)]
pub struct Verification {
    pub locales: Vec<String>,
    pub key_count: usize,
    pub files_read: usize,
    /// Set when the generated config did not load. The file is still produced:
    /// a config that needs one edit beats no config at all.
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Generated {
    pub output: String,
    pub detected: Detected,
    pub verified: Verification,
}

impl Generated {
    /// True when the generated config still needs a human.
    pub fn needs_attention(&self) -> bool {
        !self.detected.notes.is_empty() || self.detected.gem_config.is_some()
    }
}

/// Detects, renders, and reads the result back. `to` is only used in messages.
pub fn generate(root: &Path, to: &Path) -> Result<Generated, String> {
    let mut detected = detect(root);
    // The header reports what the settings read, so the file is rendered
    // twice: once to have something to load, once with the answer in it.
    let draft = render(&detected, None);
    let verified = verify(&draft, root, to);
    if let Some(err) = &verified.error {
        // Whatever the reason, it is in the error, and guessing at a fix here
        // would be wrong as often as right: a reference value in the data is
        // not a `data.read` problem.
        detected
            .notes
            .push(format!("the generated config did not load: {err}"));
    } else if verified.key_count == 0 && detected.files_seen > 0 {
        detected.notes.push(
            "the generated config loaded no keys. Check that each locale file has its \
             locale as the single top-level key."
                .into(),
        );
    }
    let output = render(&detected, Some(&verified));
    Ok(Generated {
        output,
        detected,
        verified,
    })
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
        walk(&base, &mut |p| {
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
        if matches!(bytes.get(j), Some(b'"') | Some(b'\'')) && bytes.get(j + 1) == Some(&b'.') {
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
    walk(&dir, &mut |p| {
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

fn walk(dir: &Path, visit: &mut impl FnMut(&Path)) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<PathBuf> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        let Ok(ty) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if ty.file_type().is_symlink() {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if ty.is_dir() {
            if !SKIP_DIRS.contains(&name) {
                walk(&path, visit);
            }
        } else if ty.is_file() {
            visit(&path);
        }
    }
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

/// Reads the generated config back and loads the data with it.
fn verify(output: &str, root: &Path, to: &Path) -> Verification {
    let empty = Verification {
        locales: Vec::new(),
        key_count: 0,
        files_read: 0,
        error: None,
    };
    let cfg = match Config::parse(output, to, root.to_path_buf()) {
        Ok(cfg) => cfg,
        Err(e) => {
            return Verification {
                error: Some(e),
                ..empty
            };
        }
    };
    let store = match Store::load(&cfg) {
        Ok(store) => store,
        Err(e) => {
            return Verification {
                error: Some(e),
                ..empty
            };
        }
    };
    let files: BTreeSet<&Path> = store
        .trees
        .values()
        .flat_map(|t| t.file_locales.keys().map(PathBuf::as_path))
        .collect();
    Verification {
        locales: store.locales.clone(),
        key_count: forest_stats(&store, &store.locales).key_count,
        files_read: files.len(),
        error: None,
    }
}

/// The generated file. `verified` is `None` for the draft that produces it.
fn render(d: &Detected, verified: Option<&Verification>) -> String {
    let mut out = String::new();
    out.push_str("# i18n-tasks-rs configuration.\n#\n");
    out.push_str("# Generated by `i18n-tasks-rs init-config` from the layout of this project.\n");
    out.push_str(
        "# This file is plain YAML. It is read, never evaluated: no ERB, no Ruby,\n\
         # no scanner class names. An unknown key is an error.\n#\n",
    );

    out.push_str("# Detected:\n");
    match &d.locale_dir {
        Some(dir) => out.push_str(&format!(
            "#   {dir}: {} locale file(s), {}\n",
            d.files_seen,
            if d.locales.is_empty() {
                "no locale named by any of them".to_string()
            } else {
                format!("locales {}", d.locales.join(", "))
            }
        )),
        None => out.push_str("#   no locale directory\n"),
    }
    match &d.base_locale_from {
        Some(at) => out.push_str(&format!(
            "#   base_locale {} from {at}\n",
            d.base_locale, // the assignment the project already makes
        )),
        None => out.push_str(&format!(
            "#   base_locale {}, assumed: the project's Ruby names no default_locale\n",
            d.base_locale
        )),
    }
    for root in &d.relative_roots_detected {
        out.push_str(&format!(
            "#   relative root {root}: not a default, but it uses relative keys\n"
        ));
    }
    if let Some(v) = verified.filter(|v| v.error.is_none()) {
        out.push_str(&format!(
            "#   read back: {} key(s) in {} locale(s) from {} file(s)\n",
            v.key_count,
            v.locales.len(),
            v.files_read
        ));
    }
    if let Some(gem) = &d.gem_config {
        out.push('#');
        out.push('\n');
        out.push_str(&wrap_comment(&format!(
            "{} is still in the project. `i18n-tasks-rs migrate-config` converts it, and \
             keeps the ignore lists, which this file has none of.",
            gem.display().to_string().replace('\\', "/")
        )));
    }
    if !d.notes.is_empty() {
        out.push_str("#\n# NEEDS ATTENTION:\n");
        for note in &d.notes {
            out.push_str(&wrap_comment(note));
        }
    }

    out.push_str(&format!("\nbase_locale: {}\n", d.base_locale));
    if !d.locales.is_empty() {
        out.push_str(&format!(
            "# locales: [{}]\n\
             # Detected, and left unset on purpose: with no list here the locales come\n\
             # from the files `data.read` matches, so a new one needs no edit.\n",
            d.locales.join(", ")
        ));
    }

    out.push_str("\ndata:\n  read:\n");
    for pattern in &d.read {
        out.push_str(&format!("    - {}\n", quoted(pattern)));
    }
    out.push_str("  # Where a new key goes, and only under `normalize --write`.\n  write:\n");
    out.push_str(&format!("    - {}\n", quoted(&d.write)));
    out.push_str(
        "  # router: conservative_router    # or pattern_router\n\
         \x20 # keep_order: false\n\
         \x20 # external: []                   # read-only data: never missing, never unused\n",
    );

    out.push_str("\nsearch:\n  paths:\n");
    for path in &d.search_paths {
        out.push_str(&format!("    - {path}\n"));
    }
    if !d.exclude.is_empty() {
        out.push_str("  exclude:\n");
        for path in &d.exclude {
            out.push_str(&format!("    - {path}\n"));
        }
    } else {
        out.push_str("  # exclude: []\n");
    }
    if d.relative_roots.is_empty() {
        // Not omitted: leaving the key out would restore the gem's five
        // defaults, none of which this project has.
        out.push_str("  relative_roots: []   # no directory here uses relative keys\n");
    } else {
        out.push_str("  relative_roots:\n");
        for root in &d.relative_roots {
            out.push_str(&format!("    - {root}\n"));
        }
    }
    out.push_str(
        "  # only: []\n\
         \x20 # relative_exclude_method_name_paths: []\n",
    );

    out.push_str(
        "\n# Nothing is ignored yet. Each list takes key patterns — `devise.*`,\n\
         # `{a,b}.*`, `a.*.b` — and `ignore` applies to every check at once.\n\
         #\n\
         # ignore: []\n\
         # ignore_missing: []\n\
         # ignore_unused: []\n\
         # ignore_eq_base: []\n\
         # ignore_inconsistent_interpolations: []\n",
    );
    out
}

/// A `data` path is quoted when YAML would read it as anything but a string.
fn quoted(path: &str) -> String {
    if path.starts_with([
        '*', '&', '{', '[', '"', '\'', '%', '@', '`', '!', '|', '>', '#',
    ]) || path.contains(": ")
    {
        format!("\"{}\"", path.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        path.to_string()
    }
}

/// One note, wrapped into comment lines that stay inside 80 columns.
fn wrap_comment(note: &str) -> String {
    let mut out = String::new();
    let mut line = String::from("#   ");
    for word in note.split_whitespace() {
        if line.len() + 1 + word.len() > 79 && line.len() > 4 {
            out.push_str(line.trim_end());
            out.push('\n');
            line = String::from("#     ");
        }
        line.push_str(word);
        line.push(' ');
    }
    out.push_str(line.trim_end());
    out.push('\n');
    out
}

/// The report for the terminal.
pub fn to_text(g: &Generated, to: &Path, written: bool) -> String {
    let d = &g.detected;
    let mut s = String::new();
    s.push_str(&format!(
        "{} -> {}\n",
        d.locale_dir.clone().unwrap_or_else(|| "(no data)".into()),
        to.display()
    ));
    s.push_str(&format!(
        "  data.read: {} pattern(s) covering {} locale file(s)\n",
        d.read.len(),
        d.files_seen
    ));
    s.push_str(&format!("  data.write: {}\n", d.write));
    match &d.base_locale_from {
        Some(at) => s.push_str(&format!("  base_locale: {} from {at}\n", d.base_locale)),
        None => s.push_str(&format!("  base_locale: {}, assumed\n", d.base_locale)),
    }
    s.push_str(&format!("  search.paths: {}\n", d.search_paths.join(", ")));
    s.push_str(&format!(
        "  search.relative_roots: {}{}\n",
        if d.relative_roots.is_empty() {
            "none".into()
        } else {
            d.relative_roots.join(", ")
        },
        if d.relative_roots_detected.is_empty() {
            String::new()
        } else {
            format!(" ({} detected)", d.relative_roots_detected.join(", "))
        }
    ));
    if g.verified.error.is_none() {
        s.push_str(&format!(
            "  read back: {} key(s) in {} locale(s) from {} file(s)\n",
            g.verified.key_count,
            g.verified.locales.len(),
            g.verified.files_read
        ));
    }
    if let Some(gem) = &d.gem_config {
        s.push_str(&format!(
            "  NOTE {} exists. `migrate-config` converts it and keeps the ignore lists.\n",
            gem.display()
        ));
    }
    for note in &d.notes {
        s.push_str(&format!("  NEEDS ATTENTION: {note}\n"));
    }
    if written {
        s.push_str(&format!("  wrote {}\n", to.display()));
    } else {
        s.push_str("  nothing written. Pass `--write` to create the file.\n");
    }
    s
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

    /// Every generated path starts with the locale directory, so none of them
    /// needs quoting. The rule is here for the one that would: a `data.read`
    /// beginning with `*` is a YAML alias.
    #[test]
    fn a_path_is_quoted_only_when_yaml_needs_it() {
        for plain in [
            "config/locales/%{locale}.yml",
            "config/locales/*.%{locale}.yml",
            "config/locales/%{locale}/**/*.yml",
        ] {
            assert_eq!(quoted(plain), plain);
        }
        assert_eq!(quoted("*.%{locale}.yml"), "\"*.%{locale}.yml\"");
    }
}
