import struct
import sys
import tempfile
import unittest
from pathlib import Path

import pyarrow as pa
import pyarrow.parquet as pq


sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from validate_type_evidence import decode_exact, spec_rows, validate  # noqa: E402


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


if __name__ == "__main__":
    unittest.main()
