#!/usr/bin/env bash
# Builds fly with profile-guided optimization (PGO) into target/pgo/optimized/release/fly.
#
# PGO records how the interpreter runs the benchmarks and fixtures, then rebuilds with that
# knowledge. It needs an llvm-profdata that matches rustc's LLVM, from the rustup component:
#   rustup component add llvm-tools
# Set LLVM_PROFDATA to use another tool.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

profdata="${LLVM_PROFDATA:-}"
if [ -z "$profdata" ]; then
  profdata="$(find "$(rustc --print sysroot)/lib/rustlib" -name llvm-profdata -type f 2>/dev/null | head -n 1)"
fi
if [ -z "$profdata" ] || [ ! -x "$profdata" ]; then
  echo "llvm-profdata not found. Install it with: rustup component add llvm-tools" >&2
  exit 1
fi

work="$root/target/pgo"
rm -rf "$work"
mkdir -p "$work/raw"

echo "1/3 instrumented build"
RUSTFLAGS="-Cprofile-generate=$work/raw" \
  cargo build --release --quiet --target-dir "$work/instrumented"

echo "2/3 training runs (benchmarks and fixtures)"
fly="$work/instrumented/release/fly"
export LLVM_PROFILE_FILE="$work/raw/run_%p.profraw"
for program in benchmarks/*.fly; do
  "$fly" "$program" > /dev/null
done
for fixture in tests/fixtures/*.fly; do
  (cd tests/fixtures && "$fly" "$(basename "$fixture")" > /dev/null 2>&1 || true)
done
unset LLVM_PROFILE_FILE

echo "3/3 optimized build"
"$profdata" merge -o "$work/merged.profdata" "$work/raw"
RUSTFLAGS="-Cprofile-use=$work/merged.profdata" \
  cargo build --release --quiet --target-dir "$work/optimized"

echo "built $work/optimized/release/fly"
