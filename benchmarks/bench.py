#!/usr/bin/env python3
"""Times each benchmark in fly, Node.js and Bun, and prints the median wall-clock time of the whole process.

Usage: benchmarks/bench.py [--runs N] [--fly PATH] [--only a,b] [--timeout S]

Every benchmark is a pair <name>.fly and <name>.js with the same program. The three runtimes run interleaved, one
run each in turn, so the drift of the machine reaches them all alike. The outputs must agree; a mismatch is shown
instead of a time, and a run past the timeout is shown as TIMEOUT.
"""
import argparse
import os
import statistics
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
RUNTIMES = ("fly", "node", "bun")


def timed(cmd, timeout):
    """Returns (milliseconds, stdout), or (None, reason) when the run failed or timed out."""
    start = time.perf_counter()
    try:
        proc = subprocess.run(cmd, cwd=HERE, capture_output=True, text=True, timeout=timeout)
    except subprocess.TimeoutExpired:
        return None, "TIMEOUT"
    elapsed = (time.perf_counter() - start) * 1000
    if proc.returncode != 0:
        return None, f"exit {proc.returncode}: {proc.stderr.strip()[:120]}"
    return elapsed, proc.stdout.strip()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--runs", type=int, default=5)
    parser.add_argument("--fly", default=os.path.join(HERE, "..", "target", "release", "fly"))
    parser.add_argument("--only", default="")
    parser.add_argument("--timeout", type=float, default=60)
    args = parser.parse_args()

    names = sorted(f[:-4] for f in os.listdir(HERE) if f.endswith(".fly"))
    if args.only:
        wanted = set(args.only.split(","))
        names = [n for n in names if n in wanted]
    commands = {
        "fly": lambda n: [args.fly, n + ".fly"],
        "node": lambda n: ["node", n + ".js"],
        "bun": lambda n: ["bun", n + ".js"],
    }

    print(f"{'benchmark':<16}{'fly ms':>10}{'node ms':>10}{'bun ms':>10}  {'best rival':>10}  verdict")
    wins = 0
    for name in names:
        samples = {r: [] for r in RUNTIMES}
        outputs = {}
        for _ in range(args.runs):
            for r in RUNTIMES:
                ms, out = timed(commands[r](name), args.timeout)
                if ms is not None:
                    samples[r].append(ms)
                outputs[r] = out
        medians = {r: (statistics.median(samples[r]) if samples[r] else None) for r in RUNTIMES}
        agree = len(set(outputs.values())) == 1
        rivals = [medians[r] for r in ("node", "bun") if medians[r] is not None]
        fly = medians["fly"]
        if fly is None or not rivals:
            verdict, ratio = "NO DATA", ""
        else:
            best = min(rivals)
            ratio = f"{best / fly:.2f}x"
            verdict = "WIN" if fly < best else "LOSE"
            wins += verdict == "WIN"
        if not agree:
            verdict += " (OUTPUT MISMATCH)"

        def cell(v):
            return f"{v:>10.1f}" if v is not None else f"{'-':>10}"

        print(f"{name:<16}{cell(medians['fly'])}{cell(medians['node'])}{cell(medians['bun'])}  {ratio:>10}  {verdict}")
        if not agree:
            for r in RUNTIMES:
                print(f"    {r}: {outputs[r][:160]!r}")
    print(f"fly is faster than both Node.js and Bun in {wins} of {len(names)} benchmarks")
    return 0


if __name__ == "__main__":
    sys.exit(main())
