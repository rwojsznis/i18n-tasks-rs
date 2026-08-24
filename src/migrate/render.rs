//! The migrated config file, and the report for the terminal.
//!
//! The header is the record of the migration: what the source set, what was
//! dropped and why, and what no tool can migrate. Whoever opens the file in six
//! months reads it there.

use super::Migration;
use super::erb::redact_erb;
use std::path::Path;

/// The header explains itself to whoever opens the file in six months.
pub(super) fn render(m: &Migration, blocks: &[String], from: &Path) -> String {
    let mut out = String::new();
    out.push_str("# i18n-tasks-rs configuration.\n#\n");
    out.push_str(&format!(
        "# Migrated from {} by `i18n-tasks-rs migrate-config`.\n",
        from.display()
    ));
    out.push_str(
        "# This file is plain YAML. It is read, never evaluated: no ERB, no Ruby,\n\
         # no scanner class names. An unknown key is an error.\n",
    );
    if m.base_locale_defaulted {
        out.push_str(
            "#\n# The source set no `base_locale`, so the gem's default was written out\n\
             # below.\n",
        );
    }
    if !m.dropped.is_empty() {
        out.push_str("#\n# Dropped in migration:\n");
        for d in &m.dropped {
            out.push_str(&format!("#   {} (line {}) — {}\n", d.key, d.line, d.reason));
        }
    }
    if !m.manual.is_empty() {
        out.push_str(
            "#\n# NEEDS ATTENTION. These lines computed their value with ERB, which this\n\
             # tool cannot evaluate. Nothing replaced them; write the values out by hand:\n",
        );
        for man in &m.manual {
            // The ERB itself cannot be quoted here: `Config::parse` rejects
            // `<%` anywhere in the file, comments included. The untouched line
            // is on the terminal, and in the source file that still exists.
            out.push_str(&format!(
                "#   line {}: {}\n",
                man.line,
                redact_erb(&man.text)
            ));
        }
    }
    for block in blocks {
        out.push('\n');
        out.push_str(block.trim_end());
        out.push('\n');
    }
    out
}

/// The report for the terminal.
pub fn to_text(m: &Migration, from: &Path, to: &Path, written: bool) -> String {
    let mut s = String::new();
    s.push_str(&format!("{} -> {}\n", from.display(), to.display()));
    s.push_str(&format!(
        "  kept {} setting(s): {}\n",
        m.kept.len(),
        m.kept.join(", ")
    ));
    if m.base_locale_defaulted {
        s.push_str("  added base_locale: en, the gem's default. The source set none.\n");
    }
    if !m.erb_lines.is_empty() {
        s.push_str(&format!("  removed ERB on {} line(s)\n", m.erb_lines.len()));
    }
    for d in &m.dropped {
        s.push_str(&format!("  dropped {} ({}): {}\n", d.key, d.line, d.reason));
    }
    for man in &m.manual {
        s.push_str(&format!(
            "  NEEDS ATTENTION line {}: {}\n",
            man.line, man.text
        ));
    }
    if written {
        s.push_str(&format!("  wrote {}\n", to.display()));
    } else {
        s.push_str("  nothing written. Pass `--write` to create the file.\n");
    }
    s
}
