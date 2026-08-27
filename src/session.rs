//! The state every project-reading command shares, and the locale list it runs
//! over.
//!
//! This is the CLI's work, but not the binary's: `src/cli/` keeps the clap
//! structs, the exit codes and the four printing macros, and everything a test
//! would otherwise have to spawn a process to reach lives here.

use crate::config::Config;
use crate::data::load::Store;
use crate::used::UsedKeys;
use std::path::Path;

/// What `Session::open` needs, named rather than positional.
///
/// The two paths and the flag are all transposable without a compile error —
/// the same reason `NormalizeFlags` is a struct — so the caller names each one.
#[derive(Debug)]
pub struct SessionOptions<'a> {
    /// The config file to read.
    pub config: &'a Path,
    /// Directory every config path is relative to. `None` means the working
    /// directory, which is what the gem uses.
    pub root: Option<&'a Path>,
    /// The locales asked for, before `all`, `base` and validation.
    pub locales: Vec<String>,
    /// `-f json`.
    pub json: bool,
}

/// Everything the commands share, loaded once.
#[derive(Debug)]
pub struct Session {
    pub cfg: Config,
    pub store: Store,
    pub locales: Vec<String>,
    pub json: bool,
}

impl Session {
    /// Reads the config, reads the locale data, then resolves the locale list.
    ///
    /// `warn` takes each load warning as it is produced rather than the caller
    /// reading `store.warnings` afterwards, because the locale list is resolved
    /// last: a run that asks for an unknown locale still reports the warnings
    /// the load produced before it names the error.
    ///
    /// The rayon pool is *not* installed here. It is a process-global side
    /// effect, so it belongs to the binary, and `--jobs` is applied before this.
    ///
    /// # Errors
    ///
    /// The config does not parse, the locale data does not load, or a requested
    /// locale is invalid or is not configured.
    pub fn open(opts: &SessionOptions<'_>, mut warn: impl FnMut(&str)) -> Result<Session, String> {
        let cfg = Config::load(opts.config, opts.root)?;
        let store = Store::load(&cfg)?;
        for warning in &store.warnings {
            warn(warning);
        }
        let locales = resolve_locales(&opts.locales, &store)?;
        Ok(Session {
            cfg,
            store,
            locales,
            json: opts.json,
        })
    }

    /// # Errors
    ///
    /// A search path does not exist, or a key pattern does not compile.
    pub fn scan(&self) -> Result<UsedKeys, String> {
        UsedKeys::scan(&self.cfg)
    }
}

/// ref: lib/i18n/tasks/command/option_parsers/locale.rb#ListParser
///
/// An empty list, or a lone `all`, means every configured locale. Otherwise
/// `base` stands in for the base locale, and if the base locale lands anywhere
/// but first it is swapped to the front, so a report always starts there.
///
/// # Errors
///
/// A requested locale is not a valid locale name, or is not configured.
pub fn resolve_locales(requested: &[String], store: &Store) -> Result<Vec<String>, String> {
    if requested.is_empty() || requested == ["all"] {
        return Ok(store.locales.clone());
    }
    let mut locales: Vec<String> = requested
        .iter()
        .map(|l| {
            if l == "base" {
                store.base_locale.clone()
            } else {
                l.clone()
            }
        })
        .collect();
    // ref: ListParser#move_base_locale_to_front!. A swap, not a rotation: the
    // locale that held the front takes the base locale's old slot.
    if let Some(pos) = locales
        .iter()
        .position(|l| *l == store.base_locale)
        .filter(|p| *p > 0)
    {
        locales.swap(0, pos);
    }
    // The gem merges per-locale forests, so repeated locale roots coalesce.
    let mut seen = std::collections::HashSet::new();
    locales.retain(|locale| seen.insert(locale.clone()));
    for l in &locales {
        // ref: Locale::Validator::VALID_LOCALE_RE. Reported before the
        // membership check so a malformed name like `-l -de` names the real
        // problem rather than being listed as unconfigured.
        if !valid_locale(l) {
            return Err(format!("invalid locale `{l}`"));
        }
        if !store.locales.contains(l) {
            return Err(format!(
                "unknown locale `{l}`. Configured locales: {}",
                store.locales.join(", ")
            ));
        }
    }
    Ok(locales)
}

/// ref: `/\A\w[\w\-.]*\z/i`. Ruby's `\w` here is ASCII-only.
#[must_use]
pub fn valid_locale(locale: &str) -> bool {
    let mut chars = locale.chars();
    let first_ok = matches!(chars.next(), Some(c) if c.is_ascii_alphanumeric() || c == '_');
    first_ok && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

#[cfg(test)]
mod tests {
    use super::{resolve_locales, valid_locale};
    use crate::data::load::Store;
    use std::collections::HashMap;

    /// A store with locales and nothing in them. `resolve_locales` reads
    /// `base_locale` and `locales` and nothing else.
    fn store(base: &str, locales: &[&str]) -> Store {
        Store {
            base_locale: base.to_string(),
            locales: locales.iter().map(|l| (*l).to_string()).collect(),
            trees: HashMap::new(),
            external: HashMap::new(),
            warnings: Vec::new(),
        }
    }

    fn resolve(requested: &[&str], store: &Store) -> Result<Vec<String>, String> {
        let requested: Vec<String> = requested.iter().map(|l| (*l).to_string()).collect();
        resolve_locales(&requested, store)
    }

    #[test]
    fn no_locales_asked_for_means_every_configured_one() {
        let s = store("en", &["en", "de", "fr"]);
        assert_eq!(resolve(&[], &s), Ok(vec_of(&["en", "de", "fr"])));
        assert_eq!(resolve(&["all"], &s), Ok(vec_of(&["en", "de", "fr"])));
    }

    #[test]
    fn base_stands_in_for_the_base_locale() {
        let s = store("en", &["en", "de"]);
        assert_eq!(resolve(&["base"], &s), Ok(vec_of(&["en"])));
    }

    /// ref: ListParser#move_base_locale_to_front!. A swap, not a rotation.
    #[test]
    fn the_base_locale_is_swapped_to_the_front() {
        let s = store("en", &["en", "de", "fr"]);
        assert_eq!(
            resolve(&["de", "fr", "en"], &s),
            Ok(vec_of(&["en", "fr", "de"]))
        );
    }

    #[test]
    fn duplicate_locales_coalesce_like_merged_gem_forests() {
        let s = store("en", &["en", "de"]);
        assert_eq!(
            resolve(&["de", "en", "de", "base"], &s),
            Ok(vec_of(&["en", "de"]))
        );
    }

    #[test]
    fn an_invalid_locale_is_named_before_an_unknown_one() {
        let s = store("en", &["en"]);
        let err = resolve(&["-de"], &s).expect_err("`-de` is not a valid locale");
        assert!(err.contains("invalid locale `-de`"), "{err}");
    }

    #[test]
    fn an_unknown_locale_lists_the_configured_ones() {
        let s = store("en", &["en", "de"]);
        let err = resolve(&["zz"], &s).expect_err("`zz` is not configured");
        assert!(err.contains("unknown locale `zz`"), "{err}");
        assert!(err.contains("en, de"), "{err}");
    }

    /// ref: `/\A\w[\w\-.]*\z/i`, where Ruby's `\w` is ASCII-only.
    #[test]
    fn valid_locale_is_the_gems_regex() {
        for ok in ["en", "en-GB", "zh_Hans", "en.x", "_x", "9"] {
            assert!(valid_locale(ok), "{ok} is valid in the gem");
        }
        for bad in ["", "-en", ".en", "en/de", "en de", "ü"] {
            assert!(!valid_locale(bad), "{bad} is invalid in the gem");
        }
    }

    fn vec_of(items: &[&str]) -> Vec<String> {
        items.iter().map(|l| (*l).to_string()).collect()
    }
}
