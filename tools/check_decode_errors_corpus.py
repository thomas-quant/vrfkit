"""Assert that the type overlay and the struct-blob decoders decode cleanly
across a whole corpus.

`vrfkit validate` does not print the overlay counters at all -- only `export`
does -- so validate_corpus.py cannot see a decode error, and never could. That
matters because a wrong overlay type is exactly the failure this project is
least able to notice by other means: the row still emits, the block still
walks, the block/field/RPC totals do not move, and every counter
validate_corpus.py reads stays identical.

The overlay decoder is strict in both directions -- a decoder that runs off the
end of the payload returns Err(BitIo) and one that leaves bits behind returns
Err(NotFullyConsumed) -- so `Decode errors: 0` over a corpus is a real
statement about every (group, field) type in the table, not just the ones one
replay happens to exercise.

This is the only check that can catch a per-class type whose two candidate
readings happen to be indistinguishable on the reference replay. A rotator
quantization, for instance, is not on the wire: it is a descriptor choice, and
when a class never replicates a rotation both readings consume the same bits.
The choice only becomes observable on a replay where some payload does set a
rotator flag, which may be any replay in the corpus but is not necessarily the
one being developed against.

Exports run into a temporary directory that is deleted as soon as its counters
have been read, so the peak disk cost is (jobs x one replay's Parquet output)
rather than the whole corpus.

Corpus discovery is shared with `validate_corpus.py` through `corpus_scan.py`
-- read that module's docstring for why the default does not recurse into
subdirectories (a `validate_corpus.py`/`check_decode_errors_corpus.py` run
pointed at the same directory used to disagree by exactly this: 153 files
against 126, a 27-file gap in a `Demos/old` subdirectory that this tool's
narrower glob silently skipped, with nothing printed to say so) and why the
excluded count always prints. Pass `--recursive` to walk subdirectories too.

Usage:
    python tools/check_decode_errors_corpus.py <vrfkit.exe> <corpus dir>
    python tools/check_decode_errors_corpus.py <vrfkit.exe> <corpus dir> --jobs 8
    python tools/check_decode_errors_corpus.py <vrfkit.exe> <corpus dir> --recursive
    python tools/check_decode_errors_corpus.py <vrfkit.exe> <corpus dir> --checkpoints

`--checkpoints` passes the same flag on to `vrfkit export`, so it additionally
decodes every Checkpoint chunk each replay carries, and this tool then checks
the checkpoint counters the same way it checks the main pass: every counter in
`CHECKPOINT_COUNTERS` must be present, and every one in `CHECKPOINT_FAILURES`
must be zero. `conflicts` is present-and-printed but NOT a failure counter: a
handle conflict is the overlay REFUSING to type a row whose handle the
replay renamed, which is the protection working, so a nonzero count is a
legitimate outcome and gating on it would be a false alarm on real data. It is
required and printed so a rule that started refusing everything is visible. It is opt-in, not the default: decoding checkpoints is
real extra work, and both this tool's own docstring and every corpus sweep in
this repo need a `--checkpoints`-free invocation to keep meaning the same
thing it always has (see docs/USAGE.md). Before this flag existed, checkpoint
decoding was verified on exactly one pinned replay
(`tools/baselines/checkpoint_02d4d478.json`) and never across a corpus, even
though docs/archive/PROJECT_STATUS.md measured 4,024 checkpoints across the
215-replay corpus.

The struct-blob decoders (RoundResults, TeamEconomy, RoundInfos) are checked
here for the same reason and are, if anything, a worse case: they are additive,
so a total failure moves NOTHING else on the summary. Build 13.02 shifted
RoundResults from handle 93 to 81 and the export stayed clean on every counter
above while the match score silently stopped being written. "Struct blobs:
N decoded / 0 failed" is the statement that did not exist then.

Exit code is 0 only when every replay reported zero on every counter in
`FAILURES` -- overlay decode errors, struct-blob failures, the array, leaf,
truncated-RPC and movement failures summary.rs prints beside them, and the
unwalked CNC brute-force payloads and movement-section tails --
AND every replay reported every counter in `REQUIRED` at all, AND the corpus
as a whole moved every work counter in `MUST_MOVE`. A counter that stops being
printed must not read as zero; that is how the corpus malformed figure stayed a
vacuous 0 for the project's whole history (see docs/archive/PROJECT_STATUS.md
5-O).

The array, leaf, truncated-RPC and movement lines were printed on every export
and never read here, on either pass, although verify_build_corpus.py requires
every one of them to be zero (its SINK_ZERO). The first 2026-09-25 build audit
found 116 nonzero array-counter occurrences across 81 replays on 13.01-13.05
(docs/BUILD_VERIFICATION.md, "Resolved findings") -- main array errors, main
leaf errors and checkpoint leaf errors, not one of which this gate read. The
gates now match verify_build_corpus.py's, pinned by
test_check_decode_errors_corpus.py. `RPC suffix bits` stays ungated on
purpose: verify_build_corpus.py records `rpc_suffix_bits_dropped` as a
limitation, not a counter that must be zero.

The same pin caught the next addition: verify_build_corpus.py's SINK_ZERO grew
`cnc_bruteforce_payloads_unwalked`, `movement_sized_section_tails` and
`movement_open_section_tails` (zero in both passes of all 1,018 audit replays
when added), and this gate reads them off the `CNC brute force` and
`Movement tails` lines and their checkpoint twins. Only the counts are gated,
as there: `attempted` and the tails' bit totals are read and printed, not
failed on.

The last of those three is the same argument one step further, and it was
missing: `Decoded OK` and `Struct blobs: N decoded` were summed, printed, and
never read again. An exporter whose decoders never ran prints

    Rows offered: 0 / Decoded OK: 0 / Decode errors: 0 / Struct blobs: 0
    decoded / 0 failed

for every replay -- every counter a truthful zero, no error anywhere -- and
that used to print "OK: every replay reported Decode errors: 0" and exit 0. A
counter that CANNOT MOVE must not read as success either; see `dead_counters`.

The array, leaf and movement gates then repeated the hole: `Array decode: N
elements / M fields` was parsed, required and printed, and never read, so an
array walker that was never reached passed as "0 array failures". Every
failure gate is now either paired in `MUST_MOVE` / `CHECKPOINT_MUST_MOVE`
with the work counter whose movement makes its zero mean something, or listed
in `UNBACKED` / `CHECKPOINT_UNBACKED` with the reason none exists -- the
truncated-RPC gate on both passes and the checkpoint movement gate -- and
those are printed as unbacked on every run rather than quoted as evidence.
test_check_decode_errors_corpus.py pins that every gate is one or the other.

The process exit status is read for the same reason. `vrfkit export` prints
this summary before it finalises the Parquet files, so an exporter that dies
writing them has already printed `Decode errors: 0`. A nonzero exit makes the
replay unreadable rather than clean; see `read_counters`.
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
    means the line is a single bare number. Anchored on the whole line, start
    to end: `re.search` takes the FIRST match anywhere in the log, and the same
    summary prints free-text diagnostics (`Struct blob err:`, `Movement err:`,
    `Event layout msg:`) whose content this tool does not control. Unanchored,
    a label quoted inside one of those lines would be read in place of the
    counter -- `Array leaf errs: 0` in an error message ahead of a real
    `Array leaf errs: 7` reads as clean. (The other pass's look-alikes,
    `Checkpoint array:`, `Checkpoint movement:` and `truncated RPC`, differ in
    case or wording and cannot match either way.)
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
    """Field `index` of summary.rs's `{} sized ({} bits) / {} open ({} bits)`.

    Not `N unit` pairs joined by ` / `, so `_line_field` cannot spell it; the
    anchoring is the same, and for the same reason. `Checkpoint movement
    tails:` begins with `Checkpoint`, so the anchored main-pass pattern can
    never read it.
    """
    fields = [r"\d+"] * 4
    fields[index] = r"(\d+)"
    return re.compile(
        rf"(?m)^\s*{re.escape(label)}\s+{fields[0]} sized \({fields[1]} bits\) / "
        rf"{fields[2]} open \({fields[3]} bits\)\s*$")


MOVEMENT_SIZED_TAILS = _movement_tails_field("Movement tails:", 0)
MOVEMENT_SIZED_TAIL_BITS = _movement_tails_field("Movement tails:", 1)
MOVEMENT_OPEN_TAILS = _movement_tails_field("Movement tails:", 2)
MOVEMENT_OPEN_TAIL_BITS = _movement_tails_field("Movement tails:", 3)


#: `(key, regex)` for every counter read off the export summary. `no_field_name`
#: is here -- and REQUIRED below -- because summary.rs defines
#: `Rows offered = decoded_ok + decoded_err + raw_or_skip + not_in_table +
#: no_field_name`; leaving it out (as this tool used to) means the four
#: categories it prints sum to about 0.3% less than the `rows offered` line it
#: also prints, and a reader has to go read Rust source to know why. See
#: `reconcile`.
#:
#: The sink lines after `tracked_rewards_opaque_empty_variants` are read whole,
#: work counters (`elements`, `fields`) included, so the totals this tool
#: prints mirror what summary.rs printed and every field of those lines is
#: named -- test_check_decode_errors_corpus.py checks both against summary.rs.
#: `movement_rows` is the work counter behind `movement_errors`; see
#: `MUST_MOVE`.
COUNTERS = (
    ("decode_errors", DECODE_ERRORS),
    ("decoded_ok", DECODED_OK),
    ("raw_skip", RAW_SKIP),
    ("not_in_table", NOT_IN_TABLE),
    ("no_field_name", NO_FIELD_NAME),
    ("rows_offered", ROWS_OFFERED),
    ("struct_blobs_decoded", STRUCT_DECODED),
    ("struct_blobs_failed", STRUCT_FAILED),
    ("tracked_rewards_opaque_empty_variants", REWARD_OPAQUE),
    ("movement_rows", MOVEMENT_ROWS),
    ("movement_errors", MOVEMENT_ERRORS),
    ("array_elements", ARRAY_ELEMENTS),
    ("array_fields", ARRAY_FIELDS),
    ("array_errors", ARRAY_ERRORS),
    ("array_truncations", ARRAY_TRUNCATIONS),
    ("array_root_bits", ARRAY_ROOT_BITS),
    ("array_nested_bits", ARRAY_NESTED_BITS),
    ("array_implicit_ends", ARRAY_IMPLICIT_ENDS),
    ("array_leaf_errors", ARRAY_LEAF_ERRORS),
    ("truncated_rpcs", TRUNCATED_RPCS),
    ("cnc_bruteforce_attempted", CNC_ATTEMPTED),
    ("cnc_bruteforce_unwalked", CNC_UNWALKED),
    ("movement_sized_tails", MOVEMENT_SIZED_TAILS),
    ("movement_sized_tail_bits", MOVEMENT_SIZED_TAIL_BITS),
    ("movement_open_tails", MOVEMENT_OPEN_TAILS),
    ("movement_open_tail_bits", MOVEMENT_OPEN_TAIL_BITS),
)

#: Counters a replay MUST report for its run to mean anything. The work
#: counters (`decoded_ok`, `struct_blobs_decoded`, `movement_rows`, the array
#: `elements` and `fields`) are here as well as the error counters because a
#: zero in an error counter is only evidence when the matching work counter
#: proves the work happened; see `MUST_MOVE`. `no_field_name` is required for
#: the same reason every other line here is: `summary.rs` prints it
#: unconditionally on a healthy export, so its absence means this run's summary
#: cannot be trusted, not that the category was legitimately empty -- and
#: `reconcile` depends on it being a real number, never a defaulted one.
REQUIRED = (
    ("decode_errors", "Decode errors"),
    ("decoded_ok", "Decoded OK"),
    ("raw_skip", "Raw/Skip"),
    ("not_in_table", "Not in table"),
    ("no_field_name", "No field name"),
    ("rows_offered", "Rows offered"),
    ("struct_blobs_decoded", "Struct blobs ... decoded"),
    ("struct_blobs_failed", "Struct blobs ... failed"),
    ("tracked_rewards_opaque_empty_variants", "Reward opaque"),
    ("movement_rows", "Movement rows"),
    ("movement_errors", "Movement errors"),
    ("array_elements", "Array decode ... elements"),
    ("array_fields", "Array decode ... fields"),
    ("array_errors", "Array decode ... errors"),
    ("array_truncations", "Array decode ... truncations"),
    ("array_root_bits", "Array residual ... root bits"),
    ("array_nested_bits", "Array residual ... nested bits"),
    ("array_implicit_ends", "Array residual ... implicit ends"),
    ("array_leaf_errors", "Array leaf errs"),
    ("truncated_rpcs", "Truncated RPCs"),
    ("cnc_bruteforce_attempted", "CNC brute force ... attempted"),
    ("cnc_bruteforce_unwalked", "CNC brute force ... unwalked"),
    ("movement_sized_tails", "Movement tails ... sized"),
    ("movement_sized_tail_bits", "Movement tails ... sized bits"),
    ("movement_open_tails", "Movement tails ... open"),
    ("movement_open_tail_bits", "Movement tails ... open bits"),
)

#: Main-pass counters that must be zero on every replay, and the label a
#: failure is reported under. The same thirteen quantities verify_build_corpus.py's
#: SINK_ZERO requires to be zero, read off the summary rather than the
#: manifest; test_check_decode_errors_corpus.py pins the correspondence, so a
#: counter added there and not here turns that test red.
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

#: `(work counter, label, failure counters it backs)`: corpus totals that
#: cannot legitimately stay at zero, the label to name in the failure, and the
#: `FAILURES` whose zero it turns into evidence. A zero here means the decoder
#: never ran, not that it ran and found nothing -- and every gate in the third
#: column then reads 0 vacuously, because a walker that is never reached
#: cannot report an error.
#:
#: The array walker, the leaf decoders and the movement decoder are additive
#: passes, exactly like the struct blobs: `ExportSink::on_field` calls the
#: array flattener before the parent row is pushed and the overlay applied, so
#: if `is_known_array_field` stopped matching, `Decoded OK`, the struct blobs
#: and `reconcile` would all be unchanged and the five array gates and the
#: leaf gate would read 0 forever. `Array decode: N elements` is what moves
#: when the walker runs (every leaf-error path runs after a
#: `decode_struct_array*` call, so `fields` backs the leaf gate), and
#: `Movement rows` is what moves when `decode_movement_rpc` runs -- it is the
#: only producer of movement.parquet rows.
#:
#: None of these is zero on a healthy replay, so a corpus of any size -- one
#: replay included -- does not fail on them. Measured 2026-09-28 over the 1,018
#: `export.log`s of the 259ed10 build audit (`vrfkit export --checkpoints`,
#: 24 builds 11.06-13.06), each line read with this module's own regexes:
#: minimum per replay 4,654 decoded rows, 16 struct blobs, 32 array elements,
#: 182 array fields and 3,001 movement rows; zero replays at zero on any of
#: them, the pre-13.01 builds (outside the measured-route allowlist) included.
#:
#: A corpus total catches a walker that stopped everywhere. It cannot catch one
#: that stopped on one build of a mixed corpus, or on one route while the
#: others keep the total up: the measured array routes are branch-gated in
#: `enable_measured_array_routes` and the generic routes run on every build.
#: summary.rs prints no per-route counter to catch that with.
MUST_MOVE = (
    ("decoded_ok", "Decoded OK", ("decode_errors",)),
    ("struct_blobs_decoded", "Struct blobs ... decoded", ("struct_blobs_failed",)),
    ("array_elements", "Array decode ... elements",
     ("array_errors", "array_truncations", "array_root_bits",
      "array_nested_bits", "array_implicit_ends")),
    ("array_fields", "Array decode ... fields", ("array_leaf_errors",)),
    ("movement_rows", "Movement rows",
     ("movement_errors", "movement_sized_tails", "movement_open_tails")),
)

#: `(failure counter, why no work counter backs it)`: the `FAILURES` whose zero
#: this tool cannot turn into evidence, printed on every run so the OK line is
#: not read as vouching for them.
#:
#: `truncated_rpcs` is bumped inside `try_parse_rpc_params` when a
#: FunctionParameters walk breaks, and summary.rs prints no count of walks
#: begun. The nearest printed counters do not stand in for one: `RPCs:` and
#: `Sink tally: ... RPCs` also count the post-RepLayout ClassNetCache tails,
#: which never enter the walk and numbered at least 6 on every one of the
#: 1,018 audit replays -- so those totals stay above zero with the walk never
#: running -- and `on_rpc` counts an RPC whose function name did not resolve
#: exactly as it counts one it walked: `try_parse_rpc_params` returns before
#: the walk begins, the raw fallback row is written, the RPC tally still
#: moves. Backing this gate needs a walk counter on the Rust side.
UNBACKED = (
    ("truncated_rpcs", "summary.rs prints no count of RPC parameter walks"),
    ("cnc_bruteforce_unwalked",
     "`CNC brute force: N attempted` is legitimately 0 on some builds -- the "
     "12.10 and 12.11 public fixtures -- so it cannot be a must-move counter"),
)

# --- Checkpoint counters, parsed only when --checkpoints was passed --------
#
# summary.rs's print_checkpoints packs several counters onto each line (see
# crates/vrfkit/src/driver/summary.rs), unlike the main pass which gets one
# regex per counter -- so each of these three patterns carries more than one
# capture group, and CHECKPOINT_COUNTERS names which group is which counter.
# The seven fields summary.rs prints, in its order. `conflicts` is the one this
# pattern was missing: it prints
#
#   Overlay: {} decoded / {} errors / {} raw-skip / {} not-in-table /
#            {} unnamed / {} conflicts / {} effect blobs
#
# and this regex asked for six fields, so it matched NOTHING on a real
# `--checkpoints` run -- every replay came back "no Overlay ... decoded
# (checkpoint) counter" and the whole sweep failed as unreadable. It failed
# loudly rather than passing vacuously, which is the one thing that saved it,
# but the check had not run since the Rust side grew the field.
# test_check_decode_errors_corpus.py reads this format string OUT OF summary.rs
# so the next field added there breaks the test instead of silently disabling
# the check again.
CHECKPOINT_OVERLAY = re.compile(
    r"Overlay:\s+(\d+) decoded / (\d+) errors / (\d+) raw-skip / "
    r"(\d+) not-in-table / (\d+) unnamed / (\d+) conflicts / "
    r"(\d+) effect blobs")
CHECKPOINT_BLOBS = re.compile(r"Checkpoint blobs:\s+(\d+) decoded / (\d+) failed")
CHECKPOINT_FAILS = re.compile(
    r"Checkpoint fails:\s+(\d+) array / (\d+) truncated RPC / (\d+) movement")
CHECKPOINT_REWARD_OPAQUE = re.compile(
    r"(?m)^\s*Checkpoint reward opaque:\s+(\d+) empty variants\s*$")
# `Checkpoint fails: N array` above is the array walker's `errors` only. Its
# truncations, residual bits and implicit ends are on this line, and leaf
# decode errors on the next; neither line was read before 2026-09-28.
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

#: `(key, regex, group)` for every checkpoint counter. Only consulted when the
#: caller asks `read_counters` for `require_checkpoints=True` -- a summary from
#: a run without `--checkpoints` never has this block at all, and treating its
#: absence as failure there would break the existing, checkpoint-free
#: invocation this tool has always supported.
CHECKPOINT_COUNTERS = (
    ("checkpoint_decoded", CHECKPOINT_OVERLAY, 1),
    ("checkpoint_errors", CHECKPOINT_OVERLAY, 2),
    ("checkpoint_raw_skip", CHECKPOINT_OVERLAY, 3),
    ("checkpoint_not_in_table", CHECKPOINT_OVERLAY, 4),
    ("checkpoint_unnamed", CHECKPOINT_OVERLAY, 5),
    ("checkpoint_conflicts", CHECKPOINT_OVERLAY, 6),
    ("checkpoint_effect_blobs", CHECKPOINT_OVERLAY, 7),
    ("checkpoint_blobs_decoded", CHECKPOINT_BLOBS, 1),
    ("checkpoint_blobs_failed", CHECKPOINT_BLOBS, 2),
    ("checkpoint_fail_array", CHECKPOINT_FAILS, 1),
    ("checkpoint_fail_truncated_rpc", CHECKPOINT_FAILS, 2),
    ("checkpoint_fail_movement", CHECKPOINT_FAILS, 3),
    ("checkpoint_tracked_rewards_opaque_empty_variants", CHECKPOINT_REWARD_OPAQUE, 1),
    ("checkpoint_array_elements", CHECKPOINT_ARRAY, 1),
    ("checkpoint_array_fields", CHECKPOINT_ARRAY, 2),
    ("checkpoint_array_truncations", CHECKPOINT_ARRAY, 3),
    ("checkpoint_array_root_bits", CHECKPOINT_ARRAY, 4),
    ("checkpoint_array_nested_bits", CHECKPOINT_ARRAY, 5),
    ("checkpoint_array_implicit_ends", CHECKPOINT_ARRAY, 6),
    ("checkpoint_leaf_errors", CHECKPOINT_LEAF, 1),
    ("checkpoint_cnc_bruteforce_attempted", CHECKPOINT_CNC_BRUTEFORCE, 1),
    ("checkpoint_cnc_bruteforce_unwalked", CHECKPOINT_CNC_BRUTEFORCE, 2),
    ("checkpoint_movement_sized_tails", CHECKPOINT_MOVEMENT_TAILS, 1),
    ("checkpoint_movement_sized_tail_bits", CHECKPOINT_MOVEMENT_TAILS, 2),
    ("checkpoint_movement_open_tails", CHECKPOINT_MOVEMENT_TAILS, 3),
    ("checkpoint_movement_open_tail_bits", CHECKPOINT_MOVEMENT_TAILS, 4),
)

#: Every checkpoint counter is REQUIRED, on the same reasoning as `REQUIRED`
#: above: `with_checkpoints.then_some(&cp_stats)` in driver/mod.rs means the
#: whole `=== Checkpoints ===` block prints, zeros included, on every export
#: run with `--checkpoints` -- even for a replay with no checkpoint chunks at
#: all. Its absence therefore means this run's checkpoint pass cannot be
#: trusted, not that there was nothing to report. Labels name the printed line
#: they come from, exactly as `REQUIRED` above does.
CHECKPOINT_REQUIRED = (
    ("checkpoint_decoded", "Overlay ... decoded (checkpoint)"),
    ("checkpoint_errors", "Overlay ... errors (checkpoint)"),
    ("checkpoint_raw_skip", "Overlay ... raw-skip (checkpoint)"),
    ("checkpoint_not_in_table", "Overlay ... not-in-table (checkpoint)"),
    ("checkpoint_unnamed", "Overlay ... unnamed (checkpoint)"),
    ("checkpoint_conflicts", "Overlay ... conflicts (checkpoint)"),
    ("checkpoint_effect_blobs", "Overlay ... effect blobs (checkpoint)"),
    ("checkpoint_blobs_decoded", "Checkpoint blobs ... decoded"),
    ("checkpoint_blobs_failed", "Checkpoint blobs ... failed"),
    ("checkpoint_fail_array", "Checkpoint fails ... array"),
    ("checkpoint_fail_truncated_rpc", "Checkpoint fails ... truncated RPC"),
    ("checkpoint_fail_movement", "Checkpoint fails ... movement"),
    ("checkpoint_tracked_rewards_opaque_empty_variants", "Checkpoint reward opaque"),
    ("checkpoint_array_elements", "Checkpoint array ... elements"),
    ("checkpoint_array_fields", "Checkpoint array ... fields"),
    ("checkpoint_array_truncations", "Checkpoint array ... truncations"),
    ("checkpoint_array_root_bits", "Checkpoint array ... root bits"),
    ("checkpoint_array_nested_bits", "Checkpoint array ... nested bits"),
    ("checkpoint_array_implicit_ends", "Checkpoint array ... implicit ends"),
    ("checkpoint_leaf_errors", "Checkpoint leaf ... typed decode errors"),
    ("checkpoint_cnc_bruteforce_attempted", "Checkpoint CNC brute force ... attempted"),
    ("checkpoint_cnc_bruteforce_unwalked", "Checkpoint CNC brute force ... unwalked"),
    ("checkpoint_movement_sized_tails", "Checkpoint movement tails ... sized"),
    ("checkpoint_movement_sized_tail_bits", "Checkpoint movement tails ... sized bits"),
    ("checkpoint_movement_open_tails", "Checkpoint movement tails ... open"),
    ("checkpoint_movement_open_tail_bits", "Checkpoint movement tails ... open bits"),
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

#: Checkpoint corpus totals that cannot legitimately stay at zero once
#: --checkpoints is on, mirroring MUST_MOVE for the main pass, in the same
#: `(work counter, label, failure counters it backs)` shape. This is the
#: 13.02 RoundResults incident one level down: `Checkpoint blobs: N decoded`
#: is additive, so a checkpoint decoder that stopped running would move
#: nothing else on the summary, and "0 failed" beside a corpus-wide zero
#: `decoded` says nothing. The checkpoint array walker is additive the same
#: way. Measured on the same 1,018 audit logs as `MUST_MOVE` (every replay
#: carried at least 5 checkpoints): minimum per replay 2,054 decoded fields,
#: 16 blobs, 30 array elements and 255 array fields, no replay at zero.
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

#: `UNBACKED`, for the checkpoint pass. Both gates sit behind the RPC callback
#: (`on_rpc` runs the movement decoder and the parameter walk), and no
#: checkpoint RPC reached it on the 1,018 audit replays: every one was a
#: post-RepLayout ClassNetCache tail -- `Checkpoint tails: N decoded` equalled
#: `Checkpoint sink: ... N RPCs` on 1,018/1,018 (181,108 in all), and
#: `Dropped: ... N movement rows (checkpoint snapshot)` read 0 on all of them.
#: A must-move entry here would fail every healthy corpus; backing
#: `checkpoint_fail_truncated_rpc` with the checkpoint RPC count would vouch for
#: a walk that never ran.
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
    """`(counters, error)` for one export's output. `counters` is None on failure.

    A nonzero exit is a failure even when the summary parsed cleanly: the
    exporter prints these counters before it finalises the Parquet files, so a
    run that dies writing them has already printed `Decode errors: 0`.

    `require_checkpoints` defaults to False so a summary from a run without
    `--checkpoints` -- which has no `=== Checkpoints ===` block at all -- keeps
    reading exactly as it always has. Pass it only when this replay's export
    was itself run with `--checkpoints`.
    """
    tail = " | ".join(l for l in text.splitlines()[-3:] if l.strip())
    if returncode != 0:
        return None, f"exit {returncode}: {tail[:200]}"
    counters: dict[str, int] = {}
    for key, pattern in COUNTERS:
        m = pattern.search(text)
        if m:
            counters[key] = int(m.group(1))
    for required, label in REQUIRED:
        if required not in counters:
            return None, f"no {label} counter: {tail[:200]}"
    if require_checkpoints:
        for key, pattern, group in CHECKPOINT_COUNTERS:
            m = pattern.search(text)
            if m:
                counters[key] = int(m.group(group))
        for required, label in CHECKPOINT_REQUIRED:
            if required not in counters:
                return None, f"no {label} counter: {tail[:200]}"
    return counters, ""


def replay_failures(counters: dict[str, int], checkpoints: bool) -> list[tuple[str, int]]:
    """`(key, count)` for every failure counter that is nonzero on one replay.

    One table decides what a failure is, for both passes. The per-counter
    classification used to live inline in `main()`, which gated two main-pass
    counters and five checkpoint ones while summary.rs printed ten of each.

    Indexed, never `.get(key, 0)`: a counter missing here must raise, not
    gate as a zero. `read_counters` requires every one of them, so on a real
    run the KeyError cannot fire.
    """
    gated = FAILURES + (CHECKPOINT_FAILURES if checkpoints else ())
    return [(key, counters[key]) for key, _label in gated if counters[key]]


def _vacuous(gates: tuple[str, ...]) -> str:
    """The failure labels a dead work counter leaves without evidence."""
    return ", ".join(FAILURE_LABELS[key] for key in gates)


def dead_counters(totals: dict[str, int]) -> list[str]:
    """Corpus totals that never moved, as human-readable failures.

    `Decode errors: 0` is only a statement about the overlay if something was
    decoded, and `Struct blobs: 0 failed` is only a statement about the struct
    decoders if some blob was decoded. Both counters were summed and printed
    and then never read, so a corpus on which nothing ran at all reported a
    clean sweep. The array and movement gates had the same hole after they
    were added: their work counters were summed and printed and never read, so
    an array walker that was never reached passed as "0 array failures".
    """
    return [f"{label} totalled 0 across the corpus: nothing decoded, so the "
            f"zero in {_vacuous(gates)} says nothing"
            for key, label, gates in MUST_MOVE if not totals.get(key)]


def dead_checkpoint_counters(totals: dict[str, int]) -> list[str]:
    """`dead_counters`, for the checkpoint pass. Only meaningful -- and only
    ever called -- when `--checkpoints` was requested; see `CHECKPOINT_MUST_MOVE`.
    """
    return [f"{label} totalled 0 across the corpus (with --checkpoints): "
            f"nothing decoded, so the zero in {_vacuous(gates)} says nothing"
            for key, label, gates in CHECKPOINT_MUST_MOVE if not totals.get(key)]


def unbacked_line(unbacked: tuple[tuple[str, str], ...]) -> str:
    """The failure gates no work counter backs, and why, on one line.

    Printed on every run, clean or not: the OK line counts these gates among
    the ones that read 0, and without this line nothing would say that their
    zero is not evidence of anything.
    """
    return "; ".join(f"{FAILURE_LABELS[key]} ({why})" for key, why in unbacked)


def backing_summary(must_move, unbacked, totals: dict[str, int]) -> str:
    """How many of a pass's zero failure counters are evidence, and of what.

    Built from the tables rather than spelled out, so a work counter added to
    `MUST_MOVE` cannot be left out of the OK line that quotes it, and an
    unbacked gate cannot be counted among the ones the work vouches for.
    """
    backed = sum(len(gates) for _key, _label, gates in must_move)
    work = ", ".join(f"{label} {totals[key]:,}" for key, label, _gates in must_move)
    none = ", ".join(FAILURE_LABELS[key] for key, _why in unbacked)
    return (f"{backed} backed by work that moved ({work}), {len(unbacked)} "
            f"with no work counter ({none})")


def reconcile(totals: dict[str, int]) -> str | None:
    """None if the five overlay categories sum to `rows_offered`; else why not.

    summary.rs defines `Rows offered = decoded_ok + decoded_err + raw_or_skip
    + not_in_table + no_field_name`. This tool printed the first four for a
    long time and never `no_field_name`, so its own printed categories summed
    to about 0.3% less than its own `rows offered` line -- correct numbers,
    illegible arithmetic, and a reader had to go read the Rust source to know
    the fifth category existed at all.

    `no_field_name` is indexed directly, never `totals.get("no_field_name",
    0)`: a `.get` with a default would make an ABSENT counter reconcile
    silently, which is the exact doctrine this function exists to enforce
    against -- see `REQUIRED`, which is what keeps this KeyError from ever
    firing on a real run.

    `no_field_name` deliberately does NOT join `MUST_MOVE`/`dead_counters`
    above: unlike `decoded_ok` and `struct_blobs_decoded`, which are large on
    any real corpus and a corpus-wide zero for either means the decoder never
    ran, a corpus where every handle happens to resolve to a name is a
    legitimate (if unlikely) outcome for `no_field_name`, not evidence the
    check never ran. Gating on it would be a false failure on a clean corpus.
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
) -> tuple[str, dict[str, int] | None, str]:
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
        counters, err = read_counters((r.stdout or "") + (r.stderr or ""),
                                      r.returncode, require_checkpoints=with_checkpoints)
        return replay.name, counters, err
    except subprocess.TimeoutExpired:
        return replay.name, None, "timeout"
    finally:
        shutil.rmtree(out, ignore_errors=True)


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    """`argv=None` defers to `sys.argv[1:]` (argparse's own default); tests pass
    an explicit list instead of monkeypatching `sys.argv`."""
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
    totals = {key: 0 for key, _pattern in COUNTERS}
    if args.checkpoints:
        totals.update({key: 0 for key, _pattern, _group in CHECKPOINT_COUNTERS})
    done = 0

    with ThreadPoolExecutor(max_workers=args.jobs) as pool:
        for name, counters, err in pool.map(
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
    # Unconditional, zeros included: these are failure counters, and a line
    # printed only when nonzero could not tell "clean" from "never read".
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
        # Unconditional, zeros included, on the same reasoning as every other
        # line here: a conditional line could not tell "the checkpoint pass
        # ran clean" from "the checkpoint pass never reached these counters".
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
        dead_cp = dead_checkpoint_counters(totals)
        if dead_cp:
            print(f"\nFAILED: {len(dead_cp)} checkpoint counter(s) never "
                  f"moved, so the clean checkpoint failure counters beside "
                  f"them are vacuous", file=sys.stderr)
            for line in dead_cp:
                print(f"    {line}", file=sys.stderr)
            return 1

    # Fails loudly rather than printing a plausible wrong subtotal: a
    # mismatch here means summary.rs's five overlay categories no longer sum
    # to its own `rows offered` line -- most likely a sixth category was added
    # in Rust that this tool does not know to parse yet, which is exactly the
    # kind of drift a passing sweep must not paper over. See `reconcile`.
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
