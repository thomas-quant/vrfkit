import json
import sys
import tempfile
import unittest
from pathlib import Path


sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import check_baseline_schemas as schemas  # noqa: E402


class BaselineSchemaTests(unittest.TestCase):
    def test_committed_baselines_are_schema_valid_and_cross_consistent(self):
        self.assertEqual(schemas.validate_repository(), [])

    def test_legacy_export_without_hashes_fails(self):
        data = {
            "replay": "sample.vrf",
            "counters": {key: 0 for key in schemas.MAIN_COUNTERS},
            "parquet": {
                name: {"rows": 0, "bytes": 0} for name in schemas.MAIN_PARQUET
            },
        }
        problems = schemas.validate_export_baseline(Path("export_sample.json"), data)
        self.assertTrue(any("sha256" in problem for problem in problems), problems)

    def test_a_non_sha256_placeholder_is_rejected(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            data = {
                "replay": "sample.vrf",
                "counters": {key: 0 for key in schemas.MAIN_COUNTERS},
                "parquet": {
                    name: {"rows": 0, "bytes": 0, "sha256": "not-measured"}
                    for name in schemas.MAIN_PARQUET
                },
            }
            path = root / "export_sample.json"
            path.write_text(json.dumps(data), encoding="utf-8")

            problems = schemas.validate_export_baseline(path, data)

        self.assertTrue(any("sha256" in problem for problem in problems), problems)

    def test_checkpoint_baseline_requires_actor_and_guid_tables_and_counters(self):
        data = {
            "replay": "sample.vrf",
            "counters": {key: 0 for key in schemas.MAIN_COUNTERS | schemas.CHECKPOINT_ONLY_COUNTERS},
            "parquet": {
                name: {"rows": 0, "bytes": 0, "sha256": "a" * 64}
                for name in (*schemas.MAIN_PARQUET, *schemas.CHECKPOINT_PARQUET_FILES)
            },
        }
        path = Path("checkpoint_sample.json")
        self.assertEqual(schemas.validate_export_baseline(path, data), [])

        missing_table = json.loads(json.dumps(data))
        del missing_table["parquet"]["checkpoint_actors"]
        problems = schemas.validate_export_baseline(path, missing_table)
        self.assertTrue(any("checkpoint_actors" in problem for problem in problems), problems)

        missing_counter = json.loads(json.dumps(data))
        del missing_counter["counters"]["cp_net_guid_rows_written"]
        problems = schemas.validate_export_baseline(path, missing_counter)
        self.assertTrue(any("cp_net_guid_rows_written" in problem for problem in problems), problems)

    def test_unknown_baseline_json_fails_closed(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root / "unvalidated.json").write_text("{}", encoding="utf-8")
            problems = schemas.validate_repository(root)
        self.assertTrue(any("unvalidated.json" in p and "unknown" in p for p in problems),
                        problems)

    def test_bench_rejects_boolean_timing_and_non_replay_name(self):
        problems = schemas.validate_bench_baseline(
            Path("bench.json"), {"export": True, "replay": 7}
        )
        self.assertTrue(any("export" in p for p in problems), problems)
        self.assertTrue(any("replay" in p for p in problems), problems)

    def test_metrics_reject_wrong_replay_and_negative_or_wrong_typed_values(self):
        metrics = json.loads(
            (schemas.BASELINES / "metrics_builds.json").read_text(encoding="utf-8")
        )
        metrics["replays"]["12.10"] = 12
        metrics["metrics"]["12.11"]["kills"] = -1
        metrics["metrics"]["13.00"]["players"] = 1.5
        metrics["metrics"]["13.01"]["damage_dealt"] = "24139.22"
        metrics["metrics"]["13.02"]["team_score"] = {"Blue": 13, "Red": True}

        problems = schemas.validate_metrics_baseline(
            Path("metrics_builds.json"), metrics
        )

        joined = "\n".join(problems)
        for expected in ("replays.12.10", "kills", "players", "damage_dealt",
                         "team_score.Red"):
            self.assertIn(expected, joined)


if __name__ == "__main__":
    unittest.main()
