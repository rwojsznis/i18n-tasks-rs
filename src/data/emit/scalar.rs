//! How one scalar is written: the quoting rules, and the block-scalar forms.
//!
//! ref: blocker B1 in `docs/design-notes.md`. Psych defines the shape, so
//! every rule here cites the Psych behaviour it copies.

use crate::data::load::Value;

pub(super) fn push_indent(out: &mut String, indent: usize) {
    for _ in 0..indent {
        out.push_str("  ");
    }
}

/// A mapping key. A key is always one line, so it never becomes a block scalar.
pub fn format_key(key: &str) -> String {
    match style_of(key) {
        Style::Plain => key.to_string(),
        Style::Single => single_quoted(key),
        Style::Double => double_quoted(key),
    }
}

/// A leaf value, including the block-scalar forms. `indent` is the indent of
/// the line that carries the key or the dash; block content goes one deeper.
pub fn format_scalar(value: &Value, indent: usize) -> String {
    let s = match value {
        Value::Str(s) => s.as_str(),
        // A number, a `true`, or a Symbol inside a sequence keeps the form it
        // was written in, so the file round-trips unchanged.
        Value::Plain(s) => return s.clone(),
        Value::Bool(b) => return b.to_string(),
        Value::Nil => return String::new(),
        Value::Seq(_) | Value::Map(_) => unreachable!("handled by write_value"),
    };
    if block_safe(s) {
        return block_scalar(s, indent + 1);
    }
    match style_of(s) {
        Style::Plain => s.to_string(),
        Style::Single => single_quoted(s),
        Style::Double => double_quoted(s),
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Style {
    Plain,
    Single,
    Double,
}

/// Decides how a one-line scalar is written.
///
/// "Required" is defined by these rules, and every one of them has a test:
///
/// * **Q1** the empty string, which has no plain form;
/// * **Q2** the value opens with a character that is not a word character and
///   holds no `"`. The YAML grammar needs only part of this: no plain scalar
///   may open with an indicator (`- ? : , [ ] { } # & * ! | > ' " % @ \``) or a
///   space, which is why `indicator_start` is checked on its own. The rest of
///   it — every other non-word character, and the `"` exception — is Psych's
///   rule copied verbatim (`o =~ /^[^[:word:]][^"]*$/` in
///   `Psych::Visitors::YAMLTree#visit_String`). Copying it keeps the output
///   identical to the gem's for the ordinary case, and that is what makes the
///   one-time reformat reviewable by hand: 15 files on the reference project
///   instead of several hundred;
/// * **Q3** a trailing space;
/// * **Q4** `": "` inside the value, or a trailing `:`, either of which would
///   read back as a mapping;
/// * **Q5** `" #"` inside the value, which would start a comment;
/// * **Q6** YAML 1.1 resolves the text to something that is not a string — a
///   null, a boolean, a number, a timestamp, `<<` or `=`;
/// * **Q7** a control character, a tab included, which no plain scalar can
///   carry legibly. This one forces the double-quoted form.
///
/// Between the two quoted forms: double quotes when escapes are needed and
/// when Q2 applies, single quotes otherwise. That is Psych's choice too.
fn style_of(s: &str) -> Style {
    if needs_escapes(s) {
        return Style::Double;
    }
    let first = s.chars().next();
    let non_word_start =
        first.is_some_and(|c| !c.is_alphanumeric() && c != '_') && !s.contains('"');
    let indicator_start = first.is_some_and(|c| c == ' ' || "-?:,[]{}#&*!|>'\"%@`".contains(c));
    let required = s.is_empty()                                     // Q1
        || non_word_start || indicator_start                        // Q2
        || s.ends_with(' ')                                         // Q3
        || s.contains(": ") || s.ends_with(':')                     // Q4
        || s.contains(" #")                                         // Q5
        || resolves_to_non_string(s); // Q6
    if !required {
        return Style::Plain;
    }
    if non_word_start {
        Style::Double
    } else {
        Style::Single
    }
}

/// Q7. `char::is_control` covers `\t`, `\n`, `\r` and the rest of C0 and C1.
fn needs_escapes(s: &str) -> bool {
    s.chars().any(char::is_control)
}

/// Q6. A plain scalar that YAML 1.1 does not read back as a string.
///
/// The answer comes from `yaml::resolve_plain`, the same resolver the loader
/// classifies with, so the two cannot disagree about a value's type. The four
/// extras are values Psych reads as strings but that are worth a pair of
/// quotes anyway: `y` and `n`, which a less strict YAML 1.1 reader takes for
/// booleans, and `<<` and `=`, Psych's merge and value keys.
fn resolves_to_non_string(s: &str) -> bool {
    !matches!(crate::yaml::resolve_plain(s), crate::yaml::Resolved::Str)
        || matches!(s, "y" | "Y" | "n" | "N" | "<<" | "=")
}

fn single_quoted(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// Non-BMP characters stay literal. The gem has to undo Psych's `\Uxxxxxxxx`
/// with a regex; there is nothing to undo here.
fn double_quoted(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Can this value be written as a `|` block scalar without changing it?
///
/// The first line has to carry the indentation, so it must not be empty and
/// must not open with a space; otherwise Psych needs an explicit indicator
/// such as `|2`, and this emitter double-quotes instead. A line that ends in a
/// space, or holds a tab or any other control character, is also refused,
/// because block content is copied verbatim and the whitespace would be easy
/// to lose in an editor.
fn block_safe(s: &str) -> bool {
    if !s.contains('\n') {
        return false;
    }
    let body = s.trim_end_matches('\n');
    let Some(first) = body.split('\n').next() else {
        return false;
    };
    if first.is_empty() || first.starts_with(' ') {
        return false;
    }
    body.split('\n')
        .all(|line| !line.ends_with(' ') && !line.chars().any(char::is_control))
}

/// ref: spec/yaml_spec.rb — Psych normalizes `|+` to `|` and every folded
/// style to a literal one, so only three chomping indicators ever appear.
fn block_scalar(s: &str, indent: usize) -> String {
    let trailing = s.len() - s.trim_end_matches('\n').len();
    let header = match trailing {
        0 => "|-",
        1 => "|",
        _ => "|+",
    };
    let mut lines: Vec<&str> = s.split('\n').collect();
    if trailing > 0 {
        lines.pop();
    }
    let mut out = String::from(header);
    for line in lines {
        out.push('\n');
        // An empty line is written empty, never as indentation alone, so the
        // file holds no trailing spaces.
        if !line.is_empty() {
            push_indent(&mut out, indent);
            out.push_str(line);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> Value {
        Value::Str(v.into())
    }

    fn scalar(v: &str) -> String {
        format_scalar(&s(v), 1)
    }

    // One test per quoting rule.

    #[test]
    fn q1_empty_string() {
        assert_eq!(scalar(""), "''");
    }

    #[test]
    fn q2_non_word_first_character() {
        assert_eq!(scalar("%{count} items"), "\"%{count} items\"");
        assert_eq!(scalar("-dash"), "\"-dash\"");
        assert_eq!(scalar("#hash"), "\"#hash\"");
        assert_eq!(scalar(" lead"), "\" lead\"");
        assert_eq!(scalar("«quoted»"), "\"«quoted»\"");
        // A word character opens a plain scalar, accents included.
        assert_eq!(scalar("Ärzte"), "Ärzte");
        assert_eq!(scalar("5% off"), "5% off");
        // A `"` inside takes the value out of Q2, the same as in Psych, but an
        // opening indicator still forces quotes.
        assert_eq!(scalar("«a\"b»"), "«a\"b»");
        assert_eq!(scalar("<a href=\"x\">y</a>"), "<a href=\"x\">y</a>");
        assert_eq!(scalar("#a\"b"), "'#a\"b'");
    }

    #[test]
    fn q3_trailing_space_or_tab() {
        assert_eq!(scalar("trail "), "'trail '");
        assert_eq!(scalar("a\tb"), "\"a\\tb\"");
    }

    #[test]
    fn q4_colon_space_or_trailing_colon() {
        assert_eq!(scalar("a: b"), "'a: b'");
        assert_eq!(scalar("note:"), "'note:'");
        // A colon that is not followed by a space is plain.
        assert_eq!(scalar("a:b"), "a:b");
    }

    #[test]
    fn q5_space_hash() {
        assert_eq!(scalar("a #b"), "'a #b'");
        assert_eq!(scalar("a#b"), "a#b");
    }

    #[test]
    fn q6_values_yaml_would_not_read_as_strings() {
        for v in [
            "true",
            "false",
            "yes",
            "no",
            "on",
            "off",
            "y",
            "n",
            "null",
            "123",
            "1.5",
            "0x1f",
            "010",
            "089",
            "1_000",
            "1:30",
            ".inf",
            "2020-01-01",
            "<<",
            "=",
        ] {
            assert_ne!(scalar(v), v, "{v} must be quoted");
        }
        // These only look numeric.
        for v in ["1.2.3", "e5", "12a", "0x", "1-2"] {
            assert_eq!(scalar(v), v, "{v} must stay plain");
        }
    }

    #[test]
    fn q7_control_characters() {
        assert_eq!(scalar("a\u{7}b"), "\"a\\x07b\"");
        assert_eq!(scalar("a\rb"), "\"a\\rb\"");
    }

    #[test]
    fn non_bmp_characters_stay_literal() {
        assert_eq!(scalar("😀 emoji"), "\"😀 emoji\"");
        assert_eq!(scalar("emoji 😀"), "emoji 😀");
    }

    #[test]
    fn single_quotes_double_up() {
        assert_eq!(scalar("'quoted'"), "\"'quoted'\"");
        assert_eq!(scalar("it's: here"), "'it''s: here'");
    }

    // ref: spec/yaml_spec.rb
    #[test]
    fn multi_line_values_use_block_scalars() {
        assert_eq!(scalar("hello\nworld\n"), "|\n    hello\n    world");
        assert_eq!(scalar("hello\nworld"), "|-\n    hello\n    world");
        assert_eq!(scalar("hello\nworld\n\n"), "|+\n    hello\n    world\n");
        assert_eq!(scalar("hi\n"), "|\n    hi");
    }

    #[test]
    fn unsafe_multi_line_values_are_double_quoted() {
        // A leading space would need Psych's `|2` indicator.
        assert_eq!(scalar("  indented\nnext\n"), "\"  indented\\nnext\\n\"");
        // A trailing space would be invisible in the block form.
        assert_eq!(scalar("tail \nnext\n"), "\"tail \\nnext\\n\"");
    }

    #[test]
    fn a_double_quoted_value_escapes_the_quote_and_the_backslash() {
        // A `"` in the value takes it out of Q2 but the newline still forces
        // the double-quoted form, which then has to escape both characters.
        // A leading space on the first line rules out the block form, so the
        // double-quoted form has to escape the quote, the backslash and the
        // newline itself.
        assert_eq!(scalar(" say \"hi\"\nx"), "\" say \\\"hi\\\"\\nx\"");
        assert_eq!(scalar(" a\\b\nc"), "\" a\\\\b\\nc\"");
        // A tab anywhere rules out the block form too.
        assert_eq!(scalar("tab\there\nx"), "\"tab\\there\\nx\"");
    }

    /// A key follows the same quoting rules as a value, including the double
    /// quoted form.
    #[test]
    fn a_key_that_needs_double_quotes_gets_them() {
        assert_eq!(format_key("a\tb"), "\"a\\tb\"");
        assert_eq!(format_key("plain"), "plain");
        assert_eq!(format_key("has space"), "has space");
    }

    #[test]
    fn a_bool_and_a_nil_leaf_keep_their_written_form() {
        assert_eq!(format_scalar(&Value::Bool(true), 0), "true");
        assert_eq!(format_scalar(&Value::Bool(false), 0), "false");
        assert_eq!(format_scalar(&Value::Nil, 0), "");
        assert_eq!(format_scalar(&Value::Plain("1.50".into()), 0), "1.50");
    }
}
