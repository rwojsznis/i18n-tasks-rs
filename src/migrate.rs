//! `migrate-config`: the gem's config to the plain-YAML config this tool reads.
//!
//! Blocker B3: the gem evaluates its config as ERB and then as Ruby, so a real
//! config can `require` a scanner, shell out, or boot Rails. This tool never
//! executes code, which means a gem config cannot simply be renamed. The
//! migration therefore does three things:
//!
//!   1. strips the ERB, keeping the line numbering intact so every message
//!      still points at the original file;
//!   2. drops the settings this port has no equivalent for, each with the
//!      reason recorded in the output header;
//!   3. re-checks the result with [`Config::parse`] before anything is written,
//!      so a migration either produces a config this tool accepts or fails.
//!
//! The output is produced by slicing the original lines, not by re-serializing
//! a parsed tree, so comments, quoting and list formatting all survive. That
//! matters: in a real config the comments above an `ignore_unused` entry are
//! often the only record of *why* the key is ignored.

use crate::config::Config;
use crate::yaml::{self, Node};
use std::path::{Path, PathBuf};

/// The file the migrated config is written to.
pub const MIGRATION_TARGET: &str = "config/i18n-tasks-rs.yml";

/// Where a gem config is looked for, in the gem's own order of preference.
///
/// ref: lib/i18n/tasks/configuration.rb:18-21
pub const GEM_CONFIG_CANDIDATES: &[&str] = &["config/i18n-tasks.yml", "config/i18n-tasks.yml.erb"];

/// A setting that did not survive the migration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dropped {
    /// Dotted path, as it appeared in the source.
    pub key: String,
    /// Line in the source file, 1-based.
    pub line: usize,
    pub reason: String,
}

/// A line that used ERB in a position where the value itself was computed.
/// Nothing can be done with it automatically, so it is reported and left out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manual {
    pub line: usize,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct Migration {
    /// The migrated config, ready to write.
    pub output: String,
    /// Top-level keys that survived, in source order.
    pub kept: Vec<String>,
    pub dropped: Vec<Dropped>,
    pub manual: Vec<Manual>,
    /// Lines that held nothing but ERB, such as an `<% require ... %>` prelude.
    pub erb_lines: Vec<usize>,
    /// True when the source had no `base_locale` and the gem's default was
    /// written out explicitly.
    pub base_locale_defaulted: bool,
}

impl Migration {
    /// True when the migrated config still needs a human.
    pub fn needs_attention(&self) -> bool {
        !self.manual.is_empty()
    }
}

/// Picks the gem config to migrate when none was named.
pub fn find_gem_config(root: &Path) -> Option<PathBuf> {
    GEM_CONFIG_CANDIDATES
        .iter()
        .map(|c| root.join(c))
        .find(|p| p.is_file())
}

/// `from` and `to` are only used for messages and for the header.
pub fn migrate(src: &str, from: &Path, to: &Path) -> Result<Migration, String> {
    let stripped = strip_erb(src);
    let lines: Vec<&str> = stripped.text.lines().collect();

    let node = yaml::parse(&stripped.text, from)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| {
            format!(
                "{}: no settings found. After the ERB was removed the file held \
                 nothing but comments.",
                from.display()
            )
        })?;
    let top = node
        .as_map()
        .ok_or_else(|| format!("{}: config must be a YAML mapping", from.display()))?;

    let mut dropped = Vec::new();
    let mut kept = Vec::new();
    let mut blocks: Vec<String> = Vec::new();

    let starts: Vec<usize> = top.iter().map(|(k, _)| k.line()).collect();
    let extents = extents(&lines, &starts, 1, lines.len());

    for (i, (k, v)) in top.iter().enumerate() {
        let key = k
            .as_str()
            .ok_or_else(|| format!("{}: non-string config key", from.display()))?;
        let line = k.line();
        match decide(key, v, None) {
            Decision::Drop(reason) => dropped.push(Dropped {
                key: key.to_string(),
                line,
                reason,
            }),
            Decision::Keep => {
                kept.push(key.to_string());
                blocks.push(slice(&lines, extents[i].lead_start, extents[i].body_end));
            }
            // `data` and `search` are the only nested sections, and both are
            // filtered one key at a time.
            Decision::Recurse => {
                let block = nested(&lines, key, line, v, &extents[i], from, &mut dropped)?;
                match block {
                    Some(text) => {
                        kept.push(key.to_string());
                        blocks.push(text);
                    }
                    None => dropped.push(Dropped {
                        key: key.to_string(),
                        line,
                        reason: "every setting under it was dropped".into(),
                    }),
                }
            }
        }
    }

    let base_locale_defaulted = !kept.iter().any(|k| k == "base_locale");
    if base_locale_defaulted {
        // Written out rather than left implicit: the gem defaults to `en`, and
        // a config whose base locale is invisible is a trap.
        blocks.insert(0, "base_locale: en".to_string());
    }

    let mut migration = Migration {
        output: String::new(),
        kept,
        dropped,
        manual: stripped.manual,
        erb_lines: stripped.erb_lines,
        base_locale_defaulted,
    };
    migration.output = render(&migration, &blocks, from);

    // The whole point of the command is to produce a config this tool accepts.
    // If it did not, that is a bug here, not the user's problem to debug.
    Config::parse(&migration.output, to, PathBuf::from(".")).map_err(|e| {
        format!(
            "migration produced a config this tool cannot read. This is a bug in \
             `migrate-config`; please report it with your {}.\n  {e}",
            from.display()
        )
    })?;
    Ok(migration)
}

/// What to do with one config key.
enum Decision {
    Keep,
    /// Filter the children instead. `data` and `search` only.
    Recurse,
    Drop(String),
}

/// `section` is `None` at the top level, otherwise the parent key.
fn decide(key: &str, value: &Node, section: Option<&str>) -> Decision {
    // A key with no value is the gem template's way of showing an example, e.g.
    // `external:` followed by commented-out samples. Keeping it would hand the
    // tool a list holding one empty path.
    if yaml::is_null_scalar(value) {
        return Decision::Drop("it had no value".into());
    }
    let drop = |reason: &str| Decision::Drop(reason.to_string());
    match (section, key) {
        (None, "base_locale" | "locales" | "ignore") => Decision::Keep,
        (None, "ignore_missing" | "ignore_unused" | "ignore_inconsistent_interpolations") => {
            Decision::Keep
        }
        // Read, and reported as such, but the `eq-base` report itself is out of
        // scope. Kept so the patterns are not lost.
        (None, "ignore_eq_base") => Decision::Keep,
        (None, "data" | "search") => Decision::Recurse,
        (None, "internal_locale") => drop("reports are English only"),
        (None, "translation") => drop("translation backends are out of scope"),

        (Some("data"), "read" | "write" | "external" | "keep_order") => Decision::Keep,
        (Some("data"), "router") => match value.as_str() {
            Some("conservative_router") | Some("pattern_router") => Decision::Keep,
            Some(other) => Decision::Drop(format!(
                "`{other}` is not one of conservative_router, pattern_router; \
                 the default conservative_router applies"
            )),
            None => drop("the router must be a name"),
        },
        (Some("data"), "adapter") => drop("the only data adapter is the YAML file system"),
        (Some("data"), "yaml") => {
            drop("the emitter has no options; it never folds lines (blocker B1)")
        }
        (Some("data"), "json") => drop("JSON locale files are not supported"),

        (Some("search"), "paths" | "exclude" | "only" | "relative_roots") => Decision::Keep,
        (Some("search"), "relative_exclude_method_name_paths") => Decision::Keep,
        (Some("search"), "scanners") => {
            drop("scanners are built in and picked by file extension; no class names")
        }
        (Some("search"), "prism") => {
            drop("Prism is the only Ruby parser and Rails detection is always on")
        }
        (Some("search"), "strict") => {
            drop("a dynamic key always becomes a pattern; there is no strict mode (blocker B5)")
        }
        (Some("search"), "ast_matchers") => {
            drop("the Rails model and mailer-subject matchers are always on")
        }
        _ => drop("i18n-tasks-rs has no such setting"),
    }
}

/// Renders `data` or `search` with the unsupported children removed. Returns
/// `None` when nothing under it survived.
fn nested(
    lines: &[&str],
    section: &str,
    parent_line: usize,
    value: &Node,
    extent: &Extent,
    from: &Path,
    dropped: &mut Vec<Dropped>,
) -> Result<Option<String>, String> {
    let entries = value.as_map().ok_or_else(|| {
        format!(
            "{}:{parent_line}: `{section}` must be a mapping",
            from.display()
        )
    })?;
    let starts: Vec<usize> = entries.iter().map(|(k, _)| k.line()).collect();
    // A flow mapping puts the children on the parent's line, so there are no
    // line ranges to slice. Rare enough to refuse rather than re-serialize.
    if starts.iter().any(|&s| s <= parent_line) {
        return Err(format!(
            "{}:{parent_line}: `{section}` is written in flow style (`{{a: b}}`). \
             Rewrite it as an indented block and run the migration again.",
            from.display()
        ));
    }
    let child_extents = extents(lines, &starts, parent_line + 1, extent.body_end);

    let mut kept_blocks = Vec::new();
    for (i, (k, v)) in entries.iter().enumerate() {
        let name = k
            .as_str()
            .ok_or_else(|| format!("{}: non-string config key", from.display()))?;
        let path = format!("{section}.{name}");
        match decide(name, v, Some(section)) {
            Decision::Keep => kept_blocks.push(slice(
                lines,
                child_extents[i].lead_start,
                child_extents[i].body_end,
            )),
            // Neither section has a third level, so `Recurse` cannot appear.
            Decision::Recurse | Decision::Drop(_) => {
                let reason = match decide(name, v, Some(section)) {
                    Decision::Drop(reason) => reason,
                    _ => unreachable!("no config section nests twice"),
                };
                dropped.push(Dropped {
                    key: path,
                    line: k.line(),
                    reason,
                });
            }
        }
    }
    if kept_blocks.is_empty() {
        return Ok(None);
    }
    // The section header keeps its own leading comments, and only those: the
    // comments that sat above a dropped child leave with it.
    let mut out = slice(lines, extent.lead_start, parent_line);
    for block in kept_blocks {
        out.push('\n');
        out.push_str(&block);
    }
    Ok(Some(out))
}

/// The line range one config entry owns.
#[derive(Debug, Clone, Copy)]
struct Extent {
    /// First line of the comment block above the key, or the key itself.
    lead_start: usize,
    /// Last line of the value, comments below it excluded.
    body_end: usize,
}

/// Splits a level into one extent per entry. All lines are 1-based.
///
/// Two conventions decide who owns a comment, and between them they cover how
/// config files are actually written:
///
///   * the run of comment lines directly above a key, with no blank line in
///     between, documents that key and moves with it;
///   * anything below a key — a commented-out list entry, a trailing note —
///     stays with the key above it.
///
/// So `## Translation Services` leaves with the `translation:` block it
/// introduces, while a commented-out `# - 'errors.messages.*'` under
/// `ignore_missing` stays under `ignore_missing`.
fn extents(lines: &[&str], starts: &[usize], level_start: usize, level_end: usize) -> Vec<Extent> {
    let at = |n: usize| lines.get(n - 1).copied().unwrap_or("");
    let lead_start = |i: usize| {
        let start = starts[i];
        // Never climb past the previous key, whatever is in between.
        let floor = if i == 0 {
            level_start
        } else {
            starts[i - 1] + 1
        };
        let mut lead = start;
        while lead > floor && is_comment(at(lead - 1)) {
            lead -= 1;
        }
        lead
    };
    (0..starts.len())
        .map(|i| {
            let start = starts[i];
            let mut body_end = if i + 1 < starts.len() {
                lead_start(i + 1).saturating_sub(1)
            } else {
                level_end
            }
            .max(start);
            loop {
                // A blank line at the end of a block is a separator.
                while body_end > start && at(body_end).trim().is_empty() {
                    body_end -= 1;
                }
                // A comment block that a blank line separates from the value
                // below it belongs to neither key. In a gem config that is
                // always the commented-out documentation of a setting nobody
                // enabled, and half of it documents settings this port dropped.
                let mut probe = body_end;
                while probe > start && is_comment(at(probe)) {
                    probe -= 1;
                }
                if probe < body_end && at(probe).trim().is_empty() {
                    body_end = probe;
                } else {
                    break;
                }
            }
            Extent {
                lead_start: lead_start(i),
                body_end,
            }
        })
        .collect()
}

fn is_comment(line: &str) -> bool {
    line.trim_start().starts_with('#')
}

fn slice(lines: &[&str], from: usize, to: usize) -> String {
    lines[from - 1..to].join("\n")
}

struct Stripped {
    /// The source with every ERB line blanked, so line numbers still match.
    text: String,
    erb_lines: Vec<usize>,
    manual: Vec<Manual>,
}

/// Removes the ERB, one line at a time.
///
/// A line that held nothing but an ERB tag is dropped outright — that is the
/// `<% require ... %>` prelude and its kind. A line that mixed ERB into a YAML
/// value cannot be migrated at all, because the value was computed by code, so
/// it is dropped and reported for a human. Comments are left alone unless they
/// contain `<%`, which [`Config::parse`] rejects wherever it appears.
///
/// Blanked lines keep their place in the file so that every line number in
/// every message still refers to the original.
fn strip_erb(src: &str) -> Stripped {
    let mut text = String::with_capacity(src.len());
    let mut erb_lines = Vec::new();
    let mut manual = Vec::new();
    // Set while an ERB tag opened on an earlier line is still open.
    let mut open = false;

    for (idx, raw) in src.lines().enumerate() {
        let line_no = idx + 1;
        let mut rest = raw;
        let had_erb = open || raw.contains("<%");

        if !had_erb {
            text.push_str(raw);
            text.push('\n');
            continue;
        }

        // Whatever is left of the line once every ERB tag is cut out.
        let mut kept = String::new();
        if open {
            match rest.find("%>") {
                Some(pos) => {
                    rest = &rest[pos + 2..];
                    open = false;
                }
                None => {
                    erb_lines.push(line_no);
                    text.push('\n');
                    continue;
                }
            }
        }
        loop {
            match rest.find("<%") {
                None => {
                    kept.push_str(rest);
                    break;
                }
                Some(pos) => {
                    kept.push_str(&rest[..pos]);
                    match rest[pos..].find("%>") {
                        Some(end) => rest = &rest[pos + end + 2..],
                        None => {
                            open = true;
                            break;
                        }
                    }
                }
            }
        }

        erb_lines.push(line_no);
        text.push('\n');
        // A comment that held ERB simply goes: `Config::parse` rejects `<%`
        // wherever it appears, comments included. A *value* built by ERB is a
        // different matter — no one can guess what the code returned.
        if !kept.trim().is_empty() && !is_comment(raw) {
            manual.push(Manual {
                line: line_no,
                text: raw.trim().to_string(),
            });
        }
    }
    Stripped {
        text,
        erb_lines,
        manual,
    }
}

/// Every ERB tag in `text` becomes `[ERB]`, so the line can be quoted in a
/// config this tool will read back.
fn redact_erb(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(pos) = rest.find("<%") {
        out.push_str(&rest[..pos]);
        out.push_str("[ERB]");
        rest = match rest[pos..].find("%>") {
            Some(end) => &rest[pos + end + 2..],
            None => "",
        };
    }
    out.push_str(rest);
    out
}

/// The header explains itself to whoever opens the file in six months.
fn render(m: &Migration, blocks: &[String], from: &Path) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::IgnoreType;

    fn run(src: &str) -> Migration {
        migrate(
            src,
            Path::new("config/i18n-tasks.yml.erb"),
            Path::new(MIGRATION_TARGET),
        )
        .expect("migration succeeds")
    }

    #[test]
    fn drops_the_erb_prelude_and_the_unsupported_sections() {
        let m = run("<% require 'lib/scanner' %>\n\
                     base_locale: de\n\
                     internal_locale: ru\n\
                     data:\n\
                     \x20 read:\n\
                     \x20   - config/locales/%{locale}.yml\n\
                     \x20 yaml:\n\
                     \x20   write:\n\
                     \x20     line_width: -1\n\
                     search:\n\
                     \x20 prism: \"rails\"\n\
                     \x20 strict: true\n\
                     \x20 paths:\n\
                     \x20   - app/\n\
                     translation:\n\
                     \x20 backend: openai\n");
        assert_eq!(m.kept, ["base_locale", "data", "search"]);
        assert_eq!(m.erb_lines, [1]);
        assert!(m.manual.is_empty());
        let dropped: Vec<&str> = m.dropped.iter().map(|d| d.key.as_str()).collect();
        assert_eq!(
            dropped,
            [
                "internal_locale",
                "data.yaml",
                "search.prism",
                "search.strict",
                "translation"
            ]
        );
        assert!(!m.output.contains("line_width"), "{}", m.output);
        assert!(!m.output.contains("openai"), "{}", m.output);
        assert!(m.output.contains("- config/locales/%{locale}.yml"));
        assert!(m.output.contains("- app/"));
    }

    #[test]
    fn keeps_the_comments_above_a_kept_key() {
        let m = run("base_locale: de\n\
                     \n\
                     # Rendered from a status enum.\n\
                     ignore_unused:\n\
                     \x20 - \"jobs.status.*\" # one per enum value\n");
        assert!(m.output.contains("# Rendered from a status enum."));
        assert!(m.output.contains("# one per enum value"));
    }

    #[test]
    fn comments_above_a_dropped_key_leave_with_it() {
        let m = run("base_locale: de\n\
                     \n\
                     ## Translation Services\n\
                     translation:\n\
                     \x20 backend: openai\n\
                     \n\
                     ignore: [\"a.*\"]\n");
        assert!(!m.output.contains("Translation Services"), "{}", m.output);
        assert!(m.output.contains("ignore: [\"a.*\"]"));
    }

    #[test]
    fn a_computed_value_is_reported_not_guessed() {
        let m = run("base_locale: de\n\
                     data:\n\
                     \x20 external:\n\
                     \x20   - \"<%= gem_path %>/locales/%{locale}.yml\"\n\
                     \x20 read:\n\
                     \x20   - config/locales/%{locale}.yml\n");
        assert!(m.needs_attention());
        assert_eq!(m.manual.len(), 1);
        assert_eq!(m.manual[0].line, 4);
        assert!(m.manual[0].text.contains("gem_path"));
        assert!(m.output.contains("NEEDS ATTENTION"));
        assert!(m.output.contains("[ERB]"), "{}", m.output);
        // The `external:` left behind by the removed line must not survive as a
        // list holding one blank path.
        let cfg =
            Config::parse(&m.output, Path::new(MIGRATION_TARGET), PathBuf::from(".")).unwrap();
        assert!(cfg.data.external.is_empty());
        assert_eq!(cfg.data.read, ["config/locales/%{locale}.yml"]);
    }

    #[test]
    fn a_key_with_no_value_is_dropped() {
        let m = run("base_locale: de\n\
                     data:\n\
                     \x20 read:\n\
                     \x20   - config/locales/%{locale}.yml\n\
                     \x20 external:\n\
                     \x20 ## Example:\n\
                     \x20 # - vendor/locales/%{locale}.yml\n");
        assert!(m.dropped.iter().any(|d| d.key == "data.external"));
        let cfg =
            Config::parse(&m.output, Path::new(MIGRATION_TARGET), PathBuf::from(".")).unwrap();
        assert!(cfg.data.external.is_empty());
        // The commented-out example that belonged to it goes too.
        assert!(!m.output.contains("vendor/locales"), "{}", m.output);
    }

    #[test]
    fn a_section_whose_every_child_is_dropped_goes_too() {
        let m = run("base_locale: de\nsearch:\n  scanners: [A]\n  strict: true\n");
        assert_eq!(m.kept, ["base_locale"]);
        assert!(m.dropped.iter().any(|d| d.key == "search"));
        assert!(!m.output.contains("search:"), "{}", m.output);
    }

    #[test]
    fn an_unknown_router_falls_back_to_the_default() {
        let m = run("base_locale: de\ndata:\n  router: isolating_router\n  keep_order: true\n");
        let d = m.dropped.iter().find(|d| d.key == "data.router").unwrap();
        assert!(d.reason.contains("isolating_router"), "{}", d.reason);
        assert!(m.output.contains("keep_order: true"));
    }

    #[test]
    fn a_missing_base_locale_is_written_out() {
        let m = run("locales: [de, en]\n");
        assert!(m.base_locale_defaulted);
        assert!(m.output.contains("base_locale: en"));
        let cfg =
            Config::parse(&m.output, Path::new(MIGRATION_TARGET), PathBuf::from(".")).unwrap();
        assert_eq!(cfg.base_locale, "en");
    }

    #[test]
    fn per_locale_ignore_hashes_survive() {
        let m = run("base_locale: de\n\
                     ignore_missing:\n\
                     \x20 all:\n\
                     \x20   - \"a.*\"\n\
                     \x20 \"fr,es\":\n\
                     \x20   - \"b.*\"\n");
        let cfg =
            Config::parse(&m.output, Path::new(MIGRATION_TARGET), PathBuf::from(".")).unwrap();
        let fr = cfg.ignore_patterns(IgnoreType::Missing, Some("fr"));
        assert!(fr.is_match("a.x") && fr.is_match("b.x"));
        let de = cfg.ignore_patterns(IgnoreType::Missing, Some("de"));
        assert!(de.is_match("a.x") && !de.is_match("b.x"));
    }

    #[test]
    fn a_multi_line_erb_block_is_removed_whole() {
        let m = run("<% I18n::Tasks.add_scanner 'X',\n\
                     \x20     only: %w(*.slim) %>\n\
                     base_locale: de\n");
        assert_eq!(m.erb_lines, [1, 2]);
        assert!(m.manual.is_empty());
        assert!(m.output.contains("base_locale: de"));
    }

    #[test]
    fn erb_inside_a_comment_is_removed() {
        let m = run("base_locale: de\n# <%# I18n::Tasks.add_scanner 'X' %>\nignore: [\"a.*\"]\n");
        assert!(!m.output.contains("<%"), "{}", m.output);
        assert!(m.output.contains("ignore: [\"a.*\"]"));
    }

    #[test]
    fn a_flow_style_section_is_refused_with_advice() {
        let e = migrate(
            "base_locale: de\nsearch: {paths: [app/], strict: true}\n",
            Path::new("config/i18n-tasks.yml"),
            Path::new(MIGRATION_TARGET),
        )
        .unwrap_err();
        assert!(e.contains("flow style"), "{e}");
    }

    /// Every nested setting the gem has and this tool does not gets its own
    /// reason, so a reader of the header knows why it went.
    #[test]
    fn every_dropped_nested_setting_names_its_reason() {
        let m = run("base_locale: de\n\
             data:\n\
             \x20 read:\n\
             \x20   - config/locales/%{locale}.yml\n\
             \x20 adapter: json\n\
             \x20 json:\n\
             \x20   write:\n\
             \x20     indent: 2\n\
             \x20 something_else: 1\n\
             search:\n\
             \x20 paths: [app/]\n\
             \x20 ast_matchers:\n\
             \x20   - I18n::Tasks::Scanners::AstMatchers::RailsModelMatcher\n\
             \x20 unheard_of: 1\n");
        let reason = |key: &str| {
            m.dropped
                .iter()
                .find(|d| d.key == key)
                .map(|d| d.reason.clone())
                .unwrap_or_else(|| panic!("{key} was not dropped: {:?}", m.dropped))
        };
        assert!(reason("data.adapter").contains("YAML file system"));
        assert!(reason("data.json").contains("JSON locale files"));
        assert!(reason("data.something_else").contains("no such setting"));
        assert!(reason("search.ast_matchers").contains("always on"));
        assert!(reason("search.unheard_of").contains("no such setting"));
        // What survived is still a config this tool reads.
        assert!(m.output.contains("read:"));
        assert!(m.output.contains("paths:"));
    }

    /// A `data.router` that is not a name at all cannot be checked against the
    /// two supported routers, so it is dropped with its own reason.
    #[test]
    fn a_router_that_is_not_a_name_is_dropped() {
        let m = run("base_locale: de\n\
             data:\n\
             \x20 read:\n\
             \x20   - config/locales/%{locale}.yml\n\
             \x20 router:\n\
             \x20   - conservative_router\n");
        let d = m
            .dropped
            .iter()
            .find(|d| d.key == "data.router")
            .expect("dropped");
        assert!(d.reason.contains("must be a name"), "{}", d.reason);
        // The setting itself is gone; only the header still names it.
        assert!(!m.output.contains("  router:"), "{}", m.output);
        assert!(m.output.contains("read:"), "{}", m.output);
    }

    /// An `<% ... %>` tag that is never closed swallows the rest of the file
    /// rather than leaving half a tag in the output.
    #[test]
    fn an_unterminated_erb_tag_is_removed_to_the_end() {
        // In a comment, where the whole comment line goes.
        let m = run("base_locale: de\nlocales: [de]\n# <% never closed\n");
        assert!(!m.output.contains("<%"), "{}", m.output);
        assert!(m.output.contains("base_locale: de"), "{}", m.output);
        // In a value, where the tag is replaced and the rest of the line with
        // it, because there is no `%>` to stop at.
        let m = run("base_locale: de\nlocales: [de]\nignore_unused:\n  - \"<%= never closed\"\n");
        assert!(!m.output.contains("<%"), "{}", m.output);
        assert!(m.output.contains("[ERB]"), "{}", m.output);
        assert!(m.needs_attention());
    }

    /// A source that holds nothing but comments once the ERB is gone is an
    /// error, not an empty config.
    #[test]
    fn a_source_with_no_settings_left_is_an_error() {
        let e = migrate(
            "<% require 'x' %>\n# just a comment\n",
            Path::new("config/i18n-tasks.yml.erb"),
            Path::new(MIGRATION_TARGET),
        )
        .unwrap_err();
        assert!(e.contains("no settings found"), "{e}");
    }

    /// The top level and each nested section have to be mappings.
    #[test]
    fn a_source_of_the_wrong_shape_is_an_error() {
        let e = migrate(
            "- a\n- b\n",
            Path::new("config/i18n-tasks.yml"),
            Path::new(MIGRATION_TARGET),
        )
        .unwrap_err();
        assert!(e.contains("config must be a YAML mapping"), "{e}");
        let e = migrate(
            "base_locale: de\ndata: config/locales\n",
            Path::new("config/i18n-tasks.yml"),
            Path::new(MIGRATION_TARGET),
        )
        .unwrap_err();
        assert!(e.contains("`data` must be a mapping"), "{e}");
    }

    /// The header is what a human reads after the command runs, so every part
    /// of it is worth asserting.
    #[test]
    fn the_report_text_names_what_happened() {
        let m = run("<% require 'x' %>\n\
             locales: [de]\n\
             internal_locale: ru\n\
             data:\n\
             \x20 read:\n\
             \x20   - config/locales/%{locale}.yml\n");
        let text = to_text(
            &m,
            Path::new("config/i18n-tasks.yml.erb"),
            Path::new(MIGRATION_TARGET),
            false,
        );
        assert!(text.contains("added base_locale: en"), "{text}");
        assert!(text.contains("removed ERB on 1 line(s)"), "{text}");
        assert!(text.contains("internal_locale"), "{text}");
        // Nothing needs a human here.
        assert!(!m.needs_attention());
        // With `write` set the header says where it went instead.
        let written = to_text(
            &m,
            Path::new("config/i18n-tasks.yml.erb"),
            Path::new(MIGRATION_TARGET),
            true,
        );
        assert!(written.contains(MIGRATION_TARGET), "{written}");
    }

    #[test]
    fn the_output_never_holds_erb() {
        let m = run("base_locale: de\ndata:\n  read:\n    - config/locales/%{locale}.yml\n");
        assert!(!m.output.contains("<%"));
    }
}
