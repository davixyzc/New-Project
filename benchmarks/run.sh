#!/usr/bin/env bash
# Times the same workloads in fly, Node.js and Python 3 (wall clock, one run each).
# Usage: benchmarks/run.sh [path/to/fly]
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
fly_bin="${1:-$here/../target/release/fly}"

# Prints the elapsed milliseconds of running the given command once.
elapsed_ms() {
  local start end
  start=$(date +%s%N)
  "$@" > /dev/null
  end=$(date +%s%N)
  echo $(( (end - start) / 1000000 ))
}

printf '%-10s %10s %10s %10s\n' benchmark fly_ms node_ms python_ms
for name in fib loop array objects string; do
  fly_ms=$(elapsed_ms "$fly_bin" "$here/$name.fly")
  node_ms=$(elapsed_ms node "$here/$name.js")
  py_ms=$(elapsed_ms python3 "$here/$name.py")
  printf '%-10s %10s %10s %10s\n' "$name" "$fly_ms" "$node_ms" "$py_ms"
done
