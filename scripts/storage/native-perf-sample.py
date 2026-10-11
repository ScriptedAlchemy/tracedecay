#!/usr/bin/env python3
"""Wall-clock stack samples of one bench thread, aggregated by frame (#3491).

`perf` cannot unwind the copied opt binaries past a few frames, so this
samples the main (writer) thread with `eu-stack -1` until the bench exits and
counts inclusive and innermost frames over the samples whose stack contains
`--filter`. Off-CPU time (fsync, page faults) shows up, because a sample is
taken whether the thread runs or waits.

  scripts/storage/native-perf-sample.py --binary /tmp/bin/native/tracedecay-index-bench \\
      --corpus /path/to/corpus --out /tmp/native.stacks
"""

import argparse
import collections
import re
import subprocess
import time


def sample(binary, corpus, out, interval):
    with open(out, "w") as stacks, open(out + ".json", "w") as report:
        bench = subprocess.Popen([binary, "--corpus", corpus], stdout=report, stderr=subprocess.DEVNULL)
        while bench.poll() is None:
            result = subprocess.run(
                ["eu-stack", "-1", "-p", str(bench.pid)], capture_output=True, text=True, check=False
            )
            if result.stdout:
                stacks.write(result.stdout + "----\n")
            time.sleep(interval)
        return bench.returncode


def clean(frame):
    frame = re.sub(r"\[[0-9a-f]{16}\]", "", frame)
    return re.sub(r"\.llvm\.\d+", "", frame)[:160]


def aggregate(path, needle, top):
    text = open(path).read()
    demangled = subprocess.run(["c++filt"], input=text, capture_output=True, text=True).stdout
    inclusive, innermost, matching = collections.Counter(), collections.Counter(), 0
    for chunk in demangled.split("----"):
        frames = [clean(m.group(1)) for m in re.finditer(r"#\d+\s+0x[0-9a-f]+\s+(.*)", chunk)]
        if not any(needle in frame for frame in frames):
            continue
        matching += 1
        inclusive.update(set(frames))
        innermost[frames[0]] += 1
    print(f"samples matching {needle!r}: {matching}")
    print("== innermost")
    for frame, count in innermost.most_common(top):
        print(f"{count:5d} {100 * count / max(matching, 1):5.1f}% {frame}")
    print("== inclusive")
    for frame, count in inclusive.most_common(top * 3):
        print(f"{count:5d} {100 * count / max(matching, 1):5.1f}% {frame}")


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--binary")
    parser.add_argument("--corpus")
    parser.add_argument("--out", required=True)
    parser.add_argument("--interval", type=float, default=0.02)
    parser.add_argument("--filter", default="ingest")
    parser.add_argument("--top", type=int, default=25)
    options = parser.parse_args()
    if options.binary:
        code = sample(options.binary, options.corpus, options.out, options.interval)
        if code != 0:
            raise SystemExit(f"bench exited {code}")
    aggregate(options.out, options.filter, options.top)


if __name__ == "__main__":
    main()
