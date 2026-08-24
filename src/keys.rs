//! Key splitting and the `underscore` inflection.

/// Splits a key on dots. Dots inside `{}`, `[]`, `()` or `<>` do not split.
///
/// ref: lib/i18n/tasks/split_key.rb
pub fn split_key(key: &str) -> Vec<String> {
    split_key_max(key, usize::MAX)
}

/// ref: lib/i18n/tasks/split_key.rb#split_key
pub fn split_key_max(key: &str, max: usize) -> Vec<String> {
    if max == 1 {
        return vec![key.to_string()];
    }
    let mut parts: Vec<String> = Vec::new();
    let mut end_char: Option<char> = None;
    let mut part = String::new();
    for (index, ch) in key.char_indices() {
        if let Some(want) = end_char {
            part.push(ch);
            if ch == want {
                end_char = None;
            }
        } else if let Some(want) = closing_for(ch) {
            part.push(ch);
            end_char = Some(want);
        } else if ch == '.' {
            parts.push(std::mem::take(&mut part));
            if parts.len() + 1 == max {
                let remaining = &key[index + 1..];
                if !remaining.is_empty() {
                    parts.push(remaining.to_string());
                }
                return parts;
            }
        } else {
            part.push(ch);
        }
    }
    if part.is_empty() {
        return parts;
    }
    if end_char.is_some() {
        parts.extend(part.split('.').map(str::to_string));
    } else {
        parts.push(part);
    }
    parts
}

fn closing_for(ch: char) -> Option<char> {
    match ch {
        '{' => Some('}'),
        '[' => Some(']'),
        '(' => Some(')'),
        '<' => Some('>'),
        _ => None,
    }
}

/// ref: lib/i18n/tasks/split_key.rb#last_key_part
pub fn last_key_part(key: &str) -> String {
    split_key(key).pop().unwrap_or_default()
}

/// `ActiveSupport::Inflector#underscore` with the default (empty) acronym list.
///
/// The gem calls `String#underscore` in
/// `scanners/prism_scanners/nodes.rb#path_name`. With no acronyms registered,
/// `acronyms_underscore_regex` never matches, which leaves four steps:
///
/// ```text
/// gsub("::", "/")
/// gsub(/([A-Z\d]+)([A-Z][a-z])/, '\1_\2')
/// gsub(/([a-z\d])([A-Z])/, '\1_\2')
/// tr("-", "_"); downcase
/// ```
pub fn underscore(word: &str) -> String {
    if !word.chars().any(|c| c.is_ascii_uppercase() || c == '-') && !word.contains("::") {
        return word.to_string();
    }
    let src: Vec<char> = word.replace("::", "/").chars().collect();
    // /([A-Z\d]+)([A-Z][a-z])/ -> '\1_\2'
    let mut a: Vec<char> = Vec::with_capacity(src.len() + 8);
    let mut i = 0;
    while i < src.len() {
        // Longest run of [A-Z\d] starting here, followed by [A-Z][a-z].
        let mut run = i;
        while run < src.len() && (src[run].is_ascii_uppercase() || src[run].is_ascii_digit()) {
            run += 1;
        }
        // The run must leave at least the `[A-Z]` of `[A-Z][a-z]` behind, and
        // the character after that must be lowercase.
        if run > i + 1
            && run <= src.len()
            && src[run - 1].is_ascii_uppercase()
            && run < src.len()
            && src[run].is_ascii_lowercase()
        {
            a.extend(&src[i..run - 1]);
            a.push('_');
            a.push(src[run - 1]);
            a.push(src[run]);
            i = run + 1;
        } else {
            a.push(src[i]);
            i += 1;
        }
    }
    // /([a-z\d])([A-Z])/ -> '\1_\2'
    let mut b = String::with_capacity(a.len() + 8);
    let mut j = 0;
    while j < a.len() {
        b.push(a[j]);
        if (a[j].is_ascii_lowercase() || a[j].is_ascii_digit())
            && j + 1 < a.len()
            && a[j + 1].is_ascii_uppercase()
        {
            b.push('_');
            b.push(a[j + 1]);
            j += 2;
        } else {
            j += 1;
        }
    }
    b.replace('-', "_").to_lowercase()
}

/// The parent key, or `None` at the root.
pub fn parent_key(key: &str) -> Option<&str> {
    key.rsplit_once('.').map(|(head, _)| head)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Full port of the table in spec/split_key_spec.rb.
    #[test]
    fn splits_on_dots() {
        let empty: Vec<String> = Vec::new();
        assert_eq!(split_key(""), empty);
        assert_eq!(split_key("a"), vec!["a"]);
        assert_eq!(split_key("a.b"), vec!["a", "b"]);
        // A trailing dot contributes no part.
        assert_eq!(split_key("a.b."), vec!["a", "b"]);
        assert_eq!(split_key("a.b.c"), vec!["a", "b", "c"]);
        assert_eq!(split_key("a.#{b.c}"), vec!["a", "#{b.c}"]);
        assert_eq!(split_key("a.#{b.c}."), vec!["a", "#{b.c}"]);
        assert_eq!(split_key("a.#{b.c}.d"), vec!["a", "#{b.c}", "d"]);
        assert_eq!(
            split_key("a.#{b.c}.d.[e.f]"),
            vec!["a", "#{b.c}", "d", "[e.f]"]
        );
        assert_eq!(
            split_key("a.#{b.c}.d.<e.f>"),
            vec!["a", "#{b.c}", "d", "<e.f>"]
        );
        assert_eq!(split_key("a.b->c.d.<e.f>"), vec!["a", "b->c", "d", "<e.f>"]);
        // Opened but never closed: the dots inside split after all.
        assert_eq!(
            split_key("a.b.c.d.<e.f"),
            vec!["a", "b", "c", "d", "<e", "f"]
        );
        // `(` closes on `)`, the fourth bracket pair the gem recognises.
        assert_eq!(split_key("a.(b.c).d"), vec!["a", "(b.c)", "d"]);
    }

    /// ref: spec/split_key_spec.rb "limits results to second argument"
    #[test]
    fn split_key_honours_the_limit() {
        assert_eq!(split_key_max("a.b.c", 1), vec!["a.b.c"]);
        assert_eq!(split_key_max("a.b.c", 2), vec!["a", "b.c"]);
        assert_eq!(split_key_max("a.b.c.", 2), vec!["a", "b.c."]);
        assert_eq!(
            split_key_max("a.b.c.d.e.f", 4),
            vec!["a", "b", "c", "d.e.f"]
        );
        // The limit lands exactly on the trailing dot, so there is no tail.
        assert_eq!(split_key_max("a.b.", 3), vec!["a", "b"]);
    }

    /// ref: spec/split_key_spec.rb "last part"
    #[test]
    fn last_part_of_a_key() {
        assert_eq!(last_key_part("a.b.c"), "c");
        assert_eq!(last_key_part("a"), "a");
        assert_eq!(last_key_part("a.b.c.d"), "d");
        assert_eq!(last_key_part(""), "");
    }

    #[test]
    fn underscore_matches_active_support() {
        assert_eq!(underscore("HTTPServer"), "http_server");
        assert_eq!(underscore("UsersController"), "users_controller");
        assert_eq!(
            underscore("Admin::V2::JobsController"),
            "admin/v2/jobs_controller"
        );
        assert_eq!(underscore("already_snake"), "already_snake");
        assert_eq!(underscore("Foo-Bar"), "foo_bar");
        assert_eq!(underscore("A"), "a");
        assert_eq!(underscore("APIv2Thing"), "ap_iv2_thing");
        assert_eq!(underscore("Job::IndexPresenter"), "job/index_presenter");
        assert_eq!(underscore("ABCDef"), "abc_def");
        assert_eq!(underscore("XMLHttpRequest"), "xml_http_request");
        assert_eq!(underscore("V2"), "v2");
        assert_eq!(underscore("A1B2"), "a1_b2");
        assert_eq!(underscore("SimpleForm2Thing"), "simple_form2_thing");
    }

    #[test]
    fn parent_key_walks_up() {
        assert_eq!(parent_key("a.b.c"), Some("a.b"));
        assert_eq!(parent_key("a"), None);
    }
}
