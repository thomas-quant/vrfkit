import struct
import sys
import tempfile
import unittest
from pathlib import Path

import pyarrow as pa
import pyarrow.parquet as pq


sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from validate_type_evidence import decode_exact, exported_value, validate  # noqa: E402


def pack_bits(*fields):
    """`(bytes, bit_count)` for `(value, width)` fields written LSB-first."""
    value = position = 0
    for field, width in fields:
        value |= (field & ((1 << width) - 1)) << position
        position += width
    return value.to_bytes((position + 7) // 8, "little"), position


def fstring_fields(text: str):
    data = text.encode("utf-8") + b"\0"
    return [(len(data), 32)] + [(byte, 8) for byte in data]


#: Two real `ReplicatedMovement` rows and the JSON the Rust exporter wrote for
#: them (audit exports at 259ed10). The Short one is the case a string compare
#: cannot survive: f32 yaw 283.88671875 is printed as `283.88672`.
DIVEBOMB_BYTE = (
    "e0ec4e985d5421c37b4d07e4de1b04", 118,
    '{"linear_velocity":{"x":2062,"y":-530,"z":525},"angular_velocity":null,'
    '"location":{"x":25.25,"y":-44.04,"z":6.8},"rotation":{"pitch":16.875,'
    '"yaw":347.34375,"roll":0},"simulated_physics_sleep":false,"rep_physics":false,'
    '"server_frame":null,"server_physics_handle":null}',
)
SEEKER_NADE_SHORT = (
    "4095feb5fa74b0e00ac1930501", 100,
    '{"linear_velocity":{"x":0,"y":0,"z":0},"angular_velocity":null,'
    '"location":{"x":4423.22,"y":598.93,"z":891.1},"rotation":{"pitch":0,'
    '"yaw":283.88672,"roll":0},"simulated_physics_sleep":false,"rep_physics":false,'
    '"server_frame":null,"server_physics_handle":null}',
)


class BitLevelTypeTests(unittest.TestCase):
    def test_enum_byte_takes_its_width_from_the_payload(self):
        self.assertEqual(decode_exact(b"\x05", 3, "EnumByte"), 5)
        self.assertEqual(decode_exact(b"\xff", 8, "EnumByte"), 255)
        with self.assertRaisesRegex(ValueError, "1..8"):
            decode_exact(b"\x00\x01", 9, "EnumByte")
        with self.assertRaisesRegex(ValueError, "1..8"):
            decode_exact(b"", 0, "EnumByte")

    def test_bit_level_padding_must_be_zero(self):
        with self.assertRaisesRegex(ValueError, "padding"):
            decode_exact(b"\x0d", 3, "EnumByte")

    def test_fname_reads_the_two_corpus_payloads(self):
        """The only two OriginalBuyerTeam payloads in 1,018 replays."""
        self.assertEqual(
            decode_exact(bytes.fromhex("08000000a4cac8000000000000"), 97, "FName"), "Red")
        self.assertEqual(
            decode_exact(bytes.fromhex("0a00000084d8eaca000000000000"), 105, "FName"), "Blue")

    def test_fname_instance_number_and_hardcoded_index(self):
        raw, bits = pack_bits((0, 1), *fstring_fields("Red"), (2, 32))
        self.assertEqual(decode_exact(raw, bits, "FName"), "Red_1")
        raw, bits = pack_bits((1, 1), (0x59, 8), (0x04, 8))
        self.assertEqual(decode_exact(raw, bits, "FName"), "300")

    def test_fname_rejects_residue_bad_numbers_and_missing_terminators(self):
        raw, bits = pack_bits((0, 1), *fstring_fields("Red"), (0, 32), (0, 1))
        with self.assertRaisesRegex(ValueError, "residual"):
            decode_exact(raw, bits, "FName")
        raw, bits = pack_bits((0, 1), *fstring_fields("Red"), (-1, 32))
        with self.assertRaisesRegex(ValueError, "negative"):
            decode_exact(raw, bits, "FName")
        raw, bits = pack_bits((0, 1), (3, 32), *[(b, 8) for b in b"Red"], (0, 32))
        with self.assertRaisesRegex(ValueError, "terminator"):
            decode_exact(raw, bits, "FName")

    def test_rep_movement_matches_the_exported_json(self):
        for (hex_bits, bits, text), type_name in (
            (DIVEBOMB_BYTE, "RepMovementByte"),
            (SEEKER_NADE_SHORT, "RepMovementShort"),
        ):
            decoded = decode_exact(bytes.fromhex(hex_bits), bits, type_name)
            self.assertEqual(
                exported_value({"value_str": text}, type_name), decoded, type_name)

    def test_the_wrong_rotator_width_is_caught(self):
        """A Byte row read as Short runs off the end. The reverse can consume
        exactly and still be wrong -- which only the value compare sees."""
        hex_bits, bits, _text = DIVEBOMB_BYTE
        with self.assertRaisesRegex(ValueError, "past the end"):
            decode_exact(bytes.fromhex(hex_bits), bits, "RepMovementShort")
        hex_bits, bits, text = SEEKER_NADE_SHORT
        as_byte = decode_exact(bytes.fromhex(hex_bits), bits, "RepMovementByte")
        self.assertNotEqual(exported_value({"value_str": text}, "RepMovementByte"), as_byte)

    def test_rep_movement_optional_members_are_read_in_wire_order(self):
        # flags: physics, server frame and server handle set; every vector and
        # rotator component present and non-zero.
        raw, bits = pack_bits(
            (0, 1), (1, 1), (1, 1), (1, 1),
            (70, 7), (5, 6), (-3, 6), (1, 6),         # location, scaled x100
            (1, 1), (64, 8), (0, 1), (1, 1), (128, 8),  # pitch, no yaw, roll
            (6, 7), (-7, 6), (0, 6), (31, 6),          # velocity, unscaled
            (6, 7), (1, 6), (2, 6), (3, 6),            # angular velocity
            (0x06, 8), (0x0a, 8),                      # frame 3, handle 5
        )
        self.assertEqual(decode_exact(raw, bits, "RepMovementByte"), {
            "linear_velocity": (-7.0, 0.0, 31.0),
            "angular_velocity": (1.0, 2.0, 3.0),
            "location": (0.05, -0.03, 0.01),
            "rotation": (90.0, 0.0, 180.0),
            "simulated_physics_sleep": False,
            "rep_physics": True,
            "server_frame": 3,
            "server_physics_handle": 5,
        })

    def test_an_unparseable_export_is_a_mismatch_not_a_crash(self):
        hex_bits, bits, _text = DIVEBOMB_BYTE
        decoded = decode_exact(bytes.fromhex(hex_bits), bits, "RepMovementByte")
        self.assertNotEqual(
            exported_value({"value_str": "{not json"}, "RepMovementByte"), decoded)

    def test_compare_typed_checks_every_bit_level_type(self):
        rows = [
            ("g", "e", 3, bytes.fromhex("05"), 5, None),
            ("g", "n", 97, bytes.fromhex("08000000a4cac8000000000000"), None, "Red"),
            ("g", "m", DIVEBOMB_BYTE[1], bytes.fromhex(DIVEBOMB_BYTE[0]), None,
             DIVEBOMB_BYTE[2]),
        ]
        specs = [{"group": "g", "field": "e", "type": "EnumByte"},
                 {"group": "g", "field": "n", "type": "FName"},
                 {"group": "g", "field": "m", "type": "RepMovementByte"}]

        def run(rows):
            with tempfile.TemporaryDirectory() as directory:
                pq.write_table(pa.table({
                    "group_path": [r[0] for r in rows], "field_name": [r[1] for r in rows],
                    "handle": [1] * len(rows), "compatible_checksum": [2] * len(rows),
                    "bit_count": [r[2] for r in rows], "raw_bits": [r[3] for r in rows],
                    "value_i64": pa.array([r[4] for r in rows], pa.int64()),
                    "value_str": pa.array([r[5] for r in rows], pa.string()),
                    "value_f64": pa.array([None] * len(rows), pa.float64()),
                    "value_bool": pa.array([None] * len(rows), pa.bool_()),
                }), Path(directory) / "fields.parquet")
                return validate(Path(directory), specs, compare_typed=True)

        clean = run(rows)
        self.assertEqual((clean["failure_count"], clean["typed_mismatch_count"]), (0, 0))
        self.assertEqual(clean["missing"], [])
        wrong = [
            ("g", "e", 3, bytes.fromhex("05"), 4, None),
            ("g", "n", 97, bytes.fromhex("08000000a4cac8000000000000"), None, "Blue"),
            ("g", "m", DIVEBOMB_BYTE[1], bytes.fromhex(DIVEBOMB_BYTE[0]), None,
             DIVEBOMB_BYTE[2].replace("347.34375", "348.75")),
        ]
        self.assertEqual(run(wrong)["typed_mismatch_count"], 3)


class DecodeExactTests(unittest.TestCase):
    def test_checksum_scope_separates_same_name_and_catches_wrong_values(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "fields.parquet"
            pq.write_table(pa.table({
                "group_path": ["g", "g"], "field_name": ["B", "B"], "handle": [39, 208],
                "compatible_checksum": [379198054, 943211507], "bit_count": [8, 32],
                "raw_bits": [b"\xff", b"\0\0\0\0"], "value_str": [None, None],
                "value_i64": [254, None], "value_f64": [None, None], "value_bool": [None, None],
            }), path)
            spec = {"group": "g", "field": "B", "type": "Byte", "checksum": 379198054}
            report = validate(Path(directory), [spec], compare_typed=True)
            self.assertEqual(report["failure_count"], 0)
            self.assertEqual(report["typed_mismatch_count"], 1)
            self.assertEqual(report["fields"]["g::B::checksum=379198054"]["rows"], 1)
            with self.assertRaisesRegex(ValueError, "overlapping"):
                validate(Path(directory), [spec, {"group": "g", "field": "B", "type": "Byte"}])
            unscoped = validate(Path(directory), [{"group": "g", "field": "B", "type": "Byte"}])
            self.assertEqual(unscoped["failure_count"], 1)

    def test_primitive_widths_and_values(self):
        self.assertIs(decode_exact(b"\x01", 1, "Bool"), True)
        self.assertEqual(decode_exact(b"\xff", 8, "Byte"), 255)
        self.assertEqual(decode_exact(struct.pack("<i", -17), 32, "Int32"), -17)
        self.assertEqual(decode_exact(struct.pack("<f", 1.25), 32, "Float"), 1.25)
        self.assertEqual(decode_exact(struct.pack("<d", 2.5), 64, "Double"), 2.5)

    def test_fstring_requires_full_consumption_and_terminator(self):
        raw = struct.pack("<i", 4) + b"abc\0"
        self.assertEqual(decode_exact(raw, 64, "FString"), "abc")
        with self.assertRaisesRegex(ValueError, "length"):
            decode_exact(raw + b"x", 72, "FString")
        with self.assertRaisesRegex(ValueError, "terminator"):
            decode_exact(struct.pack("<i", 4) + b"abcd", 64, "FString")

    def test_object_guid_requires_terminated_full_intpacked(self):
        self.assertEqual(decode_exact(b"\x59\x04", 16, "ObjectNetGuid"), 300)
        with self.assertRaisesRegex(ValueError, "residual"):
            decode_exact(b"\x02\x00", 16, "ObjectNetGuid")

    def test_object_guid_rejects_leb128_mutant_truncation_and_overflow(self):
        with self.assertRaisesRegex(ValueError, "residual"):
            decode_exact(b"\xac\x02", 16, "ObjectNetGuid")
        with self.assertRaisesRegex(ValueError, "truncated"):
            decode_exact(b"\x01", 8, "ObjectNetGuid")
        with self.assertRaisesRegex(ValueError, "overflowing"):
            decode_exact(b"\x01\x01\x01\x01\x20", 40, "ObjectNetGuid")
        with self.assertRaisesRegex(ValueError, "runaway"):
            decode_exact(b"\xff" * 5, 40, "ObjectNetGuid")

    def test_object_guid_accepts_u32_max(self):
        self.assertEqual(
            decode_exact(b"\xff\xff\xff\xff\x1e", 40, "ObjectNetGuid"),
            0xffffffff,
        )

    def test_non_finite_float_is_rejected(self):
        with self.assertRaisesRegex(ValueError, "non-finite"):
            decode_exact(struct.pack("<f", float("nan")), 32, "Float")

    def test_wrong_width_fails_instead_of_decoding_a_prefix(self):
        with self.assertRaisesRegex(ValueError, "32 bits"):
            decode_exact(struct.pack("<d", 1.0), 64, "Float")

    def test_empty_specification_and_missing_root_are_rejected(self):
        with self.assertRaisesRegex(ValueError, "empty"):
            validate(Path("."), [])
        with self.assertRaisesRegex(ValueError, "does not exist"):
            validate(Path("definitely-not-a-real-evidence-root"), [
                {"group": "g", "field": "f", "type": "Bool"}
            ])

    def test_compare_typed_checks_the_exported_value(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "fields.parquet"
            pq.write_table(pa.table({
                "group_path": ["g"], "field_name": ["f"], "handle": [1],
                "compatible_checksum": [2], "bit_count": [32],
                "raw_bits": [struct.pack("<i", 17)], "value_str": [None],
                "value_i64": [18], "value_f64": [None], "value_bool": [None],
            }), path)
            report = validate(Path(directory), [
                {"group": "g", "field": "f", "type": "Int32"}
            ], compare_typed=True)
            self.assertEqual(report["failure_count"], 0)
            self.assertEqual(report["typed_mismatch_count"], 1)
            self.assertEqual(report["typed_mismatch_examples"][0]["decoded"], 17)


if __name__ == "__main__":
    unittest.main()
