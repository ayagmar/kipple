#!/usr/bin/env python3
"""Measures commands against a bench-tree (docs/11-delivery-plan.md, S3 procedure).

Prints a Markdown table with each command's median wall time, median peak RSS and,
for warm runs, its syscall count from `strace -f -c`. Peak RSS comes from GNU time
(`/usr/bin/time`): measured from this script, the forked Python process's own memory
would count toward the child's peak, because Linux keeps it across `exec`.

Each command is one argument. It is split on whitespace, never run through a shell, and
`{tree}` in it becomes the tree path. Runs are interleaved across commands. `cold` drops
the page cache with sudo before every run, so use it only on a disposable VM. `warm`
makes one unmeasured run per command first. Linux only. Without GNU time or strace the
matching column says n/a.
"""

import argparse
import os
import shutil
import statistics
import subprocess
import sys
import tempfile
import time

GNU_TIME = "/usr/bin/time"


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("mode", choices=["cold", "warm"])
    parser.add_argument("tree")
    parser.add_argument("runs", type=int)
    parser.add_argument("commands", nargs="+")
    args = parser.parse_args()
    commands = [command.strip() for command in args.commands]
    argvs = [[word.replace("{tree}", args.tree) for word in c.split()] for c in commands]

    if args.mode == "warm":
        for argv in argvs:
            run(argv)

    walls = [[] for _ in argvs]
    rss = [[] for _ in argvs]
    outputs = [""] * len(argvs)
    for _ in range(args.runs):
        for i, argv in enumerate(argvs):
            if args.mode == "cold":
                drop_caches()
            wall_ms, rss_kib, outputs[i] = run(argv)
            walls[i].append(wall_ms)
            rss[i].append(rss_kib)

    print("| command | output | median wall (ms) | runs (ms) | median peak RSS (MiB) | syscalls |")
    print("| --- | --- | ---: | --- | ---: | ---: |")
    for i, command in enumerate(commands):
        calls = syscalls(argvs[i]) if args.mode == "warm" else "n/a (cold)"
        peak = "n/a (no GNU time)"
        if None not in rss[i]:
            peak = f"{statistics.median(rss[i]) / 1024:.1f}"
        print(
            f"| `{command}` | `{outputs[i]}` | {statistics.median(walls[i]):.0f} "
            f"| {' '.join(f'{w:.0f}' for w in walls[i])} | {peak} | {calls} |"
        )


def run(argv: list[str]) -> tuple[float, float | None, str]:
    """One run: wall time in ms, peak RSS in KiB (None without GNU time), first stdout line."""
    with tempfile.TemporaryFile() as out, tempfile.NamedTemporaryFile(mode="r") as rss:
        timed = [GNU_TIME, "-f", "%M", "-o", rss.name, *argv] if has_gnu_time() else argv
        start = time.perf_counter_ns()
        status = subprocess.run(timed, stdout=out, check=False).returncode
        wall_ms = (time.perf_counter_ns() - start) / 1e6
        if status != 0:
            sys.exit(f"bench.py: {argv} exited with {status}")
        out.seek(0)
        first_line = out.readline().decode(errors="replace").strip()
        peak = float(rss.read().split()[-1]) if has_gnu_time() else None
    return wall_ms, peak, first_line


def has_gnu_time() -> bool:
    return os.access(GNU_TIME, os.X_OK)


def drop_caches() -> None:
    os.sync()
    subprocess.run(
        ["sudo", "tee", "/proc/sys/vm/drop_caches"],
        input=b"3\n",
        stdout=subprocess.DEVNULL,
        check=True,
    )


def syscalls(argv: list[str]) -> str:
    """Total syscalls of one run and its children, or n/a without strace."""
    if shutil.which("strace") is None:
        return "n/a (no strace)"
    with tempfile.NamedTemporaryFile(mode="r") as report:
        subprocess.run(
            ["strace", "-f", "-c", "-o", report.name, *argv],
            stdout=subprocess.DEVNULL,
            check=True,
        )
        for line in report:
            fields = line.split()
            if fields and fields[-1] == "total":
                return fields[3]
    return "n/a (no strace total)"


if __name__ == "__main__":
    main()
