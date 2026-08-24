# Design notes

Why this port looks the way it does. Source comments refer to the numbered
items here — "design decision 3", "blocker B5", "section 3" — so keep the
numbering stable.

The Ruby gem, [`i18n-tasks`](https://github.com/glebm/i18n-tasks), is the
reference implementation. Every deliberate behavioural difference is written up
in [`accepted-diffs.md`](accepted-diffs.md); anything not listed there is a bug.

---

## 1. Five design decisions

Scanning is only about a third of the cost of `unused`. A port that swaps only
the parser gets roughly 1.3×, which the gem's own parser-versus-Prism benchmark
shows. The wins are in the data layer.

1. **Flat key map, not a node tree.** `HashMap<KeyPath, Value>` with interned
   segments. The gem's cost comes from `select_nodes` deep-copying every
   matching node through `node.derive`
   (`data/tree/traversal.rb:93-128`) — measured at 2.04 s of 5.5 s.
2. **Scan once.** The used-key set does not depend on the locale. The gem
   recomputes `used_tree` per locale (`unused_keys.rb:16`,
   `missing_keys.rb:111`), about 37% of `unused`, and parses the whole source
   tree twice, strict and non-strict (`used_keys.rb:143`), a further 22%.
3. **One Prism parse per file, ERB included.** The gem parses each ERB tag
   separately (`erb_ast_scanner.rb:107`), so a view with 150 tags means 150
   parses. This port concatenates every tag of a file into one synthetic Ruby
   buffer with a source map and parses that once. It also removes the gem's
   `ignore_blocks` hack in `local_ruby_parser.rb`, which strips a trailing
   `do |x|` because a per-tag parse breaks blocks. A concatenated buffer keeps
   `<% if %>...<% end %>` intact, and Prism is error-tolerant.
4. **`rayon` over the file list, not the scanner list.** The gem removed its
   concurrency in PR #687 because threads broke on shared IO and `warn`. Here
   each file scan is a pure function from bytes to occurrences.
5. **The per-file scan stays a pure function** — `fn(&[u8], &Path) -> FileScan`,
   with a `Serialize` `Occurrence`.

Two O(n²) bugs in the gem this port does not copy:

- `occurrence_from_position.rb:20` does `contents[0..position].count("\n")` per
  occurrence. Use a precomputed line-offset index (`src/lineindex.rs`).
- `pattern_scanner.rb:88` re-reads the whole file from disk per occurrence to
  grep backwards for `def`. Take the enclosing method name from the AST.

---

## 2. Resolved constraints

Each of these forced an architectural choice. They are decided; the comments in
`src/` cite them by number.

### B1. The YAML emitter cannot match Psych byte for byte

`FileFormats#normalized?` in `lib/i18n/tasks/data/file_formats.rb` is **exact
string equality** between the file on disk and Psych's output, so Psych
*defines* "normalized" for the gem. Psych quotes only when the grammar requires
it, normalizes block scalar styles (`|+`→`|`, `>`→`|`, `>+`→`|`, `>-`→`|-`),
folds long lines at `line_width`, escapes non-BMP characters as `\Uxxxxxxxx`,
and emits trailing spaces in some folded output.

**Decision.** Hand-write the emitter (`src/data/emit.rs`); no general YAML
emitter. Never fold lines, which removes the whole line-width class of bugs and
keeps diffs stable. Correctness is *value preservation plus idempotence*, not
byte equality with Psych.

**Consequence.** The first `normalize` run against a project set up for the gem
rewrites nearly every locale file. Land it as one dedicated, reviewed commit,
and drop the Ruby `check-normalized` from CI in that same commit.

### B2. Ruby regex features the Rust `regex` crate does not have

`key_pattern_matching.rb:26-39` compiles the key-pattern DSL to a Ruby regex
using lookbehind and lookahead — `:` becomes `(?<=^|\.)[^.]+?(?=\.|$)` — plus
capture groups, because `data.write` refers to them as `\1`.
`interpolations.rb` uses negative lookbehind: `/(?<!%)%{[^}]+}/`. The `regex`
crate supports neither lookaround nor backreferences.

**Decision.** Do not translate the DSL to a regex. `src/pattern.rs` is a
segment-wise matcher over the already-split key path, and the interpolation
scan is hand-rolled over the value bytes.

### B3. The gem's config file runs Ruby

`configuration.rb:26` is
`YAML.load(eval(Erubi::Engine.new(File.read(file)).src))`. That config can boot
Rails, shell out, and register scanner classes.

**Decision.** A new plain-YAML config, `config/i18n-tasks-rs.yml`. No code
execution, ever. `migrate-config` converts a gem config into it.

### B4. Psych parses `:foo.bar` as a Ruby Symbol, and the gem's reference feature depends on it

`data/tree/node.rb` defines `reference?` as `value.is_a?(Symbol)`, and the YAML
adapter passes `permitted_classes: [Symbol]` to keep it working. That is Psych
behaviour, not YAML behaviour.

**Decision.** Drop the reference subsystem. **Error** on any unquoted scalar
matching `^:[\w.]+$`, naming the file and line, so one cannot pass silently and
get emitted as a plain string.

### B5. Prism mode drops dynamic keys, which makes `unused` unsafe

`prism_scanners/arguments_visitor.rb:50-60` returns `nil` for every interpolated
string or symbol node, and `TranslationCall#full_key` then bails because the key
is not a `String`. So in the gem, `t("foo.#{bar}.title")` produces **no key at
all** — and for `unused` that can delete a live translation.

**Decision.** Improve on the gem. Prism exposes `InterpolatedStringNode.parts`,
so build a key *pattern* from the static parts: `t("foo.#{bar}.title")` →
`foo.*:.title`, fed to the B2 segment matcher, and mark matching keys used.
Report fully-opaque calls such as `t(some_var)` in a separate list so a human
can add a `# i18n-tasks-use` comment or an `ignore_unused` rule. **Never treat
an opaque call as "no keys used".**

The gem's own non-Prism path splices raw source text into the key
(`ast_matchers/base_matcher.rb:46-53`) and rewrites `#{...}` back to `*:` with a
`StringScanner` (`used_keys.rb:135-176`). This port does not copy that.

### B6. Relative key resolution has two divergent implementations in the gem

- Parser path, `scanners/relative_keys.rb:12-66`: honours
  `search.relative_roots` and `relative_exclude_method_name_paths`, picks the
  **longest** matching root, drops a `_controller` suffix, appends the calling
  method name, strips partial underscores with `gsub("._", ".")`, and strips all
  extensions.
- Prism path, `prism_scanners/nodes.rb:36-72` and `:354-381`: **ignores both
  config keys** and hardcodes `app/views/` and `app/components/`.

**Decision.** Implement the Prism path's semantics, since that is the path
modern projects run, but make the roots configurable. A project that lists
`app/forms` or `app/presenters` in `relative_roots` gets them honoured — a bug
fix, not a refinement.

### B7. `rails-i18n` plural rules are `eval`-ed Ruby

`missing_keys.rb:90-93` reads `<rails-i18n>/rails/pluralization/<locale>.rb` and
`eval`s it, because those files contain lambdas. It powers
`missing --types plural` only.

**Decision.** Embed a static CLDR plural-category table (`src/plural.rs`).

### B8. Destructive write behaviour

`FileSystemBase#set` deletes any locale file that ends up with no keys, via
`FileUtils.remove_file`. The conservative router depends on each leaf
remembering its origin file in `node.data[:path]`.

**Decision.** Track origin paths per key on read. Writing is opt-in: `--write`
to apply, `--dry-run` for the diff, and `--allow-delete` on top of `--write`
before any file is removed. The list of files that would be deleted is always
printed, flag or not.

### B9. `ruby-prism` crate lifetimes

The generated node API ties node lifetimes to the `ParseResult`, so nodes cannot
outlive the parse. Extract owned data during the visit — which is what the gem's
visitor does with its own IR anyway.

### B10. Sort order is portable for free

The gem sorts with Ruby `String#<=>`, comparing UTF-8 codepoints, and
`Nodes#to_hash` does `sort_by(&:key)` per level, recursively. Rust `str` `Ord`
is also byte-wise over UTF-8. Sort per level, recursively, and the output
matches. No special handling.

---

## 3. What is dropped

Not implemented, on purpose.

| Dropped | Reason |
|---|---|
| All translation backends, `translate-missing`, `add-missing` | Out of scope. `add-missing` also needs `Inflector.humanize` templating. |
| Custom scanners, `ast_matchers`, routers and adapters named in the config | Each is a Ruby class name resolved with `ActiveSupport::Inflector.constantize`. There is no Ruby runtime. |
| The ERB config file | See B3. |
| The whitequark `parser` backend, the `ast` gem, `PatternMapper` | Prism only. Two parsers is the gem's main source of behaviour drift. |
| `strict: false` as implemented | Replaced by proper Prism dynamic patterns. See B5. |
| `isolating_router` | ViewComponent sidecar layout; conservative and pattern routers cover the rest. |
| Haml | No scanner. Slim, ERB, JS and TS are covered. |
| The reference-key subsystem | See B4. Errors on a reference value instead. |
| Rails inference — `human_attribute_name`, `model_name.human`, `default_i18n_subject`, `before_action` re-parenting | See accepted diffs 4 and 4a. Cover the resulting `activerecord.*` keys with `ignore_unused`. |
| A persistent scan cache | A cold run over a few thousand files is well under a second, so there is nothing to buy. Cache bugs are invalidation bugs, and they present as "this key is unused" — the failure mode that deletes a live translation. |
| `mv`, `cp`, `rm`, `data`, `data-merge`, `data-remove`, `prune`, `eq-base`, `irb`, `gem-path`, `config`, `check-prism` | Outside this tool's command surface. |
| `internal_locale` — the CLI's own en/ru catalog | Reports are in English. This removes the `i18n` gem's `reserved_keys_pattern` too, which is hardcoded instead. |
| YAML comment preservation | The gem does not preserve them either. |
| YAML anchors, aliases and merge keys | The gem reads them with `aliases: true`, expands them, and never re-creates them, so its `normalize` silently inlines them. This port **errors** on read instead. |
| Dates, times, `!ruby/*` YAML types | Psych raises `DisallowedClass` today. Rejected with a clear error. |
| The exact terminal-table layout and Rainbow colour codes | Reimplemented plainly, plus a `--format json` the gem lacks. |
