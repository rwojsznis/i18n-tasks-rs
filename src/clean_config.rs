//! Finds ignore rules that suppress no current report row.

use crate::config::{Config, IgnoreSpec};
use crate::data::load::Store;
use crate::pattern::PatternSet;
use crate::report::{eq_base, interpolations, missing, unused};
use crate::session::Session;
use crate::used::UsedKeys;
use crate::yaml::{self, Node};
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::Path;

#[derive(Debug, Serialize)]
pub struct CleanConfigReport {
    pub stale_rules: Vec<StaleRule>,
    #[serde(skip)]
    pub cleaned: String,
    #[serde(skip)]
    original: String,
    #[serde(skip)]
    display_path: String,
}

#[derive(Debug, Serialize)]
pub struct StaleRule {
    pub setting: String,
    pub pattern: String,
    pub line: usize,
    /// True when the write cannot remove this rule: it shares its line with a
    /// rule that stays, so only a human can take it out. See `removal_plan`.
    pub manual: bool,
}

#[derive(Clone, Copy)]
enum Kind {
    Global,
    Missing,
    Unused,
    EqBase,
    Interpolations,
}

struct Rule {
    setting: String,
    pattern: String,
    line: usize,
    locales: Option<Vec<String>>,
    kind: Kind,
    container_lines: Vec<usize>,
}

/// Build a cleanup plan without changing the config file.
///
/// # Errors
///
/// Returns an error when the config source cannot be parsed.
pub fn plan(
    cfg: &Config,
    store: &Store,
    used: &UsedKeys,
    locales: &[String],
    source: &str,
    path: &Path,
) -> Result<CleanConfigReport, String> {
    let rules = parse_rules(source, path)?;
    let mut bare = cfg.clone();
    bare.ignore.clear();
    bare.ignore_missing = IgnoreSpec::Empty;
    bare.ignore_unused = IgnoreSpec::Empty;
    bare.ignore_eq_base = IgnoreSpec::Empty;
    bare.ignore_inconsistent_interpolations = IgnoreSpec::Empty;

    let missing_rows =
        missing::report(&bare, store, used, locales, &missing::MissingType::ALL).rows;
    let unused_rows = unused::report(&bare, store, used, locales).rows;
    let eq_rows = eq_base::report(&bare, store, locales).rows;
    let interpolation_rows = interpolations::inconsistent(&bare, store, locales).rows;

    let stale: Vec<usize> = rules
        .iter()
        .enumerate()
        .filter(|(_, rule)| {
            let pattern = PatternSet::new(std::slice::from_ref(&rule.pattern));
            let matches = |rows: &[crate::report::KeyRow]| {
                rows.iter().any(|row| {
                    locale_applies(rule.locales.as_deref(), &row.locale)
                        && pattern.is_match(&row.key)
                })
            };
            !match rule.kind {
                Kind::Global => {
                    matches(&missing_rows)
                        || matches(&unused_rows)
                        || matches(&eq_rows)
                        || matches(&interpolation_rows)
                }
                Kind::Missing => matches(&missing_rows),
                Kind::Unused => matches(&unused_rows),
                Kind::EqBase => matches(&eq_rows),
                Kind::Interpolations => matches(&interpolation_rows),
            }
        })
        .map(|(index, _)| index)
        .collect();
    let (stale_lines, manual) = removal_plan(&rules, &stale);
    let cleaned = remove_lines(source, &stale_lines);
    let stale_rules = stale
        .iter()
        .zip(&manual)
        .filter_map(|(&index, &manual)| {
            let rule = rules.get(index)?;
            Some(StaleRule {
                setting: rule.setting.clone(),
                pattern: rule.pattern.clone(),
                line: rule.line,
                manual,
            })
        })
        .collect();
    Ok(CleanConfigReport {
        stale_rules,
        cleaned,
        original: source.to_string(),
        display_path: path.display().to_string(),
    })
}

/// Turn the stale rules into the set of lines the write removes.
///
/// A rule's identity is its index, not its line. A flow-style list holds
/// several rules on one line (`ignore_unused: ["bye", "zzz.*"]`), so removing
/// the line of a stale rule takes its live neighbours with it — with `--write`
/// the live rule is gone from the config. A stale rule that shares its line
/// with a rule that stays is therefore `manual`: the line stays, and the report
/// asks a human to remove the rule. `migrate-config` refuses a flow-style
/// section for the same reason.
///
/// Returns the lines to remove, and one `manual` flag per entry of `stale`.
fn removal_plan(rules: &[Rule], stale: &[usize]) -> (BTreeSet<usize>, Vec<bool>) {
    let stale_set: BTreeSet<usize> = stale.iter().copied().collect();
    let kept_lines: BTreeSet<usize> = rules
        .iter()
        .enumerate()
        .filter(|(index, _)| !stale_set.contains(index))
        .map(|(_, rule)| rule.line)
        .collect();
    let manual: Vec<bool> = stale
        .iter()
        .map(|index| {
            rules
                .get(*index)
                .is_some_and(|rule| kept_lines.contains(&rule.line))
        })
        .collect();
    let removed: BTreeSet<usize> = stale
        .iter()
        .zip(&manual)
        .filter(|(_, manual)| !**manual)
        .map(|(&index, _)| index)
        .collect();
    let mut lines: BTreeSet<usize> = removed
        .iter()
        .filter_map(|index| rules.get(*index))
        .map(|rule| rule.line)
        .collect();
    for rule in removed.iter().filter_map(|index| rules.get(*index)) {
        for &container_line in &rule.container_lines {
            let has_kept_rule = rules.iter().enumerate().any(|(index, candidate)| {
                candidate.container_lines.contains(&container_line) && !removed.contains(&index)
            });
            if !has_kept_rule {
                lines.insert(container_line);
            }
        }
    }
    (lines, manual)
}

fn locale_applies(group: Option<&[String]>, locale: &str) -> bool {
    group.is_none_or(|locales| locales.iter().any(|l| l == "all" || l == locale))
}

fn parse_rules(source: &str, path: &Path) -> Result<Vec<Rule>, String> {
    let root = yaml::parse(source, path)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("{}: config is empty", path.display()))?;
    let mut rules = Vec::new();
    let Some(entries) = root.as_map() else {
        return Ok(rules);
    };
    for (key, value) in entries {
        let Some(setting) = key.as_str() else {
            continue;
        };
        let kind = match setting {
            "ignore" => Kind::Global,
            "ignore_missing" => Kind::Missing,
            "ignore_unused" => Kind::Unused,
            "ignore_eq_base" => Kind::EqBase,
            "ignore_inconsistent_interpolations" => Kind::Interpolations,
            _ => continue,
        };
        collect_rules(&mut rules, setting, kind, None, value, vec![key.line()]);
    }
    Ok(rules)
}

fn collect_rules(
    out: &mut Vec<Rule>,
    setting: &str,
    kind: Kind,
    locales: Option<Vec<String>>,
    node: &Node,
    container_lines: Vec<usize>,
) {
    match node {
        Node::Scalar { value, line, .. } => out.push(Rule {
            setting: setting.to_string(),
            pattern: value.clone(),
            line: *line,
            locales,
            kind,
            container_lines,
        }),
        Node::Seq { items, .. } => {
            for item in items {
                collect_rules(
                    out,
                    setting,
                    kind,
                    locales.clone(),
                    item,
                    container_lines.clone(),
                );
            }
        }
        Node::Map { entries, .. } => {
            for (group, values) in entries {
                let group_locales = group.as_str().map(|raw| {
                    raw.split(',')
                        .map(str::trim)
                        .filter(|locale| !locale.is_empty())
                        .map(str::to_string)
                        .collect()
                });
                let mut group_containers = container_lines.clone();
                group_containers.push(group.line());
                collect_rules(out, setting, kind, group_locales, values, group_containers);
            }
        }
    }
}

/// Drops the given lines, and the comment block directly above each one.
///
/// A comment at the rule's own indent documents that rule, the same convention
/// `migrate::lines` follows, so leaving it behind would strand a note about a
/// pattern that is no longer there.
fn remove_lines(source: &str, stale_lines: &BTreeSet<usize>) -> String {
    let lines: Vec<&str> = source.split_inclusive('\n').collect();
    let mut remove = stale_lines.clone();
    for &line_num in stale_lines {
        let Some(line) = lines.get(line_num.saturating_sub(1)) else {
            continue;
        };
        let indent = line.len() - line.trim_start().len();
        let mut previous = line_num.saturating_sub(1);
        while previous > 0 {
            let candidate = lines[previous - 1];
            let trimmed = candidate.trim_start();
            let candidate_indent = candidate.len() - trimmed.len();
            if candidate_indent != indent || !trimmed.starts_with('#') {
                break;
            }
            remove.insert(previous);
            previous -= 1;
        }
    }
    lines
        .into_iter()
        .enumerate()
        .filter(|(index, _)| !remove.contains(&(index + 1)))
        .map(|(_, line)| line)
        .collect()
}

impl CleanConfigReport {
    pub fn is_clean(&self) -> bool {
        self.stale_rules.is_empty()
    }

    /// True when `--write` changes the file. False when every stale rule is
    /// `manual`, because then the cleaned text is the original text.
    pub fn has_edit(&self) -> bool {
        self.cleaned != self.original
    }

    pub fn has_manual(&self) -> bool {
        self.stale_rules.iter().any(|rule| rule.manual)
    }

    /// The stale rules the write leaves in place, and what to do about them.
    /// Empty when there are none.
    pub fn manual_note(&self) -> String {
        let manual: Vec<&StaleRule> = self.stale_rules.iter().filter(|rule| rule.manual).collect();
        if manual.is_empty() {
            return String::new();
        }
        let (noun, verb, pronoun) = if manual.len() == 1 {
            ("rule", "shares", "it")
        } else {
            ("rules", "share", "them")
        };
        let mut note = format!(
            "{} stale {noun} {verb} a line with a rule that is still in use \
             (flow style, `[a, b]`):\n",
            manual.len(),
        );
        for rule in manual {
            note.push_str(&format!(
                "  {}:{}: `{}` rule `{}`\n",
                self.display_path, rule.line, rule.setting, rule.pattern
            ));
        }
        note.push_str(&format!(
            "Remove {pronoun} by hand, or rewrite the list as an indented block \
             and run this again.\n"
        ));
        note
    }

    /// The `clean-config` envelope. `written` follows the file, not the
    /// finding: a run that leaves only manual rules writes nothing.
    ///
    /// # Errors
    ///
    /// The stale-rule list does not serialize.
    pub fn to_json(&self, session: &Session, written: bool) -> Result<String, String> {
        serde_json::to_string_pretty(&serde_json::json!({
            "check": "clean_config",
            "written": written,
            "config_digest": session.cfg.digest,
            "locales": session.locales,
            "stale_rules": self.stale_rules,
        }))
        .map_err(|e| e.to_string())
    }

    pub fn diff(&self) -> String {
        if self.is_clean() {
            return "Config ignore rules are clean\n".to_string();
        }
        if !self.has_edit() {
            return String::new();
        }
        similar::TextDiff::from_lines(&self.original, &self.cleaned)
            .unified_diff()
            .header(&self.display_path, &self.display_path)
            .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::{Kind, Rule, removal_plan};
    use std::collections::BTreeSet;

    fn rule(pattern: &str, line: usize, container_lines: &[usize]) -> Rule {
        Rule {
            setting: "ignore_unused".to_string(),
            pattern: pattern.to_string(),
            line,
            locales: None,
            kind: Kind::Unused,
            container_lines: container_lines.to_vec(),
        }
    }

    /// `ignore_unused:` on line 1, `- bye` on 2 and `- zzz.*` on 3.
    #[test]
    fn a_block_list_removes_the_line_of_the_stale_rule_only() {
        let rules = [rule("bye", 2, &[1]), rule("zzz.*", 3, &[1])];
        let (lines, manual) = removal_plan(&rules, &[1]);
        assert_eq!(lines, BTreeSet::from([3]));
        assert_eq!(manual, vec![false]);
    }

    /// The setting itself goes once its last rule does.
    #[test]
    fn a_block_list_of_only_stale_rules_takes_its_setting_with_it() {
        let rules = [rule("yyy.*", 2, &[1]), rule("zzz.*", 3, &[1])];
        let (lines, manual) = removal_plan(&rules, &[0, 1]);
        assert_eq!(lines, BTreeSet::from([1, 2, 3]));
        assert_eq!(manual, vec![false, false]);
    }

    /// `ignore_unused: ["bye", "zzz.*"]` — both rules and the setting are on
    /// line 1, so removing the stale rule's line would delete the live `bye`.
    #[test]
    fn a_flow_list_with_a_live_rule_removes_nothing() {
        let rules = [rule("bye", 1, &[1]), rule("zzz.*", 1, &[1])];
        let (lines, manual) = removal_plan(&rules, &[1]);
        assert!(lines.is_empty(), "{lines:?}");
        assert_eq!(manual, vec![true]);
    }

    /// Nothing survives on the line, so the line goes as a whole.
    #[test]
    fn a_flow_list_of_only_stale_rules_is_removed() {
        let rules = [rule("yyy.*", 1, &[1]), rule("zzz.*", 1, &[1])];
        let (lines, manual) = removal_plan(&rules, &[0, 1]);
        assert_eq!(lines, BTreeSet::from([1]));
        assert_eq!(manual, vec![false, false]);
    }

    /// A locale group keeps its header while one of its rules stays.
    #[test]
    fn a_locale_group_keeps_its_header_while_a_rule_stays() {
        let rules = [rule("bye", 3, &[1, 2]), rule("zzz.*", 4, &[1, 2])];
        let (lines, _) = removal_plan(&rules, &[1]);
        assert_eq!(lines, BTreeSet::from([4]));
    }
}
