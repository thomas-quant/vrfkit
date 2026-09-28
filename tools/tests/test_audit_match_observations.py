"""Focused regression coverage for the ammo/RPC corroboration audit."""

from __future__ import annotations

import contextlib
import io
import sys
import tempfile
import unittest
from pathlib import Path

import pyarrow as pa
import pyarrow.parquet as pq

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import audit_match_observations as audit  # noqa: E402


def write_export(root: Path, rows: list[tuple]) -> None:
    pq.write_table(pa.table({
        "net_guid": [10, 20],
        "path": ["MagazineAmmo", "/Game/Equippables/Guns/Rifles/Test.Test_C"],
        "outer_net_guid": [20, None],
    }), root / "net_guids.parquet")
    names = ("time_ms", "packet_id", "actor_net_guid", "object_net_guid",
             "group_path", "field_name", "value_i64")
    pq.write_table(pa.table({name: [row[index] for row in rows]
                             for index, name in enumerate(names)}),
                   root / "fields.parquet")


def write_complete_export(root: Path, rows: list[tuple]) -> None:
    """An export as `vrfkit export` publishes it: tables, then manifest.json last."""
    root.mkdir()
    write_export(root, rows)
    (root / "manifest.json").write_text('{"replay_build": "13.02"}', encoding="utf-8")


class MatchObservationAuditTests(unittest.TestCase):
    ammo_group = "/Script/ShooterGame.AmmoComponent"
    rpc_group = "/Game/Equippables/Guns/Rifles/Test.Test_C_ClassNetCache"

    def test_unique_weapon_identity_match_is_corroborated(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            write_export(root, [
                (100, 10, 20, 10, self.ammo_group, audit.AMMO_FIELD, 30),
                (120, 12, 20, 10, self.ammo_group, audit.AMMO_FIELD, 29),
                (121, 13, 20, 0, self.rpc_group, audit.EFFECT_FIELD, 7),
            ])
            result = audit.audit_export(root)
        self.assertEqual(result["counts"]["corroborated_unique_rpc"], 1)
        self.assertEqual(result["counts"]["corroborated_same_packet"], 0)
        self.assertEqual(result["unique_rpc_time_offset_ms"], {"1": 1})

    def test_multiple_and_missing_rpc_evidence_stay_visible(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            write_export(root, [
                (100, 10, 20, 10, self.ammo_group, audit.AMMO_FIELD, 30),
                (120, 12, 20, 10, self.ammo_group, audit.AMMO_FIELD, 29),
                (121, 13, 20, 0, self.rpc_group, audit.EFFECT_FIELD, 7),
                (122, 14, 20, 0, self.rpc_group, audit.EFFECT_FIELD, 8),
                (500, 20, 20, 10, self.ammo_group, audit.AMMO_FIELD, 28),
            ])
            result = audit.audit_export(root, window_ms=30)
        self.assertEqual(result["counts"]["ambiguous_multiple_rpc"], 1)
        self.assertEqual(result["counts"]["unmatched"], 1)
        self.assertEqual(result["counts"]["magazine_streams"], 1)
        self.assertEqual(result["counts"]["all_ambiguous_magazine_streams"], 0)

    def test_conflicting_ammo_packet_is_counted_not_ordered(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            write_export(root, [
                (100, 10, 20, 10, self.ammo_group, audit.AMMO_FIELD, 30),
                (120, 12, 20, 10, self.ammo_group, audit.AMMO_FIELD, 29),
                (120, 12, 20, 10, self.ammo_group, audit.AMMO_FIELD, 28),
                (150, 15, 20, 10, self.ammo_group, audit.AMMO_FIELD, 27),
            ])
            result = audit.audit_export(root)
        self.assertEqual(result["counts"]["ambiguous_ammo_packets"], 1)
        self.assertEqual(result["counts"]["ammo_decreases_examined"], 0)

    def test_a_stream_with_no_determinate_sample_is_counted(self):
        """Every packet of stream 11 carries two values; streams 10 and 12 are
        determinate, so a count of determinate streams would read 2, not 1."""
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            write_export(root, [
                (100, 10, 20, 10, self.ammo_group, audit.AMMO_FIELD, 30),
                (120, 12, 20, 10, self.ammo_group, audit.AMMO_FIELD, 29),
                (100, 10, 20, 11, self.ammo_group, audit.AMMO_FIELD, 30),
                (100, 10, 20, 11, self.ammo_group, audit.AMMO_FIELD, 25),
                (130, 13, 20, 11, self.ammo_group, audit.AMMO_FIELD, 24),
                (130, 13, 20, 11, self.ammo_group, audit.AMMO_FIELD, 23),
                (110, 11, 20, 12, self.ammo_group, audit.AMMO_FIELD, 20),
                (140, 14, 20, 12, self.ammo_group, audit.AMMO_FIELD, 19),
            ])
            pq.write_table(pa.table({
                "net_guid": [10, 11, 12, 20],
                "path": ["MagazineAmmo", "MagazineAmmo", "MagazineAmmo",
                         "/Game/Equippables/Guns/Rifles/Test.Test_C"],
                "outer_net_guid": [20, 20, 20, None]}), root / "net_guids.parquet")
            counts = audit.audit_export(root)["counts"]
        self.assertEqual(counts["magazine_streams"], 3)
        self.assertEqual(counts["all_ambiguous_magazine_streams"], 1)
        self.assertEqual(counts["ambiguous_ammo_packets"], 2)
        self.assertNotIn("left_censored_magazine_streams", counts)

    def test_corpus_failure_and_empty_population_do_not_report_success(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            with self.assertRaisesRegex(ValueError, "no export"):
                audit.audit_exports(root)
            bad = root / "bad"
            bad.mkdir()
            (bad / "fields.parquet").write_bytes(b"broken")
            self.assertEqual(audit.main(["--exports", str(root), "--out", str(root / "audit.json")]), 1)

    def corroborated_rows(self) -> list[tuple]:
        return [
            (100, 10, 20, 10, self.ammo_group, audit.AMMO_FIELD, 30),
            (120, 12, 20, 10, self.ammo_group, audit.AMMO_FIELD, 29),
            (121, 13, 20, 0, self.rpc_group, audit.EFFECT_FIELD, 7),
        ]

    def test_export_leftovers_are_listed_and_never_audited(self):
        """Neither sibling `vrfkit export` leaves behind is an export (the
        259ed10 measurements are export_scan.py's)."""
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            write_complete_export(root / "pub2", self.corroborated_rows())
            staging = root / ".pub2.vrfkit-staging-55396-0"
            staging.mkdir()
            (staging / "fields.parquet").write_bytes(b"PAR1 no footer")
            write_complete_export(root / ".pub2.vrfkit-previous-55396-1", self.corroborated_rows())
            expected_skipped = [str((root / name).resolve()) for name in (
                ".pub2.vrfkit-previous-55396-1", ".pub2.vrfkit-staging-55396-0")]
            result = audit.audit_exports(root)
            with contextlib.redirect_stdout(io.StringIO()):
                code = audit.main(["--exports", str(root), "--out", str(root / "audit.json")])
        self.assertEqual(result["candidate_exports"], 1)
        self.assertEqual(result["exports_scanned"], 1)
        self.assertEqual(result["exports_failed"], 0)
        self.assertEqual(result["aggregate_counts"]["corroborated_unique_rpc"], 1)
        self.assertEqual(result["skipped_generated_dirs"], expected_skipped)
        self.assertEqual(code, 0)

    def test_a_clean_corpus_still_reports_an_empty_skip_list(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            write_complete_export(root / "one", self.corroborated_rows())
            result = audit.audit_exports(root)
        self.assertEqual(result["skipped_generated_dirs"], [])
        self.assertEqual(result["exports_scanned"], 1)

    def test_a_child_without_a_manifest_is_a_failure_not_an_export(self):
        """manifest.json is written last, so its absence means unfinished."""
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            write_complete_export(root / "complete", self.corroborated_rows())
            partial = root / "partial"
            partial.mkdir()
            write_export(partial, self.corroborated_rows())
            result = audit.audit_exports(root)
        self.assertEqual(result["candidate_exports"], 2)
        self.assertEqual(result["exports_scanned"], 1)
        self.assertEqual(result["exports_failed"], 1)
        self.assertIn("manifest.json", result["failures"][0]["error"])

    def test_a_parent_holding_only_leftovers_is_an_error_that_counts_them(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            staging = root / ".pub2.vrfkit-staging-55396-0"
            staging.mkdir()
            (staging / "fields.parquet").write_bytes(b"PAR1 no footer")
            with self.assertRaisesRegex(ValueError, r"no export directories found.*1 "):
                audit.audit_exports(root)

    def test_conflicting_weapon_identity_cannot_corroborate(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            write_export(root, [
                (100, 10, 20, 10, self.ammo_group, audit.AMMO_FIELD, 30),
                (120, 12, 20, 10, self.ammo_group, audit.AMMO_FIELD, 29),
                (121, 13, 20, 0, self.rpc_group, audit.EFFECT_FIELD, 7),
            ])
            pq.write_table(pa.table({"net_guid": [10, 20, 20],
                "path": ["MagazineAmmo", "Weapon1", "Weapon2"],
                "outer_net_guid": [20, None, None]}), root / "net_guids.parquet")
            result = audit.audit_export(root)
        self.assertEqual(result["counts"]["corroborated_unique_rpc"], 0)
        self.assertEqual(result["counts"]["identity_unresolved"], 1)
        self.assertEqual(result["counts"]["conflicting_net_guid_mappings"], 1)

    def test_dynamic_weapon_guid_does_not_require_a_static_path_registration(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            write_export(root, [
                (100, 10, 20, 10, self.ammo_group, audit.AMMO_FIELD, 30),
                (120, 12, 20, 10, self.ammo_group, audit.AMMO_FIELD, 29),
                (121, 13, 20, 0, self.rpc_group, audit.EFFECT_FIELD, 7),
            ])
            pq.write_table(pa.table({"net_guid": [10], "path": ["MagazineAmmo"],
                                    "outer_net_guid": [20]}), root / "net_guids.parquet")
            result = audit.audit_export(root)
        self.assertEqual(result["counts"]["corroborated_unique_rpc"], 1)
        self.assertEqual(result["counts"]["identity_unresolved"], 0)


if __name__ == "__main__":
    unittest.main()
