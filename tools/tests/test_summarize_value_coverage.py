"""Physical coverage counts rows once and never substitutes truthiness for null."""
import contextlib
import io
import json
from pathlib import Path
import sys
import tempfile
import unittest

import pyarrow as pa
import pyarrow.parquet as pq

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import summarize_value_coverage as coverage


class PhysicalCoverageTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    def write(self, directory, filename="fields.parquet"):
        directory.mkdir(exist_ok=True)
        (directory / "manifest.json").write_text("{}", encoding="utf-8")
        pq.write_table(pa.table({
            "group_path": pa.array(["/Game/Test"] * 5),
            "field_name": pa.array(["ReviewedField"] * 5),
            "marker": pa.array([None, "other", None, None, None], type=pa.string()),
            "value_i64": pa.array([0, None, None, None, 7], type=pa.int64()),
            "value_f64": pa.array([None, None, None, None, 2.5], type=pa.float64()),
            "value_bool": pa.array([None, False, None, None, None], type=pa.bool_()),
            "value_str": pa.array([None, None, "", None, None], type=pa.string()),
        }), directory / filename)

    def test_null_union_and_checkpoint_denominators_are_independent(self):
        first, second = self.root / "first", self.root / "second"
        self.write(first)
        self.write(first, "checkpoint_fields.parquet")
        self.write(second)
        paths = coverage.discover([self.root, first])
        self.assertEqual(len(paths), 2)
        report = coverage.summarize(paths, 2)
        main = report["tables"]["fields"]
        cp = report["tables"]["checkpoint_fields"]
        self.assertTrue(report["complete"])
        self.assertEqual((main["rows"], main["typed_rows"], main["multi_value_rows"]), (10, 8, 2))
        self.assertEqual(main["typed_fraction"], 0.8)
        self.assertEqual((cp["exports_with_table"], cp["rows"], cp["typed_rows"]), (1, 5, 4))

    def test_export_leftovers_beside_an_export_are_not_counted(self):
        """`vrfkit export` siblings are not exports, and the report says so
        (the 259ed10 measurements are export_scan.py's)."""
        self.write(self.root / "pub2")
        self.write(self.root / ".pub2.vrfkit-previous-4242-7")
        staging = self.root / ".pub2.vrfkit-staging-55396-0"
        staging.mkdir()
        (staging / "fields.parquet").write_bytes(b"PAR1 no footer")
        expected_skipped = [str((self.root / name).resolve()) for name in (
            ".pub2.vrfkit-previous-4242-7", ".pub2.vrfkit-staging-55396-0")]

        self.assertEqual(coverage.discover([self.root]), [(self.root / "pub2").resolve()])
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            code = coverage.main([str(self.root), "--jobs", "1"])
        report = json.loads(output.getvalue())
        self.assertEqual(code, 0)
        self.assertEqual(report["export_count"], 1)
        self.assertEqual(report["tables"]["fields"]["rows"], 5)
        self.assertEqual(report["skipped_generated_dirs"], expected_skipped)

    def test_a_parent_holding_only_leftovers_is_an_error_that_counts_them(self):
        self.write(self.root / ".pub2.vrfkit-previous-4242-7")
        with self.assertRaisesRegex(ValueError, r"no direct child exports.*1 "):
            coverage.discover([self.root])

    def test_bad_export_is_explicit_and_cli_fails(self):
        self.write(self.root / "good")
        broken = self.root / "broken"
        broken.mkdir()
        (broken / "fields.parquet").write_bytes(b"not parquet")
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            code = coverage.main([str(self.root)])
        report = json.loads(output.getvalue())
        self.assertEqual(code, 1)
        self.assertFalse(report["complete"])
        self.assertEqual(report["successful_exports"], 1)
        self.assertEqual(len(report["errors"]), 1)

    def test_a_table_without_a_manifest_is_a_failed_export(self):
        """vrfkit writes manifest.json last: without it the export may be a
        partial copy whose checkpoint table has not arrived yet."""
        self.write(self.root / "done")
        partial = self.root / "partial"
        self.write(partial)
        (partial / "manifest.json").unlink()
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            code = coverage.main([str(self.root)])
        report = json.loads(output.getvalue())
        self.assertEqual((code, report["complete"], report["successful_exports"]), (1, False, 1))
        self.assertIn("manifest.json", report["errors"][0]["error"])

    def test_empty_table_has_unknown_fraction_and_schema_is_required(self):
        empty = self.root / "fields.parquet"
        (self.root / "manifest.json").write_text("{}", encoding="utf-8")
        pq.write_table(pa.table({name: pa.array([], type=pa.int64())
                                for name in coverage.VALUE_COLUMNS}), empty)
        self.assertEqual(coverage.count_table(empty)["rows"], 0)
        report = coverage.summarize([self.root], 1)
        self.assertTrue(report["complete"])
        self.assertIsNone(report["tables"]["fields"]["typed_fraction"])
        self.assertIsNone(report["tables"]["checkpoint_fields"]["typed_fraction"])
        pq.write_table(pa.table({"different": [1]}), empty)
        with self.assertRaisesRegex(ValueError, "missing value columns"):
            coverage.count_table(empty)

    def evidence_catalog(self, claims, schema_version=1):
        path = self.root / "semantic-evidence.json"
        path.write_text(json.dumps({
            "schema_version": schema_version,
            "catalog_version": "review-2026-09-08",
            "sources": [{
                "id": "wire-review",
                "version": "abc123",
                "scope": {"build": "13.05", "replay_count": 1, "commands": ["export"]},
            }],
            "claims": claims,
        }), encoding="utf-8")
        return path

    def write_path_rows(self, directory):
        directory.mkdir(exist_ok=True)
        (directory / "manifest.json").write_text("{}", encoding="utf-8")
        pq.write_table(pa.table({
            "group_path": pa.array(["/Game/Combat", "/Game/Combat", "/Game/Other", "/Game/Combat", "/Game/Combat", "/Game/Combat"]),
            "field_name": pa.array([
                "Rounds[0].Reports[12].Interactions[3].ParticipantSubject",
                "Rounds[0].Reports[-1].Interactions[3].ParticipantSubject",
                "Rounds[0].Reports[1].Interactions[2].ParticipantSubject",
                "prefix.Rounds[0].Reports[1].Interactions[2].ParticipantSubject",
                "Rounds[0].Reports[1].Interactions[2].ParticipantSubjectSuffix",
                None,
            ], type=pa.string()),
            "value_i64": pa.array([1, 2, 3, 4, 5, 6], type=pa.int64()),
            "value_f64": pa.array([None] * 6, type=pa.float64()),
            "value_bool": pa.array([None] * 6, type=pa.bool_()),
            "value_str": pa.array([None] * 6, type=pa.string()),
        }), directory / "fields.parquet")

        table = pq.read_table(directory / "fields.parquet")
        for name in ("group_path", "field_name"):
            table = table.set_column(table.schema.get_field_index(name), name,
                                     table[name].dictionary_encode())
        pq.write_table(table, directory / "fields.parquet")

    def test_reviewed_semantic_rows_need_catalog_criteria_and_keep_unknown_unknown(self):
        self.write(self.root / "one")
        catalog = self.evidence_catalog([
            {
                "id": "zero-is-a-reviewed-observation",
                "source_id": "wire-review",
                "table": "fields",
                "evidence_status": "reviewed",
                "semantic_label": "reviewed test observation",
                "reviewed_at": "2026-09-08",
                "evidence": "independent fixture check",
                "applicability": {"export_ids": ["one"]},
                "criteria": {"group_path": "/Game/Test", "field_name": "ReviewedField", "value_i64": 0},
            },
            {
                "id": "typed-is-not-semantic",
                "source_id": "wire-review",
                "table": "fields",
                "evidence_status": "unknown",
                "criteria": {"value_i64": 7},
            },
        ])
        report = coverage.summarize(coverage.discover([self.root]), 1,
                                    coverage.load_semantic_evidence(catalog))
        semantic = report["semantic_evidence"]
        self.assertTrue(report["complete"])
        self.assertEqual(semantic["sources"][0]["scope"]["build"], "13.05")
        self.assertEqual(semantic["tables"]["fields"]["reviewed_rows"], 1)
        self.assertEqual(semantic["tables"]["fields"]["reviewed_typed_rows"], 1)
        self.assertEqual(semantic["tables"]["fields"]["unknown_or_unsupported_claim_count"], 1)

    def test_duplicate_claim_and_mismatched_criteria_field_are_rejected(self):
        duplicate = {
            "id": "same", "source_id": "wire-review", "table": "fields",
            "evidence_status": "unknown", "criteria": {"value_i64": 7},
        }
        catalog = self.evidence_catalog([duplicate, duplicate.copy()])
        with self.assertRaisesRegex(coverage.EvidenceError, "duplicate semantic evidence claim id"):
            coverage.load_semantic_evidence(catalog)

        self.write(self.root / "one")
        catalog = self.evidence_catalog([{
            "id": "false-field", "source_id": "wire-review", "table": "fields",
            "evidence_status": "reviewed", "semantic_label": "bad selector",
            "reviewed_at": "2026-09-08", "evidence": "none",
            "applicability": {"export_ids": ["one"]},
            "criteria": {"group_path": "/Game/Test", "field_name": "ReviewedField", "invented_field": "plausible"},
        }])
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            code = coverage.main([str(self.root), "--semantic-evidence", str(catalog)])
        report = json.loads(output.getvalue())
        self.assertEqual(code, 1)
        self.assertFalse(report["complete"])
        self.assertIn("criteria fields absent", report["errors"][0]["error"])

    def test_reviewed_claim_scope_and_null_masks_are_enforced(self):
        self.write(self.root / "one")
        self.write(self.root / "outside")
        catalog = self.evidence_catalog([
            {
                "id": "zero", "source_id": "wire-review", "table": "fields",
                "evidence_status": "reviewed", "semantic_label": "zero observation",
                "reviewed_at": "2026-09-08", "evidence": "fixture",
                "applicability": {"export_ids": ["one"]},
                "criteria": {"group_path": "/Game/Test", "field_name": "ReviewedField", "value_i64": 0},
            },
            {
                "id": "marker", "source_id": "wire-review", "table": "fields",
                "evidence_status": "reviewed", "semantic_label": "marker observation",
                "reviewed_at": "2026-09-08", "evidence": "fixture",
                "applicability": {"export_ids": ["one"]},
                "criteria": {"group_path": "/Game/Test", "field_name": "ReviewedField", "marker": "other"},
            },
        ])
        report = coverage.summarize(coverage.discover([self.root]), 1,
                                    coverage.load_semantic_evidence(catalog))
        table = report["semantic_evidence"]["tables"]["fields"]
        self.assertTrue(report["complete"])
        self.assertEqual(table["applicable_exports"], 1)
        self.assertEqual(table["reviewed_rows"], 2)
        self.assertEqual(table["claims"], [{"id": "marker", "rows": 1}, {"id": "zero", "rows": 1}])
        self.assertEqual(len(report["semantic_evidence"]["catalog_sha256"]), 64)

    def test_reviewed_claim_requires_identity_scope_and_finite_values(self):
        base = {
            "id": "bad", "source_id": "wire-review", "table": "fields",
            "evidence_status": "reviewed", "semantic_label": "bad", "reviewed_at": "2026-09-08",
            "evidence": "fixture", "criteria": {"value_i64": 0},
        }
        with self.assertRaisesRegex(coverage.EvidenceError, "exact group_path"):
            coverage.load_semantic_evidence(self.evidence_catalog([base]))
        base["criteria"] = {"group_path": "/Game/Test", "field_name": "ReviewedField", "value_f64": float("nan")}
        with self.assertRaisesRegex(coverage.EvidenceError, "JSON scalar"):
            coverage.load_semantic_evidence(self.evidence_catalog([base]))

    def test_replay_build_applicability_reads_manifest(self):
        for name, build in (("matching", "release-ok"), ("other", "release-other")):
            directory = self.root / name
            self.write(directory)
            (directory / "manifest.json").write_text(json.dumps({"replay_build": build}), encoding="utf-8")
        catalog = self.evidence_catalog([{
            "id": "build-scoped", "source_id": "wire-review", "table": "fields",
            "evidence_status": "reviewed", "semantic_label": "build observation",
            "reviewed_at": "2026-09-08", "evidence": "fixture",
            "applicability": {"replay_builds": ["release-ok"]},
            "criteria": {"group_path": "/Game/Test", "field_name": "ReviewedField"},
        }])
        report = coverage.summarize(coverage.discover([self.root]), 1,
                                    coverage.load_semantic_evidence(catalog))
        table = report["semantic_evidence"]["tables"]["fields"]
        self.assertTrue(report["complete"])
        self.assertEqual((table["applicable_exports"], table["reviewed_rows"]), (1, 5))

        # Both restrictions narrow the claim: a matching ID cannot override
        # a mismatching build, and a matching build cannot override the ID.
        scoped = coverage.load_semantic_evidence(catalog)
        scoped["claims"][0]["applicability"]["export_ids"] = ["other"]
        report = coverage.summarize(coverage.discover([self.root]), 1, scoped)
        table = report["semantic_evidence"]["tables"]["fields"]
        self.assertEqual((table["applicable_exports"], table["reviewed_rows"]), (0, 0))

    def test_schema_two_indexed_path_selector_is_whole_path_group_scoped_and_null_safe(self):
        self.write_path_rows(self.root / "one")
        template = "Rounds[].Reports[].Interactions[].ParticipantSubject"
        claims = [
            {
                "id": "path", "source_id": "wire-review", "table": "fields",
                "evidence_status": "reviewed", "semantic_label": "participant subject",
                "reviewed_at": "2026-09-08", "evidence": "fixture",
                "applicability": {"export_ids": ["one"]},
                "field_path_template": template,
                "criteria": {"group_path": "/Game/Combat"},
            },
            {
                "id": "path-overlap", "source_id": "wire-review", "table": "fields",
                "evidence_status": "reviewed", "semantic_label": "same participant subject",
                "reviewed_at": "2026-09-08", "evidence": "fixture",
                "applicability": {"export_ids": ["one"]},
                "field_path_template": template,
                "criteria": {"group_path": "/Game/Combat"},
            },
        ]
        catalog = self.evidence_catalog(claims, schema_version=2)
        report = coverage.summarize(coverage.discover([self.root]), 1,
                                    coverage.load_semantic_evidence(catalog))
        table = report["semantic_evidence"]["tables"]["fields"]
        self.assertTrue(report["complete"])
        self.assertEqual(table["reviewed_rows"], 1)
        self.assertEqual(table["claims"], [{"id": "path", "rows": 1}, {"id": "path-overlap", "rows": 1}])

    def test_indexed_path_templates_reject_malformed_and_schema_one_selector(self):
        base = {
            "id": "path", "source_id": "wire-review", "table": "fields",
            "evidence_status": "reviewed", "semantic_label": "path", "reviewed_at": "2026-09-08",
            "evidence": "fixture", "applicability": {"export_ids": ["one"]},
            "criteria": {"group_path": "/Game/Combat"},
        }
        for template in ("Rounds[0].Reports[]", "Rounds[].Bad-Name", "Rounds.*.Reports[]", "Plain.Field"):
            with self.subTest(template=template):
                claim = base | {"field_path_template": template}
                with self.assertRaisesRegex(coverage.EvidenceError, "field_path_template"):
                    coverage.load_semantic_evidence(self.evidence_catalog([claim], schema_version=2))
        claim = base | {"field_path_template": "Rounds[].Reports[]"}
        with self.assertRaisesRegex(coverage.EvidenceError, "requires schema_version 2"):
            coverage.load_semantic_evidence(self.evidence_catalog([claim], schema_version=1))
        claim = claim | {"criteria": {"group_path": "/Game/Combat", "field_name": "literal"}}
        with self.assertRaisesRegex(coverage.EvidenceError, "exactly one"):
            coverage.load_semantic_evidence(self.evidence_catalog([claim], schema_version=2))


if __name__ == "__main__":
    unittest.main()
