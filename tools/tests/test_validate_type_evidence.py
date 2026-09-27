import struct
import sys
import tempfile
import unittest
from pathlib import Path

import pyarrow as pa
import pyarrow.parquet as pq


sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from validate_type_evidence import decode_exact, validate  # noqa: E402


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

    def test_uint32_keeps_the_high_bit_positive_and_catches_a_sign_flip(self):
        # 0xe28c69d7 is a real player-state GUID word: an Int32 reading of the
        # same four bytes is negative, and only an unsigned check can tell.
        raw = struct.pack("<I", 0xE28C69D7)
        self.assertEqual(decode_exact(raw, 32, "UInt32"), 0xE28C69D7)
        with self.assertRaisesRegex(ValueError, "32 bits"):
            decode_exact(raw[:3], 24, "UInt32")
        with tempfile.TemporaryDirectory() as directory:
            pq.write_table(pa.table({
                "group_path": ["g", "g"], "field_name": ["D", "D"], "handle": [210, 210],
                "compatible_checksum": [1032080829, 1032080829], "bit_count": [32, 32],
                "raw_bits": [raw, raw], "value_str": [None, None],
                "value_i64": [0xE28C69D7, 0xE28C69D7 - 2**32],
                "value_f64": [None, None], "value_bool": [None, None],
            }), Path(directory) / "fields.parquet")
            spec = {"group": "g", "field": "D", "type": "UInt32", "checksum": 1032080829}
            report = validate(Path(directory), [spec], compare_typed=True)
            self.assertEqual(report["failure_count"], 0)
            self.assertEqual(report["typed_mismatch_count"], 1)
            self.assertEqual(report["typed_mismatch_examples"][0]["exported"], 0xE28C69D7 - 2**32)
            self.assertEqual(report["fields"]["g::D::checksum=1032080829"]["min"], 0xE28C69D7)

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
