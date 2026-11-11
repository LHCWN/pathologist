#!/usr/bin/env python3
"""Enforce per-crate line-coverage floors from an lcov report.

Usage:
    coverage_check.py lcov.info [--floor crate=PERCENT ...] [--total-floor PERCENT]

Counts every executable line under ``crates/<name>/src/`` inside the
workspace, including inline ``#[cfg(test)]`` modules. Paths are normalized, so
``src/../build_support.rs`` is not treated as a source file. Separate
test/example/bench directories, crate-root build scripts, and dependency
checkouts under ``.cargo/``, ``vendor/`` or ``target/`` are ignored. Floors
are set below the baseline measured by the coverage CI job.
"""
from __future__ import annotations

import argparse
from collections import defaultdict
import math
from pathlib import Path
import posixpath
import re
import sys

# Current measured values minus a small margin (see the coverage job in CI).
DEFAULT_FLOORS = {
    "trace-analysis": 94.0,
    "trace-capi": 86.0,
    "trace-cli": 93.0,
    "trace-db": 95.0,
    "trace-ir": 96.0,
    "trace-lsp": 94.0,
    "trace-merge": 92.0,
    "trace-parse": 94.0,
    "trace-preproc": 94.0,
}
DEFAULT_TOTAL_FLOOR = 94.0

_CRATE_SOURCE = re.compile(r"(?:^|/)crates/([^/]+)/src/")


def _source_path(path: str, root: Path | None) -> str | None:
    """Normalize one lcov path to a workspace-relative path, if it is source."""
    path = posixpath.normpath(path.replace("\\", "/"))
    if root is not None:
        root_path = posixpath.normpath(str(root).replace("\\", "/"))
        if path == root_path:
            return None
        prefix = root_path.rstrip("/") + "/"
        if path.startswith(prefix):
            path = path[len(prefix):]
        elif path.startswith("/") or re.match(r"^[A-Za-z]:/", path):
            # An absolute path outside the workspace is a dependency checkout.
            return None
    if path.startswith(("vendor/", ".cargo/", "target/", ".git/")):
        return None
    return path


def per_crate(text: str, root: Path | None = None) -> dict[str, tuple[int, int]]:
    """Return ``{crate: (lines, missed)}`` for workspace ``src/`` files only.

    ``root`` restricts absolute paths to the workspace; when omitted, paths
    are assumed to belong to it (tests and relative lcov reports).
    """
    totals: dict[str, list[int]] = defaultdict(lambda: [0, 0])
    current = None
    for line in text.splitlines():
        if line.startswith("SF:"):
            relative = _source_path(line[3:], root)
            match = _CRATE_SOURCE.search(relative) if relative else None
            current = match.group(1) if match else None
        elif line.startswith("DA:") and current is not None:
            try:
                _, count = line[3:].split(",", 1)
                count = int(count.split(",", 1)[0])
            except ValueError:
                continue
            totals[current][0] += 1
            if count == 0:
                totals[current][1] += 1
        elif line == "end_of_record":
            current = None
    if not totals:
        raise ValueError("no workspace `crates/*/src/` coverage records in the report")
    return {crate: (lines, missed) for crate, (lines, missed) in totals.items()}


def percentage(lines: int, missed: int) -> float:
    return 100.0 * (lines - missed) / lines if lines else 0.0


def failures(
    per_crate_counts: dict[str, tuple[int, int]],
    floors: dict[str, float],
    total_floor: float | None,
) -> list[str]:
    """Human-readable floor violations; empty means the report passed."""
    problems = []
    for crate, floor in sorted(floors.items()):
        if crate not in per_crate_counts:
            problems.append(f"{crate}: no coverage records (floor {floor:.2f}%)")
            continue
        lines, missed = per_crate_counts[crate]
        cover = percentage(lines, missed)
        if cover < floor:
            problems.append(
                f"{crate}: {cover:.2f}% lines below floor {floor:.2f}% "
                f"({lines - missed}/{lines})"
            )
    if total_floor is not None:
        lines = sum(l for l, _ in per_crate_counts.values())
        missed = sum(m for _, m in per_crate_counts.values())
        cover = percentage(lines, missed)
        if cover < total_floor:
            problems.append(
                f"total: {cover:.2f}% lines below floor {total_floor:.2f}% "
                f"({lines - missed}/{lines})"
            )
    return problems


def render(per_crate_counts: dict[str, tuple[int, int]]) -> str:
    rows = [f"{'crate':20} {'lines':>8} {'missed':>8} {'cover':>8}"]
    for crate, (lines, missed) in sorted(per_crate_counts.items()):
        rows.append(f"{crate:20} {lines:8d} {missed:8d} {percentage(lines, missed):7.2f}%")
    lines = sum(l for l, _ in per_crate_counts.values())
    missed = sum(m for _, m in per_crate_counts.values())
    rows.append(f"{'TOTAL':20} {lines:8d} {missed:8d} {percentage(lines, missed):7.2f}%")
    return "\n".join(rows)


def parse_percentage(value: str) -> float:
    """A finite percentage in ``0..=100``; ``nan``/``inf`` are rejected."""
    try:
        percent = float(value)
    except ValueError:
        raise argparse.ArgumentTypeError(f"not a percentage: {value!r}")
    if not math.isfinite(percent) or not 0.0 <= percent <= 100.0:
        raise argparse.ArgumentTypeError(f"expected a percentage in 0..100, got {value!r}")
    return percent


def parse_floor(value: str) -> tuple[str, float]:
    crate, _, number = value.partition("=")
    if not crate or not number:
        raise argparse.ArgumentTypeError(f"expected crate=PERCENT, got {value!r}")
    return crate, parse_percentage(number)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("lcov", type=Path, help="lcov.info produced by llvm-cov")
    parser.add_argument("--floor", action="append", type=parse_floor, default=[],
                        metavar="CRATE=PERCENT", help="override one crate floor; other defaults still apply")
    parser.add_argument("--total-floor", type=parse_percentage, default=DEFAULT_TOTAL_FLOOR,
                        help="minimum total line coverage (default %(default)s)")
    args = parser.parse_args(argv)

    try:
        root = Path(__file__).resolve().parent.parent
        counts = per_crate(args.lcov.read_text(), root=root)
    except (OSError, ValueError) as error:
        print(f"coverage_check: {error}", file=sys.stderr)
        return 2

    floors = dict(DEFAULT_FLOORS)
    for crate, floor in args.floor:
        floors[crate] = floor
    problems = failures(counts, floors, args.total_floor)

    print(render(counts))
    for problem in problems:
        print(f"coverage floor failed: {problem}", file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
