"""Assert that the type overlay and the struct-blob decoders decode cleanly
across a whole corpus.

`vrfkit validate` does not print the overlay counters -- only `export` does --
so validate_corpus.py cannot see a decode error, and a wrong overlay type moves
no counter it reads: the row still emits, the block/field/RPC totals stay put.
The overlay is strict both ways (Err(BitIo) past the end, Err(NotFullyConsumed)
on leftover bits), so `Decode errors: 0` over a corpus is a statement about
every (group, field) type in the table. Only a corpus catches a type whose two
readings consume the same bits on the reference replay: a rotator
quantization, for instance, shows only where some payload sets the flag.

The struct-blob decoders (RoundResults, TeamEconomy, RoundInfos) are additive,
so a total failure moves nothing else on the summary: build 13.02 moved
RoundResults from handle 93 to 81 and every other counter stayed clean while
the match score stopped being written.

Each replay exports into a temporary directory deleted once its counters are
read, so peak disk is jobs x one replay's output. Discovery is corpus_scan.py's
(non-recursive by default -- see its docstring); `--recursive` walks
subdirectories too.

Usage:
    python tools/check_decode_errors_corpus.py <vrfkit.exe> <corpus dir>
    python tools/check_decode_errors_corpus.py <vrfkit.exe> <corpus dir> --jobs 8
    python tools/check_decode_errors_corpus.py <vrfkit.exe> <corpus dir> --recursive
    python tools/check_decode_errors_corpus.py <vrfkit.exe> <corpus dir> --checkpoints

`--checkpoints` (opt-in: real extra time and disk) passes the flag on to
`vrfkit export` and gates the `=== Checkpoints ===` block the same way:
every `CHECKPOINT_COUNTERS` line present, every `CHECKPOINT_FAILURES` counter
zero. `conflicts` is required and printed but never gated: a conflict is the
overlay refusing to type a row whose handle the replay renamed, the protection
working, so gating it would fail real data.

Exit 0 only when every replay printed every `COUNTERS` line with every
`FAILURES` counter at zero, and the corpus moved every `MUST_MOVE` work
counter. A counter that stops being printed must not read as zero
(docs/archive/PROJECT_STATUS.md 5-O), and one that cannot move must not read
as success (`dead_counters`). `FAILURES` are verify_build_corpus.py's
SINK_ZERO read off the summary, pinned by test_check_decode_errors_corpus.py.
As there, only counts are gated: CNC `attempted` and the movement tails' bit
totals are printed, not failed on, and `RPC suffix bits` stays ungated because
verify_build_corpus.py records `rpc_suffix_bits_dropped` as a limitation.

Every failure gate is either backed in `MUST_MOVE` / `CHECKPOINT_MUST_MOVE` by
the work counter whose movement makes its zero evidence, or listed in
`UNBACKED` / `CHECKPOINT_UNBACKED` with the reason none exists and printed as
unbacked on every run; the test pins that every gate is one or the other.

A nonzero export exit makes the replay unreadable, not clean: `vrfkit export`
prints this summary before it finalises the Parquet files (`read_counters`).
"""
from __future__ import annotations

import argparse
import re
import shutil
import subprocess
import sys
import tempfile
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import corpus_scan

DECODE_ERRORS = re.compile(r"Decode errors:\s+(\d+)")
DECODED_OK = re.compile(r"Decoded OK:\s+(\d+)")
NOT_IN_TABLE = re.compile(r"Not in table:\s+(\d+)")
RAW_SKIP = re.compile(r"Raw/Skip:\s+(\d+)")
NO_FIELD_NAME = re.compile(r"No field name:\s+(\d+)")
ROWS_OFFERED = re.compile(r"Rows offered:\s+(\d+)")
STRUCT_DECODED = re.compile(r"Struct blobs:\s+(\d+) decoded")
STRUCT_FAILED = re.compile(r"Struct blobs:\s+\d+ decoded / (\d+) failed")
REWARD_OPAQUE = re.compile(r"(?m)^\s*Reward opaque:\s+(\d+) empty variants\s*$")


def _line_field(label: str, units: tuple[str, ...], index: int) -> re.Pattern[str]:
    """A regex reading field `index` of one of summary.rs's sink lines.

    `units` are the words after each `{}` of the Rust format string; `()`
    means a single bare number. Anchored on the whole line: `re.search` takes
    the first match anywhere, and free-text diagnostics (`Struct blob err:`,
    `Movement err:`) can quote a label -- `Array leaf errs: 0` inside a message
    ahead of the real `Array leaf errs: 7` would read as clean.
    """
    if not units:
        body = r"(\d+)"
    else:
        body = " / ".join(
            (r"(\d+) " if i == index else r"\d+ ") + re.escape(unit)
            for i, unit in enumerate(units))
    return re.compile(rf"(?m)^\s*{re.escape(label)}\s+{body}\s*$")


ARRAY_DECODE_UNITS = ("elements", "fields", "errors", "truncations")
ARRAY_RESIDUAL_UNITS = ("root bits", "nested bits", "implicit ends")
MOVEMENT_ROWS = _line_field("Movement rows:", (), 0)
MOVEMENT_ERRORS = _line_field("Movement errors:", (), 0)
ARRAY_ELEMENTS = _line_field("Array decode:", ARRAY_DECODE_UNITS, 0)
ARRAY_FIELDS = _line_field("Array decode:", ARRAY_DECODE_UNITS, 1)
ARRAY_ERRORS = _line_field("Array decode:", ARRAY_DECODE_UNITS, 2)
ARRAY_TRUNCATIONS = _line_field("Array decode:", ARRAY_DECODE_UNITS, 3)
ARRAY_ROOT_BITS = _line_field("Array residual:", ARRAY_RESIDUAL_UNITS, 0)
ARRAY_NESTED_BITS = _line_field("Array residual:", ARRAY_RESIDUAL_UNITS, 1)
ARRAY_IMPLICIT_ENDS = _line_field("Array residual:", ARRAY_RESIDUAL_UNITS, 2)
ARRAY_LEAF_ERRORS = _line_field("Array leaf errs:", (), 0)
TRUNCATED_RPCS = _line_field("Truncated RPCs:", (), 0)
CNC_BRUTEFORCE_UNITS = ("attempted", "unwalked")
CNC_ATTEMPTED = _line_field("CNC brute force:", CNC_BRUTEFORCE_UNITS, 0)
CNC_UNWALKED = _line_field("CNC brute force:", CNC_BRUTEFORCE_UNITS, 1)


def _movement_tails_field(label: str, index: int) -> re.Pattern[str]:
    """Field `index` of summary.rs's `{} sized ({} bits) / {} open ({} bits)`,
    which `_line_field` cannot spell; anchored the same way, so the
    `Checkpoint movement tails:` twin never matches."""
    fields = [r"\d+"] * 4
    fields[index] = r"(\d+)"
    return re.compile(
        rf"(?m)^\s*{re.escape(label)}\s+{fields[0]} sized \({fields[1]} bits\) / "
        rf"{fields[2]} open \({fields[3]} bits\)\s*$")


MOVEMENT_SIZED_TAILS = _movement_tails_field("Movement tails:", 0)
MOVEMENT_SIZED_TAIL_BITS = _movement_tails_field("Movement tails:", 1)
MOVEMENT_OPEN_TAILS = _movement_tails_field("Movement tails:", 2)
MOVEMENT_OPEN_TAIL_BITS = _movement_tails_field("Movement tails:", 3)


#: `(key, regex, label)` for every counter read off the export summary. All
#: are required: a summary missing one makes the replay unreadable, and `label`
#: names the missing line. `no_field_name` completes summary.rs's `Rows offered
#: = decoded_ok + decoded_err + raw_or_skip + not_in_table + no_field_name`,
#: which `reconcile` checks with real numbers, never defaults. Work counters
#: are required beside the error counters because an error zero is evidence
#: only when the work happened (`MUST_MOVE`). The sink lines after
#: `tracked_rewards_opaque_empty_variants` are read whole, every field named;
#: test_check_decode_errors_corpus.py checks both against summary.rs.
COUNTERS = (
    ("decode_errors", DECODE_ERRORS, "Decode errors"),
    ("decoded_ok", DECODED_OK, "Decoded OK"),
    ("raw_skip", RAW_SKIP, "Raw/Skip"),
    ("not_in_table", NOT_IN_TABLE, "Not in table"),
    ("no_field_name", NO_FIELD_NAME, "No field name"),
    ("rows_offered", ROWS_OFFERED, "Rows offered"),
    ("struct_blobs_decoded", STRUCT_DECODED, "Struct blobs ... decoded"),
    ("struct_blobs_failed", STRUCT_FAILED, "Struct blobs ... failed"),
    ("tracked_rewards_opaque_empty_variants", REWARD_OPAQUE, "Reward opaque"),
    ("movement_rows", MOVEMENT_ROWS, "Movement rows"),
    ("movement_errors", MOVEMENT_ERRORS, "Movement errors"),
    ("array_elements", ARRAY_ELEMENTS, "Array decode ... elements"),
    ("array_fields", ARRAY_FIELDS, "Array decode ... fields"),
    ("array_errors", ARRAY_ERRORS, "Array decode ... errors"),
    ("array_truncations", ARRAY_TRUNCATIONS, "Array decode ... truncations"),
    ("array_root_bits", ARRAY_ROOT_BITS, "Array residual ... root bits"),
    ("array_nested_bits", ARRAY_NESTED_BITS, "Array residual ... nested bits"),
    ("array_implicit_ends", ARRAY_IMPLICIT_ENDS, "Array residual ... implicit ends"),
    ("array_leaf_errors", ARRAY_LEAF_ERRORS, "Array leaf errs"),
    ("truncated_rpcs", TRUNCATED_RPCS, "Truncated RPCs"),
    ("cnc_bruteforce_attempted", CNC_ATTEMPTED, "CNC brute force ... attempted"),
    ("cnc_bruteforce_unwalked", CNC_UNWALKED, "CNC brute force ... unwalked"),
    ("movement_sized_tails", MOVEMENT_SIZED_TAILS, "Movement tails ... sized"),
    ("movement_sized_tail_bits", MOVEMENT_SIZED_TAIL_BITS, "Movement tails ... sized bits"),
    ("movement_open_tails", MOVEMENT_OPEN_TAILS, "Movement tails ... open"),
    ("movement_open_tail_bits", MOVEMENT_OPEN_TAIL_BITS, "Movement tails ... open bits"),
)

#: Main-pass counters that must be zero on every replay, with the label a
#: failure names: the thirteen quantities verify_build_corpus.py's SINK_ZERO
#: requires to be zero, read off the summary rather than the manifest (the
#: test pins the correspondence).
FAILURES = (
    ("decode_errors", "Decode errors"),
    ("struct_blobs_failed", "Struct blobs failed"),
    ("movement_errors", "Movement errors"),
    ("array_errors", "Array decode errors"),
    ("array_truncations", "Array decode truncations"),
    ("array_root_bits", "Array residual root bits"),
    ("array_nested_bits", "Array residual nested bits"),
    ("array_implicit_ends", "Array residual implicit ends"),
    ("array_leaf_errors", "Array leaf errs"),
    ("truncated_rpcs", "Truncated RPCs"),
    ("cnc_bruteforce_unwalked", "CNC brute force unwalked"),
    ("movement_sized_tails", "Movement tails sized"),
    ("movement_open_tails", "Movement tails open"),
)

#: `(work counter, label, FAILURES it backs)`: corpus totals that cannot
#: legitimately stay at zero. A zero means the decoder never ran, and every
#: gate it backs then reads 0 vacuously. The array walker, leaf decoders and
#: movement decoder are additive like the struct blobs: `ExportSink::on_field`
#: runs the array flattener before the parent row is pushed, so if
#: `is_known_array_field` stopped matching, `Decoded OK`, the blobs and
#: `reconcile` would not move. Every leaf-error path follows a
#: `decode_struct_array*` call, so `fields` backs the leaf gate;
#: `decode_movement_rpc` is the only producer of movement rows.
#:
#: Measured 2026-09-28 over the 1,018 `export.log`s of the 259ed10 build audit
#: (`vrfkit export --checkpoints`, 24 builds 11.06-13.06), read with this
#: module's regexes: minimum per replay 4,654 decoded rows, 16 struct blobs,
#: 32 array elements, 182 array fields and 3,001 movement rows, pre-13.01
#: builds included -- so a one-replay corpus passes. A corpus total cannot
#: catch a walker stopped on one build, or on one route while others keep the
#: total up (`enable_measured_array_routes` branch-gates the measured routes);
#: summary.rs prints no per-route counter.
MUST_MOVE = (
    ("decoded_ok", "Decoded OK", ("decode_errors",)),
    ("struct_blobs_decoded", "Struct blobs ... decoded", ("struct_blobs_failed",)),
    ("array_elements", "Array decode ... elements",
     ("array_errors", "array_truncations", "array_root_bits",
      "array_nested_bits", "array_implicit_ends")),
    ("array_fields", "Array decode ... fields", ("array_leaf_errors",)),
    ("movement_rows", "Movement rows", ("movement_errors", "movement_open_tails")),
)

#: `(failure counter, why no work counter backs it)`, printed on every run so
#: the OK line does not vouch for them.
#:
#: `truncated_rpcs`: summary.rs prints no count of FunctionParameters walks
#: begun. `RPCs:` and `Sink tally: ... RPCs` cannot stand in: they also count
#: post-RepLayout ClassNetCache tails, which never enter the walk (at least 6
#: on every one of the 1,018 audit replays), and RPCs whose function name did
#: not resolve (`try_parse_rpc_params` returns first, the raw fallback row is
#: written, the tally still moves). Backing it needs a Rust-side walk counter.
#:
#: `movement_sized_tails` counts only in a window `movementBitCount` sized
#: (`parse_movement_with_bit_count`, vrf-movement's rpc.rs): `vrfkit validate`,
#: instrumented to count windows, found none in 156,407,150 sections of 80
#: replays covering 11.06-13.06 (2026-09-28). `Movement rows` moves in open
#: windows only, so it backs the open-tail gate alone.
UNBACKED = (
    ("truncated_rpcs", "summary.rs prints no count of RPC parameter walks"),
    ("cnc_bruteforce_unwalked",
     "`CNC brute force: N attempted` is legitimately 0 on some builds -- the "
     "12.10 and 12.11 public fixtures -- so it cannot be a must-move counter"),
    ("movement_sized_tails",
     "only a sized movement window can count one, and no measured replay has one"),
)

# Checkpoint counters, parsed only under --checkpoints. summary.rs's
# print_checkpoints packs several counters per line, so each pattern carries
# several groups and CHECKPOINT_COUNTERS names which is which.
# test_check_decode_errors_corpus.py reads the Overlay format string out of
# summary.rs, so a field added there breaks the test, not this regex.
CHECKPOINT_OVERLAY = re.compile(
    r"Overlay:\s+(\d+) decoded / (\d+) errors / (\d+) raw-skip / "
    r"(\d+) not-in-table / (\d+) unnamed / (\d+) conflicts / "
    r"(\d+) effect blobs")
CHECKPOINT_BLOBS = re.compile(r"Checkpoint blobs:\s+(\d+) decoded / (\d+) failed")
CHECKPOINT_FAILS = re.compile(
    r"Checkpoint fails:\s+(\d+) array / (\d+) truncated RPC / (\d+) movement")
CHECKPOINT_REWARD_OPAQUE = re.compile(
    r"(?m)^\s*Checkpoint reward opaque:\s+(\d+) empty variants\s*$")
# `Checkpoint fails: N array` is the array walker's `errors` only; its
# truncations, residual bits and implicit ends are on this line, leaf decode
# errors on the next.
CHECKPOINT_ARRAY = re.compile(
    r"(?m)^\s*Checkpoint array:\s+(\d+) elements / (\d+) fields / "
    r"(\d+) truncations / (\d+) root bits / (\d+) nested bits / "
    r"(\d+) implicit ends\s*$")
CHECKPOINT_LEAF = re.compile(
    r"(?m)^\s*Checkpoint leaf:\s+(\d+) typed decode errors\s*$")
# The checkpoint twins of `CNC brute force` and `Movement tails`. Anchored:
# `Checkpoint CNC:   N RPC rows` shares the `Checkpoint CNC` prefix.
CHECKPOINT_CNC_BRUTEFORCE = re.compile(
    r"(?m)^\s*Checkpoint CNC brute force:\s+(\d+) attempted / (\d+) unwalked\s*$")
CHECKPOINT_MOVEMENT_TAILS = re.compile(
    r"(?m)^\s*Checkpoint movement tails:\s+(\d+) sized \((\d+) bits\) / "
    r"(\d+) open \((\d+) bits\)\s*$")

#: `(key, regex, group, label)`, all required as `COUNTERS` are: driver/mod.rs
#: (`with_checkpoints.then_some(&cp_stats)`) prints the whole checkpoint
#: block, zeros included, on every `--checkpoints` export even without
#: checkpoint chunks, so a missing line means the checkpoint pass cannot be
#: trusted. Read only for `require_checkpoints=True`.
CHECKPOINT_COUNTERS = (
    ("checkpoint_decoded", CHECKPOINT_OVERLAY, 1, "Overlay ... decoded (checkpoint)"),
    ("checkpoint_errors", CHECKPOINT_OVERLAY, 2, "Overlay ... errors (checkpoint)"),
    ("checkpoint_raw_skip", CHECKPOINT_OVERLAY, 3, "Overlay ... raw-skip (checkpoint)"),
    ("checkpoint_not_in_table", CHECKPOINT_OVERLAY, 4,
     "Overlay ... not-in-table (checkpoint)"),
    ("checkpoint_unnamed", CHECKPOINT_OVERLAY, 5, "Overlay ... unnamed (checkpoint)"),
    ("checkpoint_conflicts", CHECKPOINT_OVERLAY, 6, "Overlay ... conflicts (checkpoint)"),
    ("checkpoint_effect_blobs", CHECKPOINT_OVERLAY, 7,
     "Overlay ... effect blobs (checkpoint)"),
    ("checkpoint_blobs_decoded", CHECKPOINT_BLOBS, 1, "Checkpoint blobs ... decoded"),
    ("checkpoint_blobs_failed", CHECKPOINT_BLOBS, 2, "Checkpoint blobs ... failed"),
    ("checkpoint_fail_array", CHECKPOINT_FAILS, 1, "Checkpoint fails ... array"),
    ("checkpoint_fail_truncated_rpc", CHECKPOINT_FAILS, 2,
     "Checkpoint fails ... truncated RPC"),
    ("checkpoint_fail_movement", CHECKPOINT_FAILS, 3, "Checkpoint fails ... movement"),
    ("checkpoint_tracked_rewards_opaque_empty_variants", CHECKPOINT_REWARD_OPAQUE, 1,
     "Checkpoint reward opaque"),
    ("checkpoint_array_elements", CHECKPOINT_ARRAY, 1, "Checkpoint array ... elements"),
    ("checkpoint_array_fields", CHECKPOINT_ARRAY, 2, "Checkpoint array ... fields"),
    ("checkpoint_array_truncations", CHECKPOINT_ARRAY, 3,
     "Checkpoint array ... truncations"),
    ("checkpoint_array_root_bits", CHECKPOINT_ARRAY, 4, "Checkpoint array ... root bits"),
    ("checkpoint_array_nested_bits", CHECKPOINT_ARRAY, 5,
     "Checkpoint array ... nested bits"),
    ("checkpoint_array_implicit_ends", CHECKPOINT_ARRAY, 6,
     "Checkpoint array ... implicit ends"),
    ("checkpoint_leaf_errors", CHECKPOINT_LEAF, 1,
     "Checkpoint leaf ... typed decode errors"),
    ("checkpoint_cnc_bruteforce_attempted", CHECKPOINT_CNC_BRUTEFORCE, 1,
     "Checkpoint CNC brute force ... attempted"),
    ("checkpoint_cnc_bruteforce_unwalked", CHECKPOINT_CNC_BRUTEFORCE, 2,
     "Checkpoint CNC brute force ... unwalked"),
    ("checkpoint_movement_sized_tails", CHECKPOINT_MOVEMENT_TAILS, 1,
     "Checkpoint movement tails ... sized"),
    ("checkpoint_movement_sized_tail_bits", CHECKPOINT_MOVEMENT_TAILS, 2,
     "Checkpoint movement tails ... sized bits"),
    ("checkpoint_movement_open_tails", CHECKPOINT_MOVEMENT_TAILS, 3,
     "Checkpoint movement tails ... open"),
    ("checkpoint_movement_open_tail_bits", CHECKPOINT_MOVEMENT_TAILS, 4,
     "Checkpoint movement tails ... open bits"),
)

#: `FAILURES` for the checkpoint pass: the same thirteen quantities, read off the
#: `=== Checkpoints ===` block. Consulted only under --checkpoints.
CHECKPOINT_FAILURES = (
    ("checkpoint_errors", "Checkpoint overlay errors"),
    ("checkpoint_blobs_failed", "Checkpoint blobs failed"),
    ("checkpoint_fail_movement", "Checkpoint fails movement"),
    ("checkpoint_fail_array", "Checkpoint fails array"),
    ("checkpoint_array_truncations", "Checkpoint array truncations"),
    ("checkpoint_array_root_bits", "Checkpoint array root bits"),
    ("checkpoint_array_nested_bits", "Checkpoint array nested bits"),
    ("checkpoint_array_implicit_ends", "Checkpoint array implicit ends"),
    ("checkpoint_leaf_errors", "Checkpoint leaf errors"),
    ("checkpoint_fail_truncated_rpc", "Checkpoint fails truncated RPC"),
    ("checkpoint_cnc_bruteforce_unwalked", "Checkpoint CNC brute force unwalked"),
    ("checkpoint_movement_sized_tails", "Checkpoint movement tails sized"),
    ("checkpoint_movement_open_tails", "Checkpoint movement tails open"),
)

FAILURE_LABELS = dict(FAILURES + CHECKPOINT_FAILURES)

#: `MUST_MOVE` for the checkpoint pass: the checkpoint blob decoders and array
#: walker are additive too (13.02's RoundResults case one level down).
#: Measured on the same 1,018 audit logs (at least 5 checkpoints per replay):
#: minimum per replay 2,054 decoded fields, 16 blobs, 30 array elements and
#: 255 array fields, no replay at zero.
CHECKPOINT_MUST_MOVE = (
    ("checkpoint_decoded", "Overlay ... decoded (checkpoint)", ("checkpoint_errors",)),
    ("checkpoint_blobs_decoded", "Checkpoint blobs ... decoded",
     ("checkpoint_blobs_failed",)),
    ("checkpoint_array_elements", "Checkpoint array ... elements",
     ("checkpoint_fail_array", "checkpoint_array_truncations",
      "checkpoint_array_root_bits", "checkpoint_array_nested_bits",
      "checkpoint_array_implicit_ends")),
    ("checkpoint_array_fields", "Checkpoint array ... fields",
     ("checkpoint_leaf_errors",)),
)

#: `UNBACKED` for the checkpoint pass. These gates sit behind `on_rpc` (the
#: movement decoder and the parameter walk), which no checkpoint RPC reached on
#: the 1,018 audit replays: `Checkpoint tails: N decoded` equalled `Checkpoint
#: sink: ... N RPCs` on 1,018/1,018 (181,108 in all, every one a post-RepLayout
#: ClassNetCache tail) and `Dropped: ... N movement rows (checkpoint snapshot)`
#: read 0 on all. A must-move entry would fail every healthy corpus, and the
#: checkpoint RPC count would vouch for a walk that never ran.
CHECKPOINT_UNBACKED = (
    ("checkpoint_fail_movement",
     "no checkpoint RPC reaches the movement decoder"),
    ("checkpoint_fail_truncated_rpc",
     "no checkpoint RPC reaches the parameter walk"),
    ("checkpoint_movement_sized_tails",
     "no checkpoint RPC reaches the movement decoder"),
    ("checkpoint_movement_open_tails",
     "no checkpoint RPC reaches the movement decoder"),
    ("checkpoint_cnc_bruteforce_unwalked",
     "no checkpoint RPC reaches the fc=34 brute-force walk (attempted is 0)"),
)


def read_counters(
    text: str, returncode: int, require_checkpoints: bool = False,
) -> tuple[dict[str, int] | None, str]:
    """`(counters, error)` for one export's output; `counters` is None on failure.

    A nonzero exit fails even when the summary parsed: the exporter prints
    these counters before it finalises the Parquet files. Pass
    `require_checkpoints` only when the export ran with `--checkpoints`; a run
    without it has no `=== Checkpoints ===` block.
    """
    tail = " | ".join(l for l in text.splitlines()[-3:] if l.strip())
    if returncode != 0:
        return None, f"exit {returncode}: {tail[:200]}"
    table = [(key, pattern, 1, label) for key, pattern, label in COUNTERS]
    if require_checkpoints:
        table += CHECKPOINT_COUNTERS
    counters: dict[str, int] = {}
    for key, pattern, group, label in table:
        m = pattern.search(text)
        if m is None:
            return None, f"no {label} counter: {tail[:200]}"
        counters[key] = int(m.group(group))
    return counters, ""


def replay_failures(counters: dict[str, int], checkpoints: bool) -> list[tuple[str, int]]:
    """`(key, count)` for every failure counter that is nonzero on one replay.

    Indexed, never `.get(key, 0)`: a missing counter must raise, not gate as a
    zero (`read_counters` requires every one).
    """
    gated = FAILURES + (CHECKPOINT_FAILURES if checkpoints else ())
    return [(key, counters[key]) for key, _label in gated if counters[key]]


def _vacuous(gates: tuple[str, ...]) -> str:
    """The failure labels a dead work counter leaves without evidence."""
    return ", ".join(FAILURE_LABELS[key] for key in gates)


def dead_counters(totals: dict[str, int], must_move=MUST_MOVE, suffix: str = "") -> list[str]:
    """Corpus totals that never moved, as human-readable failures.

    `Decode errors: 0` says something only if something was decoded; a corpus
    on which nothing ran must not report a clean sweep. The checkpoint pass
    passes `CHECKPOINT_MUST_MOVE` and the suffix ` (with --checkpoints)`.
    """
    return [f"{label} totalled 0 across the corpus{suffix}: nothing decoded, so "
            f"the zero in {_vacuous(gates)} says nothing"
            for key, label, gates in must_move if not totals.get(key)]


def unbacked_line(unbacked: tuple[tuple[str, str], ...]) -> str:
    """The failure gates no work counter backs, and why, on one line. Printed
    on every run: the OK line counts these gates among the zeros, and nothing
    else says their zero is not evidence."""
    return "; ".join(f"{FAILURE_LABELS[key]} ({why})" for key, why in unbacked)


def backing_summary(must_move, unbacked, totals: dict[str, int]) -> str:
    """How many of a pass's zero failure counters are evidence, and of what.
    Built from the tables, so the OK line cannot omit a work counter or count
    an unbacked gate as backed."""
    backed = sum(len(gates) for _key, _label, gates in must_move)
    work = ", ".join(f"{label} {totals[key]:,}" for key, label, _gates in must_move)
    none = ", ".join(FAILURE_LABELS[key] for key, _why in unbacked)
    return (f"{backed} backed by work that moved ({work}), {len(unbacked)} "
            f"with no work counter ({none})")


def reconcile(totals: dict[str, int]) -> str | None:
    """None if the five overlay categories sum to `rows_offered`; else why not.

    summary.rs defines `Rows offered = decoded_ok + decoded_err + raw_or_skip
    + not_in_table + no_field_name`. Every term is indexed, never `.get(key,
    0)`: a default would let an absent counter reconcile silently.
    `no_field_name` is not in `MUST_MOVE`: a corpus where every handle resolves
    to a name is legitimate, so gating it would fail a clean corpus.
    """
    reconciled = (totals["decoded_ok"] + totals["decode_errors"]
                 + totals["raw_skip"] + totals["not_in_table"]
                 + totals["no_field_name"])
    offered = totals["rows_offered"]
    if reconciled == offered:
        return None
    return (f"decoded OK + decode errors + raw/skip + not in table + no "
            f"field name = {reconciled:,}, but rows offered = {offered:,} "
            f"({reconciled - offered:+,}) -- summary.rs's five categories no "
            f"longer sum to its own total")


def export_command(exe: Path, replay: Path, out: Path, with_checkpoints: bool) -> list[str]:
    """The `vrfkit export` argv, split out so the `--checkpoints` wiring is
    testable without a subprocess."""
    cmd = [str(exe), "export", str(replay), "--out", str(out)]
    if with_checkpoints:
        cmd.append("--checkpoints")
    return cmd


def _export_one(
    exe: Path, replay: Path, with_checkpoints: bool = False,
) -> tuple[dict[str, int] | None, str]:
    """Export one replay to a scratch dir and return its overlay counters."""
    out = Path(tempfile.mkdtemp(prefix="vrfkit-decode-"))
    try:
        r = subprocess.run(
            export_command(exe, replay, out, with_checkpoints),
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
            timeout=600,
        )
        return read_counters((r.stdout or "") + (r.stderr or ""),
                             r.returncode, require_checkpoints=with_checkpoints)
    except subprocess.TimeoutExpired:
        return None, "timeout"
    finally:
        shutil.rmtree(out, ignore_errors=True)


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    ap = argparse.ArgumentParser()
    ap.add_argument("exe", type=Path)
    ap.add_argument("corpus", type=Path)
    ap.add_argument("--jobs", type=int, default=8)
    ap.add_argument("--limit", type=int, default=0, help="only the first N replays")
    ap.add_argument("--recursive", action="store_true",
                    help="also walk subdirectories of <corpus> -- see "
                         "corpus_scan.py for why this is opt-in")
    ap.add_argument("--checkpoints", action="store_true",
                    help="also pass --checkpoints to vrfkit export and check "
                         "the checkpoint counters (opt-in: real extra time "
                         "and disk per replay)")
    ap.add_argument("--redact-identifiers", action="store_true",
                    help="replace corpus paths and replay filenames in output "
                         "with private, run-local labels")
    return ap.parse_args(argv)


def main() -> int:
    args = parse_args()

    if not args.exe.is_file():
        print(f"executable not found: {args.exe}", file=sys.stderr)
        return 2

    scan = corpus_scan.discover(args.corpus, args.recursive)
    # Unconditional, `excluded=0` included -- see corpus_scan.py's docstring.
    print(corpus_scan.scope_line(scan, args.redact_identifiers))
    files = scan.files
    if args.limit:
        files = files[: args.limit]
        print(f"limited to the first {len(files)} of {len(scan.files)} discovered")
    if not files:
        where = "<private corpus>" if args.redact_identifiers else str(args.corpus)
        print(f"no .vrf files under {where}", file=sys.stderr)
        return 2

    if args.checkpoints:
        print("also decoding Checkpoint chunks (--checkpoints); this costs "
              "real extra time and disk per replay")

    print(f"exporting {len(files)} replays {args.jobs}-wide to read the overlay counters")
    started = time.time()
    unreadable: list[tuple[str, str]] = []
    failing: list[tuple[str, list[tuple[str, int]]]] = []
    totals = {key: 0 for key, *_ in COUNTERS}
    if args.checkpoints:
        totals.update({key: 0 for key, *_ in CHECKPOINT_COUNTERS})
    done = 0

    with ThreadPoolExecutor(max_workers=args.jobs) as pool:
        for counters, err in pool.map(
            lambda f: _export_one(args.exe, f, with_checkpoints=args.checkpoints),
            files,
        ):
            done += 1
            name = corpus_scan.replay_label(
                files[done - 1], done, args.redact_identifiers)
            err = corpus_scan.diagnostic(err, args.redact_identifiers)
            if counters is None:
                unreadable.append((name, err))
            else:
                for k, v in counters.items():
                    totals[k] += v
                failures = replay_failures(counters, args.checkpoints)
                if failures:
                    failing.append((name, failures))
            if done % 25 == 0 or done == len(files):
                print(f"  [{done}/{len(files)}] unreadable={len(unreadable)} "
                      f"failing={len(failing)}")

    elapsed = time.time() - started
    print(f"\nelapsed {elapsed:.1f}s ({elapsed / len(files):.2f}s per replay)")
    print(f"replays read      : {len(files) - len(unreadable)}/{len(files)}")
    print(f"decode errors     : {totals['decode_errors']:,}")
    print(f"decoded OK        : {totals['decoded_ok']:,}")
    print(f"raw/skip          : {totals['raw_skip']:,}")
    print(f"not in table      : {totals['not_in_table']:,}")
    print(f"no field name     : {totals['no_field_name']:,}")
    print(f"rows offered      : {totals['rows_offered']:,}")
    print(f"struct blobs      : {totals['struct_blobs_decoded']:,} decoded / "
          f"{totals['struct_blobs_failed']:,} failed")
    print(f"reward opaque     : {totals['tracked_rewards_opaque_empty_variants']:,} "
          f"empty variants")
    # Unconditional, zeros included: a line printed only when nonzero could
    # not tell "clean" from "never read".
    print(f"movement rows     : {totals['movement_rows']:,}")
    print(f"movement errors   : {totals['movement_errors']:,}")
    print(f"array decode      : {totals['array_elements']:,} elements / "
          f"{totals['array_fields']:,} fields / {totals['array_errors']:,} "
          f"errors / {totals['array_truncations']:,} truncations")
    print(f"array residual    : {totals['array_root_bits']:,} root bits / "
          f"{totals['array_nested_bits']:,} nested bits / "
          f"{totals['array_implicit_ends']:,} implicit ends")
    print(f"array leaf errs   : {totals['array_leaf_errors']:,}")
    print(f"truncated RPCs    : {totals['truncated_rpcs']:,}")
    print(f"cnc brute force   : {totals['cnc_bruteforce_attempted']:,} attempted / "
          f"{totals['cnc_bruteforce_unwalked']:,} unwalked")
    print(f"movement tails    : {totals['movement_sized_tails']:,} sized "
          f"({totals['movement_sized_tail_bits']:,} bits) / "
          f"{totals['movement_open_tails']:,} open "
          f"({totals['movement_open_tail_bits']:,} bits)")
    print(f"unbacked gates    : {unbacked_line(UNBACKED)}")
    if args.checkpoints:
        # Unconditional, zeros included, for the same reason.
        print(f"checkpoint overlay: {totals['checkpoint_decoded']:,} decoded / "
              f"{totals['checkpoint_errors']:,} errors / "
              f"{totals['checkpoint_raw_skip']:,} raw-skip / "
              f"{totals['checkpoint_not_in_table']:,} not-in-table / "
              f"{totals['checkpoint_unnamed']:,} unnamed / "
              f"{totals['checkpoint_conflicts']:,} conflicts / "
              f"{totals['checkpoint_effect_blobs']:,} effect blobs")
        print(f"checkpoint blobs  : {totals['checkpoint_blobs_decoded']:,} "
              f"decoded / {totals['checkpoint_blobs_failed']:,} failed")
        print(f"checkpoint fails  : {totals['checkpoint_fail_array']:,} array / "
              f"{totals['checkpoint_fail_truncated_rpc']:,} truncated RPC / "
              f"{totals['checkpoint_fail_movement']:,} movement")
        print(f"checkpoint array  : {totals['checkpoint_array_elements']:,} "
              f"elements / {totals['checkpoint_array_fields']:,} fields / "
              f"{totals['checkpoint_array_truncations']:,} truncations / "
              f"{totals['checkpoint_array_root_bits']:,} root bits / "
              f"{totals['checkpoint_array_nested_bits']:,} nested bits / "
              f"{totals['checkpoint_array_implicit_ends']:,} implicit ends")
        print(f"checkpoint leaf   : {totals['checkpoint_leaf_errors']:,} "
              f"typed decode errors")
        print("checkpoint cnc brute force: "
              f"{totals['checkpoint_cnc_bruteforce_attempted']:,} attempted / "
              f"{totals['checkpoint_cnc_bruteforce_unwalked']:,} unwalked")
        print("checkpoint movement tails: "
              f"{totals['checkpoint_movement_sized_tails']:,} sized "
              f"({totals['checkpoint_movement_sized_tail_bits']:,} bits) / "
              f"{totals['checkpoint_movement_open_tails']:,} open "
              f"({totals['checkpoint_movement_open_tail_bits']:,} bits)")
        print("checkpoint reward opaque: "
              f"{totals['checkpoint_tracked_rewards_opaque_empty_variants']:,} "
              "empty variants")
        print(f"checkpoint unbacked gates: {unbacked_line(CHECKPOINT_UNBACKED)}")

    if unreadable:
        print(f"\nFAILED: {len(unreadable)} replay(s) did not report the counter",
              file=sys.stderr)
        for name, err in unreadable[:15]:
            print(f"    {name}: {err}", file=sys.stderr)
        return 1
    if failing:
        failing.sort(key=lambda item: -sum(count for _key, count in item[1]))
        print(f"\nFAILED: {len(failing)} replay(s) reported a nonzero failure "
              f"counter", file=sys.stderr)
        for name, failures in failing[:20]:
            print(f"    {name}: " + ", ".join(
                f"{FAILURE_LABELS[key]}={count:,}" for key, count in failures),
                file=sys.stderr)
        print("  Re-run one by hand and read its summary: the 'Struct blob err:', "
              "'Movement err:' and 'Checkpoint blob error:' lines name the "
              "first failure of their kind.", file=sys.stderr)
        return 1
    dead = dead_counters(totals)
    if dead:
        print(f"\nFAILED: {len(dead)} counter(s) never moved, so the clean "
              f"error counters beside them are vacuous", file=sys.stderr)
        for line in dead:
            print(f"    {line}", file=sys.stderr)
        return 1
    if args.checkpoints:
        dead_cp = dead_counters(totals, CHECKPOINT_MUST_MOVE, " (with --checkpoints)")
        if dead_cp:
            print(f"\nFAILED: {len(dead_cp)} checkpoint counter(s) never "
                  f"moved, so the clean checkpoint failure counters beside "
                  f"them are vacuous", file=sys.stderr)
            for line in dead_cp:
                print(f"    {line}", file=sys.stderr)
            return 1

    mismatch = reconcile(totals)
    if mismatch:
        print(f"\nFAILED: the overlay categories do not reconcile: {mismatch}",
              file=sys.stderr)
        return 1
    print(f"reconciles        : decoded OK + decode errors + raw/skip + not "
          f"in table + no field name = rows offered "
          f"({totals['rows_offered']:,})")

    ok_msg = (f"\nOK: {len(files)} replays reported 0 on all {len(FAILURES)} "
              f"failure counters (Decode errors: 0, 0 struct-blob failures, 0 "
              f"array/leaf/truncated-RPC/movement failures, 0 unwalked CNC "
              f"payloads, 0 movement tails): "
              f"{backing_summary(MUST_MOVE, UNBACKED, totals)}")
    if args.checkpoints:
        ok_msg += (f"; checkpoints 0 on all {len(CHECKPOINT_FAILURES)} failure "
                   f"counters: "
                   f"{backing_summary(CHECKPOINT_MUST_MOVE, CHECKPOINT_UNBACKED, totals)}")
    print(ok_msg)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
