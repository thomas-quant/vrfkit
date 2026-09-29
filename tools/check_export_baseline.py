#!/usr/bin/env python3
"""Pin the export path's own numbers and fail when they drift.

`check_corpus_baseline.py` guards the *validate* path; this guards *export*:
the counters its summary prints and the six main Parquet files (the
checkpoint tables too, under `--checkpoints`). A number nobody machine-checks
is not guarded, however often a document quotes it.

Two independent checks run here, and they fail on different things:

  1. CROSS-CHECK.   Some printed counters are identities against the Parquet
     files: `NetGUID rows` is net_guids.parquet's row count, `Movement rows`
     is movement.parquet's, `Actor opens + Actor closes` is actors.parquet's,
     and so on. If the summary and the file disagree, the summary is lying,
     and this fails with no baseline needed; so does a counter the summary
     did not print. The set lives in `cross_check_identities`; do not
     restate its size here, where it cannot be checked.
     (fields.parquet has no such identity: it also carries RPC parameters and
     flattened dynamic-array leaves, so its row count is pinned, not derived.)

  2. BASELINE.      Every counter and every Parquet row count and byte size is
     compared against a pinned JSON. This catches the other failure mode --
     the data moving and the summary faithfully reporting the new, wrong
     number -- which a cross-check alone cannot see. A byte-size difference
     with every counter equal means the row VALUES moved, or the parquet
     crate did: Cargo.lock pins it, so check that before assuming a data bug,
     and do not disable the guard.

With `--checkpoints` a third check, `checkpoint_guid_crosscheck`, holds each
checkpoint GUID path rebuilt by the path-index rule against the main stream's
own declaration; it needs no baseline either.

The .vrf lives outside the repo: a baseline names it by bare filename,
resolved against VRFKIT_CORPUS_DIR. A missing replay is reported and SKIPPED
rather than failed (unless --require-input or VRFKIT_REQUIRE_CORPUS is set):
a guard that fails on someone else's machine gets disabled, and a disabled
guard protects nothing.

Usage:
    python tools/check_export_baseline.py --baseline tools/baselines/export_02d4d478.json
    python tools/check_export_baseline.py --baseline <path> --update
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
from pathlib import Path

import pyarrow as pa
import pyarrow.parquet as pq

if __package__:
    from .atomic_io import atomic_write_text, sha256_file
else:  # direct script execution
    from atomic_io import atomic_write_text, sha256_file

REPO = Path(__file__).resolve().parent.parent
DEFAULT_EXE = REPO / "target" / "release" / "vrfkit.exe"

# Counters the export summary prints, anchored on the exact labels
# crates/vrfkit/src/driver/summary.rs emits. Not every line is pinned: on
# 2026-09-29, 56 of the 120 labelled `eprintln!` literals in summary.rs,
# rendered as test_check_export_baseline.py's SummaryLabelTests renders them,
# matched no pattern here or in CHECKPOINT_COUNTERS. A listed label that
# stops being printed reads as missing (None), never as 0.
COUNTERS = {
    "chunks": r"Chunks:\s+(\d+)",
    "packets": r"Packets:\s+(\d+)",
    "export_groups": r"Export groups:\s+(\d+)",
    "content_blocks": r"Content blocks:\s+(\d+)",
    "rep_layout_blocks": r"RepLayout blocks:\s+(\d+)",
    "class_net_cache_blocks": r"ClassNetCache:\s+(\d+)",
    "fields": r"Fields:\s+(\d+)",
    # Anchored: unanchored, it also read `Truncated RPCs:` whenever that line
    # came first or this one went missing.
    "rpcs": r"(?m)^\s*RPCs:\s+(\d+)\s*$",
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
    # Outside the overlay ratio: the overlay buckets are decided before the
    # effect pass runs, so without this line only fields.parquet's byte count
    # would show the effect decoder ran, and bytes cannot say it made values.
    "effect_blobs_decoded": r"Effect blobs:\s+(\d+)",
    # Additive decoders: when 13.02 moved RoundResults from handle 93 to 81,
    # nothing else on this summary moved and the match score vanished from the
    # Parquet. `failed` is the alarm; `decoded` keeps a decoder that stops
    # running (0 decoded, 0 failed) from reading as a clean one.
    "struct_blobs_decoded": r"Struct blobs:\s+(\d+) decoded",
    "struct_blobs_failed": r"Struct blobs:\s+\d+ decoded / (\d+) failed",
    "targeting_world_locations_decoded": r"(?m)^\s*Target locations:\s+(\d+) array children\s*$",
    # This is a measured opaque shape, not a decode-error counter: the main
    # corpus is expected to contain it. Require its unconditional summary
    # line and reconcile it with the manifest instead of requiring zero.
    "tracked_rewards_opaque_empty_variants": (
        r"(?m)^\s*Reward opaque:\s+(\d+) empty variants\s*$"
    ),
    # The AbilitiesAndBuffs ClassNetCache brute force and the post-RepLayout
    # tails. `CNC RPC rows` counts successes (tail decodes included), so it can
    # only shrink when the fc=34 walk stops fitting; `unwalked` names that
    # failure and `attempted` its denominator. Anchored at the line start:
    # "Checkpoint CNC brute force:" contains "CNC brute force:".
    "cnc_rpcs_emitted": r"(?m)^\s*CNC RPC rows:\s+(\d+)\s*$",
    "cnc_bruteforce_payloads_attempted": (
        r"(?m)^\s*CNC brute force:\s+(\d+) attempted / \d+ unwalked\s*$"
    ),
    "cnc_bruteforce_payloads_unwalked": (
        r"(?m)^\s*CNC brute force:\s+\d+ attempted / (\d+) unwalked\s*$"
    ),
    "rep_layout_cnc_tails_decoded": (
        r"(?m)^\s*RepLayout tails:\s+(\d+) decoded / \d+ preserved\s*$"
    ),
    "rep_layout_cnc_tails_preserved": (
        r"(?m)^\s*RepLayout tails:\s+\d+ decoded / (\d+) preserved\s*$"
    ),
    # Movement sections that stopped with bits of their window unread, in
    # sized and open windows. A measured tally, not a loss verdict: pinned so
    # a change in either direction on the reference replay is seen.
    "movement_sized_section_tails": (
        r"(?m)^\s*Movement tails:\s+(\d+) sized \(\d+ bits\) / \d+ open \(\d+ bits\)\s*$"
    ),
    "movement_sized_section_tail_bits": (
        r"(?m)^\s*Movement tails:\s+\d+ sized \((\d+) bits\) / \d+ open \(\d+ bits\)\s*$"
    ),
    "movement_open_section_tails": (
        r"(?m)^\s*Movement tails:\s+\d+ sized \(\d+ bits\) / (\d+) open \(\d+ bits\)\s*$"
    ),
    "movement_open_section_tail_bits": (
        r"(?m)^\s*Movement tails:\s+\d+ sized \(\d+ bits\) / \d+ open \((\d+) bits\)\s*$"
    ),
    # Every byte-wrapped movement stream and the bits after its envelope,
    # which nothing reads; verify_build_corpus.py fails a replay whose bits are
    # not 24 per stream.
    "movement_envelope_trailers": (
        r"(?m)^\s*Envelope trailers:\s+(\d+) streams / \d+ bits\s*$"
    ),
    "movement_envelope_trailer_bits": (
        r"(?m)^\s*Envelope trailers:\s+\d+ streams / (\d+) bits\s*$"
    ),
    # Empty ActiveBlinds deltas whose trailing zero byte the strict walker
    # was spared: a measured tolerance, legitimately nonzero on some builds.
    "active_blinds_empty_trailers": (
        r"(?m)^\s*ActiveBlinds trailers:\s+(\d+) empty deltas\s*$"
    ),
    # Section bytes the DemoFrame walk stepped over; the skip is length-
    # prefixed, so a build that starts sending ExternalData or
    # GameSpecificFrameData moves nothing else here. Anchored so this and the
    # unanchored `cp_frames` can never read each other's line.
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
    # Frames whose NaN or infinite time was read as 0 ms.
    "frame_non_finite_times": r"(?m)^\s*Frame times:\s+(\d+) non-finite\s*$",
}
PATTERNS = {k: re.compile(v) for k, v in COUNTERS.items()}
#: The frame-walk tallies -- the three skip counts and the frames with a
#: non-finite time -- as the manifest names them after its `frame_` /
#: `checkpoint_frame_` prefixes.
FRAME_SKIP_KEYS = ("external_data_blobs", "external_data_bytes", "game_specific_bytes",
                   "non_finite_times")
#: Sink tallies of bits or bytes nothing reads, published under the same key in
#: the manifest's `sink` blocks as in COUNTERS (checkpoint: `cp_` + key).
SINK_TALLY_KEYS = ("movement_envelope_trailers", "movement_envelope_trailer_bits",
                   "active_blinds_empty_trailers")

# Only printed under `--checkpoints`, so they live apart from COUNTERS -- a
# default run must not record them as None and then diff that against a
# baseline taken with the flag.
CHECKPOINT_COUNTERS = {
    "cp_partial_rows": r"Checkpoint partial raw:\s+(\d+) rows",
    "cp_partial_bits": r"Checkpoint partial raw:\s+\d+ rows / (\d+) bits",
    "cp_chunks": r"Checkpoints:\s+(\d+)",
    # Checkpoint bytes no reader consumed. Anchored: the free-text error lines
    # printed before it (`Struct blob err:`, `Movement err:`) could quote it.
    "cp_trailing_bytes": r"(?m)^\s*Trailing bytes:\s+(\d+)\s*$",
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
    "cp_cnc_rpcs_emitted": r"(?m)^\s*Checkpoint CNC:\s+(\d+) RPC rows\s*$",
    "cp_cnc_bruteforce_payloads_attempted": (
        r"(?m)^\s*Checkpoint CNC brute force:\s+(\d+) attempted / \d+ unwalked\s*$"
    ),
    "cp_cnc_bruteforce_payloads_unwalked": (
        r"(?m)^\s*Checkpoint CNC brute force:\s+\d+ attempted / (\d+) unwalked\s*$"
    ),
    "cp_rep_layout_cnc_tails_decoded": (
        r"(?m)^\s*Checkpoint tails:\s+(\d+) decoded / \d+ preserved\s*$"
    ),
    "cp_rep_layout_cnc_tails_preserved": (
        r"(?m)^\s*Checkpoint tails:\s+\d+ decoded / (\d+) preserved\s*$"
    ),
    "cp_movement_sized_section_tails": (
        r"(?m)^\s*Checkpoint movement tails:\s+(\d+) sized \(\d+ bits\) / \d+ open \(\d+ bits\)\s*$"
    ),
    "cp_movement_sized_section_tail_bits": (
        r"(?m)^\s*Checkpoint movement tails:\s+\d+ sized \((\d+) bits\) / \d+ open \(\d+ bits\)\s*$"
    ),
    "cp_movement_open_section_tails": (
        r"(?m)^\s*Checkpoint movement tails:\s+\d+ sized \(\d+ bits\) / (\d+) open \(\d+ bits\)\s*$"
    ),
    "cp_movement_open_section_tail_bits": (
        r"(?m)^\s*Checkpoint movement tails:\s+\d+ sized \(\d+ bits\) / \d+ open \((\d+) bits\)\s*$"
    ),
    "cp_movement_envelope_trailers": (
        r"(?m)^\s*Checkpoint envelope trailers:\s+(\d+) streams / \d+ bits\s*$"
    ),
    "cp_movement_envelope_trailer_bits": (
        r"(?m)^\s*Checkpoint envelope trailers:\s+\d+ streams / (\d+) bits\s*$"
    ),
    "cp_active_blinds_empty_trailers": (
        r"(?m)^\s*Checkpoint ActiveBlinds trailers:\s+(\d+) empty deltas\s*$"
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
    "cp_frame_non_finite_times": (
        r"(?m)^\s*Checkpoint frame times:\s+(\d+) non-finite\s*$"
    ),
}

PARQUET_FILES = ("fields", "movement", "actors", "net_guids", "events", "partials")
CHECKPOINT_PARQUET_FILES = (
    "checkpoint_fields", "checkpoint_actors", "checkpoint_net_guids", "checkpoint_blocks",
    "checkpoint_guid_entries", "checkpoint_export_groups", "checkpoint_export_fields",
)


def cross_check_identities(counters: dict, parquet: dict) -> list:
    """The printed counters that ARE Parquet row counts, as (label, printed
    value, actual rows); split out so the pass message counts them rather than
    stating a literal."""
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
    A counter the summary did not print fails too: its identity cannot be
    checked."""
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

    `measure` records an unprinted counter as None, never 0; pinned, that None
    would compare equal to a summary that has STOPPED printing the counter.
    The cross-check refuses this for the Parquet row identities
    (`cross_check_identities`); this covers every other counter.
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


#: The columns `checkpoint_guid_crosscheck` reads from each table.
GUID_ENTRY_COLUMNS = ("checkpoint_index", "ordinal", "net_guid", "outer_net_guid",
                      "path_is_string", "literal_path", "name_index")
MAIN_GUID_COLUMNS = ("net_guid", "path", "outer_net_guid")

#: Every count `checkpoint_guid_crosscheck` returns, in print order. All of them
#: are always returned and printed, zeros included: a line that shows a count
#: only when it is non-zero cannot tell "nothing differed" from "nothing ran".
#: For each kind, joined = path_equal + path_differs = outer_equal +
#: outer_value_differs + outer_presence_differs; indexed entries are joined,
#: unjoined or unresolved.
GUID_CROSSCHECK_KEYS = (
    "indexed_joined", "indexed_path_equal", "indexed_path_differs",
    "indexed_unresolved", "indexed_outer_equal", "indexed_outer_value_differs",
    "indexed_outer_presence_differs", "indexed_unjoined",
    "literal_joined", "literal_path_equal", "literal_path_differs",
    "literal_outer_equal", "literal_outer_value_differs",
    "literal_outer_presence_differs", "literal_unjoined",
    "main_duplicate_guids", "malformed_entries", "ordinal_errors",
)

#: Counts that fail the check when non-zero. `indexed_joined == 0` fails too.
#: Unjoined entries do not: a checkpoint may declare a GUID the main stream
#: never exported.
GUID_CROSSCHECK_FAILURES = {
    "indexed_path_differs": "indexed entries resolve to a path the main stream does not declare for that GUID",
    "indexed_unresolved": "indexed entries name a position past the literals that precede them",
    "indexed_outer_value_differs": "indexed entries carry a different outer GUID than the main stream",
    "indexed_outer_presence_differs": "indexed entries disagree with the main stream on whether an outer GUID exists",
    "literal_path_differs": "literal entries carry a different path than the main stream",
    "literal_outer_value_differs": "literal entries carry a different outer GUID than the main stream",
    "literal_outer_presence_differs": "literal entries disagree with the main stream on whether an outer GUID exists",
    "main_duplicate_guids": "net_guids.parquet rows repeat a net_guid, so the join is ambiguous",
}


def format_guid_crosscheck(counts: dict) -> str:
    """One line with every cross-check count, zeros included."""
    return "Checkpoint GUID cross-check: " + ", ".join(
        f"{key.replace('_', ' ')} {counts[key]}" for key in GUID_CROSSCHECK_KEYS)


def _outer_verdict(checkpoint_outer: int, main_outer: int | None) -> str:
    """Compare outers under an explicit rule; never fold null into 0.

    `checkpoint_guid_entries` keeps the wire value, 0 meaning "no outer";
    `net_guids` writes null for "no outer" and never 0, the invalid GUID. So
    checkpoint 0 must meet main null and a non-zero outer the same main value.
    A main 0 is a presence difference, not a match for checkpoint 0, or the
    main table could start writing 0 unnoticed.
    """
    checkpoint_present = checkpoint_outer != 0
    main_present = main_outer is not None
    if checkpoint_present != main_present:
        return "outer_presence_differs"
    if checkpoint_present and checkpoint_outer != main_outer:
        return "outer_value_differs"
    return "outer_equal"


def checkpoint_guid_crosscheck(out_dir: Path) -> tuple[dict, list[str]]:
    """Check checkpoint GUID paths against the main stream's own declarations.

    Returns `(counts, errors)`: every key of `GUID_CROSSCHECK_KEYS`, and one
    message per reason the check fails. An empty error list is a pass.

    A checkpoint GUID entry carries its path as a literal or as an index, and
    `checkpoint_guid_entries.parquet` keeps the raw record, so the path is
    rebuilt here by the reader's rule (docs/CHECKPOINT_PATH_RESOLUTION.md):
    the index is a zero-based position among the literals earlier in the same
    checkpoint, indexed entries do not join that table, and it starts empty
    for every `checkpoint_index`. Grouping is by `checkpoint_index`, never
    `checkpoint_id` (IDs repeat within a replay), after sorting rows by
    `(checkpoint_index, ordinal)`.

    The main stream declares the same server GUIDs through a separate reader
    into `net_guids.parquet`, never through the index rule. Each entry is
    joined to it by `net_guid` and its path and outer compared
    (`_outer_verdict`); literal entries test the premise that a GUID number
    names the same path in both tables. Agreement is evidence for the
    path-index rule, not for actor identity across streams.

    Fails on any path or outer difference, an index past the preceding
    literals, duplicate `net_guid` keys in the main table, malformed rows or
    non-contiguous ordinals, and when no indexed entry joined at all -- a check
    that compared nothing must not read as one that passed.
    """
    counts = dict.fromkeys(GUID_CROSSCHECK_KEYS, 0)
    try:
        entries = pq.read_table(out_dir / "checkpoint_guid_entries.parquet",
                                columns=list(GUID_ENTRY_COLUMNS)).to_pydict()
        main = pq.read_table(out_dir / "net_guids.parquet",
                             columns=list(MAIN_GUID_COLUMNS)).to_pydict()
    except (OSError, ValueError, pa.ArrowException) as exc:
        return counts, [f"checkpoint GUID cross-check cannot read its tables: {exc}"]

    main_rows: dict[int, tuple] = {}
    for guid, path, outer in zip(main["net_guid"], main["path"], main["outer_net_guid"]):
        if guid in main_rows:
            counts["main_duplicate_guids"] += 1
        main_rows[guid] = (path, outer)

    rows = list(zip(*(entries[name] for name in GUID_ENTRY_COLUMNS)))
    well_formed = []
    for row in rows:
        checkpoint, ordinal, guid, outer, is_literal, literal, index = row
        if (None in (checkpoint, ordinal, guid, outer, is_literal)
                or (is_literal and (literal is None or index is not None))
                or (not is_literal and (index is None or literal is not None))):
            counts["malformed_entries"] += 1
        else:
            well_formed.append(row)
    well_formed.sort(key=lambda row: (row[0], row[1]))
    previous = None
    for checkpoint, ordinal, *_ in well_formed:
        expected = previous[1] + 1 if previous and previous[0] == checkpoint else 0
        if ordinal != expected:
            counts["ordinal_errors"] += 1
        previous = (checkpoint, ordinal)
    if counts["malformed_entries"] or counts["ordinal_errors"]:
        return counts, [
            f"checkpoint_guid_entries.parquet is not a complete raw record "
            f"({counts['malformed_entries']} malformed entries, "
            f"{counts['ordinal_errors']} ordinal errors); paths were not compared"]

    # The first example of each difference, for the error message.
    first: dict[str, str] = {}

    def note(key: str, row: tuple, detail: str) -> None:
        if key not in first:
            first[key] = (f"checkpoint_index {row[0]} ordinal {row[1]} "
                          f"net_guid {row[2]}: {detail}")

    literals: list[str] = []
    current = None
    for row in well_formed:
        checkpoint, _, guid, outer, is_literal, literal, index = row
        if checkpoint != current:
            current, literals = checkpoint, []
        if is_literal:
            literals.append(literal)
            kind, path = "literal", literal
        elif index < len(literals):
            kind, path = "indexed", literals[index]
        else:
            counts["indexed_unresolved"] += 1
            note("indexed_unresolved", row, f"index {index}, {len(literals)} preceding literals")
            continue
        if guid not in main_rows:
            counts[f"{kind}_unjoined"] += 1
            continue
        counts[f"{kind}_joined"] += 1
        main_path, main_outer = main_rows[guid]
        if path == main_path:
            counts[f"{kind}_path_equal"] += 1
        else:
            counts[f"{kind}_path_differs"] += 1
            note(f"{kind}_path_differs", row, f"checkpoint {path!r}, main {main_path!r}")
        verdict = _outer_verdict(outer, main_outer)
        counts[f"{kind}_{verdict}"] += 1
        if verdict != "outer_equal":
            note(f"{kind}_{verdict}", row, f"checkpoint outer {outer}, main outer {main_outer}")

    errors = []
    if counts["indexed_joined"] == 0:
        errors.append("no indexed checkpoint GUID entry joined a main-stream GUID, "
                      "so the path-index rule was not compared at all")
    for key, reason in GUID_CROSSCHECK_FAILURES.items():
        if counts[key]:
            example = f" (first: {first[key]})" if key in first else ""
            errors.append(f"checkpoint GUID cross-check: {counts[key]} {reason}{example}")
    return counts, errors


def _manifest_agreement(out_dir: Path, counters: dict, what: str, pick) -> list[str]:
    """The counts `pick` reads from the manifest's `quality` object must be
    nonnegative integers equal to the summary's; `what` names them in every
    message."""
    try:
        quality = json.loads((out_dir / "manifest.json").read_text(encoding="utf-8"))["quality"]
        values = pick(quality)
    except (OSError, ValueError, KeyError, TypeError) as exc:
        return [f"manifest omits {what} quality data: {exc}"]
    if any(type(value) is not int or value < 0 for value in values.values()):
        return [f"{what} counts must be nonnegative integers"]
    return [f"manifest {key}={value} disagrees with summary {counters.get(key)}"
            for key, value in values.items() if counters.get(key) != value]


def _sink_counts(key: str, checkpoints: bool):
    """A `pick` for `quality.sink[key]` and, with checkpoints, its `cp_` twin."""
    def pick(quality):
        values = {key: quality["sink"][key]}
        if checkpoints:
            values["cp_" + key] = quality["checkpoints"]["sink"][key]
        return values
    return pick


def reward_opaque_manifest_errors(
    out_dir: Path, counters: dict, checkpoints: bool,
) -> list[str]:
    """The measured reward count must agree between CLI and manifest. Not a
    zero gate: it counts a known opaque payload variant, legitimately nonzero."""
    return _manifest_agreement(out_dir, counters, "tracked rewards opaque-empty", _sink_counts(
        "tracked_rewards_opaque_empty_variants", checkpoints))


def targeting_manifest_errors(out_dir: Path, counters: dict, checkpoints: bool) -> list[str]:
    """Require the additive targeting count even when it is zero."""
    return _manifest_agreement(out_dir, counters, "targeting world-location", _sink_counts(
        "targeting_world_locations_decoded", checkpoints))


def sink_tally_manifest_errors(out_dir: Path, counters: dict, checkpoints: bool) -> list[str]:
    """The SINK_TALLY_KEYS counts must agree between CLI and manifest, zeros
    included. Not a zero gate: each counts bits a decoder left unread on every
    replay (envelope trailers) or on some (ActiveBlinds trailers)."""
    def pick(quality):
        values = {key: quality["sink"][key] for key in SINK_TALLY_KEYS}
        if checkpoints:
            values.update({"cp_" + key: quality["checkpoints"]["sink"][key]
                           for key in SINK_TALLY_KEYS})
        return values
    return _manifest_agreement(out_dir, counters, "sink unread-bits tally", pick)


def frame_skip_manifest_errors(out_dir: Path, counters: dict, checkpoints: bool) -> list[str]:
    """The frame-walk tallies must agree between CLI and manifest, zeros included.

    Not a zero gate: these sections are skipped by design, so a non-zero
    count is data left undecoded, not a failure. It cannot see a pass that
    stops absorbing its frame walk: summary and manifest read one variable, so
    both would say 0, the value 02d4d478's baselines pin.
    crates/vrfkit/tests/frame_skips.rs guards that wiring on a replay carrying
    both sections.
    """
    def pick(quality):
        values = {f"frame_{key}": quality[f"frame_{key}"] for key in FRAME_SKIP_KEYS}
        if checkpoints:
            values.update({f"cp_frame_{key}": quality["checkpoints"][f"checkpoint_frame_{key}"]
                           for key in FRAME_SKIP_KEYS})
        return values
    return _manifest_agreement(out_dir, counters, "frame-skip", pick)


def checkpoint_trailing_manifest_errors(out_dir: Path, counters: dict,
                                        checkpoints: bool) -> list[str]:
    """`Trailing bytes:` must agree with the manifest's
    `checkpoints.checkpoint_trailing_bytes`, zeros included. Not a zero gate
    here: verify_build_corpus.py is one, and the baseline pins the value.

    As with the frame skips, summary and manifest read one field, so a chunk
    whose count never reaches `CheckpointStats` reads 0 in both;
    crates/vrfkit/tests/checkpoint_unread.rs guards that wiring.
    """
    if not checkpoints:
        return []
    return _manifest_agreement(out_dir, counters, "checkpoint trailing-bytes", lambda quality: {
        "cp_trailing_bytes": quality["checkpoints"]["checkpoint_trailing_bytes"]})


def measure(exe: Path, replay: Path, out_dir: Path, checkpoints: bool = False) -> dict:
    """Export one replay and collect the summary counters and Parquet shape.

    The exporter replaces the output directory transactionally, so it is not
    deleted here first: a failed export must leave the previous complete
    result. `checkpoints` runs the optional Checkpoint pass and pins its
    counters and tables too, so the off-by-default path has a baseline.
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
                       + sink_tally_manifest_errors(out_dir, counters, checkpoints)
                       + frame_skip_manifest_errors(out_dir, counters, checkpoints)
                       + checkpoint_trailing_manifest_errors(out_dir, counters, checkpoints))
    if manifest_errors:
        raise SystemExit("; ".join(manifest_errors))

    if checkpoints:
        manifest_errors = checkpoint_manifest_errors(out_dir, counters)
        if manifest_errors:
            raise SystemExit("; ".join(manifest_errors))
        # Printed, never pinned: adding these to `counters` would make every
        # existing checkpoint baseline report them as drift from None.
        guid_counts, guid_errors = checkpoint_guid_crosscheck(out_dir)
        print(format_guid_crosscheck(guid_counts))
        if guid_errors:
            raise SystemExit("; ".join(guid_errors))

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
                    help="also run the optional Checkpoint pass: pin its summary "
                         "counters and every checkpoint_*.parquet table, require "
                         "its manifest block (no dropped actor rows, GUID path "
                         "counts that add up and match the summary), and "
                         "cross-check its GUID paths against net_guids.parquet")
    ap.add_argument("--require-input", action="store_true",
                    help="fail instead of skipping when the replay is absent")
    args = ap.parse_args()

    if not args.exe.exists():
        print(f"build the release binary first: {args.exe}", file=sys.stderr)
        return 2

    stored = json.loads(args.baseline.read_text(encoding="utf-8")) \
        if args.baseline.exists() else {}
    # A new baseline pins --replay as given, never the path resolved below:
    # a path would put one machine's directory into a committed file.
    if args.update and not stored.get("replay") and args.replay is not None \
            and args.replay.anchor:
        print(f"FAILED: --update would write the path {args.replay} into "
              f"{args.baseline.name}; a baseline names its replay by bare filename. "
              f"Set VRFKIT_CORPUS_DIR to its directory and pass --replay "
              f"{args.replay.name}.", file=sys.stderr)
        return 2
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
        payload = {"replay": stored.get("replay") or str(args.replay), **current}
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
