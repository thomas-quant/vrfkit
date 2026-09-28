import base64
import struct
import sys
import tempfile
import unittest
from pathlib import Path

import pyarrow as pa
import pyarrow.parquet as pq


sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from validate_type_evidence import (  # noqa: E402
    decode_exact, exported_matches, spec_rows, validate)


def pack_bits(*fields: tuple[int, int]) -> tuple[bytes, int]:
    """(value, width) pairs, least significant bit first, as Unreal writes them."""
    value = position = 0
    for field, width in fields:
        value |= (field & ((1 << width) - 1)) << position
        position += width
    return value.to_bytes((position + 7) // 8, "little"), position


def write_fields(path: Path, rows: list[tuple[str, str, bytes, int]]) -> None:
    """A fields.parquet with dictionary-encoded group/field columns, as vrfkit
    writes them. `rows` are (group, field, raw_bits, bit_count)."""
    pq.write_table(pa.table({
        "group_path": pa.array([r[0] for r in rows]).dictionary_encode(),
        "field_name": pa.array([r[1] for r in rows]).dictionary_encode(),
        "handle": [1] * len(rows),
        "compatible_checksum": pa.array([7] * len(rows), pa.uint32()),
        "bit_count": [r[3] for r in rows],
        "raw_bits": [r[2] for r in rows],
        "value_str": pa.array([None] * len(rows), pa.string()),
        "value_i64": pa.array([None] * len(rows), pa.int64()),
        "value_f64": pa.array([None] * len(rows), pa.float64()),
        "value_bool": pa.array([None] * len(rows), pa.bool_()),
    }), path)


#: A payload `Int32` cannot decode: three bytes declared as 32 bits.
MALFORMED = (b"\x01\x02\x03", 32)
VALID = (struct.pack("<i", 5), 32)


class SpecificationScopeTests(unittest.TestCase):
    """Only (group, field[, checksum]) pairs the specification names are
    evidence. `validate` narrows rows by group AND field name before handing
    them to Python, then resolves the exact pair; these pin both halves."""

    def test_an_unspecified_field_of_a_specified_group_is_ignored(self):
        with tempfile.TemporaryDirectory() as directory:
            write_fields(Path(directory) / "fields.parquet", [
                ("g", "f", *VALID), ("g", "other", *MALFORMED)])
            report = validate(Path(directory),
                              [{"group": "g", "field": "f", "type": "Int32"}])
        self.assertEqual(report["failure_count"], 0)
        self.assertEqual(list(report["fields"]), ["g::f"])
        self.assertEqual(report["fields"]["g::f"]["rows"], 1)

    def test_a_group_paired_with_another_entrys_field_is_ignored(self):
        """Rows can pass a group-set x field-set prefilter without being a
        specified pair; the exact lookup after it must still drop them."""
        with tempfile.TemporaryDirectory() as directory:
            write_fields(Path(directory) / "fields.parquet", [
                ("a", "x", *VALID), ("b", "y", *VALID),
                ("a", "y", *MALFORMED), ("b", "x", *MALFORMED)])
            report = validate(Path(directory), [
                {"group": "a", "field": "x", "type": "Int32"},
                {"group": "b", "field": "y", "type": "Int32"}])
        self.assertEqual(report["failure_count"], 0)
        self.assertEqual(sorted(report["fields"]), ["a::x", "b::y"])
        self.assertEqual(report["missing"], [])

    def test_the_prefilter_keeps_only_specified_groups_and_fields_in_order(self):
        """What reaches `to_pylist`: rows of a specified group whose field is
        also specified, in file order. Group alone let every other field of
        a specified group through -- 173,510 rows materialised to use 22,989
        on one 13.06 export with the public-fixture specification."""
        table = pa.table({
            "group_path": pa.array(["g", "g", "h", "g", "h"]).dictionary_encode(),
            "field_name": pa.array(["f", "other", "f", "f", "other"]).dictionary_encode(),
            "handle": [1, 2, 3, 4, 5],
        })
        kept = spec_rows(table, pa.array(["g"]), pa.array(["f"]))
        self.assertEqual(kept["handle"].to_pylist(), [1, 4])


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


class ShapedTypeTests(unittest.TestCase):
    """The non-primitive decoders scoped entries may use.

    The base64 vectors are upstream's own recorded payloads from replay
    42e03082 (tests/Replay.Valorant.Tests/Descriptors/ClayDescriptorTests.cs at
    8b7afcb), which is not in the local corpus: an independent statement of
    what those exact bits decode to, and of their exact widths.
    """

    def test_upstream_recorded_raze_payloads_consume_their_exact_widths(self):
        self.assertEqual(decode_exact(base64.b64decode("4elLQA=="), 32, "Int32"), 1078716897)
        self.assertEqual(decode_exact(base64.b64decode("AQ=="), 3, "EnumRemainingBits"), 1)
        self.assertEqual(decode_exact(base64.b64decode("Aw=="), 3, "EnumRemainingBits"), 3)
        location = decode_exact(base64.b64decode("0yBnt6iXSAA="), 64, "VectorNetQuantize100")
        self.assertEqual(location, (-782.71, -1366.59, 5.8))
        rotation = decode_exact(base64.b64decode("AYDuJ/f/Bw=="), 51, "RotationShort")
        self.assertEqual(rotation, (90.0, 284.0350341796875, 359.989013671875))
        # Upstream's truncation case: one byte cannot hold a rotator's flags
        # and the components they announce.
        with self.assertRaisesRegex(ValueError, "truncated"):
            decode_exact(base64.b64decode("AQ=="), 8, "RotationShort")

    def test_quantized_vector_scaled_components_and_double_fallback(self):
        raw, bits = pack_bits((8 | 64, 7), (100, 8), (100, 8), (-100, 8))
        self.assertEqual(bits, 31)
        self.assertEqual(decode_exact(raw, bits, "VectorNetQuantize100"), (1.0, 1.0, -1.0))
        with self.assertRaisesRegex(ValueError, "residual"):
            decode_exact(*pack_bits((8 | 64, 7), (100, 8), (100, 8), (100, 8), (0, 1)),
                         "VectorNetQuantize100")
        with self.assertRaisesRegex(ValueError, "truncated"):
            decode_exact(*pack_bits((8 | 64, 7), (100, 8), (100, 8)), "VectorNetQuantize100")
        doubles = [struct.unpack("<Q", struct.pack("<d", v))[0] for v in (1.5, -2.25, 3.0)]
        raw, bits = pack_bits((64, 7), *((d, 64) for d in doubles))
        self.assertEqual(decode_exact(raw, bits, "VectorNetQuantize100"), (1.5, -2.25, 3.0))
        nan = struct.unpack("<I", struct.pack("<f", float("nan")))[0]
        with self.assertRaisesRegex(ValueError, "non-finite"):
            decode_exact(*pack_bits((0, 7), (nan, 32), (0, 32), (0, 32)), "VectorNetQuantize100")

    def test_rotation_short_reads_only_the_announced_components(self):
        self.assertEqual(decode_exact(*pack_bits((0, 3)), "RotationShort"), (0.0, 0.0, 0.0))
        raw, bits = pack_bits((0, 1), (1, 1), (16384, 16), (0, 1))
        self.assertEqual(bits, 19)
        self.assertEqual(decode_exact(raw, bits, "RotationShort"), (0.0, 90.0, 0.0))

    def test_rep_movement_quantization_is_decided_by_exact_consumption(self):
        # Flags, a 31-bit location, byte rotation with only yaw, and a 7-bit
        # zero-width velocity header that falls back to three floats.
        fields = [(0, 4), (8 | 64, 7), (1, 8), (2, 8), (3, 8),
                  (0, 1), (1, 1), (64, 8), (0, 1),
                  (0, 7), (0, 32), (0, 32), (0, 32)]
        raw, bits = pack_bits(*fields)
        value = decode_exact(raw, bits, "RepMovementByte")
        self.assertEqual(value["location"], (0.01, 0.02, 0.03))
        self.assertEqual(value["rotation"], (0.0, 90.0, 0.0))
        self.assertIsNone(value["angular_velocity"])
        self.assertIsNone(value["server_frame"])
        with self.assertRaises(ValueError):
            decode_exact(raw, bits, "RepMovementShort")
        raw, bits = pack_bits(*fields[:6], (1, 1), (16384, 16), *fields[8:])
        self.assertEqual(decode_exact(raw, bits, "RepMovementShort")["rotation"], (0.0, 90.0, 0.0))
        with self.assertRaises(ValueError):
            decode_exact(raw, bits, "RepMovementByte")

    def test_rep_movement_flags_gate_angular_velocity_and_packed_frames(self):
        raw, bits = pack_bits((0b0110, 4), (8 | 64, 7), (0, 8), (0, 8), (0, 8), (0, 3),
                              (8 | 64, 7), (1, 8), (1, 8), (1, 8),
                              (8 | 64, 7), (2, 8), (2, 8), (2, 8),
                              (0x59, 8), (0x04, 8))
        value = decode_exact(raw, bits, "RepMovementByte")
        self.assertEqual(value["angular_velocity"], (2.0, 2.0, 2.0))
        self.assertEqual(value["server_frame"], 300)
        self.assertIsNone(value["server_physics_handle"])
        self.assertTrue(value["rep_physics"])

    def test_enum_remaining_bits_takes_every_bit_and_refuses_wider_than_32(self):
        self.assertEqual(decode_exact(None, 0, "EnumRemainingBits"), 0)
        self.assertEqual(decode_exact(*pack_bits((6, 3)), "EnumRemainingBits"), 6)
        with self.assertRaisesRegex(ValueError, "wider than 32"):
            decode_exact(b"\0" * 5, 33, "EnumRemainingBits")

    def test_compare_typed_parses_geometry_strings(self):
        self.assertTrue(exported_matches("VectorNetQuantize100", "(1,1,-1)", (1.0, 1.0, -1.0)))
        self.assertFalse(exported_matches("VectorNetQuantize100", "(1,1,1)", (1.0, 1.0, -1.0)))
        self.assertFalse(exported_matches("VectorNetQuantize100", None, (1.0, 1.0, -1.0)))
        rotation = (90.0, 284.0350341796875, 359.989013671875)
        # Rust prints f32 in its shortest round-trip spelling: "359.989" is not
        # the double 359.989013671875, but it is that single-precision value.
        self.assertTrue(exported_matches("RotationShort", "rot(90,284.03503,359.989)", rotation))
        self.assertFalse(exported_matches("RotationShort", "rot(90,284.03503,359.98)", rotation))
        decoded = {"location": (0.01, 0.02, 0.03), "rotation": (0.0, 90.0, 0.0),
                   "linear_velocity": (0.0, 0.0, 0.0), "angular_velocity": None,
                   "simulated_physics_sleep": False, "rep_physics": False,
                   "server_frame": None, "server_physics_handle": None}
        exported = ('{"linear_velocity":{"x":0,"y":0,"z":0},"angular_velocity":null,'
                    '"location":{"x":0.01,"y":0.02,"z":0.03},'
                    '"rotation":{"pitch":0,"yaw":90,"roll":0},'
                    '"simulated_physics_sleep":false,"rep_physics":false,'
                    '"server_frame":null,"server_physics_handle":null}')
        self.assertTrue(exported_matches("RepMovementByte", exported, decoded))
        self.assertFalse(exported_matches(
            "RepMovementByte", exported.replace('"yaw":90', '"yaw":91'), decoded))
        self.assertFalse(exported_matches(
            "RepMovementByte", exported.replace('"rep_physics":false', '"rep_physics":true'), decoded))
        self.assertFalse(exported_matches("RepMovementByte", "not json", decoded))

    def test_validate_reports_vector_component_ranges(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "fields.parquet"
            first, bits = pack_bits((8 | 64, 7), (100, 8), (100, 8), (100, 8))
            second, _ = pack_bits((8 | 64, 7), (-50, 8), (100, 8), (120, 8))
            pq.write_table(pa.table({
                "group_path": ["g", "g"], "field_name": ["RelativeScale3D"] * 2,
                "handle": [6, 6], "compatible_checksum": [1992268157] * 2,
                "bit_count": [bits, bits], "raw_bits": [first, second],
                "value_str": ["(1,1,1)", "(-0.5,1,1.2)"], "value_i64": [None, None],
                "value_f64": [None, None], "value_bool": [None, None],
            }), path)
            report = validate(Path(directory), [{"group": "g", "field": "RelativeScale3D",
                                                 "type": "VectorNetQuantize100",
                                                 "checksum": 1992268157}], compare_typed=True)
            self.assertEqual(report["failure_count"], 0)
            self.assertEqual(report["typed_mismatch_count"], 0)
            field = report["fields"]["g::RelativeScale3D::checksum=1992268157"]
            self.assertEqual(field["min"], (-0.5, 1.0, 1.0))
            self.assertEqual(field["max"], (1.0, 1.0, 1.2))


if __name__ == "__main__":
    unittest.main()
