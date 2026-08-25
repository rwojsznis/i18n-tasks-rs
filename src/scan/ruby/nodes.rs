//! Small questions about a Prism node: its name, whether it is a `t`-family
//! call, and whether a receiver is `I18n`.
//!
//! ref: lib/i18n/tasks/scanners/prism_scanners/visitor.rb

use ruby_prism as pr;

pub(super) fn name_of(id: &pr::ConstantId) -> String {
    String::from_utf8_lossy(id.as_slice()).into_owned()
}

/// ref: visitor.rb:98 — the `t`-family method names.
///
/// The name is compared as raw bytes because every call node in the file
/// reaches this test, and a `String` per node buys nothing.
pub(super) fn is_translation_name(name: &[u8]) -> bool {
    matches!(name, b"t" | b"t!" | b"translate" | b"translate!")
}

/// ref: visitor.rb#i18n_receiver? (lines 188-197)
pub(super) fn is_i18n_receiver(recv: &pr::Node) -> bool {
    if let Some(n) = recv.as_constant_read_node() {
        return n.name().as_slice() == b"I18n";
    }
    // `::I18n` — no parent, and the name is I18n.
    if let Some(n) = recv.as_constant_path_node() {
        return n.parent().is_none() && n.name().map(|n| n.as_slice()) == Some(&b"I18n"[..]);
    }
    false
}

/// The parts of a constant path, as `full_name_parts` returns them.
pub(super) fn constant_path_parts(node: &pr::Node) -> Vec<String> {
    if let Some(n) = node.as_constant_read_node() {
        return vec![name_of(&n.name())];
    }
    if let Some(n) = node.as_constant_path_node() {
        let mut parts = n
            .parent()
            .map(|p| constant_path_parts(&p))
            .unwrap_or_default();
        if let Some(name) = n.name() {
            parts.push(name_of(&name));
        }
        return parts;
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ref: visitor.rb:98 — the `t`-family names, and nothing else.
    ///
    /// The name is compared as bytes, so the near misses are what matter: a
    /// name that only shares a prefix, and one that only differs in case,
    /// must both stay out.
    #[test]
    fn only_the_t_family_names_are_translation_calls() {
        for name in [&b"t"[..], b"t!", b"translate", b"translate!"] {
            assert!(
                is_translation_name(name),
                "{} should be a translation call",
                String::from_utf8_lossy(name)
            );
        }
        for name in [
            &b"tt"[..],
            b"T",
            b"Translate",
            b"translate?",
            b"",
            b"private",
        ] {
            assert!(
                !is_translation_name(name),
                "{} should not be a translation call",
                String::from_utf8_lossy(name)
            );
        }
    }
}
