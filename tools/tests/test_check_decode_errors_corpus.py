"""Guards for the corpus decode-error gate.

The gate's own docstring argues that "a counter that stops being printed must
not read as zero". Its exit path did not carry that argument one step further:
`Decoded OK` and `Struct blobs: N decoded` were summed and printed and then
never read, so an exporter that decoded NOTHING -- every counter a legitimate
zero -- printed "OK: every replay reported Decode errors: 0" and exited 0. A
counter that cannot move must not read as success either.

The second hole was the process exit status. The summary is printed before the
Parquet files are finalised, so an exporter that dies writing them has already
printed `Decode errors: 0`, and the run counted as a clean replay.

A third hole, unrelated to either of those: this file never parsed `No field
name`, even though summary.rs defines
`Rows offered = decoded_ok + decoded_err + raw_or_skip + not_in_table +
no_field_name`. The five categories this tool DID print therefore summed to
about 0.3% less than its own `rows offered` line, and a reader could not make
the numbers add up without going to read the Rust source. See `LIVE_EXPORT`
and `ReconcileTests`.

A fourth: the main pass never read the array, leaf, truncated-RPC and movement
failure lines, and the checkpoint pass never read its array truncation,
residual and leaf lines, although summary.rs prints all of them unconditionally
and verify_build_corpus.py requires every one to be zero. A replay with seven
leaf errors classified clean and the run printed OK. See `SinkFailureTests`.

A fifth, the first one again for those gates: their work counters (`Array
decode: N elements / M fields`, `Movement rows`) were read and printed and
never required to move, so an array walker that was never reached passed as
"0 array failures". See `LivenessCoverageTests` and the `noarray` /
`cpnoarray` / `nomovement` wiring tests.
"""
import contextlib
import io
import os
import re
import sys
import tempfile
import unittest
from pathlib import Path


sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import check_decode_errors_corpus as guard  # noqa: E402


#: The main-pass sink lines summary.rs prints unconditionally, every failure
#: counter at zero -- the shape a clean export has. Values taken from a real
#: 13.02 `--checkpoints` export log. Every main-pass fixture below carries
#: them, because the gate requires each line. The `CNC brute force` and
#: `Movement tails` lines joined when their counters entered
#: verify_build_corpus.py's SINK_ZERO; their values are illustrative, not
#: from that log.
CLEAN_SINK = """
Movement rows:     2407298
Movement errors:   0
Array decode:      25052 elements / 96076 fields / 0 errors / 0 truncations
Array residual:    0 root bits / 0 nested bits / 0 implicit ends
Array leaf errs:   0
Truncated RPCs:    0
CNC brute force:   454 attempted / 0 unwalked
Movement tails:    0 sized (0 bits) / 0 open (0 bits)
"""

#: A healthy export summary, labels as driver.rs prints them.
CLEAN = """
Rows offered:      130000
Decoded OK:        129000
Decode errors:     0
Raw/Skip:          900
Not in table:      100
No field name:     0
Struct blobs:      63 decoded / 0 failed
Reward opaque:     4470 empty variants
""" + CLEAN_SINK

#: Measured on a live export -- not synthesized. 742738 + 0 + 72644 + 171605 +
#: 1996 = 988983, an exact match to "Rows offered" only once `No field name`
#: is part of the sum. `CLEAN_SINK` is appended: those lines are not part of
#: that reconciliation, and every clean export prints them as zeros.
LIVE_EXPORT = """
Rows offered:      988983
Decoded OK:        742738
Decode errors:     0
Raw/Skip:          72644
Not in table:      171605
No field name:     1996
Struct blobs:      63 decoded / 0 failed
Reward opaque:     4470 empty variants
""" + CLEAN_SINK


def replace_line(text: str, label: str, new: str) -> str:
    """`text` with its one line starting with `label` replaced by `new`.

    Asserts the label occurs exactly once, so a fixture that lost the line
    cannot turn a mutation into a no-op and the test built on it into one
    that passes without testing anything.
    """
    lines = text.splitlines()
    hits = [i for i, line in enumerate(lines) if line.strip().startswith(label)]
    if len(hits) != 1:
        raise AssertionError(f"fixture has {len(hits)} lines starting {label!r}")
    lines[hits[0]] = new
    return "\n".join(lines) + "\n"


def drop_line(text: str, label: str) -> str:
    """`text` without its one line starting with `label`; see `replace_line`."""
    lines = text.splitlines()
    kept = [line for line in lines if not line.strip().startswith(label)]
    if len(lines) - len(kept) != 1:
        raise AssertionError(
            f"fixture has {len(lines) - len(kept)} lines starting {label!r}")
    return "\n".join(kept) + "\n"


class ReadCountersTests(unittest.TestCase):
    def test_a_clean_summary_reads_as_counters(self):
        counters, err = guard.read_counters(CLEAN, 0)
        self.assertEqual(err, "")
        self.assertEqual(counters["decode_errors"], 0)
        self.assertEqual(counters["decoded_ok"], 129000)
        self.assertEqual(counters["struct_blobs_decoded"], 63)
        self.assertEqual(counters["tracked_rewards_opaque_empty_variants"], 4470)

    def test_a_nonzero_exit_is_not_a_clean_replay(self):
        """The summary prints before the Parquet files are finalised.

        An exporter that prints `Decode errors: 0` and then dies writing its
        output is not a replay that decoded cleanly, and counting it as one is
        how a whole corpus can pass on partial exports.
        """
        counters, err = guard.read_counters(CLEAN, 1)
        self.assertIsNone(counters)
        self.assertIn("exit 1", err)

    def test_a_summary_without_decoded_ok_is_unreadable(self):
        text = "\n".join(l for l in CLEAN.splitlines() if "Decoded OK" not in l)
        counters, err = guard.read_counters(text, 0)
        self.assertIsNone(counters)
        self.assertIn("Decoded OK", err)

    def test_a_summary_without_the_struct_blob_line_is_unreadable(self):
        text = "\n".join(l for l in CLEAN.splitlines() if "Struct blobs" not in l)
        counters, err = guard.read_counters(text, 0)
        self.assertIsNone(counters)

    def test_reward_opaque_label_is_required_and_cannot_match_checkpoint_label(self):
        text = "\n".join(l for l in CLEAN.splitlines() if "Reward opaque" not in l)
        text += "\nCheckpoint reward opaque: 4470 empty variants\n"
        counters, err = guard.read_counters(text, 0)
        self.assertIsNone(counters)
        self.assertIn("Reward opaque", err)

    def test_no_field_name_is_read(self):
        counters, err = guard.read_counters(LIVE_EXPORT, 0)
        self.assertEqual(err, "")
        self.assertEqual(counters["no_field_name"], 1996)

    def test_a_summary_without_no_field_name_is_unreadable(self):
        """`no_field_name` feeds the reconciliation, so it must be REQUIRED --
        the same "a counter that stops being printed must not read as zero"
        rule the other counters already get."""
        text = "\n".join(l for l in CLEAN.splitlines() if "No field name" not in l)
        counters, err = guard.read_counters(text, 0)
        self.assertIsNone(counters)
        self.assertIn("No field name", err)

    def test_every_overlay_summary_counter_is_required_even_when_zero(self):
        """Omitting a zero line must not be indistinguishable from printing 0."""
        for label in ("Raw/Skip", "Not in table", "Rows offered"):
            with self.subTest(label=label):
                text = "\n".join(
                    line for line in CLEAN.splitlines() if label not in line
                )
                counters, err = guard.read_counters(text, 0)
                self.assertIsNone(counters)
                self.assertIn(label, err)


#: Corpus totals from a working main pass: every work counter moved, every
#: failure counter at zero.
WORKING_TOTALS = {
    "decode_errors": 0, "decoded_ok": 129000, "raw_skip": 900,
    "not_in_table": 100, "no_field_name": 0, "rows_offered": 130000,
    "struct_blobs_decoded": 63, "struct_blobs_failed": 0,
    "movement_rows": 2407298, "array_elements": 25052, "array_fields": 96076,
}

#: `(work counter, the label its failure must name, a failure label it must
#: say is left without evidence)` for every main-pass work counter.
MAIN_WORK_CASES = (
    ("decoded_ok", "Decoded OK", "Decode errors"),
    ("struct_blobs_decoded", "Struct blobs ... decoded", "Struct blobs failed"),
    ("array_elements", "Array decode ... elements", "Array residual root bits"),
    ("array_fields", "Array decode ... fields", "Array leaf errs"),
    ("movement_rows", "Movement rows", "Movement errors"),
)


class DeadCounterTests(unittest.TestCase):
    """The counters that are summed for the whole corpus and must have moved."""

    def test_a_working_corpus_has_no_dead_counters(self):
        self.assertEqual(guard.dead_counters(WORKING_TOTALS), [])

    def test_a_corpus_where_no_overlay_row_decoded_is_not_a_pass(self):
        totals = dict(WORKING_TOTALS, decoded_ok=0, raw_skip=0, not_in_table=0,
                      rows_offered=0)
        dead = guard.dead_counters(totals)
        self.assertTrue(dead)
        self.assertIn("Decoded OK", " ".join(dead))

    def test_a_corpus_where_no_struct_blob_decoded_is_not_a_pass(self):
        """The 13.02 shape: the decoders stop running and nothing else moves."""
        totals = dict(WORKING_TOTALS, struct_blobs_decoded=0)
        dead = guard.dead_counters(totals)
        self.assertTrue(dead)
        self.assertIn("Struct blobs", " ".join(dead))

    def test_each_work_counter_that_stays_at_zero_is_named_alone(self):
        """The array walker and the movement decoder are additive passes, like
        the struct blobs: stopping one moves nothing else, so its own work
        counter is the only thing that can say so -- and the failure must name
        the gates it leaves without evidence."""
        for key, label, gate in MAIN_WORK_CASES:
            with self.subTest(counter=key):
                dead = guard.dead_counters(dict(WORKING_TOTALS, **{key: 0}))
                self.assertEqual(len(dead), 1, dead)
                self.assertTrue(dead[0].startswith(f"{label} totalled 0"), dead)
                self.assertIn(gate, dead[0])

    def test_an_absent_work_counter_is_dead_not_a_pass(self):
        """`read_counters` requires each one; a totals dict without it must
        still not read as work that happened."""
        for key, label, _gate in MAIN_WORK_CASES:
            with self.subTest(counter=key):
                totals = dict(WORKING_TOTALS)
                del totals[key]
                self.assertEqual(len(guard.dead_counters(totals)), 1)

    def test_an_exporter_that_decoded_nothing_at_all_fails_on_every_one(self):
        totals = {key: 0 for key in WORKING_TOTALS}
        self.assertEqual(len(guard.dead_counters(totals)), len(MAIN_WORK_CASES))


class ReconcileTests(unittest.TestCase):
    """`Rows offered` is defined in summary.rs as the sum of five categories;
    this tool used to print only four of them. `reconcile()` is the check that
    the five categories this tool now prints actually add up to the sixth
    number it also prints, so a reader never has to go read Rust source to
    make the totals add up.
    """

    def test_the_live_export_reconciles(self):
        """742738 + 0 + 72644 + 171605 + 1996 = 988983 -- measured, not invented."""
        counters, err = guard.read_counters(LIVE_EXPORT, 0)
        self.assertEqual(err, "")
        self.assertIsNone(guard.reconcile(counters))

    def test_a_mismatch_is_reported_with_both_numbers(self):
        totals = dict(decoded_ok=742738, decode_errors=0, raw_skip=72644,
                      not_in_table=171605, no_field_name=0,  # dropped 1996
                      rows_offered=988983)
        problem = guard.reconcile(totals)
        self.assertIsNotNone(problem)
        self.assertIn("988,983", problem)
        self.assertIn("986,987", problem)  # the wrong sum without no_field_name

    def test_no_field_name_absent_from_totals_is_a_loud_error_not_a_silent_zero(self):
        """`.get(..., 0)` here would make an absent counter reconcile by
        accident -- the same doctrine violation this whole fix exists to
        close. Indexing directly means a missing key raises."""
        totals = dict(decoded_ok=129000, decode_errors=0, raw_skip=900,
                      not_in_table=100, rows_offered=130000)
        with self.assertRaises(KeyError):
            guard.reconcile(totals)


class ArgParsingTests(unittest.TestCase):
    def test_every_flag_is_opt_in_and_checkpoints_reach_the_export_argv(self):
        """The existing invocation in docs/USAGE.md must keep working
        unchanged -- checkpoints cost real time and disk, so opt-in only --
        and `--checkpoints` must reach the `vrfkit export` argv, not just this
        tool's own flag parsing."""
        plain = guard.parse_args(["vrfkit.exe", "corpus"])
        for flag in ("recursive", "checkpoints", "redact_identifiers"):
            with self.subTest(flag=flag):
                self.assertFalse(getattr(plain, flag))
                given = guard.parse_args(
                    ["vrfkit.exe", "corpus", "--" + flag.replace("_", "-")])
                self.assertTrue(getattr(given, flag))
        paths = (Path("vrfkit.exe"), Path("a.vrf"), Path("out"))
        self.assertNotIn("--checkpoints",
                         guard.export_command(*paths, with_checkpoints=False))
        self.assertIn("--checkpoints",
                      guard.export_command(*paths, with_checkpoints=True))


#: The `=== Checkpoints ===` block, appended to a healthy main summary, exactly
#: as summary.rs's print_checkpoints prints it.
CLEAN_WITH_CHECKPOINTS = LIVE_EXPORT + """
=== Checkpoints ===
  Checkpoints:      12
  Overlay:          500 decoded / 0 errors / 20 raw-skip / 5 not-in-table / 2 unnamed / 4 conflicts / 1 effect blobs
  Checkpoint blobs: 8 decoded / 0 failed
  Checkpoint fails: 0 array / 0 truncated RPC / 0 movement
  Checkpoint array: 44652 elements / 364594 fields / 0 truncations / 0 root bits / 0 nested bits / 0 implicit ends
  Checkpoint leaf:  0 typed decode errors
  Checkpoint reward opaque: 7 empty variants
  Checkpoint movement: 0 failures
  Checkpoint movement tails: 0 sized (0 bits) / 0 open (0 bits)
  Checkpoint CNC:   3 RPC rows
  Checkpoint CNC brute force: 0 attempted / 0 unwalked
"""


class CheckpointCounterTests(unittest.TestCase):
    """Follows the module's own discipline: a checkpoint counter that stops
    being printed must be a failure, never read as a passing zero -- read the
    module docstring's RoundResults/13.02 incident, now one level down for the
    checkpoint pass specifically.
    """

    def test_default_call_does_not_require_checkpoint_counters(self):
        """Backward compatible: a summary with no checkpoint block at all is
        still readable when `--checkpoints` was not requested."""
        counters, err = guard.read_counters(LIVE_EXPORT, 0)
        self.assertEqual(err, "")
        self.assertNotIn("checkpoint_decoded", counters)

    def test_checkpoint_counters_are_read_when_required(self):
        counters, err = guard.read_counters(
            CLEAN_WITH_CHECKPOINTS, 0, require_checkpoints=True)
        self.assertEqual(err, "")
        self.assertEqual(counters["checkpoint_decoded"], 500)
        self.assertEqual(counters["checkpoint_errors"], 0)
        self.assertEqual(counters["checkpoint_unnamed"], 2)
        self.assertEqual(counters["checkpoint_conflicts"], 4)
        self.assertEqual(counters["checkpoint_effect_blobs"], 1)
        self.assertEqual(counters["checkpoint_blobs_decoded"], 8)
        self.assertEqual(counters["checkpoint_blobs_failed"], 0)
        self.assertEqual(counters["checkpoint_fail_array"], 0)
        self.assertEqual(counters["checkpoint_fail_truncated_rpc"], 0)
        self.assertEqual(counters["checkpoint_fail_movement"], 0)
        self.assertEqual(counters["checkpoint_tracked_rewards_opaque_empty_variants"], 7)

    def test_checkpoint_reward_opaque_label_is_required_and_cannot_match_main_label(self):
        text = "\n".join(
            l for l in CLEAN_WITH_CHECKPOINTS.splitlines()
            if "Checkpoint reward opaque" not in l)
        text += "\nReward opaque: 7 empty variants\n"
        counters, err = guard.read_counters(text, 0, require_checkpoints=True)
        self.assertIsNone(counters)
        self.assertIn("Checkpoint reward opaque", err)

    def test_a_summary_missing_the_checkpoint_block_is_unreadable_when_required(self):
        counters, err = guard.read_counters(
            LIVE_EXPORT, 0, require_checkpoints=True)
        self.assertIsNone(counters)

    def test_a_summary_missing_just_checkpoint_fails_is_unreadable_when_required(self):
        """Each of the three checkpoint lines packs several counters into one
        regex (see CHECKPOINT_COUNTERS), so a line either supplies all of its
        counters or none of them -- dropping just "Checkpoint fails" must
        still make the whole replay unreadable, not just those three counters
        silently absent from the total."""
        text = "\n".join(
            l for l in CLEAN_WITH_CHECKPOINTS.splitlines()
            if "Checkpoint fails" not in l)
        counters, err = guard.read_counters(text, 0, require_checkpoints=True)
        self.assertIsNone(counters)
        self.assertIn("Checkpoint fails", err)

    def test_a_summary_missing_just_the_overlay_line_is_unreadable_when_required(self):
        text = "\n".join(
            l for l in CLEAN_WITH_CHECKPOINTS.splitlines()
            if not l.strip().startswith("Overlay:"))
        counters, err = guard.read_counters(text, 0, require_checkpoints=True)
        self.assertIsNone(counters)

    def test_a_summary_missing_just_checkpoint_blobs_is_unreadable_when_required(self):
        text = "\n".join(
            l for l in CLEAN_WITH_CHECKPOINTS.splitlines()
            if "Checkpoint blobs" not in l)
        counters, err = guard.read_counters(text, 0, require_checkpoints=True)
        self.assertIsNone(counters)
        # The message must name the missing line itself. This used to also
        # assert "Checkpoint fails" was in `err`, which only the log tail
        # appended to the message satisfied -- that line was the fixture's
        # last -- so it tested the fixture's order, not the parse.
        self.assertTrue(err.startswith("no Checkpoint blobs ... decoded counter"),
                        err)


#: `crates/vrfkit/src/driver/summary.rs` -- read, not copied. See
#: `SummaryFormatDriftTests`.
SUMMARY_RS = (
    Path(__file__).resolve().parents[2]
    / "crates" / "vrfkit" / "src" / "driver" / "summary.rs"
)


def _overlay_format_string(source: str) -> str:
    """The checkpoint `Overlay:` format string as summary.rs literally spells it.

    Anchored on `Overlay:` inside a quoted Rust literal. The main pass prints
    its overlay counters one-per-line and has no such literal, so this is
    unambiguous -- but the count is asserted rather than assumed, because a
    second matching literal would make "the format string" a coin toss.
    """
    matches = re.findall(r'"(  Overlay:[^"]*)"', source)
    if len(matches) != 1:
        raise AssertionError(
            f"expected exactly one quoted '  Overlay:' format string in "
            f"{SUMMARY_RS.name}, found {len(matches)}: {matches!r}"
        )
    return matches[0]


class SummaryFormatDriftTests(unittest.TestCase):
    """The regex is pinned against the REAL format string, read off summary.rs.

    This is the defect that made the test necessary, not a hypothetical. The
    Rust side grew a seventh field -- `conflicts` -- and `CHECKPOINT_OVERLAY`
    still asked for six, so it matched NOTHING on a live `--checkpoints` run.
    The fixture in this very file pinned the stale six-field format, so the
    suite stayed green while the check it guards could not run at all.

    A hand-copied fixture cannot catch that: it drifts in exactly the same
    step as the regex. Reading summary.rs means the NEXT field added there
    turns this red, which is the only version of this test that works.
    """

    def setUp(self):
        if not SUMMARY_RS.is_file():
            self.fail(f"{SUMMARY_RS} is missing: this test cannot be vacuous")
        self.source = SUMMARY_RS.read_text(encoding="utf-8")

    def test_the_regex_matches_the_line_summary_rs_actually_prints(self):
        """Render summary.rs's own format string and match the regex on it.

        Each `{}` becomes a distinct number, so a regex that matched the line
        while mis-assigning its groups fails here too, not just one that fails
        to match at all.
        """
        fmt = _overlay_format_string(self.source)
        placeholders = fmt.count("{}")
        values = [str(11 * (n + 1)) for n in range(placeholders)]
        rendered = fmt
        for value in values:
            rendered = rendered.replace("{}", value, 1)

        match = guard.CHECKPOINT_OVERLAY.search(rendered)
        self.assertIsNotNone(
            match,
            f"CHECKPOINT_OVERLAY does not match the line summary.rs prints.\n"
            f"  summary.rs: {fmt}\n"
            f"  rendered  : {rendered}\n"
            f"  regex     : {guard.CHECKPOINT_OVERLAY.pattern}\n"
            f"A field was added to or removed from the Rust format string. "
            f"Update CHECKPOINT_OVERLAY and CHECKPOINT_COUNTERS' group numbers "
            f"and labels together.",
        )
        self.assertEqual(
            list(match.groups()), values,
            "CHECKPOINT_OVERLAY matched but its capture groups are in the "
            "wrong order or the wrong count for summary.rs's field order",
        )

    def test_every_field_summary_rs_prints_is_a_named_counter(self):
        """A captured group nothing names is a counter that reaches no total.

        The group count and the CHECKPOINT_COUNTERS entries pointing at this
        pattern must agree, or a field is parsed and then dropped -- read but
        never summed, never required, never printed.
        """
        fmt = _overlay_format_string(self.source)
        named = [
            key for key, pattern, _group, _label in guard.CHECKPOINT_COUNTERS
            if pattern is guard.CHECKPOINT_OVERLAY
        ]
        self.assertEqual(
            len(named), fmt.count("{}"),
            f"summary.rs prints {fmt.count('{}')} overlay fields but "
            f"CHECKPOINT_COUNTERS names {len(named)} of them: {named}",
        )
        groups = sorted(
            group for _key, pattern, group, _label in guard.CHECKPOINT_COUNTERS
            if pattern is guard.CHECKPOINT_OVERLAY
        )
        self.assertEqual(
            groups, list(range(1, fmt.count("{}") + 1)),
            "the overlay capture groups CHECKPOINT_COUNTERS reads are not "
            f"exactly 1..{fmt.count('{}')}: {groups}",
        )

    def test_the_fixture_in_this_file_matches_summary_rs_field_count(self):
        """CLEAN_WITH_CHECKPOINTS is the fixture that drifted. Pin it too.

        The stale fixture is what let the suite stay green: it described a
        six-field line the Rust side had stopped printing, so every test built
        on it agreed with the broken regex.
        """
        fmt = _overlay_format_string(self.source)
        fixture = [
            line for line in CLEAN_WITH_CHECKPOINTS.splitlines()
            if line.strip().startswith("Overlay:")
        ]
        self.assertEqual(len(fixture), 1, fixture)
        self.assertEqual(
            fixture[0].count(" / ") + 1, fmt.count("{}"),
            f"the fixture's Overlay line has a different field count from "
            f"summary.rs:\n  fixture   : {fixture[0]}\n  summary.rs: {fmt}",
        )


class DeadCheckpointCounterTests(unittest.TestCase):
    """Mirrors DeadCounterTests for the checkpoint pass: a corpus where the
    checkpoint decoders never ran must not read as a clean checkpoint sweep.
    """

    WORKING = {"checkpoint_decoded": 500, "checkpoint_blobs_decoded": 8,
               "checkpoint_array_elements": 44652,
               "checkpoint_array_fields": 364594}

    #: `(work counter, label, a failure label it must say is vacuous)`.
    CASES = (
        ("checkpoint_decoded", "Overlay ... decoded (checkpoint)",
         "Checkpoint overlay errors"),
        ("checkpoint_blobs_decoded", "Checkpoint blobs ... decoded",
         "Checkpoint blobs failed"),
        ("checkpoint_array_elements", "Checkpoint array ... elements",
         "Checkpoint fails array"),
        ("checkpoint_array_fields", "Checkpoint array ... fields",
         "Checkpoint leaf errors"),
    )

    @staticmethod
    def dead(totals):
        return guard.dead_counters(totals, guard.CHECKPOINT_MUST_MOVE,
                                   " (with --checkpoints)")

    def test_a_working_checkpoint_corpus_has_no_dead_counters(self):
        self.assertEqual(self.dead(self.WORKING), [])

    def test_a_corpus_where_no_checkpoint_field_decoded_is_not_a_pass(self):
        totals = dict(self.WORKING, checkpoint_decoded=0)
        dead = self.dead(totals)
        self.assertTrue(dead)

    def test_a_corpus_where_no_checkpoint_blob_decoded_is_not_a_pass(self):
        totals = dict(self.WORKING, checkpoint_blobs_decoded=0)
        dead = self.dead(totals)
        self.assertTrue(dead)

    def test_each_checkpoint_work_counter_that_stays_at_zero_is_named_alone(self):
        for key, label, gate in self.CASES:
            with self.subTest(counter=key):
                dead = self.dead(dict(self.WORKING, **{key: 0}))
                self.assertEqual(len(dead), 1, dead)
                self.assertTrue(dead[0].startswith(f"{label} totalled 0"), dead)
                self.assertIn(gate, dead[0])

    def test_a_checkpoint_pass_that_decoded_nothing_fails_on_every_one(self):
        totals = {key: 0 for key in self.WORKING}
        self.assertEqual(len(self.dead(totals)),
                         len(self.CASES))


#: `(counter key, label its line starts with, replacement line, value)` for
#: every main-pass counter that must fail the replay it is nonzero on. Each
#: value is distinct, so a regex that read the wrong field of a shared line
#: would report the wrong number rather than pass.
MAIN_FAILURE_CASES = (
    ("decode_errors", "Decode errors:", "Decode errors:     11", 11),
    ("struct_blobs_failed", "Struct blobs:",
     "Struct blobs:      63 decoded / 10 failed", 10),
    ("movement_errors", "Movement errors:", "Movement errors:   5", 5),
    ("array_errors", "Array decode:",
     "Array decode:      25052 elements / 96076 fields / 9 errors / 0 truncations", 9),
    ("array_truncations", "Array decode:",
     "Array decode:      25052 elements / 96076 fields / 0 errors / 8 truncations", 8),
    ("array_root_bits", "Array residual:",
     "Array residual:    3 root bits / 0 nested bits / 0 implicit ends", 3),
    ("array_nested_bits", "Array residual:",
     "Array residual:    0 root bits / 2 nested bits / 0 implicit ends", 2),
    ("array_implicit_ends", "Array residual:",
     "Array residual:    0 root bits / 0 nested bits / 1 implicit ends", 1),
    ("array_leaf_errors", "Array leaf errs:", "Array leaf errs:   7", 7),
    ("truncated_rpcs", "Truncated RPCs:", "Truncated RPCs:    6", 6),
    ("cnc_bruteforce_unwalked", "CNC brute force:",
     "CNC brute force:   454 attempted / 21 unwalked", 21),
    ("movement_sized_tails", "Movement tails:",
     "Movement tails:    22 sized (40 bits) / 0 open (0 bits)", 22),
    ("movement_open_tails", "Movement tails:",
     "Movement tails:    0 sized (0 bits) / 23 open (50 bits)", 23),
)

#: The same for the checkpoint pass, applied to `CLEAN_WITH_CHECKPOINTS`.
CHECKPOINT_FAILURE_CASES = (
    ("checkpoint_errors", "Overlay:",
     "  Overlay:          500 decoded / 12 errors / 20 raw-skip / 5 not-in-table "
     "/ 2 unnamed / 4 conflicts / 1 effect blobs", 12),
    ("checkpoint_blobs_failed", "Checkpoint blobs:",
     "  Checkpoint blobs: 8 decoded / 13 failed", 13),
    ("checkpoint_fail_array", "Checkpoint fails:",
     "  Checkpoint fails: 14 array / 0 truncated RPC / 0 movement", 14),
    ("checkpoint_fail_truncated_rpc", "Checkpoint fails:",
     "  Checkpoint fails: 0 array / 15 truncated RPC / 0 movement", 15),
    ("checkpoint_fail_movement", "Checkpoint fails:",
     "  Checkpoint fails: 0 array / 0 truncated RPC / 16 movement", 16),
    ("checkpoint_array_truncations", "Checkpoint array:",
     "  Checkpoint array: 44652 elements / 364594 fields / 3 truncations / "
     "0 root bits / 0 nested bits / 0 implicit ends", 3),
    ("checkpoint_array_root_bits", "Checkpoint array:",
     "  Checkpoint array: 44652 elements / 364594 fields / 0 truncations / "
     "17 root bits / 0 nested bits / 0 implicit ends", 17),
    ("checkpoint_array_nested_bits", "Checkpoint array:",
     "  Checkpoint array: 44652 elements / 364594 fields / 0 truncations / "
     "0 root bits / 18 nested bits / 0 implicit ends", 18),
    ("checkpoint_array_implicit_ends", "Checkpoint array:",
     "  Checkpoint array: 44652 elements / 364594 fields / 0 truncations / "
     "0 root bits / 0 nested bits / 19 implicit ends", 19),
    ("checkpoint_leaf_errors", "Checkpoint leaf:",
     "  Checkpoint leaf:  4 typed decode errors", 4),
    ("checkpoint_cnc_bruteforce_unwalked", "Checkpoint CNC brute force:",
     "  Checkpoint CNC brute force: 3 attempted / 24 unwalked", 24),
    ("checkpoint_movement_sized_tails", "Checkpoint movement tails:",
     "  Checkpoint movement tails: 25 sized (60 bits) / 0 open (0 bits)", 25),
    ("checkpoint_movement_open_tails", "Checkpoint movement tails:",
     "  Checkpoint movement tails: 0 sized (0 bits) / 26 open (70 bits)", 26),
)


class SinkFailureTests(unittest.TestCase):
    """Every failure counter summary.rs prints must fail the replay it is
    nonzero on.

    The main pass's `Array decode` / `Array residual` / `Array leaf errs` /
    `Truncated RPCs` / `Movement errors` lines and the checkpoint pass's
    `Checkpoint array` truncation/residual and `Checkpoint leaf` lines were
    printed on every export and never read: a replay carrying seven leaf errors
    read the same as a clean one, and the run printed OK. verify_build_corpus.py
    already required every one of them to be zero, so the two tools disagreed
    about what a failure is.
    """

    def test_a_clean_summary_has_no_failures(self):
        counters, err = guard.read_counters(CLEAN, 0)
        self.assertEqual(err, "")
        self.assertEqual(guard.replay_failures(counters, checkpoints=False), [])
        counters, err = guard.read_counters(
            CLEAN_WITH_CHECKPOINTS, 0, require_checkpoints=True)
        self.assertEqual(err, "")
        self.assertEqual(guard.replay_failures(counters, checkpoints=True), [])

    def test_each_main_pass_failure_counter_fails_the_replay(self):
        for key, label, line, value in MAIN_FAILURE_CASES:
            with self.subTest(counter=key):
                counters, err = guard.read_counters(
                    replace_line(CLEAN, label, line), 0)
                self.assertEqual(err, "")
                self.assertEqual(
                    guard.replay_failures(counters, checkpoints=False),
                    [(key, value)])

    def test_each_checkpoint_failure_counter_fails_the_replay(self):
        for key, label, line, value in CHECKPOINT_FAILURE_CASES:
            with self.subTest(counter=key):
                counters, err = guard.read_counters(
                    replace_line(CLEAN_WITH_CHECKPOINTS, label, line), 0,
                    require_checkpoints=True)
                self.assertEqual(err, "")
                self.assertEqual(
                    guard.replay_failures(counters, checkpoints=True),
                    [(key, value)])

    def test_a_handle_conflict_is_read_but_is_not_a_failure(self):
        """The docstring's one deliberate exemption: a conflict is the overlay
        refusing to type a renamed handle, which is the protection working."""
        text = replace_line(
            CLEAN_WITH_CHECKPOINTS, "Overlay:",
            "  Overlay:          500 decoded / 0 errors / 20 raw-skip / "
            "5 not-in-table / 2 unnamed / 99 conflicts / 1 effect blobs")
        counters, err = guard.read_counters(text, 0, require_checkpoints=True)
        self.assertEqual(err, "")
        self.assertEqual(counters["checkpoint_conflicts"], 99)
        self.assertEqual(guard.replay_failures(counters, checkpoints=True), [])

    def test_an_absent_failure_counter_is_a_loud_error_not_a_silent_zero(self):
        """Indexed, never `.get(key, 0)`: an absent counter must not gate as 0.
        `read_counters` requires every counter, which keeps this from
        firing on a real run."""
        counters, _ = guard.read_counters(CLEAN, 0)
        del counters["array_leaf_errors"]
        with self.assertRaises(KeyError):
            guard.replay_failures(counters, checkpoints=False)

    def test_every_new_main_pass_line_is_required(self):
        for label in ("Movement errors:", "Array decode:", "Array residual:",
                      "Array leaf errs:", "Truncated RPCs:", "CNC brute force:",
                      "Movement tails:"):
            with self.subTest(label=label):
                counters, err = guard.read_counters(drop_line(CLEAN, label), 0)
                self.assertIsNone(counters)
                self.assertIn(f"no {label[:-1]}", err)

    def test_every_new_checkpoint_line_is_required(self):
        for label in ("Checkpoint array:", "Checkpoint leaf:",
                      "Checkpoint CNC brute force:", "Checkpoint movement tails:"):
            with self.subTest(label=label):
                counters, err = guard.read_counters(
                    drop_line(CLEAN_WITH_CHECKPOINTS, label), 0,
                    require_checkpoints=True)
                self.assertIsNone(counters)
                self.assertIn(f"no {label[:-1]}", err)

    def test_a_label_quoted_inside_a_diagnostic_line_is_not_read(self):
        """`Struct blob err:` and its siblings print free text. A counter label
        inside one, ahead of the real line, must not be read in its place --
        here that would read the quoted 0 and pass a replay with 7 leaf
        errors."""
        text = replace_line(CLEAN, "Array leaf errs:", "Array leaf errs:   7")
        text = replace_line(
            text, "Struct blobs:",
            "Struct blobs:      63 decoded / 0 failed\n"
            "Struct blob err:  decode stopped near Array leaf errs: 0")
        counters, err = guard.read_counters(text, 0)
        self.assertEqual(err, "")
        self.assertEqual(guard.replay_failures(counters, checkpoints=False),
                         [("array_leaf_errors", 7)])

    def test_a_line_from_the_other_pass_cannot_stand_in(self):
        """`Checkpoint movement:`, `truncated RPC` and `Checkpoint array:` sit
        in the same log as the main-pass lines they resemble. Dropping the
        main-pass line must still make the replay unreadable, and dropping
        `Checkpoint array:` must not be satisfied by `Array decode:`."""
        for label in ("Movement errors:", "Truncated RPCs:", "Array decode:",
                      "Checkpoint array:", "CNC brute force:", "Movement tails:",
                      "Checkpoint CNC brute force:", "Checkpoint movement tails:"):
            with self.subTest(label=label):
                counters, err = guard.read_counters(
                    drop_line(CLEAN_WITH_CHECKPOINTS, label), 0,
                    require_checkpoints=True)
                self.assertIsNone(counters)
                self.assertIn(f"no {label[:-1]}", err)


#: `(counter key, summary.rs label, which `{}` of that label's format string
#: the counter reads)`. Declared here rather than derived from the guard's
#: regexes, so a regex that reads the wrong field of its line is caught
#: instead of restated.
SINK_FORMATS = (
    ("movement_rows", "Movement rows:", 0),
    ("movement_errors", "Movement errors:", 0),
    ("array_elements", "Array decode:", 0),
    ("array_fields", "Array decode:", 1),
    ("array_errors", "Array decode:", 2),
    ("array_truncations", "Array decode:", 3),
    ("array_root_bits", "Array residual:", 0),
    ("array_nested_bits", "Array residual:", 1),
    ("array_implicit_ends", "Array residual:", 2),
    ("array_leaf_errors", "Array leaf errs:", 0),
    ("truncated_rpcs", "Truncated RPCs:", 0),
    ("cnc_bruteforce_attempted", "CNC brute force:", 0),
    ("cnc_bruteforce_unwalked", "CNC brute force:", 1),
    ("movement_sized_tails", "Movement tails:", 0),
    ("movement_sized_tail_bits", "Movement tails:", 1),
    ("movement_open_tails", "Movement tails:", 2),
    ("movement_open_tail_bits", "Movement tails:", 3),
    ("checkpoint_array_elements", "Checkpoint array:", 0),
    ("checkpoint_array_fields", "Checkpoint array:", 1),
    ("checkpoint_array_truncations", "Checkpoint array:", 2),
    ("checkpoint_array_root_bits", "Checkpoint array:", 3),
    ("checkpoint_array_nested_bits", "Checkpoint array:", 4),
    ("checkpoint_array_implicit_ends", "Checkpoint array:", 5),
    ("checkpoint_leaf_errors", "Checkpoint leaf:", 0),
    ("checkpoint_cnc_bruteforce_attempted", "Checkpoint CNC brute force:", 0),
    ("checkpoint_cnc_bruteforce_unwalked", "Checkpoint CNC brute force:", 1),
    ("checkpoint_movement_sized_tails", "Checkpoint movement tails:", 0),
    ("checkpoint_movement_sized_tail_bits", "Checkpoint movement tails:", 1),
    ("checkpoint_movement_open_tails", "Checkpoint movement tails:", 2),
    ("checkpoint_movement_open_tail_bits", "Checkpoint movement tails:", 3),
)


def _format_string(source: str, label: str) -> str:
    """The one quoted summary.rs literal that prints `label`; see
    `_overlay_format_string` for why the count is asserted."""
    matches = re.findall(r'"(  ' + re.escape(label) + r'[^"]*)"', source)
    if len(matches) != 1:
        raise AssertionError(
            f"expected exactly one quoted '  {label}' format string in "
            f"{SUMMARY_RS.name}, found {len(matches)}: {matches!r}")
    return matches[0]


class SinkFormatDriftTests(unittest.TestCase):
    """The sink regexes, pinned against the format strings summary.rs prints.

    Same reasoning as `SummaryFormatDriftTests`: a fixture in this file drifts
    in the same step as the regex, so only the Rust literal can catch a label
    or field change. A regex that stopped matching makes every replay
    unreadable, since `read_counters` requires every counter it parses.
    """

    def setUp(self):
        if not SUMMARY_RS.is_file():
            self.fail(f"{SUMMARY_RS} is missing: this test cannot be vacuous")
        self.source = SUMMARY_RS.read_text(encoding="utf-8")

    def test_each_regex_reads_its_own_field_of_the_printed_line(self):
        main = {key: pattern for key, pattern, _label in guard.COUNTERS}
        checkpoint = {key: (pattern, group)
                      for key, pattern, group, _label in guard.CHECKPOINT_COUNTERS}
        for key, label, index in SINK_FORMATS:
            with self.subTest(counter=key):
                fmt = _format_string(self.source, label)
                values = [str(11 * (n + 1)) for n in range(fmt.count("{}"))]
                rendered = fmt
                for value in values:
                    rendered = rendered.replace("{}", value, 1)
                pattern, group = (main[key], 1) if key in main else checkpoint[key]
                match = pattern.search(rendered)
                self.assertIsNotNone(
                    match, f"{key}: {pattern.pattern} does not match what "
                           f"summary.rs prints: {rendered!r}")
                self.assertEqual(
                    match.group(group), values[index],
                    f"{key} reads the wrong field of {rendered!r}")

    def test_every_field_of_these_lines_is_a_named_counter(self):
        """A printed field nothing names reaches no total and no gate."""
        named: dict[str, set[int]] = {}
        for _key, label, index in SINK_FORMATS:
            named.setdefault(label, set()).add(index)
        for label, indices in named.items():
            with self.subTest(label=label):
                fmt = _format_string(self.source, label)
                self.assertEqual(indices, set(range(fmt.count("{}"))))


#: tools/verify_build_corpus.py -- read, not imported, so this module keeps
#: needing nothing beyond the standard library.
VERIFY_BUILD_CORPUS = Path(__file__).resolve().parents[1] / "verify_build_corpus.py"

#: verify_build_corpus.py's SINK_ZERO (manifest counters it requires to be zero
#: on both passes), mapped to the main-pass and checkpoint counters this gate
#: reads off the summary for the same quantity.
SINK_ZERO_EQUIVALENTS = {
    "overlay_decoded_err": ("decode_errors", "checkpoint_errors"),
    "struct_blobs_failed": ("struct_blobs_failed", "checkpoint_blobs_failed"),
    "movement_rpc_errors": ("movement_errors", "checkpoint_fail_movement"),
    "array_errors": ("array_errors", "checkpoint_fail_array"),
    "array_truncations": ("array_truncations", "checkpoint_array_truncations"),
    "array_unconsumed_root_bits": ("array_root_bits", "checkpoint_array_root_bits"),
    "array_unconsumed_nested_bits": ("array_nested_bits",
                                     "checkpoint_array_nested_bits"),
    "array_implicit_terminations": ("array_implicit_ends",
                                    "checkpoint_array_implicit_ends"),
    "array_leaf_decode_errors": ("array_leaf_errors", "checkpoint_leaf_errors"),
    "truncated_rpcs": ("truncated_rpcs", "checkpoint_fail_truncated_rpc"),
    "cnc_bruteforce_payloads_unwalked": ("cnc_bruteforce_unwalked",
                                         "checkpoint_cnc_bruteforce_unwalked"),
    "movement_sized_section_tails": ("movement_sized_tails",
                                     "checkpoint_movement_sized_tails"),
    "movement_open_section_tails": ("movement_open_tails",
                                    "checkpoint_movement_open_tails"),
}


def _sink_zero() -> tuple[str, ...]:
    import ast
    tree = ast.parse(VERIFY_BUILD_CORPUS.read_text(encoding="utf-8"))
    for node in tree.body:
        if (isinstance(node, ast.Assign)
                and any(isinstance(t, ast.Name) and t.id == "SINK_ZERO"
                        for t in node.targets)):
            return tuple(ast.literal_eval(node.value))
    raise AssertionError(f"no SINK_ZERO assignment in {VERIFY_BUILD_CORPUS}")


class VerifyBuildCorpusAgreementTests(unittest.TestCase):
    """The two corpus tools must agree on what a failure is.

    verify_build_corpus.py gates SINK_ZERO on both passes; this gate used to
    gate two of those ten on the main pass. A counter added there must be
    placed here too, or this test names it.
    """

    def test_every_sink_zero_counter_has_an_equivalent_here(self):
        self.assertEqual(set(_sink_zero()), set(SINK_ZERO_EQUIVALENTS))

    def test_every_equivalent_fails_the_replay_in_its_pass(self):
        base, err = guard.read_counters(
            CLEAN_WITH_CHECKPOINTS, 0, require_checkpoints=True)
        self.assertEqual(err, "")
        for sink_key, keys in SINK_ZERO_EQUIVALENTS.items():
            for key in keys:
                with self.subTest(sink_zero=sink_key, counter=key):
                    counters = dict(base)
                    counters[key] = 1
                    self.assertEqual(
                        guard.replay_failures(counters, checkpoints=True),
                        [(key, 1)])


class LivenessCoverageTests(unittest.TestCase):
    """Every failure gate is backed by a work counter that must move, or is
    declared unbacked with a reason -- never neither.

    The gate that made this necessary: the array, leaf and movement failure
    counters were added to FAILURES with their work counters parsed, required
    and printed but never required to MOVE, so an array walker that was never
    reached passed as "0 array failures" -- the Decoded OK / struct blob hole
    this module had already closed once. A failure counter added to FAILURES
    without a work counter, or without saying why it has none, turns this red.
    """

    @property
    def PASSES(self):
        # Read at test time, not class creation: a missing table must fail
        # these tests, not take the whole module down at import.
        return (
            ("main", guard.FAILURES, guard.MUST_MOVE, guard.UNBACKED,
             guard.COUNTERS),
            ("checkpoint", guard.CHECKPOINT_FAILURES, guard.CHECKPOINT_MUST_MOVE,
             guard.CHECKPOINT_UNBACKED, guard.CHECKPOINT_COUNTERS),
        )

    def test_every_failure_gate_is_backed_or_declared_unbacked_exactly_once(self):
        for name, failures, must_move, unbacked, _required in self.PASSES:
            with self.subTest(pass_=name):
                claimed = [gate for _key, _label, gates in must_move for gate in gates]
                claimed += [key for key, _why in unbacked]
                self.assertEqual(sorted(claimed),
                                 sorted(key for key, _label in failures))

    def test_every_work_counter_is_required_and_is_not_itself_a_failure(self):
        """A work counter that could be absent would read as a dead zero or,
        defaulted, as work; one that is also a failure counter is gated both
        ways and backs nothing."""
        for name, failures, must_move, _unbacked, required in self.PASSES:
            failure_keys = {key for key, _label in failures}
            required_keys = {key for key, *_ in required}
            for key, _label, gates in must_move:
                with self.subTest(pass_=name, work=key):
                    self.assertIn(key, required_keys)
                    self.assertNotIn(key, failure_keys)
                    self.assertTrue(gates, f"{key} backs no gate")

    def test_every_unbacked_gate_says_why(self):
        for name, _failures, _must_move, unbacked, _required in self.PASSES:
            for key, why in unbacked:
                with self.subTest(pass_=name, gate=key):
                    self.assertTrue(why.strip())


#: Stand-in for `vrfkit.exe`, playing the part `_export_one` expects --
#: `[str(exe), "export", str(replay), "--out", str(out)]`. Run under
#: `sys.executable`, the "export" token becomes the script Python executes (the
#: same trick `test_check_export_baseline.py` uses for its fake `export`), so a
#: file literally named `export` in the process's cwd stands in for the real
#: binary. Every helper above (`read_counters`, `dead_counters`, `reconcile`)
#: is proven correct on synthetic text; none of that proves `main()` actually
#: calls them and acts on what they return -- which is exactly the shape of
#: this file's own recorded defect ("OK: every replay reported Decode errors:
#: 0" printed over an exporter that never ran). These tests are that call.
FAKE_EXPORT_SCRIPT = '''\
import sys
from pathlib import Path

argv = sys.argv
replay = Path(argv[1])
out = Path(argv[argv.index("--out") + 1])
out.mkdir(parents=True, exist_ok=True)
name = replay.name

# The main-pass sink lines every export prints, failure counters at zero.
SINK = """
Movement rows:     100
Movement errors:   0
Array decode:      10 elements / 40 fields / 0 errors / 0 truncations
Array residual:    0 root bits / 0 nested bits / 0 implicit ends
Array leaf errs:   0
Truncated RPCs:    0
CNC brute force:   4 attempted / 0 unwalked
Movement tails:    0 sized (0 bits) / 0 open (0 bits)
"""

# Printed only when the gate passed --checkpoints on, as vrfkit does.
CHECKPOINTS = """
=== Checkpoints ===
  Overlay:          500 decoded / 0 errors / 20 raw-skip / 5 not-in-table / 2 unnamed / 4 conflicts / 1 effect blobs
  Checkpoint blobs: 8 decoded / 0 failed
  Checkpoint fails: 0 array / 0 truncated RPC / 0 movement
  Checkpoint array: 30 elements / 90 fields / 0 truncations / 0 root bits / 0 nested bits / 0 implicit ends
  Checkpoint leaf:  0 typed decode errors
  Checkpoint reward opaque: 0 empty variants
  Checkpoint movement tails: 0 sized (0 bits) / 0 open (0 bits)
  Checkpoint CNC brute force: 0 attempted / 0 unwalked
"""

CLEAN = """
Rows offered:      100
Decoded OK:        90
Decode errors:     0
Raw/Skip:          5
Not in table:      3
No field name:     2
Struct blobs:      5 decoded / 0 failed
Reward opaque:     0 empty variants
"""


def emit(summary, sink=SINK, checkpoints=CHECKPOINTS):
    print(summary + sink + (checkpoints if "--checkpoints" in argv else ""))
    raise SystemExit(0)


if "badexit" in name:
    print("exporter crashed", file=sys.stderr)
    raise SystemExit(9)

if "nothingran" in name:
    # The 13.02 shape one level down: every counter a legitimate zero.
    emit("""
Rows offered:      0
Decoded OK:        0
Decode errors:     0
Raw/Skip:          0
Not in table:      0
No field name:     0
Struct blobs:      0 decoded / 0 failed
Reward opaque:     0 empty variants
""", sink=SINK.replace("10 elements / 40 fields", "0 elements / 0 fields"))

if "decodeerr" in name:
    emit("""
Rows offered:      100
Decoded OK:        90
Decode errors:     10
Raw/Skip:          0
Not in table:      0
No field name:     0
Struct blobs:      5 decoded / 0 failed
Reward opaque:     0 empty variants
""")

if "blobfail" in name:
    emit("""
Rows offered:      100
Decoded OK:        95
Decode errors:     0
Raw/Skip:          3
Not in table:      2
No field name:     0
Struct blobs:      5 decoded / 1 failed
Reward opaque:     0 empty variants
""")

if "missingcounter" in name:
    # "No field name" omitted entirely -- must not read as 0.
    emit("""
Rows offered:      100
Decoded OK:        100
Decode errors:     0
Raw/Skip:          0
Not in table:      0
Struct blobs:      5 decoded / 0 failed
Reward opaque:     0 empty variants
""")

if "mismatch" in name:
    # Every REQUIRED counter present, but the five categories that make up
    # "Rows offered" undercount it by one -- summary.rs grew a sixth category
    # this tool does not know to parse yet.
    emit("""
Rows offered:      100
Decoded OK:        90
Decode errors:     0
Raw/Skip:          5
Not in table:      3
No field name:     1
Struct blobs:      5 decoded / 0 failed
Reward opaque:     0 empty variants
""")

if "arrayleaf" in name:
    # Every counter the gate used to read is clean; only the leaf line is not.
    emit(CLEAN, sink=SINK.replace("Array leaf errs:   0", "Array leaf errs:   7"))

if "cpleaf" in name:
    emit(CLEAN, checkpoints=CHECKPOINTS.replace(
        "Checkpoint leaf:  0", "Checkpoint leaf:  4"))

if "cncunwalked" in name:
    emit(CLEAN, sink=SINK.replace("4 attempted / 0 unwalked", "4 attempted / 2 unwalked"))

if "cptails" in name:
    emit(CLEAN, checkpoints=CHECKPOINTS.replace(
        "Checkpoint movement tails: 0 sized (0 bits)",
        "Checkpoint movement tails: 3 sized (9 bits)"))
# The array walker never reached: Decoded OK, the struct blobs and every
# failure counter are exactly what a good run prints. The checkpoint names are
# tested first because they contain the main-pass ones.
if "cpnoarray" in name:
    emit(CLEAN, checkpoints=CHECKPOINTS.replace(
        "30 elements / 90 fields", "0 elements / 0 fields"))

if "cpnofields" in name:
    emit(CLEAN, checkpoints=CHECKPOINTS.replace(
        "30 elements / 90 fields", "30 elements / 0 fields"))

if "noarray" in name:
    emit(CLEAN, sink=SINK.replace("10 elements / 40 fields", "0 elements / 0 fields"))

if "nofields" in name:
    emit(CLEAN, sink=SINK.replace("10 elements / 40 fields", "10 elements / 0 fields"))

if "nomovement" in name:
    emit(CLEAN, sink=SINK.replace("Movement rows:     100", "Movement rows:     0"))

emit(CLEAN)
'''


class MainWiringTests(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.root = Path(self._tmp.name)
        (self.root / "export").write_text(FAKE_EXPORT_SCRIPT, encoding="utf-8")
        self.corpus = self.root / "corpus"
        self.corpus.mkdir()
        self._previous_cwd = Path.cwd()
        os.chdir(self.root)
        self.addCleanup(os.chdir, self._previous_cwd)
        self._argv = sys.argv

    def make_replay(self, name: str) -> None:
        (self.corpus / name).write_bytes(b"not a real replay")

    def run_main(self, extra_args=()):
        argv = [sys.executable, str(self.corpus), "--jobs", "1", *extra_args]
        sys.argv = ["check_decode_errors_corpus.py", *argv]
        out = io.StringIO()
        try:
            with contextlib.redirect_stdout(out), contextlib.redirect_stderr(out):
                code = guard.main()
        finally:
            sys.argv = self._argv
        return code, out.getvalue()

    def test_a_clean_corpus_exits_zero(self):
        self.make_replay("a.vrf")
        code, output = self.run_main()
        self.assertEqual(code, 0, output)
        self.assertIn("OK:", output)

    def test_decode_errors_fail_the_run(self):
        """Asserted on the failure block's line, not a bare "decode errors":
        the unconditional totals line prints that phrase on every run, so it
        could not tell a failure from a pass."""
        self.make_replay("decodeerr.vrf")
        code, output = self.run_main()
        self.assertEqual(code, 1, output)
        self.assertIn("decodeerr.vrf: Decode errors=10", output)

    def test_struct_blob_failures_fail_the_run(self):
        self.make_replay("blobfail.vrf")
        code, output = self.run_main()
        self.assertEqual(code, 1, output)
        self.assertIn("blobfail.vrf: Struct blobs failed=1", output)
        self.assertIn("Struct blob err:", output)

    def test_array_leaf_errors_fail_the_run(self):
        """The shape that used to print OK: every counter the gate read was
        clean and `Array leaf errs: 7` sat unread beside them."""
        self.make_replay("arrayleaf.vrf")
        code, output = self.run_main()
        self.assertEqual(code, 1, output)
        self.assertIn("arrayleaf.vrf: Array leaf errs=7", output)
        self.assertNotIn("OK:", output)

    def test_checkpoint_leaf_errors_fail_a_checkpoint_run(self):
        self.make_replay("cpleaf.vrf")
        code, output = self.run_main(["--checkpoints"])
        self.assertEqual(code, 1, output)
        self.assertIn("cpleaf.vrf: Checkpoint leaf errors=4", output)

    def test_unwalked_cnc_payloads_fail_the_run(self):
        """verify_build_corpus.py fails a replay on this counter; the gate
        reading the same export must not print OK over it."""
        self.make_replay("cncunwalked.vrf")
        code, output = self.run_main()
        self.assertEqual(code, 1, output)
        self.assertIn("cncunwalked.vrf: CNC brute force unwalked=2", output)
        self.assertNotIn("OK:", output)

    def test_checkpoint_movement_tails_fail_a_checkpoint_run(self):
        self.make_replay("cptails.vrf")
        code, output = self.run_main(["--checkpoints"])
        self.assertEqual(code, 1, output)
        self.assertIn("cptails.vrf: Checkpoint movement tails sized=3", output)

    def test_a_clean_checkpoint_run_prints_every_sink_total_with_its_zeros(self):
        """A line that appears only when nonzero cannot tell "nothing wrong"
        from "never read", so every new total prints on a clean run."""
        self.make_replay("a.vrf")
        code, output = self.run_main(["--checkpoints"])
        self.assertEqual(code, 0, output)
        for line in (
            "movement rows     : 100",
            "movement errors   : 0",
            "array decode      : 10 elements / 40 fields / 0 errors / 0 truncations",
            "array residual    : 0 root bits / 0 nested bits / 0 implicit ends",
            "array leaf errs   : 0",
            "truncated RPCs    : 0",
            "cnc brute force   : 4 attempted / 0 unwalked",
            "movement tails    : 0 sized (0 bits) / 0 open (0 bits)",
            "checkpoint array  : 30 elements / 90 fields / 0 truncations / "
            "0 root bits / 0 nested bits / 0 implicit ends",
            "checkpoint leaf   : 0 typed decode errors",
            "checkpoint cnc brute force: 0 attempted / 0 unwalked",
            "checkpoint movement tails: 0 sized (0 bits) / 0 open (0 bits)",
        ):
            with self.subTest(line=line):
                self.assertRegex(output, rf"(?m)^{re.escape(line)}$")

    def test_the_ok_line_separates_backed_gates_from_unbacked_ones(self):
        """Every run says which zero failure counters are evidence: the work
        that moved under them, and the gates no work counter backs -- never
        "0 on all 10" with the unbacked ones silently counted in."""
        self.make_replay("a.vrf")
        code, output = self.run_main(["--checkpoints"])
        self.assertEqual(code, 0, output)
        # `Movement rows` cannot vouch for the sized tails: only a sized
        # window counts one, and no measured replay has a sized window.
        self.assertRegex(
            output,
            r"(?m)^unbacked gates    : Truncated RPCs \(summary\.rs prints no "
            r"count of RPC parameter walks\); CNC brute force unwalked \(.+\); "
            r"Movement tails sized \(.+\)$")
        self.assertRegex(
            output,
            r"(?m)^checkpoint unbacked gates: Checkpoint fails movement \(.+\); "
            r"Checkpoint fails truncated RPC \(.+\); Checkpoint movement tails "
            r"sized \(.+\); Checkpoint movement tails open \(.+\); Checkpoint CNC "
            r"brute force unwalked \(.+\)$")
        ok = [line for line in output.splitlines() if line.startswith("OK:")]
        self.assertEqual(len(ok), 1, output)
        self.assertIn(
            "10 backed by work that moved (Decoded OK 90, Struct blobs ... "
            "decoded 5, Array decode ... elements 10, Array decode ... fields "
            "40, Movement rows 100), 3 with no work counter (Truncated RPCs, "
            "CNC brute force unwalked, Movement tails sized)",
            ok[0])
        self.assertIn(
            "8 backed by work that moved (Overlay ... decoded (checkpoint) 500, "
            "Checkpoint blobs ... decoded 8, Checkpoint array ... elements 30, "
            "Checkpoint array ... fields 90), 5 with no work counter "
            "(Checkpoint fails movement, Checkpoint fails truncated RPC, "
            "Checkpoint movement tails sized, Checkpoint movement tails open, "
            "Checkpoint CNC brute force unwalked)",
            ok[0])

    def test_an_array_walker_that_never_ran_fails_the_run(self):
        """The finding's shape: Decoded OK, the struct blobs and every failure
        counter read exactly as on a good run, and `Array decode: 0 elements /
        0 fields` sat printed and unread beside them. It used to print OK."""
        self.make_replay("noarray.vrf")
        code, output = self.run_main()
        self.assertEqual(code, 1, output)
        self.assertIn("Array decode ... elements totalled 0", output)
        self.assertIn("Array decode ... fields totalled 0", output)
        self.assertNotIn("OK:", output)

    def test_array_leaves_that_never_emitted_fail_the_run(self):
        self.make_replay("nofields.vrf")
        code, output = self.run_main()
        self.assertEqual(code, 1, output)
        self.assertIn("Array decode ... fields totalled 0", output)
        self.assertNotIn("Array decode ... elements totalled 0", output)

    def test_a_movement_decoder_that_never_ran_fails_the_run(self):
        self.make_replay("nomovement.vrf")
        code, output = self.run_main()
        self.assertEqual(code, 1, output)
        self.assertIn("Movement rows totalled 0", output)
        self.assertNotIn("OK:", output)

    def test_a_checkpoint_array_walker_that_never_ran_fails_a_checkpoint_run(self):
        self.make_replay("cpnoarray.vrf")
        code, output = self.run_main(["--checkpoints"])
        self.assertEqual(code, 1, output)
        self.assertIn("Checkpoint array ... elements totalled 0", output)
        self.assertIn("Checkpoint array ... fields totalled 0", output)
        self.assertNotIn("OK:", output)

    def test_checkpoint_array_leaves_that_never_emitted_fail_a_checkpoint_run(self):
        self.make_replay("cpnofields.vrf")
        code, output = self.run_main(["--checkpoints"])
        self.assertEqual(code, 1, output)
        self.assertIn("Checkpoint array ... fields totalled 0", output)
        self.assertNotIn("Checkpoint array ... elements totalled 0", output)

    def test_one_replay_that_did_the_work_keeps_the_corpus_alive(self):
        """Corpus totals, as the docstring says: a replay whose walker found
        no array beside one whose walker did is not a dead counter. (A
        per-replay rule would need every healthy replay to carry arrays;
        the 2026-09-28 audit measured that they all do, but the gate does not
        depend on it.)"""
        self.make_replay("a.vrf")
        self.make_replay("noarray.vrf")
        code, output = self.run_main()
        self.assertEqual(code, 0, output)

    def test_an_exporter_that_decoded_nothing_fails_the_run(self):
        """The 13.02 shape, one script down from the Rust regression: every
        counter is a legitimate zero, `Decode errors: 0` is vacuously true,
        and only `dead_counters` -- consulted by `main()` -- can catch it."""
        self.make_replay("nothingran.vrf")
        code, output = self.run_main()
        self.assertEqual(code, 1, output)
        self.assertIn("never moved", output)

    def test_a_missing_required_counter_fails_the_run(self):
        self.make_replay("missingcounter.vrf")
        code, output = self.run_main()
        self.assertEqual(code, 1, output)
        self.assertIn("did not report the counter", output)

    def test_a_nonzero_exporter_exit_fails_the_run(self):
        self.make_replay("badexit.vrf")
        code, output = self.run_main()
        self.assertEqual(code, 1, output)
        self.assertIn("did not report the counter", output)

    def test_a_reconciliation_mismatch_fails_the_run(self):
        self.make_replay("mismatch.vrf")
        code, output = self.run_main()
        self.assertEqual(code, 1, output)
        self.assertIn("do not reconcile", output)

    def test_no_vrf_files_is_a_controlled_failure(self):
        code, output = self.run_main()
        self.assertEqual(code, 2, output)


if __name__ == "__main__":
    unittest.main()
