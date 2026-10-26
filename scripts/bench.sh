#!/usr/bin/env bash
# Benchmarks the gem against this checkout's release build on a Rails project
# and rewrites the README table between the bench markers.
#
# Usage: scripts/bench.sh <rails-project> [hyperfine options...]
#
#   GEM_CMD  how to run the gem in the project (default: bundle exec i18n-tasks)
#   RS_BIN   the binary to measure (default: a fresh release build of this checkout)
#
# Hyperfine options replace the default `--warmup 1`, e.g. `--runs 3` for a quick pass.
# Needs hyperfine 2 (for `--ignore-failure=<codes>`) and jq.
set -euo pipefail
export LC_ALL=en_US.UTF-8 # ${#s} must count "×" as one column, not two bytes

repo=$(cd "$(dirname "$0")/.." && pwd)
readme=$repo/README.md
project=$(cd "${1:?usage: scripts/bench.sh <rails-project> [hyperfine options...]}" && pwd)
shift

opts=("$@")
[[ $# -eq 0 ]] && opts=(--warmup 1)

gem_cmd=${GEM_CMD:-bundle exec i18n-tasks}
if [[ -z ${RS_BIN:-} ]]; then
  (cd "$repo" && cargo build --release --quiet)
  RS_BIN=$repo/target/release/i18n-tasks-rs
fi

if ! grep -q '^<!-- bench:start -->$' "$readme" || ! grep -q '^<!-- bench:end -->$' "$readme"; then
  echo "README.md has no bench:start/bench:end markers" >&2
  exit 2
fi
if ! grep -q '^(powered by .*; idle .*)$' "$readme"; then
  echo "README.md has no \"(powered by ...; idle ...)\" footnote" >&2
  exit 2
fi

commands=(unused missing check-consistent-interpolations check-reserved-interpolations check-normalized health find)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# The checks exit 1 when they find something; that is a result. Exit 2 is the
# tool failing, and timing it would put a bogus number in the table.
for cmd in "${commands[@]}"; do
  gem_args=$cmd
  # The gem's terminal `find` crashes under `prism: rails` on the first `t()` that
  # is not at the start of its line (Occurrence#line is the call, line_pos the
  # column in the file's line). JSON still scans everything but prints only the
  # key tree, less than the port prints, so the speed-up errs low.
  [[ $cmd == find ]] && gem_args="find -f json"
  (cd "$project" && hyperfine "${opts[@]}" --ignore-failure=1 \
    --export-json "$tmp/$cmd.json" \
    -n i18n-tasks "$gem_cmd $gem_args" \
    -n i18n-tasks-rs "$RS_BIN $cmd")
done

# Sub-second times read better in ms; the README never shows "0.19 s".
fmt() {
  awk -v t="$1" 'BEGIN { if (t < 1) printf "%d ms", t * 1000 + 0.5; else printf "%.2f s", t }'
}

mean() {
  jq ".results[$1] | .summary.time_wall_clock.mean" "$2"
}

rows=()
for cmd in "${commands[@]}"; do
  gem=$(mean 0 "$tmp/$cmd.json")
  rs=$(mean 1 "$tmp/$cmd.json")
  label="\`$cmd\`"
  [[ $cmd == health ]] && label="$label (all five checks)"
  speedup=$(awk -v g="$gem" -v r="$rs" 'BEGIN { printf "%d×", g / r + 0.5 }')
  rows+=("$label|$(fmt "$gem")|**$(fmt "$rs")**|$speedup")
done

# Padded as prettier would, so a later format pass leaves the table alone.
header="Command|i18n-tasks|i18n-tasks-rs|Speed-up"
widths=(0 0 0 0)
for row in "$header" "${rows[@]}"; do
  IFS='|' read -r -a cells <<<"$row"
  for i in 0 1 2 3; do
    ((${#cells[i]} > widths[i])) && widths[i]=${#cells[i]}
  done
done

line() {
  local out="|" cell i
  IFS='|' read -r -a cells <<<"$1"
  for i in 0 1 2 3; do
    cell=${cells[i]}
    out+=" $cell$(printf '%*s' $((widths[i] - ${#cell})) '') |"
  done
  echo "$out"
}

if [[ $(uname) == Darwin ]]; then
  hw=$(system_profiler SPHardwareDataType)
  model=$(sed -n 's/^ *Model Name: //p' <<<"$hw")
  chip=$(sed -n 's/^ *Chip: Apple //p' <<<"$hw")
  machine="$model $chip"
else
  machine=$(uname -sm)
fi
ruby_version=$(cd "$project" && ruby -e 'print RUBY_VERSION')
rust_version=$(cd "$repo" && rustc -V | awk '{ print $2 }')

{
  line "$header"
  sep=""
  for i in 0 1 2 3; do sep+="$(printf '%*s' "${widths[i]}" '' | tr ' ' '-')|"; done
  line "${sep%|}"
  for row in "${rows[@]}"; do line "$row"; done
  echo
  # The project description before "; idle" is hand-written; only the machine
  # and versions are measured.
  grep '^(powered by .*; idle .*)$' "$readme" |
    sed "s/; idle .*)\$/; idle $machine, Ruby $ruby_version, Rust $rust_version)/"
} >"$tmp/block"

awk -v block="$tmp/block" '
  /^<!-- bench:end -->$/ { while ((getline l < block) > 0) print l; skip = 0 }
  !skip { print }
  /^<!-- bench:start -->$/ { skip = 1 }
' "$readme" >"$tmp/README.md"
mv "$tmp/README.md" "$readme"
echo "Updated $readme"
