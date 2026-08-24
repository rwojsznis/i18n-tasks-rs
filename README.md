# i18n-tasks-rs

A Rust port of [`i18n-tasks`](https://github.com/glebm/i18n-tasks). The Ruby gem
is the reference implementation for every behaviour here; every deliberate
difference is written up in [`docs/accepted-diffs.md`](docs/accepted-diffs.md),
and anything not listed there is a bug.

It reads every file type the gem does bar Haml: `.rb` through Prism, `.erb`
through one synthetic Ruby buffer per file, and everything else — Slim, JS, JSX,
TS, TSX, Vue — through a regex scanner. It covers the read-only reports, the
write path (`normalize`, `check-normalized`) and a `rayon` fan-out over the file
list. Rails inference is out of scope; see
[`docs/design-notes.md`](docs/design-notes.md).

## Install

```bash
cargo install --path .
# or
cargo build --release      # target/release/i18n-tasks-rs
```

## Commands

```
i18n-tasks-rs missing   [-l LOCALES] [locale ...] [--types used,diff,plural]
i18n-tasks-rs unused    [-l LOCALES] [locale ...]
i18n-tasks-rs eq-base   [-l LOCALES] [locale ...]
i18n-tasks-rs check-consistent-interpolations [-l LOCALES] [locale ...]
i18n-tasks-rs check-reserved-interpolations   [-l LOCALES] [locale ...]
i18n-tasks-rs check-normalized [-l LOCALES] [locale ...]
i18n-tasks-rs normalize [-l LOCALES] [locale ...] [-p/--pattern-router]
                                     [--write | --dry-run] [--allow-delete]
i18n-tasks-rs health    [-l LOCALES] [locale ...]
i18n-tasks-rs find      [-l LOCALES] [locale ...]
i18n-tasks-rs init-config    [-o/--to PATH] [--write] [--force]
i18n-tasks-rs migrate-config [-i/--from PATH] [-o/--to PATH]
                             [--write] [--force]
```

Every command that reads a project — that is, all of them except the two
config commands — takes `-c/--config PATH` (default
`config/i18n-tasks-rs.yml`), `--root PATH` (default the working directory),
`-f/--format text|json` and `-j/--jobs N`. `init-config` and `migrate-config`
take `--root` alone, because neither scans the source.

Exit codes match the gem: **0** the check passed, **1** the check found
something, **2** the tool itself failed.

### `-l/--locales`

Restricts a check to some of the configured locales. Comma-separated,
repeatable, and concatenated with any trailing positional locales, so these all
ask for the same two:

```
i18n-tasks-rs missing -l en,ru
i18n-tasks-rs missing -l en -l ru
i18n-tasks-rs missing -l en ru
i18n-tasks-rs missing en,ru
```

Two values are special: `base` stands for `base_locale`, and `all` — the
default when the flag is absent — means every configured locale. When the base
locale appears anywhere but first it is swapped to the front, so a report always
opens on the base locale. A locale that is not configured, or is not a
well-formed locale name, exits **2** rather than reporting on nothing.

ref: `lib/i18n/tasks/command/option_parsers/locale.rb#ListParser`.

### `--jobs`

Source files are scanned in parallel, one task per file, over as many threads as
the machine has cores. `--jobs N` sizes the pool, and `--jobs 1` scans on one
thread, which is what you want while debugging a scanner.

The output is byte-identical at every setting: `tests/jobs.rs` asserts it for
every command in both formats. A parallel run that reorders anything is a bug.
Only the source scan is parallel; reading and emitting locale data is not.

### Writing is opt-in

`normalize` is the one command that writes, and it does nothing on disk unless
you ask (blocker B8):

- no flag — print the summary of what would change, write nothing;
- `--dry-run` — print a unified diff per file, write nothing;
- `--write` — apply it;
- `--allow-delete` — needed on top of `--write` before a file that ends up with
  no keys is removed. The list of such files is always printed, flag or not.

`check-normalized` and `health` never write, whatever the flags.

## Config

`config/i18n-tasks-rs.yml`, and plain YAML. It never executes code — no ERB, no
Ruby, no scanner class names. See blocker B3 in
[`docs/design-notes.md`](docs/design-notes.md), and
`tests/fixtures/sample_app/i18n-tasks-rs.yml` for a realistic one.

### Starting from nothing

`init-config` generates one from the project. The gem's answer here is to copy
a template — `cp $(bundle exec i18n-tasks gem-path)/templates/config/...` —
which is the same file for every project and so is right about nothing but the
defaults. This one is read off the project:

```bash
i18n-tasks-rs init-config              # print the result, write nothing
i18n-tasks-rs init-config --write      # create config/i18n-tasks-rs.yml
```

- **`data.read`** comes from the locale files that are there, one pattern per
  layout found: `config/locales/en.yml` gives `%{locale}.yml`,
  `devise.en.yml` gives `*.%{locale}.yml`, a directory per locale gives both
  `%{locale}/*.yml` and `%{locale}/**/*.yml` — two, because the loader's `**`
  sits between two slashes and never matches nothing. Every file found must be
  matched by a pattern that was emitted, judged with the loader's own rule and
  not a second implementation of it. A file that names no locale is listed in
  the header and the command exits **1**.
- **`data.write`** is the first candidate target those patterns read back, so
  `normalize --write` cannot move keys somewhere nothing looks for them. In a
  namespaced layout that is `config/locales/common.%{locale}.yml`, not
  `config/locales/%{locale}.yml`, which nothing there reads.
- **`base_locale`** comes from `config.i18n.default_locale` in the project's
  Ruby, read as text — blocker B3 holds here too, so a computed value is not
  detected and the fallback says so.
- **`search.paths`** and **`search.exclude`** are the candidates that exist:
  `app/`, `lib/`, minus build directories such as `app/assets/builds` and
  `lib/assets`. The gem searches `app/` alone; `lib/` is added because a key
  used only from a rake task there would otherwise be reported unused, and
  acting on that deletes a live translation.
- **`search.relative_roots`** keeps the gem defaults the project has, and adds
  a directory only when a file under it uses a relative key — which is the only
  condition under which a relative root does anything. `app/components` earns
  its place; `app/helpers` does not if the project has no such directory.
- `locales`, the routers and every `ignore` list are written out commented, so
  the file still documents the supported surface.

The generated config is then read back with the normal parser and the data
loaded with it, before anything is offered for writing. The header and the
terminal report both say what it read: `645 key(s) in 4 locale(s) from 9
file(s)`. `--write` never replaces an existing file without `--force`.

`init-config` says so, and exits **1**, when the project still has a gem config:
`migrate-config` is the better command there, because it keeps the ignore lists.

### Starting from a gem config

A gem config cannot be renamed into place. `migrate-config` converts one:

```bash
i18n-tasks-rs migrate-config              # print the result, write nothing
i18n-tasks-rs migrate-config --write      # create config/i18n-tasks-rs.yml
```

Without `--from` it reads `config/i18n-tasks.yml`, then
`config/i18n-tasks.yml.erb`, which is the gem's own order of preference. The
migration:

- strips the ERB. A tag that stood alone, such as an `<% require %>` prelude,
  simply goes; a tag that *computed a value* cannot be migrated at all, so the
  line is dropped, named in the output header, and the command exits **1**;
- drops every setting this port has no answer for — `translation`,
  `search.{scanners,prism,strict,ast_matchers}`, `data.{adapter,yaml,json}`,
  `internal_locale`, an unknown `data.router` — and records the reason for each
  in the header of the file it writes;
- keeps the comments. It slices the original lines rather than re-serializing a
  parsed tree, so the note above an `ignore_unused` entry, which is often the
  only record of *why* the key is ignored, comes along;
- reads its own output back with the normal config parser before writing
  anything, so the command either produces a config this tool accepts or fails.

`--write` never replaces an existing file without `--force`.

Supported keys, and nothing else:

```yaml
base_locale: de
locales: [de, en, fr]
data:
  read: [...]            # %{locale} in every path
  write: [...]           # a path, or a [key_pattern, path] pair
  external: [...]
  router: conservative_router | pattern_router
  keep_order: false
search:
  paths: [app/]
  exclude: [...]
  only: [...]
  relative_roots: [...]
  relative_exclude_method_name_paths: [...]
ignore: [...]
ignore_missing: [...]     # a list, or a per-locale mapping
ignore_unused: [...]
ignore_eq_base: [...]
ignore_inconsistent_interpolations: [...]
```

An unknown key is an error, with the supported list in the message.

## Layout

| Path | What |
|---|---|
| `src/config.rs` | the plain-YAML config, plus a stable config digest |
| `src/init/` | `init-config`: a config read off the project's layout |
| `src/migrate/` | `migrate-config`: a gem config to the above |
| `src/pattern.rs` | the key pattern DSL, as a segment matcher (B2) |
| `src/keys.rs` | key splitting and `ActiveSupport#underscore` |
| `src/discover.rs` | the file walk and the aho-corasick prefilter |
| `src/lineindex.rs` | one line-offset index per file |
| `src/scan/ruby.rs` | the Prism visitor — the core |
| `src/scan/erb.rs` | ERB tags to one Ruby buffer per file, plus a source map |
| `src/scan/template.rs` | Slim, JS, TS and every other extension, by regex |
| `src/data/load.rs` | YAML reading, one flat key map per locale |
| `src/data/emit.rs` | the hand-written YAML emitter (B1) |
| `src/data/route.rs` | the conservative and pattern routers |
| `src/plural.rs` | plural nodes and the static CLDR table (B7) |
| `src/report/` | `unused`, `missing`, `eq-base`, the interpolation checks, `normalize` |
| `src/used.rs` | the used-key set, scanned once for every locale, in parallel |
| `src/yaml.rs` | a YAML reader over the `saphyr-parser` event stream |
| `tests/fixtures/` | the gem's own scanner fixtures, plus two config fixtures |
| `tests/golden/` | the emitter's golden input and output |
| `docs/design-notes.md` | why the port looks the way it does; blockers B1–B10 |
| `docs/accepted-diffs.md` | every deliberate difference from the gem |

## Tests

```bash
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

`tests/jobs.rs` holds every command, in both formats, to the same bytes at
`--jobs 1`, `2`, `8`, `16` and the default.

`tests/relative_keys.rs`, `tests/used_keys.rs`, `tests/erb_keys.rs`,
`tests/template_keys.rs` and `src/pattern.rs`'s test module are ports of
`spec/relative_keys_spec.rb`, `spec/prism_scanner_spec.rb`,
`spec/used_keys_erb_prism_spec.rb`, `spec/used_keys_slim_spec.rb`,
`spec/pattern_scanner_spec.rb`, `spec/pattern_with_scope_scanner_spec.rb` and
`spec/key_pattern_matching_spec.rb`. They read the gem's own fixtures, vendored
under `tests/fixtures/`.

`i18n-tasks-rs find -f json` exists so that this tool and the gem can be diffed
over the same project: run it against a checkout of the gem and compare the
occurrence sets.

## The emitter

Psych *defines* "normalized" for the gem, because `FileFormats#normalized?` is
exact string equality against Psych output. Psych cannot be reproduced byte for
byte from Rust, so the emitter targets a different and stronger pair of
properties, both asserted in `tests/normalize.rs`:

- **value preservation** — parse, emit, parse again, and every key maps to the
  same value;
- **idempotence** — emitting twice produces the same bytes.

Two Psych behaviours are dropped on purpose: lines are never folded, and
non-BMP characters are written literally rather than as `\Uxxxxxxxx`. The
quoting rules are Q1 to Q7 in `style_of`, one test each. Q2 copies Psych's
`/^[^[:word:]][^"]*$/` past what the YAML grammar needs, which keeps the output
identical to the gem's for the ordinary case.

## License

MIT, the same as the gem. See [`LICENSE.txt`](LICENSE.txt).
