//! Locale data loading.
//!
//! ref: lib/i18n/tasks/data/file_system_base.rb
//! ref: lib/i18n/tasks/data/adapter/yaml_adapter.rb
//!
//! Design decision 1 in `docs/design-notes.md`: a flat key map, not a node
//! tree. The gem's
//! `select_nodes` deep-copies every matching node through `node.derive`
//! (`data/tree/traversal.rb:93-128`), which is 2.04 s of a 5.5 s `unused` run.

use crate::config::{Config, interpolate_locale};
use crate::walk::{Descend, walk};
use crate::yaml::{self, Node, Resolved};
use rayon::prelude::*;
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A leaf value. Anything that is not a mapping is a leaf, which is what
/// `Node.from_key_value` does with a non-Hash value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Str(String),
    Nil,
    Bool(bool),
    /// The scalar as written, for a number or any other plain scalar.
    Plain(String),
    /// A YAML sequence. The gem treats a sequence as a leaf value.
    Seq(Vec<Value>),
    /// A mapping nested inside a sequence. The flattener never produces one at
    /// the top of a leaf, because it walks into every mapping, so this only
    /// occurs under a `Seq`. A large real-world project has dozens of them.
    Map(Vec<(String, Value)>),
}

impl Value {
    /// Ruby `#to_s`, which is what `forest_stats` counts characters of.
    /// ref: lib/i18n/tasks/stats.rb
    pub fn to_display_string(&self) -> String {
        match self {
            // A plain scalar and a quoted one differ only in how they were
            // written, so `#to_s` cannot tell them apart.
            Value::Str(s) | Value::Plain(s) => s.clone(),
            Value::Nil => String::new(),
            Value::Bool(b) => b.to_string(),
            // Ruby `Array#to_s` and `Hash#to_s` are the `inspect` forms.
            Value::Seq(_) | Value::Map(_) => self.inspect(),
        }
    }

    fn inspect(&self) -> String {
        match self {
            Value::Str(s) => format!("{s:?}"),
            Value::Nil => "nil".into(),
            Value::Bool(b) => b.to_string(),
            Value::Plain(s) => s.clone(),
            Value::Seq(items) => {
                let inner: Vec<String> = items.iter().map(Value::inspect).collect();
                format!("[{}]", inner.join(", "))
            }
            // Ruby 3.4 onwards writes `{"a" => "b"}`, with spaces around `=>`.
            Value::Map(entries) => {
                let inner: Vec<String> = entries
                    .iter()
                    .map(|(k, v)| format!("{k:?} => {}", v.inspect()))
                    .collect();
                format!("{{{}}}", inner.join(", "))
            }
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) | Value::Plain(s) => Some(s),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Leaf {
    /// The dotted key, without the locale.
    pub key: String,
    pub value: Value,
    /// Number of key levels, which is what `forest_stats` calls a segment.
    pub depth: u16,
    /// Blocker B8: the conservative router needs the origin file per key.
    pub path: Arc<Path>,
    /// Set only when a key segment holds a dot, which the dotted form cannot
    /// express. The emitter needs the real segments, or it would split
    /// `2.5` into two nesting levels and rewrite the file wrongly.
    pub odd_segments: Option<Box<[Box<str>]>>,
}

impl Leaf {
    /// The real key segments.
    pub fn segments(&self) -> Vec<&str> {
        match &self.odd_segments {
            Some(segs) => segs.iter().map(AsRef::as_ref).collect(),
            None => self.key.split('.').collect(),
        }
    }
}

// Sibling examinations made while recording each key's parent-child pairs.
// H21: the pairs are deduplicated, and the deduplication must not scan the
// siblings already recorded.
#[cfg(test)]
thread_local! {
    static SIBLINGS_EXAMINED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn note_siblings_examined(n: usize) {
    SIBLINGS_EXAMINED.with(|c| c.set(c.get() + n));
}

// Locales read on the calling thread. `Store::load` fans the locales out over
// rayon, which runs the whole job inside the pool when the caller is not a
// worker, so a parallel load leaves this at zero.
#[cfg(test)]
thread_local! {
    static LOCALES_READ: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn note_locale_read() {
    LOCALES_READ.with(|c| c.set(c.get() + 1));
}

#[cfg(test)]
fn locales_read_on_this_thread() -> usize {
    LOCALES_READ.with(std::cell::Cell::get)
}

#[derive(Debug, Default)]
pub struct LocaleTree {
    pub locale: String,
    pub leaves: Vec<Leaf>,
    index: HashMap<String, usize>,
    /// Every key that is an interior node, so ancestor lookups are cheap.
    interior: HashSet<String>,
    /// Immediate child segment names, in insertion order.
    children: HashMap<String, Vec<String>>,
    /// Keys whose children are all leaves with a plural suffix.
    /// ref: lib/i18n/tasks/plural_keys.rb#plural_forms?
    plural_nodes: HashSet<String>,
    /// Every top-level key of each file read for this locale. `normalize`
    /// writes one locale per file, so it refuses to touch a file that holds a
    /// locale it is not writing.
    pub file_locales: HashMap<PathBuf, Vec<String>>,
}

impl LocaleTree {
    pub fn get(&self, key: &str) -> Option<&Leaf> {
        self.index.get(key).map(|&i| &self.leaves[i])
    }

    /// True when the key names a leaf or an interior node, which is what
    /// `key_value?` tests through `value_or_children_hash`.
    pub fn has_key(&self, key: &str) -> bool {
        self.index.contains_key(key) || self.interior.contains(key)
    }

    pub fn is_interior(&self, key: &str) -> bool {
        self.interior.contains(key)
    }

    /// Leaves sorted by key, for deterministic reports.
    pub fn sorted_keys(&self) -> Vec<&Leaf> {
        let mut out: Vec<&Leaf> = self.leaves.iter().collect();
        out.sort_by(|a, b| a.key.cmp(&b.key));
        out
    }

    fn insert(&mut self, leaf: Leaf) {
        // A later file overrides an earlier one, matching `reduce(:merge!)`.
        if let Some(&i) = self.index.get(&leaf.key) {
            self.leaves[i] = leaf;
            return;
        }
        self.index.insert(leaf.key.clone(), self.leaves.len());
        self.leaves.push(leaf);
    }

    /// The immediate child segment names of `key`.
    pub fn children(&self, key: &str) -> &[String] {
        self.children.get(key).map_or(&[][..], Vec::as_slice)
    }

    /// ref: lib/i18n/tasks/plural_keys.rb#plural_forms?
    pub fn is_plural_node(&self, key: &str) -> bool {
        self.plural_nodes.contains(key)
    }

    /// Every plural node, sorted. ref: plural_keys.rb#plural_nodes
    pub fn sorted_plural_nodes(&self) -> Vec<&str> {
        let mut out: Vec<&str> = self.plural_nodes.iter().map(String::as_str).collect();
        out.sort_unstable();
        out
    }

    fn finish(&mut self) {
        // One pass to record ancestors and immediate children. Doing this once
        // keeps `depluralize_key` constant time instead of a scan per key.
        //
        // `recorded` holds every node path whose parent-child pair is already
        // in `children`, so a repeated ancestor costs one hash lookup instead
        // of a scan over the siblings recorded before it — a parent with a few
        // thousand children is an ordinary locale file, and the scan made this
        // quadratic. It also ends the walk: a path that is already recorded had
        // its whole ancestor chain recorded by the leaf that first reached it,
        // so there is nothing above it left to do.
        let mut recorded: HashSet<&str> = HashSet::new();
        for leaf in &self.leaves {
            let mut key = leaf.key.as_str();
            while let Some(parent) = crate::keys::parent_key(key) {
                #[cfg(test)]
                note_siblings_examined(1);
                if !recorded.insert(key) {
                    break;
                }
                self.interior.insert(parent.to_string());
                let seg = &key[parent.len() + 1..];
                self.children
                    .entry(parent.to_string())
                    .or_default()
                    .push(seg.to_string());
                key = parent;
            }
        }
        for (parent, children) in &self.children {
            let all_plural_leaves = !children.is_empty()
                && children.iter().all(|c| {
                    crate::plural::plural_suffix(c) && {
                        let full = format!("{parent}.{c}");
                        self.index.contains_key(&full) && !self.interior.contains(&full)
                    }
                });
            if all_plural_leaves {
                self.plural_nodes.insert(parent.clone());
            }
        }
    }
}

#[derive(Debug)]
pub struct Store {
    pub base_locale: String,
    /// Base locale first, then the rest sorted. ref: lib/i18n/tasks/locale_list.rb
    pub locales: Vec<String>,
    pub trees: HashMap<String, LocaleTree>,
    /// External data is never unused and never missing.
    /// ref: lib/i18n/tasks/data.rb#external_key?
    pub external: HashMap<String, LocaleTree>,
    pub warnings: Vec<String>,
}

impl Store {
    pub fn tree(&self, locale: &str) -> Option<&LocaleTree> {
        self.trees.get(locale)
    }

    pub fn external_has(&self, locale: &str, key: &str) -> bool {
        self.external.get(locale).is_some_and(|t| t.has_key(key))
    }

    /// ref: lib/i18n/tasks/data.rb#key_value?
    pub fn key_value(&self, locale: &str, key: &str) -> bool {
        self.trees.get(locale).is_some_and(|t| t.has_key(key))
    }

    /// # Errors
    ///
    /// No `data.read` pattern matched a file and `locales` is not configured,
    /// or a locale file does not parse — which includes the YAML this port
    /// refuses to read at all, such as an alias or a tag.
    pub fn load(cfg: &Config) -> Result<Store, String> {
        let locales = match &cfg.locales {
            Some(l) => normalize_locale_list(l, &cfg.base_locale),
            None => {
                let found = available_locales(cfg);
                if found.is_empty() {
                    return Err(format!(
                        "no locale data found. `data.read` patterns: {:?}, relative to {}",
                        cfg.data.read,
                        cfg.root.display()
                    ));
                }
                normalize_locale_list(&found, &cfg.base_locale)
            }
        };

        let (trees, mut warnings) = read_all(cfg, &locales, &cfg.data.read)?;
        let (external, external_warnings) = read_all(cfg, &locales, &cfg.data.external)?;
        warnings.extend(external_warnings);
        Ok(Store {
            base_locale: cfg.base_locale.clone(),
            locales,
            trees,
            external,
            warnings,
        })
    }
}

/// One tree per locale, plus the warnings the reads produced.
///
/// The locales share nothing, so they are read in parallel. Determinism is the
/// constraint — `tests/jobs.rs` asserts every command is byte-identical at
/// `--jobs 1`, `2`, `8`, `16` and the default — so each locale collects its own
/// warnings and the results are folded back in locale order. That ordering also
/// decides which of several broken locales names the error: the first one, as
/// the serial loop gave.
fn read_all(
    cfg: &Config,
    locales: &[String],
    patterns: &[String],
) -> Result<(HashMap<String, LocaleTree>, Vec<String>), String> {
    let per_locale: Vec<Result<(LocaleTree, Vec<String>), String>> = locales
        .par_iter()
        .map(|locale| {
            let mut warnings = Vec::new();
            read_locale(cfg, locale, patterns, &mut warnings).map(|tree| (tree, warnings))
        })
        .collect();
    let mut trees = HashMap::with_capacity(locales.len());
    let mut warnings = Vec::new();
    for (locale, result) in locales.iter().zip(per_locale) {
        let (tree, locale_warnings) = result?;
        trees.insert(locale.clone(), tree);
        warnings.extend(locale_warnings);
    }
    Ok((trees, warnings))
}

/// ref: lib/i18n/tasks/locale_list.rb#normalize_locale_list
fn normalize_locale_list(locales: &[String], base: &str) -> Vec<String> {
    let mut sorted: Vec<String> = locales.to_vec();
    sorted.sort();
    let mut out = vec![base.to_string()];
    out.extend(sorted.into_iter().filter(|l| l != base));
    out
}

/// ref: lib/i18n/tasks/data/file_system_base.rb#available_locales
fn available_locales(cfg: &Config) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for pattern in &cfg.data.read {
        if !pattern.contains("%{locale}") {
            continue;
        }
        let Some(re) = locale_pattern_re(pattern) else {
            continue;
        };
        for path in glob_paths(&cfg.root, &interpolate_locale(pattern, "*")) {
            let rel = path
                .strip_prefix(&cfg.root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            if let Some(locale) = extract_locale(&re, &rel)
                && !found.contains(&locale)
            {
                found.push(locale);
            }
        }
    }
    found
}

/// The read pattern as an anchored regex whose one group is the locale.
///
/// ref: file_system_base.rb:122-124. The glob is deliberately more permissive
/// than this regex: `config/locales/%{locale}.yml` globs to
/// `config/locales/*.yml`, which matches `other.fr.yml`, but `%{locale}`
/// becomes `([^/.]+)`, which a dotted name cannot satisfy. So that file names
/// no locale. Only `.`, `/` and `\` are escaped, exactly as the gem escapes
/// them; a read pattern holding another regex metacharacter fails to compile
/// and is skipped, where the gem would raise.
fn locale_pattern_re(pattern: &str) -> Option<Regex> {
    let mut out = String::with_capacity(pattern.len() * 2);
    out.push_str("\\A");
    let bytes = pattern.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if pattern[i..].starts_with("%{locale}") {
            out.push_str("([^/.]+)");
            i += "%{locale}".len();
            continue;
        }
        if bytes[i] == b'*' {
            let start = i;
            while i < bytes.len() && bytes[i] == b'*' {
                i += 1;
            }
            // Exactly `**` crosses directories, any other run does not.
            out.push_str(if i - start == 2 { ".*" } else { "[^/]*?" });
            continue;
        }
        // See `interpolate_locale`: the `else` arm restates the loop condition.
        let Some(ch) = pattern[i..].chars().next() else {
            break;
        };
        match ch {
            '.' | '/' | '\\' => {
                out.push('\\');
                out.push(ch);
            }
            _ => out.push(ch),
        }
        i += ch.len_utf8();
    }
    out.push_str("\\z");
    Regex::new(&out).ok()
}

/// The locale a concrete path names under a `data.read` pattern, or `None`
/// when the pattern does not read that path at all.
///
/// Exposed so `init-config` can check the patterns it generates with the rule
/// the loader will apply to them, rather than with a second implementation of
/// it. `path` is project-relative and slash-separated.
pub fn locale_for_path(pattern: &str, path: &str) -> Option<String> {
    extract_locale(&locale_pattern_re(pattern)?, path)
}

/// Reads the locale back out of a concrete path.
fn extract_locale(re: &Regex, path: &str) -> Option<String> {
    Some(re.captures(path)?.get(1)?.as_str().to_string())
}

/// ref: lib/i18n/tasks/data/file_system_base.rb#read_locale
fn read_locale(
    cfg: &Config,
    locale: &str,
    patterns: &[String],
    warnings: &mut Vec<String>,
) -> Result<LocaleTree, String> {
    #[cfg(test)]
    note_locale_read();
    let mut tree = LocaleTree {
        locale: locale.to_string(),
        ..Default::default()
    };
    let mut seen: HashSet<PathBuf> = HashSet::new();
    for pattern in patterns {
        let concrete = interpolate_locale(pattern, locale);
        for path in glob_paths(&cfg.root, &concrete) {
            // A real-world config has two overlapping `data.read` globs, where the
            // second matches every file the first does. Deduplicate by resolved
            // path so a file is read once, in first-glob order.
            if !seen.insert(path.clone()) {
                continue;
            }
            let src = std::fs::read_to_string(&path)
                .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
            let Some(root) = yaml::parse(&src, &path).map_err(|e| e.to_string())? else {
                continue;
            };
            let shared: Arc<Path> = Arc::from(path.as_path());
            let Some(entries) = root.as_map() else {
                return Err(format!(
                    "{}: expected a mapping at the top level",
                    path.display()
                ));
            };
            tree.file_locales.insert(
                path.clone(),
                entries
                    .iter()
                    .filter_map(|(k, _)| k.as_str().map(str::to_string))
                    .collect(),
            );
            // Each file maps locale to data. Only the locale being read is kept.
            for (k, v) in entries {
                if k.as_str() != Some(locale) {
                    continue;
                }
                flatten(v, &mut Vec::new(), &shared, &path, &mut tree, warnings)?;
            }
        }
    }
    tree.finish();
    Ok(tree)
}

fn flatten(
    node: &Node,
    prefix: &mut Vec<String>,
    file: &Arc<Path>,
    path: &Path,
    out: &mut LocaleTree,
    warnings: &mut Vec<String>,
) -> Result<(), String> {
    match node {
        Node::Map { entries, .. } => {
            for (k, v) in entries {
                // ref: file_system_base.rb#filter_nil_keys!
                if yaml::is_null_scalar(k) {
                    warnings.push(format!(
                        "{}:{}: skipping a nil key under `{}`. The unquoted YAML keys \
                         null, Null, NULL and ~ all produce a nil key, which i18n does not \
                         support.",
                        path.display(),
                        k.line(),
                        prefix.join(".")
                    ));
                    continue;
                }
                let Some(name) = k.as_str() else {
                    return Err(format!(
                        "{}:{}: non-scalar YAML key",
                        path.display(),
                        k.line()
                    ));
                };
                prefix.push(name.to_string());
                flatten(v, prefix, file, path, out, warnings)?;
                prefix.pop();
            }
            Ok(())
        }
        _ => {
            if prefix.is_empty() {
                return Ok(());
            }
            out.insert(Leaf {
                key: prefix.join("."),
                value: to_value(node, path, false)?,
                // `flatten` recurses once per level, so the stack gives out
                // long before 65 535 nested maps; saturating keeps the count
                // monotone if it ever did not.
                depth: u16::try_from(prefix.len()).unwrap_or(u16::MAX),
                path: Arc::clone(file),
                odd_segments: prefix.iter().any(|s| s.contains('.')).then(|| {
                    prefix
                        .iter()
                        .map(|s| s.as_str().into())
                        .collect::<Vec<Box<str>>>()
                        .into_boxed_slice()
                }),
            });
            Ok(())
        }
    }
}

/// `in_sequence` marks a scalar that lives inside a YAML sequence. The gem's
/// `reference?` tests a leaf node's own value (`data/tree/node.rb`), and a
/// sequence's leaf value is an Array, never a Symbol. So a Symbol inside a
/// sequence — Rails writes `date.order: [:day, :month, :year]` — is data, not a
/// reference, and must be preserved rather than rejected.
fn to_value(node: &Node, path: &Path, in_sequence: bool) -> Result<Value, String> {
    match node {
        Node::Seq { items, .. } => {
            let mut out = Vec::with_capacity(items.len());
            for i in items {
                out.push(to_value(i, path, true)?);
            }
            Ok(Value::Seq(out))
        }
        // Only reachable under a sequence, because `flatten` walks into every
        // mapping it meets on the way down.
        Node::Map { entries, .. } => {
            let mut out = Vec::with_capacity(entries.len());
            for (k, v) in entries {
                let Some(name) = k.as_str() else {
                    return Err(format!(
                        "{}:{}: non-scalar YAML key",
                        path.display(),
                        k.line()
                    ));
                };
                out.push((name.to_string(), to_value(v, path, in_sequence)?));
            }
            Ok(Value::Map(out))
        }
        Node::Scalar { value, .. } => {
            if !node.is_plain() {
                return Ok(Value::Str(value.clone()));
            }
            // A Symbol inside a sequence keeps its written form, so the emitter
            // can write it back unquoted and Psych still reads a Symbol.
            if in_sequence && is_symbol_reference(value) {
                return Ok(Value::Plain(value.clone()));
            }
            // Blocker B4: Psych turns `:foo.bar` into a Ruby Symbol, which the
            // gem uses as a reference key. The reference subsystem is dropped,
            // so a reference must not pass silently.
            if is_symbol_reference(value) {
                return Err(format!(
                    "{}:{}: `{}` is a reference value. Psych reads it as a Ruby Symbol, \
                     and the reference subsystem is out of scope. Write the value out, \
                     or quote it if a literal string was meant.",
                    path.display(),
                    node.line(),
                    value
                ));
            }
            // One resolver answers this for the emitter too, so a value the
            // loader calls a number is never quoted back into a string.
            Ok(match yaml::resolve_plain(value) {
                Resolved::Null => Value::Nil,
                Resolved::Bool(b) => Value::Bool(b),
                // A number and a date keep the form they were written in, so
                // the file round-trips unchanged and Psych reads the same
                // Integer, Float or Date as before.
                Resolved::Number | Resolved::Timestamp => Value::Plain(value.clone()),
                Resolved::Str => Value::Str(value.clone()),
            })
        }
    }
}

/// Blocker B4: error on any value matching `^:[\w.]+$`.
fn is_symbol_reference(value: &str) -> bool {
    let Some(rest) = value.strip_prefix(':') else {
        return false;
    };
    !rest.is_empty()
        && rest
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
}

/// Expands a glob relative to `root`. Only `*` and `**` are supported, which is
/// all the gem's `Dir.glob` patterns use.
fn glob_paths(root: &Path, pattern: &str) -> Vec<PathBuf> {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A project of its own per test, so one test's leftovers cannot reach
    /// another. The config file itself is never written: `Config::parse` takes
    /// the source, and only the path it names in an error.
    fn project(name: &str, files: &[(String, String)]) -> (PathBuf, Config) {
        let root = std::env::temp_dir().join(format!("i18n-tasks-rs-load-{name}"));
        let _ = std::fs::remove_dir_all(&root);
        for (rel, body) in files {
            let path = root.join(rel);
            std::fs::create_dir_all(path.parent().expect("a file has a parent")).unwrap();
            std::fs::write(path, body).unwrap();
        }
        let src = format!(
            "base_locale: en\nlocales: [{}]\ndata:\n  read:\n    - config/locales/%{{locale}}.yml\n",
            LOCALES.join(", ")
        );
        let cfg =
            Config::parse(&src, &root.join("config/i18n-tasks-rs.yml"), root.clone()).unwrap();
        (root, cfg)
    }

    /// Enough locales that a serial read is plainly different from a parallel
    /// one. `en` is the base, so the load order is `en` and then the rest
    /// sorted.
    const LOCALES: [&str; 4] = ["en", "de", "fr", "it"];

    /// One file per locale, with `%{locale}` in the body replaced by its name.
    fn locale_files(body: &str) -> Vec<(String, String)> {
        LOCALES
            .iter()
            .map(|l| {
                (
                    format!("config/locales/{l}.yml"),
                    body.replace("%{locale}", l),
                )
            })
            .collect()
    }

    /// The locales share nothing, so `Store::load` fans them out over rayon.
    /// A rayon job started from a thread that is not a pool worker runs
    /// entirely inside the pool, so a parallel load reads no locale on the
    /// calling thread, where a serial loop reads every one of them here.
    #[test]
    fn no_locale_is_read_on_the_calling_thread() {
        let (root, cfg) = project("offthread", &locale_files("%{locale}:\n  a: A\n"));
        let before = locales_read_on_this_thread();
        let store = Store::load(&cfg).unwrap();
        assert_eq!(store.locales.len(), LOCALES.len());
        assert_eq!(
            locales_read_on_this_thread() - before,
            0,
            "the locales were read on the calling thread, so the load is serial"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// `warnings` is the one thing every locale writes to, and `--jobs` must
    /// not change a byte of the output, so each locale collects its own and the
    /// lists are joined in locale order.
    #[test]
    fn warnings_stay_in_locale_order() {
        let (root, cfg) = project(
            "warnorder",
            &locale_files("%{locale}:\n  a: A\n  ~: dropped\n"),
        );
        let store = Store::load(&cfg).unwrap();
        let named: Vec<&str> = store
            .warnings
            .iter()
            .map(|w| {
                assert!(w.contains("nil key"), "{w}");
                LOCALES
                    .into_iter()
                    .find(|l| w.contains(&format!("/{l}.yml")))
                    .expect("a warning names the file it comes from")
            })
            .collect();
        assert_eq!(named, store.locales);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Two broken locales must always give the same message: the first one in
    /// locale order, as the serial loop gave.
    #[test]
    fn the_first_broken_locale_in_order_names_the_error() {
        let mut files = locale_files("%{locale}:\n  a: A\n");
        for (path, body) in &mut files {
            if path.ends_with("de.yml") || path.ends_with("it.yml") {
                *body = "- not a mapping\n".to_string();
            }
        }
        let (root, cfg) = project("errorder", &files);
        for _ in 0..20 {
            let err = Store::load(&cfg).unwrap_err();
            assert!(err.contains("de.yml"), "{err}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    fn leaf(key: &str) -> Leaf {
        Leaf {
            key: key.to_string(),
            value: Value::Str("v".into()),
            depth: u16::try_from(key.split('.').count()).expect("shallow key"),
            path: Arc::from(Path::new("config/locales/en.yml")),
            odd_segments: None,
        }
    }

    /// H21: recording a key's ancestors must not cost one comparison per
    /// sibling already recorded. A few thousand keys under one parent is an
    /// ordinary locale file, and a linear scan makes `finish` quadratic — and
    /// `finish` runs inside `Store::load`, so every command pays it.
    #[test]
    fn recording_a_child_does_not_scan_the_children_before_it() {
        const N: usize = 500;
        let mut tree = LocaleTree::default();
        for i in 0..N {
            tree.insert(leaf(&format!("parent.key{i:04}")));
        }
        SIBLINGS_EXAMINED.with(|c| c.set(0));
        tree.finish();
        let examined = SIBLINGS_EXAMINED.with(std::cell::Cell::get);
        // One examination per pair, plus one for `parent` under the root, so
        // the floor is N + 1.
        assert!(
            examined < 4 * N,
            "{examined} sibling comparisons for {N} keys; want O(N), not O(N^2)"
        );
    }

    /// A leaf whose key is also an interior node is what ends the ancestor
    /// walk early: the shorter key is recorded on the way up from the longer
    /// one, whichever of the two comes first. Both directions must record the
    /// same thing.
    #[test]
    fn a_key_that_is_also_an_interior_node_records_its_whole_chain() {
        for order in [["a.b", "a.b.c"], ["a.b.c", "a.b"]] {
            let mut tree = LocaleTree::default();
            for key in order {
                tree.insert(leaf(key));
            }
            tree.finish();
            assert!(tree.is_interior("a"), "{order:?}");
            assert!(tree.is_interior("a.b"), "{order:?}");
            assert_eq!(tree.children("a"), ["b"], "{order:?}");
            assert_eq!(tree.children("a.b"), ["c"], "{order:?}");
        }
    }

    /// `children()` hands out the list in first-seen order, which the plural
    /// rule and the emitter both read. Deduplication must not reorder it.
    #[test]
    fn children_stay_in_first_seen_order() {
        let mut tree = LocaleTree::default();
        for key in ["a.z.one", "a.b", "a.z.two", "a.c", "a.b"] {
            tree.insert(leaf(key));
        }
        tree.finish();
        assert_eq!(tree.children("a"), ["z", "b", "c"]);
        assert_eq!(tree.children("a.z"), ["one", "two"]);
    }

    #[test]
    fn value_to_display_string_matches_ruby_to_s() {
        assert_eq!(Value::Nil.to_display_string(), "");
        assert_eq!(Value::Bool(true).to_display_string(), "true");
        assert_eq!(Value::Plain("1.5".into()).to_display_string(), "1.5");
        assert_eq!(
            Value::Seq(vec![Value::Str("a".into()), Value::Plain("2".into())]).to_display_string(),
            "[\"a\", 2]"
        );
    }

    #[test]
    fn detects_symbol_references() {
        assert!(is_symbol_reference(":other.key"));
        assert!(is_symbol_reference(":other"));
        assert!(!is_symbol_reference(":"));
        assert!(!is_symbol_reference("plain"));
        assert!(!is_symbol_reference(":has spaces"));
    }

    #[test]
    fn locale_list_puts_base_first() {
        assert_eq!(
            normalize_locale_list(&["fr".into(), "de".into(), "en".into()], "de"),
            vec!["de", "en", "fr"]
        );
    }

    /// See `config::interpolate_locale`: the same byte-scan loop, so the same
    /// invariant. A multi-byte directory name must not be cut in half, and it
    /// must not swallow the group either.
    #[test]
    fn a_multi_byte_read_pattern_still_names_the_locale() {
        assert_eq!(
            locale_of("config/переводы/%{locale}.yml", "config/переводы/ru.yml"),
            Some("ru".to_string())
        );
        assert_eq!(
            locale_of("config/переводы/%{locale}.yml", "config/x/ru.yml"),
            None
        );
    }

    fn locale_of(pattern: &str, path: &str) -> Option<String> {
        extract_locale(&locale_pattern_re(pattern).expect("pattern compiles"), path)
    }

    /// ref: spec/file_system_data_spec.rb `#available_locales`. The three read
    /// patterns there see three different sets of the same three files, and the
    /// whole difference is in what the anchored regex accepts.
    #[test]
    fn the_read_pattern_decides_which_files_name_a_locale() {
        let files = [
            "config/locales/en.yml",
            "config/locales/es.yml",
            "config/locales/other.fr.yml",
        ];
        let names = |pattern: &str| -> Vec<String> {
            files.iter().filter_map(|f| locale_of(pattern, f)).collect()
        };
        // "default pattern" -> en, es. `other.fr` holds a dot, and `([^/.]+)`
        // cannot match it, even though the `*.yml` glob reached the file.
        assert_eq!(names("config/locales/%{locale}.yml"), vec!["en", "es"]);
        // "more inclusive pattern" -> en, es, fr.
        assert_eq!(
            names("config/locales/*%{locale}.yml"),
            vec!["en", "es", "fr"]
        );
        // "another pattern" -> fr only.
        assert_eq!(names("config/locales/*.%{locale}.yml"), vec!["fr"]);
    }

    #[test]
    fn extracts_locale_from_path() {
        assert_eq!(
            locale_of(
                "config/locales/base.%{locale}.yml",
                "config/locales/base.de.yml"
            ),
            Some("de".into())
        );
        assert_eq!(
            locale_of(
                "config/locales/*.%{locale}.yml",
                "config/locales/jobs.fr.yml"
            ),
            Some("fr".into())
        );
        assert_eq!(
            locale_of("config/locales/%{locale}.yml", "config/locales/de.yml"),
            Some("de".into())
        );
        // `**` crosses directories, a single `*` does not.
        assert_eq!(
            locale_of(
                "config/locales/**/%{locale}.yml",
                "config/locales/a/b/de.yml"
            ),
            Some("de".into())
        );
        assert_eq!(
            locale_of(
                "config/locales/*/%{locale}.yml",
                "config/locales/a/b/de.yml"
            ),
            None
        );
        // A locale segment inside the path, not in the file name.
        assert_eq!(
            locale_of(
                "config/locales/%{locale}/models.yml",
                "config/locales/de/models.yml"
            ),
            Some("de".into())
        );
    }

    #[test]
    fn extract_locale_rejects_what_cannot_be_a_locale() {
        // The extension has to match.
        assert_eq!(
            locale_of("config/locales/%{locale}.yml", "config/locales/de.json"),
            None
        );
        // The literal part of the pattern has to be there.
        assert_eq!(
            locale_of("config/locales/%{locale}.yml", "other/de.yml"),
            None
        );
        // `+` needs at least one character.
        assert_eq!(
            locale_of("config/locales/%{locale}.yml", "config/locales/.yml"),
            None
        );
        // Nothing may follow the pattern.
        assert_eq!(
            locale_of("config/locales/%{locale}.yml", "config/locales/de.yml.bak"),
            None
        );
    }

    /// A read pattern the gem's escaping cannot express as a regex is skipped
    /// rather than crashing the locale scan.
    #[test]
    fn an_uncompilable_read_pattern_is_skipped() {
        assert!(locale_pattern_re("config/locales/%{locale}.yml").is_some());
        assert!(locale_pattern_re("config/locales/[%{locale}.yml").is_none());
    }

    /// The `Value` display forms feed the `Base value` column of the reports,
    /// so they follow Ruby `to_s`: a String is bare, a collection is inspected.
    #[test]
    fn collection_values_use_the_ruby_inspect_form() {
        assert_eq!(Value::Str("a".into()).to_display_string(), "a");
        assert_eq!(
            Value::Seq(vec![Value::Nil, Value::Bool(false)]).to_display_string(),
            "[nil, false]"
        );
        // Ruby 3.4 writes `{"a" => "b"}`, with spaces around the arrow.
        assert_eq!(
            Value::Map(vec![
                ("a".into(), Value::Str("b".into())),
                ("n".into(), Value::Plain("1".into())),
            ])
            .to_display_string(),
            "{\"a\" => \"b\", \"n\" => 1}"
        );
        // A nested collection inspects all the way down.
        assert_eq!(
            Value::Seq(vec![Value::Map(vec![(
                "k".into(),
                Value::Seq(vec![Value::Str("v".into())])
            )])])
            .to_display_string(),
            "[{\"k\" => [\"v\"]}]"
        );
    }

    #[test]
    fn as_str_is_only_for_scalars() {
        assert_eq!(Value::Str("a".into()).as_str(), Some("a"));
        assert_eq!(Value::Plain("1".into()).as_str(), Some("1"));
        assert_eq!(Value::Nil.as_str(), None);
        assert_eq!(Value::Bool(true).as_str(), None);
        assert_eq!(Value::Seq(Vec::new()).as_str(), None);
        assert_eq!(Value::Map(Vec::new()).as_str(), None);
    }

    /// ref: locale_list.rb#normalize_locale_list, called with `add_base = true`,
    /// so the base locale is prepended whether or not the list held it.
    #[test]
    fn locale_list_keeps_the_base_first_even_when_absent_from_the_list() {
        assert_eq!(
            normalize_locale_list(&["fr".into(), "de".into()], "en"),
            vec!["en", "de", "fr"]
        );
        // Duplicates collapse.
        assert_eq!(
            normalize_locale_list(&["en".into(), "en".into(), "de".into()], "en"),
            vec!["en", "de"]
        );
    }
}
