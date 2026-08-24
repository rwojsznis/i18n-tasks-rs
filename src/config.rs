//! The plain-YAML configuration file.
//!
//! Blocker B3: the gem evaluates its config as ERB and then as Ruby
//! (`lib/i18n/tasks/configuration.rb:26`), which lets the file boot Rails and
//! register scanner classes. This tool never executes code. A `<%` anywhere in
//! the file is an error, and so is any key the tool does not understand.
//!
//! Because of that a gem config cannot be reused as is. `migrate-config`
//! converts one; see `src/migrate.rs`.

use crate::pattern::PatternSet;
use crate::yaml::{self, Node};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const DEFAULT_CONFIG_PATH: &str = "config/i18n-tasks-rs.yml";

/// Files the gem never searches, whatever `search.exclude` says.
///
/// ref: lib/i18n/tasks/used_keys.rb:32-34
pub const ALWAYS_EXCLUDE: &[&str] = &[
    "*.jpg", "*.jpeg", "*.png", "*.gif", "*.svg", "*.ico", "*.eot", "*.otf", "*.ttf", "*.woff",
    "*.woff2", "*.pdf", "*.css", "*.sass", "*.scss", "*.less", "*.yml", "*.json", "*.zip",
    "*.tar.gz", "*.swf", "*.flv", "*.mp3", "*.wav", "*.flac", "*.webm", "*.mp4", "*.ogg", "*.opus",
    "*.webp", "*.map", "*.xlsx",
];

/// ref: lib/i18n/tasks/used_keys.rb#SEARCH_DEFAULTS
///
/// Public because `init-config` filters this list down to the directories a
/// project actually has.
pub const DEFAULT_RELATIVE_ROOTS: &[&str] = &[
    "app/controllers",
    "app/helpers",
    "app/mailers",
    "app/presenters",
    "app/views",
];

/// The four typed `ignore_*` config keys.
///
/// The fifth `ignore*` key, the global `ignore`, has no variant: it is merged
/// into every type by `Config::ignore_patterns`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IgnoreType {
    Missing,
    Unused,
    EqBase,
    InconsistentInterpolations,
}

/// Either a flat list or a per-locale map.
///
/// ref: lib/i18n/tasks/ignore_keys.rb:7-30
#[derive(Debug, Clone, Default)]
pub enum IgnoreSpec {
    #[default]
    Empty,
    All(Vec<String>),
    /// Keys are locale lists such as `fr,es`, plus the special `all`.
    PerLocale(Vec<(Vec<String>, Vec<String>)>),
}

impl IgnoreSpec {
    /// The patterns that apply to `locale`, or the locale-independent ones when
    /// `locale` is `None`.
    fn resolve(&self, locale: Option<&str>) -> Vec<String> {
        match self {
            IgnoreSpec::Empty => Vec::new(),
            IgnoreSpec::All(v) => v.clone(),
            IgnoreSpec::PerLocale(groups) => {
                let mut out = Vec::new();
                for (locales, pats) in groups {
                    let applies = locales
                        .iter()
                        .any(|l| l == "all" || locale.is_some_and(|want| l == want));
                    if applies {
                        out.extend(pats.iter().cloned());
                    }
                }
                out
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct DataConfig {
    pub read: Vec<String>,
    /// Each entry is either a bare path, or a `[key_pattern, path]` pair for
    /// the pattern router.
    pub write: Vec<WriteRule>,
    pub external: Vec<String>,
    pub router: Router,
    pub keep_order: bool,
}

#[derive(Debug, Clone)]
pub struct WriteRule {
    pub pattern: Option<String>,
    pub path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Router {
    Conservative,
    Pattern,
}

#[derive(Debug, Clone)]
pub struct SearchConfig {
    pub paths: Vec<String>,
    pub exclude: Vec<String>,
    pub only: Option<Vec<String>>,
    pub relative_roots: Vec<String>,
    pub relative_exclude_method_name_paths: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub base_locale: String,
    /// `None` means "infer from the data files".
    pub locales: Option<Vec<String>>,
    pub data: DataConfig,
    pub search: SearchConfig,
    /// Merged into every other ignore type.
    pub ignore: Vec<String>,
    pub ignore_missing: IgnoreSpec,
    pub ignore_unused: IgnoreSpec,
    pub ignore_eq_base: IgnoreSpec,
    pub ignore_inconsistent_interpolations: IgnoreSpec,
    /// A stable digest of the resolved config, reported under `--format json`
    /// so a differential run can prove both tools read the same settings.
    pub digest: String,
    /// Directory the config paths are relative to.
    pub root: PathBuf,
}

impl Config {
    /// Compiles the pattern set for one ignore type and locale.
    ///
    /// The global `ignore` list is merged into every type.
    /// ref: lib/i18n/tasks/ignore_keys.rb#ignore_pattern
    pub fn ignore_patterns(&self, ty: IgnoreType, locale: Option<&str>) -> PatternSet {
        let spec = match ty {
            IgnoreType::Missing => &self.ignore_missing,
            IgnoreType::Unused => &self.ignore_unused,
            IgnoreType::EqBase => &self.ignore_eq_base,
            IgnoreType::InconsistentInterpolations => &self.ignore_inconsistent_interpolations,
        };
        let mut pats = self.ignore.clone();
        pats.extend(spec.resolve(locale));
        PatternSet::new(&pats)
    }

    /// `root` is the directory every config path is relative to. The gem runs
    /// from the project root and resolves paths against the working directory,
    /// so that is the default.
    ///
    /// # Errors
    ///
    /// The file cannot be read — the message then names the gem config, if one
    /// is there to migrate — the working directory cannot be read when `root`
    /// is `None`, or `parse` rejects the contents.
    pub fn load(path: &Path, root: Option<&Path>) -> Result<Config, String> {
        let src = std::fs::read_to_string(path).map_err(|e| {
            let mut msg = format!("cannot read config {}: {e}", path.display());
            // The most likely reason is that nobody has migrated the gem
            // config yet, and that is a one-command fix. Failing that, the
            // project never had one, and that is also a one-command fix.
            match crate::migrate::find_gem_config(root.unwrap_or(Path::new("."))) {
                Some(gem) => msg.push_str(&format!(
                    "\n  found {}: run `i18n-tasks-rs migrate-config` to convert it.",
                    gem.display()
                )),
                None => msg.push_str(
                    "\n  run `i18n-tasks-rs init-config` to generate one from this project.",
                ),
            }
            msg
        })?;
        let root = match root {
            Some(r) => r.to_path_buf(),
            None => std::env::current_dir().map_err(|e| e.to_string())?,
        };
        Config::parse(&src, path, root)
    }

    /// # Errors
    ///
    /// The source holds `<%` (blocker B3: no code execution, ever), is not a
    /// YAML mapping, names a setting this port does not have, or gives a
    /// setting a value of the wrong kind.
    pub fn parse(src: &str, path: &Path, root: PathBuf) -> Result<Config, String> {
        // Blocker B3: no code execution, ever.
        if src.contains("<%") {
            return Err(format!(
                "{}: ERB is not supported. This config format is plain YAML and never \
                 executes code. Run `i18n-tasks-rs migrate-config` to convert this file.",
                path.display()
            ));
        }
        let node = yaml::parse(src, path)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("{}: config is empty", path.display()))?;
        let top = node
            .as_map()
            .ok_or_else(|| format!("{}: config must be a YAML mapping", path.display()))?;

        let ctx = |k: &str| format!("{}: `{}`", path.display(), k);
        let mut base_locale = "en".to_string();
        let mut locales: Option<Vec<String>> = None;
        let mut data = DataConfig {
            read: vec!["config/locales/%{locale}.yml".into()],
            write: vec![WriteRule {
                pattern: None,
                path: "config/locales/%{locale}.yml".into(),
            }],
            external: Vec::new(),
            router: Router::Conservative,
            keep_order: false,
        };
        let mut search = SearchConfig {
            paths: vec!["app/".into()],
            exclude: Vec::new(),
            only: None,
            relative_roots: DEFAULT_RELATIVE_ROOTS
                .iter()
                .map(ToString::to_string)
                .collect(),
            relative_exclude_method_name_paths: Vec::new(),
        };
        let mut ignore = Vec::new();
        let mut ignore_missing = IgnoreSpec::Empty;
        let mut ignore_unused = IgnoreSpec::Empty;
        let mut ignore_eq_base = IgnoreSpec::Empty;
        let mut ignore_inconsistent = IgnoreSpec::Empty;

        for (k, v) in top {
            let key = k
                .as_str()
                .ok_or_else(|| format!("{}: non-string config key", path.display()))?;
            match key {
                "base_locale" => {
                    base_locale = v
                        .as_str()
                        .ok_or_else(|| ctx(key) + " must be a string")?
                        .into();
                }
                "locales" => locales = Some(str_list(v, &ctx(key))?),
                "ignore" => ignore = str_list(v, &ctx(key))?,
                "ignore_missing" => ignore_missing = ignore_spec(v, &ctx(key))?,
                "ignore_unused" => ignore_unused = ignore_spec(v, &ctx(key))?,
                "ignore_eq_base" => ignore_eq_base = ignore_spec(v, &ctx(key))?,
                "ignore_inconsistent_interpolations" => {
                    ignore_inconsistent = ignore_spec(v, &ctx(key))?;
                }
                "data" => {
                    let m = v.as_map().ok_or_else(|| ctx(key) + " must be a mapping")?;
                    for (dk, dv) in m {
                        let dkey = dk.as_str().unwrap_or_default();
                        let c = ctx(&format!("data.{dkey}"));
                        match dkey {
                            "read" => data.read = str_list(dv, &c)?,
                            "write" => data.write = write_rules(dv, &c)?,
                            "external" => data.external = str_list(dv, &c)?,
                            "router" => {
                                data.router = match dv.as_str().unwrap_or_default() {
                                    "conservative_router" => Router::Conservative,
                                    "pattern_router" => Router::Pattern,
                                    other => {
                                        return Err(format!(
                                            "{c}: unknown router `{other}`. Supported: \
                                             conservative_router, pattern_router."
                                        ));
                                    }
                                }
                            }
                            "keep_order" => data.keep_order = truthy(dv),
                            other => return Err(unknown(path, &format!("data.{other}"))),
                        }
                    }
                }
                "search" => {
                    let m = v.as_map().ok_or_else(|| ctx(key) + " must be a mapping")?;
                    for (sk, sv) in m {
                        let skey = sk.as_str().unwrap_or_default();
                        let c = ctx(&format!("search.{skey}"));
                        match skey {
                            "paths" => search.paths = str_list(sv, &c)?,
                            "exclude" => search.exclude = str_list(sv, &c)?,
                            "only" => search.only = Some(str_list(sv, &c)?),
                            "relative_roots" => search.relative_roots = str_list(sv, &c)?,
                            "relative_exclude_method_name_paths" => {
                                search.relative_exclude_method_name_paths = str_list(sv, &c)?;
                            }
                            other => return Err(unknown(path, &format!("search.{other}"))),
                        }
                    }
                }
                other => return Err(unknown(path, other)),
            }
        }

        let mut cfg = Config {
            base_locale,
            locales,
            data,
            search,
            ignore,
            ignore_missing,
            ignore_unused,
            ignore_eq_base,
            ignore_inconsistent_interpolations: ignore_inconsistent,
            digest: String::new(),
            root,
        };
        cfg.digest = cfg.compute_digest();
        Ok(cfg)
    }

    fn compute_digest(&self) -> String {
        // A stable order, so the digest does not depend on file layout.
        let mut fields: BTreeMap<&str, String> = BTreeMap::new();
        fields.insert("base_locale", self.base_locale.clone());
        fields.insert("locales", format!("{:?}", self.locales));
        fields.insert("data.read", format!("{:?}", self.data.read));
        fields.insert("data.write", format!("{:?}", self.data.write));
        fields.insert("data.external", format!("{:?}", self.data.external));
        fields.insert("data.router", format!("{:?}", self.data.router));
        fields.insert("data.keep_order", format!("{:?}", self.data.keep_order));
        fields.insert("search", format!("{:?}", self.search));
        fields.insert("ignore", format!("{:?}", self.ignore));
        fields.insert("ignore_missing", format!("{:?}", self.ignore_missing));
        fields.insert("ignore_unused", format!("{:?}", self.ignore_unused));
        fields.insert("ignore_eq_base", format!("{:?}", self.ignore_eq_base));
        fields.insert(
            "ignore_inconsistent_interpolations",
            format!("{:?}", self.ignore_inconsistent_interpolations),
        );
        let joined = fields
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("\n");
        format!("{:016x}", fnv1a64(joined.as_bytes()))
    }
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    h
}

fn unknown(path: &Path, key: &str) -> String {
    format!(
        "{}: unknown config key `{key}`. Supported keys: base_locale, locales, \
         data.{{read,write,external,router,keep_order}}, \
         search.{{paths,exclude,only,relative_roots,relative_exclude_method_name_paths}}, \
         ignore, ignore_missing, ignore_unused, ignore_eq_base, \
         ignore_inconsistent_interpolations.",
        path.display()
    )
}

fn truthy(node: &Node) -> bool {
    matches!(node.as_str(), Some("true" | "yes" | "on"))
}

fn str_list(node: &Node, ctx: &str) -> Result<Vec<String>, String> {
    match node {
        Node::Scalar { value, .. } => Ok(vec![value.clone()]),
        Node::Seq { items, .. } => items
            .iter()
            .map(|i| {
                i.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| format!("{ctx}: expected a string"))
            })
            .collect(),
        Node::Map { .. } => Err(format!("{ctx}: expected a string or a list")),
    }
}

/// `data.write` entries are either a bare path or a `[pattern, path]` pair.
///
/// ref: lib/i18n/tasks/data/router/pattern_router.rb
fn write_rules(node: &Node, ctx: &str) -> Result<Vec<WriteRule>, String> {
    let items: Vec<&Node> = match node {
        Node::Scalar { .. } => vec![node],
        Node::Seq { items, .. } => items.iter().collect(),
        Node::Map { .. } => return Err(format!("{ctx}: expected a string or a list")),
    };
    items
        .into_iter()
        .map(|item| match item {
            Node::Scalar { value, .. } => Ok(WriteRule {
                pattern: None,
                path: value.clone(),
            }),
            Node::Seq { items, .. } if items.len() == 2 => Ok(WriteRule {
                pattern: Some(
                    items[0]
                        .as_str()
                        .ok_or_else(|| format!("{ctx}: pattern must be a string"))?
                        .into(),
                ),
                path: items[1]
                    .as_str()
                    .ok_or_else(|| format!("{ctx}: path must be a string"))?
                    .into(),
            }),
            _ => Err(format!(
                "{ctx}: each entry must be a path or a [pattern, path] pair"
            )),
        })
        .collect()
}

/// The gem selects per-locale groups with `/\b#{locale}\b/`, so `fr,es:`
/// matches `fr`. This splits on `,` and trims instead, which is clearer and
/// cannot match a substring by accident.
///
/// ref: lib/i18n/tasks/ignore_keys.rb:7-30
fn ignore_spec(node: &Node, ctx: &str) -> Result<IgnoreSpec, String> {
    match node {
        Node::Scalar { value, .. } => Ok(IgnoreSpec::All(vec![value.clone()])),
        Node::Seq { .. } => Ok(IgnoreSpec::All(str_list(node, ctx)?)),
        Node::Map { entries, .. } => {
            let mut groups = Vec::new();
            for (k, v) in entries {
                let raw = k
                    .as_str()
                    .ok_or_else(|| format!("{ctx}: non-string locale group"))?;
                let locales: Vec<String> = raw
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                groups.push((locales, str_list(v, ctx)?));
            }
            Ok(IgnoreSpec::PerLocale(groups))
        }
    }
}

/// `%{locale}` substitution, with `%%` as an escaped `%`.
///
/// ref: `Kernel#format` semantics, which is what the gem uses in
/// `file_system_base.rb#read_locale`.
pub fn interpolate_locale(template: &str, locale: &str) -> String {
    let mut out = String::with_capacity(template.len() + locale.len());
    let bytes = template.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if bytes.get(i + 1) == Some(&b'%') {
                out.push('%');
                i += 2;
                continue;
            }
            if template[i..].starts_with("%{locale}") {
                out.push_str(locale);
                i += "%{locale}".len();
                continue;
            }
        }
        // `i` is on a character boundary and inside the string, so the `else`
        // arm only restates the loop condition. A character is copied whole:
        // the loop compares bytes, but must not split a multi-byte character.
        let Some(ch) = template[i..].chars().next() else {
            break;
        };
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Four loops scan bytes but copy characters — here, in `locale_pattern_re`,
    /// in `route::substitute_captures` and in `pattern::tokenize`. Each one must
    /// step a whole character, so a multi-byte name survives the pass.
    #[test]
    fn a_multi_byte_path_survives_locale_interpolation() {
        assert_eq!(
            interpolate_locale("config/переводы/%{locale}.yml", "ru"),
            "config/переводы/ru.yml"
        );
        // The escape and the substitution still work either side of it.
        assert_eq!(interpolate_locale("100%% ü %{locale}", "de"), "100% ü de");
    }

    fn parse(src: &str) -> Result<Config, String> {
        Config::parse(src, Path::new("config/i18n-tasks.yml"), PathBuf::from("."))
    }

    #[test]
    fn rejects_erb() {
        let e = parse("<% x %>\nbase_locale: de\n").unwrap_err();
        assert!(e.contains("ERB is not supported"));
    }

    #[test]
    fn rejects_unknown_keys() {
        let e = parse("base_locale: de\ntranslation:\n  backend: openai\n").unwrap_err();
        assert!(e.contains("unknown config key `translation`"), "{e}");
        let e = parse("search:\n  prism: rails\n").unwrap_err();
        assert!(e.contains("unknown config key `search.prism`"), "{e}");
    }

    #[test]
    fn reads_a_translated_real_world_config() {
        let cfg = parse(
            "base_locale: de\n\
             locales: [de, en, fr]\n\
             data:\n\
             \x20 read:\n\
             \x20   - config/locales/base.%{locale}.yml\n\
             \x20   - config/locales/*.%{locale}.yml\n\
             \x20 write:\n\
             \x20   - [\"{activerecord, views}.*\", 'config/locales/\\1.%{locale}.yml']\n\
             \x20   - config/locales/base.%{locale}.yml\n\
             search:\n\
             \x20 paths: [app/]\n\
             \x20 exclude: [app/webpack]\n\
             \x20 relative_roots: [app/controllers, app/forms, app/presenters]\n\
             ignore_unused:\n\
             \x20 - \"devise.*\"\n",
        )
        .unwrap();
        assert_eq!(cfg.base_locale, "de");
        assert_eq!(
            cfg.locales.as_deref(),
            Some(&["de".into(), "en".into(), "fr".into()][..])
        );
        assert_eq!(cfg.data.read.len(), 2);
        assert_eq!(cfg.data.write.len(), 2);
        assert_eq!(
            cfg.data.write[0].pattern.as_deref(),
            Some("{activerecord, views}.*")
        );
        assert_eq!(cfg.data.write[0].path, "config/locales/\\1.%{locale}.yml");
        assert_eq!(cfg.data.write[1].pattern, None);
        assert!(cfg.search.relative_roots.contains(&"app/forms".to_string()));
        assert_eq!(cfg.digest.len(), 16);
    }

    #[test]
    fn global_ignore_merges_into_every_type() {
        let cfg = parse("ignore: [\"a.*\"]\nignore_unused: [\"b.*\"]\n").unwrap();
        let p = cfg.ignore_patterns(IgnoreType::Unused, Some("en"));
        assert!(p.is_match("a.x"));
        assert!(p.is_match("b.x"));
        let p = cfg.ignore_patterns(IgnoreType::Missing, Some("en"));
        assert!(p.is_match("a.x"));
        assert!(!p.is_match("b.x"));
    }

    #[test]
    fn per_locale_ignore_hash() {
        let cfg =
            parse("ignore_missing:\n  all: [\"a.*\"]\n  \"fr,es\": [\"b.*\"]\n  de: [\"c.*\"]\n")
                .unwrap();
        let fr = cfg.ignore_patterns(IgnoreType::Missing, Some("fr"));
        assert!(fr.is_match("a.x") && fr.is_match("b.x") && !fr.is_match("c.x"));
        let de = cfg.ignore_patterns(IgnoreType::Missing, Some("de"));
        assert!(de.is_match("a.x") && !de.is_match("b.x") && de.is_match("c.x"));
    }

    /// Every ignore type accepts both a list and per-locale groups.
    #[test]
    fn every_ignore_type_resolves() {
        let cfg = parse(
            "ignore: [\"g.*\"]\n\
             ignore_missing: [\"m.*\"]\n\
             ignore_unused: [\"u.*\"]\n\
             ignore_eq_base: [\"e.*\"]\n\
             ignore_inconsistent_interpolations: [\"i.*\"]\n",
        )
        .unwrap();
        for (ty, own) in [
            (IgnoreType::Missing, "m.x"),
            (IgnoreType::Unused, "u.x"),
            (IgnoreType::EqBase, "e.x"),
            (IgnoreType::InconsistentInterpolations, "i.x"),
        ] {
            let p = cfg.ignore_patterns(ty, None);
            assert!(p.is_match(own), "{own}");
            // The global list is merged into every type.
            assert!(p.is_match("g.x"));
        }
    }

    /// A single string stands in for a one-element list everywhere the gem
    /// accepts one.
    #[test]
    fn a_scalar_stands_in_for_a_list() {
        let cfg = parse(
            "data:\n\
             \x20 read: config/locales/%{locale}.yml\n\
             \x20 write: config/locales/%{locale}.yml\n\
             \x20 external: config/ext/%{locale}.yml\n\
             search:\n\
             \x20 paths: app/\n\
             \x20 relative_exclude_method_name_paths: app/views\n\
             ignore_missing: \"a.*\"\n",
        )
        .unwrap();
        assert_eq!(cfg.data.read, vec!["config/locales/%{locale}.yml"]);
        assert_eq!(cfg.data.write.len(), 1);
        assert_eq!(cfg.data.write[0].pattern, None);
        assert_eq!(cfg.data.external, vec!["config/ext/%{locale}.yml"]);
        assert_eq!(cfg.search.paths, vec!["app/"]);
        assert_eq!(
            cfg.search.relative_exclude_method_name_paths,
            vec!["app/views"]
        );
        assert!(
            cfg.ignore_patterns(IgnoreType::Missing, Some("en"))
                .is_match("a.x")
        );
    }

    #[test]
    fn both_routers_are_accepted_and_anything_else_is_an_error() {
        assert_eq!(
            parse("data:\n  router: conservative_router\n")
                .unwrap()
                .data
                .router,
            Router::Conservative
        );
        assert_eq!(
            parse("data:\n  router: pattern_router\n")
                .unwrap()
                .data
                .router,
            Router::Pattern
        );
        let e = parse("data:\n  router: isolating_router\n").unwrap_err();
        assert!(e.contains("unknown router `isolating_router`"), "{e}");
        // A mapping where a name belongs is the same error.
        let e = parse("data:\n  router:\n    a: b\n").unwrap_err();
        assert!(e.contains("unknown router"), "{e}");
    }

    #[test]
    fn keep_order_is_read_from_every_truthy_spelling() {
        for spelling in ["true", "yes", "on"] {
            let cfg = parse(&format!("data:\n  keep_order: {spelling}\n")).unwrap();
            assert!(cfg.data.keep_order, "{spelling}");
        }
        assert!(
            !parse("data:\n  keep_order: false\n")
                .unwrap()
                .data
                .keep_order
        );
        assert!(!parse("base_locale: en\n").unwrap().data.keep_order);
    }

    #[test]
    fn rejects_unknown_nested_keys() {
        let e = parse("data:\n  adapter: json\n").unwrap_err();
        assert!(e.contains("unknown config key `data.adapter`"), "{e}");
        let e = parse("search:\n  scanners: []\n").unwrap_err();
        assert!(e.contains("unknown config key `search.scanners`"), "{e}");
    }

    #[test]
    fn rejects_the_wrong_shape_with_the_key_in_the_message() {
        // A mapping where a string or a list belongs.
        let e = parse("search:\n  paths:\n    a: b\n").unwrap_err();
        assert!(e.contains("search.paths"), "{e}");
        assert!(e.contains("expected a string or a list"), "{e}");
        // A list holding something that is not a string.
        let e = parse("search:\n  paths:\n    - [a]\n").unwrap_err();
        assert!(e.contains("expected a string"), "{e}");
        // `data` and `search` themselves must be mappings.
        let e = parse("data: config/locales\n").unwrap_err();
        assert!(e.contains("must be a mapping"), "{e}");
        let e = parse("search: app/\n").unwrap_err();
        assert!(e.contains("must be a mapping"), "{e}");
    }

    #[test]
    fn rejects_a_malformed_write_rule() {
        // A mapping where the rule list belongs.
        let e = parse("data:\n  write:\n    a: b\n").unwrap_err();
        assert!(e.contains("expected a string or a list"), "{e}");
        // Three elements is neither a path nor a [pattern, path] pair.
        let e = parse("data:\n  write:\n    - [a, b, c]\n").unwrap_err();
        assert!(
            e.contains("must be a path or a [pattern, path] pair"),
            "{e}"
        );
        // The two halves of a pair must both be strings.
        let e = parse("data:\n  write:\n    - [[a], b]\n").unwrap_err();
        assert!(e.contains("pattern must be a string"), "{e}");
        let e = parse("data:\n  write:\n    - [a, [b]]\n").unwrap_err();
        assert!(e.contains("path must be a string"), "{e}");
    }

    #[test]
    fn locale_interpolation() {
        assert_eq!(
            interpolate_locale("config/locales/%{locale}.yml", "de"),
            "config/locales/de.yml"
        );
        assert_eq!(interpolate_locale("a%%b%{locale}", "de"), "a%bde");
    }
}
