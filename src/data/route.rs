//! Routers: they decide which file each key is written to.
//!
//! ref: lib/i18n/tasks/data/router/pattern_router.rb
//! ref: lib/i18n/tasks/data/router/conservative_router.rb
//! ref: lib/i18n/tasks/locale_pathname.rb#replace_locale

use crate::config::{Config, Router, interpolate_locale};
use crate::data::load::Store;
use crate::pattern::Pattern;
use std::collections::HashMap;
use std::path::PathBuf;

/// One destination file and the keys that belong in it.
#[derive(Debug)]
pub struct Destination {
    pub path: PathBuf,
    /// Dotted keys, without the locale.
    pub keys: Vec<String>,
}

/// The compiled `data.write` rules.
///
/// A bare path entry is the same as `['*', path]`, which is what
/// `compile_routes` does.
#[derive(Debug)]
pub struct PatternRouter {
    routes: Vec<(Pattern, String)>,
    /// For the error message on an unroutable key.
    sources: Vec<String>,
}

impl PatternRouter {
    pub fn new(cfg: &Config) -> PatternRouter {
        PatternRouter {
            routes: cfg
                .data
                .write
                .iter()
                .map(|r| {
                    (
                        Pattern::compile(r.pattern.as_deref().unwrap_or("*")),
                        r.path.clone(),
                    )
                })
                .collect(),
            sources: cfg
                .data
                .write
                .iter()
                .map(|r| match &r.pattern {
                    Some(p) => format!("[{p:?}, {:?}]", r.path),
                    None => format!("{:?}", r.path),
                })
                .collect(),
        }
    }

    /// The destination for one key, with `%{locale}` and `\1`..`\9` filled in.
    ///
    /// The gem substitutes the locale first and the captures second, so a
    /// capture that holds a `%{locale}` is not expanded twice. Same here.
    ///
    /// # Errors
    ///
    /// No `data.write` rule matches the key. The message lists the rules.
    pub fn route_key(&self, locale: &str, key: &str) -> Result<PathBuf, String> {
        for (pattern, path) in &self.routes {
            let Some(caps) = pattern.captures(key) else {
                continue;
            };
            let path = interpolate_locale(path, locale);
            return Ok(PathBuf::from(substitute_captures(&path, key, &caps)));
        }
        Err(format!(
            "cannot route key `{key}`. `data.write` rules are [{}]",
            self.sources.join(", ")
        ))
    }
}

/// Replaces `\1`..`\9` with the matching `{...}` group of the key pattern.
///
/// ref: `path.gsub!(/\\\d+/) { |m| key_match[m[1..].to_i] }`
fn substitute_captures(path: &str, key: &str, caps: &crate::pattern::Captures) -> String {
    let bytes = path.as_bytes();
    let mut out = String::with_capacity(path.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\'
            && let Some(d) = bytes.get(i + 1).filter(|b| b.is_ascii_digit())
        {
            let n = (d - b'0') as usize;
            // The gem's group numbers are one-based; ours are zero-based.
            if let Some(Some((s, e))) = n.checked_sub(1).map(|idx| caps.get(idx).copied().flatten())
            {
                out.push_str(&key[s..e]);
            }
            i += 2;
            continue;
        }
        // See `config::interpolate_locale`: the `else` arm restates the loop
        // condition.
        let Some(ch) = path[i..].chars().next() else {
            break;
        };
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Rewrites the locale part of a path.
///
/// ref: `%r{(?<=^|[/.-])#{locale}(?=[/.])}`, applied globally. So
/// `config/locales/base.de.yml` with `de` → `en` becomes
/// `config/locales/base.en.yml`.
pub fn replace_locale(path: &str, from: &str, to: &str) -> String {
    if from.is_empty() {
        return path.to_string();
    }
    let bytes = path.as_bytes();
    let mut out = String::with_capacity(path.len());
    let mut i = 0;
    while i < bytes.len() {
        let before_ok = i == 0 || matches!(bytes[i - 1], b'/' | b'.' | b'-');
        let after = bytes.get(i + from.len());
        let after_ok = matches!(after, Some(b'/' | b'.'));
        if before_ok && after_ok && path[i..].starts_with(from) {
            out.push_str(to);
            i += from.len();
            continue;
        }
        // See `config::interpolate_locale`.
        let Some(ch) = path[i..].chars().next() else {
            break;
        };
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// The conservative router: a key stays in the file it came from.
///
/// ref: lib/i18n/tasks/data/router/conservative_router.rb
///
/// A genuinely new key falls through to `data.write`, so the router borrows a
/// compiled `PatternRouter` rather than compiling its own — one set of rules
/// per command, whichever router is in charge.
#[derive(Debug)]
pub struct ConservativeRouter<'a> {
    store: &'a Store,
    fallback: &'a PatternRouter,
}

impl<'a> ConservativeRouter<'a> {
    pub fn new(fallback: &'a PatternRouter, store: &'a Store) -> ConservativeRouter<'a> {
        ConservativeRouter { store, fallback }
    }

    /// # Errors
    ///
    /// A key the store has never seen falls through to the pattern router, and
    /// fails when no `data.write` rule matches it.
    pub fn route_key(&self, locale: &str, key: &str) -> Result<PathBuf, String> {
        if let Some(leaf) = self.store.tree(locale).and_then(|t| t.get(key)) {
            return Ok(leaf.path.to_path_buf());
        }
        // Not in this locale yet: take the file another locale keeps it in and
        // rewrite the locale part of that path.
        for other in self.store.locales.iter().filter(|l| *l != locale) {
            if let Some(leaf) = self.store.tree(other).and_then(|t| t.get(key)) {
                let rewritten = replace_locale(&leaf.path.to_string_lossy(), other, locale);
                return Ok(PathBuf::from(rewritten));
            }
        }
        // A genuinely new key falls through to the pattern router.
        self.fallback.route_key(locale, key)
    }
}

/// Groups every key of one locale by the file it is written to.
///
/// `force_pattern` is the `-p` / `--pattern-router` flag, which makes keys
/// physically move to where `data.write` says they belong.
///
/// # Errors
///
/// The locale has no data, or a key cannot be routed.
pub fn route(
    cfg: &Config,
    store: &Store,
    locale: &str,
    force_pattern: bool,
) -> Result<Vec<Destination>, String> {
    route_filtered(cfg, store, locale, force_pattern, &|_| true)
}

/// Groups the selected keys of one locale by destination file.
///
/// # Errors
///
/// The locale has no data, or a selected key cannot be routed.
pub fn route_filtered(
    cfg: &Config,
    store: &Store,
    locale: &str,
    force_pattern: bool,
    keep: &impl Fn(&str) -> bool,
) -> Result<Vec<Destination>, String> {
    let pattern = PatternRouter::new(cfg);
    let conservative = if force_pattern || cfg.data.router == Router::Pattern {
        None
    } else {
        Some(ConservativeRouter::new(&pattern, store))
    };
    let tree = store
        .tree(locale)
        .ok_or_else(|| format!("locale `{locale}` has no data"))?;

    let mut groups: HashMap<PathBuf, Vec<String>> = HashMap::new();
    for leaf in &tree.leaves {
        if !keep(&leaf.key) {
            continue;
        }
        let path = match &conservative {
            Some(r) => r.route_key(locale, &leaf.key)?,
            None => pattern.route_key(locale, &leaf.key)?,
        };
        let path = if path.is_absolute() {
            path
        } else {
            cfg.root.join(path)
        };
        groups.entry(path).or_default().push(leaf.key.clone());
    }

    // Sorted, so neither the report nor the write order depends on hashing.
    let mut out: Vec<Destination> = groups
        .into_iter()
        .map(|(path, keys)| Destination { path, keys })
        .collect();
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

/// The files that hold this locale today, so `normalize` can tell which ones
/// end up empty and are due for deletion.
///
/// ref: `paths_before` in `FileSystemBase#set`.
pub fn origin_paths(store: &Store, locale: &str) -> Vec<PathBuf> {
    let Some(tree) = store.tree(locale) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = tree
        .leaves
        .iter()
        .map(|l| l.path.to_path_buf())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn replaces_the_locale_part_of_a_path() {
        assert_eq!(
            replace_locale("config/locales/base.de.yml", "de", "en"),
            "config/locales/base.en.yml"
        );
        assert_eq!(
            replace_locale("config/locales/de/models.yml", "de", "fr"),
            "config/locales/fr/models.yml"
        );
        // `de` inside a word is left alone, because of the lookbehind.
        assert_eq!(
            replace_locale("config/locales/decor.de.yml", "de", "en"),
            "config/locales/decor.en.yml"
        );
        // A locale-less path stays as it is.
        assert_eq!(
            replace_locale("config/locales/all.yml", "de", "en"),
            "config/locales/all.yml"
        );
    }

    /// See `config::interpolate_locale`: two of the byte-scan loops are here.
    #[test]
    fn a_multi_byte_path_survives_both_rewrites() {
        assert_eq!(
            replace_locale("config/переводы/de/a.yml", "de", "en"),
            "config/переводы/en/a.yml"
        );
        let pattern = Pattern::compile("{ü,x}.*");
        let caps = pattern.captures("ü.key").expect("the key matches");
        assert_eq!(
            substitute_captures("config/переводы/\\1.yml", "ü.key", &caps),
            "config/переводы/ü.yml"
        );
    }

    /// Full port of spec/locale_pathname_spec.rb.
    #[test]
    fn replace_locale_matches_the_gem_spec() {
        assert_eq!(replace_locale("es.yml", "es", "fr"), "fr.yml");
        assert_eq!(replace_locale("scope.es.yml", "es", "fr"), "scope.fr.yml");
        assert_eq!(replace_locale("path/es.yml", "es", "fr"), "path/fr.yml");
        assert_eq!(
            replace_locale("path/scope.es.yml", "es", "fr"),
            "path/scope.fr.yml"
        );
    }

    #[test]
    fn replace_locale_needs_a_boundary_on_both_sides() {
        // A `-` also opens a locale, per the lookbehind `(?<=^|[/.-])`.
        assert_eq!(replace_locale("a-de.yml", "de", "en"), "a-en.yml");
        // The lookahead is `[/.]`, so a locale at the very end is left alone.
        assert_eq!(replace_locale("locales/de", "de", "en"), "locales/de");
        // A locale that is only a prefix of the segment does not match.
        assert_eq!(replace_locale("de_CH/a.yml", "de", "en"), "de_CH/a.yml");
        // Every occurrence is replaced, not just the first.
        assert_eq!(replace_locale("de/de.yml", "de", "en"), "en/en.yml");
        // An empty locale would match everywhere, so it matches nowhere.
        assert_eq!(replace_locale("a/b.yml", "", "en"), "a/b.yml");
    }

    #[test]
    fn origin_paths_of_an_unknown_locale_are_empty() {
        let cfg = alternation_router();
        let store = Store {
            base_locale: cfg.base_locale.clone(),
            locales: Vec::new(),
            trees: HashMap::default(),
            external: HashMap::default(),
            warnings: Vec::new(),
        };
        assert!(origin_paths(&store, "de").is_empty());
    }

    #[test]
    fn substitutes_numbered_captures() {
        let p = Pattern::compile("{activerecord, views}.*");
        let key = "views.home.title";
        let caps = p.captures(key).unwrap();
        assert_eq!(
            substitute_captures("config/locales/\\1.de.yml", key, &caps),
            "config/locales/views.de.yml"
        );
    }

    /// A real-world `data.write`: a wide alternation feeding `\1`.
    fn alternation_router() -> Config {
        Config::parse(
            "base_locale: de\n\
             locales: [de, en, fr]\n\
             data:\n\
             \x20 write:\n\
             \x20   - [\"{about, activerecord, views}.*\", 'config/locales/\\1.%{locale}.yml']\n\
             \x20   - config/locales/base.%{locale}.yml\n",
            Path::new("config/i18n-tasks.yml"),
            PathBuf::from("."),
        )
        .unwrap()
    }

    #[test]
    fn pattern_router_routes_by_key() {
        let cfg = alternation_router();
        let r = PatternRouter::new(&cfg);
        assert_eq!(
            r.route_key("de", "activerecord.models.user").unwrap(),
            PathBuf::from("config/locales/activerecord.de.yml")
        );
        assert_eq!(
            r.route_key("fr", "about.team.title").unwrap(),
            PathBuf::from("config/locales/about.fr.yml")
        );
    }

    #[test]
    fn pattern_router_falls_through_to_the_catch_all() {
        let cfg = alternation_router();
        let r = PatternRouter::new(&cfg);
        assert_eq!(
            r.route_key("de", "some.other.key").unwrap(),
            PathBuf::from("config/locales/base.de.yml")
        );
    }

    /// The conservative router falls back to the pattern router, so `route`
    /// must build the compiled `data.write` rules once and share them.
    #[test]
    fn route_compiles_the_write_patterns_once() {
        let cfg = alternation_router(); // two `data.write` rules
        let mut tree = crate::data::load::LocaleTree::default();
        tree.leaves.push(crate::data::load::Leaf {
            key: "brand.new".to_string(),
            value: crate::data::load::Value::Str("x".to_string()),
            depth: 2,
            path: std::sync::Arc::from(Path::new("config/locales/base.de.yml")),
            odd_segments: None,
        });
        let store = Store {
            base_locale: cfg.base_locale.clone(),
            locales: vec!["de".to_string()],
            trees: HashMap::from([("de".to_string(), tree)]),
            external: HashMap::default(),
            warnings: Vec::new(),
        };

        let before = crate::pattern::compiles_on_this_thread();
        route(&cfg, &store, "de", false).unwrap();
        assert_eq!(crate::pattern::compiles_on_this_thread() - before, 2);
    }

    #[test]
    fn pattern_router_reports_an_unroutable_key() {
        let cfg = Config::parse(
            "data:\n  write:\n    - [\"only.*\", 'config/locales/only.%{locale}.yml']\n",
            Path::new("config/i18n-tasks.yml"),
            PathBuf::from("."),
        )
        .unwrap();
        let e = PatternRouter::new(&cfg)
            .route_key("en", "elsewhere.key")
            .unwrap_err();
        assert!(e.contains("cannot route key `elsewhere.key`"), "{e}");
    }
}
