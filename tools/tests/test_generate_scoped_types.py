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


    def test_every_type_name_is_a_live_field_type_variant(self):
        decode_rs = (gen.ROOT / "crates/vrf-decode/src/decode.rs").read_text(encoding="utf-8")
        enum = decode_rs.split("pub enum FieldType {", 1)[1].split("\n}", 1)[0]
        variants = {line.strip().split(" ")[0].rstrip(",{")
                    for line in enum.splitlines()
                    if line.strip() and not line.strip().startswith(("//", "///", "}"))
                    and line.startswith("    ") and not line.startswith("        ")}
        for name, expression in gen.TYPES.items():
            with self.subTest(name=name):
                head = expression[0]
                self.assertTrue(head.startswith("FieldType::"))
                self.assertIn(head.removeprefix("FieldType::").split(" ")[0], variants)

    def test_rotator_quantization_import_only_when_an_entry_needs_it(self):
        base = {"group": "/G.G_C", "field": "ReplicatedMovement", "checksum": 2749104612,
                "observed_builds": ["b"], "evidence": "e",
                "location_quantization": "RoundWholeNumber"}
        byte = gen.render([{**base, "type": "RepMovementByte"}])
        self.assertIn("use crate::types::{RotatorQuantization, VectorQuantization};", byte)
        self.assertIn("        FieldType::RepMovement {\n"
                      "            rotation: RotatorQuantization::ByteComponents,\n"
                      "            location: VectorQuantization::RoundWholeNumber,\n"
                      "        },\n", byte)
        short = gen.render([{**base, "type": "RepMovementShort",
                             "location_quantization": "RoundTwoDecimals"}])
        self.assertIn("RotatorQuantization::ShortComponents", short)
        self.assertIn("location: VectorQuantization::RoundTwoDecimals,", short)
        plain = gen.render([{"group": "/G.G_C", "field": "Scale", "checksum": 2749104612,
                             "observed_builds": ["b"], "evidence": "e",
                             "type": "VectorNetQuantize100"}])
        self.assertNotIn("RotatorQuantization", plain)
        self.assertNotIn("VectorQuantization", plain)
        self.assertIn("        FieldType::VectorNetQuantize { scale: 100 },\n", plain)

    def test_quantization_is_part_of_the_scoped_identity_type(self):
        # One group may not carry both quantizations for one checksum: the
        # identity is exact, so the second entry is a duplicate, not a choice.
        base = {"group": "/G.G_C", "field": "ReplicatedMovement", "checksum": 2749104612,
                "observed_builds": ["b"], "evidence": "e",
                "location_quantization": "RoundWholeNumber"}
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "evidence.json"
            path.write_text(json.dumps({"schema_version": 1, "entries": [
                {**base, "type": "RepMovementByte"}, {**base, "type": "RepMovementShort"}]}))
            with self.assertRaisesRegex(ValueError, "duplicate"):
                gen.load(path)

    def test_a_rep_movement_entry_must_state_a_measured_location_level(self):
        # The level is not on the wire and changes no width, so nothing
        # downstream could catch a defaulted one: the fixture states it, and
        # only a RepMovement entry may.
        base = {"group": "/G.G_C", "field": "ReplicatedMovement", "checksum": 2749104612,
                "observed_builds": ["b"], "evidence": "e", "type": "RepMovementShort"}
        scalar = {"group": "/G.G_C", "field": "Seed", "checksum": 1,
                  "observed_builds": ["b"], "evidence": "e", "type": "Int32"}
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "evidence.json"
            for entry, message in (
                (base, "needs a measured location_quantization"),
                ({**base, "location_quantization": "RoundHalves"}, "needs a measured"),
                ({**scalar, "location_quantization": "RoundWholeNumber"}, "only to RepMovement"),
            ):
                with self.subTest(entry=entry):
                    path.write_text(json.dumps({"schema_version": 1, "entries": [entry]}))
                    with self.assertRaisesRegex(ValueError, message):
                        gen.load(path)
            path.write_text(json.dumps({"schema_version": 1, "entries": [
                {**base, "location_quantization": "RoundOneDecimal"}]}))
            self.assertIn("location: VectorQuantization::RoundOneDecimal,",
                          gen.render(gen.load(path)))


if __name__ == "__main__":
    unittest.main()
