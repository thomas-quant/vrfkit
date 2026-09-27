"""Guards for the export baseline pinner.

`measure` deliberately records a counter the summary did not print as None
rather than 0 -- its own comment says "a counter that silently reads as absent
is how this class of bug survives". `--update` then pinned the None anyway, so
a summary that STOPPED printing a counter matched the baseline from then on.

The cross-check already catches this for the four counters that are Parquet row
identities. The other twenty had nothing.
"""
import contextlib
import io
import json
import os
import re
import sys
import tempfile
import unittest
from types import SimpleNamespace
from unittest.mock import patch
from pathlib import Path

import pyarrow as pa
import pyarrow.parquet as pq

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import check_export_baseline as guard  # noqa: E402


def measurement(**overrides):
    counters = {key: 1 for key in guard.COUNTERS}
    counters.update(overrides)
    return {
        "counters": counters,
        "parquet": {name: {"rows": 1, "bytes": 100, "sha256": "a" * 64}
                    for name in guard.PARQUET_FILES},
    }


def checkpoint_measurement(**overrides):
    current = measurement()
    current["counters"].update({key: 1 for key in guard.CHECKPOINT_COUNTERS})
    current["counters"]["cp_literal_paths"] = 0
    current["parquet"].update({
        name: {"rows": 1, "bytes": 100, "sha256": "a" * 64}
        for name in guard.CHECKPOINT_PARQUET_FILES
    })
    current["counters"].update(overrides)
    return current


class UnpinnableTests(unittest.TestCase):
    def test_a_complete_summary_can_be_pinned(self):
        self.assertEqual(guard.unpinnable(measurement()), [])

    def test_a_counter_the_summary_did_not_print_cannot_be_pinned(self):
        reasons = guard.unpinnable(measurement(struct_blobs_decoded=None))
        self.assertTrue(reasons)
        self.assertIn("struct_blobs_decoded", " ".join(reasons))

    def test_every_absent_counter_is_named_not_just_the_first(self):
        reasons = guard.unpinnable(
            measurement(struct_blobs_decoded=None, effect_blobs_decoded=None))
        self.assertEqual(len(reasons), 2, reasons)

    def test_reward_opaque_counter_is_required_even_when_zero(self):
        reasons = guard.unpinnable(measurement(
            tracked_rewards_opaque_empty_variants=None))
        self.assertIn("tracked_rewards_opaque_empty_variants", " ".join(reasons))

    def test_the_checkpoint_counters_are_only_required_when_measured(self):
        """A default run never prints them, so their absence is not a fault.

        They live outside `COUNTERS` precisely so a default run does not record
        them as None and diff that against a `--checkpoints` baseline.
        """
        current = measurement()
        self.assertNotIn("cp_frames", current["counters"])
        self.assertEqual(guard.unpinnable(current), [])


class CrossCheckTests(unittest.TestCase):
    """Unchanged behaviour, pinned alongside the new refusal."""

    def test_partial_identity_includes_checkpoint_rows_only_when_present(self):
        current = measurement(partial_rows=2, cp_partial_rows=3)
        current["parquet"]["partials"]["rows"] = 5
        checks = guard.cross_check_identities(current["counters"], current["parquet"])
        self.assertIn(("Partial raw rows (main + checkpoint)", 5, 5), checks)
        current["parquet"]["partials"]["rows"] = 2
        self.assertTrue(any("Partial raw" in problem for problem in guard.cross_checks(current["counters"], current["parquet"])))

    def test_a_summary_disagreeing_with_its_parquet_is_a_lie(self):
        current = measurement(net_guid_rows=99)
        lies = guard.cross_checks(current["counters"], current["parquet"])
        self.assertTrue(any("NetGUID rows" in l for l in lies))

    def test_an_absent_identity_counter_is_itself_a_failure(self):
        current = measurement(movement_rows=None)
        lies = guard.cross_checks(current["counters"], current["parquet"])
        self.assertTrue(any("did not print it" in l for l in lies))

    def test_checkpoint_actor_and_guid_identities_reject_counter_mismatches(self):
        current = checkpoint_measurement(cp_actor_rows_written=2, cp_net_guid_rows_written=3,
                                         cp_block_rows_written=5)
        current["parquet"]["checkpoint_actors"]["rows"] = 1
        current["parquet"]["checkpoint_net_guids"]["rows"] = 4
        problems = guard.cross_checks(current["counters"], current["parquet"])
        self.assertTrue(any("Checkpoint actors" in p for p in problems), problems)
        self.assertTrue(any("Checkpoint GUID rows" in p for p in problems), problems)
        self.assertTrue(any("Checkpoint blocks" in p for p in problems), problems)

    def test_checkpoint_schema_rows_must_match_reader_and_writer_counts(self):
        for table, parsed, written in (
            ("checkpoint_guid_entries", "cp_guid_entries", "cp_guid_entry_rows_written"),
            ("checkpoint_export_groups", "cp_group_records", "cp_export_group_rows_written"),
            ("checkpoint_export_fields", "cp_exported_fields", "cp_export_field_rows_written"),
        ):
            with self.subTest(table=table):
                current = checkpoint_measurement(actor_closes=0, cp_partial_rows=0)
                self.assertEqual(guard.cross_checks(current["counters"], current["parquet"]), [])
                # A dropped row with a matching writer counter is still wrong.
                current["counters"][written] = 0
                current["parquet"][table]["rows"] = 0
                problems = guard.cross_checks(current["counters"], current["parquet"])
                self.assertEqual(len(problems), 1, problems)
                self.assertIn("parsed", problems[0])
                current["counters"][parsed] = 0
                if parsed == "cp_guid_entries":
                    current["counters"]["cp_indexed_paths"] = 0
                    current["counters"]["cp_resolved_path_indices"] = 0
                self.assertEqual(guard.cross_checks(current["counters"], current["parquet"]), [])
                current["counters"][written] = None
                self.assertIn("did not print", " ".join(guard.cross_checks(
                    current["counters"], current["parquet"])))

    def test_guid_writer_label_cannot_hide_missing_parser_count(self):
        summary = "  Checkpoint GUID entries: 42 rows\n"
        self.assertIsNone(re.search(guard.CHECKPOINT_COUNTERS["cp_guid_entries"], summary))
        summary += "  GUID entries: 43\n"
        self.assertEqual(re.search(guard.CHECKPOINT_COUNTERS["cp_guid_entries"], summary).group(1), "43")

    def test_guid_path_counter_regexes_are_exactly_anchored(self):
        summary = "  Checkpoint GUID paths: 1 literals / 2 indices / 2 resolved\n"
        for key in ("cp_literal_paths", "cp_indexed_paths", "cp_resolved_path_indices"):
            self.assertIsNone(re.search(guard.CHECKPOINT_COUNTERS[key], summary))
        summary = "  GUID paths: 1 literals / 2 indices / 2 resolved\n"
        self.assertEqual(re.search(guard.CHECKPOINT_COUNTERS["cp_literal_paths"], summary).group(1), "1")
        self.assertEqual(re.search(guard.CHECKPOINT_COUNTERS["cp_indexed_paths"], summary).group(1), "2")
        self.assertEqual(re.search(guard.CHECKPOINT_COUNTERS["cp_resolved_path_indices"], summary).group(1), "2")

    def test_guid_path_counters_reject_omission_and_arithmetic_mismatch(self):
        current = checkpoint_measurement(actor_closes=0, cp_partial_rows=0)
        self.assertEqual(guard.cross_checks(current["counters"], current["parquet"]), [])
        current["counters"]["cp_literal_paths"] = None
        self.assertIn("did not print", " ".join(guard.cross_checks(current["counters"], current["parquet"])))
        current = checkpoint_measurement(actor_closes=0, cp_partial_rows=0,
                                         cp_guid_entries=4, cp_literal_paths=2,
                                         cp_indexed_paths=1, cp_resolved_path_indices=0)
        problems = guard.cross_checks(current["counters"], current["parquet"])
        self.assertTrue(any("literals + indices" in problem for problem in problems), problems)
        self.assertTrue(any("resolved indices" in problem for problem in problems), problems)

    def test_checkpoint_measurement_requires_every_new_table_and_zero_dropped_actors(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            out = root / "out"
            out.mkdir()
            for name in (*guard.PARQUET_FILES, "checkpoint_fields", "checkpoint_net_guids"):
                pq.write_table(pa.table({"value": [1]}), out / f"{name}.parquet")
            (out / "manifest.json").write_text(json.dumps({"quality": {"checkpoints": {
                "checkpoint_actor_rows_dropped": 0,
                "checkpoint_path_resolution_mode": "preceding_literal_zero_based",
                "checkpoint_literal_paths": 1,
                "checkpoint_indexed_paths": 0,
                "checkpoint_resolved_path_indices": 0,
                "checkpoint_guid_entries": 1}}}), encoding="utf-8")
            with patch.object(guard.subprocess, "run", return_value=SimpleNamespace(
                    returncode=0, stdout="", stderr="")):
                with self.assertRaisesRegex(SystemExit, "checkpoint_actors.parquet"):
                    guard.measure(Path("fake.exe"), root / "sample.vrf", out, checkpoints=True)

            (out / "manifest.json").write_text(json.dumps({"quality": {"checkpoints": {
                "checkpoint_actor_rows_dropped": 1,
                "checkpoint_path_resolution_mode": "preceding_literal_zero_based",
                "checkpoint_literal_paths": 1,
                "checkpoint_indexed_paths": 0,
                "checkpoint_resolved_path_indices": 0,
                "checkpoint_guid_entries": 1}}}), encoding="utf-8")
            self.assertIn("expected 0", " ".join(guard.checkpoint_manifest_errors(out)))

    def test_checkpoint_manifest_rejects_missing_and_mismatched_path_evidence(self):
        with tempfile.TemporaryDirectory() as temp:
            manifest = Path(temp) / "manifest.json"
            manifest.write_text(json.dumps({"quality": {"checkpoints": {
                "checkpoint_actor_rows_dropped": 0}}}), encoding="utf-8")
            self.assertIn("omits required", " ".join(guard.checkpoint_manifest_errors(Path(temp))))
            manifest.write_text(json.dumps({"quality": {"checkpoints": {
                "checkpoint_actor_rows_dropped": 0,
                "checkpoint_path_resolution_mode": "legacy_decimal",
                "checkpoint_literal_paths": 2,
                "checkpoint_indexed_paths": 1,
                "checkpoint_resolved_path_indices": 0,
                "checkpoint_guid_entries": 4}}}), encoding="utf-8")
            problems = guard.checkpoint_manifest_errors(Path(temp))
            self.assertTrue(any("mode" in problem for problem in problems), problems)
            self.assertTrue(any("literals + indices" in problem for problem in problems), problems)
            self.assertTrue(any("resolved indices" in problem for problem in problems), problems)

    def test_manifest_path_counts_must_match_summary_and_have_integer_types(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            manifest = root / "manifest.json"
            cp = {"checkpoint_actor_rows_dropped": 0,
                  "checkpoint_path_resolution_mode": "preceding_literal_zero_based",
                  "checkpoint_literal_paths": 2, "checkpoint_indexed_paths": 1,
                  "checkpoint_resolved_path_indices": 1, "checkpoint_guid_entries": 3}
            counters = {"cp_literal_paths": 2, "cp_indexed_paths": 1,
                        "cp_resolved_path_indices": 1, "cp_guid_entries": 3}
            manifest.write_text(json.dumps({"quality": {"checkpoints": cp}}), encoding="utf-8")
            self.assertEqual(guard.checkpoint_manifest_errors(root, counters), [])
            self.assertIn("disagrees", " ".join(guard.checkpoint_manifest_errors(
                root, dict(counters, cp_literal_paths=3, cp_guid_entries=4))))
            for value in (None, True, "2", 2.0, -1):
                with self.subTest(value=value):
                    changed = dict(cp, checkpoint_literal_paths=value)
                    manifest.write_text(json.dumps({"quality": {"checkpoints": changed}}), encoding="utf-8")
                    self.assertIn("nonnegative integers", " ".join(guard.checkpoint_manifest_errors(root)))

    def test_reward_opaque_manifest_reconciles_main_and_checkpoint_counts(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            quality = {
                "sink": {"tracked_rewards_opaque_empty_variants": 4470},
                "checkpoints": {"sink": {"tracked_rewards_opaque_empty_variants": 7}},
            }
            (root / "manifest.json").write_text(
                json.dumps({"quality": quality}), encoding="utf-8")
            counters = {"tracked_rewards_opaque_empty_variants": 4470,
                        "cp_tracked_rewards_opaque_empty_variants": 7}
            self.assertEqual(guard.reward_opaque_manifest_errors(root, counters, True), [])
            problems = guard.reward_opaque_manifest_errors(
                root, dict(counters, tracked_rewards_opaque_empty_variants=1), True)
            self.assertIn("disagrees", " ".join(problems))
            problems = guard.reward_opaque_manifest_errors(
                root, dict(counters, cp_tracked_rewards_opaque_empty_variants=1), True)
            self.assertIn("cp_tracked_rewards_opaque_empty_variants", " ".join(problems))

    def test_reward_opaque_manifest_rejects_missing_and_noninteger_counts(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            manifest = root / "manifest.json"
            manifest.write_text(json.dumps({"quality": {}}), encoding="utf-8")
            self.assertIn("omits", " ".join(guard.reward_opaque_manifest_errors(
                root, {}, False)))
            # The real manifest puts sink-owned counters below `quality.sink`;
            # accepting this tempting flat shape would hide a wiring drift.
            manifest.write_text(json.dumps({"quality": {
                "tracked_rewards_opaque_empty_variants": 4470
            }}), encoding="utf-8")
            self.assertIn("omits", " ".join(guard.reward_opaque_manifest_errors(
                root, {}, False)))
            for value in (None, True, "4470", 4470.0, -1):
                with self.subTest(value=value):
                    manifest.write_text(json.dumps({"quality": {
                        "sink": {"tracked_rewards_opaque_empty_variants": value}
                    }}), encoding="utf-8")
                    self.assertIn("nonnegative integers", " ".join(
                        guard.reward_opaque_manifest_errors(root, {}, False)))
            manifest.write_text(json.dumps({"quality": {
                "sink": {"tracked_rewards_opaque_empty_variants": 4470},
                "checkpoints": {"sink": {"tracked_rewards_opaque_empty_variants": "7"}},
            }}), encoding="utf-8")
            self.assertIn("nonnegative integers", " ".join(
                guard.reward_opaque_manifest_errors(root, {}, True)))


class ContentIdentityTests(unittest.TestCase):
    def test_equal_size_different_bytes_do_not_satisfy_byte_identity(self):
        baseline = measurement()
        current = measurement()
        current["parquet"]["fields"]["sha256"] = "b" * 64

        problems = guard.diff(baseline, current)

        self.assertTrue(any("fields.parquet sha256" in p for p in problems), problems)


class TargetingCounterTests(unittest.TestCase):
    def test_targeting_counts_require_matching_main_and_checkpoint_evidence(self):
        key = "targeting_world_locations_decoded"
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            manifest = root / "manifest.json"
            counts = {key: 12, "cp_" + key: 0}
            data = {"quality": {"sink": {key: 12}, "checkpoints": {"sink": {key: 0}}}}
            manifest.write_text(json.dumps(data), encoding="utf-8")
            self.assertEqual(guard.targeting_manifest_errors(root, counts, True), [])
            for changed in ({key: 11, "cp_" + key: 0}, {key: 12}, {key: 12, "cp_" + key: 1}):
                self.assertIn("disagrees", " ".join(guard.targeting_manifest_errors(root, changed, True)))
            for invalid in (True, -1, "0"):
                data["quality"]["checkpoints"]["sink"][key] = invalid
                manifest.write_text(json.dumps(data), encoding="utf-8")
                self.assertIn("nonnegative integers", " ".join(guard.targeting_manifest_errors(root, counts, True)))
            del data["quality"]["checkpoints"]
            manifest.write_text(json.dumps(data), encoding="utf-8")
            self.assertIn("omits", " ".join(guard.targeting_manifest_errors(root, counts, True)))
            self.assertEqual(guard.targeting_manifest_errors(root, counts, False), [])
        text = "Target locations: 12 array children\nCheckpoint targets: 0 array children\n"
        self.assertEqual(guard.PATTERNS[key].search(text).group(1), "12")
        self.assertEqual(re.search(guard.CHECKPOINT_COUNTERS["cp_" + key], text).group(1), "0")
        self.assertIsNone(guard.PATTERNS[key].search("Checkpoint targets: 12 array children"))


class CncCounterTests(unittest.TestCase):
    """The brute-force and tail lines, main and checkpoint, read their own line."""

    SUMMARY = (
        "  CNC RPC rows:     529\n"
        "  CNC brute force:  454 attempted / 0 unwalked\n"
        "  RepLayout tails:  75 decoded / 17 preserved\n"
        "  Checkpoint CNC:   3 RPC rows\n"
        "  Checkpoint CNC brute force: 3 attempted / 1 unwalked\n"
        "  Checkpoint tails: 0 decoded / 2 preserved\n"
    )
    MAIN = {"cnc_rpcs_emitted": 529, "cnc_bruteforce_payloads_attempted": 454,
            "cnc_bruteforce_payloads_unwalked": 0, "rep_layout_cnc_tails_decoded": 75,
            "rep_layout_cnc_tails_preserved": 17}
    CHECKPOINT = {"cp_cnc_rpcs_emitted": 3, "cp_cnc_bruteforce_payloads_attempted": 3,
                  "cp_cnc_bruteforce_payloads_unwalked": 1,
                  "cp_rep_layout_cnc_tails_decoded": 0,
                  "cp_rep_layout_cnc_tails_preserved": 2}

    def test_each_counter_reads_its_own_value(self):
        for key, value in self.MAIN.items():
            with self.subTest(key=key):
                self.assertEqual(int(guard.PATTERNS[key].search(self.SUMMARY).group(1)), value)
        for key, value in self.CHECKPOINT.items():
            with self.subTest(key=key):
                self.assertEqual(
                    int(re.search(guard.CHECKPOINT_COUNTERS[key], self.SUMMARY).group(1)), value)

    def test_a_missing_main_line_is_not_read_off_the_checkpoint_block(self):
        """`Checkpoint CNC brute force:` contains `CNC brute force:`; an
        unanchored main pattern would silently pin the checkpoint value."""
        checkpoint_only = "".join(
            line for line in self.SUMMARY.splitlines(True) if "Checkpoint" in line)
        for key in self.MAIN:
            with self.subTest(key=key):
                self.assertIsNone(guard.PATTERNS[key].search(checkpoint_only))
        main_only = "".join(
            line for line in self.SUMMARY.splitlines(True) if "Checkpoint" not in line)
        for key in self.CHECKPOINT:
            with self.subTest(key=key):
                self.assertIsNone(re.search(guard.CHECKPOINT_COUNTERS[key], main_only))

    def test_the_counters_cannot_be_pinned_when_unprinted(self):
        reasons = guard.unpinnable(measurement(cnc_bruteforce_payloads_unwalked=None))
        self.assertIn("cnc_bruteforce_payloads_unwalked", " ".join(reasons))


class MovementTailCounterTests(unittest.TestCase):
    """Each of the four numbers on each tails line reads its own position."""

    SUMMARY = (
        "  Movement tails:   11 sized (12 bits) / 13 open (14 bits)\n"
        "  Checkpoint movement tails: 21 sized (22 bits) / 23 open (24 bits)\n"
    )

    def test_each_position_is_its_own_counter_in_its_own_block(self):
        names = ("movement_sized_section_tails", "movement_sized_section_tail_bits",
                 "movement_open_section_tails", "movement_open_section_tail_bits")
        for offset, name in enumerate(names):
            with self.subTest(name=name):
                self.assertEqual(int(guard.PATTERNS[name].search(self.SUMMARY).group(1)),
                                 11 + offset)
                self.assertEqual(int(re.search(guard.CHECKPOINT_COUNTERS["cp_" + name],
                                               self.SUMMARY).group(1)), 21 + offset)
                checkpoint_only = self.SUMMARY.splitlines(True)[1]
                self.assertIsNone(guard.PATTERNS[name].search(checkpoint_only))


class RequiredInputTests(unittest.TestCase):
    def test_explicit_required_mode_cannot_report_missing_replay_as_skip(self):
        with tempfile.TemporaryDirectory() as temp:
            baseline = Path(temp) / "baseline.json"
            baseline.write_text(
                json.dumps({"replay": "missing.vrf"}), encoding="utf-8"
            )
            argv = sys.argv
            sys.argv = [
                "check_export_baseline.py",
                "--baseline", str(baseline),
                "--exe", sys.executable,
                "--require-input",
            ]
            output = io.StringIO()
            try:
                with contextlib.redirect_stdout(output), contextlib.redirect_stderr(output):
                    code = guard.main()
            finally:
                sys.argv = argv

        self.assertEqual(code, 2)
        self.assertIn("required", output.getvalue().lower())
        self.assertNotIn("SKIP:", output.getvalue())


class TransactionalOutputTests(unittest.TestCase):
    SUMMARY = """
Total content blocks: 1
Fields emitted: 1
RPCs emitted: 1
Movement rows: 1
Event rows: 1
NetGUID rows: 1
Actor opens: 1
Actor closes: 0
Reward opaque: 0 empty variants
Target locations: 0 array children
"""

    def run_fake_export(self, *, fail: bool):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        replay = root / "match.vrf"
        replay.write_bytes(b"replay")
        output = root / "published"
        output.mkdir()
        sentinel = output / "complete.sentinel"
        sentinel.write_text("previous complete output", encoding="utf-8")

        script = root / "export"
        if fail:
            script.write_text(
                "import sys\n"
                "print('deliberate export failure', file=sys.stderr)\n"
                "raise SystemExit(7)\n",
                encoding="utf-8",
            )
        else:
            script.write_text(
                "import json, os, shutil, sys\n"
                "from pathlib import Path\n"
                "import pyarrow as pa\n"
                "import pyarrow.parquet as pq\n"
                "out = Path(sys.argv[sys.argv.index('--out') + 1])\n"
                "stage = out.parent / (out.name + '.stage')\n"
                "backup = out.parent / (out.name + '.backup')\n"
                "stage.mkdir()\n"
                "for name in ('actors', 'fields', 'movement', 'net_guids', 'events', 'partials'):\n"
                "    pq.write_table(pa.table({'value': [1]}), stage / (name + '.parquet'))\n"
                "(stage / 'manifest.json').write_text(json.dumps({'quality': {'sink': {'tracked_rewards_opaque_empty_variants': 0, 'targeting_world_locations_decoded': 0}}}), encoding='utf-8')\n"
                "os.replace(out, backup)\n"
                "os.replace(stage, out)\n"
                "shutil.rmtree(backup)\n"
                f"print({self.SUMMARY!r})\n",
                encoding="utf-8",
            )

        previous = Path.cwd()
        os.chdir(root)
        try:
            if fail:
                with self.assertRaises(SystemExit) as caught:
                    guard.measure(Path(sys.executable), replay, output)
                self.assertIn("exit 7", str(caught.exception))
                self.assertIn("deliberate export failure", str(caught.exception))
                result = None
            else:
                result = guard.measure(Path(sys.executable), replay, output)
        finally:
            os.chdir(previous)
        return output, sentinel, result

    def test_failed_export_preserves_previous_complete_output(self):
        _, sentinel, _ = self.run_fake_export(fail=True)

        self.assertEqual(sentinel.read_text(encoding="utf-8"),
                         "previous complete output")

    def test_successful_export_replaces_previous_complete_output(self):
        output, sentinel, result = self.run_fake_export(fail=False)

        self.assertFalse(sentinel.exists())
        self.assertTrue((output / "fields.parquet").is_file())
        self.assertEqual(result["parquet"]["fields"]["rows"], 1)


if __name__ == "__main__":
    unittest.main()
