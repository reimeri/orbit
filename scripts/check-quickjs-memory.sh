#!/usr/bin/env bash
# Cargo runner for lifetime regressions; without arguments, check raw bytecode reads.
set -euo pipefail
if (( $# == 0 )); then
  root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
  source="$root/.build-deps/rquickjs-sys/quickjs"
  work="$(mktemp -d /tmp/orbit-quickjs-bytecode-XXXXXX)"
  echo "QuickJS bytecode regression artifacts: $work"
  "${CC:-cc}" -std=c11 -D_GNU_SOURCE -g -O1 -I"$source" \
    "$root/patches/quickjs/bytecode-lifetime.c" \
    "$source/quickjs.c" "$source/cutils.c" "$source/libregexp.c" \
    "$source/libunicode.c" "$source/dtoa.c" -lm -lpthread -ldl \
    -o "$work/bytecode-lifetime"
  set -- "$work/bytecode-lifetime"
fi
exec valgrind --quiet --error-exitcode=1 --leak-check=full \
  --show-leak-kinds=definite,indirect --errors-for-leak-kinds=definite,indirect "$@"
