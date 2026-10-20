//! `translate-missing`: the base locale's values for the keys a locale lacks,
//! translated and written. Accepted diff 34.
//!
//! ref: lib/i18n/tasks/command/commands/missing.rb:40-57
//! ref: lib/i18n/tasks/translators/base_translator.rb

pub mod language_names;
pub mod openai;

use crate::config::{Config, DataConfig, IgnoreType, Router};
use crate::data::load::{Leaf, Store, Value};
use crate::data::route::{self, ConservativeRouter, PatternRouter};
use crate::keys::parent_key;
use crate::report::interpolations::variables;
use crate::report::missing::diff_leaves;
use crate::report::normalize::{self, NormalizeReport};
use crate::report::{KeyRow, render_table};
use crate::session::Session;
use regex::Regex;
use std::collections::{BTreeSet, HashSet};
use std::sync::{Arc, LazyLock};

/// ref: openai_translator.rb:11
pub const BATCH_SIZE: usize = 50;

#[allow(
    clippy::expect_used,
    reason = "a static pattern that fails to compile is a bug here, not a run-time condition"
)]
static INTERPOLATION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"%\{[^}]+\}").expect("static pattern compiles"));

#[allow(
    clippy::expect_used,
    reason = "a static pattern that fails to compile is a bug here, not a run-time condition"
)]
static PLACEHOLDER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"X__(\d+)").expect("static pattern compiles"));

#[allow(
    clippy::expect_used,
    reason = "a static pattern that fails to compile is a bug here, not a run-time condition"
)]
static DIGIT_AFTER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"%\{[^}]+\}\d").expect("static pattern compiles"));

#[derive(Debug, Clone)]
pub struct LocaleLeaves {
    pub locale: String,
    pub leaves: Vec<Leaf>,
}

#[derive(Debug)]
pub struct Failure {
    pub keys: Vec<String>,
    pub error: String,
}

#[derive(Debug, Default)]
pub struct Translated {
    pub leaves: Vec<Leaf>,
    pub failures: Vec<Failure>,
}

/// The base leaves each locale lacks, every plural form included. The other
/// direction of `missing_diff_forest`, keys the base lacks, is not translated:
/// accepted diff 34.
pub fn pending(cfg: &Config, store: &Store, locales: &[String]) -> Vec<LocaleLeaves> {
    let Some(source) = store.tree(&store.base_locale) else {
        return Vec::new();
    };
    locales
        .iter()
        .map(|locale| {
            let ignore = cfg.ignore_patterns(IgnoreType::Missing, Some(locale));
            LocaleLeaves {
                locale: locale.clone(),
                leaves: diff_leaves(store, source, &ignore, locale)
                    .map(|(_, leaf)| leaf.clone())
                    .collect(),
            }
        })
        .filter(|p| !p.leaves.is_empty())
        .collect()
}

/// ref: base_translator.rb#translate_pairs
pub fn translate(
    leaves: &[Leaf],
    is_plural: impl Fn(&str) -> bool,
    mut call: impl FnMut(&[String]) -> Result<Vec<String>, String>,
) -> Translated {
    let mut done: Vec<(usize, Leaf)> = Vec::with_capacity(leaves.len());
    let mut failures = Vec::new();
    let mut queued = Vec::new();
    for (i, leaf) in leaves.iter().enumerate() {
        if has_ambiguous_placeholder(&leaf.value) {
            failures.push(Failure {
                keys: vec![leaf.key.clone()],
                error: "the source holds an interpolation that its `X__n` placeholder \
                        cannot restore unambiguously: a literal `X__n`, or a digit \
                        right after it"
                    .to_string(),
            });
            continue;
        }
        let mut strings = Vec::new();
        dump(&leaf.value, &mut strings);
        if strings.is_empty() {
            done.push((i, leaf.clone()));
        } else {
            queued.push((i, strings));
        }
    }
    for batch in batches(queued) {
        let sent: Vec<String> = batch.iter().flat_map(|(_, s)| s.iter().cloned()).collect();
        let keys = |batch: &[(usize, Vec<String>)]| {
            batch.iter().map(|(i, _)| leaves[*i].key.clone()).collect()
        };
        let received = match call(&sent) {
            Ok(r) if r.len() == sent.len() => r,
            Ok(r) => {
                failures.push(Failure {
                    keys: keys(&batch),
                    error: format!(
                        "sent {} strings, received {} translations",
                        sent.len(),
                        r.len()
                    ),
                });
                continue;
            }
            Err(error) => {
                failures.push(Failure {
                    keys: keys(&batch),
                    error,
                });
                continue;
            }
        };
        let mut received = received.into_iter();
        for (i, strings) in &batch {
            let leaf = &leaves[*i];
            let own: Vec<String> = received.by_ref().take(strings.len()).collect();
            match implode(&leaf.value, &mut own.into_iter()) {
                Ok(value) => done.push((
                    *i,
                    Leaf {
                        value,
                        ..leaf.clone()
                    },
                )),
                Err(error) => failures.push(Failure {
                    keys: vec![leaf.key.clone()],
                    error,
                }),
            }
        }
    }
    let broken: HashSet<&str> = failures
        .iter()
        .flat_map(|f| &f.keys)
        .filter_map(|key| parent_key(key).filter(|p| is_plural(p)))
        .collect();
    let (siblings, mut done): (Vec<_>, Vec<_>) = done
        .into_iter()
        .partition(|(_, leaf)| parent_key(&leaf.key).is_some_and(|p| broken.contains(p)));
    if !siblings.is_empty() {
        failures.push(Failure {
            keys: siblings.into_iter().map(|(_, leaf)| leaf.key).collect(),
            error: "another form of the same plural key failed".to_string(),
        });
    }
    done.sort_by_key(|(i, _)| *i);
    Translated {
        leaves: done.into_iter().map(|(_, leaf)| leaf).collect(),
        failures,
    }
}

/// Consecutive keys up to `BATCH_SIZE` strings a batch. A key is never split,
/// so one that holds more strings goes alone.
fn batches(queued: Vec<(usize, Vec<String>)>) -> Vec<Vec<(usize, Vec<String>)>> {
    let mut out: Vec<Vec<(usize, Vec<String>)>> = Vec::new();
    let mut size = 0;
    for (i, strings) in queued {
        if out.is_empty() || size + strings.len() > BATCH_SIZE {
            out.push(Vec::new());
            size = 0;
        }
        size += strings.len();
        if let Some(batch) = out.last_mut() {
            batch.push((i, strings));
        }
    }
    out
}

fn dump(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Str(s) if !s.is_empty() => out.push(protect(s)),
        Value::Seq(items) => items.iter().for_each(|v| dump(v, out)),
        Value::Map(entries) => entries.iter().for_each(|(_, v)| dump(v, out)),
        _ => {}
    }
}

fn implode(value: &Value, translated: &mut impl Iterator<Item = String>) -> Result<Value, String> {
    Ok(match value {
        Value::Str(s) if !s.is_empty() => {
            let text = translated
                .next()
                .ok_or_else(|| "fewer translations than strings".to_string())?;
            Value::Str(restore(s, &text)?)
        }
        Value::Seq(items) => Value::Seq(
            items
                .iter()
                .map(|v| implode(v, translated))
                .collect::<Result<_, _>>()?,
        ),
        Value::Map(entries) => Value::Map(
            entries
                .iter()
                .map(|(k, v)| Ok((k.clone(), implode(v, translated)?)))
                .collect::<Result<_, String>>()?,
        ),
        other => other.clone(),
    })
}

fn has_ambiguous_placeholder(value: &Value) -> bool {
    match value {
        Value::Str(s) => {
            INTERPOLATION.is_match(s) && (PLACEHOLDER.is_match(s) || DIGIT_AFTER.is_match(s))
        }
        Value::Seq(items) => items.iter().any(has_ambiguous_placeholder),
        Value::Map(entries) => entries.iter().any(|(_, v)| has_ambiguous_placeholder(v)),
        _ => false,
    }
}

fn protect(value: &str) -> String {
    let mut i = 0;
    INTERPOLATION
        .replace_all(value, |_: &regex::Captures<'_>| {
            let placeholder = format!("X__{i}");
            i += 1;
            placeholder
        })
        .into_owned()
}

/// ref: base_translator.rb#restore_interpolations. The variable check is ours:
/// accepted diff 34.
fn restore(source: &str, translated: &str) -> Result<String, String> {
    let originals: Vec<&str> = INTERPOLATION
        .find_iter(source)
        .map(|m| m.as_str())
        .collect();
    let restored = if originals.is_empty() {
        translated.to_string()
    } else {
        let mut unknown = None;
        let out = PLACEHOLDER.replace_all(translated, |c: &regex::Captures<'_>| {
            match c[1].parse::<usize>().ok().and_then(|i| originals.get(i)) {
                Some(original) => (*original).to_string(),
                None => {
                    unknown = Some(c[0].to_string());
                    c[0].to_string()
                }
            }
        });
        if let Some(placeholder) = unknown {
            return Err(format!(
                "`{source}` came back as `{translated}`, with the unknown placeholder `{placeholder}`"
            ));
        }
        out.into_owned()
    };
    let expected: BTreeSet<&str> = variables(source).into_iter().collect();
    let got: BTreeSet<&str> = variables(&restored).into_iter().collect();
    if expected != got {
        return Err(format!(
            "`{source}` came back as `{restored}`, with other interpolation variables"
        ));
    }
    Ok(restored)
}

/// Adds the translations to `store` and plans the files that receive them.
/// The configured router places each new key; every existing key stays in
/// the file it is in. Nothing is written.
///
/// # Errors
///
/// A key cannot be routed, a locale has no data, or the write plan fails one
/// of its guards.
pub fn merge(
    cfg: &Config,
    store: &mut Store,
    translated: Vec<LocaleLeaves>,
) -> Result<NormalizeReport, String> {
    let pattern = PatternRouter::new(cfg);
    let mut routed = Vec::with_capacity(translated.len());
    {
        let conservative = ConservativeRouter::new(&pattern, store, &cfg.root);
        for LocaleLeaves { locale, leaves } in translated {
            if leaves.is_empty() {
                continue;
            }
            let mut out = Vec::with_capacity(leaves.len());
            for leaf in leaves {
                let path = if cfg.data.router == Router::Pattern {
                    pattern.route_key(&locale, &leaf.key)?
                } else {
                    conservative.route_key(&locale, &leaf.key)?
                };
                out.push(Leaf {
                    path: Arc::from(path.as_path()),
                    ..leaf
                });
            }
            routed.push((locale, out));
        }
    }
    let in_place = Config {
        data: DataConfig {
            router: Router::Conservative,
            ..cfg.data.clone()
        },
        ..cfg.clone()
    };
    let mut locales = Vec::with_capacity(routed.len());
    let mut touched = HashSet::new();
    for (locale, leaves) in routed {
        let keys: HashSet<String> = leaves.iter().map(|l| l.key.clone()).collect();
        let tree = store
            .tree(&locale)
            .ok_or_else(|| format!("locale `{locale}` has no data"))?
            .extended(leaves);
        store.trees.insert(locale.clone(), tree);
        for dest in route::route_filtered(&in_place, store, &locale, false, &|k| keys.contains(k))?
        {
            touched.insert(dest.path);
        }
        locales.push(locale);
    }
    normalize::plan_files(&in_place, store, &locales, false, &|_, _| true, &|path| {
        touched.contains(path)
    })
}

pub fn rows(pending: &[LocaleLeaves]) -> Vec<KeyRow> {
    pending
        .iter()
        .flat_map(|p| {
            p.leaves.iter().map(|leaf| KeyRow {
                locale: p.locale.clone(),
                key: leaf.key.clone(),
                value: Some(leaf.value.to_display_string()),
                reason: None,
            })
        })
        .collect()
}

pub fn to_text(rows: &[KeyRow]) -> String {
    let cells: Vec<Vec<String>> = rows
        .iter()
        .map(|r| {
            vec![
                r.locale.clone(),
                r.key.clone(),
                r.value.clone().unwrap_or_default(),
            ]
        })
        .collect();
    render_table("Translated keys", &["Locale", "Key", "Translation"], &cells)
}

/// # Errors
///
/// The rows do not serialize.
pub fn to_json(session: &Session, rows: &[KeyRow]) -> Result<String, String> {
    serde_json::to_string_pretty(&serde_json::json!({
        "check": "translate-missing",
        "config_digest": session.cfg.digest,
        "locales": session.locales,
        "keys": rows,
    }))
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn leaf(key: &str, value: Value) -> Leaf {
        Leaf {
            key: key.to_string(),
            value,
            depth: 1,
            path: Arc::from(Path::new("config/locales/en.yml")),
            odd_segments: None,
        }
    }

    fn s(v: &str) -> Value {
        Value::Str(v.to_string())
    }

    fn upcase(batch: &[String]) -> Vec<String> {
        batch.iter().map(|v| v.to_uppercase()).collect()
    }

    #[test]
    fn interpolations_travel_as_numbered_placeholders() {
        assert_eq!(protect("Hi %{name}, %{count} new"), "Hi X__0, X__1 new");
        assert_eq!(
            restore("Hi %{name}, %{count} new", "X__1 uutta, hei X__0").unwrap(),
            "%{count} uutta, hei %{name}"
        );
    }

    #[test]
    fn a_lost_or_unknown_placeholder_fails_the_value() {
        assert!(restore("Hi %{name}", "Hei").is_err());
        assert!(restore("Hi %{name}", "Hei X__3").is_err());
        assert!(restore("Hi", "Hei %{name}").is_err());
    }

    #[test]
    fn collections_are_translated_item_by_item_and_other_scalars_pass_through() {
        let leaves = [
            leaf("days", Value::Seq(vec![s("mon"), Value::Nil, s("tue")])),
            leaf("limit", Value::Plain("5".into())),
            leaf("empty", s("")),
            leaf("flag", Value::Bool(true)),
            leaf(
                "nested",
                Value::Seq(vec![Value::Map(vec![("a".into(), s("x"))])]),
            ),
        ];
        let mut calls = 0;
        let out = translate(
            &leaves,
            |_| false,
            |batch| {
                calls += 1;
                Ok(upcase(batch))
            },
        );
        assert_eq!(calls, 1);
        assert!(out.failures.is_empty());
        let values: Vec<&Value> = out.leaves.iter().map(|l| &l.value).collect();
        assert_eq!(
            values,
            [
                &Value::Seq(vec![s("MON"), Value::Nil, s("TUE")]),
                &Value::Plain("5".into()),
                &s(""),
                &Value::Bool(true),
                &Value::Seq(vec![Value::Map(vec![("a".into(), s("X"))])]),
            ]
        );
    }

    #[test]
    fn a_batch_fills_up_to_fifty_strings_without_splitting_a_key() {
        let mut leaves: Vec<Leaf> = (0..49).map(|i| leaf(&format!("k{i:02}"), s("v"))).collect();
        leaves.push(leaf("list", Value::Seq(vec![s("a"), s("b")])));
        leaves.push(leaf("last", s("v")));
        let mut sizes = Vec::new();
        let out = translate(
            &leaves,
            |_| false,
            |batch| {
                sizes.push(batch.len());
                Ok(upcase(batch))
            },
        );
        assert_eq!(sizes, [49, 3]);
        assert_eq!(out.leaves.len(), 51);
    }

    #[test]
    fn a_failed_batch_drops_only_its_own_keys() {
        let leaves: Vec<Leaf> = (0..60).map(|i| leaf(&format!("k{i:02}"), s("v"))).collect();
        let mut call = 0;
        let out = translate(
            &leaves,
            |_| false,
            |batch| {
                call += 1;
                if call == 1 {
                    Ok(vec!["short".into()])
                } else {
                    Ok(upcase(batch))
                }
            },
        );
        assert_eq!(out.failures.len(), 1);
        assert_eq!(out.failures[0].keys.len(), 50);
        assert!(
            out.failures[0].error.contains("received 1"),
            "{}",
            out.failures[0].error
        );
        assert_eq!(out.leaves.len(), 10);
        assert_eq!(out.leaves[0].key, "k50");
    }

    #[test]
    fn a_value_that_loses_a_variable_fails_alone() {
        let leaves = [leaf("good", s("Hi %{name}")), leaf("bad", s("Bye %{name}"))];
        let out = translate(
            &leaves,
            |_| false,
            |batch| Ok(batch.iter().map(|v| v.replace("Bye X__0", "Moi")).collect()),
        );
        assert_eq!(out.failures.len(), 1);
        assert_eq!(out.failures[0].keys, ["bad"]);
        assert_eq!(out.leaves.len(), 1);
        assert_eq!(out.leaves[0].value, s("Hi %{name}"));
    }

    #[test]
    fn a_source_with_a_literal_placeholder_and_a_variable_is_refused() {
        let leaves = [
            leaf("device", s("Device X__0 belongs to %{name}")),
            leaf("code", s("Code X__0")),
            leaf("tens", s("%{count}0 apples")),
        ];
        let mut sent = Vec::new();
        let out = translate(
            &leaves,
            |_| false,
            |batch| {
                sent.extend(batch.iter().cloned());
                Ok(batch.to_vec())
            },
        );
        assert_eq!(sent, ["Code X__0"]);
        let failed: Vec<&str> = out
            .failures
            .iter()
            .flat_map(|f| f.keys.iter().map(String::as_str))
            .collect();
        assert_eq!(failed, ["device", "tens"]);
        assert_eq!(out.leaves.len(), 1);
        assert_eq!(out.leaves[0].value, s("Code X__0"));
    }

    #[test]
    fn a_failed_collection_does_not_shift_the_next_keys_translation() {
        let leaves = [
            leaf("list", Value::Seq(vec![s("Hi %{name}"), s("Bye")])),
            leaf("after", s("Later")),
        ];
        let out = translate(
            &leaves,
            |_| false,
            |batch| {
                Ok(batch
                    .iter()
                    .map(|v| v.replace("Hi X__0", "Moi").to_uppercase())
                    .collect())
            },
        );
        assert_eq!(out.failures.len(), 1);
        assert_eq!(out.failures[0].keys, ["list"]);
        assert_eq!(out.leaves.len(), 1);
        assert_eq!(out.leaves[0].key, "after");
        assert_eq!(out.leaves[0].value, s("LATER"));
    }

    #[test]
    fn a_plural_form_that_fails_takes_its_sibling_forms_with_it() {
        let leaves = [
            leaf("apples.one", s("One apple")),
            leaf("apples.other", s("%{count} apples")),
            leaf("pear", s("Pear")),
        ];
        let out = translate(
            &leaves,
            |key| key == "apples",
            |batch| Ok(batch.iter().map(|v| v.replace("X__0", "many")).collect()),
        );
        let failed: Vec<&str> = out
            .failures
            .iter()
            .flat_map(|f| f.keys.iter().map(String::as_str))
            .collect();
        assert_eq!(failed, ["apples.other", "apples.one"]);
        let written: Vec<&str> = out.leaves.iter().map(|l| l.key.as_str()).collect();
        assert_eq!(written, ["pear"]);
    }

    #[test]
    fn a_key_with_more_strings_than_a_batch_goes_alone_and_whole() {
        let long = Value::Seq((0..60).map(|i| s(&format!("v{i}"))).collect());
        let leaves = [leaf("short", s("v")), leaf("long", long)];
        let mut sizes = Vec::new();
        let out = translate(
            &leaves,
            |_| false,
            |batch| {
                sizes.push(batch.len());
                Ok(upcase(batch))
            },
        );
        assert_eq!(sizes, [1, 60]);
        assert_eq!(out.leaves.len(), 2);
    }

    #[test]
    fn pending_takes_every_plural_form_of_a_missing_key() {
        let root = std::env::temp_dir().join("i18n-tasks-rs-translate-pending");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("config/locales")).unwrap();
        std::fs::write(
            root.join("config/locales/en.yml"),
            "en:\n  apples:\n    one: One\n    other: Many\n  pears:\n    zero: None\n    one: One\n    other: Many\n  hi: Hi\n  skip:\n    me: x\n",
        )
        .unwrap();
        std::fs::write(
            root.join("config/locales/de.yml"),
            "de:\n  hi: Hallo\n  pears:\n    other: Viele\n",
        )
        .unwrap();
        let cfg = Config::parse(
            "base_locale: en\nlocales: [en, de]\ndata:\n  read:\n    - config/locales/%{locale}.yml\nignore_missing:\n  - skip.*\n",
            &root.join("config/i18n-tasks.yml"),
            root.clone(),
        )
        .unwrap();
        let store = Store::load(&cfg).unwrap();
        let pending = pending(&cfg, &store, &["en".into(), "de".into()]);
        let keys: Vec<(&str, &str)> = pending
            .iter()
            .flat_map(|p| p.leaves.iter().map(|l| (p.locale.as_str(), l.key.as_str())))
            .collect();
        assert_eq!(keys, [("de", "apples.one"), ("de", "apples.other")]);
        let _ = std::fs::remove_dir_all(&root);
    }
}
