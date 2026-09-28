#!/usr/bin/env python3
"""Pin a corpus's oracle numbers and fail when they drift.

validate_corpus.py prints what a corpus currently does; a regression guard
has to answer "did my change move it". This stores per-file and total figures
in a JSON baseline and exits non-zero on any difference.

The replays live outside the repo (a relative corpus path resolves against
VRFKIT_CORPUS_DIR), so a missing corpus is reported and SKIPPED rather than
failed, unless --require-input or VRFKIT_REQUIRE_CORPUS is set: a guard that
fails on someone else's machine gets disabled, and a disabled guard protects
nothing.

Usage:
    python tools/check_corpus_baseline.py --baseline tools/baselines/build_1302.json
    python tools/check_corpus_baseline.py --baseline <path> --update
"""

from __future__ import annotations

import argparse
import json
import os
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from validate_corpus import _run_one, parse_oracle_output  # noqa: E402
from corpus_scan import find_replays  # noqa: E402

if __package__:
    from .atomic_io import atomic_write_text
else:  # direct script execution
    from atomic_io import atomic_write_text

REPO = Path(__file__).resolve().parent.parent
DEFAULT_EXE = REPO / "target" / "release" / "vrfkit.exe"


def measure(exe: Path, root: Path) -> dict:
    """Run the oracle over every .vrf under root and collect the numbers."""
    files = find_replays(root, recursive=True)
    per_file = {}
    totals = {"blocks": 0, "fields": 0, "rpcs": 0, "malformed": 0, "skipped": 0}
    branches = {}

    for f in files:
        name = f.relative_to(root).as_posix()
        error, out = _run_one(exe, f)
        if error is None:
            got, error = parse_oracle_output(out)
        if error is not None:
            per_file[name] = {"error": error}
            continue
        branch = got["branch"].group(1)
        branches[branch] = branches.get(branch, 0) + 1
        entry = {"branch": branch, "rate": got["rate"].group(1)}
        for key in totals:
            match = got.get(key)
            if match is None:
                # None, not 0: a counter the oracle stopped printing is a
                # change worth failing on, which a total would hide.
                entry[key] = None
                continue
            value = int(match.group(1))
            entry[key] = value
            totals[key] += value
        per_file[name] = entry

    return {"branches": branches, "totals": totals, "per_file": per_file}


def unpinnable(current: dict) -> list[str]:
    """Why this run must not become a baseline, if it must not.

    A replay the oracle could not validate (recorded as `{"error": ...}` and
    left out of the totals) or a counter it did not print (None) would pin a
    number never measured, which a later run failing the same way MATCHES.
    """
    reasons = []
    if not current["per_file"]:
        return ["no replay produced any numbers"]
    for name in sorted(current["per_file"]):
        entry = current["per_file"][name]
        if "error" in entry:
            reasons.append(f"{name}: the oracle failed ({entry['error']})")
            continue
        for key in sorted(k for k, v in entry.items() if v is None):
            reasons.append(f"{name}: the oracle did not print {key}")
    return reasons


def diff(baseline: dict, current: dict) -> list[str]:
    """Every way the two disagree, as human-readable lines."""
    out = []
    for key, want in baseline["totals"].items():
        got = current["totals"].get(key)
        if got != want:
            out.append(f"total {key}: {got} (baseline {want})")
    if baseline["branches"] != current["branches"]:
        out.append(f"branches: {current['branches']} (baseline {baseline['branches']})")

    b_files, c_files = set(baseline["per_file"]), set(current["per_file"])
    for name in sorted(b_files - c_files):
        out.append(f"missing replay: {name}")
    for name in sorted(c_files - b_files):
        out.append(f"replay not in baseline: {name}")
    for name in sorted(b_files & c_files):
        want, got = baseline["per_file"][name], current["per_file"][name]
        for key in sorted(set(want) | set(got)):
            if want.get(key) != got.get(key):
                out.append(f"{name} {key}: {got.get(key)} (baseline {want.get(key)})")
    return out


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--baseline", type=Path, required=True)
    ap.add_argument("--exe", type=Path, default=DEFAULT_EXE)
    ap.add_argument("--corpus", type=Path, default=None,
                    help="overrides the corpus path stored in the baseline")
    ap.add_argument("--update", action="store_true",
                    help="rewrite the baseline from the current numbers")
    ap.add_argument("--require-input", action="store_true",
                    help="fail instead of skipping when the corpus is absent/empty")
    args = ap.parse_args()

    if not args.exe.exists():
        print(f"build the release binary first: {args.exe}", file=sys.stderr)
        return 2

    stored = json.loads(args.baseline.read_text(encoding="utf-8")) \
        if args.baseline.exists() else {}
    # Decided on the text, not the Path: Path("") is Path("."), which exists,
    # so a baseline naming no corpus walked and pinned the working directory.
    named = args.corpus or os.path.expandvars(stored.get("corpus", ""))
    corpus = Path(named) if named else None
    # A relative path in the baseline resolves against VRFKIT_CORPUS_DIR so the
    # repo ships no absolute path; absolute paths and --corpus are used as-is.
    if corpus is not None and corpus.name and not corpus.is_absolute():
        corpus_dir = os.environ.get("VRFKIT_CORPUS_DIR", "")
        if corpus_dir:
            corpus = Path(corpus_dir) / corpus
    if corpus is None or not corpus.exists():
        missing = ("no corpus named (pass --corpus or store one in the baseline)"
                   if corpus is None else f"corpus not present ({corpus})")
        if args.require_input or os.environ.get("VRFKIT_REQUIRE_CORPUS"):
            print(f"REQUIRED INPUT MISSING: {missing}", file=sys.stderr)
            return 2
        print(f"SKIP: {missing}")
        print("      these replays are machine-local; nothing to guard here.")
        return 0

    current = measure(args.exe, corpus)
    if not current["per_file"]:
        if args.require_input or os.environ.get("VRFKIT_REQUIRE_CORPUS"):
            print(f"REQUIRED INPUT MISSING: no .vrf under {corpus}", file=sys.stderr)
            return 2
        print(f"SKIP: no .vrf under {corpus}")
        return 0

    if args.update:
        refusals = unpinnable(current)
        if refusals:
            print(f"FAILED: refusing to pin a broken run -- {len(refusals)} "
                  f"figure(s) were never measured", file=sys.stderr)
            for line in refusals[:15]:
                print(f"  {line}", file=sys.stderr)
            print("  Fix the run first; a baseline of zeros is matched by the "
                  "same failure next time.", file=sys.stderr)
            return 1
        payload = {"corpus": stored.get("corpus") or str(corpus), **current}
        atomic_write_text(args.baseline, json.dumps(payload, indent=1) + "\n")
        n = len(current["per_file"])
        print(f"wrote {args.baseline} ({n} replays, "
              f"branches {current['branches']})")
        return 0

    if not stored:
        print(f"no baseline at {args.baseline} -- run with --update",
              file=sys.stderr)
        return 2

    problems = diff(stored, current)
    n = len(current["per_file"])
    if problems:
        print(f"DRIFT: {len(problems)} difference(s) across {n} replays")
        for line in problems:
            print(f"  {line}")
        return 1

    print(f"OK: {n} replays match the baseline "
          f"(branches {current['branches']}, "
          f"malformed {current['totals']['malformed']})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
