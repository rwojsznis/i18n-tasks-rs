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
        let ch = path[i..].chars().next().unwrap();
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
        let after_ok = matches!(after, Some(b'/') | Some(b'.'));
        if before_ok && after_ok && path[i..].starts_with(from) {
            out.push_str(to);
            i += from.len();
            continue;
        }
        let ch = path[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// The conservative router: a key stays in the file it came from.
///
/// ref: lib/i18n/tasks/data/router/conservative_router.rb
pub struct ConservativeRouter<'a> {
    store: &'a Store,
    fallback: PatternRouter,
}

impl<'a> ConservativeRouter<'a> {
    pub fn new(cfg: &Config, store: &'a Store) -> ConservativeRouter<'a> {
        ConservativeRouter {
            store,
            fallback: PatternRouter::new(cfg),
        }
    }

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
pub fn route(
    cfg: &Config,
    store: &Store,
    locale: &str,
    force_pattern: bool,
) -> Result<Vec<Destination>, String> {
    let conservative = if force_pattern || cfg.data.router == Router::Pattern {
        None
    } else {
        Some(ConservativeRouter::new(cfg, store))
    };
    let pattern = PatternRouter::new(cfg);
    let tree = store
        .tree(locale)
        .ok_or_else(|| format!("locale `{locale}` has no data"))?;

    let mut groups: HashMap<PathBuf, Vec<String>> = HashMap::new();
    for leaf in &tree.leaves {
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
            trees: Default::default(),
            external: Default::default(),
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
