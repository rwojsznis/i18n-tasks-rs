//! Which locale a path names, and which locales a project has.
//!
//! ref: lib/i18n/tasks/data/file_system_base.rb:122-124

use super::glob::glob_paths;
use crate::config::{Config, interpolate_locale};
use regex::Regex;

/// ref: lib/i18n/tasks/data/file_system_base.rb#available_locales
pub(super) fn available_locales(cfg: &Config) -> Vec<String> {
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
