//! One check's result under the name the CLI reports it by, and the two `-f
//! json` envelopes built from it.
//!
//! The names are the CLI's, not the reports': `InterpolationReport` serves two
//! checks and `NormalizeReport` serves `check-normalized` as well as
//! `normalize`, so an associated const on a report type cannot carry the name
//! and this enum carries it instead.

use crate::report::{Outcome, eq_base, interpolations, missing, normalize, unused};
use crate::session::Session;
use crate::stats::ForestStats;
use serde::ser::SerializeMap;
use serde::{Serialize, Serializer};

/// One check's result, under the name the CLI and the JSON report it by.
///
/// The CLI names a check once, then the envelope and `health` both read the
/// name, the outcome and the text out of it.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum Check {
    Missing(missing::MissingReport),
    Unused(unused::UnusedReport),
    EqBase(eq_base::EqBaseReport),
    ConsistentInterpolations(interpolations::InterpolationReport),
    ReservedInterpolations(interpolations::InterpolationReport),
    Normalized(normalize::NormalizeReport),
}

impl Check {
    /// The `check` field of the JSON envelope, and the field name `health`
    /// nests the report under.
    pub fn name(&self) -> &'static str {
        match self {
            Check::Missing(_) => "missing",
            Check::Unused(_) => "unused",
            Check::EqBase(_) => "eq_base",
            Check::ConsistentInterpolations(_) => "check_consistent_interpolations",
            Check::ReservedInterpolations(_) => "check_reserved_interpolations",
            Check::Normalized(_) => "check_normalized",
        }
    }

    pub fn outcome(&self) -> Outcome {
        match self {
            Check::Missing(r) => r.outcome(),
            Check::Unused(r) => r.outcome(),
            Check::EqBase(r) => r.outcome(),
            Check::ConsistentInterpolations(r) | Check::ReservedInterpolations(r) => r.outcome(),
            Check::Normalized(r) => r.outcome(),
        }
    }

    pub fn to_text(&self) -> String {
        match self {
            Check::Missing(r) => r.to_text(),
            Check::Unused(r) => r.to_text(),
            Check::EqBase(r) => r.to_text(),
            Check::ConsistentInterpolations(r) | Check::ReservedInterpolations(r) => r.to_text(),
            // `normalize` prints the same report differently, so the report has
            // two renderers and this is the read-only one.
            Check::Normalized(r) => r.to_check_text(),
        }
    }

    /// One check in the shared envelope: the four fields every check emits,
    /// then the report's own fields flattened in.
    ///
    /// # Errors
    ///
    /// The report does not serialize.
    pub fn to_json(&self, session: &Session) -> Result<String, String> {
        #[derive(Serialize)]
        struct Envelope<'a> {
            check: &'a str,
            passed: bool,
            config_digest: &'a str,
            locales: &'a [String],
            #[serde(flatten)]
            report: &'a Check,
        }
        let env = Envelope {
            check: self.name(),
            passed: self.outcome() == Outcome::Clean,
            config_digest: &session.cfg.digest,
            locales: &session.locales,
            report: self,
        };
        serde_json::to_string_pretty(&env).map_err(|e| e.to_string())
    }
}

/// Whether any check found something: the exit-1 condition, and the negation
/// of the envelope's `passed`. One expression, so the code and the report
/// cannot disagree.
pub fn any_found(checks: &[Check]) -> bool {
    checks.iter().any(|c| c.outcome() == Outcome::Found)
}

/// The `health` envelope: the shared four fields, the statistics header, then
/// one field per check, named by `Check::name`.
///
/// # Errors
///
/// A report does not serialize.
pub fn health_json(
    session: &Session,
    stats: &ForestStats,
    checks: &[Check],
) -> Result<String, String> {
    let env = Health {
        passed: !any_found(checks),
        config_digest: &session.cfg.digest,
        locales: &session.locales,
        stats,
        checks,
    };
    serde_json::to_string_pretty(&env).map_err(|e| e.to_string())
}

/// Written by hand rather than derived, because the field names come from the
/// checks at run time. A `serde_json::Map` would not do: without the
/// `preserve_order` feature it is a `BTreeMap`, so the five reports would come
/// out in alphabetical order instead of report order.
struct Health<'a> {
    passed: bool,
    config_digest: &'a str,
    locales: &'a [String],
    stats: &'a ForestStats,
    checks: &'a [Check],
}

impl Serialize for Health<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(5 + self.checks.len()))?;
        map.serialize_entry("check", "health")?;
        map.serialize_entry("passed", &self.passed)?;
        map.serialize_entry("config_digest", self.config_digest)?;
        map.serialize_entry("locales", self.locales)?;
        map.serialize_entry("stats", self.stats)?;
        for check in self.checks {
            map.serialize_entry(check.name(), check)?;
        }
        map.end()
    }
}

#[cfg(test)]
mod tests {
    use super::{Check, health_json};
    use crate::config::Config;
    use crate::data::load::Store;
    use crate::report::{eq_base, interpolations, missing, normalize, unused};
    use crate::session::Session;
    use crate::stats::forest_stats;
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};

    /// A session over a config that reads nothing. The envelope only asks the
    /// session for the digest and the locale list, so nothing has to be on disk.
    fn session() -> Session {
        let cfg = Config::parse(
            "base_locale: en\nlocales: [en, de]\n",
            Path::new("config/i18n-tasks-rs.yml"),
            PathBuf::from("."),
        )
        .expect("the config parses");
        let store = Store {
            base_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            trees: HashMap::new(),
            external: HashMap::new(),
            warnings: Vec::new(),
        };
        Session {
            cfg,
            store,
            locales: vec!["en".to_string(), "de".to_string()],
            json: true,
        }
    }

    /// One of each variant, all empty, so every check passes.
    fn every_check(s: &Session) -> Vec<Check> {
        vec![
            Check::Missing(missing::MissingReport { rows: Vec::new() }),
            Check::Unused(unused::UnusedReport {
                rows: Vec::new(),
                opaque: Vec::new(),
            }),
            Check::EqBase(eq_base::report(&s.cfg, &s.store, &s.locales)),
            Check::ConsistentInterpolations(interpolations::inconsistent(
                &s.cfg, &s.store, &s.locales,
            )),
            Check::ReservedInterpolations(interpolations::reserved(&s.store, &s.locales)),
            Check::Normalized(normalize::NormalizeReport {
                changes: Vec::new(),
                files_routed: 0,
            }),
        ]
    }

    #[test]
    fn a_check_is_named_after_its_command() {
        let s = session();
        let names: Vec<&str> = every_check(&s).iter().map(Check::name).collect();
        assert_eq!(
            names,
            [
                "missing",
                "unused",
                "eq_base",
                "check_consistent_interpolations",
                "check_reserved_interpolations",
                "check_normalized",
            ]
        );
    }

    #[test]
    fn the_envelope_opens_with_the_four_shared_fields() {
        let s = session();
        let json = Check::Missing(missing::MissingReport { rows: Vec::new() })
            .to_json(&s)
            .expect("the envelope serializes");
        assert_eq!(
            field_order(&json),
            ["check", "passed", "config_digest", "locales", "rows"],
            "{json}"
        );
        assert!(json.contains(r#""check": "missing""#), "{json}");
        assert!(json.contains(r#""passed": true"#), "{json}");
    }

    /// The reason `Health` is hand-written rather than derived: a
    /// `serde_json::Map` is a `BTreeMap` without `preserve_order`, so the five
    /// reports would come out alphabetically instead of in report order.
    #[test]
    fn health_nests_the_checks_in_report_order() {
        let s = session();
        let checks = every_check(&s);
        let stats = forest_stats(&s.store, &s.locales);
        let json = health_json(&s, &stats, &checks).expect("the envelope serializes");
        assert_eq!(
            field_order(&json),
            [
                "check",
                "passed",
                "config_digest",
                "locales",
                "stats",
                "missing",
                "unused",
                "eq_base",
                "check_consistent_interpolations",
                "check_reserved_interpolations",
                "check_normalized",
            ],
            "{json}"
        );
    }

    /// The top-level keys of a pretty-printed object, in order. Two spaces of
    /// indent is exactly one level, so nested keys are skipped.
    fn field_order(json: &str) -> Vec<&str> {
        json.lines()
            .filter_map(|l| l.strip_prefix("  \""))
            .filter_map(|l| l.split('"').next())
            .collect()
    }
}
