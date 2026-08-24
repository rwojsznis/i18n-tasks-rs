# Design notes

Why this port looks the way it does. Source comments refer to the numbered
items here — "design decision 2", "blocker B5" — so keep the
numbering stable.

The Ruby gem, [`i18n-tasks`](https://github.com/glebm/i18n-tasks), is the
reference implementation. Every deliberate behavioural difference is written up
in [`accepted-diffs.md`](accepted-diffs.md); anything not listed there is a bug.

---

## 1. Five design decisions

Scanning is only about a third of the cost of `unused`. A port that swaps only
the parser gets roughly 1.3×, which the gem's own parser-versus-Prism benchmark
shows. The wins are in the data layer.

1. **Flat key map, not a node tree.** A `Vec<Leaf>` of dotted keys with a
   `HashMap<String, usize>` index over it, plus the interior-node and
   plural-node sets ancestor lookups need. Segments are not interned; see
   section 5. The gem's cost comes from
   `select_nodes` deep-copying every
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

---

## 4. Four behaviours the gem states only in code

All four were port bugs. The first two were caught by the last specs to be
ported, and neither fix changed any of the four differential reports in
[`accepted-diffs.md`](accepted-diffs.md). The last two were caught by review and
not by a spec — for the globs, the ported spec had its own globs rewritten to
fit the bug, so it passed.

They are written up here because the code that gets them right looks arbitrary
without them.

### The read pattern decides which files name a locale

`file_system_base.rb:122-124` builds an anchored regex out of the read pattern:
`%{locale}` becomes `([^/.]+)`, which a dotted file name cannot satisfy.
`src/data/load.rs#locale_pattern_re` does the same, `**` and all.

Anchoring on the literal tail of the pattern prefix instead cannot tell
`config/locales/%{locale}.yml` from `config/locales/*%{locale}.yml`: the glob
reaches `other.fr.yml` under both, and the heuristic reads `fr` out of it under
both. A project whose only read pattern is `config/locales/%{locale}.yml` then
infers a locale `fr` the gem never sees, builds an empty tree for it, and
`missing --types diff` reports every base key against it.

ref: `spec/file_system_data_spec.rb#available_locales`, which runs three read
patterns over the same three files and expects three different answers. The
three cases are `the_read_pattern_decides_which_files_name_a_locale`.

### A `search.paths` entry may name a file

`Find.find` yields a path handed to it directly, so `paths: %w[a/a a/b/a.txt]`
finds `a/b/a.txt`. Calling `read_dir` on every entry finds nothing for a file,
silently. `Finder::consider` holds the per-file decision, and both the walk and
a direct path go through it, so a named file meets the same hidden, `only` and
`exclude` rules the gem applies to it.

ref: `spec/scanners/files/file_finder_spec.rb`.

### What the search globs are matched against

`search.only` and `search.exclude` are matched against the root-relative path —
`app/views/x.rb`, `/`-separated, with no `./` — and not against the absolute
one. `Finder::match_path` strips `cfg.root` to get there.

Matching the absolute path looks harmless and is not. A wildcard-free pattern
kept working, because `build_globs` also added `**/{g}` variants for it, so
`exclude: [app/webpack]` matched and hid the problem. A pattern holding a `*`
got no such variant and was matched against `/Users/…/app/legacy/y.rb`, which
nobody writes a glob for: `exclude` silently did nothing, and `only` silently
matched nothing. An `only` that matches nothing is the damaging half — the scan
finds no used keys, so `unused` reports every key in the project, and a human
acting on that report deletes live translations.

The gem matches `File.fnmatch?` against the path `Find.find` yielded, which
carries the `search.paths` entry as written. Root-relative is the same string
for the ordinary `paths: [app/]` case and a stable one for the rest; accepted
difference 26 covers where the two part company.

A path that is not under the root — an absolute `search.paths` entry — keeps its
full form, because a glob for it has to be written that way.

### A symlink is a file, and never a directory to walk into

`Find.find` decides with `File.lstat`, so it yields a symlink and does not
descend through one, whatever it points at. `Dir.glob` behind `data.read` then
reads a symlinked locale file like any other, because its `File.file?` follows
the link.

`crate::walk` states both halves once, for all three walks over it — source
discovery, `**` expansion under `data.read`, and `init-config`'s detection.
An entry is a directory only when `DirEntry::file_type` says so without
following, so a symlink is offered as a file and the walk never enters it. That
also removes the one way a walk here could not terminate.

Each of the three used to answer this for itself, and `init-config` answered it
differently: it skipped every symlink. So a project whose `config/locales`
holds a symlinked `fr.yml` — a vendored or generated catalog, linked in — got a
generated config that reported one locale file too few and no `fr` among the
locales, in the header and in the `locales:` line it writes commented out, while
the loader that same config feeds read the file all along. Detection has to see
what the loader sees, and `a_symlinked_locale_file_is_detected_like_a_real_one`
in `tests/init_config.rs` pins it.

---

## 5. Implementation choices

Not behaviour differences, and not visible in the output.

- **No segment interning.** An early sketch had `KeyPath = SmallVec<[SegmentId;
  8]>`. Keys are plain dotted `String`s instead, because the gem matches its
  patterns against the dotted key anyway, and `unused` on the `large` fixture
  already runs in 120 ms. Ancestor walks use `rsplit_once('.')`, exactly like
  the gem's `sub(/\.[^.]+\z/, "")`.
- **`similar` provides the unified diff.** `normalize --dry-run` needs a real
  diff, and a hand-rolled LCS would need quadratic memory on a 5,000-line locale
  file.
- **Rails context detection is always on.** The gem gates controller, mailer and
  ViewComponent detection behind `search.prism: "rails"`. Rails inference is
  dropped anyway (section 3, and accepted diffs 4 and 4a), and the flag is not
  part of the new config format.
- **The lint settings live in the manifest.** `Cargo.toml` holds `[lints.rust]`
  and `[lints.clippy]`, so a local `cargo clippy` says what CI says; CI passes
  only `-D warnings`. `pedantic` is on, with six lints allowed and a reason
  written beside each. `clippy::unwrap_used`, `clippy::expect_used` and
  `clippy::panic` are the exception: they are declared in `src/lib.rs` and
  `src/main.rs`, because a manifest `[lints]` table also covers `tests/`, where
  a panic *is* the failure report. `clippy.toml` exempts the unit tests inside
  `src/`. Four dependencies were declared and never imported (`anyhow`,
  `fancy-regex`, `ignore`, `smallvec`); `cargo machete` runs in CI so that
  cannot come back.
- **`rayon` changed no behaviour**, which is the point of design decision 4.
  Every differential report is byte-identical to its single-threaded version,
  and `tests/jobs.rs` holds the output identical at every `--jobs` setting.
