"""Tests for the identifier-redacted raw-property corpus inventory."""
from __future__ import annotations

import io
import json
import sys
import tempfile
import unittest
from collections import Counter
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path

import pyarrow as pa
import pyarrow.parquet as pq


sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import analyze_raw_properties as raw_inventory  # noqa: E402


SCHEMA = pa.schema(
    [
        pa.field("packet_id", pa.uint32(), nullable=False),
        pa.field("channel_index", pa.uint32(), nullable=False),
        pa.field("actor_net_guid", pa.uint32(), nullable=False),
        pa.field("object_net_guid", pa.uint32(), nullable=True),
        pa.field("group_path", pa.string(), nullable=False),
        pa.field("handle", pa.uint32(), nullable=False),
        pa.field("field_name", pa.string(), nullable=True),
        pa.field("compatible_checksum", pa.uint32(), nullable=True),
        pa.field("bit_count", pa.uint32(), nullable=False),
        pa.field("raw_bits", pa.binary(), nullable=True),
        pa.field("value_i64", pa.int64(), nullable=True),
        pa.field("value_f64", pa.float64(), nullable=True),
        pa.field("value_bool", pa.bool_(), nullable=True),
        pa.field("value_str", pa.string(), nullable=True),
    ]
)


def row(
    *,
    packet: int,
    group: str,
    handle: int,
    name: str | None,
    bits: int,
    raw: bytes | None,
    value_i64: int | None = None,
) -> dict:
    return {
        "packet_id": packet,
        "channel_index": 2,
        "actor_net_guid": 3,
        "object_net_guid": 4,
        "group_path": group,
        "handle": handle,
        "field_name": name,
        "compatible_checksum": None,
        "bit_count": bits,
        "raw_bits": raw,
        "value_i64": value_i64,
        "value_f64": None,
        "value_bool": None,
        "value_str": None,
    }


class AnalyzeExportTests(unittest.TestCase):
    def _write_export(self, root: Path, rows: list[dict]) -> None:
        (root / "manifest.json").write_text(
            json.dumps({"replay_build": "++Ares-Core+release-13.04"}),
            encoding="utf-8",
        )
        pq.write_table(pa.Table.from_pylist(rows, schema=SCHEMA), root / "fields.parquet")

    def test_counts_only_unnamed_replicated_properties(self):
        rows = [
            row(packet=1, group="/private/group", handle=1, name="Typed", bits=8,
                raw=b"\x05", value_i64=5),
            row(packet=2, group="/private/group", handle=2, name="NamedRaw", bits=8,
                raw=b"\x06"),
            row(packet=3, group="/private/group", handle=3, name=None, bits=3,
                raw=b"\x00"),
            row(packet=4, group="/private/group", handle=3, name=None, bits=3,
                raw=b"\x05"),
            # RPC rows are not replicated properties and must not enter this tally.
            row(packet=5, group="/private/Rpc_ClassNetCache", handle=4, name=None,
                bits=8, raw=b"\xff"),
        ]
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            self._write_export(root, rows)
            inventory = raw_inventory.Inventory()
            build = raw_inventory.analyze_export(root, inventory, replay_ordinal=1)

        self.assertEqual(build, "13.04")
        self.assertEqual(inventory.field_rows[build], 5)
        self.assertEqual(inventory.property_rows[build], 4)
        self.assertEqual(inventory.property_raw_only_rows[build], 3)
        self.assertEqual(inventory.named_raw_only_rows[build], 1)
        self.assertEqual(inventory.unnamed_rows[build], 2)
        self.assertEqual(inventory.unnamed_raw_rows[build], 2)
        self.assertEqual(inventory.unnamed_zero_rows[build], 1)
        self.assertEqual(inventory.unnamed_checksum_rows[build], 0)
        self.assertEqual(inventory.unnamed_sentinel_handle_rows[build], 0)
        self.assertEqual(inventory.unnamed_wrong_length_rows[build], 0)
        self.assertEqual(inventory.unnamed_widths[build], Counter({3: 2}))
        self.assertEqual(len(inventory.signatures), 1)
        recurrence = next(iter(inventory.signatures.values()))
        self.assertEqual(recurrence.rows, 2)
        self.assertEqual(recurrence.rows_by_build, Counter({"13.04": 2}))
        self.assertTrue(recurrence.payload_varied)

    def test_missing_raw_bits_is_a_visible_integrity_failure(self):
        rows = [
            row(packet=1, group="/private/group", handle=1, name=None, bits=0, raw=None)
        ]
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            self._write_export(root, rows)
            inventory = raw_inventory.Inventory()
            raw_inventory.analyze_export(root, inventory, replay_ordinal=1)

        self.assertEqual(inventory.integrity_failures, 1)
        self.assertEqual(inventory.unnamed_raw_rows["13.04"], 0)

    def test_wrong_raw_byte_length_is_a_visible_integrity_failure(self):
        rows = [
            row(packet=1, group="/private/group", handle=1, name=None,
                bits=9, raw=b"\x01")
        ]
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            self._write_export(root, rows)
            inventory = raw_inventory.Inventory()
            raw_inventory.analyze_export(root, inventory, replay_ordinal=1)

        self.assertEqual(inventory.integrity_failures, 1)
        self.assertEqual(inventory.unnamed_wrong_length_rows["13.04"], 1)

    def test_report_cannot_expose_structural_identifiers(self):
        private_group = "/private/account-derived/group"
        rows = [
            row(packet=991, group=private_group, handle=987654, name=None,
                bits=24, raw=b"\x01\x02\x03")
        ]
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            self._write_export(root, rows)
            inventory = raw_inventory.Inventory()
            raw_inventory.analyze_export(root, inventory, replay_ordinal=1)

        report = raw_inventory.render_report(
            inventory,
            eligible_by_build=Counter({"13.04": 1}),
            selected_by_build=Counter({"13.04": 1}),
            excluded=0,
            recursive=False,
        )
        self.assertNotIn(private_group, report)
        self.assertNotIn("987654", report)
        self.assertNotIn("991", report)
        self.assertNotIn("010203", report)
        self.assertIn("24:1", report)

    def test_json_summary_is_versioned_and_cannot_expose_identifiers(self):
        private_group = "/private/account-derived/group"
        rows = [
            row(packet=991, group=private_group, handle=987654, name=None,
                bits=24, raw=b"\x01\x02\x03")
        ]
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            self._write_export(root, rows)
            inventory = raw_inventory.Inventory()
            raw_inventory.analyze_export(root, inventory, replay_ordinal=1)

        document = raw_inventory.summary_document(
            inventory,
            eligible_by_build=Counter({"13.04": 1}),
            selected_by_build=Counter({"13.04": 1}),
            excluded=0,
            recursive=False,
        )
        encoded = json.dumps(document, sort_keys=True)
        self.assertEqual(document["schema_version"], 1)
        self.assertTrue(document["identifier_redacted"])
        self.assertFalse(document["typing_inference_performed"])
        self.assertNotIn(private_group, encoded)
        self.assertNotIn("987654", encoded)
        self.assertNotIn("991", encoded)
        self.assertNotIn("010203", encoded)
        self.assertEqual(
            document["builds"]["13.04"]["unnamed_widths_bits"], {"24": 1}
        )


class SamplingTests(unittest.TestCase):
    def test_sample_spans_size_range_without_duplicates(self):
        candidates = [
            raw_inventory.ReplayCandidate(Path(f"private-{i}.vrf"), "13.04", i)
            for i in range(10)
        ]
        sampled = raw_inventory.stratified_sample(candidates, 4)
        self.assertEqual([item.size for item in sampled], [0, 3, 6, 9])
        self.assertEqual(len({item.path for item in sampled}), 4)

    def test_single_sample_selects_median_size(self):
        candidates = [
            raw_inventory.ReplayCandidate(Path(f"private-{i}.vrf"), "13.02", i)
            for i in range(5)
        ]
        sampled = raw_inventory.stratified_sample(candidates, 1)
        self.assertEqual(sampled[0].size, 2)


class BuildTests(unittest.TestCase):
    def test_parse_build_ignores_other_version_numbers(self):
        text = "Replay version: 5.3.2\nBranch: ++Ares-Core+release-13.04"
        self.assertEqual(raw_inventory.parse_build(text), "13.04")

    def test_parse_build_requires_release_label(self):
        self.assertIsNone(raw_inventory.parse_build("Replay version: 5.3.2"))

    def test_cli_rejects_non_numeric_build_label_before_it_can_be_printed(self):
        error = io.StringIO()
        with redirect_stderr(error), self.assertRaises(SystemExit):
            raw_inventory.parse_args(
                ["vrfkit", "corpus", "--build", "/private/replay-name"]
            )
        self.assertNotIn("/private/replay-name", error.getvalue())

    def test_build_help_states_the_default_builds(self):
        """The help said "13.02 and 13.04" while DEFAULT_BUILDS also held 13.05,
        so a default run sampled a build --help said it excluded."""
        printed = io.StringIO()
        with redirect_stdout(printed), self.assertRaises(SystemExit):
            raw_inventory.parse_args(["--help"])
        text = " ".join(printed.getvalue().split())
        self.assertIn(f"(default: {', '.join(raw_inventory.DEFAULT_BUILDS)})", text)


if __name__ == "__main__":
    unittest.main()
