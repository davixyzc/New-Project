#!/usr/bin/env python3
"""Times each benchmark in fly, Node.js and Python 3 and prints the median wall-clock time.

Usage: benchmarks/bench.py [--runs N] [--fly PATH]

Each program prints one result line. Outputs are compared across runtimes, so a
faster but wrong build is reported instead of quietly accepted.
"""
import argparse
import os
import statistics
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
BENCHES = ["fib", "loop", "array", "objects", "string"]


def commands(name, fly):
    return {
        "fly": [fly, os.path.join(HERE, name + ".fly")],
        "node": ["node", os.path.join(HERE, name + ".js")],
        "python": ["python3", os.path.join(HERE, name + ".py")],
    }


def measure(cmd, runs):
    """Returns (median milliseconds, stdout of the last run)."""
    samples, stdout = [], ""
    for _ in range(runs):
        start = time.perf_counter()
        proc = subprocess.run(cmd, capture_output=True, text=True, check=True)
        samples.append((time.perf_counter() - start) * 1000)
        stdout = proc.stdout
    return statistics.median(samples), stdout.strip()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--runs", type=int, default=5)
    parser.add_argument("--fly", default=os.path.join(HERE, "..", "target", "release", "fly"))
    args = parser.parse_args()

    print(f"{'benchmark':<10} {'fly ms':>9} {'node ms':>9} {'python ms':>10}  output")
    status = 0
    for name in BENCHES:
        results, outputs = {}, {}
        for runtime, cmd in commands(name, args.fly).items():
            results[runtime], outputs[runtime] = measure(cmd, args.runs)
        agree = len(set(outputs.values())) == 1
        status |= 0 if agree else 1
        note = outputs["fly"] if agree else f"MISMATCH {outputs}"
        print(f"{name:<10} {results['fly']:>9.0f} {results['node']:>9.0f} {results['python']:>10.0f}  {note}")
    return status


if __name__ == "__main__":
    sys.exit(main())
