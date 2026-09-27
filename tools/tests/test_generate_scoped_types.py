import copy
import json
from pathlib import Path
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import generate_scoped_types as gen


class ScopedTypeGenerationTests(unittest.TestCase):
    def test_committed_table_matches_complete_evidence(self):
        self.assertEqual(gen.main(["--check"]), 0)
        entries = gen.load(gen.EVIDENCE)
        # 943211507 is the 32-bit B -- the second player-state GUID word, not
        # the byte-shaped B fields. It may only ever be UInt32, and only on the
        # two player-state groups (it was excluded outright until 2026-09-28).
        wide_b = sorted((e["group"].rsplit(".", 1)[-1], e["field"], e["type"])
                        for e in entries if e["checksum"] == 943211507)
        self.assertEqual(wide_b, [("BombPlayerState_C", "B", "UInt32"),
                                  ("Swiftplay_EoRCredits_PlayerState_C", "B", "UInt32")])
        self.assertEqual(len({(e["group"], e["field"], e["checksum"]) for e in entries}), len(entries))

    def test_rejects_ambiguous_identity_and_unproven_entries(self):
        entry = gen.load(gen.EVIDENCE)[0]
        cases = [[entry, {**entry, "type": "Float"}]]
        for key, value in [("checksum", True), ("checksum", 2**32), ("evidence", ""), ("observed_builds", []), ("type", "Raw")]:
            mutant = copy.deepcopy(entry)
            mutant[key] = value
            cases.append([mutant])
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "evidence.json"
            for entries in cases:
                with self.subTest(entries=entries):
                    path.write_text(json.dumps({"schema_version": 1, "entries": entries}))
                    with self.assertRaises(ValueError):
                        gen.load(path)

    def test_check_fails_for_a_changed_type(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "scoped.rs"
            path.write_text(gen.render(gen.load(gen.EVIDENCE)).replace("FieldType::Byte", "FieldType::Float", 1))
            self.assertEqual(gen.main(["--output", str(path), "--check"]), 1)


if __name__ == "__main__":
    unittest.main()
