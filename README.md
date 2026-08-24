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
i18n-tasks-rs check-consistent-interpolations [-l LOCALES] [locale ...]
i18n-tasks-rs check-reserved-interpolations   [-l LOCALES] [locale ...]
i18n-tasks-rs check-normalized [-l LOCALES] [locale ...]
i18n-tasks-rs normalize [-l LOCALES] [locale ...] [-p/--pattern-router]
                                     [--write | --dry-run] [--allow-delete]
i18n-tasks-rs health    [-l LOCALES] [locale ...]
i18n-tasks-rs find      [-l LOCALES] [locale ...]
i18n-tasks-rs migrate-config [-i/--from PATH] [-o/--to PATH]
                             [--write] [--force]
```

Global flags: `-c/--config PATH` (default `config/i18n-tasks-rs.yml`),
`--root PATH` (default the working directory), `-f/--format text|json`,
`-j/--jobs N`.

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

A gem config therefore cannot be renamed into place. `migrate-config` converts
one:

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
ignore_eq_base: [...]     # accepted, unused: eq-base is out of scope
ignore_inconsistent_interpolations: [...]
```

An unknown key is an error, with the supported list in the message.

## Layout

| Path | What |
|---|---|
| `src/config.rs` | the plain-YAML config, plus a stable config digest |
| `src/migrate.rs` | `migrate-config`: a gem config to the above |
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
| `src/report/` | `unused`, `missing`, the interpolation checks, `normalize` |
| `src/used.rs` | the used-key set, scanned once for every locale, in parallel |
| `src/yaml.rs` | a YAML reader over the `saphyr-parser` event stream |
| `examples/phase_timing.rs` | where the wall clock goes, stage by stage |
| `tests/fixtures/` | the gem's own scanner fixtures, plus two config fixtures |
| `tests/golden/` | the emitter's golden input and output |
| `docs/design-notes.md` | why the port looks the way it does; blockers B1–B10 |
| `docs/accepted-diffs.md` | every deliberate difference from the gem |

## Tests

```bash
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
./tests/no_cache.sh
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

## Caching

There is none, anywhere, by decision. See section 4a of
[`docs/design-notes.md`](docs/design-notes.md). `./tests/no_cache.sh` runs in CI
to keep it that way.

## License

MIT, the same as the gem. See [`LICENSE.txt`](LICENSE.txt).
