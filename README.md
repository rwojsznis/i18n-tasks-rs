<img width="400" height="266" alt="i18n-tasks-rs-logo" src="https://github.com/user-attachments/assets/ac40f483-8ea0-4d46-b106-8b01de554dd8" />


# i18n-tasks-rs

tldr: Stripped down and simplified Rust port of [i18n-tasks](https://github.com/glebm/i18n-tasks) gem. Up to 70x faster - for all your git hooks and CI needs.

## Why?

I love `i18n-tasks` and it served me well for many years, but having it on git hooks become really painful as project grows because of slow feedback loop. See benchmarks below:

| Command                           | i18n-tasks | i18n-tasks-rs | Speed-up |
| --------------------------------- | ---------- | ------------- | -------- |
| `unused`                          | 5.92 s     | **191 ms**    | 31×      |
| `missing`                         | 7.68 s     | **218 ms**    | 35×      |
| `check-consistent-interpolations` | 3.47 s     | **99 ms**     | 35×      |
| `check-reserved-interpolations`   | 2.37 s     | **90 ms**     | 26×      |
| `check-normalized`                | 21.83 s    | **225 ms**    | 97×      |
| `health` (all five checks)        | 29.12 s    | **414 ms**    | 70×      |
| `find`                            | 3.71 s     | **147 ms**    | 25×      |

(powered by [hyperfine](https://github.com/sharkdp/hyperfine) - ran on real production project with 3 locales; idle Macbook Pro M1 Max, Ruby 4.0.6, Rust 1.98.0)
## Was this LLMed?

Obviously. What isn't those days? I tried to take different approach and contribute to i18n-tasks instead - there was a brief discussion [about caching](https://github.com/glebm/i18n-tasks/issues/753) and some ideas about [multi-process](https://github.com/glebm/i18n-tasks/pull/754) approach. However I quickly realized that patching ruby gem will never yield desired speedups. There is just too much [baggage](https://nesbitt.io/2025/12/26/how-uv-got-so-fast.html). The fact that [prism](https://github.com/ruby/prism) was released with rust bindings - it was just too good not to use.

Library uses some "tricks" - like early files _discovery_ to skip work when it's not needed, parallel execution, cheaper data structures to avoid overhead - but most gain comes from prism and just not loading Ruby at all. It doesn't aim to be 100% compatible with original i18n-tasks behavior or feature set.

## Who is this for?
- you have established Rails project with medium+ amount of translation keys/locales
- you are using `i18n-tasks` already as part of git hooks / CI; ideally with `prism` as scanner
- you care mostly about checks and normalization, not so much about yaml tree operations/translations management

## What was yanked
- all `mv` / `cp` / `rm` / `data-*` / `tree-*` operations - I didn't used those very often and you can still just use original gem if needed
- `translate-missing` / `add-missing` - I found this less and less relevant/useful in the agentic coding era
- all magical Rails i18n key references discovery (if you were using `prism` in `i18n-tasks` you won't notice that much difference; if you're using `whitequark` you probably will have some bad time)
- custom scanners - if you need one - feel free to open a PR

## What works
- `erb`, `haml`, `slim`, `js`, `tsx`, `jsx` (and more via regexp) scanning
-  `missing` / `unused` / `check-consistent-interpolations` / `check-reserved-interpolations` / `check-normalized` (+ `health` which wraps all commands), `eq-base`
- `normalize` - yet it will yield slightly different yaml format than gem! also see `--pattern-router` / `--allow-delete` switches
- `# i18n-tasks-use` comment hints (might cause some quirks, feel feel free to report any issues)

## What was added
- `clean-config` - cleans stale `ignore`, `ignore_missing`, `ignore_unused`, `ignore_eq_base`, and `interpolation-ignore` rules

## How to migrate
1. Grab binary from the [releases page](https://github.com/rwojsznis/i18n-tasks-rs/releases) (nowadays [I recommend using mise](https://mise.jdx.dev/dev-tools/backends/github.html) for local env)
2. Try to migrate your config with `i18n-tasks-rs migrate-config`; when actually migrate it with `--write`
3. Run `i18n-tasks-rs normalize` -- check if it looks good - then do same command with `--write` switch
4. Run `i18n-takss-rs health` - you most likely will need to adapt few things, you can try to just paste the output into your _agent_.
5. Once you're happy with results you can replace `i18n-tasks` calls with `i18n-tasks-rs`
6. Enjoy those previous saved seconds.

## How to start?

1. Grab binary as above
2. `i18n-tasks-rs init config`
3. For all the rest `i18n-tasks-rs help`

## Commands

```bash
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

## Config syntax

```yaml
base_locale: fr
locales: [en, fr]
data:
  read: [...]            # %{locale} in every path
  write: [...]           # a path, or a [key_pattern, path] pair
  external: [...]
  router: conservative_router | pattern_router # used during normalize
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

## License

MIT, the same as the gem. See [`LICENSE.txt`](LICENSE.txt).
