//! The Prism Ruby scanner. This is the core of the tool.
//!
//! ref: lib/i18n/tasks/scanners/prism_scanners/visitor.rb
//! ref: lib/i18n/tasks/scanners/prism_scanners/nodes.rb
//! ref: lib/i18n/tasks/scanners/prism_scanners/arguments_visitor.rb
//!
//! Three deliberate departures from the gem, each recorded in
//! `docs/accepted-diffs.md`:
//!
//! * **B5.** An interpolated key produces a key *pattern* instead of nothing,
//!   and a fully opaque call is reported instead of being silently dropped.
//! * **B6.** `search.relative_roots` and
//!   `search.relative_exclude_method_name_paths` are honoured. The gem's Prism
//!   path hardcodes `app/views/` and `app/components/`.
//! * The Rails inference layer (`human_attribute_name`, `model_name.human`,
//!   `before_action` re-parenting) is dropped. See accepted diffs 4 and 4a.

use super::{FileScan, Locator, Occurrence, ScanConfig, SourceMap};
use crate::keys::underscore;
use crate::lineindex::LineIndex;
use ruby_prism as pr;
use ruby_prism::Visit as _;
use std::path::Path;
use std::rc::Rc;

/// ref: visitor.rb#MAGIC_COMMENT_PREFIX
const MAGIC_COMMENT_MARKER: &str = "i18n-tasks-use";

pub fn scan(bytes: &[u8], path: &Path, cfg: &ScanConfig) -> FileScan {
    let index = LineIndex::new(bytes);
    scan_buffer(bytes, path, cfg, Locator::direct(&index))
}

/// Scans Ruby source that is not the file itself, as the ERB scanner needs: the
/// buffer is parsed, and every position is translated back through the locator.
pub(super) fn scan_synthetic(
    buffer: &[u8],
    path: &Path,
    cfg: &ScanConfig,
    index: &LineIndex,
    map: &SourceMap,
) -> FileScan {
    scan_buffer(buffer, path, cfg, Locator::mapped(index, map))
}

fn scan_buffer(bytes: &[u8], path: &Path, cfg: &ScanConfig, loc: Locator) -> FileScan {
    super::count_source_parse();
    let parsed = pr::parse(bytes);
    let mut v = Visitor::new(path, cfg, loc);
    v.visit(&parsed.node());

    // Magic comments are handled after the walk, once every scope range is
    // known. See `resolve_comment_ctx`.
    let comments: Vec<(usize, Vec<u8>)> = parsed
        .comments()
        .map(|c| {
            (
                c.location().start_offset(),
                c.location().as_slice().to_vec(),
            )
        })
        .collect();
    let mut covered_lines = Vec::new();
    for (offset, text) in comments {
        if v.handle_magic_comment(offset, &text) {
            covered_lines.push(v.loc.locate(offset).1 + 1);
        }
    }
    v.out
        .opaque
        .retain(|occ| !covered_lines.contains(&occ.line_num));
    v.out
}

/// What a `t` call inside a scope may do with a leading-dot key.
#[derive(Debug, Clone, Copy)]
struct Caps {
    /// ref: nodes.rb#support_relative_keys?
    relative: bool,
    /// ref: nodes.rb#support_candidate_keys?
    candidate: bool,
}

impl Caps {
    /// A scope that resolves no relative key at all: a module, a class body, or
    /// anything reached through one.
    fn none() -> Caps {
        Caps {
            relative: false,
            candidate: false,
        }
    }
}

/// A lexical scope. `path` is already resolved against the parent, so it needs
/// no further walking, and it is shared rather than copied because every `t`
/// call inside the scope reads it.
#[derive(Debug)]
struct Scope {
    kind: ScopeKind,
    path: Rc<[String]>,
    caps: Caps,
}

#[derive(Debug)]
enum ScopeKind {
    /// ref: nodes.rb ParsedModule — never supports relative keys, so its
    /// `caps` are always `Caps::none()`.
    Module,
    Class {
        view_component: bool,
        /// Flipped by a bare `private`. ref: visitor.rb:95-96
        private_now: bool,
    },
    Method,
}

/// Everything a `t` call needs from its enclosing scope.
///
/// ref: nodes.rb TranslationCall#support_relative_keys?, which requires the
/// parent to be a `ParsedMethod` or the `Root`.
#[derive(Debug, Clone)]
struct CallCtx {
    path: Rc<[String]>,
    caps: Caps,
}

/// A scope's byte range, kept so a magic comment can be attached to the
/// innermost scope that contains it.
struct ScopeRange {
    start: usize,
    end: usize,
    ctx: CallCtx,
}

struct Visitor<'a> {
    path: &'a Path,
    cfg: &'a ScanConfig,
    loc: Locator<'a>,
    scopes: Vec<Scope>,
    ranges: Vec<ScopeRange>,
    /// ref: nodes.rb Root#path, for a template file under a relative root.
    root_path: Rc<[String]>,
    /// ref: nodes.rb Root#rails_view?, and Root#support_candidate_keys? which
    /// is always false.
    root_caps: Caps,
    /// ref: relative_keys.rb:26-28
    append_method_name: bool,
    view_component_file: bool,
    out: FileScan,
}

impl<'a> Visitor<'a> {
    fn new(path: &'a Path, cfg: &'a ScanConfig, loc: Locator<'a>) -> Visitor<'a> {
        let posix = path.to_string_lossy().replace('\\', "/");
        // Case-sensitive on purpose: extension dispatch in `scan::scan_file` is
        // too, and the gem compares the literal suffix.
        #[allow(
            clippy::case_sensitive_file_extension_comparisons,
            reason = "gem parity: a `.RB` file is not a Ruby file to either tool"
        )]
        let is_rb = posix.ends_with(".rb");
        let root = cfg.matching_root(path);
        // Blocker B6: the gem hardcodes `app/views/` and `app/components/`.
        // Any configured relative root counts, and a `.rb` file never counts as
        // a template, which is what keeps `app/controllers/*.rb` out.
        let relative_root = if is_rb { None } else { root };
        let root_path: Vec<String> = relative_root
            .map(|r| template_path(&posix, r))
            .unwrap_or_default();
        Visitor {
            path,
            cfg,
            loc,
            scopes: Vec::new(),
            ranges: Vec::new(),
            root_path: root_path.into(),
            root_caps: Caps {
                relative: relative_root.is_some(),
                candidate: false,
            },
            append_method_name: !root.is_some_and(|r| cfg.skips_method_name(r)),
            view_component_file: posix.contains("app/components/"),
            out: FileScan::default(),
        }
    }

    fn root_ctx(&self) -> CallCtx {
        CallCtx {
            path: Rc::clone(&self.root_path),
            caps: self.root_caps,
        }
    }

    fn current_ctx(&self) -> CallCtx {
        match self.scopes.last() {
            None => self.root_ctx(),
            Some(s) => CallCtx {
                path: Rc::clone(&s.path),
                caps: match s.kind {
                    ScopeKind::Method => s.caps,
                    // The parent is a class or a module body, so a relative key
                    // never resolves there whatever the scope itself supports.
                    _ => Caps::none(),
                },
            },
        }
    }

    fn parent_path(&self) -> &[String] {
        match self.scopes.last() {
            Some(s) => &s.path,
            None => &self.root_path,
        }
    }

    /// A `def` attaches to the enclosing class or module, never to an enclosing
    /// `def`. ref: visitor.rb#visit_def_node, which reads
    /// `@current_class || @current_module || @root`.
    fn enclosing_definition(&self) -> Option<&Scope> {
        self.scopes
            .iter()
            .rev()
            .find(|s| !matches!(s.kind, ScopeKind::Method))
    }

    fn record_range(&mut self, start: usize, end: usize) {
        let ctx = self.current_ctx();
        self.ranges.push(ScopeRange { start, end, ctx });
    }

    // ---- translation calls ----

    fn occurrence(
        &self,
        node_loc: &pr::Location,
        raw_key: &str,
        candidates: &[String],
    ) -> Occurrence {
        let (pos, line_num, line_pos) = self.loc.locate(node_loc.start_offset());
        Occurrence {
            path: self.path.to_path_buf(),
            snippet: String::from_utf8_lossy(node_loc.as_slice()).into_owned(),
            pos,
            line_pos,
            line_num,
            raw_key: raw_key.to_string(),
            candidate_keys: candidates.to_vec(),
        }
    }

    fn handle_translation_call(&mut self, node: &pr::CallNode, ctx: &CallCtx) {
        // ref: visitor.rb:99-101 — a receiver that is not I18n drops the call
        // and its whole subtree.
        if let Some(recv) = node.receiver()
            && !is_i18n_receiver(&recv)
        {
            return;
        }
        let (args, kwargs) = process_arguments(node);
        let receiver_present = node.receiver().is_some();

        // ref: nodes.rb#scope. A `ScopeError` drops the occurrence entirely.
        let scope = match resolve_scope(&kwargs) {
            Ok(s) => s,
            Err(ScopeError) => return,
        };

        let Some(first) = args.first() else { return };
        let loc = node.location();
        match first {
            ArgVal::Str(key) => {
                let Some(resolved) = full_key(key, scope.as_deref(), ctx, receiver_present) else {
                    return;
                };
                let occ = self.occurrence(&loc, key, &resolved);
                self.out.keys.push((resolved[0].clone(), occ.clone()));
                // A literal key that ends with a dot is a dynamic key whose
                // tail is built elsewhere. ref: used_keys.rb#expr_key_re
                if resolved[0].ends_with('.') {
                    let pat = format!("{}*:", resolved[0]);
                    if !is_all_wildcard(&pat) {
                        self.out.patterns.push((pat, occ));
                    }
                }
            }
            // Blocker B5.
            ArgVal::Pattern(pat) => {
                let Some(resolved) = full_key(pat, scope.as_deref(), ctx, receiver_present) else {
                    return;
                };
                let occ = self.occurrence(&loc, pat, &resolved);
                if is_all_wildcard(&resolved[0]) {
                    // Too broad to be useful: it would mark every key used.
                    self.out.opaque.push(occ);
                } else {
                    self.out.patterns.push((resolved[0].clone(), occ));
                }
            }
            // Blocker B5: never treat an opaque call as "no keys used".
            ArgVal::Unresolvable | ArgVal::Nil => {
                let snippet = String::from_utf8_lossy(loc.as_slice()).into_owned();
                let occ = self.occurrence(&loc, &snippet, &[]);
                self.out.opaque.push(occ);
            }
            // An integer, array or hash key resolves to nothing, as in the gem.
            _ => {}
        }
    }

    // ---- magic comments ----

    /// The innermost scope whose byte range contains the comment.
    ///
    /// The gem relies on Prism attaching the comment to a node and then uses
    /// that node's scope (`visitor.rb#handle_comments`), with a fallback in
    /// `ruby_scanner.rb:193-211` that loses the scope completely — which is why
    /// a comment directly before an `end` does not resolve a relative key
    /// there. Containment handles that case correctly.
    fn resolve_comment_ctx(&self, offset: usize) -> CallCtx {
        self.ranges
            .iter()
            .filter(|r| r.start <= offset && offset < r.end)
            .min_by_key(|r| r.end - r.start)
            .map_or_else(|| self.root_ctx(), |r| r.ctx.clone())
    }

    fn handle_magic_comment(&mut self, offset: usize, text: &[u8]) -> bool {
        let Ok(text) = std::str::from_utf8(text) else {
            return false;
        };
        let Some(payload) = strip_magic_prefix(text) else {
            return false;
        };
        // ref: ruby_scanner.rb:203 — `delete("#")` then `strip`.
        let payload: String = payload.chars().filter(|c| *c != '#').collect();
        let payload = payload.trim();
        if payload.is_empty() {
            return false;
        }
        // The gem splits several calls on `/\s+(?=t)/` and rejoins with "; ".
        // ref: ruby_scanner.rb:87
        let joined = split_calls(payload).join("; ");
        let nested = pr::parse(joined.as_bytes());
        if nested.errors().count() > 0 {
            return false;
        }
        let ctx = self.resolve_comment_ctx(offset);
        let calls = collect_translation_calls(&nested.node());
        let key_count = self.out.keys.len();
        // The occurrence points at the comment, not at the nested parse.
        let (pos, line_num, line_pos) = self.loc.locate(offset);
        for call in calls {
            if call.receiver_present && !call.receiver_is_i18n {
                continue;
            }
            let scope = match resolve_scope(&call.kwargs) {
                Ok(s) => s,
                Err(ScopeError) => continue,
            };
            let Some(ArgVal::Str(key)) = call.args.first() else {
                continue;
            };
            let Some(resolved) = full_key(key, scope.as_deref(), &ctx, call.receiver_present)
            else {
                continue;
            };
            let occ = Occurrence {
                path: self.path.to_path_buf(),
                snippet: text.to_string(),
                pos,
                line_pos,
                line_num,
                raw_key: key.clone(),
                candidate_keys: resolved.clone(),
            };
            self.out.keys.push((resolved[0].clone(), occ));
        }
        self.out.keys.len() > key_count
    }
}

impl<'pr> pr::Visit<'pr> for Visitor<'_> {
    fn visit_module_node(&mut self, node: &pr::ModuleNode<'pr>) {
        let mut path = self.parent_path().to_vec();
        path.push(underscore(&name_of(&node.name())));
        let loc = node.location();
        self.scopes.push(Scope {
            kind: ScopeKind::Module,
            path: path.into(),
            caps: Caps::none(),
        });
        self.record_range(loc.start_offset(), loc.end_offset());
        pr::visit_module_node(self, node);
        self.scopes.pop();
    }

    fn visit_class_node(&mut self, node: &pr::ClassNode<'pr>) {
        let name = name_of(&node.name());
        // ref: nodes.rb ParsedClass#controller?, #mailer?, #view_component?
        let controller = name.ends_with("Controller");
        let mailer = name.ends_with("Mailer");
        let view_component = self.view_component_file;

        // ref: nodes.rb#path_name — the full constant path, underscored, with a
        // trailing `_controller` removed from the last part.
        let mut own: Vec<String> = constant_path_parts(&node.constant_path())
            .iter()
            .map(|p| underscore(p))
            .collect();
        if controller
            && let Some(last) = own.last_mut()
            && let Some(stripped) = last.strip_suffix("_controller")
        {
            *last = stripped.to_string();
        }
        let mut path = self.parent_path().to_vec();
        path.extend(own);

        // Blocker B6: a class in a file under a configured relative root
        // supports relative keys too, not only controllers, mailers and
        // ViewComponents.
        let under_root = self.cfg.matching_root(self.path).is_some();
        let scope = Scope {
            kind: ScopeKind::Class {
                view_component,
                private_now: false,
            },
            path: path.into(),
            caps: Caps {
                relative: controller || mailer || view_component || under_root,
                candidate: controller,
            },
        };
        let loc = node.location();
        self.scopes.push(scope);
        self.record_range(loc.start_offset(), loc.end_offset());
        pr::visit_class_node(self, node);
        self.scopes.pop();
    }

    fn visit_def_node(&mut self, node: &pr::DefNode<'pr>) {
        let name = name_of(&node.name());
        // ref: visitor.rb:74-79 — privacy comes from the enclosing class.
        let (parent_path, parent_caps, view_component, private_method) =
            match self.enclosing_definition() {
                Some(Scope {
                    kind:
                        ScopeKind::Class {
                            view_component,
                            private_now,
                        },
                    path,
                    caps,
                }) => (Rc::clone(path), *caps, *view_component, *private_now),
                // A module, whose `caps` are `Caps::none()` already.
                // ref: nodes.rb ParsedModule#support_relative_keys? is false.
                //
                // The arm also covers a method, which `enclosing_definition`
                // never returns: it skips every one of them, so the parent of a
                // `def` is a class, a module or the root.
                Some(s) => (Rc::clone(&s.path), s.caps, false, false),
                None => (Rc::clone(&self.root_path), self.root_caps, false, false),
            };
        // ref: nodes.rb ParsedMethod#path — a ViewComponent collapses to the
        // class path, so the method name is not appended.
        let path = if view_component || !self.append_method_name {
            parent_path
        } else {
            let mut path = parent_path.to_vec();
            path.push(name);
            path.into()
        };
        let scope = Scope {
            kind: ScopeKind::Method,
            path,
            caps: Caps {
                // ref: nodes.rb ParsedMethod#support_relative_keys?
                relative: !private_method && parent_caps.relative,
                candidate: parent_caps.candidate,
            },
        };
        let loc = node.location();
        self.scopes.push(scope);
        self.record_range(loc.start_offset(), loc.end_offset());
        pr::visit_def_node(self, node);
        self.scopes.pop();
    }

    fn visit_call_node(&mut self, node: &pr::CallNode<'pr>) {
        match node.name().as_slice() {
            // ref: visitor.rb:95-96
            b"private" => {
                if let Some(private_now) =
                    self.scopes
                        .iter_mut()
                        .rev()
                        .find_map(|s| match &mut s.kind {
                            ScopeKind::Class { private_now, .. } => Some(private_now),
                            _ => None,
                        })
                {
                    *private_now = true;
                }
            }
            name if is_translation_name(name) => {
                let ctx = self.current_ctx();
                self.handle_translation_call(node, &ctx);
                // ref: visitor.rb:99 — a non-I18n receiver returns before
                // `super`, so the subtree is skipped.
                if let Some(recv) = node.receiver()
                    && !is_i18n_receiver(&recv)
                {
                    return;
                }
            }
            _ => {}
        }
        pr::visit_call_node(self, node);
    }
}

// ---- key resolution ----

/// ref: nodes.rb TranslationCall#full_key
///
/// Returns the candidate list, most specific first, or `None` when the call
/// resolves to nothing.
fn full_key(
    key: &str,
    scope: Option<&str>,
    ctx: &CallCtx,
    receiver_present: bool,
) -> Option<Vec<String>> {
    // ref: nodes.rb#relative_key?
    let relative = key.starts_with('.') && !receiver_present;
    if relative && !ctx.caps.relative {
        return None;
    }
    let base: Vec<String> = scope.map(|s| vec![s.to_string()]).unwrap_or_default();

    if relative && ctx.caps.candidate {
        // ref: nodes.rb:133-150 — progressively strip trailing path segments,
        // and never emit a bare unscoped key.
        let rel = &key[1..];
        let mut out = Vec::new();
        for keep in (1..=ctx.path.len()).rev() {
            let mut parts = base.clone();
            parts.extend(ctx.path[..keep].iter().cloned());
            parts.push(rel.to_string());
            out.push(join_key(&parts));
        }
        if out.is_empty() {
            return None;
        }
        Some(out)
    } else if relative {
        let mut parts = base;
        parts.extend(ctx.path.iter().cloned());
        parts.push(key[1..].to_string());
        Some(vec![join_key(&parts)])
    } else if let Some(stripped) = key.strip_prefix('.') {
        // A leading dot with an explicit receiver is not relative.
        let mut parts = base;
        parts.push(stripped.to_string());
        Some(vec![join_key(&parts)])
    } else {
        let mut parts = base;
        parts.push(key.to_string());
        Some(vec![join_key(&parts)])
    }
}

/// ref: nodes.rb — `.flatten.compact.join(".").gsub("..", ".")`
pub(super) fn join_key(parts: &[String]) -> String {
    parts
        .iter()
        .filter(|p| !p.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join(".")
        .replace("..", ".")
}

/// A pattern with no static content would mark every key used.
///
/// ref: used_keys.rb#expr_key_re — `ignore_pattern_re = /\A[.*:]*\z/`
pub(super) fn is_all_wildcard(pattern: &str) -> bool {
    pattern.chars().all(|c| c == '.' || c == '*' || c == ':')
}

/// ref: nodes.rb Root#path, generalised to any configured relative root (B6).
///
/// Strips the root, drops every extension from the file name and removes one
/// leading underscore from a partial.
pub(super) fn template_path(posix_path: &str, root: &str) -> Vec<String> {
    let root = root.trim_end_matches('/');
    let marker = format!("{root}/");
    let Some(idx) = posix_path.rfind(&marker) else {
        return Vec::new();
    };
    let rest = &posix_path[idx + marker.len()..];
    let mut parts: Vec<String> = rest.split('/').map(str::to_string).collect();
    if let Some(name) = parts.pop() {
        let stem = name.split('.').next().unwrap_or("");
        let stem = stem.strip_prefix('_').unwrap_or(stem);
        parts.push(stem.to_string());
    }
    parts
}

// ---- arguments ----

/// ref: arguments_visitor.rb
#[derive(Debug, Clone, PartialEq)]
enum ArgVal {
    Str(String),
    Int,
    Arr(Vec<ArgVal>),
    Hash(Vec<(String, ArgVal)>),
    /// A local variable or constant read. The gem keeps the node so the caller
    /// can tell the value is not static.
    Unresolvable,
    /// A call node, or anything else the gem maps to `nil`.
    Nil,
    /// Blocker B5: an interpolated string or symbol, reduced to a key pattern.
    Pattern(String),
}

/// ref: visitor.rb#process_arguments
fn process_arguments(node: &pr::CallNode) -> (Vec<ArgVal>, Vec<(String, ArgVal)>) {
    let Some(arguments) = node.arguments() else {
        return (Vec::new(), Vec::new());
    };
    let mut positional = Vec::new();
    let mut kwargs = Vec::new();
    for arg in &arguments.arguments() {
        if let Some(kw) = arg.as_keyword_hash_node() {
            kwargs = keyword_hash(&kw);
        } else {
            positional.push(reduce(&arg));
        }
    }
    (positional, kwargs)
}

fn keyword_hash(node: &pr::KeywordHashNode) -> Vec<(String, ArgVal)> {
    let mut out = Vec::new();
    for el in &node.elements() {
        // ref: arguments_visitor.rb:14 — an `**splat` is skipped.
        let Some(assoc) = el.as_assoc_node() else {
            continue;
        };
        let ArgVal::Str(key) = reduce(&assoc.key()) else {
            continue;
        };
        out.push((key, reduce(&assoc.value())));
    }
    out
}

/// ref: arguments_visitor.rb — one `ArgVal` per node kind.
///
/// A chain rather than a `match`, because `pr::Node`'s variants carry no public
/// fields: matching one proves the kind but not the value, which still has to
/// come from the matching `as_*_node`. Each of those answers for exactly one
/// kind, so the arms are disjoint and the order does not matter.
fn reduce(node: &pr::Node) -> ArgVal {
    if let Some(n) = node.as_string_node() {
        return ArgVal::Str(String::from_utf8_lossy(n.unescaped()).into_owned());
    }
    if let Some(n) = node.as_symbol_node() {
        return ArgVal::Str(String::from_utf8_lossy(n.unescaped()).into_owned());
    }
    if node.as_integer_node().is_some() {
        return ArgVal::Int;
    }
    if let Some(n) = node.as_array_node() {
        return ArgVal::Arr(n.elements().iter().map(|e| reduce(&e)).collect());
    }
    // Shorthand `scope:` wraps the value in an ImplicitNode.
    // ref: arguments_visitor.rb:38-40
    if let Some(n) = node.as_implicit_node() {
        return reduce(&n.value());
    }
    // Blocker B5: build a key pattern from the static parts.
    if let Some(n) = node.as_interpolated_string_node() {
        return ArgVal::Pattern(interpolated_pattern(&n.parts()));
    }
    if let Some(n) = node.as_interpolated_symbol_node() {
        return ArgVal::Pattern(interpolated_pattern(&n.parts()));
    }
    // Only ever an array element — `t([a: 1])`. `process_arguments` takes the
    // call's own keyword hash before it reduces anything, and an `assoc` value
    // is never a keyword hash. A braced `t({a: 1})` is a `HashNode`, which
    // falls through to `Nil`.
    if let Some(n) = node.as_keyword_hash_node() {
        return ArgVal::Hash(keyword_hash(&n));
    }
    if matches!(
        node,
        pr::Node::LocalVariableReadNode { .. }
            | pr::Node::ConstantReadNode { .. }
            | pr::Node::ConstantPathNode { .. }
            | pr::Node::InstanceVariableReadNode { .. }
    ) {
        return ArgVal::Unresolvable;
    }
    ArgVal::Nil
}

/// `t("foo.#{bar}.title")` becomes `foo.*:.title`.
///
/// `*:` is "part of exactly one segment", which is the replacement the gem uses
/// for the same job in `used_keys.rb#replace_key_exp`.
fn interpolated_pattern(parts: &pr::NodeList) -> String {
    let mut out = String::new();
    for part in parts {
        if let Some(n) = part.as_string_node() {
            out.push_str(&String::from_utf8_lossy(n.unescaped()));
        } else if let Some(n) = part.as_interpolated_string_node() {
            out.push_str(&interpolated_pattern(&n.parts()));
        } else {
            // Any `#{...}` becomes a single-segment wildcard.
            out.push_str("*:");
        }
    }
    out
}

struct ScopeError;

/// ref: nodes.rb#scope (lines 166-182)
fn resolve_scope(kwargs: &[(String, ArgVal)]) -> Result<Option<String>, ScopeError> {
    let Some((_, value)) = kwargs.iter().find(|(k, _)| k == "scope") else {
        return Ok(None);
    };
    // `Array(value)` then "all entries are String or Symbol", so anything else
    // is a ScopeError, which drops the occurrence.
    let parts: Vec<String> = match value {
        ArgVal::Str(s) => vec![s.clone()],
        ArgVal::Arr(items) => {
            let mut out = Vec::with_capacity(items.len());
            for i in items {
                match i {
                    ArgVal::Str(s) => out.push(s.clone()),
                    _ => return Err(ScopeError),
                }
            }
            out
        }
        _ => return Err(ScopeError),
    };
    // `Array(nil)` is empty, and an empty scope is a ScopeError. See PR #731:
    // a falsey scope and an absent scope are different cases.
    if parts.is_empty() {
        return Err(ScopeError);
    }
    Ok(Some(parts.join(".")))
}

// ---- helpers ----

fn name_of(id: &pr::ConstantId) -> String {
    String::from_utf8_lossy(id.as_slice()).into_owned()
}

/// ref: visitor.rb:98 — the `t`-family method names.
///
/// The name is compared as raw bytes because every call node in the file
/// reaches this test, and a `String` per node buys nothing.
fn is_translation_name(name: &[u8]) -> bool {
    matches!(name, b"t" | b"t!" | b"translate" | b"translate!")
}

/// ref: visitor.rb#i18n_receiver? (lines 188-197)
fn is_i18n_receiver(recv: &pr::Node) -> bool {
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
fn constant_path_parts(node: &pr::Node) -> Vec<String> {
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

/// ref: visitor.rb#MAGIC_COMMENT_PREFIX = /\A.\s*i18n-tasks-use\s+/
///
/// The leading `.` matches the comment's own `#`.
fn strip_magic_prefix(text: &str) -> Option<&str> {
    let mut chars = text.char_indices();
    let (_, _first) = chars.next()?;
    let rest_start = chars.clone().next().map_or(text.len(), |(i, _)| i);
    let rest = &text[rest_start..];
    let trimmed = rest.trim_start_matches([' ', '\t']);
    let payload = trimmed.strip_prefix(MAGIC_COMMENT_MARKER)?;
    // The prefix requires at least one space after the marker.
    if !payload.starts_with([' ', '\t']) {
        return None;
    }
    Some(payload.trim_start())
}

/// ref: ruby_scanner.rb:87 — `split(/\s+(?=t)/)`
fn split_calls(payload: &str) -> Vec<&str> {
    let bytes = payload.as_bytes();
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i].is_ascii_whitespace() {
            let ws_start = i;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b't' && ws_start > start {
                out.push(&payload[start..ws_start]);
                start = i;
            }
        } else {
            i += 1;
        }
    }
    if start < payload.len() {
        out.push(&payload[start..]);
    }
    if out.is_empty() {
        out.push(payload);
    }
    out
}

/// A `t`-family call reduced to owned data.
///
/// Blocker B9: node lifetimes end with the `ParseResult`, so everything is
/// extracted during the visit.
struct ExtractedCall {
    args: Vec<ArgVal>,
    kwargs: Vec<(String, ArgVal)>,
    receiver_present: bool,
    receiver_is_i18n: bool,
}

/// Collects the `t`-family calls in a nested (magic comment) parse.
fn collect_translation_calls(node: &pr::Node) -> Vec<ExtractedCall> {
    struct Collector {
        found: Vec<ExtractedCall>,
    }
    impl<'pr> pr::Visit<'pr> for Collector {
        fn visit_call_node(&mut self, node: &pr::CallNode<'pr>) {
            if is_translation_name(node.name().as_slice()) {
                let (args, kwargs) = process_arguments(node);
                let receiver = node.receiver();
                self.found.push(ExtractedCall {
                    args,
                    kwargs,
                    receiver_present: receiver.is_some(),
                    receiver_is_i18n: receiver.as_ref().is_some_and(is_i18n_receiver),
                });
            }
            pr::visit_call_node(self, node);
        }
    }
    let mut c = Collector { found: Vec::new() };
    c.visit(node);
    c.found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn magic_comment_prefix() {
        // ref: /\A.\s*i18n-tasks-use\s+/
        assert_eq!(
            strip_magic_prefix("# i18n-tasks-use t('a')"),
            Some("t('a')")
        );
        assert_eq!(strip_magic_prefix("#i18n-tasks-use t('a')"), Some("t('a')"));
        assert_eq!(
            strip_magic_prefix("#   i18n-tasks-use   t('a')"),
            Some("t('a')")
        );
        assert_eq!(strip_magic_prefix("# not a magic comment"), None);
        // The marker needs whitespace after it.
        assert_eq!(strip_magic_prefix("# i18n-tasks-uset('a')"), None);
    }

    /// `full_key` never returns an empty candidate list. With a context that
    /// has no path at all there is nothing to build a candidate from, so a
    /// relative key resolves to nothing rather than to a bare key.
    #[test]
    fn a_candidate_context_with_no_path_resolves_to_nothing() {
        let ctx = CallCtx {
            path: Rc::from([]),
            caps: Caps {
                relative: true,
                candidate: true,
            },
        };
        assert_eq!(full_key(".rel", None, &ctx, false), None);
        // An absolute key still resolves, path or no path.
        assert_eq!(
            full_key("abs", None, &ctx, false),
            Some(vec!["abs".to_string()])
        );
        // With a path, the candidates run from the most specific down, and
        // never as far as a bare key.
        let ctx = CallCtx {
            path: Rc::from(["events".to_string(), "create".to_string()]),
            caps: Caps {
                relative: true,
                candidate: true,
            },
        };
        assert_eq!(
            full_key(".rel", None, &ctx, false),
            Some(vec![
                "events.create.rel".to_string(),
                "events.rel".to_string()
            ])
        );
    }

    #[test]
    fn splits_several_calls_in_one_comment() {
        // ref: ruby_scanner.rb:87 — split(/\s+(?=t)/)
        assert_eq!(split_calls("t('a') t('b')"), vec!["t('a')", "t('b')"]);
        assert_eq!(split_calls("t('a')"), vec!["t('a')"]);
        // The fallback: nothing to split, so the payload comes back whole.
        assert_eq!(split_calls(""), vec![""]);
        // Only whitespace directly before a `t` splits, so a `scope:` argument
        // on the same line stays with its call.
        assert_eq!(split_calls("t('a', scope: :x)"), vec!["t('a', scope: :x)"]);
        assert_eq!(
            split_calls("t('a', scope: :x) t('b')"),
            vec!["t('a', scope: :x)", "t('b')"]
        );
    }

    #[test]
    fn template_paths_drop_every_extension_and_the_partial_underscore() {
        // The view cases from spec/relative_keys_spec.rb.
        assert_eq!(
            template_path("app/views/movies/show.html.slim", "app/views"),
            vec!["movies", "show"]
        );
        assert_eq!(
            template_path("app/views-mobile/movies/show.html.slim", "app/views-mobile"),
            vec!["movies", "show"]
        );
        // A leading underscore marks a partial and is stripped once.
        assert_eq!(
            template_path("app/views/application/_event.html.erb", "app/views"),
            vec!["application", "event"]
        );
        assert_eq!(
            template_path("app/views/index.html.erb", "app/views"),
            vec!["index"]
        );
    }

    /// A root that is nowhere in the path yields no template path at all.
    #[test]
    fn a_path_outside_the_root_has_no_template_path() {
        assert!(template_path("lib/tasks/thing.rb", "app/views").is_empty());
        assert!(template_path("app/view/x.html.erb", "app/views").is_empty());
    }

    #[test]
    fn all_wildcard_patterns_are_rejected() {
        // ref: used_keys.rb#expr_key_re — /\A[.*:]*\z/
        assert!(is_all_wildcard("*:"));
        assert!(is_all_wildcard("*:.*:"));
        assert!(is_all_wildcard(".*:."));
        assert!(!is_all_wildcard("hash.*:"));
    }

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

    #[test]
    fn join_key_collapses_double_dots() {
        assert_eq!(join_key(&["a".into(), "b".into()]), "a.b");
        assert_eq!(join_key(&["a.".into(), "b".into()]), "a.b");
    }

    /// The last statement of `src` must be a call; `f` receives its arguments.
    /// The closure is what keeps the parse result alive around them.
    fn last_call_arguments<T>(src: &str, f: impl FnOnce(pr::ArgumentsNode) -> T) -> T {
        let parsed = pr::parse(src.as_bytes());
        let node = parsed.node();
        let program = node.as_program_node().unwrap();
        let statements = program.statements();
        let last = statements.body().iter().last().unwrap();
        let call = last.as_call_node().unwrap();
        f(call.arguments().unwrap())
    }

    fn reduce_argument(src: &str) -> ArgVal {
        last_call_arguments(src, |args| reduce(&args.arguments().iter().next().unwrap()))
    }

    /// `reduce` decides three observable outcomes at `record_call`: a key, a
    /// pattern, or "not a key" — and "not a key" splits again into the silent
    /// kinds (`Int`, `Arr`, `Hash`) and the reported ones (`Unresolvable`,
    /// `Nil`). Each node kind must land on its own variant, so the mapping is
    /// pinned here rather than only through the outcomes, which collapse it.
    #[test]
    fn reduce_maps_each_node_kind_to_its_own_argval() {
        // Literals: the only two kinds that are a key.
        assert_eq!(reduce_argument("t('a')"), ArgVal::Str("a".into()));
        assert_eq!(reduce_argument("t(:a)"), ArgVal::Str("a".into()));
        // Escapes come from `unescaped`, not from the source slice.
        assert_eq!(reduce_argument(r#"t("a\nb")"#), ArgVal::Str("a\nb".into()));

        // Silent: nothing is recorded, not even an opaque call.
        assert_eq!(reduce_argument("t(1)"), ArgVal::Int);
        assert_eq!(
            reduce_argument("t([:a, 'b'])"),
            ArgVal::Arr(vec![ArgVal::Str("a".into()), ArgVal::Str("b".into())])
        );

        // Blocker B5: an interpolation becomes a single-segment wildcard.
        assert_eq!(
            reduce_argument(r#"t("a.#{b}.c")"#),
            ArgVal::Pattern("a.*:.c".into())
        );
        assert_eq!(
            reduce_argument(r#"t(:"a.#{b}")"#),
            ArgVal::Pattern("a.*:".into())
        );

        // Reported as opaque: a read the gem keeps the node for.
        for src in ["x = 1\nt(x)", "t(X)", "t(X::Y)", "t(@x)"] {
            assert_eq!(reduce_argument(src), ArgVal::Unresolvable, "{src}");
        }

        // Reported as opaque: everything the gem maps to `nil`.
        for src in ["t(nil)", "t(build_key)", "t({a: 1})", "t(1..2)"] {
            assert_eq!(reduce_argument(src), ArgVal::Nil, "{src}");
        }

        // `[a: 1]` is an array holding a keyword hash, which is the one way
        // a `KeywordHashNode` reaches `reduce`: `process_arguments` takes the
        // call's own keyword hash before it reduces anything.
        assert_eq!(
            reduce_argument("t([a: 1])"),
            ArgVal::Arr(vec![ArgVal::Hash(vec![("a".to_string(), ArgVal::Int)])])
        );

        // Shorthand `scope:` wraps its value, so the wrapper must be seen
        // through. ref: arguments_visitor.rb:38-40
        let kwargs = last_call_arguments("scope = 1\nt('a', scope:)", |args| {
            let hash = args.arguments().iter().last().unwrap();
            keyword_hash(&hash.as_keyword_hash_node().unwrap())
        });
        assert_eq!(kwargs, vec![("scope".to_string(), ArgVal::Unresolvable)]);
    }
}
