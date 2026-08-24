//! Plural keys and the CLDR category table.
//!
//! ref: lib/i18n/tasks/plural_keys.rb
//!
//! Blocker B7: the gem reads
//! `<rails-i18n gem>/rails/pluralization/<locale>.rb` and `eval`s it, because
//! those files hold lambdas (`missing_keys.rb:90-93`). The table below is the
//! same data, extracted from rails-i18n 8.0.2, as static text.

use crate::data::load::LocaleTree;
use crate::keys::{last_key_part, parent_key};

/// ref: plural_keys.rb#CLDR_CATEGORY_KEYS
pub const CLDR_CATEGORY_KEYS: &[&str] = &["zero", "one", "two", "few", "many", "other"];

/// ref: plural_keys.rb#EXPLICIT_0_1
pub const EXPLICIT_0_1: &[&str] = &["0", "1"];

/// ref: plural_keys.rb#plural_suffix?
pub fn plural_suffix(key: &str) -> bool {
    CLDR_CATEGORY_KEYS.contains(&key) || EXPLICIT_0_1.contains(&key)
}

/// The category set every plural node in `locale` must provide.
///
/// An unknown locale returns `None`, which is how the gem behaves when
/// rails-i18n ships no pluralization file for it: `plural_keys_for_locale`
/// rescues `SystemCallError` and returns an empty set, so no plural is reported.
pub fn required_categories(locale: &str) -> Option<&'static [&'static str]> {
    const ONE_OTHER: &[&str] = &["one", "other"];
    const OTHER: &[&str] = &["other"];
    const ONE_FEW_OTHER: &[&str] = &["one", "few", "other"];
    // Not currently used by any rails-i18n locale, kept so the shape is honest.
    #[allow(dead_code)]
    const ONE_TWO_OTHER: &[&str] = &["one", "two", "other"];
    const ONE_TWO_FEW_OTHER: &[&str] = &["one", "two", "few", "other"];
    const ONE_FEW_MANY_OTHER: &[&str] = &["one", "few", "many", "other"];
    const ALL_SIX: &[&str] = &["zero", "one", "two", "few", "many", "other"];

    let table: &[(&[&str], &[&str])] = &[
        // OneOther, OneUptoTwoOther, OneWithZeroOther, Latvian, Macedonian
        (
            &[
                "bg", "bn", "ca", "da", "de", "de-AT", "de-CH", "de-DE", "el", "el-CY", "en",
                "en-AU", "en-CA", "en-CY", "en-GB", "en-IE", "en-IN", "en-NZ", "en-TT", "en-US",
                "en-ZA", "eo", "es", "es-419", "es-AR", "es-CL", "es-CO", "es-CR", "es-EC",
                "es-ES", "es-MX", "es-NI", "es-PA", "es-PE", "es-US", "es-VE", "et", "eu", "fi",
                "fy", "gl", "he", "hu", "is", "it", "it-CH", "ka", "kk", "mn", "nb", "ne", "nl",
                "nn", "oc", "pt", "pt-BR", "sc", "st", "sv", "sv-SE", "sw", "ta", "tr", "ur", "fr",
                "fr-CA", "fr-CH", "fr-FR", "hi", "hi-IN", "mg", "ml", "mr-IN", "or", "pa", "tl",
                "lv", "mk",
            ],
            ONE_OTHER,
        ),
        // Other
        (
            &[
                "az", "fa", "id", "ja", "km", "kn", "ko", "lo", "ms", "pap-AW", "pap-CW", "th",
                "vi", "wo", "zh-CN", "zh-HK", "zh-TW", "zh-YUE",
            ],
            OTHER,
        ),
        // OneFewOther, WestSlavic, Romanian, Lithuanian
        (&["bs", "hr", "sr", "cs", "sk", "ro", "lt"], ONE_FEW_OTHER),
        // EastSlavic, Polish
        (&["be", "ru", "uk", "pl"], ONE_FEW_MANY_OTHER),
        // ScottishGaelic, UpperSorbian, Slovenian
        (&["gd", "hsb", "sl"], ONE_TWO_FEW_OTHER),
        (&["ar"], ALL_SIX),
    ];
    let candidates = [locale.to_string(), alternate_locale(locale)];
    for candidate in candidates.iter().filter(|c| !c.is_empty()) {
        for (locales, keys) in table {
            if locales.contains(&candidate.as_str()) {
                return Some(keys);
            }
        }
    }
    None
}

/// ref: missing_keys.rb#alternate_locale_from — `pt-br` also tries `pt-BR`.
fn alternate_locale(locale: &str) -> String {
    match locale.split_once('-') {
        Some((lang, region)) => format!("{lang}-{}", region.to_uppercase()),
        None => String::new(),
    }
}

/// The base form when the key names one plural category, the key otherwise.
///
/// ref: plural_keys.rb#depluralize_key
pub fn depluralize_key(key: &str, tree: Option<&LocaleTree>, base: Option<&LocaleTree>) -> String {
    let last = last_key_part(key);
    if !CLDR_CATEGORY_KEYS.contains(&last.as_str()) {
        return key.to_string();
    }
    let Some(parent) = parent_key(key) else {
        return key.to_string();
    };
    // The gem looks the parent up in `locale` first, then in the base locale.
    for candidate in [tree, base].into_iter().flatten() {
        if candidate.children(parent).is_empty() {
            continue;
        }
        return if candidate.is_plural_node(parent) {
            parent.to_string()
        } else {
            key.to_string()
        };
    }
    key.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plural_suffixes() {
        assert!(plural_suffix("one"));
        assert!(plural_suffix("other"));
        assert!(plural_suffix("0"));
        assert!(plural_suffix("1"));
        assert!(!plural_suffix("title"));
    }

    #[test]
    fn required_categories_match_rails_i18n() {
        assert_eq!(required_categories("de"), Some(&["one", "other"][..]));
        assert_eq!(required_categories("en"), Some(&["one", "other"][..]));
        assert_eq!(required_categories("fr"), Some(&["one", "other"][..]));
        assert_eq!(
            required_categories("ru"),
            Some(&["one", "few", "many", "other"][..])
        );
        assert_eq!(required_categories("ja"), Some(&["other"][..]));
        assert_eq!(required_categories("ar").unwrap().len(), 6);
        // `pt-br` falls back to `pt-BR`.
        assert_eq!(required_categories("pt-br"), Some(&["one", "other"][..]));
        // rails-i18n ships nothing for this, so no plural check happens.
        assert_eq!(required_categories("xx"), None);
    }
}
