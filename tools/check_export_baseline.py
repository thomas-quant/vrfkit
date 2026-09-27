#!/usr/bin/env python3
"""Pin the export path's own numbers and fail when they drift.

`check_corpus_baseline.py` guards the *validate* path. Nothing guarded the
*export* path, so every figure the export summary prints -- content blocks,
fields, RPCs, movement rows, NetGUID rows, decode errors, the four Parquet
files themselves -- was pinned only in a comment in the task brief and in
docs/archive/PROJECT_STATUS.md prose. `NetGUID rows: 16167` is the clearest
case: no harness read it, because `validate` never writes Parquet and
`validate_corpus.py`'s PATTERNS has no entry for a counter the oracle does
not print.

That is the same shape as the malformed counter, which went unread for the
project's whole history because its regex never matched. A number nobody
machine-checks is not guarded, however often it appears in a document.

Two independent checks run here, and they fail on different things:

  1. CROSS-CHECK.   Some printed counters are identities against the Parquet
     files: `NetGUID rows` is net_guids.parquet's row count, `Movement rows`
     is movement.parquet's, `Event rows` is events.parquet's, and
     `Actor opens + Actor closes` is actors.parquet's. If the summary and the
     file disagree, the summary is lying, and this fails with no baseline
     needed. The set lives in `cross_check_identities`; do not restate its
     size here, where it cannot be checked.
     (fields.parquet has no such identity: it also carries RPC parameters and
     flattened dynamic-array leaves, so its row count is pinned, not derived.)

  2. BASELINE.      Every counter and every Parquet row count and byte size is
     compared against a pinned JSON. This is what catches the other failure
     mode -- the data moving and the summary faithfully reporting the new,
     wrong number. A cross-check alone cannot see that.
     A byte-size difference with every counter equal means the row VALUES
     moved -- or that the parquet crate version did. Cargo.lock pins it, so
     check that before assuming a data bug, and do not disable the guard.

Both were confirmed to fail on a deliberately broken build before this was
committed; see the commit message.

The .vrf lives outside the repo (under valplay), so a missing replay is
reported and SKIPPED rather than failed -- the same reasoning as
check_corpus_baseline.py: a guard that fails on someone else's machine gets
disabled, and a disabled guard protects nothing.

Usage:
    python tools/check_export_baseline.py --baseline tools/baselines/export_02d4d478.json
    python tools/check_export_baseline.py --baseline <path> --update
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import subprocess
import sys
from pathlib import Path

import pyarrow.parquet as pq

if __package__:
    from .atomic_io import atomic_write_text
else:  # direct script execution
    from atomic_io import atomic_write_text

REPO = Path(__file__).resolve().parent.parent
DEFAULT_EXE = REPO / "target" / "release" / "vrfkit.exe"

# Every counter the export summary prints, except `Elapsed` and the manifest
# path. Anchored on the exact labels driver.rs emits; a label that stops being
# printed is reported as missing rather than defaulted to 0, because a counter
# that silently reads as absent is how this class of bug survives.
COUNTERS = {
    "chunks": r"Chunks:\s+(\d+)",
    "packets": r"Packets:\s+(\d+)",
    "export_groups": r"Export groups:\s+(\d+)",
    "content_blocks": r"Content blocks:\s+(\d+)",
    "rep_layout_blocks": r"RepLayout blocks:\s+(\d+)",
    "class_net_cache_blocks": r"ClassNetCache:\s+(\d+)",
    "fields": r"Fields:\s+(\d+)",
    "rpcs": r"RPCs:\s+(\d+)",
    "actor_opens": r"Actor opens:\s+(\d+)",
    "actor_closes": r"Actor closes:\s+(\d+)",
    "bunches": r"Bunches:\s+(\d+)",
    "malformed_packets": r"Malformed pkts:\s+(\d+)",
    "skipped_bits": r"Skipped bits:\s+(\d+)",
    "movement_rows": r"Movement rows:\s+(\d+)",
    "net_guid_rows": r"NetGUID rows:\s+(\d+)",
    "event_rows": r"Event rows:\s+(\d+)",
    "partial_rows": r"Partial raw rows:\s+(\d+)",
    "partial_bits": r"Partial raw rows:\s+\d+ \((\d+) bits\)",
    "event_layout_mismatches": r"Event layout err:\s+(\d+)",
    "event_payloads_decoded": r"Event payloads:\s+(\d+) decoded",
    "event_payload_unknown_groups": r"Event payloads:\s+\d+ decoded / (\d+) unknown groups",
    "overlay_decoded_ok": r"Decoded OK:\s+(\d+)",
    "overlay_decode_errors": r"Decode errors:\s+(\d+)",
    "overlay_raw_skip": r"Raw/Skip:\s+(\d+)",
    "overlay_not_in_table": r"Not in table:\s+(\d+)",
    "overlay_no_field_name": r"No field name:\s+(\d+)",
    "overlay_rows_offered": r"Rows offered:\s+(\d+)",
    # Not part of the overlay ratio: the overlay buckets are decided before the
    # effect pass runs, so a successful effect decode moves none of the five
    # counters above. Without this line the only evidence that the decoder ran
    # is fields.parquet's byte count, and a byte count cannot say whether the
    # decoder produced values or merely different padding.
    "effect_blobs_decoded": r"Effect blobs:\s+(\d+)",
    # The counter that would have caught build 13.02 on day one. Its decoders
    # are additive, so when RoundResults moved from handle 93 to 81 nothing
    # else on this summary twitched: same blocks, same fields, same rows, same
    # "Decode errors: 0" -- and no match score in the Parquet. `failed` is the
    # alarm; `decoded` is what keeps a decoder that silently stops running
    # (0 decoded, 0 failed) from reading the same as a clean one.
    "struct_blobs_decoded": r"Struct blobs:\s+(\d+) decoded",
    "struct_blobs_failed": r"Struct blobs:\s+\d+ decoded / (\d+) failed",
    "targeting_world_locations_decoded": r"(?m)^\s*Target locations:\s+(\d+) array children\s*$",
    # This is a measured opaque shape, not a decode-error counter: the main
    # corpus is expected to contain it. Require its unconditional summary
    # line and reconcile it with the manifest instead of requiring zero.
    "tracked_rewards_opaque_empty_variants": (
        r"(?m)^\s*Reward opaque:\s+(\d+) empty variants\s*$"
    ),
    # Section bytes the DemoFrame walk stepped over. The skip is
    # length-prefixed, so a build that starts sending ExternalData or
    # GameSpecificFrameData moves nothing else here. Anchored: `cp_frames`
    # below is the unanchored `Frames:\s+(\d+)`, and the two must never be
    # able to read each other's line.
    "frame_external_data_blobs": (
        r"(?m)^\s*Frame skips:\s+(\d+) external blobs / \d+ external bytes"
        r" / \d+ game-specific bytes\s*$"
    ),
    "frame_external_data_bytes": (
        r"(?m)^\s*Frame skips:\s+\d+ external blobs / (\d+) external bytes"
        r" / \d+ game-specific bytes\s*$"
    ),
    "frame_game_specific_bytes": (
        r"(?m)^\s*Frame skips:\s+\d+ external blobs / \d+ external bytes"
        r" / (\d+) game-specific bytes\s*$"
    ),
}
PATTERNS = {k: re.compile(v) for k, v in COUNTERS.items()}
#: The three frame-skip tallies, as the manifest names them after its
#: `frame_` / `checkpoint_frame_` prefixes.
FRAME_SKIP_KEYS = ("external_data_blobs", "external_data_bytes", "game_specific_bytes")

# Only printed under `--checkpoints`, so they live apart from COUNTERS -- a
# default run must not record them as None and then diff that against a
# baseline taken with the flag.
CHECKPOINT_COUNTERS = {
    "cp_partial_rows": r"Checkpoint partial raw:\s+(\d+) rows",
    "cp_partial_bits": r"Checkpoint partial raw:\s+\d+ rows / (\d+) bits",
    "cp_chunks": r"Checkpoints:\s+(\d+)",
    "cp_guid_entries": r"(?m)^\s*GUID entries:\s+(\d+)",
    "cp_group_records": r"Group records:\s+(\d+)",
    "cp_exported_fields": r"Exported fields:\s+(\d+)",
    "cp_frames": r"Frames:\s+(\d+)",
    "cp_frame_packets": r"Frame packets:\s+(\d+)",
    "cp_field_rows": r"Checkpoint rows:\s+(\d+)",
    "cp_actor_rows_written": r"Checkpoint actors:\s*(\d+) rows",
    "cp_net_guid_rows_written": r"Checkpoint GUID rows:\s+(\d+)",
    "cp_block_rows_written": r"Checkpoint blocks:\s*(\d+) rows",
    "cp_guid_entry_rows_written": r"Checkpoint GUID entries:\s*(\d+) rows",
    "cp_export_group_rows_written": r"Checkpoint export groups:\s*(\d+) rows",
    "cp_export_field_rows_written": r"Checkpoint export fields:\s*(\d+) rows",
    "cp_literal_paths": r"(?m)^\s*GUID paths:\s+(\d+) literals / \d+ indices / \d+ resolved\s*$",
    "cp_indexed_paths": r"(?m)^\s*GUID paths:\s+\d+ literals / (\d+) indices / \d+ resolved\s*$",
    "cp_resolved_path_indices": r"(?m)^\s*GUID paths:\s+\d+ literals / \d+ indices / (\d+) resolved\s*$",
    # Deliberately a different label from the main block's "Struct blobs", so
    # these regexes cannot match each other's line.
    "cp_struct_blobs_decoded": r"Checkpoint blobs:\s+(\d+) decoded",
    "cp_struct_blobs_failed": r"Checkpoint blobs:\s+\d+ decoded / (\d+) failed",
    "cp_targeting_world_locations_decoded": r"(?m)^\s*Checkpoint targets:\s+(\d+) array children\s*$",
    "cp_tracked_rewards_opaque_empty_variants": (
        r"(?m)^\s*Checkpoint reward opaque:\s+(\d+) empty variants\s*$"
    ),
    "cp_frame_external_data_blobs": (
        r"(?m)^\s*Checkpoint frame skips:\s+(\d+) external blobs / \d+ external bytes"
        r" / \d+ game-specific bytes\s*$"
    ),
    "cp_frame_external_data_bytes": (
        r"(?m)^\s*Checkpoint frame skips:\s+\d+ external blobs / (\d+) external bytes"
        r" / \d+ game-specific bytes\s*$"
    ),
    "cp_frame_game_specific_bytes": (
        r"(?m)^\s*Checkpoint frame skips:\s+\d+ external blobs / \d+ external bytes"
        r" / (\d+) game-specific bytes\s*$"
    ),
}

PARQUET_FILES = ("fields", "movement", "actors", "net_guids", "events", "partials")
CHECKPOINT_PARQUET_FILES = (
    "checkpoint_fields", "checkpoint_actors", "checkpoint_net_guids", "checkpoint_blocks",
    "checkpoint_guid_entries", "checkpoint_export_groups", "checkpoint_export_fields",
)


def sha256_file(path: Path) -> str:
    """Return a measured content digest without loading a Parquet file whole."""
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def cross_check_identities(counters: dict, parquet: dict) -> list:
    """The printed counters that ARE Parquet row counts.

    Each entry is (label, printed value, actual rows). Split out from
    `cross_checks` so the pass message can count them instead of stating a
    literal: the message said "3" for as long as there were three, and adding
    the fourth made it report a number it had not checked -- the same class of
    claim this whole script exists to catch.
    """
    identities = [
        ("NetGUID rows", counters.get("net_guid_rows"), parquet["net_guids"]["rows"]),
        ("Movement rows", counters.get("movement_rows"), parquet["movement"]["rows"]),
        ("Event rows", counters.get("event_rows"), parquet["events"]["rows"]),
        (
            "Partial raw rows (main + checkpoint)",
            None if counters.get("partial_rows") is None or counters.get("cp_partial_rows", 0) is None
            else counters["partial_rows"] + counters.get("cp_partial_rows", 0),
            parquet["partials"]["rows"],
        ),
        (
            "Actor opens + Actor closes",
            None
            if counters.get("actor_opens") is None or counters.get("actor_closes") is None
            else counters["actor_opens"] + counters["actor_closes"],
            parquet["actors"]["rows"],
        ),
    ]
    if "cp_actor_rows_written" in counters or "checkpoint_actors" in parquet:
        identities.append(("Checkpoint actors", counters.get("cp_actor_rows_written"),
                           parquet.get("checkpoint_actors", {}).get("rows")))
    if "cp_net_guid_rows_written" in counters or "checkpoint_net_guids" in parquet:
        identities.append(("Checkpoint GUID rows", counters.get("cp_net_guid_rows_written"),
                           parquet.get("checkpoint_net_guids", {}).get("rows")))
    if "cp_block_rows_written" in counters or "checkpoint_blocks" in parquet:
        identities.append(("Checkpoint blocks", counters.get("cp_block_rows_written"),
                           parquet.get("checkpoint_blocks", {}).get("rows")))
    for label, written, parsed, table in (
        ("Checkpoint GUID entries", "cp_guid_entry_rows_written", "cp_guid_entries",
         "checkpoint_guid_entries"),
        ("Checkpoint export groups", "cp_export_group_rows_written", "cp_group_records",
         "checkpoint_export_groups"),
        ("Checkpoint export fields", "cp_export_field_rows_written", "cp_exported_fields",
         "checkpoint_export_fields"),
    ):
        if written in counters or parsed in counters or table in parquet:
            actual = parquet.get(table, {}).get("rows")
            identities.append((label, counters.get(written), actual))
            # Compare the schema reader's independent count too: a writer that
            # drops a record must fail even if its own printed count agrees.
            identities.append((label + " parsed", counters.get(parsed), actual))
    return identities


def cross_checks(counters: dict, parquet: dict) -> list[str]:
    """Disagreement between a printed counter and its Parquet file = a lie.

    A counter the summary did not print is itself a failure: the identity
    cannot be checked, which is exactly the state that let the malformed
    counter read as a vacuous 0.
    """
    out = []
    for label, printed, actual in cross_check_identities(counters, parquet):
        if printed is None:
            out.append(f"{label}: the export summary did not print it")
        elif printed != actual:
            out.append(f"{label}: summary says {printed}, Parquet holds {actual}")
    if "cp_guid_entries" in counters:
        required = ("cp_guid_entries", "cp_literal_paths", "cp_indexed_paths",
                    "cp_resolved_path_indices")
        missing = [key for key in required if counters.get(key) is None]
        out.extend(f"{key}: the export summary did not print it" for key in missing)
        if not missing:
            if counters["cp_literal_paths"] + counters["cp_indexed_paths"] != counters["cp_guid_entries"]:
                out.append("Checkpoint GUID paths: literals + indices do not equal GUID entries")
            if counters["cp_resolved_path_indices"] != counters["cp_indexed_paths"]:
                out.append("Checkpoint GUID paths: resolved indices do not equal indexed paths")
    return out


def unpinnable(current: dict) -> list[str]:
    """Counters this run did not measure, which therefore must not be pinned.

    `measure` records a counter the summary did not print as None rather than
    0, for the reason stated at `COUNTERS`. Writing that None into the baseline
    undoes the whole point: from then on a summary that has STOPPED printing
    the counter compares equal to it, and the drift check reports OK.

    The cross-check already refuses this for the four counters that are Parquet
    row identities. This covers the rest, which had nothing.
    """
    return [f"{key}: the export summary did not print it"
            for key in sorted(current["counters"])
            if current["counters"][key] is None]


def checkpoint_manifest_errors(out_dir: Path, counters: dict | None = None) -> list[str]:
    """Checkpoint actor rows must be written, never silently dropped."""
    try:
        manifest = json.loads((out_dir / "manifest.json").read_text(encoding="utf-8"))
        checkpoints = manifest["quality"]["checkpoints"]
        dropped = checkpoints["checkpoint_actor_rows_dropped"]
        mode = checkpoints["checkpoint_path_resolution_mode"]
        literals = checkpoints["checkpoint_literal_paths"]
        indices = checkpoints["checkpoint_indexed_paths"]
        resolved = checkpoints["checkpoint_resolved_path_indices"]
        guid_entries = checkpoints["checkpoint_guid_entries"]
    except (OSError, KeyError, TypeError, json.JSONDecodeError) as exc:
        return [f"checkpoint manifest omits required checkpoint quality data: {exc}"]
    if dropped != 0:
        return [f"checkpoint manifest says checkpoint_actor_rows_dropped={dropped}, expected 0"]
    errors = []
    values = {"cp_literal_paths": literals, "cp_indexed_paths": indices,
              "cp_resolved_path_indices": resolved, "cp_guid_entries": guid_entries}
    if any(type(value) is not int or value < 0 for value in values.values()):
        return ["checkpoint manifest GUID path counts must be nonnegative integers"]
    if mode != "preceding_literal_zero_based":
        errors.append(f"checkpoint manifest path resolution mode is {mode!r}")
    if literals + indices != guid_entries:
        errors.append("checkpoint manifest GUID paths: literals + indices do not equal GUID entries")
    if resolved != indices:
        errors.append("checkpoint manifest GUID paths: resolved indices do not equal indexed paths")
    if counters is not None:
        for key, value in values.items():
            if counters.get(key) != value:
                errors.append(f"checkpoint manifest {key}={value} disagrees with summary {counters.get(key)}")
    return errors


def reward_opaque_manifest_errors(
    out_dir: Path, counters: dict, checkpoints: bool,
) -> list[str]:
    """The measured reward count must agree between CLI and manifest.

    It is deliberately not folded into a decode-error-zero gate: the count
    records a known opaque payload variant and can legitimately be nonzero.
    """
    try:
        quality = json.loads((out_dir / "manifest.json").read_text(encoding="utf-8"))["quality"]
        main = quality["sink"]["tracked_rewards_opaque_empty_variants"]
        checkpoint = (quality["checkpoints"]["sink"]
                      ["tracked_rewards_opaque_empty_variants"]
                      if checkpoints else None)
    except (OSError, KeyError, TypeError, json.JSONDecodeError) as exc:
        return [f"manifest omits tracked rewards opaque-empty quality data: {exc}"]
    values = {"tracked_rewards_opaque_empty_variants": main}
    if checkpoints:
        values["cp_tracked_rewards_opaque_empty_variants"] = checkpoint
    if any(type(value) is not int or value < 0 for value in values.values()):
        return ["tracked rewards opaque-empty counts must be nonnegative integers"]
    return [
        f"manifest {key}={value} disagrees with summary {counters.get(key)}"
        for key, value in values.items()
        if counters.get(key) != value
    ]


def targeting_manifest_errors(out_dir: Path, counters: dict, checkpoints: bool) -> list[str]:
    """Require the additive targeting count even when it is zero."""
    key = "targeting_world_locations_decoded"
    try:
        quality = json.loads((out_dir / "manifest.json").read_text(encoding="utf-8"))["quality"]
        values = {key: quality["sink"][key]}
        if checkpoints:
            values["cp_" + key] = quality["checkpoints"]["sink"][key]
    except (OSError, ValueError, KeyError, TypeError) as exc:
        return [f"manifest omits targeting world-location quality data: {exc}"]
    if any(type(value) is not int or value < 0 for value in values.values()):
        return ["targeting world-location counts must be nonnegative integers"]
    return [f"manifest {name}={value} disagrees with summary {counters.get(name)}"
            for name, value in values.items() if counters.get(name) != value]


def frame_skip_manifest_errors(out_dir: Path, counters: dict, checkpoints: bool) -> list[str]:
    """The frame-skip tallies must agree between CLI and manifest, zeros included.

    Not a zero gate: skipping these sections is what the reference does, so
    a non-zero count is data this parser leaves undecoded, not a failure. What
    must hold is that the two outputs report the same measurement.
    """
    try:
        quality = json.loads((out_dir / "manifest.json").read_text(encoding="utf-8"))["quality"]
        values = {f"frame_{key}": quality[f"frame_{key}"] for key in FRAME_SKIP_KEYS}
        if checkpoints:
            values.update({f"cp_frame_{key}": quality["checkpoints"][f"checkpoint_frame_{key}"]
                           for key in FRAME_SKIP_KEYS})
    except (OSError, ValueError, KeyError, TypeError) as exc:
        return [f"manifest omits frame-skip quality data: {exc}"]
    if any(type(value) is not int or value < 0 for value in values.values()):
        return ["frame-skip counts must be nonnegative integers"]
    return [f"manifest {name}={value} disagrees with summary {counters.get(name)}"
            for name, value in values.items() if counters.get(name) != value]


def measure(exe: Path, replay: Path, out_dir: Path, checkpoints: bool = False) -> dict:
    """Export one replay and collect the summary counters and Parquet shape.

    The exporter transactionally replaces the complete output directory.  Do
    not delete it here first: on export failure the previous complete result
    must remain available, while a successful replacement removes stale files
    by construction.

    `checkpoints` runs the optional Checkpoint pass and pins its counters and
    its table too. That path is off by default in the exporter, and an
    unguarded optional path is the shape of every silent change this script
    exists to prevent -- so it gets a baseline of its own rather than none.
    """
    cmd = [str(exe), "export", str(replay), "--out", str(out_dir)]
    if checkpoints:
        cmd.append("--checkpoints")
    r = subprocess.run(
        cmd, capture_output=True, text=True, encoding="utf-8",
        errors="replace", timeout=1800,
    )
    text = (r.stdout or "") + (r.stderr or "")
    if r.returncode != 0:
        tail = " | ".join(l for l in text.splitlines()[-5:] if l.strip())
        raise SystemExit(f"export failed (exit {r.returncode}): {tail[:400]}")

    patterns = dict(PATTERNS)
    files = list(PARQUET_FILES)
    if checkpoints:
        patterns.update({k: re.compile(v) for k, v in CHECKPOINT_COUNTERS.items()})
        files.extend(CHECKPOINT_PARQUET_FILES)

    counters = {}
    for key, pat in patterns.items():
        match = pat.search(text)
        counters[key] = None if match is None else int(match.group(1))

    parquet = {}
    for name in files:
        path = out_dir / f"{name}.parquet"
        if not path.exists():
            raise SystemExit(f"export wrote no {name}.parquet in {out_dir}")
        parquet[name] = {
            "rows": pq.ParquetFile(path).metadata.num_rows,
            "bytes": path.stat().st_size,
            "sha256": sha256_file(path),
        }

    manifest_errors = (reward_opaque_manifest_errors(out_dir, counters, checkpoints)
                       + targeting_manifest_errors(out_dir, counters, checkpoints)
                       + frame_skip_manifest_errors(out_dir, counters, checkpoints))
    if manifest_errors:
        raise SystemExit("; ".join(manifest_errors))

    if checkpoints:
        manifest_errors = checkpoint_manifest_errors(out_dir, counters)
        if manifest_errors:
            raise SystemExit("; ".join(manifest_errors))

    return {"counters": counters, "parquet": parquet}


def diff(baseline: dict, current: dict) -> list[str]:
    """Every way the pinned numbers and the current ones disagree."""
    out = []
    for key in sorted(set(baseline["counters"]) | set(current["counters"])):
        want = baseline["counters"].get(key)
        got = current["counters"].get(key)
        if want != got:
            out.append(f"counter {key}: {got} (baseline {want})")
    for name in sorted(set(baseline["parquet"]) | set(current["parquet"])):
        want = baseline["parquet"].get(name, {})
        got = current["parquet"].get(name, {})
        for field in ("rows", "bytes", "sha256"):
            if want.get(field) != got.get(field):
                out.append(
                    f"{name}.parquet {field}: {got.get(field)} "
                    f"(baseline {want.get(field)})"
                )
    return out


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--baseline", type=Path, required=True)
    ap.add_argument("--exe", type=Path, default=DEFAULT_EXE)
    ap.add_argument("--replay", type=Path, default=None,
                    help="overrides the replay path stored in the baseline")
    ap.add_argument("--out", type=Path, default=None,
                    help="transactionally replaced export directory "
                         "(default: out/export_check)")
    ap.add_argument("--update", action="store_true",
                    help="rewrite the baseline from the current numbers")
    ap.add_argument("--checkpoints", action="store_true",
                    help="run the optional Checkpoint pass and pin its counters "
                         "and checkpoint_fields.parquet too")
    ap.add_argument("--require-input", action="store_true",
                    help="fail instead of skipping when the replay is absent")
    args = ap.parse_args()

    if not args.exe.exists():
        print(f"build the release binary first: {args.exe}", file=sys.stderr)
        return 2

    stored = json.loads(args.baseline.read_text(encoding="utf-8")) \
        if args.baseline.exists() else {}
    replay = args.replay or Path(os.path.expandvars(stored.get("replay", "")))
    # A bare filename in the baseline resolves against VRFKIT_CORPUS_DIR so the
    # repo ships no absolute path; an absolute path (old baselines, --replay) is
    # used as-is. Unset env + filename -> relative -> not found -> SKIP below.
    if replay.name and not replay.is_absolute():
        corpus_dir = os.environ.get("VRFKIT_CORPUS_DIR", "")
        if corpus_dir:
            replay = Path(corpus_dir) / replay
    if not replay.name or not replay.exists():
        if args.require_input or os.environ.get("VRFKIT_REQUIRE_CORPUS"):
            print(f"REQUIRED INPUT MISSING: replay not present ({replay})",
                  file=sys.stderr)
            return 2
        print(f"SKIP: replay not present ({replay})")
        print("      the corpus lives outside this repo; nothing to guard here.")
        return 0

    out_dir = args.out or (REPO / "out" / "export_check")
    current = measure(args.exe, replay, out_dir, checkpoints=args.checkpoints)

    # The cross-check runs whether or not a baseline exists, and before the
    # baseline is written: pinning a summary that already contradicts its own
    # Parquet output would pin the lie.
    lies = cross_checks(current["counters"], current["parquet"])
    if lies:
        print(f"CROSS-CHECK FAILED: {len(lies)} counter(s) disagree with the "
              f"Parquet files they name")
        for line in lies:
            print(f"  {line}")
        return 1

    if args.update:
        refusals = unpinnable(current)
        if refusals:
            print(f"FAILED: refusing to pin a run with {len(refusals)} "
                  f"unmeasured counter(s)")
            for line in refusals:
                print(f"  {line}")
            print("  A None in the baseline is matched by the counter going "
                  "missing again, which is the failure this file exists to "
                  "catch.")
            return 1
        payload = {"replay": stored.get("replay") or str(replay), **current}
        atomic_write_text(args.baseline, json.dumps(payload, indent=1) + "\n")
        print(f"wrote {args.baseline} (NetGUID rows "
              f"{current['counters']['net_guid_rows']})")
        return 0

    if not stored:
        print(f"no baseline at {args.baseline} -- run with --update",
              file=sys.stderr)
        return 2

    problems = diff(stored, current)
    if problems:
        print(f"DRIFT: {len(problems)} difference(s) from {args.baseline.name}")
        for line in problems:
            print(f"  {line}")
        return 1

    c = current["counters"]
    n_identities = len(cross_check_identities(c, current["parquet"]))
    print(f"OK: {replay.name} matches the baseline "
          f"(NetGUID rows {c['net_guid_rows']}, "
          f"blocks {c['content_blocks']}, fields {c['fields']}, "
          f"rpcs {c['rpcs']}, movement {c['movement_rows']}, "
          f"events {c['event_rows']}, "
          f"decode errors {c['overlay_decode_errors']}); "
          f"{n_identities} printed counters cross-check against their Parquet files")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
