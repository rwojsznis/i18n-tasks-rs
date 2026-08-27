# AGENTS.md

You must use ASD-STE100 Simplified Technical English (STE) when it doesn't detract from meaning.

## What this is

`i18n-tasks-rs` is a Rust port of the Ruby gem
[`i18n-tasks`](https://github.com/glebm/i18n-tasks): it finds missing and unused
translations in a Rails project, checks interpolations, and normalizes locale
YAML. Single crate, one binary (`i18n-tasks-rs`) plus a library (`i18n_tasks_rs`).

**The gem is the reference implementation.** Behaviour that differs from it is a
bug *unless* it is written up in [`docs/accepted-diffs.md`](docs/accepted-diffs.md).
That file, and [`docs/design-notes.md`](docs/design-notes.md), are the two
documents to read before changing anything non-trivial:

- `docs/design-notes.md` — the five design decisions, blockers B1–B10, and what
  is deliberately dropped. Source comments cite these by number
  ("design decision 2", "blocker B5").
- `docs/accepted-diffs.md` — every deliberate difference from the gem, numbered.
  If you introduce a new one, add an entry in the same commit.

The gem is not vendored here. Differential work needs a local checkout of it;
`i18n-tasks-rs find -f json` exists so the two tools' occurrence sets can be
compared over the same project.

## Layout

| Path | What |
|---|---|
| `src/config.rs` | the plain-YAML config (no ERB, no code execution — blocker B3) |
| `src/init/` | `init-config`: detect, render, verify |
| `src/migrate/` | `migrate-config`: ERB, per-key decisions, render, over line ranges |
| `src/pattern.rs` | the key-pattern DSL as a segment matcher (B2) |
| `src/walk.rs` | the one directory walk; the callers pass the prune rule |
| `src/discover.rs` | glob filters over the walk + aho-corasick prefilter |
| `src/scan/ruby/` | the Prism visitor — the core; key, args, magic, nodes |
| `src/scan/erb.rs` | ERB tags → one Ruby buffer per file, plus a source map |
| `src/scan/template.rs` | Haml, Slim, JS, TS and everything else, by regex |
| `src/used.rs` | the used-key set, scanned once for every locale (decision 2) |
| `src/yaml.rs` | the YAML reader; an anchor, alias or Symbol is an error (B4) |
| `src/data/` | YAML load, the hand-written emitter (B1), the two routers |
| `src/report/` | `unused`, `missing`, `eq-base`, interpolations, `normalize`, `find` |
| `src/clean_config.rs` | `clean-config`: ignore rules that suppress nothing |
| `src/session.rs` | the config, the data and the resolved locale list, loaded once |
| `src/check.rs` | the CLI's name per check, and the `-f json` envelopes |
| `src/cli/` | clap only: shared flags, exit codes, printing macros, one module per command |
| `src/main.rs` | the entry point: run the command, print the error, pick the code |
| `tests/` | integration tests; `fixtures/` from the gem, `golden/` for the emitter |
| `docs/` | design notes and accepted diffs |

Unit tests live in `#[cfg(test)] mod tests` next to the code; cross-cutting and
CLI-level tests live in `tests/`.

## Workflow — do it in this order

1. **Write the failing test first.** Before any implementation, add a test that
   fails for the reason you expect. Run it and *read the failure* — a test that
   passes immediately, or fails with the wrong message, is not yet a test of the
   thing you meant.
2. **Then write the implementation**, and only enough of it to make that test
   pass.
3. **Check your work.** Run the full suite, not just the new test. If the change
   touches scanning or reporting, also run the binary against a real project and
   look at the output; `cargo test` passing is necessary, not sufficient.
4. **Run the linters last**, and get them clean before you call the work done.

```bash
cargo test --quiet                           # full suite; show details only on failure
cargo clippy --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --document-private-items
cargo fmt
```

Keep successful test output brief. We care about failures: if the quiet test
run fails, inspect and report the complete failure output.

Do not report a change as finished until every one of them is green. If
something is left failing or unfinished, say so explicitly rather than narrowing
the scope quietly.

## Where tests go

- A scanner behaviour, a config rule, a pattern-matching case → a unit test in
  that module.
- A CLI flag, an exit code, an end-to-end report → `tests/`.
- Anything mirroring a gem spec → port the gem's example and keep a
  `ref: spec/<file>.rb:<line>` comment, as the existing tests do.
- New scanner fixtures go in `tests/fixtures/`. Fixture paths in tests are
  crate-relative (`tests/fixtures/...`), but the *logical* path handed to
  `scan_file` is the project-relative one, because relative-key resolution
  depends on it.

## Conventions

- Comments cite the gem: `ref: lib/i18n/tasks/<file>.rb:<lines>`. Keep that
  habit — it is how a reader checks parity.
- **Writing is opt-in.** `normalize` is the only command that touches disk, and
  only under `--write`; deleting an emptied file additionally needs
  `--allow-delete`. Do not add a command that writes by default.
- **Parallelism must not change output.** `tests/jobs.rs` holds every command,
  in both formats, byte-identical at `--jobs 1`, `2`, `8`, `16` and the default.
  A change that reorders results is a bug even if the set is the same.
- The per-file scan stays a pure `fn(&[u8], &Path) -> FileScan`.
- Exit codes: **0** check passed, **1** check found something, **2** the tool
  itself failed.
- Write in-code comments that describe why code or a class does what it does,
  but not what it does. The "what" should be self-evident. Be concise and direct.

## Gotchas

- The emitter cannot match Psych byte for byte and does not try (blocker B1).
  Correctness is *value preservation* plus *idempotence*, both asserted in
  `tests/normalize.rs`.
- Extension dispatch has no allowlist: `.rb` → Prism, `.erb` → ERB, everything
  else → the regex scanner. So `.jsx`, `.tsx`, `.vue` and friends are scanned.
- `//`-comment skipping only covers `.js`/`.es6`; `.jsx`, `.ts` and `.tsx` are
  absent from the table. This matches the gem's `IGNORE_LINES` — do not "fix" it
  without an accepted-diffs entry.
- Lowercase `i18n.t(` is not matched; `I18n.t(` and bare `t(` are. Gem parity.
