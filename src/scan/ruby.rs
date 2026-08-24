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
    for (offset, text) in comments {
        v.handle_magic_comment(offset, &text);
    }
    v.out
}

/// A lexical scope. `path` is already resolved against the parent, so it needs
/// no further walking.
#[derive(Debug, Clone)]
enum Scope {
    /// ref: nodes.rb ParsedModule — never supports relative keys.
    Module {
        path: Vec<String>,
    },
    Class(ClassScope),
    Method(MethodScope),
}

#[derive(Debug, Clone)]
struct ClassScope {
    path: Vec<String>,
    view_component: bool,
    /// Flipped by a bare `private`. ref: visitor.rb:95-96
    private_now: bool,
    /// ref: nodes.rb ParsedClass#support_relative_keys?
    supports_relative: bool,
    /// ref: nodes.rb ParsedClass#support_candidate_keys?
    supports_candidate: bool,
}

#[derive(Debug, Clone)]
struct MethodScope {
    path: Vec<String>,
    supports_relative: bool,
    supports_candidate: bool,
}

impl Scope {
    fn path(&self) -> &[String] {
        match self {
            Scope::Module { path } => path,
            Scope::Class(c) => &c.path,
            Scope::Method(m) => &m.path,
        }
    }
}

/// Everything a `t` call needs from its enclosing scope.
///
/// ref: nodes.rb TranslationCall#support_relative_keys?, which requires the
/// parent to be a `ParsedMethod` or the `Root`.
#[derive(Debug, Clone)]
struct CallCtx {
    path: Vec<String>,
    supports_relative: bool,
    supports_candidate: bool,
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
    root_path: Vec<String>,
    /// ref: nodes.rb Root#rails_view?
    root_supports_relative: bool,
    /// ref: relative_keys.rb:26-28
    append_method_name: bool,
    view_component_file: bool,
    out: FileScan,
}

impl<'a> Visitor<'a> {
    fn new(path: &'a Path, cfg: &'a ScanConfig, loc: Locator<'a>) -> Visitor<'a> {
        let posix = path.to_string_lossy().replace('\\', "/");
        let is_rb = posix.ends_with(".rb");
        let root = cfg.matching_root(path);
        // Blocker B6: the gem hardcodes `app/views/` and `app/components/`.
        // Any configured relative root counts, and a `.rb` file never counts as
        // a template, which is what keeps `app/controllers/*.rb` out.
        let root_supports_relative = root.is_some() && !is_rb;
        let root_path = if root_supports_relative {
            template_path(&posix, root.unwrap())
        } else {
            Vec::new()
        };
        Visitor {
            path,
            cfg,
            loc,
            scopes: Vec::new(),
            ranges: Vec::new(),
            root_path,
            root_supports_relative,
            append_method_name: !root.is_some_and(|r| cfg.skips_method_name(r)),
            view_component_file: posix.contains("app/components/"),
            out: FileScan::default(),
        }
    }

    fn root_ctx(&self) -> CallCtx {
        CallCtx {
            path: self.root_path.clone(),
            supports_relative: self.root_supports_relative,
            // ref: nodes.rb Root#support_candidate_keys? is always false.
            supports_candidate: false,
        }
    }

    fn current_ctx(&self) -> CallCtx {
        match self.scopes.last() {
            None => self.root_ctx(),
            Some(Scope::Method(m)) => CallCtx {
                path: m.path.clone(),
                supports_relative: m.supports_relative,
                supports_candidate: m.supports_candidate,
            },
            // The parent is a class or a module, so relative keys never resolve.
            Some(s) => CallCtx {
                path: s.path().to_vec(),
                supports_relative: false,
                supports_candidate: false,
            },
        }
    }

    fn parent_path(&self) -> Vec<String> {
        match self.scopes.last() {
            Some(s) => s.path().to_vec(),
            None => self.root_path.clone(),
        }
    }

    /// A `def` attaches to the enclosing class or module, never to an enclosing
    /// `def`. ref: visitor.rb#visit_def_node, which reads
    /// `@current_class || @current_module || @root`.
    fn enclosing_definition(&self) -> Option<&Scope> {
        self.scopes
            .iter()
            .rev()
            .find(|s| !matches!(s, Scope::Method(_)))
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
            .map(|r| r.ctx.clone())
            .unwrap_or_else(|| self.root_ctx())
    }

    fn handle_magic_comment(&mut self, offset: usize, text: &[u8]) {
        let Ok(text) = std::str::from_utf8(text) else {
            return;
        };
        let Some(payload) = strip_magic_prefix(text) else {
            return;
        };
        // ref: ruby_scanner.rb:203 — `delete("#")` then `strip`.
        let payload: String = payload.chars().filter(|c| *c != '#').collect();
        let payload = payload.trim();
        if payload.is_empty() {
            return;
        }
        // The gem splits several calls on `/\s+(?=t)/` and rejoins with "; ".
        // ref: ruby_scanner.rb:87
        let joined = split_calls(payload).join("; ");
        let nested = pr::parse(joined.as_bytes());
        if nested.errors().count() > 0 {
            return;
        }
        let ctx = self.resolve_comment_ctx(offset);
        let calls = collect_translation_calls(&nested.node());
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
    }
}

impl<'pr> pr::Visit<'pr> for Visitor<'_> {
    fn visit_module_node(&mut self, node: &pr::ModuleNode<'pr>) {
        let mut path = self.parent_path();
        path.push(underscore(&name_of(node.name())));
        let loc = node.location();
        self.scopes.push(Scope::Module { path });
        self.record_range(loc.start_offset(), loc.end_offset());
        pr::visit_module_node(self, node);
        self.scopes.pop();
    }

    fn visit_class_node(&mut self, node: &pr::ClassNode<'pr>) {
        let name = name_of(node.name());
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
        let mut path = self.parent_path();
        path.extend(own);

        // Blocker B6: a class in a file under a configured relative root
        // supports relative keys too, not only controllers, mailers and
        // ViewComponents.
        let under_root = self.cfg.matching_root(self.path).is_some();
        let scope = ClassScope {
            path,
            view_component,
            private_now: false,
            supports_relative: controller || mailer || view_component || under_root,
            supports_candidate: controller,
        };
        let loc = node.location();
        self.scopes.push(Scope::Class(scope));
        self.record_range(loc.start_offset(), loc.end_offset());
        pr::visit_class_node(self, node);
        self.scopes.pop();
    }

    fn visit_def_node(&mut self, node: &pr::DefNode<'pr>) {
        let name = name_of(node.name());
        let (parent_path, parent_relative, parent_candidate, parent_view_component) =
            match self.enclosing_definition() {
                Some(Scope::Class(c)) => (
                    c.path.clone(),
                    c.supports_relative,
                    c.supports_candidate,
                    c.view_component,
                ),
                // ref: nodes.rb ParsedModule#support_relative_keys? is false.
                Some(Scope::Module { path }) => (path.clone(), false, false, false),
                // Unreachable: `enclosing_definition` skips every method, so
                // the parent of a `def` is a class, a module or the root. Kept
                // for exhaustiveness, and it must stay consistent with the
                // class arm if that ever changes.
                Some(Scope::Method(m)) => (
                    m.path.clone(),
                    m.supports_relative,
                    m.supports_candidate,
                    false,
                ),
                None => (
                    self.root_path.clone(),
                    self.root_supports_relative,
                    false,
                    false,
                ),
            };
        // ref: nodes.rb ParsedMethod#path — a ViewComponent collapses to the
        // class path, so the method name is not appended.
        let mut path = parent_path;
        if !parent_view_component && self.append_method_name {
            path.push(name.clone());
        }
        // ref: visitor.rb:74-79 — privacy comes from the enclosing class.
        let private_method =
            matches!(self.enclosing_definition(), Some(Scope::Class(c)) if c.private_now);
        let scope = MethodScope {
            path,
            // ref: nodes.rb ParsedMethod#support_relative_keys?
            supports_relative: !private_method && parent_relative,
            supports_candidate: parent_candidate,
        };
        let loc = node.location();
        self.scopes.push(Scope::Method(scope));
        self.record_range(loc.start_offset(), loc.end_offset());
        pr::visit_def_node(self, node);
        self.scopes.pop();
    }

    fn visit_call_node(&mut self, node: &pr::CallNode<'pr>) {
        match name_of(node.name()).as_str() {
            // ref: visitor.rb:95-96
            "private" => {
                if let Some(Scope::Class(c)) = self
                    .scopes
                    .iter_mut()
                    .rev()
                    .find(|s| matches!(s, Scope::Class(_)))
                {
                    c.private_now = true;
                }
            }
            "t" | "t!" | "translate" | "translate!" => {
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
    if relative && !ctx.supports_relative {
        return None;
    }
    let base: Vec<String> = scope.map(|s| vec![s.to_string()]).unwrap_or_default();

    if relative && ctx.supports_candidate {
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
    for arg in arguments.arguments().iter() {
        match &arg {
            pr::Node::KeywordHashNode { .. } => {
                if let Some(kw) = arg.as_keyword_hash_node() {
                    kwargs = keyword_hash(&kw);
                }
            }
            _ => positional.push(reduce(&arg)),
        }
    }
    (positional, kwargs)
}

fn keyword_hash(node: &pr::KeywordHashNode) -> Vec<(String, ArgVal)> {
    let mut out = Vec::new();
    for el in node.elements().iter() {
        // ref: arguments_visitor.rb:14 — an `**splat` is skipped.
        let Some(assoc) = el.as_assoc_node() else {
            continue;
        };
        let key = match reduce(&assoc.key()) {
            ArgVal::Str(s) => s,
            _ => continue,
        };
        out.push((key, reduce(&assoc.value())));
    }
    out
}

fn reduce(node: &pr::Node) -> ArgVal {
    match node {
        pr::Node::StringNode { .. } => {
            let n = node.as_string_node().unwrap();
            ArgVal::Str(String::from_utf8_lossy(n.unescaped()).into_owned())
        }
        pr::Node::SymbolNode { .. } => {
            let n = node.as_symbol_node().unwrap();
            ArgVal::Str(String::from_utf8_lossy(n.unescaped()).into_owned())
        }
        pr::Node::IntegerNode { .. } => ArgVal::Int,
        pr::Node::ArrayNode { .. } => {
            let n = node.as_array_node().unwrap();
            ArgVal::Arr(n.elements().iter().map(|e| reduce(&e)).collect())
        }
        // Shorthand `scope:` wraps the value in an ImplicitNode.
        // ref: arguments_visitor.rb:38-40
        pr::Node::ImplicitNode { .. } => {
            let n = node.as_implicit_node().unwrap();
            reduce(&n.value())
        }
        pr::Node::LocalVariableReadNode { .. }
        | pr::Node::ConstantReadNode { .. }
        | pr::Node::ConstantPathNode { .. }
        | pr::Node::InstanceVariableReadNode { .. } => ArgVal::Unresolvable,
        // Blocker B5: build a key pattern from the static parts.
        pr::Node::InterpolatedStringNode { .. } => {
            let n = node.as_interpolated_string_node().unwrap();
            ArgVal::Pattern(interpolated_pattern(n.parts()))
        }
        pr::Node::InterpolatedSymbolNode { .. } => {
            let n = node.as_interpolated_symbol_node().unwrap();
            ArgVal::Pattern(interpolated_pattern(n.parts()))
        }
        // Unreachable: `process_arguments` takes the keyword hash before it
        // reduces anything, and an `assoc` value is never a keyword hash. A
        // braced `{a: 1}` is a `HashNode`, which falls through to `Nil`.
        pr::Node::KeywordHashNode { .. } => {
            let n = node.as_keyword_hash_node().unwrap();
            ArgVal::Hash(keyword_hash(&n))
        }
        _ => ArgVal::Nil,
    }
}

/// `t("foo.#{bar}.title")` becomes `foo.*:.title`.
///
/// `*:` is "part of exactly one segment", which is the replacement the gem uses
/// for the same job in `used_keys.rb#replace_key_exp`.
fn interpolated_pattern(parts: pr::NodeList) -> String {
    let mut out = String::new();
    for part in parts.iter() {
        match &part {
            pr::Node::StringNode { .. } => {
                let n = part.as_string_node().unwrap();
                out.push_str(&String::from_utf8_lossy(n.unescaped()));
            }
            pr::Node::InterpolatedStringNode { .. } => {
                let n = part.as_interpolated_string_node().unwrap();
                out.push_str(&interpolated_pattern(n.parts()));
            }
            // Any `#{...}` becomes a single-segment wildcard.
            _ => out.push_str("*:"),
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

fn name_of(id: pr::ConstantId) -> String {
    String::from_utf8_lossy(id.as_slice()).into_owned()
}

/// ref: visitor.rb#i18n_receiver? (lines 188-197)
fn is_i18n_receiver(recv: &pr::Node) -> bool {
    match recv {
        pr::Node::ConstantReadNode { .. } => {
            name_of(recv.as_constant_read_node().unwrap().name()) == "I18n"
        }
        // `::I18n` — no parent, and the name is I18n.
        pr::Node::ConstantPathNode { .. } => {
            let n = recv.as_constant_path_node().unwrap();
            n.parent().is_none() && n.name().map(name_of).as_deref() == Some("I18n")
        }
        _ => false,
    }
}

/// The parts of a constant path, as `full_name_parts` returns them.
fn constant_path_parts(node: &pr::Node) -> Vec<String> {
    match node {
        pr::Node::ConstantReadNode { .. } => {
            vec![name_of(node.as_constant_read_node().unwrap().name())]
        }
        pr::Node::ConstantPathNode { .. } => {
            let n = node.as_constant_path_node().unwrap();
            let mut parts = n
                .parent()
                .map(|p| constant_path_parts(&p))
                .unwrap_or_default();
            if let Some(name) = n.name() {
                parts.push(name_of(name));
            }
            parts
        }
        _ => Vec::new(),
    }
}

/// ref: visitor.rb#MAGIC_COMMENT_PREFIX = /\A.\s*i18n-tasks-use\s+/
///
/// The leading `.` matches the comment's own `#`.
fn strip_magic_prefix(text: &str) -> Option<&str> {
    let mut chars = text.char_indices();
    let (_, _first) = chars.next()?;
    let rest_start = chars.clone().next().map(|(i, _)| i).unwrap_or(text.len());
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
            if matches!(
                name_of(node.name()).as_str(),
                "t" | "t!" | "translate" | "translate!"
            ) {
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
            path: Vec::new(),
            supports_relative: true,
            supports_candidate: true,
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
            path: vec!["events".into(), "create".into()],
            supports_relative: true,
            supports_candidate: true,
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

    #[test]
    fn join_key_collapses_double_dots() {
        assert_eq!(join_key(&["a".into(), "b".into()]), "a.b");
        assert_eq!(join_key(&["a.".into(), "b".into()]), "a.b");
    }
}
