"""Guards for the export baseline pinner: the Parquet cross-checks, the
refusal to pin an unmeasured counter or a machine path, and summary patterns
that must each read exactly one line."""
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


class CrossCheckTests(unittest.TestCase):
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


#: Checkpoint GUID declarations, independent of how an index is encoded:
#: `(checkpoint_index, net_guid, wire outer, target)`, where `target` is a
#: literal path or an int k meaning "a reference to this checkpoint's k-th
#: literal". Both checkpoints use one wire ID, so only `checkpoint_index`
#: separates their tables. The shapes are chosen so that each wrong rule below
#: resolves at least one reference to a different path.
DECLARATIONS = (
    (0, 3, 0, "/Game/Maps/Ascent"),
    (0, 5, 3, 0),                  # Ascent
    (0, 7, 3, "/Game/A.A_C"),
    (0, 19, 3, "/Game/D.D_C"),
    (0, 9, 7, 1),                  # A.A_C; D.D_C if references were appended
    (0, 13, 3, 0),                 # Ascent; A.A_C if one-based
    (1, 11, 0, "/Game/B.B_C"),
    (1, 15, 11, "/Game/C.C_C"),
    (1, 21, 11, "/Game/E.E_C"),
    (1, 23, 11, "/Game/F.F_C"),
    (1, 12, 11, 0),                # dynamic GUID; B.B_C in a fresh table, F.F_C in a shared one
    (1, 16, 15, 1),                # C.C_C
    (1, 17, 0, "/Game/OnlyInCheckpoint"),
)
#: The main stream's final registry: net_guid -> (path, outer or None).
MAIN_GUIDS = {
    3: ("/Game/Maps/Ascent", None), 5: ("/Game/Maps/Ascent", 3),
    7: ("/Game/A.A_C", 3), 19: ("/Game/D.D_C", 3), 9: ("/Game/A.A_C", 7),
    13: ("/Game/Maps/Ascent", 3), 11: ("/Game/B.B_C", None),
    15: ("/Game/C.C_C", 11), 21: ("/Game/E.E_C", 11), 23: ("/Game/F.F_C", 11),
    12: ("/Game/B.B_C", 11), 16: ("/Game/C.C_C", 15),
    40: ("/Game/OnlyInMain", None),
}
GUID_ENTRY_SCHEMA = pa.schema([
    ("checkpoint_index", pa.uint32()), ("checkpoint_id", pa.string()),
    ("ordinal", pa.uint32()), ("net_guid", pa.uint32()),
    ("outer_net_guid", pa.uint32()), ("path_is_string", pa.bool_()),
    ("literal_path", pa.string()), ("name_index", pa.uint32()), ("flags", pa.uint8()),
])


def encode_guid_entries(rule="zero_based", declarations=DECLARATIONS):
    """Raw `checkpoint_guid_entries` rows as a serializer following `rule` writes them.

    `zero_based` is the rule the checkpoint reader implements. The others are
    the wrong rules the doc's negative controls measure: one-based positions,
    references appended to the table, and one table shared by every
    checkpoint instead of one per checkpoint.
    """
    rows, current, earlier_literals = [], None, 0
    for checkpoint, guid, outer, target in declarations:
        if checkpoint != current:
            if current is not None:
                earlier_literals += len(literal_slots)
            current, table, literal_slots = checkpoint, [], []
        row = {"checkpoint_index": checkpoint, "checkpoint_id": "cp",
               "ordinal": len(table), "net_guid": guid, "outer_net_guid": outer,
               "flags": 0}
        if isinstance(target, str):
            literal_slots.append(len(table))
            table.append(target)
            row.update(path_is_string=True, literal_path=target, name_index=None)
        else:
            index = {"zero_based": target, "one_based": target + 1,
                     "append_references": literal_slots[target],
                     "shared_table": earlier_literals + target}[rule]
            table.append(table[literal_slots[target]])
            row.update(path_is_string=False, literal_path=None, name_index=index)
        rows.append(row)
    return rows


def write_guid_tables(out, entries, main=MAIN_GUIDS, duplicate=None):
    """Write the two tables the cross-check joins; `net_guids.path` is
    dictionary-encoded, as the exporter writes it."""
    pq.write_table(pa.Table.from_pylist(entries, schema=GUID_ENTRY_SCHEMA),
                   out / "checkpoint_guid_entries.parquet")
    guids = sorted(main) + ([duplicate] if duplicate is not None else [])
    pq.write_table(pa.table({
        "net_guid": pa.array(guids, pa.uint32()),
        "path": pa.array([main[g][0] for g in guids]).dictionary_encode(),
        "outer_net_guid": pa.array([main[g][1] for g in guids], pa.uint32()),
    }), out / "net_guids.parquet")


class CheckpointGuidCrossCheckTests(unittest.TestCase):
    """The main stream is the independent side of the path-index rule's check."""

    def crosscheck(self, entries, main=MAIN_GUIDS, duplicate=None):
        with tempfile.TemporaryDirectory() as temp:
            out = Path(temp)
            write_guid_tables(out, entries, main, duplicate)
            return guard.checkpoint_guid_crosscheck(out)

    def test_the_readers_rule_agrees_with_the_main_stream(self):
        counts, errors = self.crosscheck(encode_guid_entries())
        self.assertEqual(errors, [])
        expected = dict.fromkeys(guard.GUID_CROSSCHECK_KEYS, 0)
        expected.update(indexed_joined=5, indexed_path_equal=5, indexed_outer_equal=5,
                        literal_joined=7, literal_path_equal=7, literal_outer_equal=7,
                        literal_unjoined=1)
        self.assertEqual(counts, expected)

    def test_rows_are_ordered_by_checkpoint_and_ordinal_not_file_order(self):
        rows = encode_guid_entries()
        counts, errors = self.crosscheck(list(reversed(rows)))
        self.assertEqual(errors, [])
        self.assertEqual((counts["indexed_path_equal"], counts["literal_path_equal"]), (5, 7))

    def assert_rejected(self, counts, errors, **expected):
        self.assertTrue(errors, counts)
        for key, value in expected.items():
            self.assertEqual(counts[key], value, key)

    def test_one_based_indices_are_rejected(self):
        counts, errors = self.crosscheck(encode_guid_entries("one_based"))
        self.assert_rejected(counts, errors, indexed_path_equal=0,
                             indexed_path_differs=4, indexed_unresolved=1)

    def test_indices_counting_appended_references_are_rejected(self):
        counts, errors = self.crosscheck(encode_guid_entries("append_references"))
        self.assert_rejected(counts, errors, indexed_path_equal=4, indexed_path_differs=1)
        self.assertIn("net_guid 9", " ".join(errors))

    def test_indices_into_a_table_not_reset_per_checkpoint_are_rejected(self):
        counts, errors = self.crosscheck(encode_guid_entries("shared_table"))
        self.assert_rejected(counts, errors, indexed_path_equal=3,
                             indexed_path_differs=1, indexed_unresolved=1)

    def test_an_edited_main_stream_path_is_rejected(self):
        main = dict(MAIN_GUIDS)
        main[9] = ("/Game/Other.Other_C", 7)
        counts, errors = self.crosscheck(encode_guid_entries(), main)
        self.assert_rejected(counts, errors, indexed_path_differs=1, indexed_path_equal=4)
        main = dict(MAIN_GUIDS)
        main[19] = ("/Game/Other.Other_C", 3)
        counts, errors = self.crosscheck(encode_guid_entries(), main)
        self.assert_rejected(counts, errors, literal_path_differs=1, indexed_path_differs=0)

    def test_an_index_past_the_preceding_literals_is_counted_not_raised(self):
        rows = encode_guid_entries()
        rows[4]["name_index"] = 99
        counts, errors = self.crosscheck(rows)
        self.assert_rejected(counts, errors, indexed_unresolved=1, indexed_joined=4)
        self.assertIn("index 99, 3 preceding literals", " ".join(errors))

    def test_nothing_joined_is_a_failure_not_a_vacuous_pass(self):
        without_indexed = {g: v for g, v in MAIN_GUIDS.items() if g not in (5, 9, 13, 12, 16)}
        literal_only = tuple(d for d in DECLARATIONS if isinstance(d[3], str))
        for name, entries, main in (
                ("no checkpoint rows", [], MAIN_GUIDS),
                ("indexed GUIDs absent from the main stream", encode_guid_entries(), without_indexed),
                ("literal entries only", encode_guid_entries(declarations=literal_only), MAIN_GUIDS)):
            with self.subTest(name):
                counts, errors = self.crosscheck(entries, main)
                self.assertEqual(counts["indexed_joined"], 0)
                self.assertIn("not compared at all", " ".join(errors))

    def test_a_repeated_main_guid_fails_even_when_both_rows_agree(self):
        counts, errors = self.crosscheck(encode_guid_entries(), duplicate=9)
        self.assert_rejected(counts, errors, main_duplicate_guids=1, indexed_path_differs=0)

    def test_outers_compare_by_an_explicit_rule_not_null_as_zero(self):
        for name, guid, main_outer, key in (
                ("main writes 0 where the checkpoint has no outer", 3, 0,
                 "literal_outer_presence_differs"),
                ("main has no outer where the checkpoint has one", 5, None,
                 "indexed_outer_presence_differs"),
                ("both have an outer and they differ", 9, 4, "indexed_outer_value_differs")):
            with self.subTest(name):
                main = dict(MAIN_GUIDS)
                main[guid] = (main[guid][0], main_outer)
                counts, errors = self.crosscheck(encode_guid_entries(), main)
                self.assert_rejected(counts, errors, **{key: 1})
                self.assertEqual(counts["indexed_path_differs"] + counts["literal_path_differs"], 0)

    def test_an_incomplete_raw_record_fails_before_paths_are_compared(self):
        def reference_with_literal(rows):
            rows[4]["literal_path"] = "/Game/A.A_C"

        def literal_with_index(rows):
            rows[0]["name_index"] = 0

        def null_outer(rows):
            rows[4]["outer_net_guid"] = None

        def gap(rows):
            del rows[10]

        def repeat(rows):
            rows.insert(10, dict(rows[10]))

        for change, key in ((reference_with_literal, "malformed_entries"),
                            (literal_with_index, "malformed_entries"),
                            (null_outer, "malformed_entries"),
                            (gap, "ordinal_errors"), (repeat, "ordinal_errors")):
            with self.subTest(change.__name__):
                rows = encode_guid_entries()
                change(rows)
                counts, errors = self.crosscheck(rows)
                self.assert_rejected(counts, errors, **{key: 1}, indexed_joined=0)
                self.assertIn("not a complete raw record", " ".join(errors))

    def test_a_missing_table_or_column_is_reported_not_raised(self):
        with tempfile.TemporaryDirectory() as temp:
            out = Path(temp)
            write_guid_tables(out, encode_guid_entries())
            (out / "net_guids.parquet").unlink()
            counts, errors = guard.checkpoint_guid_crosscheck(out)
            self.assertIn("cannot read", " ".join(errors))
            write_guid_tables(out, encode_guid_entries())
            entries = pq.read_table(out / "checkpoint_guid_entries.parquet")
            pq.write_table(entries.drop_columns(["name_index"]),
                           out / "checkpoint_guid_entries.parquet")
            counts, errors = guard.checkpoint_guid_crosscheck(out)
            self.assertIn("cannot read", " ".join(errors))
            self.assertEqual(set(counts.values()), {0})

    def test_every_count_is_printed_including_zeros(self):
        counts, _ = self.crosscheck(encode_guid_entries())
        line = guard.format_guid_crosscheck(counts)
        for key in guard.GUID_CROSSCHECK_KEYS:
            self.assertIn(f"{key.replace('_', ' ')} {counts[key]},", line + ",")
        self.assertIn("indexed path differs 0,", line)

    def test_checkpoint_measurement_runs_the_crosscheck_and_prints_it(self):
        literals = sum(isinstance(d[3], str) for d in DECLARATIONS)
        indices = len(DECLARATIONS) - literals
        summary = (f"Reward opaque: 0 empty variants\nCheckpoint reward opaque: 0 empty variants\n"
                   f"Target locations: 0 array children\nCheckpoint targets: 0 array children\n"
                   "  Frame skips:      0 external blobs / 0 external bytes / 0 game-specific bytes\n"
                   "  Checkpoint frame skips: 0 external blobs / 0 external bytes"
                   " / 0 game-specific bytes\n"
                   f"GUID entries: {len(DECLARATIONS)}\n"
                   f"GUID paths: {literals} literals / {indices} indices / {indices} resolved\n")
        sink = {"tracked_rewards_opaque_empty_variants": 0, "targeting_world_locations_decoded": 0}
        frames = {f"frame_{key}": 0 for key in guard.FRAME_SKIP_KEYS}
        manifest = {"quality": {"sink": sink, **frames, "checkpoints": dict(
            sink=sink, checkpoint_actor_rows_dropped=0,
            **{f"checkpoint_{key}": value for key, value in frames.items()},
            checkpoint_path_resolution_mode="preceding_literal_zero_based",
            checkpoint_literal_paths=literals, checkpoint_indexed_paths=indices,
            checkpoint_resolved_path_indices=indices, checkpoint_guid_entries=len(DECLARATIONS))}}
        edited = dict(MAIN_GUIDS)
        edited[12] = ("/Game/F.F_C", 11)
        for main, passes in ((MAIN_GUIDS, True), (edited, False)):
            with self.subTest(passes=passes), tempfile.TemporaryDirectory() as temp:
                out = Path(temp)
                for name in (*guard.PARQUET_FILES, *guard.CHECKPOINT_PARQUET_FILES):
                    pq.write_table(pa.table({"value": [1]}), out / f"{name}.parquet")
                write_guid_tables(out, encode_guid_entries(), main)
                (out / "manifest.json").write_text(json.dumps(manifest), encoding="utf-8")
                printed = io.StringIO()
                with patch.object(guard.subprocess, "run", return_value=SimpleNamespace(
                        returncode=0, stdout=summary, stderr="")), \
                        contextlib.redirect_stdout(printed):
                    if passes:
                        guard.measure(Path("fake.exe"), out / "sample.vrf", out, checkpoints=True)
                    else:
                        with self.assertRaisesRegex(SystemExit, "path the main stream does not declare"):
                            guard.measure(Path("fake.exe"), out / "sample.vrf", out, checkpoints=True)
                self.assertIn("Checkpoint GUID cross-check: indexed joined 5,", printed.getvalue())


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


class RpcCounterTests(unittest.TestCase):
    """`RPCs:` is a suffix of `Truncated RPCs:`, so an unanchored pattern
    would read whichever of the two lines came first."""

    def test_the_rpc_count_is_not_read_off_the_truncated_rpcs_line(self):
        pattern = guard.PATTERNS["rpcs"]
        for summary in ("  RPCs:             529\n  Truncated RPCs:   7\n",
                        "  Truncated RPCs:   7\n  RPCs:             529\n"):
            with self.subTest(summary=summary):
                self.assertEqual(pattern.search(summary).group(1), "529")
        self.assertIsNone(pattern.search("  Truncated RPCs:   7\n"))


class SummaryLabelTests(unittest.TestCase):
    """Every counter pattern reads exactly one line summary.rs can print, each
    quoted `"  ..."` literal rendered with its placeholders filled in: a
    pattern matching two reads whichever comes first, and one matching none
    reports a printed counter as missing on every run."""

    SUMMARY_RS = (Path(__file__).resolve().parents[2]
                  / "crates" / "vrfkit" / "src" / "driver" / "summary.rs")

    def test_every_counter_pattern_reads_exactly_one_summary_line(self):
        source = self.SUMMARY_RS.read_text(encoding="utf-8")
        literals = re.findall(r'"(  [^"\\]*(?:\\.[^"\\]*)*)"', source)
        rendered = [re.sub(r"\{[^{}]*\}", "7", literal) for literal in literals]
        self.assertGreater(len(rendered), 100, "summary.rs literals were not found")
        patterns = {**guard.COUNTERS, **guard.CHECKPOINT_COUNTERS}
        for key, pattern in patterns.items():
            with self.subTest(counter=key):
                hits = [line for line in rendered if re.search(pattern, line + "\n")]
                self.assertEqual(len(hits), 1, hits)


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


class FrameSkipCounterTests(unittest.TestCase):
    MAIN = "  Frame skips:      2 external blobs / 9 external bytes / 0 game-specific bytes\n"
    CHECKPOINT = ("  Checkpoint frame skips: 1 external blobs / 4 external bytes"
                  " / 5 game-specific bytes\n")

    def write_manifest(self, root: Path, main: dict, checkpoint: dict | None) -> None:
        quality = {f"frame_{key}": value for key, value in main.items()}
        if checkpoint is not None:
            quality["checkpoints"] = {f"checkpoint_frame_{key}": value
                                      for key, value in checkpoint.items()}
        (root / "manifest.json").write_text(json.dumps({"quality": quality}), encoding="utf-8")

    def test_frame_skip_counts_must_match_the_manifest_in_both_passes(self):
        main = {"external_data_blobs": 2, "external_data_bytes": 9, "game_specific_bytes": 0}
        checkpoint = {"external_data_blobs": 1, "external_data_bytes": 4, "game_specific_bytes": 5}
        counts = {f"frame_{key}": value for key, value in main.items()}
        counts.update({f"cp_frame_{key}": value for key, value in checkpoint.items()})
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            self.write_manifest(root, main, checkpoint)
            self.assertEqual(guard.frame_skip_manifest_errors(root, counts, True), [])
            for key in counts:
                changed = dict(counts, **{key: counts[key] + 1})
                self.assertIn(key, " ".join(guard.frame_skip_manifest_errors(root, changed, True)))
            missing = {k: v for k, v in counts.items() if k != "frame_game_specific_bytes"}
            self.assertIn("disagrees", " ".join(guard.frame_skip_manifest_errors(root, missing, True)))
            for invalid in (True, -1, "0"):
                self.write_manifest(root, dict(main, external_data_bytes=invalid), checkpoint)
                self.assertIn("nonnegative integers",
                              " ".join(guard.frame_skip_manifest_errors(root, counts, True)))
            self.write_manifest(root, main, None)
            self.assertIn("omits", " ".join(guard.frame_skip_manifest_errors(root, counts, True)))
            self.assertEqual(guard.frame_skip_manifest_errors(root, counts, False), [])

    def test_frame_skip_lines_are_read_only_by_their_own_patterns(self):
        text = self.MAIN + self.CHECKPOINT + "  Frames:           3\n"
        found = {key: int(guard.PATTERNS[key].search(text).group(1))
                 for key in ("frame_external_data_blobs", "frame_external_data_bytes",
                             "frame_game_specific_bytes")}
        self.assertEqual(found, {"frame_external_data_blobs": 2,
                                 "frame_external_data_bytes": 9,
                                 "frame_game_specific_bytes": 0})
        cp = {key: int(re.search(guard.CHECKPOINT_COUNTERS[key], text).group(1))
              for key in ("cp_frame_external_data_blobs", "cp_frame_external_data_bytes",
                          "cp_frame_game_specific_bytes")}
        self.assertEqual(cp, {"cp_frame_external_data_blobs": 1,
                              "cp_frame_external_data_bytes": 4,
                              "cp_frame_game_specific_bytes": 5})
        # Neither label may be read as the other, nor as the checkpoint `Frames:`.
        self.assertIsNone(guard.PATTERNS["frame_external_data_blobs"].search(self.CHECKPOINT))
        self.assertIsNone(re.search(guard.CHECKPOINT_COUNTERS["cp_frame_external_data_blobs"],
                                    self.MAIN))
        self.assertEqual(re.search(guard.CHECKPOINT_COUNTERS["cp_frames"], text).group(1), "3")


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


class UpdateReplayNameTests(unittest.TestCase):
    """--update must not pin a resolved replay path (an absolute --replay, or
    VRFKIT_CORPUS_DIR joined to a bare one) into a new baseline: it would put
    one machine's directory into a committed file."""

    def run_update(self, root: Path, replay: str, corpus_dir: str | None):
        current = measurement(actor_closes=0)
        self.assertEqual(guard.cross_checks(current["counters"], current["parquet"]), [],
                         "the stand-in measurement must reach the update")
        argv = ["check_export_baseline.py", "--baseline", str(root / "baseline.json"),
                "--exe", sys.executable, "--replay", replay, "--update"]
        output = io.StringIO()
        with patch.dict(os.environ), patch.object(sys, "argv", argv), \
                patch.object(guard, "measure", return_value=current) as measured, \
                contextlib.redirect_stdout(output), contextlib.redirect_stderr(output):
            os.environ.pop("VRFKIT_CORPUS_DIR", None)
            if corpus_dir is not None:
                os.environ["VRFKIT_CORPUS_DIR"] = corpus_dir
            code = guard.main()
        return code, output.getvalue(), measured

    def test_an_absolute_replay_is_refused_before_the_export_runs(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root / "match.vrf").write_bytes(b"replay")
            code, output, measured = self.run_update(root, str(root / "match.vrf"), None)
            self.assertEqual(code, 2, output)
            self.assertIn("bare filename", output)
            measured.assert_not_called()
            self.assertFalse((root / "baseline.json").exists())

    def test_a_bare_replay_is_pinned_as_given_not_as_resolved(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root / "match.vrf").write_bytes(b"replay")
            code, output, measured = self.run_update(root, "match.vrf", str(root))
            self.assertEqual(code, 0, output)
            measured.assert_called_once()
            self.assertEqual(measured.call_args.args[1], root / "match.vrf")
            stored = json.loads((root / "baseline.json").read_text(encoding="utf-8"))
            self.assertEqual(stored["replay"], "match.vrf")


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
Frame skips: 0 external blobs / 0 external bytes / 0 game-specific bytes
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
                "(stage / 'manifest.json').write_text(json.dumps({'quality': {'sink': {'tracked_rewards_opaque_empty_variants': 0, 'targeting_world_locations_decoded': 0}, 'frame_external_data_blobs': 0, 'frame_external_data_bytes': 0, 'frame_game_specific_bytes': 0}}), encoding='utf-8')\n"
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
