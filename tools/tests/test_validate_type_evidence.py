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
    decode_exact, exported_matches, exported_value, spec_rows, validate,
    values_match)


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
            write_manifest(Path(directory))
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
            write_manifest(Path(directory))
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
                values_match(type_name, decoded,
                             exported_value({"value_str": text}, type_name)),
                (True, "/100"), type_name)

    def test_the_location_scale_is_reported_not_assumed(self):
        """The same bits exported in world units still match, at "/1"; a
        scale nobody uses, or two scales in one vector, does not."""
        hex_bits, bits, text = DIVEBOMB_BYTE
        decoded = decode_exact(bytes.fromhex(hex_bits), bits, "RepMovementByte")
        self.assertEqual(decoded["location"], {"packed": (2525, -4404, 680), "scaled": True})
        for location, expected in (
            ('{"x":2525,"y":-4404,"z":680}', (True, "/1")),
            ('{"x":252.5,"y":-440.4,"z":68}', (False, None)),
            ('{"x":2525,"y":-44.04,"z":6.8}', (False, None)),
        ):
            world = text.replace('{"x":25.25,"y":-44.04,"z":6.8}', location)
            self.assertEqual(
                values_match("RepMovementByte", decoded,
                             exported_value({"value_str": world}, "RepMovementByte")),
                expected, location)

    def test_the_wrong_rotator_width_is_caught(self):
        """A Byte row read as Short runs off the end. The reverse can consume
        exactly and still be wrong -- which only the value compare sees."""
        hex_bits, bits, _text = DIVEBOMB_BYTE
        with self.assertRaisesRegex(ValueError, "past the end"):
            decode_exact(bytes.fromhex(hex_bits), bits, "RepMovementShort")
        hex_bits, bits, text = SEEKER_NADE_SHORT
        as_byte = decode_exact(bytes.fromhex(hex_bits), bits, "RepMovementByte")
        self.assertEqual(
            values_match("RepMovementByte", as_byte,
                         exported_value({"value_str": text}, "RepMovementByte")),
            (False, None))

    def test_rep_movement_optional_members_are_read_in_wire_order(self):
        # flags: physics, server frame and server handle set; every vector and
        # rotator component present and non-zero.
        raw, bits = pack_bits(
            (0, 1), (1, 1), (1, 1), (1, 1),
            (70, 7), (5, 6), (-3, 6), (1, 6),         # location, "scaled" set
            (1, 1), (64, 8), (0, 1), (1, 1), (128, 8),  # pitch, no yaw, roll
            (6, 7), (-7, 6), (0, 6), (31, 6),          # velocity, unscaled
            (6, 7), (1, 6), (2, 6), (3, 6),            # angular velocity
            (0x06, 8), (0x0a, 8),                      # frame 3, handle 5
        )
        self.assertEqual(decode_exact(raw, bits, "RepMovementByte"), {
            "linear_velocity": (-7.0, 0.0, 31.0),
            "angular_velocity": (1.0, 2.0, 3.0),
            "location": {"packed": (5, -3, 1), "scaled": True},
            "rotation": (90.0, 0.0, 180.0),
            "simulated_physics_sleep": False,
            "rep_physics": True,
            "server_frame": 3,
            "server_physics_handle": 5,
        })

    def test_an_unparseable_export_is_a_mismatch_not_a_crash(self):
        hex_bits, bits, _text = DIVEBOMB_BYTE
        decoded = decode_exact(bytes.fromhex(hex_bits), bits, "RepMovementByte")
        self.assertEqual(
            values_match("RepMovementByte", decoded,
                         exported_value({"value_str": "{not json"}, "RepMovementByte")),
            (False, None))

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
                write_manifest(Path(directory))
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
        self.assertEqual(clean["fields"]["g::m"]["location_scales"], {"/100": 1})
        wrong = [
            ("g", "e", 3, bytes.fromhex("05"), 4, None),
            ("g", "n", 97, bytes.fromhex("08000000a4cac8000000000000"), None, "Blue"),
            ("g", "m", DIVEBOMB_BYTE[1], bytes.fromhex(DIVEBOMB_BYTE[0]), None,
             DIVEBOMB_BYTE[2].replace("347.34375", "348.75")),
        ]
        self.assertEqual(run(wrong)["typed_mismatch_count"], 3)


def write_manifest(directory: Path) -> None:
    """Every published export has one; discovery refuses a table without it."""
    (directory / "manifest.json").write_text('{"replay_build": "13.02"}', encoding="utf-8")


def write_int_export(directory: Path, *, manifest: bool = True) -> None:
    directory.mkdir(parents=True)
    pq.write_table(pa.table({
        "group_path": ["g"], "field_name": ["f"], "handle": [1],
        "compatible_checksum": [2], "bit_count": [32],
        "raw_bits": [struct.pack("<i", 17)], "value_str": [None],
        "value_i64": [17], "value_f64": [None], "value_bool": [None],
    }), directory / "fields.parquet")
    if manifest:
        write_manifest(directory)


class ExportDiscoveryTests(unittest.TestCase):
    SPEC = [{"group": "g", "field": "f", "type": "Int32"}]

    def test_a_stranded_prior_output_is_not_counted_a_second_time(self):
        """The silent case: `.X.vrfkit-previous-*` is a complete export.

        `vrfkit export` moves the prior `--out` there for one rename and leaves
        it when it cannot delete it, or when it dies between the two renames.
        At 259ed10 the rglob here read it as a second export: rows 2, not 1,
        with no error -- a plausible number.
        """
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_int_export(root / "a")
            write_int_export(root / ".a.vrfkit-previous-4242-7")
            expected = [str((root / ".a.vrfkit-previous-4242-7").resolve())]
            report = validate(root, self.SPEC, compare_typed=True)
        self.assertEqual(report["fields"]["g::f"]["rows"], 1)
        self.assertEqual(report["typed_mismatch_count"], 0)
        self.assertEqual(report["skipped_generated_dirs"], expected)

    def test_a_killed_exports_staging_directory_is_not_read(self):
        """What `Stop-Process -Force` 1.5 s into an export left: no footer."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_int_export(root / "pub2")
            staging = root / ".pub2.vrfkit-staging-55396-0"
            staging.mkdir()
            (staging / "fields.parquet").write_bytes(b"PAR1 no footer")
            report = validate(root, self.SPEC)
        self.assertEqual(report["fields"]["g::f"]["rows"], 1)
        self.assertEqual(len(report["skipped_generated_dirs"]), 1)

    def test_a_leftover_nested_below_the_root_is_skipped_too(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_int_export(root / "build" / "a")
            write_int_export(root / "build" / ".a.vrfkit-previous-4242-7" / "nested")
            report = validate(root, self.SPEC)
        self.assertEqual(report["fields"]["g::f"]["rows"], 1)

    def test_a_discovered_table_without_a_manifest_is_refused(self):
        """manifest.json is written last; without it the tables may be partial."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_int_export(root / "complete")
            write_int_export(root / "partial", manifest=False)
            with self.assertRaisesRegex(ValueError, "manifest.json"):
                validate(root, self.SPEC)

    def test_only_leftovers_below_the_root_is_an_error_that_counts_them(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_int_export(root / ".a.vrfkit-previous-4242-7")
            with self.assertRaisesRegex(ValueError, r"no field parquet files.*1 "):
                validate(root, self.SPEC)

    def test_a_path_named_explicitly_is_never_filtered(self):
        """Only discovery filters; pointing the tool at a leftover is deliberate."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            leftover = root / ".a.vrfkit-previous-4242-7"
            write_int_export(leftover)
            for report in (
                validate(leftover, self.SPEC),
                validate(leftover / "fields.parquet", self.SPEC),
                validate(root, self.SPEC, export_ids=[leftover.name]),
            ):
                self.assertEqual(report["fields"]["g::f"]["rows"], 1)
                self.assertEqual(report["skipped_generated_dirs"], [])


class DecodeExactTests(unittest.TestCase):
    def test_checksum_scope_separates_same_name_and_catches_wrong_values(self):
        with tempfile.TemporaryDirectory() as directory:
            write_manifest(Path(directory))
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
            write_manifest(Path(directory))
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
            write_manifest(Path(directory))
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
        # The location stays packed: its scale is not on the wire (see
        # BitLevelTypeTests.test_the_location_scale_is_reported_not_assumed).
        self.assertEqual(value["location"], {"packed": (1, 2, 3), "scaled": True})
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
        decoded = {"location": {"packed": (1, 2, 3), "scaled": True},
                   "rotation": (0.0, 90.0, 0.0),
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
            write_manifest(Path(directory))
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
