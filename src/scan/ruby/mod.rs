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
//!
//! This module holds the walk itself — the scopes it pushes and pops, and what
//! it does with a `t` call. The four questions the walk asks are next door:
//! `key` resolves a key against its scope, `args` reduces the call's
//! arguments, `magic` reads an `i18n-tasks-use` comment, and `nodes`
//! answers the small questions about one Prism node.

mod args;
mod key;
mod magic;
mod nodes;

use args::{ArgVal, ScopeError, process_arguments, resolve_scope};
use key::full_key;
use magic::{collect_translation_calls, split_calls, strip_magic_prefix};
use nodes::{constant_path_parts, is_i18n_receiver, is_translation_name, name_of};

pub(in crate::scan) use key::{is_all_wildcard, join_key, template_path};

use super::{FileScan, Locator, Occurrence, ScanConfig, SourceMap};
use crate::keys::underscore;
use crate::lineindex::LineIndex;
use ruby_prism as pr;
use ruby_prism::Visit as _;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

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
    /// One allocation for the file, cloned into every occurrence it produces.
    path: Arc<Path>,
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
    /// Blocker B6: whether the file lies under a configured relative root.
    /// The answer belongs to the file, so `new` asks once and every class node
    /// reads it.
    under_root: bool,
    view_component_file: bool,
    out: FileScan,
}

impl<'a> Visitor<'a> {
    fn new(path: &Path, cfg: &ScanConfig, loc: Locator<'a>) -> Visitor<'a> {
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
            path: Arc::from(path),
            loc,
            scopes: Vec::new(),
            ranges: Vec::new(),
            root_path: root_path.into(),
            root_caps: Caps {
                relative: relative_root.is_some(),
                candidate: false,
            },
            append_method_name: !root.is_some_and(|r| cfg.skips_method_name(r)),
            under_root: root.is_some(),
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
            path: Arc::clone(&self.path),
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
                path: Arc::clone(&self.path),
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

        let scope = Scope {
            kind: ScopeKind::Class {
                view_component,
                private_now: false,
            },
            path: path.into(),
            caps: Caps {
                // Blocker B6: a class in a file under a configured relative
                // root supports relative keys too, not only controllers,
                // mailers and ViewComponents.
                relative: controller || mailer || view_component || self.under_root,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The relative root belongs to the file, not to the node: a file with
    /// twenty classes must not ask the question twenty times. `Visitor::new`
    /// already has the answer.
    #[test]
    fn the_relative_root_is_looked_up_once_per_file() {
        let cfg = ScanConfig {
            relative_roots: vec!["app/views".to_string(), "app/controllers".to_string()],
            relative_exclude_method_name_paths: Vec::new(),
        };
        let path = Path::new("app/controllers/books_controller.rb");
        let one = b"class BooksController
  def show
    t('.x')
  end
end
"
        .to_vec();
        let mut many = Vec::new();
        for i in 0..20 {
            many.extend_from_slice(
                format!(
                    "class C{i}Controller
  def show
    t('.x')
  end
end
"
                )
                .as_bytes(),
            );
        }

        let before = crate::scan::root_lookups_on_this_thread();
        let scan_one = scan(&one, path, &cfg);
        let for_one = crate::scan::root_lookups_on_this_thread() - before;

        let before = crate::scan::root_lookups_on_this_thread();
        let scan_many = scan(&many, path, &cfg);
        let for_many = crate::scan::root_lookups_on_this_thread() - before;

        assert_eq!(scan_one.keys.len(), 1);
        assert_eq!(scan_many.keys.len(), 20);
        assert_eq!(for_one, 1);
        assert_eq!(
            for_many, for_one,
            "the lookup count follows the class count"
        );
    }
}
