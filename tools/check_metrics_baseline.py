"""Assert that each BUILD's preserved replay still produces sane MATCH METRICS.

Every other check reads counters that describe the FRAMING -- blocks, fields,
RPCs, malformed packets, skipped bits -- or compares bytes against a frozen
export, and a decoder that stops producing values emits no rows and moves no
framing counter. Build 13.02 shifted `RoundResults` from handle 93 to 81, the
match score stopped being written, and every framing guard stayed green. The
break shows one layer up, where the rows become a scoreboard, so this guard
runs that layer:

    vrfkit export -> tools/to_valplay_bundle.py -> valplay compute_metrics.py

Before bcc7d70 the 13.02 fixture failed it: `objective.round_count` was 0
while `rounds.round_count` was 21 (invariants R1 and R2 below).

Two kinds of check, and the first matters more
---------------------------------------------

**Invariants** hold for any replay of any build and need no baseline. They
survive legitimate changes that move counts, which pinned numbers do not, and
they are the ones that encode the section-26 failure directly.

**Pinned values** are drift detection for everything else. They live in ONE
file covering every build, so a build disappearing from the set is itself a
failure, which per-file baselines cannot see.

`kills == deaths` is deliberately NOT an invariant: a resurrected player who
dies again in the same round gets two `bDied` reports, so `deaths` counts both,
while `kills` counts DidKill per (round, subject) interaction and collapses
them. Across five Swiftplay replays the gap was 0 without a resurrection and
exactly 1 in each of the two with one. It is pinned instead.

Cost
----

This is the slowest check in the repo: it exports, re-nests and recomputes five
full 44-65 MB matches (13.01, 13.02, 13.04, 13.05, 13.06) besides three sub-MB
public fixtures. Run it after a non-trivial change, not in a fast sweep.

Usage:
    python tools/check_metrics_baseline.py
    python tools/check_metrics_baseline.py --update
    python tools/check_metrics_baseline.py --only 13.02 --jobs 1
"""
from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

if __package__:
    from .atomic_io import atomic_write_text
else:  # direct script execution
    from atomic_io import atomic_write_text

REPO = Path(__file__).resolve().parent.parent
DEFAULT_EXE = REPO / "target" / "release" / "vrfkit.exe"
BUNDLE_TOOL = REPO / "tools" / "to_valplay_bundle.py"
DEFAULT_BASELINE = REPO / "tools" / "baselines" / "metrics_builds.json"

#: valplay is NEVER modified; it is only ever invoked by absolute path.
#: Set VRFKIT_VALPLAY_DIR to the valplay checkout root.
COMPUTE_METRICS = Path(
    os.environ.get("VRFKIT_VALPLAY_DIR", "")
) / "pipeline" / "metrics" / "compute_metrics.py"

#: One replay per build. 13.01 is the reference replay, which lives in the
#: read-only valplay corpus. Never point one at the game's own Saved\Demos:
#: the game rotates it, and on 2026-08-02 all four replays pinned there were
#: gone. 13.06's fixture was chosen from six preserved replays run through this
#: pipeline (all pass R1-R5): 2a7d2c4c is a full match (22 rounds, 13-9, 10
#: players, 170 kills = 170 deaths, 59.6 MB); abf07066 (7-0) and ee4c3f26
#: (8-2) are not, and 57881928, a4b7406f and e02e6230 are the alternatives.
REPLAYS = {
    "12.10": r"%LOCALAPPDATA%\vrfkit\baseline-corpora\build_1210"
             r"\9f8b32c5-c243-41ec-bbbb-832582edf652.12_10.vrf",
    "12.11": r"%LOCALAPPDATA%\vrfkit\baseline-corpora\build_1211"
             r"\5c673443-5bdc-4576-b416-aab3f62471a5.12_11.vrf",
    "13.00": r"%LOCALAPPDATA%\vrfkit\baseline-corpora\build_1300"
             r"\12974d2b-848f-490d-80ba-5f03a033c2d5.13_00.vrf",
    "13.01": "02d4d478-1dfb-4412-9a77-29ca29105a9d.vrf",
    "13.02": r"%LOCALAPPDATA%\vrfkit\baseline-corpora\build_1302\1.vrf",
    "13.04": r"%LOCALAPPDATA%\vrfkit\baseline-corpora\build_1304"
             r"\01e0979f-660f-4121-b3ce-84911860df8e.vrf",
    "13.05": r"%LOCALAPPDATA%\vrfkit\baseline-corpora\build_1305"
             r"\005f5193-35ef-4ade-8539-e9e8dd0d5ed7.vrf",
    "13.06": r"%LOCALAPPDATA%\vrfkit\baseline-corpora\build_1306"
             r"\2a7d2c4c-952b-444e-9e45-c45c5ae77610.vrf",
}


def _resolve_replay(raw: str) -> Path:
    """Expand env vars in a REPLAYS entry; anchor a bare filename in
    VRFKIT_CORPUS_DIR so a portable baseline still finds the replay."""
    p = Path(os.path.expandvars(raw))
    if not p.is_absolute():
        corpus_dir = os.environ.get("VRFKIT_CORPUS_DIR", "")
        if corpus_dir:
            p = Path(corpus_dir) / p
    return p


def _sum(d: dict, field: str) -> int:
    return sum(p.get(field) or 0 for p in d.values())


def extract(m: dict) -> dict:
    """The pinned figures, all of them semantic rather than framing."""
    combat = m["combat"]["per_player"]
    tac = m["tactical"]["per_player"]
    return {
        "rounds_rpc": m["rounds"]["round_count"],
        "rounds_objective": m["objective"]["round_count"],
        "client_round_starts": m["rounds"]["client_round_start_events"],
        "team_score": m["objective"]["team_score"],
        "plants": m["objective_detail"]["plant_count"],
        "defuses": m["objective_detail"]["defuse_count"],
        "players": len(m["players"]),
        "combat_players": len(combat),
        "kills": _sum(combat, "kills"),
        "deaths": _sum(combat, "deaths"),
        "assists": _sum(combat, "assists"),
        "headshots": _sum(combat, "headshots"),
        "damage_dealt": round(sum(p["damage_dealt"] for p in combat.values()), 2),
        "first_bloods": _sum(tac, "first_bloods"),
        "trade_kills": _sum(tac, "trade_kills"),
        "kast_rounds": _sum(m["kast"]["per_player"], "kast_rounds"),
        "ultimate_casts": m["ultimate"]["total_casts"],
        "distinct_weapons": m["weapons"]["distinct_weapons"],
        "shots": sum(m["weapons"]["shots_by_weapon"].values()),
        "shot_rays": m["shot_rays"]["ray_count"],
        "ability_spawns": m["ability_usage"]["ability_spawn_count"],
        "movement_samples": m["movement_summary"]["movement_samples"],
        "economy_rounds": m["economy_detail"]["rounds"],
    }


def invariants(v: dict) -> list[str]:
    """Checks that need no baseline. Each returns a message when it FAILS.

    R1-R3 state the 13.02 break three ways; before bcc7d70 the 13.02 fixture
    violated R1 and R2 (objective 0 vs rpc 21). R3 does not fire on it.
    """
    bad = []
    if v["rounds_objective"] <= 0:
        bad.append(
            f"R1 objective.round_count is {v['rounds_objective']}: the "
            f"BombGameState round results produced nothing. This is the exact "
            f"shape of the 13.02 RoundResults handle shift (section 26)."
        )
    if v["rounds_rpc"] != v["rounds_objective"]:
        bad.append(
            f"R2 round count disagrees between its two independent sources: "
            f"ClientRoundStart RPCs say {v['rounds_rpc']}, BombGameState "
            f"RoundResults say {v['rounds_objective']}."
        )
    score_total = sum(v["team_score"].values())
    if score_total != v["rounds_objective"]:
        bad.append(
            f"R3 team_score sums to {score_total} but there are "
            f"{v['rounds_objective']} rounds: every round has exactly one "
            f"winner, so these cannot differ."
        )
    if v["players"] <= 0:
        bad.append("R4 no players were identified at all.")
    if v["kills"] > 0 and v["damage_dealt"] <= 0:
        bad.append(
            f"R5 {v['kills']} kills but zero damage: the combat report "
            f"stopped decoding while the kill timeline kept working."
        )
    return bad


def invariant_count() -> int:
    """How many checks `invariants()` runs, for the pass-message at the end:
    the distinct `R<n>` labels in its source, since it returns only failures
    and a literal would not move when a check is added or dropped."""
    import inspect
    return len(set(re.findall(r"\bR\d+\b", inspect.getsource(invariants))))


def pipeline_paths(root: Path) -> tuple[Path, Path, Path]:
    """Return sibling paths so the adapter input and output never overlap."""
    return root / "export", root / "bundle", root / "metrics.json"


def run_one(build: str, replay: Path, exe: Path) -> tuple[str, dict | None, str]:
    """export -> bundle -> compute_metrics, into a scratch dir that is removed."""
    if not replay.is_file():
        return build, None, f"replay not found: {replay}"
    out = Path(tempfile.mkdtemp(prefix=f"vrfkit-metrics-{build.replace('.', '_')}-"))
    try:
        export_dir, bundle_dir, metrics_path = pipeline_paths(out)
        steps = (
            ("export", [str(exe), "export", str(replay), "--out", str(export_dir)]),
            ("bundle", [sys.executable, str(BUNDLE_TOOL), str(export_dir),
                        "-o", str(bundle_dir)]),
            ("metrics", [sys.executable, str(COMPUTE_METRICS), str(bundle_dir),
                         "-o", str(metrics_path)]),
        )
        for name, cmd in steps:
            r = subprocess.run(cmd, capture_output=True, text=True,
                               encoding="utf-8", errors="replace", timeout=1800)
            if r.returncode != 0:
                tail = ((r.stderr or "") + (r.stdout or "")).strip().splitlines()
                return build, None, f"{name} failed rc={r.returncode}: " + (
                    " | ".join(tail[-3:])[:300] if tail else "no output")
        mj = metrics_path
        if not mj.exists():
            return build, None, "metrics.json was not written"
        return build, extract(json.loads(mj.read_text(encoding="utf-8"))), ""
    except subprocess.TimeoutExpired:
        return build, None, "timeout"
    finally:
        shutil.rmtree(out, ignore_errors=True)


def merged_metrics(stored: dict, fresh: dict, only) -> dict:
    """The metrics to write on `--update`, given what was already pinned.

    A scoped run (`--only 13.02`) knows nothing about the other builds, so it
    keeps them. An unscoped run looked at every build in REPLAYS, so it alone
    may retire one: merging there would keep a build pinned after it left
    REPLAYS and fail every later run with "MISSING from this run".
    """
    return dict(fresh) if only is None else {**stored, **fresh}


def compare(build: str, got: dict, want: dict) -> list[str]:
    drift = []
    for key in sorted(set(got) | set(want)):
        a, b = got.get(key, "<absent>"), want.get(key, "<absent>")
        if a != b:
            drift.append(f"{build} {key}: got {a}, baseline {b}")
    return drift


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--baseline", type=Path, default=DEFAULT_BASELINE)
    ap.add_argument("--exe", type=Path, default=DEFAULT_EXE)
    ap.add_argument("--jobs", type=int, default=3)
    ap.add_argument("--only", action="append", default=None,
                    help="limit to these builds (repeatable)")
    ap.add_argument("--update", action="store_true",
                    help="rewrite the baseline from this run")
    args = ap.parse_args()

    if not args.exe.is_file():
        print(f"executable not found: {args.exe}\n"
              f"build it with: cargo build --release -p vrfkit", file=sys.stderr)
        return 2
    if not COMPUTE_METRICS.is_file():
        print(f"valplay compute_metrics not found: {COMPUTE_METRICS}\n"
              f"set VRFKIT_VALPLAY_DIR to the valplay checkout root",
              file=sys.stderr)
        return 2

    builds = {k: v for k, v in REPLAYS.items()
              if args.only is None or k in args.only}
    if not builds:
        print(f"no builds selected; known: {sorted(REPLAYS)}", file=sys.stderr)
        return 2

    print(f"running export -> bundle -> compute_metrics on {len(builds)} build(s), "
          f"{args.jobs}-wide")
    started = time.time()
    results, failures = {}, []
    with ThreadPoolExecutor(max_workers=args.jobs) as pool:
        for build, values, err in pool.map(
            lambda kv: run_one(kv[0], _resolve_replay(kv[1]), args.exe),
            sorted(builds.items()),
        ):
            if values is None:
                failures.append(f"{build}: {err}")
                print(f"  {build}  PIPELINE FAILED  {err}")
            else:
                results[build] = values
                print(f"  {build}  rounds={values['rounds_objective']:>3} "
                      f"score={values['team_score']} "
                      f"players={values['players']:>2} "
                      f"kills={values['kills']:>4}")
    elapsed = time.time() - started
    print(f"elapsed {elapsed:.1f}s")

    # Invariants first: they are the point, and they apply even with --update.
    broken = []
    for build, values in sorted(results.items()):
        for msg in invariants(values):
            broken.append(f"{build}: {msg}")

    if broken:
        print(f"\nFAILED: {len(broken)} invariant violation(s)", file=sys.stderr)
        for msg in broken:
            print(f"    {msg}", file=sys.stderr)
        if args.update:
            print("  baseline NOT updated -- refusing to pin a broken run.",
                  file=sys.stderr)
        return 1
    if failures:
        print(f"\nFAILED: {len(failures)} build(s) did not complete the pipeline",
              file=sys.stderr)
        for msg in failures:
            print(f"    {msg}", file=sys.stderr)
        return 1

    if args.update:
        stored = json.loads(args.baseline.read_text(encoding="utf-8")) \
            if args.baseline.is_file() else {}
        metrics = merged_metrics(stored.get("metrics", {}), results, args.only)
        payload = {
            "note": "Semantic metrics per build. See the module docstring: "
                    "framing counters cannot see a decoder that stops "
                    "producing values.",
            "replays": REPLAYS,
            "metrics": metrics,
        }
        atomic_write_text(
            args.baseline, json.dumps(payload, indent=1, sort_keys=True) + "\n")
        kept = sorted(set(metrics) - set(results))
        print(f"\nwrote {args.baseline}: re-pinned {len(results)} build(s)"
              + (f", kept {len(kept)} not looked at ({', '.join(kept)})"
                 if kept else ""))
        return 0

    if not args.baseline.is_file():
        print(f"\nbaseline not found: {args.baseline}\n"
              f"create it with --update", file=sys.stderr)
        return 2
    baseline = json.loads(args.baseline.read_text(encoding="utf-8"))
    want = baseline.get("metrics", {})

    drift = []
    for build in sorted(results):
        if build not in want:
            drift.append(f"{build}: present in this run, absent from the baseline")
        else:
            drift.extend(compare(build, results[build], want[build]))
    for build in sorted(want):
        if build not in results and (args.only is None or build in args.only):
            drift.append(f"{build}: in the baseline, MISSING from this run")

    if drift:
        print(f"\nFAILED: {len(drift)} metric(s) drifted from "
              f"{args.baseline.name}", file=sys.stderr)
        for msg in drift:
            print(f"    {msg}", file=sys.stderr)
        print("  If the change is intended, re-pin with --update.", file=sys.stderr)
        return 1

    n_inv = len(results) * invariant_count()
    print(f"\nOK: {len(results)} build(s) pass {n_inv} invariant checks and match "
          f"{sum(len(v) for v in results.values())} pinned metric values")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
