#!/usr/bin/env bash
# Median wall-clock times of the benchmarks in fly, Node.js and Python 3.
# Usage: benchmarks/run.sh [--runs N] [--fly PATH]
set -euo pipefail
exec python3 "$(cd "$(dirname "$0")" && pwd)/bench.py" "$@"
