//! The `t` call's arguments, reduced to values the scanner can decide on.
//!
//! ref: lib/i18n/tasks/scanners/prism_scanners/arguments_visitor.rb

use ruby_prism as pr;

/// ref: arguments_visitor.rb
#[derive(Debug, Clone, PartialEq)]
pub(super) enum ArgVal {
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
pub(super) fn process_arguments(node: &pr::CallNode) -> (Vec<ArgVal>, Vec<(String, ArgVal)>) {
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

pub(super) struct ScopeError;

/// ref: nodes.rb#scope (lines 166-182)
pub(super) fn resolve_scope(kwargs: &[(String, ArgVal)]) -> Result<Option<String>, ScopeError> {
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

#[cfg(test)]
mod tests {
    use super::*;

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
