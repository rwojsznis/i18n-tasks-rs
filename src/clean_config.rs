//! Finds ignore rules that suppress no current report row.

use crate::config::{Config, IgnoreSpec};
use crate::data::load::Store;
use crate::pattern::PatternSet;
use crate::report::{eq_base, interpolations, missing, unused};
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

    let stale: Vec<&Rule> = rules
        .iter()
        .filter(|rule| {
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
        .collect();
    let mut stale_lines: BTreeSet<usize> = stale.iter().map(|rule| rule.line).collect();
    let stale_rule_lines: BTreeSet<usize> = stale.iter().map(|rule| rule.line).collect();
    for rule in &stale {
        for &container_line in &rule.container_lines {
            let has_kept_rule = rules.iter().any(|candidate| {
                candidate.container_lines.contains(&container_line)
                    && !stale_rule_lines.contains(&candidate.line)
            });
            if !has_kept_rule {
                stale_lines.insert(container_line);
            }
        }
    }
    let cleaned = remove_lines(source, &stale_lines);
    let stale_rules = stale
        .into_iter()
        .map(|rule| StaleRule {
            setting: rule.setting.clone(),
            pattern: rule.pattern.clone(),
            line: rule.line,
        })
        .collect();
    Ok(CleanConfigReport {
        stale_rules,
        cleaned,
        original: source.to_string(),
        display_path: path.display().to_string(),
    })
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

    pub fn diff(&self) -> String {
        if self.is_clean() {
            return "Config ignore rules are clean\n".to_string();
        }
        similar::TextDiff::from_lines(&self.original, &self.cleaned)
            .unified_diff()
            .header(&self.display_path, &self.display_path)
            .to_string()
    }
}
