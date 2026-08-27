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
//!
//! The three jobs above are the three modules here: `erb`, `decide` and
//! `render`, over the line ranges `lines` hands out.
//!
//! [`Config::parse`]: crate::config::Config::parse

mod decide;
mod erb;
mod lines;
mod render;

pub use render::to_text;

use crate::config::Config;
use crate::yaml;
use decide::{Decision, decide, nested};
use erb::strip_erb;
use lines::{extents, slice};
use render::render;
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
///
/// # Errors
///
/// The gem config does not parse as YAML once its ERB is stripped, is not a
/// mapping, or has a non-string key.
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
            let Some(d) = m.dropped.iter().find(|d| d.key == key) else {
                panic!("{key} was not dropped: {:?}", m.dropped)
            };
            d.reason.clone()
        };
        assert!(reason("data.adapter").contains("YAML file system"));
        assert!(reason("data.json").contains("JSON locale files"));
        assert!(reason("data.something_else").contains("no such setting"));
        assert!(reason("search.ast_matchers").contains("always on"));
        assert!(reason("search.unheard_of").contains("no such setting"));
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
        assert!(!m.output.contains("  router:"), "{}", m.output);
        assert!(m.output.contains("read:"), "{}", m.output);
    }

    /// An `<% ... %>` tag that is never closed swallows the rest of the file
    /// rather than leaving half a tag in the output.
    #[test]
    fn an_unterminated_erb_tag_is_removed_to_the_end() {
        let m = run("base_locale: de\nlocales: [de]\n# <% never closed\n");
        assert!(!m.output.contains("<%"), "{}", m.output);
        assert!(m.output.contains("base_locale: de"), "{}", m.output);
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
        assert!(!m.needs_attention());
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
