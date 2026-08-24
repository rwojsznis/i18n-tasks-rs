#!/usr/bin/env bash
# Section 4a of docs/design-notes.md: caching is out of scope entirely. No cache
# module, no `--cache` flag, no cache dependency. This guards that decision.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

fail=0

if grep -rniE '\bcache\b' src/ --include='*.rs' \
    | grep -viE 'page cache|no cache|a cache|the cache|cacheable|out of scope|section 4a'; then
  echo "found a cache reference in src/ that is not a comment about not having one" >&2
  fail=1
fi

if grep -rn -- '--cache' src/ >/dev/null 2>&1; then
  echo "found a --cache flag" >&2
  fail=1
fi

for crate in cacache lru moka cached quick-cache sled rocksdb bincode rmp-serde; do
  if grep -qE "^${crate} " Cargo.toml; then
    echo "found cache dependency: $crate" >&2
    fail=1
  fi
done

if [ "$fail" -eq 0 ]; then
  echo "no cache anywhere, as section 4a requires"
fi
exit "$fail"
