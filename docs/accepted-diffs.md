# Accepted differences from the gem

Every place `i18n-tasks-rs` behaves differently from the Ruby gem, with the
reason. The gem is the reference implementation, so anything not listed here is
a bug.

Each difference below was found and verified by running both tools over the same
inputs — the gem's own `spec/fixtures`, generated fixtures at three scales, and a
production Rails app of roughly 3,700 source files and 82,000 keys, referred to
below as "the reference project".

---

## 1. Dynamic keys become key patterns (blocker B5) — improvement

**Gem.** `arguments_visitor.rb:50-60` returns `nil` for every
`InterpolatedStringNode` and `InterpolatedSymbolNode`, so
`t("foo.#{bar}.title")` in a Ruby file produces **no key at all**. Nothing in
the Prism path consults `search.strict`, so this happens in strict and
non-strict mode alike.

The gem does have a dynamic-key mechanism — `used_in_expr?`, built from the
non-strict scan in `used_keys.rb#expr_key_re` — and `unused_tree` does consult
it, because its `strict` parameter is `nil` unless the CLI passes `--strict`.
But it only ever sees raw keys that still contain a `#{`, and only the regex
scanners produce those. On the real project the non-strict source tree holds 26
such raw keys, **every one of them from a `.slim` or `.erb` file, none from a
`.rb` file**.

So with `search.prism` set, as the real project sets it, a dynamically built key
in a Ruby file is invisible to the gem, and `unused` will report it. For a key
that is genuinely live, that means deleting a translation.

Verified directly against the gem:

```
categories.details.roofing.footer_text_html   used_in_expr? = false   (.rb)
occupations.main_job_titles.painter           used_in_expr? = false   (.rb)
about.team.benny.bio_html                     used_in_expr? = true    (.slim)
```

**Port.** The static parts of the interpolation build a key pattern:
`t("foo.#{bar}.title")` becomes `foo.*:.title`, fed to the segment matcher. A
key the pattern matches is used.

The same treatment applies to `.erb`, `.slim`, `.js` and `.ts`. There it
**matches** the gem rather than exceeding it, because the gem reaches those keys
too, through `used_in_expr?` — but only through a second, non-strict scan of
every file (`used_keys.rb:143`), which the port does not need. One consequence:
the gem's trailing-dot rule (`pattern_scanner.rb:50`) rewrites `t("foo." + x)`
to the key `foo.:` **before** `expr_key_re` selects keys ending in a dot, so the
gem never derives a pattern from one and the keys below `foo.` stay unprotected.
The port keeps the gem's literal key and adds the pattern `foo.*:`.

**Effect.** The port reports **fewer** unused keys, never more. On the gem's own
fixtures it correctly protects `hash.pattern.x`, which the gem reports as
unused — the fixture even comments that it should not be. On
the reference project this makes **22 of 49** `ignore_unused` entries redundant
with Ruby files alone. That project's own config says so out loud: "prism right
now doesn't detect a lot of dynamic interpolations in the app".

A pattern with no static content at all is rejected, reproducing the gem's
`ignore_pattern_re = /\A[.*:]*\z/` guard, so a bare `t("#{x}")` cannot mark
every key used.

## 2. Opaque calls are reported instead of dropped (blocker B5) — improvement

**Gem.** `t(some_var)`, `t(CONST)` and `t(method_call)` silently produce nothing.

**Port.** Each is recorded and printed in its own section of the `unused`
report, with a note that keys they reach cannot be verified. The rule, from
blocker B5: never treat an opaque call as "no keys used".

## 3. `relative_roots` is honoured (blocker B6) — bug fix

**Gem.** The Prism path (`prism_scanners/nodes.rb:36-72`, `:354-381`) ignores
`search.relative_roots` and `search.relative_exclude_method_name_paths`
entirely, and hardcodes `app/views/` and `app/components/`. The parser path
honours both. The real project configures
`relative_roots: [app/controllers, app/forms, app/helpers, app/mailers,
app/presenters, app/views]`, so relative keys in `app/forms` and
`app/presenters` are mis-resolved by the gem today.

**Port.** A class in a file under any configured relative root supports
relative keys, and `relative_exclude_method_name_paths` drops the method-name
segment. A `.rb` file is never treated as a template, which keeps
`app/controllers/*.rb` out of the template-path rule.

**The other side of the same coin:** a ViewComponent template with a relative
key needs `app/components` in `search.relative_roots`, because nothing is
hardcoded any more. The real project has no relative key in any file under
`app/components`, so its config does not need the entry; the gem's own
`spec/fixtures/used_keys` does, and `tests/erb_keys.rs` configures it.

## 4. Rails inference is dropped — deliberate scope cut

`human_attribute_name`, `model_name.human`, `default_i18n_subject`, bare
`mail(...)` and `before_action` re-parenting are not implemented. The measured
cost on the reference project: 1 `human_attribute_name` site, 5
`model_name.human` sites, 0 controllers needing `before_action` re-parenting.

**Effect.** The port finds one fewer used key there:
`activerecord.models.publishable_presenter.one`, from
`app/presenters/publishable_presenter.rb:9`. That project's config already covers this
class of key with `ignore_unused: ["{activerecord, activemodel, ...}.*"]` and
`ignore_missing: ["activerecord.models.publishable_presenter"]`, so no false
report follows.

## 4a. Method re-parenting is dropped — the one place `unused` can grow

This belongs with the rest of the dropped Rails inference (`nodes.rb:273-324`
and `:392-426`), but it is not Rails-specific and deserves its own entry,
because it is the **only**
accepted difference that can make the port report a key the gem does not.

**Gem.** When a method calls another method of the same class, the callee's
relative keys are re-parented onto the caller as well, guarded against cycles by
a `nested_calls` map. `spec/prism_scanner_spec.rb` has two examples:

```ruby
class EventsController
  def create
    t('.relative_key'); method_b
  end
  def method_b
    t('.error')
  end
end
```

The gem reports `events.create.error` as well as `events.method_b.error`. The
same mechanism carries a `before_action -> { t('.x') }` lambda's keys onto every
action the filter applies to.

**Port.** Each method keeps only the keys written inside it, so
`events.create.error` is never derived. A `before_action` lambda body sits in the
class scope, which resolves no relative key at all, so its keys are dropped
rather than re-parented.

**Effect.** The port finds **fewer** used keys here, which is the direction that
can turn into a false `unused` entry — the opposite of items 1 and 14. It does
not bite on any measured input: every differential run in the table above
reports `unused rust-new = 0`, the real project included, because a relative key
resolved against the wrong action is a key nobody wrote in the locale files
either. Watch this if a controller ever grows a private helper holding a
relative `t` that the locale files spell against the calling action.
`tests/used_keys.rs` pins both cases.

## 4b. JSON locale files are not supported

YAML only, with the format-dispatch seam left in place. The gem's
`Data::Adapter::JsonAdapter` reads and
writes `.json` locale files, and `spec/file_system_data_spec.rb` covers both
directions.

**Port.** Every path in `data.read`, `data.write` and `data.external` is parsed
as YAML whatever its extension. `migrate-config` drops a `data.json` section
with the reason "JSON locale files are not supported", so a migrated config says
so out loud, but nothing rejects a `.json` path in `data.read`.

Two consequences, both pinned in `tests/data_and_reports.rs`:

- **Reading works, by accident.** JSON is YAML flow style, so
  `{"en": {"a": "A"}}` loads and every read-only check is correct on it.
- **The write path would convert the file.** `normalize --write` emits YAML into
  the `.json` name, turning `{"en": {"a": "A"}}` into `---\nen:\n  a: A\n`.
  `check-normalized` reports the file as not normalized, for the same reason.

So a project with JSON locale data can run the checks and must not run
`normalize --write`. If that ever matters, the fix is to reject a `data.read`
entry whose extension is not `.yml`/`.yaml` rather than to build a JSON adapter.

## 5. A used key that is a prefix of another no longer destroys it — bug fix

**Gem.** `Siblings.from_key_occurrences` assigns each key into a forest with
`forest[key] = Node.new(...)`. When one used key is a strict prefix of another,
the assignment replaces the deeper subtree. `used_in_source_tree` is
unfiltered, so `unused` and `missing` both see the truncated set.

Concretely, `I18n.t("job_wizard_project")` at
`app/services/frontend/project_wizard_props.rb:5` destroys four occurrences of
`job_wizard_project.errors.rate_limit` and
`job_wizard_project.description.prefill_category_label`. `i18n-tasks find
'job_wizard_project.*'` shows them, because the key filter removes the
prefix key before the forest is built — which is why the bug is easy to miss.

**Port.** A flat key map, so a prefix key and a deeper key coexist.

## 6. Magic comments report the comment's own line — cosmetic

**Gem.** Prism attaches the comment to a nearby node and the occurrence takes
that node's line. Comments the parser cannot attach — for example one directly
before an `end` — fall back to `ruby_scanner.rb:193-211`, which loses the scope
entirely, so a relative key in such a comment never resolves.

**Port.** A magic comment is attached to the innermost scope whose byte range
contains it, and the occurrence points at the comment line.

In an ERB comment tag the gem instead reports `start + code.index(key)`
(`erb_ast_scanner.rb:130`), a text search for the key inside the tag. The port
does not copy that, because the source map already knows where the comment is.
The two agree wherever the search finds the key, which was every ERB magic
comment in the reference project — including six consecutive
`<%# i18n-tasks-use %>` tags in one form partial.

**Effect.** Same keys, different line number. On the real project this accounts
for 4 of the 9 gem-only and 4 of the 12 rust-only occurrences, all of them
magic comments in one `.rb` file. The comment-before-`end` case now resolves
correctly, which the gem's fallback cannot do.

## 7. Several `t` calls in one magic comment — improvement

The gem's Prism path parses the whole comment payload as one Ruby program, so
`# i18n-tasks-use t('a') t('b')` is a syntax error and the comment is skipped.
Only the parser path splits on `/\s+(?=t)/`. The port applies that split in the
Prism path too.

## 8. `i18n-tasks-skip-prism` has no effect

The gem falls back to the whitequark parser backend when it sees this comment.
That backend is dropped (plan section 2), so the comment is ignored and Prism
parses the file anyway.

## 9. The first positional argument is the key

**Gem.** `process_arguments` compacts `nil` out of the positional list, so when
the first argument cannot be resolved the *second* argument silently becomes the
key: `t(foo(), "default")` looks for the key `default`.

**Port.** The first positional argument is the key, whatever it reduces to.

## 10. YAML anchors, aliases, merge keys and reference values are errors

The gem reads aliases with `aliases: true` and expands them, so `normalize`
already inlines them silently and permanently. It also relies on Psych turning
`:foo.bar` into a Ruby Symbol to implement reference keys. Section 0d measured
zero of each in the real project, so all four are hard errors naming the file
and line, rather than silent behaviour that cannot be undone.

One clarification the survey did not cover: a Symbol **inside a YAML sequence**
is data, not a reference. Rails writes `date.order: [:day, :month, :year]`, and
the real project has 9 such scalars across `date.{de,en,fr}.yml`. The gem's
`reference?` tests a leaf node's own value, and a sequence's value is an Array,
never a Symbol. So those are preserved, and only a leaf scalar is rejected.

## 11. Per-locale `ignore_*` groups and a `nil` locale

The gem selects per-locale ignore groups with `/\b#{locale}\b/`, so `fr,es:`
matches `fr` by substring, and a `nil` locale — which is what `unused_tree`
passes — matches **every** group, because the pattern degenerates to `/\b\b/`.

The port splits the group name on `,`, trims, and matches exactly. With no
locale only the `all` group applies. The real project uses the array form for
`ignore_unused`, so nothing changes there.

## 12. `data.yaml.write.line_width` is gone

Blocker B1: the emitter never folds lines. The real project already sets
`line_width: -1`, so this matches current behaviour exactly. The config key is
therefore not part of the new format.

## 13. Reports are plain text or JSON

terminal-table and Rainbow are replaced with a plain aligned table. `--format
json` is new; the gem has no JSON output. Every JSON envelope carries the config
digest, so a differential run can prove both tools read the same settings.

## 14. One Prism parse per ERB file — performance, same keys

**Gem.** `erb_ast_scanner.rb:107` parses every code tag on its own, so a view
with 150 tags costs 150 parses. A block that spans two tags cannot parse at all,
which is why `LocalRubyParser` carries the `ignore_blocks` hack that strips a
trailing `do |x|`.

**Port.** Every code tag is concatenated into one synthetic Ruby buffer with a
source map back to file offsets, and Prism parses it once (design decision 3).
`<% if %>...<% end %>` and `<%= link_to(...) do %>...<% end %>` then parse as
what they are, so `ignore_blocks` is not needed and is not ported.

**Effect.** More keys, never fewer: a `t` call inside a two-tag block is found.
Confirmed on the real project — `t(:active)`, `I18n.translate(:closed)` and
`I18n.translate(:successful)` inside a two-tag `if` in
`app/views/employer/my_account/_jobs_posted.html.erb` are invisible to a
per-tag parse.

A comment tag becomes a run of Ruby comment lines, one per source line, so the
magic-comment path sees it. The gem rewrites `i18n-tasks-use ` to
`#i18n-tasks-use ` for the same purpose (`erb_ast_scanner.rb:124`).

One position detail: the gem computes the tag offset as
`match.begin(0) + 2 + character.size`, where `character` is only the **first**
character of the indicator, so `<%==` and `<%#-` tags are off by one. The port
uses the real start of the code group.

## 15. `LITERAL_RE` no longer truncates a key at an escaped quote — bug fix

**Gem.** `ruby_key_literals.rb:5` is `:?".+?"|:?'.+?'|:\w+`. The non-greedy body
stops at the first quote, escaped or not, so `t("say \"hi\"")` yields the key
`say \`. The port fixes this rather than reproducing it.

**Port.** The literal matches a whole quoted string, escapes included. A key
holding a quote is then rejected by `VALID_KEY_CHARS`, which is the right answer:
one bogus key fewer, and the call after it is still found.

## 16. The Slim line-continuation scanner is built in

The reference project registers a custom `I18nTasks::SlimMultilineScanner` whose one
distinctive feature is `\s*\(\s*\\?\s*`: it tolerates a Slim
line-continuation backslash between the call and its argument. The port takes
the **union** of that scanner and the gem's `PatternWithScopeScanner`, so no
custom scanner is needed and `search.scanners` is not part of the config format.

Two details of the union:

- The lookbehind is the same character set in both, so a bare `t` still needs a
  preceding character outside `[\p{L}_'\-.]`, and `I18n.t` always matches.
- The custom scanner reports the position of the **key literal**; every scanner
  in the gem itself reports the position of the **call**. The port reports the
  call. On the real project this is 4 occurrences, each one line apart:
  `services.preferred_services{,.add_categories,.edit_category}.restricted_warning_html`
  and `signup.accept_terms_and_conditions_html`.

## 17. The enclosing method name never comes from re-reading the file

**Gem.** `pattern_scanner.rb:83-91` resolves a relative key in a path matching
`controllers|mailers` by reading the whole file from disk again, per occurrence,
and grepping backwards for `def`. It is one of two O(n²) bugs this port avoids;
see design decision 5.

**Port.** Not implemented. The regex scanner only ever sees files that are
neither `.rb` nor `.erb`, and a template has no `def` in it.

---

## Coverage against the gem

Every file type the port supports — `.rb`, `.erb`, `.slim`, `.js`, `.ts` — was
compared against the gem over the same inputs. `unused` produces no new entries
relative to the gem on any of them.

| Input | used (gem-only / rust-only) | unused rust-new | missing rust-new | interpolations |
|---|---|---|---|---|
| the gem's `spec/fixtures` | 1 / 1 | 0 | 0 | 0 |
| generated `small` | 0 / 0 | 0 | 0 | 0 |
| generated `medium` | 0 / 0 | 0 | 0 | 0 |
| generated `large` | 0 / 0 | 0 | 0 | 0 |
| the reference project | 9 / 12 | **0** | 0 | 0 |

On `spec/fixtures` the two tools agree on all 61 occurrences — 48 in `.slim`, 11
in `.rb`, 2 in `.erb` — bar one magic-comment line number (item 6). On the
`large` fixture both find the same 13,200 occurrences.

### The 21 used-key differences on the reference project, one by one

Not one is a missing or a spurious key. Sixteen of the 21 entries are eight keys
that both tools find in the same file, one or two lines apart. Every difference
is an item above:

| Count | Key | Item |
|---|---|---|
| 4 gem, 4 rust | `services.preferred_services.email_notifications.*` | 6 — magic-comment line, `.rb` |
| 4 gem, 4 rust | `*.restricted_warning_html`, `signup.accept_terms_and_conditions_html` | 16 — call position, not key position |
| 1 gem, 0 rust | `activerecord.models.publishable_presenter.one` | 4 — Rails inference dropped |
| 0 gem, 4 rust | `job_wizard_project.*` | 5 — the gem's prefix key destroys them |

Scanning `.rb` alone left 5,578 `unused` entries; adding the template scanners
cleared all of them. 5,424 were literal keys in a `.slim` or `.erb` file, and
154 came from `#{...}` interpolations in Slim partials, which the 84 derived key
patterns now cover. The gem needs a second, non-strict scan of the whole tree
for that class of key; the port derives it from the one scan it already does.

### A warning about config filenames

The gem prefers `config/i18n-tasks.yml` over `config/i18n-tasks.yml.erb`
(`configuration.rb:18-21`). A project that keeps its gem config as ERB and then
drops a plain-YAML config for this port at `config/i18n-tasks.yml` will find
that a bare `i18n-tasks` run silently reads the port's config, which has no
`search.prism` and no `search.scanners` — the gem falls back to its parser
backend and loses any custom scanner.

This is why the port's config is named `config/i18n-tasks-rs.yml`, which is also
`migrate-config`'s default output path. The two tools never compete for one
filename.

---

## The write path

Blocker B1 is a one-way door. `FileFormats#normalized?` is exact string equality
against Psych output, so Psych *defines* "normalized" for the gem. That cannot
be reproduced from Rust, so the emitter targets **value preservation** plus
**idempotence** instead, both asserted in `tests/normalize.rs`.

Value preservation was proven at full scale before anything was written for
real: a copy of the reference project's whole `config/locales` tree was
normalized and re-read **with Psych**. 81,955 keys before, 81,955 after, zero
differences.

## 18. The emitter never folds a line

Psych folds at `line_width`, and the gem then strips the trailing spaces that
folding leaves behind (`yaml_adapter.rb#strip_trailing_spaces`). This emitter
writes each value on one line, or as a `|` block scalar when it holds newlines.
Folding is the whole `line_width` class of bugs and it makes diffs unstable.

On the real project this changes three values, all of them long HTML strings
that Psych had folded into a quoted scalar with a trailing newline.

## 19. Non-BMP characters are written literally

Psych escapes them as `\Uxxxxxxxx`, and the gem undoes that with `EMOJI_REGEX`
— but the double quotes Psych added stay behind. This emitter writes the
character itself, so `headline: "Error 500 👩‍🚒"` becomes `headline: Error 500
👩‍🚒`. Nine values on the real project.

## 20. Quoting is rule-based, and Q2 copies Psych on purpose

`style_of` in `src/data/emit.rs` documents rules Q1 to Q7, each with its own
test. Six of them are what the YAML grammar requires. Q2 is not: it copies
Psych's `/^[^[:word:]][^"]*$/` verbatim, which quotes more than the grammar
needs.

That is a deliberate trade. Without it the first reformat of the reference
project would have rewritten several hundred files instead of nine, and that
diff has to be readable by hand. Two values it does **not** cover, because Psych does
not either: `'079 123 45 67'`, which Psych quotes for its `/\A0[0-7]*[89]/`
rule, goes plain here. Psych reads the plain form back as a String, so the value
survives.

## 21. A block scalar is refused when it would not round-trip

Psych writes `b: |2` when the first line of a block scalar opens with a space.
This emitter double-quotes such a value instead, together with any value that
has a trailing space on a line, or a tab, or any other control character. The
explicit indentation indicator is easy to break by hand and buys nothing.

Chomping follows `spec/yaml_spec.rb`: `|` for exactly one trailing newline,
`|-` for none, `|+` for two or more. A blank line inside a block is written
empty, never as indentation alone, so no file ever holds a trailing space.

## 22. Two guards before writing, which the gem does not have

`FileSystemBase#set` writes one locale's keys to a path and deletes anything the
router did not claim. Two ways that loses data silently, both now refused with
an error instead:

- the destination file already holds a locale outside this run, so writing it
  would drop that locale;
- two locales in one run route to the same destination, so the second write
  would erase the first.

## 23. Writing is opt-in (blocker B8)

The gem's `normalize` writes immediately. Here `--write` is required, `--dry-run`
prints a unified diff and writes nothing, and a file that ends up with no keys
is only removed with `--allow-delete` on top of `--write`. The deletion list is
always printed, flag or not.

## 24. Two loader bugs the write path made dangerous — bug fixes

Neither mattered while the tool was read-only. Both would have destroyed data on
the first write.

- A mapping nested inside a sequence was loaded as an empty sequence. The real
  project has 72 of them, all real content, under keys such as
  `subscriptions.featured_jobs.list`. `Value::Map` carries them now.
- A key segment holding a dot — `numeric.2.5` — cannot be told apart from a
  nesting level in the dotted form, so the emitter split it into two levels.
  `Leaf::odd_segments` records the real segments for the rare key that needs it.
