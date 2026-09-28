import base64
import contextlib
import io
import json
import math
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import numpy
import pyarrow as pa
import pyarrow.parquet as pq


sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import to_valplay_bundle as bundle  # noqa: E402


OLD_GUARDIAN = "/Game/Equippables/Guns/SniperRifles/Dmr/DMR.DMR_C"
NEW_GUARDIAN = "/Game/Equippables/Guns/SniperRifles/DMR/DMR.DMR_C"


class EquippableResolutionTests(unittest.TestCase):
    def test_guardian_directory_alias_resolves_at_firing_state_seam(self):
        resolved = bundle._resolve_equippable(
            41,
            {41: 42},
            {},
            {42: NEW_GUARDIAN},
        )

        self.assertEqual(
            resolved,
            (42, "Guardian", "rifle", OLD_GUARDIAN),
        )

    def test_unmeasured_guardian_case_variant_stays_unresolved(self):
        resolved = bundle._resolve_equippable(
            41,
            {41: 42},
            {},
            {42: NEW_GUARDIAN.replace("/DMR/", "/dMR/")},
        )

        self.assertIsNone(resolved)


def write_fields_parquet(path: Path, rows: list[dict]) -> None:
    """Write a fields.parquet with the column set the bundle reads.

    Module level so every test class can build an export; `rows` carries only
    the columns a case cares about and the rest default to null.
    """
    def values(name, default=None):
        return [row.get(name, default) for row in rows]

    table = pa.table(
        {
            "time_ms": pa.array(values("time_ms"), type=pa.uint32()),
            "packet_id": pa.array(values("packet_id"), type=pa.uint32()),
            "channel_index": pa.array(values("channel_index", 7), type=pa.uint32()),
            "actor_net_guid": pa.array(values("actor"), type=pa.uint32()),
            "object_net_guid": pa.array(values("object"), type=pa.uint32()),
            "group_path": pa.array(values("group_path"), type=pa.string()),
            "handle": pa.array(values("handle", 0), type=pa.uint32()),
            "field_name": pa.array(values("field_name"), type=pa.string()),
            "bit_count": pa.array(values("bit_count"), type=pa.uint32()),
            "raw_bits": pa.array(values("raw_bits"), type=pa.binary()),
            "value_i64": pa.array(values("value_i64"), type=pa.int64()),
            "value_f64": pa.array(values("value_f64"), type=pa.float64()),
            "value_bool": pa.array(values("value_bool"), type=pa.bool_()),
            "value_str": pa.array(values("value_str"), type=pa.string()),
        }
    )
    pq.write_table(table, path)


def write_actors_parquet(path: Path, rows: list[dict]) -> None:
    """Write an actors.parquet with the columns `_build_actor_events` reads.

    Column types follow `crates/vrf-export/src/schema.rs::actors_schema` --
    `class_path`/`archetype_path` are dictionary-encoded there, and the adapter
    has a separate code path for dictionary columns, so encoding them as plain
    strings here would exercise the wrong branch.
    """
    def values(name, default=None):
        return [row.get(name, default) for row in rows]

    def u32(name):
        return pa.array([row.get(name, 0) for row in rows], type=pa.uint32())

    def f32(name):
        return pa.array(values(name), type=pa.float32())

    dict_type = pa.dictionary(pa.int32(), pa.string())
    table = pa.table(
        {
            "time_ms": u32("time_ms"),
            "packet_id": u32("packet_id"),
            "channel_index": u32("channel_index"),
            "actor_net_guid": u32("actor"),
            "event": pa.array(values("event"), type=pa.string()),
            "class_path": pa.array(values("class_path"), type=pa.string()).dictionary_encode().cast(dict_type),
            "archetype_path": pa.array(values("archetype_path"), type=pa.string()).dictionary_encode().cast(dict_type),
            "spawn_x": f32("spawn_x"),
            "spawn_y": f32("spawn_y"),
            "spawn_z": f32("spawn_z"),
            "spawn_pitch": f32("spawn_pitch"),
            "spawn_yaw": f32("spawn_yaw"),
            "spawn_roll": f32("spawn_roll"),
        }
    )
    pq.write_table(table, path)


#: One innocuous replicated property, so `convert` has a fields.parquet to read
#: when the case under test is about some other table.
MINIMAL_FIELD_ROWS = [
    {
        "time_ms": 10,
        "packet_id": 1,
        "actor": 101,
        "group_path": "PlayerState",
        "field_name": "Health",
        "bit_count": 32,
        "value_i64": 100,
    },
]


def write_movement_parquet(path: Path, rows: list[dict]) -> None:
    """Write a movement.parquet with the columns `_write_movement` reads."""
    def values(name, default=0.0):
        return [row.get(name, default) for row in rows]

    def u32(name):
        return pa.array([row.get(name, 0) for row in rows], type=pa.uint32())

    table = pa.table(
        {
            "time_ms": u32("time_ms"),
            "packet_id": u32("packet_id"),
            "character_net_guid": u32("char"),
            "pos_x": pa.array(values("pos_x"), type=pa.float32()),
            "pos_y": pa.array(values("pos_y"), type=pa.float32()),
            "pos_z": pa.array(values("pos_z"), type=pa.float32()),
            "yaw": pa.array(values("yaw"), type=pa.float32()),
            "pitch": pa.array(values("pitch"), type=pa.float32()),
            "vel_x": pa.array(values("vel_x"), type=pa.float32()),
            "vel_y": pa.array(values("vel_y"), type=pa.float32()),
            "vel_z": pa.array(values("vel_z"), type=pa.float32()),
        }
    )
    pq.write_table(table, path)


class MovementCollapseTests(unittest.TestCase):
    """The bundle keeps the final move PER PACKET, which is what the reference has.

    Every sub-move decoded out of one movement RPC is stamped with the
    time_ms and packet_id the sink hoisted before the loop
    (vrfkit/src/sink/stream.rs, decode_movement_rpc), so all sub-moves of one
    packet share both. Collapsing on the millisecond therefore also merges
    two DIFFERENT packets that land in the same millisecond, and the earlier
    packet's final move -- a real, distinct sample -- disappears.
    """

    @staticmethod
    def convert_movement(root: Path, rows: list[dict]) -> list[str]:
        export = root / "export"
        out = root / "bundle"
        export.mkdir()
        write_fields_parquet(export / "fields.parquet", MINIMAL_FIELD_ROWS)
        write_movement_parquet(export / "movement.parquet", rows)
        bundle.convert(export, out)
        text = (out / "movement.ndjson").read_text(encoding="utf-8")
        return text.splitlines()

    def test_two_packets_in_one_millisecond_each_keep_their_final_move(self):
        rows = [
            # Packet 1: two sub-moves, only the second survives.
            {"time_ms": 100, "packet_id": 1, "char": 42, "pos_x": 1.0},
            {"time_ms": 100, "packet_id": 1, "char": 42, "pos_x": 2.0},
            # Packet 2, same millisecond, same character: a separate final
            # move that must NOT be treated as packet 1's sub-move.
            {"time_ms": 100, "packet_id": 2, "char": 42, "pos_x": 3.0},
        ]
        with tempfile.TemporaryDirectory() as tmp:
            lines = self.convert_movement(Path(tmp), rows)

        self.assertEqual(len(lines), 2, lines)
        self.assertIn('"x":2', lines[0])
        self.assertIn('"x":3', lines[1])

    def test_sub_moves_within_one_packet_are_still_collapsed(self):
        rows = [
            {"time_ms": 100, "packet_id": 1, "char": 42, "pos_x": 1.0},
            {"time_ms": 100, "packet_id": 1, "char": 42, "pos_x": 2.0},
            {"time_ms": 100, "packet_id": 1, "char": 43, "pos_x": 9.0},
        ]
        with tempfile.TemporaryDirectory() as tmp:
            lines = self.convert_movement(Path(tmp), rows)

        self.assertEqual(len(lines), 2, lines)
        self.assertIn('"x":2', lines[0])
        self.assertIn('"x":9', lines[1])


class MovementTruncationTests(unittest.TestCase):
    """A replay with no movement table must not inherit the previous one's.

    `convert` reuses an existing output directory, so converting replay B over
    replay A's bundle left A's movement.ndjson sitting beside B's events while
    the run reported success.
    """

    def test_missing_movement_table_truncates_a_reused_bundle_directory(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            with_mv = root / "with_movement"
            without_mv = root / "without_movement"
            out = root / "bundle"
            with_mv.mkdir()
            without_mv.mkdir()
            write_fields_parquet(with_mv / "fields.parquet", MINIMAL_FIELD_ROWS)
            write_fields_parquet(without_mv / "fields.parquet", MINIMAL_FIELD_ROWS)
            write_movement_parquet(
                with_mv / "movement.parquet",
                [{"time_ms": 100, "packet_id": 1, "char": 42, "pos_x": 1.0}],
            )

            bundle.convert(with_mv, out)
            self.assertNotEqual(
                (out / "movement.ndjson").read_text(encoding="utf-8"), ""
            )

            summary = bundle.convert(without_mv, out)

            self.assertEqual(summary["movement_written"], 0)
            self.assertTrue(
                (out / "movement.ndjson").exists(),
                "movement.ndjson was neither written nor truncated",
            )
            self.assertEqual(
                (out / "movement.ndjson").read_text(encoding="utf-8"), ""
            )


#: float32 values where a vectorised shortcut and the per-value encoder can
#: disagree. Each is compared with the per-value rule the bulk path replaced
#: -- `_JSON.encode(_f32_shortest(v))` for the shortened columns and
#: `_JSON.encode(v)` for yaw/pitch -- never with a literal, so the test pins
#: the contract and not today's spelling of it.
TEXT_RULE_EDGES = (
    float("inf"), float("-inf"), float("nan"),
    # >= 2**24 every float32 is an integer, but not every one prints as its
    # exact integer: 123456792 round-trips as 123456790.
    123456792.0, 1e16, 1e20,
    3.4028234663852886e38, -3.4028234663852886e38,        # +/- FLT_MAX
    16777216.0, 16777215.0, -16777215.0, 33554436.0,
    8388608.0, 12345670.0,
    # numpy writes non-integral values from 1e6 up in scientific notation;
    # the rule writes them positional. 999999.94 is the last one both agree on.
    8388607.5, 1234567.5, -1234567.5, 1000000.0625, 999999.94,
    # float32(1e-4) is 9.99999975e-05: numpy judges the binary value and
    # writes '1e-04', repr judges the decimal and writes '0.0001'.
    1e-4, -1e-4, 1.0000000474974513e-04, 9.9e-05,
    1e-05, 1e-07, 1e-45, 1.1754943508222875e-38,          # exponent form,
    #                                                       subnormal, min normal
    2382.2, 349.99, -349.99, 253.289794921875, 0.1, 1.5,
    0.0, -0.0,
)


def column_text(values, *, shorten):
    """Per-row text `_json_scalar_column` produces for `values` as float32:
    its distinct texts fanned back out through its inverse, as the writer
    does."""
    arr = numpy.array(values, dtype=numpy.float32)
    texts, inverse = bundle._json_scalar_column(arr, shorten=shorten)
    return texts.take(inverse).to_pylist()


def per_value_text(values, *, shorten):
    """The oracle: one encoder call per row, the rule before vectorisation."""
    encode = bundle._JSON.encode
    widened = [float(v) for v in numpy.array(values, dtype=numpy.float32)]
    if shorten:
        return [encode(bundle._f32_shortest(v)) for v in widened]
    return [encode(v) for v in widened]


class MovementTextRuleTests(unittest.TestCase):
    """Movement text is built once per DISTINCT value and fanned out.

    That is only sound if the text of a distinct value is exactly what the
    per-value encoder wrote for it. The bulk shortcut (numpy's Dragon4 text
    plus an int64 fix-up for integral values) was applied to every value and
    is exact only on part of the float32 range: +/-inf went through the int64
    cast and came out as -9223372036854775808, NaN printed as the invalid-JSON
    `nan`, integral values above 2**24 printed their exact integer instead of
    the shortest round-trip one, and non-integral values outside
    [1e-4, 1e6) came out in numpy's scientific notation -- with, at most, a
    numpy RuntimeWarning as the only signal.
    """

    def test_shortened_columns_match_the_per_value_encoder(self):
        self.assertEqual(
            column_text(TEXT_RULE_EDGES, shorten=True),
            per_value_text(TEXT_RULE_EDGES, shorten=True),
        )

    def test_unshortened_columns_match_the_per_value_encoder(self):
        self.assertEqual(
            column_text(TEXT_RULE_EDGES, shorten=False),
            per_value_text(TEXT_RULE_EDGES, shorten=False),
        )

    def test_a_candidate_rounded_past_float32_is_skipped_on_every_python(self):
        # Python 3.13 made struct.pack("f", x) raise OverflowError where 3.12
        # returned inf. Shortening FLT_MAX tries 3.403e+38 on the way, so the
        # search must treat that as "does not round-trip" on both. Emulate
        # 3.13 here so the property is checked whichever Python runs it.
        real = bundle._struct

        class Strict:
            unpack = staticmethod(real.unpack)

            @staticmethod
            def pack(fmt, *values):
                out = real.pack(fmt, *values)
                if fmt.endswith("f") and any(
                        math.isfinite(v) and math.isinf(real.unpack(fmt, real.pack(fmt, v))[0])
                        for v in values):
                    raise OverflowError("float too large to pack with f format")
                return out

        flt_max = 3.4028234663852886e38
        with mock.patch.object(bundle, "_struct", Strict):
            self.assertEqual(bundle._f32_shortest(flt_max), 3.4028235e38)
            self.assertEqual(bundle._f32_shortest(-flt_max), -3.4028235e38)
            self.assertEqual(
                column_text(TEXT_RULE_EDGES, shorten=True),
                per_value_text(TEXT_RULE_EDGES, shorten=True),
            )

    def test_non_finite_values_are_spelled_by_the_encoder(self):
        """Spelled out too, so a failure names the three values that broke."""
        for shorten in (True, False):
            with self.subTest(shorten=shorten):
                self.assertEqual(
                    column_text([float("inf"), float("-inf"), float("nan")],
                                shorten=shorten),
                    ["Infinity", "-Infinity", "NaN"],
                )

    def test_each_zero_keeps_its_own_sign(self):
        """-0.0 == 0.0, so a value-level unique merges them into one entry and
        prints whichever sign sorted first for every zero in the column. Both
        orders, because which sign wins depends on the sort's tie-break.
        """
        for values in ([0.0, -0.0, 5.0, -0.0], [-0.0, 0.0, 5.0, 0.0]):
            with self.subTest(values=values):
                self.assertEqual(column_text(values, shorten=False),
                                 per_value_text(values, shorten=False))
                self.assertEqual(column_text(values, shorten=True),
                                 per_value_text(values, shorten=True))


#: The movement line as the per-row writer spelled it before the lines were
#: assembled in Arrow -- a COPY, deliberately not `_MOVEMENT_LINE`, so that a
#: change to the constant or to how the writer derives its fragments turns
#: the oracle test red instead of moving both sides at once.
ORACLE_MOVEMENT_LINE = (
    '{"time_ms":%s,"shooter_character_net_guid":%s,'
    '"position":{"x":%s,"y":%s,"z":%s},'
    '"velocity":{"x":%s,"y":%s,"z":%s},'
    '"yaw":%s,"pitch":%s}\n'
)


def oracle_movement_bytes(rows: list[dict]) -> bytes:
    """movement.ndjson as the per-row writer produced it, value by value.

    Keeps the last row per (packet_id, character) in row order -- the
    collapse rule -- encodes every value with its own encoder call, and ends
    lines the way the old text-mode file did: os.linesep.
    """
    last = {}
    for i, row in enumerate(rows):
        last[(row.get("packet_id", 0), row.get("char", 0))] = i
    encode = bundle._JSON.encode

    def f32(row, name):
        return float(numpy.float32(row.get(name, 0.0)))

    lines = []
    for i in sorted(last.values()):
        row = rows[i]
        lines.append(ORACLE_MOVEMENT_LINE % (
            row.get("time_ms", 0), row.get("char", 0),
            *(encode(bundle._f32_shortest(f32(row, n)))
              for n in ("pos_x", "pos_y", "pos_z", "vel_x", "vel_y", "vel_z")),
            encode(f32(row, "yaw")), encode(f32(row, "pitch")),
        ))
    return "".join(lines).replace("\n", os.linesep).encode("ascii")


def oracle_rows() -> list[dict]:
    """Integral, -0.0, exponent-form, large, non-finite and ordinary values,
    sub-moves to collapse, two characters, and large ids -- enough rows to
    span several write blocks at the block sizes the test patches in."""
    values = [0.0, -0.0, 1.0, -3.0, 1e-05, 1e-07, 2382.2, 349.99, -349.99,
              0.1, 1234567.5, 123456792.0, 16777215.0, 1e-4, 51292.77,
              float("inf"), float("-inf"), float("nan"), 253.289794921875]
    rows = []
    for i in range(61):
        v = values[i % len(values)]
        w = values[(i * 7 + 3) % len(values)]
        rows.append({
            "time_ms": 1000 + i // 3, "packet_id": 1 + i // 3,
            "char": (40, 4294967295)[i % 2],
            "pos_x": v, "pos_y": w, "pos_z": -v,
            "vel_x": w, "vel_y": v * 0.5, "vel_z": 0.0 if i % 5 else -0.0,
            "yaw": (0.0, -0.0, 359.9945068359375, 253.289794921875)[i % 4],
            "pitch": (-0.0, 0.0, 1e-05, 90.5)[i % 4],
        })
    return rows


class MovementLineAssemblyTests(unittest.TestCase):
    """movement.ndjson is the per-row writer's bytes, block by block.

    The lines are assembled in Arrow from per-distinct texts; the oracle
    builds them one row and one encoder call at a time.
    """

    def write(self, rows: list[dict], block_rows: int) -> bytes:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            write_movement_parquet(root / "movement.parquet", rows)
            out = root / "out"
            out.mkdir()
            with mock.patch.object(bundle, "_MOVEMENT_BLOCK_ROWS", block_rows):
                bundle._write_movement(root / "movement.parquet", out, False)
            return (out / "movement.ndjson").read_bytes()

    def test_bytes_equal_the_per_row_writer(self):
        rows = oracle_rows()
        expected = oracle_movement_bytes(rows)
        # Guards on the oracle itself: sub-moves were collapsed, and the kept
        # lines span several of the 7-row blocks below with a partial last one.
        lines = expected.count(os.linesep.encode())
        self.assertLess(lines, len(rows))
        self.assertGreater(lines, 14)
        self.assertNotEqual(lines % 7, 0)
        # One block, full blocks plus a partial last one, one row per block.
        for block_rows in (1 << 18, 7, 1):
            with self.subTest(block_rows=block_rows):
                self.assertEqual(self.write(rows, block_rows), expected)

    def test_lines_end_the_way_the_text_mode_file_ended_them(self):
        data = self.write(oracle_rows(), 7)
        self.assertEqual(data.count(os.linesep.encode()), data.count(b"\n"))
        self.assertTrue(data.endswith(os.linesep.encode()))

    def test_an_empty_table_writes_an_empty_file(self):
        self.assertEqual(self.write([], 7), b"")

    def test_a_null_in_a_movement_column_stops_the_conversion(self):
        """Every movement column is declared non-null. `to_numpy` would turn a
        null uint32 into a float64 NaN, and every line would then carry a
        float-spelled time -- plausible text, wrong type."""
        for column in ("time_ms", "pos_x"):
            with self.subTest(column=column):
                with tempfile.TemporaryDirectory() as tmp:
                    root = Path(tmp)
                    path = root / "movement.parquet"
                    write_movement_parquet(path, oracle_rows()[:3])
                    table = pq.read_table(path)
                    index = table.schema.get_field_index(column)
                    values = table.column(column).to_pylist()
                    values[1] = None
                    table = table.set_column(index, column, pa.array(
                        values, type=table.schema.field(column).type))
                    pq.write_table(table, path)
                    out = root / "out"
                    out.mkdir()
                    with self.assertRaisesRegex(ValueError, column):
                        bundle._write_movement(path, out, False)

    def test_a_null_line_is_refused_rather_than_dropped(self):
        """`binary_join_element_wise` emits NULL for a row with a null input,
        and a null adds no bytes to the data buffer: the line would vanish
        while movement_rows_written still counted it."""
        texts = pa.array(["1", None, "3"], type=pa.string())
        with self.assertRaises(RuntimeError):
            bundle._join_movement_block([pa.scalar("<"), texts, pa.scalar(">")])
        self.assertEqual(
            bytes(bundle._join_movement_block(
                [pa.scalar("<"), texts.fill_null("2"), pa.scalar(">")])),
            b"<1><2><3>")

    def test_only_the_arrays_own_bytes_are_written(self):
        """A slice shares its parent's data buffer, so the buffer holds bytes
        before and after the slice's values; only the values are the text."""
        sliced = pa.array(["a", "bb", "ccc", "dddd"], type=pa.string()).slice(1, 2)
        self.assertEqual(bytes(bundle._string_bytes(sliced)), b"bbccc")


class TransactionalConversionTests(unittest.TestCase):
    @staticmethod
    def snapshot(path: Path) -> dict[str, bytes]:
        return {
            item.relative_to(path).as_posix(): item.read_bytes()
            for item in sorted(path.rglob("*"))
            if item.is_file()
        }

    @staticmethod
    def make_export(path: Path) -> bytes:
        path.mkdir(parents=True)
        write_fields_parquet(path / "fields.parquet", MINIMAL_FIELD_ROWS)
        manifest = b'{"replay_version":"source","source_file":"match.vrf"}\n'
        (path / "manifest.json").write_bytes(manifest)
        return manifest

    def test_input_and_output_may_not_be_the_same_directory(self):
        with tempfile.TemporaryDirectory() as temp:
            export = Path(temp) / "export"
            source_manifest = self.make_export(export)
            before = self.snapshot(export)

            with self.assertRaises(ValueError):
                bundle.convert(export, export)

            self.assertEqual(self.snapshot(export), before)
            self.assertEqual((export / "manifest.json").read_bytes(), source_manifest)

    def test_output_nested_inside_input_is_rejected_before_writing(self):
        with tempfile.TemporaryDirectory() as temp:
            export = Path(temp) / "export"
            self.make_export(export)
            output = export / "bundle"

            with self.assertRaises(ValueError):
                bundle.convert(export, output)

            self.assertFalse(output.exists())

    def test_input_nested_inside_output_is_rejected_before_writing(self):
        with tempfile.TemporaryDirectory() as temp:
            output = Path(temp) / "output"
            export = output / "export"
            self.make_export(export)
            before = self.snapshot(output)

            with self.assertRaises(ValueError):
                bundle.convert(export, output)

            self.assertEqual(self.snapshot(output), before)

    def test_conversion_failure_preserves_an_existing_complete_bundle(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            export = root / "export"
            export.mkdir()
            (export / "fields.parquet").write_bytes(b"not parquet")
            (export / "manifest.json").write_text(
                '{"replay_version":"new"}', encoding="utf-8"
            )
            output = root / "bundle"
            output.mkdir()
            for name, content in {
                "manifest.json": "{\"replay_version\":\"old\"}",
                "events.ndjson": "{\"type\":\"old\"}\n",
                "movement.ndjson": "",
                ".complete": "old marker",
            }.items():
                (output / name).write_text(content, encoding="utf-8")
            before = self.snapshot(output)

            with self.assertRaises(Exception):
                bundle.convert(export, output)

            self.assertEqual(self.snapshot(output), before)
            self.assertEqual(
                [p for p in root.iterdir() if p.name.startswith(".bundle.")], []
            )

    def test_success_never_modifies_the_source_manifest(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            export = root / "export"
            original = self.make_export(export)

            bundle.convert(export, root / "bundle")

            self.assertEqual((export / "manifest.json").read_bytes(), original)

    def test_the_summary_names_the_published_bundle(self):
        """The summary used to print from inside the staging step, naming a
        `.bundle.*` directory the publish then renamed away."""
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            self.make_export(root / "export")
            output = root / "bundle"
            printed = io.StringIO()
            with contextlib.redirect_stdout(printed):
                bundle.convert(root / "export", output)
            summary = [ln for ln in printed.getvalue().splitlines()
                       if ln.startswith("Conversion ")]
            self.assertEqual(len(summary), 1, printed.getvalue())
            self.assertTrue(summary[0].endswith(": " + str(output.resolve())), summary)
            self.assertTrue(output.is_dir())

    def test_a_failed_publish_prints_no_completion_claim(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            self.make_export(root / "export")
            printed = io.StringIO()
            with mock.patch.object(bundle, "_publish_bundle",
                                   side_effect=OSError("disk full")),                     contextlib.redirect_stdout(printed),                     self.assertRaises(OSError):
                bundle.convert(root / "export", root / "bundle")
            self.assertNotIn("Conversion ", printed.getvalue())

    def test_backup_cleanup_failure_does_not_turn_a_committed_publish_into_failure(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            staging = root / ".bundle.staging"
            output = root / "bundle"
            staging.mkdir()
            output.mkdir()
            (staging / "manifest.json").write_text("new", encoding="utf-8")
            (output / "manifest.json").write_text("old", encoding="utf-8")

            real_remove_tree = bundle.remove_tree

            def fail_cleanup(path, parent):
                raise OSError(f"cannot remove {path} under {parent}")

            bundle.remove_tree = fail_cleanup
            stderr = io.StringIO()
            try:
                with contextlib.redirect_stderr(stderr):
                    bundle._publish_bundle(staging, output)
            finally:
                bundle.remove_tree = real_remove_tree

            self.assertEqual(
                (output / "manifest.json").read_text(encoding="utf-8"), "new"
            )
            backups = list(root.glob(".bundle.backup.*"))
            self.assertEqual(len(backups), 1, backups)
            self.assertEqual(
                (backups[0] / "manifest.json").read_text(encoding="utf-8"), "old"
            )
            self.assertIn("backup", stderr.getvalue().lower())


class ShotEventTests(unittest.TestCase):
    def build_shot(self, scalar_params: dict) -> dict:
        return bundle._build_shot_event(
            bundle._ShotContext(tag_table={}),
            12788,
            4368,
            2,
            22,
            1,
            scalar_params,
            bundle._EffectBlobs(),
        )["shot"]

    def test_typed_and_raw_runtime_names_produce_identical_shot_geometry(self):
        raw = self.build_shot(
            {
                "248": {
                    "BitCount": 192,
                    "Data": "mpmZmZmVc8CkcD0KV5W9wM3MzMzMZHtA",
                },
                "249": {"BitCount": 35, "Data": "l/pPkgE="},
            }
        )
        typed = self.build_shot(
            {
                "248": "(-313.35,-7573.34,438.3)",
                "249": "rot(356.19324,141.4325,0)",
            }
        )

        self.assertEqual(typed["location"], raw["location"])
        self.assertEqual(typed["rotation"], raw["rotation"])
        self.assertEqual(
            typed["location"],
            {"x": -313.35, "y": -7573.34, "z": 438.3},
        )
        self.assertEqual(
            typed["rotation"],
            {
                "pitch": 356.1932373046875,
                "yaw": 141.4324951171875,
                "roll": 0.0,
            },
        )


class EffectBlobBitLengthTests(unittest.TestCase):
    """The bit length must come from the parser, not from the byte length.

    Parquet stores whole bytes, so a payload of N bits arrives as ceil(N/8)
    bytes with up to 7 padding bits in the last one. Deriving the length as
    `len(data) * 8` hands those padding bits to the decoder as data.

    Every effect blob measured -- 692,840 across the 11 cross-validated
    replays -- has `bit_count == len(raw_bits) * 8`, so the two readings agree
    on all real data and no corpus check can tell them apart. That is exactly
    why this needs a test: the wrong reading is currently invisible.
    """

    SPEC = bundle._EFFECT_FLOATS

    # A real FloatValues payload lifted from 02d4d478's fields.parquet: 50
    # bytes, declared 400 bits, four complete tag/value pairs. It is also the
    # first of the eight vectors pinned in crates/vrf-decode/src/effect.rs.
    BLOB = bytes.fromhex(
        "08021020390412400000803f000410200f0412400000a040"
        "000610203d0412400000803f000810203b04124015f9b3ce0000"
    )
    FOUR_PAIRS = {"284": 1.0, "263": 5.0, "286": 1.0, "285": -1509722752.0}
    THREE_PAIRS = {"284": 1.0, "263": 5.0, "286": 1.0}

    def decode(self, data: bytes, bit_count: int) -> dict:
        return bundle._decode_effect_blob(
            bundle._EffectBlob(data, bit_count), self.SPEC, {}
        )

    def test_declared_length_is_used_rather_than_the_byte_length(self):
        # Same 50 bytes of storage both times. Only the declared length
        # differs, and it is the declared length that must win: at 350 bits the
        # fourth pair is cut off and must not be decoded.
        #
        # This shape does not occur in the corpus -- every measured blob has
        # bit_count == len(data) * 8 -- so no corpus check can catch the wrong
        # reading. That is what makes it worth a test rather than a comment.
        self.assertEqual(self.decode(self.BLOB, 400), self.FOUR_PAIRS)
        self.assertEqual(self.decode(self.BLOB, 350), self.THREE_PAIRS)

    def test_byte_length_would_have_given_the_wrong_answer(self):
        # Spelled out as its own case so the regression is unmistakable: the
        # old code derived the length as len(data) * 8, which is 400 here, and
        # would have returned four pairs for a payload declaring 350 bits.
        self.assertEqual(len(self.BLOB) * 8, 400)
        self.assertNotEqual(self.decode(self.BLOB, 350), self.decode(self.BLOB, 400))

    def test_absent_blob_decodes_to_an_empty_mapping(self):
        self.assertEqual(bundle._decode_effect_blob(None, self.SPEC, {}), {})

    def test_blob_carries_its_own_bit_count(self):
        # The container must not let the two drift apart silently: a caller
        # that builds one has to supply both.
        blob = bundle._EffectBlob(b"\x00\x01", 9)
        self.assertEqual(blob.data, b"\x00\x01")
        self.assertEqual(blob.bit_count, 9)
        with self.assertRaises(TypeError):
            bundle._EffectBlob(b"\x00\x01")  # bit_count is not optional


class ShotEffectRawSourceTests(unittest.TestCase):
    """An additive Rust JSON overlay must not replace the shot wire source."""

    SHOT_RPC = "/Script/ShooterGame.ShooterCharacter_ClassNetCache"

    def convert_rows(self, tmp: str, rows: list[dict]) -> dict:
        root = Path(tmp)
        export = root / "export"
        export.mkdir()
        write_fields_parquet(export / "fields.parquet", rows)
        return bundle.convert(export, root / "bundle")

    @staticmethod
    def events_of(tmp: str, event_type: str) -> list[dict]:
        events = [json.loads(line) for line in (Path(tmp) / "bundle" / "events.ndjson").read_text(encoding="utf-8").splitlines()]
        return [event for event in events if event["type"] == event_type]

    def rows(self, typed_json: bool = False, **typed) -> list[dict]:
        values = (
            ("FloatValues", EffectBlobBitLengthTests.BLOB),
            ("ObjectValues", b"\x00"),
            ("VectorValues", b"\x00"),
        )
        return [{
            "time_ms": 30, "packet_id": 3, "actor": 2, "object": 22,
            "channel_index": 1, "group_path": self.SHOT_RPC, "handle": 9,
            "field_name": f"ReplayPlayContinuousEffectAtLocation.{name}",
            "bit_count": len(raw) * 8, "raw_bits": raw,
            **({"value_str": "[]"} if typed_json else {}), **typed,
        } for name, raw in values]

    def shot_and_rpc(self, rows: list[dict]) -> tuple[dict, dict, dict]:
        with tempfile.TemporaryDirectory() as tmp:
            summary = self.convert_rows(tmp, rows)
            shot = self.events_of(tmp, "valorant_shot_received")
            rpc = self.events_of(tmp, "rpc_received")
        self.assertEqual(len(shot), 1)
        self.assertEqual(len(rpc), 1)
        return summary, shot[0], rpc[0]

    def test_typed_json_overlay_keeps_raw_shot_and_rpc_payload(self):
        raw_summary, raw_shot, raw_rpc = self.shot_and_rpc(self.rows())
        typed_summary, typed_shot, typed_rpc = self.shot_and_rpc(
            self.rows(typed_json=True)
        )
        self.assertEqual(typed_shot, raw_shot)
        self.assertEqual(typed_rpc, raw_rpc)
        for name, raw, bits in (("FloatValues", EffectBlobBitLengthTests.BLOB, 400),
                                ("ObjectValues", b"\x00", 8), ("VectorValues", b"\x00", 8)):
            with self.subTest(name=name):
                self.assertEqual(typed_rpc["payload"][name], raw_rpc["payload"][name])
                self.assertEqual(typed_rpc["payload"][name], {
                    "BitCount": bits,
                    "Data": base64.b64encode(raw).decode("ascii"),
                })
        self.assertEqual(typed_summary["tally"]["multi_typed_rows"], raw_summary["tally"]["multi_typed_rows"])

    def test_malformed_typed_overlay_still_uses_raw_and_keeps_counter(self):
        _raw_summary, raw_shot, raw_rpc = self.shot_and_rpc(self.rows())
        summary, shot, rpc = self.shot_and_rpc(
            self.rows(value_i64=7, value_str="not-json")
        )
        self.assertEqual(shot, raw_shot)
        self.assertEqual(rpc, raw_rpc)
        self.assertEqual(summary["tally"]["multi_typed_rows"], 3)

class DeathMontageBlobTests(unittest.TestCase):
    """The death-montage pair keeps the reference's blob shape once typed.

    The parser types both parameters as ObjectNetGuid; the reference bundle
    carries each as a {BitCount, Data, TypeName} blob. Without the RPC loop
    handing the wire bits back, the typed row would reach rpc_received as a
    bare integer and change the event's shape.
    """

    GROUP = "/Script/ShooterGame.DamageableComponent_ClassNetCache"
    # 16-bit IntPacked 5055 (an FXC finisher class) and 8-bit 0 (null).
    OVERRIDE = (b"\x7f\x4e", 16, 5055)
    CONTEXT = (b"\x00", 8, 0)

    def payload(self, typed: bool) -> dict:
        rows = []
        for param, (raw, bits, guid) in (
            ("DeathMontageEffectOverride", self.OVERRIDE),
            ("DeathMontageEffectOverrideContext", self.CONTEXT),
        ):
            rows.append({
                "time_ms": 40, "packet_id": 4, "actor": 2, "object": 22,
                "group_path": self.GROUP, "handle": 2,
                "field_name": f"MulticastNotifyDamage_Point.{param}",
                "bit_count": bits, "raw_bits": raw,
                **({"value_i64": guid} if typed else {}),
            })
        rows.append({
            "time_ms": 40, "packet_id": 4, "actor": 2, "object": 22,
            "group_path": self.GROUP, "handle": 2,
            "field_name": "MulticastNotifyDamage_Point.bDeathMontageEffectOverrideIsQueued",
            "bit_count": 1, "raw_bits": b"\x00", "value_bool": False,
        })
        with tempfile.TemporaryDirectory() as tmp:
            export = Path(tmp) / "export"
            export.mkdir()
            write_fields_parquet(export / "fields.parquet", rows)
            bundle.convert(export, Path(tmp) / "bundle")
            events = [json.loads(line) for line in (Path(tmp) / "bundle" / "events.ndjson")
                      .read_text(encoding="utf-8").splitlines()]
        rpcs = [e for e in events if e["type"] == "rpc_received"]
        self.assertEqual(len(rpcs), 1)
        return rpcs[0]["payload"]

    def test_typed_rows_keep_the_reference_blob(self):
        typed = self.payload(typed=True)
        self.assertEqual(typed, self.payload(typed=False))
        for param, (raw, bits, _guid) in (
            ("DeathMontageEffectOverride", self.OVERRIDE),
            ("DeathMontageEffectOverrideContext", self.CONTEXT),
        ):
            self.assertEqual(typed[param], {
                "BitCount": bits,
                "Data": base64.b64encode(raw).decode("ascii"),
                "TypeName": param,
            })
        # The Bool sibling is not swept into the blob branch.
        self.assertIs(typed["bDeathMontageEffectOverrideIsQueued"], False)


class BlockPayloadExclusionTests(unittest.TestCase):
    marker = "__vrfkit_unresolved_class_net_cache_payload__"

    @staticmethod
    def write_fields(path: Path, rows: list[dict]) -> None:
        def values(name, default=None):
            return [row.get(name, default) for row in rows]

        table = pa.table(
            {
                "time_ms": pa.array(values("time_ms"), type=pa.uint32()),
                "packet_id": pa.array(values("packet_id"), type=pa.uint32()),
                "channel_index": pa.array(values("channel_index", 7), type=pa.uint32()),
                "actor_net_guid": pa.array(values("actor"), type=pa.uint32()),
                "object_net_guid": pa.array(values("object"), type=pa.uint32()),
                "group_path": pa.array(values("group_path"), type=pa.string()),
                "handle": pa.array(values("handle", 0), type=pa.uint32()),
                "field_name": pa.array(values("field_name"), type=pa.string()),
                "bit_count": pa.array(values("bit_count"), type=pa.uint32()),
                "raw_bits": pa.array(values("raw_bits"), type=pa.binary()),
                "value_i64": pa.array(values("value_i64"), type=pa.int64()),
                "value_f64": pa.array(values("value_f64"), type=pa.float64()),
                "value_bool": pa.array(values("value_bool"), type=pa.bool_()),
                "value_str": pa.array(values("value_str"), type=pa.string()),
            }
        )
        pq.write_table(table, path)

    @staticmethod
    def bundle_files(path: Path) -> dict[str, bytes]:
        return {item.name: item.read_bytes() for item in sorted(path.iterdir())}

    def test_block_payload_row_is_excluded_before_grouping_and_lifetimes(self):
        ordinary = [
            {
                "time_ms": 10,
                "packet_id": 1,
                "actor": 101,
                "group_path": "PlayerState",
                "field_name": "Health",
                "bit_count": 32,
                "value_i64": 100,
            },
            {
                "time_ms": 20,
                "packet_id": 2,
                "actor": 202,
                "group_path": "UnknownComponent",
                "field_name": None,
                "bit_count": 3,
                "raw_bits": b"\x05",
            },
        ]
        marker_row = {
            "time_ms": 30,
            "packet_id": 3,
            "actor": 303,
            "group_path": "AbilitiesAndBuffsComponent",
            "handle": (1 << 32) - 1,
            "field_name": self.marker,
            "bit_count": 5,
            "raw_bits": b"\x15",
        }

        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            base_export = root / "base_export"
            marked_export = root / "marked_export"
            base_bundle = root / "base_bundle"
            marked_bundle = root / "marked_bundle"
            base_export.mkdir()
            marked_export.mkdir()
            self.write_fields(base_export / "fields.parquet", ordinary)
            self.write_fields(marked_export / "fields.parquet", ordinary + [marker_row])

            base_summary = bundle.convert(base_export, base_bundle)
            marked_summary = bundle.convert(marked_export, marked_bundle)

            self.assertEqual(marked_summary, base_summary)
            # The DATA files must be byte-identical: the marker row changes
            # nothing a consumer reads as an event or a position.
            for name in ("events.ndjson", "movement.ndjson"):
                self.assertEqual(
                    (marked_bundle / name).read_bytes(),
                    (base_bundle / name).read_bytes(),
                    name,
                )
            # The manifest must NOT be identical. The row was on disk and was
            # deliberately skipped; a manifest that read the same either way
            # would be a bundle that cannot say how much preservation data its
            # export carried.
            base_manifest = json.loads(
                (base_bundle / "manifest.json").read_text(encoding="utf-8")
            )["adapter"]
            marked_manifest = json.loads(
                (marked_bundle / "manifest.json").read_text(encoding="utf-8")
            )["adapter"]
            self.assertEqual(base_manifest["field_rows_read"], 2)
            self.assertEqual(base_manifest["field_rows_unresolved_class_net_cache"], 0)
            self.assertEqual(marked_manifest["field_rows_read"], 3)
            self.assertEqual(marked_manifest["field_rows_unresolved_class_net_cache"], 1)
            events = (marked_bundle / "events.ndjson").read_bytes()
            self.assertIn(b'"actor_net_guid":202', events)
            self.assertNotIn(b'"actor_net_guid":303', events)
            self.assertNotIn(self.marker.encode(), events)


class CombatReportLeafNameTests(unittest.TestCase):
    """The bundle keys combat-report leaves on the handle, not on the wire name.

    The parser labels each leaf with the name the replay declares. Two of those
    declarations would break this bundle if they reached it: Riot's own typos
    and 'b'-prefixed booleans are not what compute_metrics.py reads, and the
    quartet HUDConfig/StateRemainingTime/GameTime/GamePhase is declared at
    several handles in the SAME flattened element, so keying on the name would
    merge distinct values into one JSON key.
    """

    GROUP = ("/Game/GameModes/Bomb/Bomb_CombatReportComponent"
             ".Bomb_CombatReportComponent_C")

    def relabel(self, field_name, handle):
        return bundle._combat_report_leaf_name(self.GROUP, field_name, handle)

    def test_wire_spelling_is_mapped_back_to_the_reference_member_name(self):
        # Riot's typo, the 'b' prefix, and the Participant* prefix.
        self.assertEqual(
            self.relabel("Rounds[0].Reports[0].Interactions[0].DamageRecieved", 20),
            "Rounds[0].Reports[0].Interactions[0].DamageReceived",
        )
        self.assertEqual(
            self.relabel("Rounds[0].Reports[0].Interactions[0].bDidKill", 22),
            "Rounds[0].Reports[0].Interactions[0].DidKill",
        )
        self.assertEqual(
            self.relabel("Rounds[0].Reports[0].Interactions[0].ParticipantSubject", 11),
            "Rounds[0].Reports[0].Interactions[0].Subject",
        )
        self.assertEqual(
            self.relabel("Rounds[0].RoundNum", 3), "Rounds[0].RoundNumber",
        )

    def test_repeated_declared_names_stay_distinct_keys(self):
        # Handles 6, 99 and 105 ALL declare 'HUDConfig' at the Reports level.
        # Keying on the name would collapse three values into one.
        labels = {
            self.relabel("Rounds[0].Reports[0].HUDConfig", h) for h in (6, 99, 105)
        }
        self.assertEqual(
            labels,
            {
                "Rounds[0].Reports[0]._h6",
                "Rounds[0].Reports[0]._h99",
                "Rounds[0].Reports[0]._h105",
            },
        )

    def test_container_segments_and_foreign_groups_are_untouched(self):
        # Only the last segment moves; the container segments already match.
        self.assertEqual(
            self.relabel(
                "Rounds[0].Reports[0].Interactions[0].DealtInteractions[0]"
                ".Regions[0].bIsWallPen",
                48,
            ),
            "Rounds[0].Reports[0].Interactions[0].DealtInteractions[0]"
            ".Regions[0].IsWallPen",
        )
        # A different group with a same-shaped name is not rewritten.
        self.assertEqual(
            bundle._combat_report_leaf_name(
                "/Game/Something/Else_C", "Rounds[0].Reports[0].bDidKill", 22
            ),
            "Rounds[0].Reports[0].bDidKill",
        )
        # The bare array container row has no leaf segment to replace.
        self.assertEqual(self.relabel("Rounds", 2), "Rounds")

    def test_synthesised_rows_keep_the_parser_label(self):
        # emit_remaining_raw's row carries handle u32::MAX; rewriting it would
        # produce '_h4294967295'.
        self.assertEqual(
            self.relabel("Rounds[0]._raw", (1 << 32) - 1), "Rounds[0]._raw",
        )
        # The depth-limit row carries a CONTAINER handle and already has the
        # schema's name; rewriting it would produce '_h4'.
        self.assertEqual(
            self.relabel("Rounds[0].Reports", 4), "Rounds[0].Reports",
        )


class TallyTestCase(unittest.TestCase):
    """Shared plumbing for the cases that read the conversion's loss counters."""

    RPC_GROUP = "/Script/ShooterGame.DamageableComponent_ClassNetCache"

    def convert_rows(self, tmp: str, rows: list[dict], **files) -> dict:
        root = Path(tmp)
        export = root / "export"
        export.mkdir()
        write_fields_parquet(export / "fields.parquet", rows)
        for name, text in files.items():
            (export / name.replace("_", ".")).write_text(text, encoding="utf-8")
        return bundle.convert(export, root / "bundle")

    def tally_of(self, tmp: str, rows: list[dict], **files) -> dict:
        return self.convert_rows(tmp, rows, **files)["tally"]

    def events_of(self, tmp: str, event_type: str) -> list[dict]:
        """Every event of one type from the bundle `convert_rows` just wrote.

        The bundle also carries actor_spawned/actor_closed for each actor it
        saw, so a bare line count says nothing about the events under test.
        """
        text = (Path(tmp) / "bundle" / "events.ndjson").read_text(encoding="utf-8")
        events = [json.loads(line) for line in text.splitlines()]
        return [e for e in events if e["type"] == event_type]


class UnnamedRowTallyTests(TallyTestCase):
    """A row the parser could not name is a dropped row, and must be counted.

    Both drop sites are silent today: a property group containing one becomes
    an empty but valid-looking event, and an RPC whose rows are ALL unnamed is
    dropped whole because no field_name ever supplies the function name. The
    documented reference export carries 1,996 such rows and the bundle never
    said so.
    """

    def test_an_unnamed_property_row_is_counted(self):
        rows = [
            {
                "time_ms": 10, "packet_id": 1, "actor": 101,
                "group_path": "PlayerState", "field_name": "Health",
                "bit_count": 32, "value_i64": 100,
            },
            {
                "time_ms": 10, "packet_id": 1, "actor": 101,
                "group_path": "PlayerState", "field_name": None,
                "bit_count": 3, "raw_bits": b"\x05",
            },
        ]
        with tempfile.TemporaryDirectory() as tmp:
            tally = self.tally_of(tmp, rows)
        self.assertEqual(tally["unnamed_property_rows"], 1)

    def test_an_rpc_of_only_unnamed_rows_counts_the_rows_and_the_invocation(self):
        rows = [
            {
                "time_ms": 20, "packet_id": 2, "actor": 202,
                "group_path": self.RPC_GROUP, "handle": 5,
                "field_name": None, "bit_count": 3, "raw_bits": b"\x05",
            },
            {
                "time_ms": 20, "packet_id": 2, "actor": 202,
                "group_path": self.RPC_GROUP, "handle": 5,
                "field_name": None, "bit_count": 4, "raw_bits": b"\x06",
            },
        ]
        with tempfile.TemporaryDirectory() as tmp:
            summary = self.convert_rows(tmp, rows)
            # The invocation really is gone -- the count is its only trace.
            self.assertEqual(self.events_of(tmp, "rpc_received"), [])
        self.assertEqual(summary["tally"]["unnamed_rpc_rows"], 2)
        self.assertEqual(summary["tally"]["unnamed_rpc_invocations"], 1)


class ManifestTallyTests(TallyTestCase):
    """A substituted manifest must not read as a real one.

    With no manifest the gameplay-tag table is empty, so every effect blob is
    keyed by its numeric tag index instead of a name like
    'FiringState.AmmoRemaining' -- and every shot then reports null ammo,
    firing state, player and attack vectors while the run says SUCCESS.
    """

    SHOT_RPC = "/Script/ShooterGame.ShooterCharacter_ClassNetCache"

    def shot_rows(self) -> list[dict]:
        return [
            {
                "time_ms": 30, "packet_id": 3, "actor": 2, "object": 22,
                "channel_index": 1, "group_path": self.SHOT_RPC, "handle": 9,
                "field_name": "ReplayPlayContinuousEffectAtLocation.FloatValues",
                "bit_count": len(EffectBlobBitLengthTests.BLOB) * 8,
                "raw_bits": EffectBlobBitLengthTests.BLOB,
            },
        ]

    def test_a_missing_manifest_is_counted(self):
        with tempfile.TemporaryDirectory() as tmp:
            tally = self.tally_of(tmp, MINIMAL_FIELD_ROWS)
        self.assertEqual(tally["missing_manifest"], 1)

    def test_a_present_manifest_is_not_counted(self):
        manifest = '{"replay_version": "x", "duration_ms": 5}'
        with tempfile.TemporaryDirectory() as tmp:
            tally = self.tally_of(tmp, MINIMAL_FIELD_ROWS, manifest_json=manifest)
        self.assertEqual(tally["missing_manifest"], 0)

    def test_shots_decoded_with_no_tag_table_are_counted(self):
        with tempfile.TemporaryDirectory() as tmp:
            tally = self.tally_of(tmp, self.shot_rows())
        self.assertEqual(tally["empty_gameplay_tag_table"], 1)

    def test_a_tag_table_from_the_manifest_clears_the_count(self):
        manifest = json.dumps({
            "net_field_export_groups": [
                {
                    "path": "NetworkGameplayTagNodeIndex",
                    "fields": [{"handle": 263, "name": "FiringState.AmmoRemaining"}],
                }
            ]
        })
        with tempfile.TemporaryDirectory() as tmp:
            tally = self.tally_of(tmp, self.shot_rows(), manifest_json=manifest)
        self.assertEqual(tally["empty_gameplay_tag_table"], 0)


class RpcCollisionTallyTests(TallyTestCase):
    """Two invocations of one function by one actor in one packet collide.

    The RPC group key is (packet_id, actor, group_path, handle), so both calls
    land in one group, the second call's parameters overwrite the first's, and
    a single rpc_received comes out. Nothing can un-interleave them from the
    export, so the fix is to say it happened, not to guess a boundary.
    """

    def collided_rows(self) -> list[dict]:
        common = {
            "time_ms": 40, "packet_id": 4, "actor": 404,
            "group_path": self.RPC_GROUP, "handle": 12,
            "field_name": "MulticastNotifyKilledEnemy.MultikillLevel",
        }
        return [
            {**common, "bit_count": 8, "value_i64": 1},
            {**common, "bit_count": 8, "value_i64": 2},
        ]

    def test_a_repeated_parameter_in_one_group_is_counted(self):
        with tempfile.TemporaryDirectory() as tmp:
            summary = self.convert_rows(tmp, self.collided_rows())
            # One rpc_received for what were two calls, carrying only the
            # second call's value -- the loss the count names.
            rpcs = self.events_of(tmp, "rpc_received")
        self.assertEqual(summary["tally"]["rpc_param_collisions"], 1)
        self.assertEqual(len(rpcs), 1, rpcs)
        self.assertEqual(rpcs[0]["payload"], {"MultikillLevel": 2})

    def test_distinct_parameters_in_one_group_are_not_counted(self):
        common = {
            "time_ms": 40, "packet_id": 4, "actor": 404,
            "group_path": self.RPC_GROUP, "handle": 12, "bit_count": 8,
        }
        rows = [
            {**common, "field_name": "MulticastNotifyKilledEnemy.KillerCharacter",
             "value_i64": 1},
            {**common, "field_name": "MulticastNotifyKilledEnemy.KilledCharacter",
             "value_i64": 2},
        ]
        with tempfile.TemporaryDirectory() as tmp:
            tally = self.tally_of(tmp, rows)
        self.assertEqual(tally["rpc_param_collisions"], 0)


class PropertyKeyCollisionTallyTests(TallyTestCase):
    """Two rows with one field name in one property event: the last one wins.

    The parser flattens struct members and static-array elements under one
    field_name that only `handle` tells apart (the crosshair profile's
    LineLength at handles 63/75/110). The payload is keyed by name, so every
    value but the last is destroyed: 24,060 times on 02d4d478, with no
    counter moving. The mirror of `rpc_param_collisions`.
    """

    GROUP = "/Game/GameModes/Bomb/BombPlayerState.BombPlayerState_C"

    def row(self, name, handle, value, column="value_f64", **extra):
        return {"time_ms": 25, "packet_id": 25, "actor": 7, "group_path": self.GROUP,
                "handle": handle, "field_name": name, "bit_count": 32,
                column: value, **extra}

    def test_a_repeated_name_in_one_event_is_counted(self):
        rows = [self.row("LineLength", 63, 10.0), self.row("LineLength", 75, 2.0)]
        with tempfile.TemporaryDirectory() as tmp:
            tally = self.tally_of(tmp, rows)
            (event,) = self.events_of(tmp, "export_group_received")
        # Still last-wins: the count is what makes the loss visible.
        self.assertEqual(event["payload"], {"LineLength": 2.0})
        self.assertEqual(tally["property_key_collisions"], 1)
        self.assertEqual(tally["payload_shape_conflicts"], 0)

    def test_distinct_names_and_separate_events_are_not_counted(self):
        rows = [self.row("LineLength", 63, 10.0), self.row("Opacity", 70, 0.5),
                self.row("LineLength", 63, 2.0, packet_id=26, time_ms=26)]
        with tempfile.TemporaryDirectory() as tmp:
            tally = self.tally_of(tmp, rows)
        self.assertEqual(tally["property_key_collisions"], 0)

    def test_a_top_level_row_replacing_a_nested_value_is_counted(self):
        """'Foo' after 'Foo.Bar' is assigned directly, never by `_set_nested`."""
        rows = [self.row("Foo.Bar", 1, 2.0), self.row("Foo", 2, 1.0)]
        with tempfile.TemporaryDirectory() as tmp:
            tally = self.tally_of(tmp, rows)
            (event,) = self.events_of(tmp, "export_group_received")
        self.assertEqual(event["payload"], {"Foo": 1.0})
        self.assertEqual(tally["property_key_collisions"], 1)

    def test_a_repeated_nested_leaf_is_counted(self):
        rows = [self.row("Foo.Bar", 1, 1.0), self.row("Foo.Bar", 2, 2.0),
                self.row("Arr[0]", 3, 3.0), self.row("Arr[0]", 4, 4.0)]
        with tempfile.TemporaryDirectory() as tmp:
            tally = self.tally_of(tmp, rows)
            (event,) = self.events_of(tmp, "export_group_received")
        self.assertEqual(event["payload"], {"Foo": {"Bar": 2.0}, "Arr": [4.0]})
        self.assertEqual(tally["property_key_collisions"], 2)
        self.assertEqual(tally["payload_shape_conflicts"], 0)

    def test_a_real_index_member_replacing_the_injected_one_is_not_counted(self):
        """`_set_nested` gives every array element an Index of its own. A real
        `TeamEconomy[0].Index` row (13.01, 12.05 and 11.06 exports carry them)
        replaces that placeholder, which held nothing of the export's."""
        rows = [self.row("TeamEconomy[0].Index", 1, 0, "value_i64"),
                self.row("TeamEconomy[0].Money", 2, 800, "value_i64"),
                self.row("TeamEconomy[1].Money", 3, 900, "value_i64"),
                self.row("TeamEconomy[1].Index", 4, 1, "value_i64")]
        with tempfile.TemporaryDirectory() as tmp:
            tally = self.tally_of(tmp, rows)
            (event,) = self.events_of(tmp, "export_group_received")
        self.assertEqual(event["payload"], {"TeamEconomy": [
            {"Index": 0, "Money": 800}, {"Index": 1, "Money": 900}]})
        self.assertEqual(tally["property_key_collisions"], 0)


class FlatPathTallyTests(unittest.TestCase):
    """A path segment the parser cannot read becomes a literal key, silently.

    'Rounds[0][1].Damage' yields the literal object key 'Rounds[0][1]', and
    two rows whose shapes disagree ('Foo' and 'Foo.Bar') destroy each other
    depending on arrival order. Both produce valid JSON and a successful
    count, so a counter is the only thing that can report them.
    """

    def test_an_unparsable_segment_is_counted(self):
        tally = bundle._Tally()
        parts = bundle._parse_field_path("Rounds[0][1].Damage", tally)
        self.assertEqual(parts[0], ("Rounds[0][1]", None))
        self.assertEqual(tally["unparsable_path_segments"], 1)

    def test_a_bare_numeric_segment_is_not_counted(self):
        # '248' is the documented spelling of an unnamed handle, not a parse
        # failure; counting it would drown the real ones.
        tally = bundle._Tally()
        self.assertEqual(bundle._parse_field_path("248", tally), [("248", None)])
        self.assertEqual(tally["unparsable_path_segments"], 0)

    def test_a_blueprint_name_with_spaces_is_not_counted(self):
        # Measured on out/baseline: 449 rows carry a segment that is neither
        # an identifier nor a number, and all 41 distinct spellings are
        # Blueprint display names like these -- none has a bracket in it. A
        # name with a space is a leaf, and a literal key is the RIGHT
        # representation of a leaf, so counting these would put a
        # three-figure number in every clean conversion's summary and teach
        # the reader to skip the block.
        tally = bundle._Tally()
        for name in ("Victim FXC", "Set skeletal Collision", "Socket Name"):
            self.assertEqual(bundle._parse_field_path(name, tally),
                             [(name, None)])
        self.assertEqual(tally["unparsable_path_segments"], 0)

    def test_a_segment_whose_subscripts_did_not_parse_is_counted(self):
        # The real failure: bracket structure the parser could not read. The
        # nesting it describes is silently flattened into one literal key.
        tally = bundle._Tally()
        parts = bundle._parse_field_path("Rounds[0][1]", tally)
        self.assertEqual(parts, [("Rounds[0][1]", None)])
        self.assertEqual(tally["unparsable_path_segments"], 1)

    def test_a_scalar_overwritten_by_a_nested_value_is_counted(self):
        tally = bundle._Tally()
        payload = {}
        bundle._set_nested(payload, bundle._parse_field_path("Foo"), 1, tally)
        bundle._set_nested(payload, bundle._parse_field_path("Foo.Bar"), 2, tally)
        self.assertEqual(payload, {"Foo": {"Bar": 2}})
        self.assertEqual(tally["payload_shape_conflicts"], 1)

    def test_a_nested_value_overwritten_by_a_scalar_is_counted(self):
        tally = bundle._Tally()
        payload = {}
        bundle._set_nested(payload, bundle._parse_field_path("Foo.Bar"), 2, tally)
        bundle._set_nested(payload, bundle._parse_field_path("Foo"), 1, tally)
        self.assertEqual(payload, {"Foo": 1})
        self.assertEqual(tally["payload_shape_conflicts"], 1)

    def test_ordinary_nesting_is_not_counted(self):
        tally = bundle._Tally()
        payload = {}
        for path, value in (("Rounds[0].Damage", 5), ("Rounds[1].Damage", 7),
                            ("Rounds[0].Kills", 2), ("Plain", 1)):
            bundle._set_nested(payload, bundle._parse_field_path(path), value, tally)
        self.assertEqual(tally["payload_shape_conflicts"], 0)
        self.assertEqual(tally["unparsable_path_segments"], 0)
        self.assertEqual(tally["property_key_collisions"], 0)

    def test_life_change_child_rows_are_still_dropped(self):
        # Guard, not a new claim: these children arrive spelled with '[0]'
        # and must never reach the payload as flat keys.
        for param in ("LifeChangeEvents[0].LifeResult",
                      "LifeChangeBySection[1].Amount"):
            self.assertIsNone(
                bundle._normalize_rpc_param(
                    "MulticastNotifyDamage_Point", param, 3, False
                )
            )


class TypedColumnTallyTests(unittest.TestCase):
    """More than one typed column set is a writer regression, not a value.

    _get_value picks i64, then f64, then bool, then string. If a regression
    filled value_i64=1 and value_bool=False, the bundle emits 1, discards the
    boolean and completes normally.
    """

    def test_two_populated_typed_columns_are_counted(self):
        tally = bundle._Tally()
        value, is_raw = bundle._get_value(1, None, False, None, None, None, tally)
        self.assertEqual((value, is_raw), (1, False))
        self.assertEqual(tally["multi_typed_rows"], 1)

    def test_one_populated_typed_column_is_not_counted(self):
        tally = bundle._Tally()
        for args in ((7, None, None, None, None, None),
                     (None, 1.5, None, None, None, None),
                     (None, None, True, None, None, None),
                     (None, None, None, "s", None, None),
                     (None, None, None, None, b"\x01", 8)):
            bundle._get_value(*args, tally)
        self.assertEqual(tally["multi_typed_rows"], 0)

    def test_a_typed_value_beside_raw_bits_is_not_counted(self):
        # raw_bits travels alongside decoded values by design; only the four
        # TYPED columns are meant to be mutually exclusive.
        tally = bundle._Tally()
        bundle._get_value(1, None, None, None, b"\x01", 8, tally)
        self.assertEqual(tally["multi_typed_rows"], 0)


class FabricatedLocationTests(TallyTestCase):
    """A shot with no readable location gets the world origin -- say so.

    _parse_vector_or_zero's docstring claimed an upstream filter guarantees a
    location is present. There is no such filter: _build_rpc_events emits
    every ReplayPlayContinuousEffectAtLocation invocation and says so in its
    own comment ('No blob guard'), so the fabricated origin is reachable.
    """

    def build_shot(self, scalar_params: dict, tally):
        return bundle._build_shot_event(
            bundle._ShotContext(tag_table={}), 1, 2, 3, 4, 5,
            scalar_params, bundle._EffectBlobs(), tally=tally,
        )["shot"]

    def test_a_shot_with_no_location_counts_a_fabricated_origin(self):
        tally = bundle._Tally()
        shot = self.build_shot({}, tally)
        self.assertEqual(shot["location"], {"x": 0, "y": 0, "z": 0})
        self.assertEqual(tally["fabricated_shot_locations"], 1)

    def test_an_unparsable_location_counts_a_fabricated_origin(self):
        tally = bundle._Tally()
        self.assertEqual(
            self.build_shot({"Location": "(1,2)"}, tally)["location"],
            {"x": 0, "y": 0, "z": 0},
        )
        self.assertEqual(tally["fabricated_shot_locations"], 1)

    def test_a_real_location_counts_nothing(self):
        tally = bundle._Tally()
        self.assertEqual(
            self.build_shot({"Location": "(1,2,3)"}, tally)["location"],
            {"x": 1, "y": 2, "z": 3},
        )
        self.assertEqual(tally["fabricated_shot_locations"], 0)

    def test_a_genuine_origin_shot_counts_nothing(self):
        # (0,0,0) parsed from the wire is a real position, not a fabrication.
        tally = bundle._Tally()
        self.assertEqual(
            self.build_shot({"Location": "(0,0,0)"}, tally)["location"],
            {"x": 0, "y": 0, "z": 0},
        )
        self.assertEqual(tally["fabricated_shot_locations"], 0)


class RawSourcedFieldTests(TallyTestCase):
    """A field whose consumer decodes the raw wire blob gets the blob, typed or not.

    RoundInfos (valplay's `_roundinfo` bit-decodes it) and a damage RPC's
    LifeChangeEvents (valplay's `_decode_remaining_hp`) were gated on
    `is_raw`, which `_get_value` sets only when EVERY typed column is null --
    though raw_bits travels beside typed values by design. Once the overlay
    typed either one, RoundInfos vanished from the payload with no counter
    and LifeChangeEvents became a typed value its consumer skips. The shot
    arrays already read raw_bits directly; all three now share that path.
    """

    OEPI = "/Script/ShooterGame.OwnerExclusivePlayerInfo"
    RI_RAW = bytes.fromhex("0102030405")
    RI_BLOB = {
        "BitCount": 40,
        "Data": base64.b64encode(RI_RAW).decode("ascii"),
        "TypeName": "TArray<FAresPlayerRoundInfo>",
    }

    def roundinfos(self, **overrides) -> dict:
        row = {
            "time_ms": 10, "packet_id": 1, "actor": 5, "object": 5,
            "group_path": self.OEPI, "handle": 39, "field_name": "RoundInfos",
            "bit_count": 40, "raw_bits": self.RI_RAW,
        }
        row.update(overrides)
        return row

    def roundinfos_child(self) -> dict:
        return {
            "time_ms": 10, "packet_id": 1, "actor": 5, "object": 5,
            "group_path": self.OEPI, "handle": 43,
            "field_name": "RoundInfos[0].EndOfRoundMoney",
            "bit_count": 32, "value_i64": 900,
        }

    def property_payload(self, tmp: str) -> dict:
        (event,) = self.events_of(tmp, "export_group_received")
        return event["payload"]

    def test_an_untyped_roundinfos_row_publishes_its_blob(self):
        """The shape that has always worked, pinned so the fix cannot move it.
        stream.rs writes the decoded children first and the parent row below."""
        with tempfile.TemporaryDirectory() as tmp:
            summary = self.convert_rows(
                tmp, [self.roundinfos_child(), self.roundinfos()])
            payload = self.property_payload(tmp)
        self.assertEqual(payload, {"RoundInfos": self.RI_BLOB})
        self.assertEqual(summary["tally"]["raw_blobs_unavailable"], 0)
        self.assertEqual(summary["tally"]["property_key_collisions"], 0)

    def test_a_repeated_roundinfos_row_in_one_event_is_counted(self):
        """Two RoundInfos rows in one event: only the last blob survives."""
        second = bytes.fromhex("0a0b0c0d0e")
        with tempfile.TemporaryDirectory() as tmp:
            summary = self.convert_rows(tmp, [
                self.roundinfos(), self.roundinfos(raw_bits=second)])
            payload = self.property_payload(tmp)
        self.assertEqual(payload["RoundInfos"]["Data"],
                         base64.b64encode(second).decode("ascii"))
        self.assertEqual(summary["tally"]["property_key_collisions"], 1)

    def test_a_typed_roundinfos_row_still_publishes_its_raw_blob(self):
        """The defect: a value_str beside raw_bits made the payload `{}`."""
        with tempfile.TemporaryDirectory() as tmp:
            summary = self.convert_rows(tmp, [
                self.roundinfos_child(), self.roundinfos(value_str="[]"),
            ])
            payload = self.property_payload(tmp)
        self.assertEqual(payload, {"RoundInfos": self.RI_BLOB})
        self.assertEqual(summary["tally"]["raw_blobs_unavailable"], 0)

    def test_a_roundinfos_row_without_raw_bits_is_counted(self):
        """No raw bits, no blob: counted, and the typed value is not thrown away."""
        with tempfile.TemporaryDirectory() as tmp:
            summary = self.convert_rows(tmp, [
                self.roundinfos(raw_bits=None, bit_count=None, value_str="[]"),
            ])
            payload = self.property_payload(tmp)
        self.assertEqual(summary["tally"]["raw_blobs_unavailable"], 1)
        self.assertEqual(payload, {"RoundInfos": "[]"})

    def test_roundinfos_children_without_their_blob_are_counted(self):
        """The children are dropped by design -- the blob carries them -- so
        children with no blob beside them are a loss, counted once."""
        with tempfile.TemporaryDirectory() as tmp:
            summary = self.convert_rows(tmp, [self.roundinfos_child()])
            payload = self.property_payload(tmp)
        self.assertEqual(payload, {})
        self.assertEqual(summary["tally"]["raw_blobs_unavailable"], 1)

    DAMAGE = "MulticastNotifyDamage_Point"
    LCE_RAW = bytes.fromhex("02021620772418400000ce421a400000b0c11c02010000")
    LCE_BLOB = {
        "BitCount": 177,
        "Data": base64.b64encode(LCE_RAW).decode("ascii"),
        "TypeName": "LifeChangeEvents",
    }

    def damage_rows(self, **parent) -> list[dict]:
        """One damage invocation shaped like the corpus: every row carries the
        function's handle, and the decoded LifeChangeEvents members precede
        the parent row that holds the whole blob."""
        common = {"time_ms": 20, "packet_id": 2, "actor": 7,
                  "group_path": self.RPC_GROUP, "handle": 1}
        lce = {**common, "field_name": f"{self.DAMAGE}.LifeChangeEvents",
               "bit_count": 177, "raw_bits": self.LCE_RAW}
        lce.update(parent)
        return [
            {**common, "field_name": f"{self.DAMAGE}.DamageTaken",
             "bit_count": 32, "value_f64": 30.0},
            {**common,
             "field_name": f"{self.DAMAGE}.LifeChangeEvents[0].LifeResult",
             "bit_count": 32, "value_f64": 70.0, "raw_bits": b"\x00\x00\x8cB"},
            lce,
        ]

    def damage_payload(self, tmp: str) -> dict:
        (event,) = self.events_of(tmp, "rpc_received")
        return event["payload"]

    def test_an_untyped_life_change_blob_is_unchanged(self):
        with tempfile.TemporaryDirectory() as tmp:
            summary = self.convert_rows(tmp, self.damage_rows())
            payload = self.damage_payload(tmp)
        self.assertEqual(payload, {"DamageTaken": 30.0,
                                   "LifeChangeEvents": self.LCE_BLOB})
        self.assertEqual(summary["tally"]["raw_blobs_unavailable"], 0)

    def test_a_typed_life_change_row_still_publishes_its_raw_blob(self):
        """Typed, it fell through to the generic pass-through and shipped the
        typed value where valplay's HP decoder reads the blob."""
        with tempfile.TemporaryDirectory() as tmp:
            summary = self.convert_rows(tmp, self.damage_rows(value_str="[{}]"))
            payload = self.damage_payload(tmp)
        self.assertEqual(payload["LifeChangeEvents"], self.LCE_BLOB)
        self.assertEqual(summary["tally"]["raw_blobs_unavailable"], 0)

    def test_a_life_change_row_without_raw_bits_is_counted(self):
        with tempfile.TemporaryDirectory() as tmp:
            summary = self.convert_rows(tmp, self.damage_rows(
                raw_bits=None, bit_count=None, value_str="[{}]"))
            payload = self.damage_payload(tmp)
        self.assertEqual(summary["tally"]["raw_blobs_unavailable"], 1)
        self.assertEqual(payload["LifeChangeEvents"], "[{}]")

    def test_life_change_members_without_their_blob_are_counted(self):
        with tempfile.TemporaryDirectory() as tmp:
            summary = self.convert_rows(tmp, self.damage_rows()[:2])
            payload = self.damage_payload(tmp)
        self.assertNotIn("LifeChangeEvents", payload)
        self.assertEqual(summary["tally"]["raw_blobs_unavailable"], 1)

    def test_shot_arrays_without_raw_bits_are_counted(self):
        """Their consumer is this file's own effect decoder; it gets nothing."""
        rows = [{
            "time_ms": 30, "packet_id": 3, "actor": 2, "object": 22,
            "channel_index": 1, "group_path": ShotEffectRawSourceTests.SHOT_RPC,
            "handle": 9,
            "field_name": f"ReplayPlayContinuousEffectAtLocation.{name}",
            "value_str": "[]",
        } for name in ("FloatValues", "ObjectValues", "VectorValues")]
        with tempfile.TemporaryDirectory() as tmp:
            summary = self.convert_rows(tmp, rows)
        self.assertEqual(summary["tally"]["raw_blobs_unavailable"], 3)


class DamagedBoneTests(TallyTestCase):
    """DamagedBone is an FName the overlay decodes; an undecoded one is null, not guessed.

    The raw branch ASCII-decoded the wire bytes with errors='replace' inside a
    bare `except`, so it could not fail -- and this rendering already shipped
    mojibake once (apply_type_corrections.py records it for all 581 payloads
    when the field was forced to Raw). `null` is also what valplay can take:
    its `_bone_region` files None under 'other', where a raw blob dict would
    raise TypeError on `bone in HEAD_BONES`, a frozenset.
    """

    FIELD = "MulticastNotifyDamage_Point.DamagedBone"
    # An FName "Head" as it sits in the corpus (105 bits).
    HEAD_RAW = bytes.fromhex("0a00000090cac2c8aa00000000")

    def bone_payload(self, **row) -> tuple[dict, dict]:
        base = {"time_ms": 20, "packet_id": 2, "actor": 7,
                "group_path": self.RPC_GROUP, "handle": 1,
                "field_name": self.FIELD, "bit_count": 105,
                "raw_bits": self.HEAD_RAW}
        base.update(row)
        with tempfile.TemporaryDirectory() as tmp:
            summary = self.convert_rows(tmp, [base])
            (event,) = self.events_of(tmp, "rpc_received")
        return event["payload"], summary["tally"]

    def test_a_decoded_bone_is_passed_through(self):
        payload, tally = self.bone_payload(value_str="Head")
        self.assertEqual(payload, {"DamagedBone": "Head"})
        self.assertEqual(tally["damaged_bone_undecoded"], 0)

    def test_an_undecoded_bone_is_null_and_counted(self):
        payload, tally = self.bone_payload()
        self.assertEqual(payload, {"DamagedBone": None})
        self.assertEqual(tally["damaged_bone_undecoded"], 1)


class RawGateHardeningTests(TallyTestCase):
    """Two more `is_raw` gates that lost a value, uncounted, once it was typed."""

    def test_a_typed_container_row_does_not_replace_its_decoded_elements(self):
        """stream.rs emits a flattened array's element rows first and the
        container row below. The container was skipped only when raw, so a
        typed one landed through the direct top-level assignment -- which no
        conflict counter saw then -- and replaced the decoded list."""
        common = {"time_ms": 10, "packet_id": 1, "actor": 5,
                  "group_path": "/Game/Test/Holder.Holder_C"}
        rows = [
            {**common, "field_name": "Items[0].Count", "bit_count": 32,
             "value_i64": 3},
            {**common, "field_name": "Items", "bit_count": 40,
             "raw_bits": b"\x01\x02\x03\x04\x05", "value_str": "[3]"},
        ]
        with tempfile.TemporaryDirectory() as tmp:
            summary = self.convert_rows(tmp, rows)
            (event,) = self.events_of(tmp, "export_group_received")
        self.assertEqual(event["payload"], {"Items": [{"Index": 0, "Count": 3}]})
        self.assertEqual(summary["tally"]["payload_shape_conflicts"], 0)

    def test_a_typed_function_row_is_carried(self):
        """A row that IS the function carried its value only when raw."""
        row = {"time_ms": 20, "packet_id": 2, "actor": 7,
               "group_path": self.RPC_GROUP, "handle": 4,
               "field_name": "MulticastSomething", "bit_count": 8,
               "value_i64": 5}
        with tempfile.TemporaryDirectory() as tmp:
            self.convert_rows(tmp, [row])
            (event,) = self.events_of(tmp, "rpc_received")
        self.assertEqual(event["payload"], {"MulticastSomething": 5})


class SummaryReportingTests(TallyTestCase):
    """The summary must not say 'complete' about a conversion that lost rows."""

    def test_a_clean_conversion_reports_no_losses(self):
        with tempfile.TemporaryDirectory() as tmp:
            manifest = '{"replay_version": "x"}'
            summary = self.convert_rows(
                tmp, MINIMAL_FIELD_ROWS, manifest_json=manifest
            )
        self.assertEqual(summary["tally"].total, 0)

    def test_a_lossy_conversion_names_every_loss_in_its_summary(self):
        rows = MINIMAL_FIELD_ROWS + [
            {
                "time_ms": 10, "packet_id": 1, "actor": 101,
                "group_path": "PlayerState", "field_name": None,
                "bit_count": 3, "raw_bits": b"\x05",
            },
        ]
        with tempfile.TemporaryDirectory() as tmp:
            summary = self.convert_rows(tmp, rows)
        lines = summary["tally"].lines()
        self.assertTrue(any("unnamed_property_rows" in ln for ln in lines), lines)
        self.assertTrue(any("missing_manifest" in ln for ln in lines), lines)
        # Only the non-zero counters are printed.
        self.assertEqual(len(lines), 2, lines)


# ---------------------------------------------------------------------------
# The seam
#
# Everything below tests the contract between this repository and valplay:
# what the bundle manifest carries, what it deliberately does not, and the
# order events.ndjson is written in. valplay reads all of it and has no way to
# check any of it -- these are the assertions that fail HERE when a change
# would have broken it silently over there.
# ---------------------------------------------------------------------------

#: A quality object shaped like the one crates/vrfkit/src/manifest.rs emits.
#: Trimmed to the members the seam actually reads plus enough of the rest to
#: prove the forwarding is verbatim and not a hand-picked subset.
UPSTREAM_QUALITY = {
    "content_blocks_lost": 0,
    "chunks_processed": 19,
    "export_groups": 475,
    "movement_rows": 3,
    "net_guid_rows": 2,
    "event_rows": 195,
    "event_trailing_bytes": 0,
    "replay_data_trailing_bytes": 0,
    "event_layout_mismatches": 0,
    "event_first_layout_mismatch": None,
    "overlay_error_buckets": 0,
    "overlay_errors_reported": 0,
    "checkpoints_enabled": False,
    "net": {"packets": 530401, "malformed_packets": 0, "skipped_bits": 19135006},
    "sink": {"overlay_decoded_ok": 742738, "struct_blob_first_error": None},
    "checkpoints": None,
}

UPSTREAM_GROUPS = [
    {
        "path": "/Script/ShooterGame.OwnerExclusivePlayerInfo",
        "path_name_index": 12,
        "fields": [
            {"handle": 40, "name": "RoundNumber", "compatible_checksum": 1},
            {"handle": 41, "name": "StartOfRoundMoney", "compatible_checksum": 2},
        ],
    },
]

UPSTREAM_LEVELS = [
    {"name": "/Game/Maps/Infinity/Infinity", "time_ms": 0},
]

#: Two account UUIDs, shaped like the ones vrfkit's manifest `players` array
#: carries. Present in the EXPORT manifest so the omission test has something
#: real to prove is absent from the bundle.
UPSTREAM_PLAYERS = [
    {
        "actor_net_guid": 101,
        "subject": "11111111-2222-3333-4444-555555555555",
        "character_net_guid": 501,
    },
]


def write_net_guids_parquet(path: Path, rows: list[dict]) -> None:
    """Write a net_guids.parquet with the columns `_load_net_guids` reads."""
    table = pa.table(
        {
            "net_guid": pa.array([r["net_guid"] for r in rows], type=pa.uint32()),
            "outer_net_guid": pa.array(
                [r.get("outer") for r in rows], type=pa.uint32()
            ),
            "path": pa.array([r.get("path") for r in rows], type=pa.string()),
        }
    )
    pq.write_table(table, path)


def write_events_parquet(path: Path, rows: list[dict]) -> None:
    """Write the privacy-sensitive source timeline table.

    ``id``, ``metadata`` and ``raw_payload`` deliberately contain a marker in
    the seam tests below.  If the adapter ever copies a whole row instead of
    applying its explicit allowlist, the marker makes that leak fail loudly.
    """
    def values(name, default=None):
        return [row.get(name, default) for row in rows]

    table = pa.table(
        {
            "id": pa.array(values("id", "private-id"), type=pa.string()),
            "group": pa.array(values("group"), type=pa.string()),
            "metadata": pa.array(
                values("metadata", "private-metadata"), type=pa.string()
            ),
            "time1": pa.array(values("time1", 0), type=pa.uint32()),
            "time2": pa.array(values("time2", 0), type=pa.uint32()),
            "payload_size": pa.array(
                values("payload_size", 0), type=pa.int32()
            ),
            "raw_payload": pa.array(
                values("raw_payload", b"private-payload"), type=pa.binary()
            ),
            "word0": pa.array(values("word0"), type=pa.uint32()),
            "word1": pa.array(values("word1"), type=pa.uint32()),
            "payload_tag": pa.array(values("payload_tag"), type=pa.uint32()),
            "payload_name": pa.array(values("payload_name"), type=pa.string()),
            "payload_seconds": pa.array(
                values("payload_seconds"), type=pa.float32()
            ),
        }
    )
    pq.write_table(table, path)


class SeamTestCase(unittest.TestCase):
    """Build an export with every table the adapter reads, then convert it."""

    def build(self, tmp, *, field_rows=None, movement_rows=(), guid_rows=(),
              actor_rows=(), timeline_rows=(), manifest=None):
        root = Path(tmp)
        export = root / "export"
        export.mkdir()
        write_fields_parquet(
            export / "fields.parquet",
            list(MINIMAL_FIELD_ROWS if field_rows is None else field_rows),
        )
        if actor_rows:
            write_actors_parquet(export / "actors.parquet", list(actor_rows))
        if movement_rows:
            write_movement_parquet(export / "movement.parquet", list(movement_rows))
        if guid_rows:
            write_net_guids_parquet(export / "net_guids.parquet", list(guid_rows))
        if timeline_rows:
            write_events_parquet(export / "events.parquet", list(timeline_rows))
        if manifest is not None:
            (export / "manifest.json").write_text(
                json.dumps(manifest), encoding="utf-8"
            )
        out = root / "bundle"
        summary = bundle.convert(export, out)
        published = json.loads((out / "manifest.json").read_text(encoding="utf-8"))
        return out, published, summary

    def full_manifest(self, **overrides):
        manifest = {
            "source_file": "02d4d478.vrf",
            "source_size_bytes": 55297993,
            "replay_version": "5.3.2",
            "replay_build": "++Ares-Core+release-13.02",
            "replay_changelist": 2152699011,
            "duration_ms": 2296000,
            "quality": json.loads(json.dumps(UPSTREAM_QUALITY)),
            "players": json.loads(json.dumps(UPSTREAM_PLAYERS)),
            "net_field_export_groups": json.loads(json.dumps(UPSTREAM_GROUPS)),
            "level_names_and_times": json.loads(json.dumps(UPSTREAM_LEVELS)),
            "game_specific_data": [
                '{"subject":"aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"}',
            ],
        }
        manifest.update(overrides)
        return manifest


#: A settled Sage wall: it opens, then goes DORMANT (the server stops
#: replicating an actor that is still standing). `dormant` is the third value
#: of `actors.event`; there is no `close` here, because nothing destroyed it.
SAGE_WALL_CLASS = "/Game/Characters/Sage/S0/Ability_Barrier/BarrierProjectile_C"

DORMANCY_ACTOR_ROWS = [
    {"time_ms": 100, "packet_id": 1, "actor": 501, "event": "open",
     "class_path": SAGE_WALL_CLASS, "archetype_path": "Default__Barrier",
     "spawn_x": 1.0, "spawn_y": 2.0, "spawn_z": 3.0},
    # Destroyed for real, early.
    {"time_ms": 200, "packet_id": 2, "actor": 502, "event": "open",
     "class_path": SAGE_WALL_CLASS, "archetype_path": "Default__Barrier",
     "spawn_x": 4.0, "spawn_y": 5.0, "spawn_z": 6.0},
    {"time_ms": 3200, "packet_id": 3, "actor": 502, "event": "close"},
    # Settles into dormancy, still alive.
    {"time_ms": 40100, "packet_id": 4, "actor": 501, "event": "dormant"},
]


class ActorLifecycleEventTests(SeamTestCase):
    """`actors.event` has THREE values and this file used to publish two.

    The branch was `if event == "open": spawn else: closed`, so every `dormant`
    row -- the server suspending replication of an actor that is STILL ALIVE --
    published as `actor_closed`. valplay reads `actor_closed` as a despawn
    (`pipeline/metrics/ability_detail.py` pairs spawn/close into a lifetime), so
    a settled smoke, wall or trap read as destroyed with its lifetime truncated
    to the moment it stopped moving.

    CLAUDE.md names this trap outright, and `tools/extract_active_effects.py`
    in this same directory already honours it on the same column.
    """

    def read_events(self, out):
        text = (out / "events.ndjson").read_text(encoding="utf-8")
        return [json.loads(line) for line in text.splitlines()]

    def convert(self, actor_rows):
        with tempfile.TemporaryDirectory() as tmp:
            out, published, _ = self.build(
                tmp, actor_rows=actor_rows, manifest=self.full_manifest()
            )
            return self.read_events(out), published

    def test_a_dormant_row_is_not_published_as_a_despawn(self):
        """The defect itself. `dormant` must not become `actor_closed`."""
        events, _ = self.convert(DORMANCY_ACTOR_ROWS)
        closed = [e for e in events if e["type"] == "actor_closed"]
        self.assertEqual(
            [e["actor_net_guid"] for e in closed], [502],
            "a dormant actor was published as a despawn: "
            f"{[(e['type'], e['actor_net_guid']) for e in events]}",
        )

    def test_a_dormant_row_is_published_under_its_own_type(self):
        """Not dropped either: the row and its timestamp are real data."""
        events, _ = self.convert(DORMANCY_ACTOR_ROWS)
        dormant = [e for e in events if e["type"] == "actor_dormant"]
        self.assertEqual(len(dormant), 1, events)
        self.assertEqual(dormant[0]["actor_net_guid"], 501)
        self.assertEqual(dormant[0]["time_ms"], 40100)

    def test_every_actors_row_still_becomes_exactly_one_event(self):
        """No row is lost and none is duplicated by the re-labelling."""
        events, _ = self.convert(DORMANCY_ACTOR_ROWS)
        lifecycle = [
            e for e in events
            if e["type"] in {"actor_spawned", "actor_closed", "actor_dormant",
                             "actor_lifecycle_unknown"}
        ]
        self.assertEqual(len(lifecycle), len(DORMANCY_ACTOR_ROWS))

    def test_the_raw_wire_value_is_carried_on_the_event(self):
        """A consumer can audit the mapping instead of trusting the type name."""
        events, _ = self.convert(DORMANCY_ACTOR_ROWS)
        by_type = {
            e["type"]: e.get("actor_event")
            for e in events if e["type"] in {"actor_closed", "actor_dormant"}
        }
        self.assertEqual(by_type, {"actor_closed": "close",
                                   "actor_dormant": "dormant"})

    def test_an_unknown_event_value_is_not_folded_into_a_close(self):
        """A fourth value must be a visible unknown, never a plausible despawn.

        This is the same drift one step ahead: `dormant` WAS such a value once,
        and the `else` turned it into a despawn silently.
        """
        rows = [
            {"time_ms": 100, "packet_id": 1, "actor": 601, "event": "open",
             "class_path": SAGE_WALL_CLASS, "archetype_path": "Default__X"},
            {"time_ms": 200, "packet_id": 2, "actor": 601,
             "event": "torn_off_in_a_future_build"},
        ]
        events, published = self.convert(rows)
        lifecycle = [
            e["type"] for e in events if e["type"].startswith("actor_")
        ]
        self.assertEqual(
            lifecycle, ["actor_spawned", "actor_lifecycle_unknown"], events)
        self.assertNotIn("actor_closed", lifecycle)
        unknown = [e for e in events if e["type"] == "actor_lifecycle_unknown"][0]
        self.assertEqual(unknown["actor_event"], "torn_off_in_a_future_build")
        self.assertEqual(
            published["adapter"]["losses"]["unknown_actor_lifecycle_events"], 1,
            "an unrecognised lifecycle value was published without a count",
        )

    def test_the_known_values_do_not_touch_the_unknown_counter(self):
        """The counter must not cry wolf on the three values that are known."""
        _, published = self.convert(DORMANCY_ACTOR_ROWS)
        self.assertEqual(
            published["adapter"]["losses"]["unknown_actor_lifecycle_events"], 0)

    def test_the_event_type_map_covers_the_values_claude_md_documents(self):
        """`open` is handled by the branch above the map; `close`/`dormant` in it.

        Pins the three-value fact itself, so a map that quietly lost `dormant`
        again is a red test rather than a silent despawn.
        """
        self.assertEqual(set(bundle._ACTOR_EVENT_TYPES),
                         {"close", "dormant"})
        self.assertEqual(bundle._ACTOR_EVENT_TYPES["close"], "actor_closed")
        self.assertNotEqual(bundle._ACTOR_EVENT_TYPES["dormant"],
                            bundle._ACTOR_EVENT_TYPES["close"])

    def test_spawns_are_unchanged_by_the_dormancy_split(self):
        """The open branch must keep its class path, archetype and location."""
        events, _ = self.convert(DORMANCY_ACTOR_ROWS)
        spawns = [e for e in events if e["type"] == "actor_spawned"]
        self.assertEqual(len(spawns), 2)
        first = [e for e in spawns if e["actor_net_guid"] == 501][0]
        self.assertEqual(first["archetype_path"], "Default__Barrier")
        self.assertEqual(first["location"], {"x": 1.0, "y": 2.0, "z": 3.0})
        self.assertTrue(first["replication_class_path"].endswith(
            "BarrierProjectile_C"))

    def test_spawn_channel_and_rotation_cross_without_fabrication(self):
        rows = [
            {
                "time_ms": 100, "packet_id": 1, "channel_index": 17,
                "actor": 701, "event": "open",
                "class_path": SAGE_WALL_CLASS,
                "archetype_path": "Default__Rotated",
                "spawn_pitch": 12.5, "spawn_yaw": 90.0,
                "spawn_roll": -3.25,
            },
            {
                "time_ms": 200, "packet_id": 2, "channel_index": 18,
                "actor": 702, "event": "open",
                "class_path": SAGE_WALL_CLASS,
                "archetype_path": "Default__Static",
            },
            {
                "time_ms": 300, "packet_id": 3, "channel_index": 17,
                "actor": 701, "event": "close",
            },
        ]
        events, _ = self.convert(rows)
        rotated = next(e for e in events if e.get("actor_net_guid") == 701
                       and e["type"] == "actor_spawned")
        static = next(e for e in events if e.get("actor_net_guid") == 702)
        closed = next(e for e in events if e["type"] == "actor_closed")
        self.assertEqual(rotated["channel"], 17)
        self.assertEqual(rotated["rotation"], {
            "pitch": 12.5, "yaw": 90, "roll": -3.25,
        })
        self.assertIsNone(static["rotation"])
        self.assertEqual(closed["channel"], 17)

    def test_the_fallback_path_does_not_claim_a_close_reason_it_cannot_know(self):
        """With no actors.parquet there is no `event` column at all.

        The inferred close comes from the last field row, which says nothing
        about WHY the actor stopped appearing. `actor_event` is null there --
        a visible absence rather than a fabricated "close".
        """
        with tempfile.TemporaryDirectory() as tmp:
            out, _, _ = self.build(tmp, manifest=self.full_manifest())
            events = self.read_events(out)
        closed = [e for e in events if e["type"] == "actor_closed"]
        self.assertTrue(closed)
        for event in closed:
            self.assertIsNone(event["actor_event"])
            self.assertIn("actor_event", event)


class UpstreamAccountingForwardingTests(SeamTestCase):
    """vrfkit counts the losses; the bundle has to carry the count.

    Before this, `_write_manifest` emitted six header scalars and dropped the
    entire `quality` object, so valplay's only way to judge completeness was
    to recount the NDJSON it had just been handed -- which cannot detect
    anything that never reached the NDJSON in the first place.
    """

    def test_quality_is_forwarded_verbatim(self):
        with tempfile.TemporaryDirectory() as tmp:
            _, published, _ = self.build(
                tmp,
                movement_rows=[{"time_ms": 1, "packet_id": 1, "char": 5}] * 3,
                guid_rows=[{"net_guid": 1, "outer": 2, "path": "a"},
                           {"net_guid": 2, "outer": 3, "path": "b"}],
                manifest=self.full_manifest(),
            )
        self.assertEqual(published["quality"], UPSTREAM_QUALITY)

    def test_net_field_export_groups_are_forwarded_verbatim(self):
        with tempfile.TemporaryDirectory() as tmp:
            _, published, _ = self.build(tmp, manifest=self.full_manifest())
        self.assertEqual(published["net_field_export_groups"], UPSTREAM_GROUPS)

    def test_public_level_names_are_forwarded_without_private_header_data(self):
        """The level root identifies the map; the adjacent header blob may identify players."""
        manifest = self.full_manifest()
        manifest["level_names_and_times"][0]["account_subject"] = (
            "99999999-8888-7777-6666-555555555555"
        )
        with tempfile.TemporaryDirectory() as tmp:
            out, published, _ = self.build(tmp, manifest=manifest)
            raw = (out / "manifest.json").read_text(encoding="utf-8")
        self.assertEqual(published["level_names_and_times"], UPSTREAM_LEVELS)
        self.assertNotIn("game_specific_data", published)
        self.assertNotIn("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee", raw)
        self.assertNotIn("99999999-8888-7777-6666-555555555555", raw)

    def test_an_export_without_quality_publishes_null_not_zeroes(self):
        """A missing value renders as a visible absence, never as a number.

        `{}` or a zero-filled object here would tell a consumer the export was
        complete on the strength of nobody having counted.
        """
        with tempfile.TemporaryDirectory() as tmp:
            _, published, summary = self.build(
                tmp, manifest={"replay_version": "5.3.2"}
            )
        self.assertIsNone(published["quality"])
        self.assertIsNone(published["net_field_export_groups"])
        self.assertIsNone(published["level_names_and_times"])
        self.assertIn("quality", published)
        # Nothing was dropped, so this must NOT read as a lossy conversion.
        self.assertEqual(summary["tally"].total, 0)

    def test_account_subjects_are_not_forwarded(self):
        """`players` is deliberately left behind; prove it stays behind.

        vrfkit's manifest bridges actor guid -> account subject -> character
        guid. valplay derives the same table from the same BombPlayerState
        rows, and its version is strictly richer (a SET of characters, which is
        what keeps a resurrected player's kills attributed). Forwarding a
        poorer copy would add account UUIDs to a second file while offering a
        tempting alternative that silently loses those kills.
        """
        with tempfile.TemporaryDirectory() as tmp:
            out, published, _ = self.build(tmp, manifest=self.full_manifest())
            raw = (out / "manifest.json").read_text(encoding="utf-8")
        self.assertNotIn("players", published)
        self.assertNotIn("11111111-2222-3333-4444-555555555555", raw)

    def test_the_manifest_key_set_is_pinned(self):
        """The bundle's shape is a contract, so it is spelled out once."""
        with tempfile.TemporaryDirectory() as tmp:
            _, published, _ = self.build(tmp, manifest=self.full_manifest())
        self.assertEqual(
            sorted(published),
            sorted([
                "adapter",
                "bundle_schema_version",
                "converter",
                "duration_ms",
                "level_names_and_times",
                "net_field_export_groups",
                "quality",
                "replay_build",
                "replay_changelist",
                "replay_version",
                "source_file",
                "source_size_bytes",
            ]),
        )


class AdapterAccountingTests(SeamTestCase):
    """What this adapter measured, kept apart from what vrfkit declared."""

    def test_events_written_equals_the_lines_on_disk(self):
        """The one identity a consumer can re-verify exactly.

        valplay recounts events.ndjson; this is the number it recounts
        against. If they ever differ, the bundle was truncated after it was
        written, and no metric computed from it is worth publishing.
        """
        with tempfile.TemporaryDirectory() as tmp:
            out, published, _ = self.build(tmp, manifest=self.full_manifest())
            lines = (out / "events.ndjson").read_text(encoding="utf-8").splitlines()
        self.assertEqual(published["adapter"]["events_written"], len(lines))
        self.assertGreater(len(lines), 0)

    def test_movement_rows_read_is_the_table_height_not_the_written_count(self):
        """They differ by the intra-packet collapse, and both are published.

        Comparing the WRITTEN count with `quality.movement_rows` would report
        every healthy replay as lossy, because the collapse is intentional.
        """
        movement = [
            {"time_ms": 1, "packet_id": 1, "char": 5, "pos_x": 1.0},
            {"time_ms": 1, "packet_id": 1, "char": 5, "pos_x": 2.0},
            {"time_ms": 2, "packet_id": 2, "char": 5, "pos_x": 3.0},
        ]
        with tempfile.TemporaryDirectory() as tmp:
            _, published, _ = self.build(
                tmp, movement_rows=movement, manifest=self.full_manifest()
            )
        adapter = published["adapter"]
        self.assertEqual(adapter["movement_rows_read"], 3)
        self.assertEqual(adapter["movement_rows_written"], 2)
        self.assertTrue(adapter["upstream_row_counts"]["movement_rows"]["agrees"])

    def test_event_rows_are_reconciled_against_events_parquet_height(self):
        quality = json.loads(json.dumps(UPSTREAM_QUALITY))
        quality["event_rows"] = 2
        timeline = [
            {"group": "roundStarted", "time1": 100, "time2": 100,
             "word0": 7},
            {"group": "spikePlanted", "time1": 200, "time2": 200},
        ]
        with tempfile.TemporaryDirectory() as tmp:
            _, published, _ = self.build(
                tmp, timeline_rows=timeline,
                manifest=self.full_manifest(quality=quality),
            )
        adapter = published["adapter"]
        self.assertEqual(adapter["server_timeline_rows_read"], 2)
        self.assertEqual(adapter["server_timeline_events_written"], 2)
        self.assertEqual(
            adapter["upstream_row_counts"]["event_rows"],
            {"declared": 2, "observed": 2, "agrees": True},
        )

    def test_a_declared_count_its_own_table_contradicts_is_reported(self):
        """The producer disagreeing with itself is a signal, not a crash.

        The adapter counts it and publishes both numbers. It does not repair
        either -- which of the two is wrong is not knowable here -- and it does
        not refuse: refusing is the consumer's call, at the point of
        publication.
        """
        quality = json.loads(json.dumps(UPSTREAM_QUALITY))
        quality["movement_rows"] = 999
        quality["event_rows"] = 1
        with tempfile.TemporaryDirectory() as tmp:
            _, published, summary = self.build(
                tmp,
                movement_rows=[{"time_ms": 1, "packet_id": 1, "char": 5}],
                guid_rows=[{"net_guid": 1, "outer": 2, "path": "a"},
                           {"net_guid": 2, "outer": 3, "path": "b"}],
                timeline_rows=[{
                    "group": "spikePlanted", "time1": 1, "time2": 1,
                }],
                manifest=self.full_manifest(quality=quality),
            )
        counts = published["adapter"]["upstream_row_counts"]
        self.assertEqual(
            counts["movement_rows"],
            {"declared": 999, "observed": 1, "agrees": False},
        )
        # The table that DOES match must still read as agreement, so the
        # signal names the one table that is wrong.
        self.assertEqual(
            counts["net_guid_rows"],
            {"declared": 2, "observed": 2, "agrees": True},
        )
        self.assertEqual(
            counts["event_rows"],
            {"declared": 1, "observed": 1, "agrees": True},
        )
        self.assertEqual(summary["tally"]["upstream_row_count_disagreement"], 1)
        self.assertGreater(summary["tally"].total, 0)

    def test_a_declared_table_that_is_absent_is_a_disagreement(self):
        """"vrfkit wrote 2 rows" and "the file is not there" cannot both hold.

        An absent table used to convert silently -- weapon identity simply
        went unresolved -- which is right when nothing claimed the table
        existed. Once the export declares a row count, its absence is a
        contradiction and has to be said out loud.
        """
        with tempfile.TemporaryDirectory() as tmp:
            _, published, summary = self.build(tmp, manifest=self.full_manifest())
        check = published["adapter"]["upstream_row_counts"]["net_guid_rows"]
        self.assertEqual(check, {"declared": 2, "observed": None, "agrees": False})
        self.assertGreaterEqual(
            summary["tally"]["upstream_row_count_disagreement"], 1
        )

    def test_nothing_declared_reads_as_unknown_not_as_agreement(self):
        with tempfile.TemporaryDirectory() as tmp:
            _, published, summary = self.build(
                tmp, manifest={"replay_version": "5.3.2"}
            )
        for name in ("movement_rows", "net_guid_rows", "event_rows"):
            check = published["adapter"]["upstream_row_counts"][name]
            self.assertIsNone(check["declared"], name)
            self.assertIsNone(
                check["agrees"],
                "an export that declared nothing must not certify itself",
            )
        self.assertEqual(summary["tally"]["upstream_row_count_disagreement"], 0)

    def test_the_loss_tally_reaches_the_manifest(self):
        rows = list(MINIMAL_FIELD_ROWS) + [
            {
                "time_ms": 10, "packet_id": 1, "actor": 101,
                "group_path": "PlayerState", "field_name": None,
                "bit_count": 3, "raw_bits": b"\x05",
            },
        ]
        with tempfile.TemporaryDirectory() as tmp:
            _, published, _ = self.build(
                tmp, field_rows=rows, manifest=self.full_manifest()
            )
        losses = published["adapter"]["losses"]
        self.assertEqual(losses["unnamed_property_rows"], 1)
        # Present and zero, not absent: a key that appears only when non-zero
        # cannot distinguish "clean" from "this counter stopped running".
        self.assertEqual(losses["unnamed_rpc_rows"], 0)

    def test_the_loss_counter_set_is_pinned(self):
        """Every counter reaches the manifest under a fixed name, zero or not.

        Spelled out rather than read back from `_Tally.REASONS`: comparing the
        manifest with the dict it was written from could not fail. A counter
        that is renamed or dropped has to turn this red.
        """
        with tempfile.TemporaryDirectory() as tmp:
            _, published, _ = self.build(tmp, manifest=self.full_manifest())
        self.assertEqual(
            sorted(published["adapter"]["losses"]),
            sorted([
                "unnamed_property_rows",
                "unnamed_rpc_rows",
                "unnamed_rpc_invocations",
                "rpc_param_collisions",
                "property_key_collisions",
                "unparsable_path_segments",
                "payload_shape_conflicts",
                "multi_typed_rows",
                "fabricated_shot_locations",
                "fabricated_shot_rotations",
                "effect_half_read_pairs",
                "effect_array_residual_bits",
                "missing_manifest",
                "empty_gameplay_tag_table",
                "events_time_ms_regressions",
                "upstream_row_count_disagreement",
                "unknown_actor_lifecycle_events",
                "non_finite_movement_rows",
                "raw_blobs_unavailable",
                "damaged_bone_undecoded",
            ]),
        )


class NonFiniteMovementTests(SeamTestCase):
    """A non-finite movement value is written as the encoder spells it, and counted.

    The decoder can produce one: vrf-movement reads raw f32/f64 components
    with no finiteness check and stream.rs narrows f64 with a bare `as f32`.
    `Infinity`/`NaN` is how every other float in this bundle is spelled, and
    Python's json reads it; a strict parser (orjson) rejects the line. Before
    this, the shortened columns wrote +/-inf as -9223372036854775808 -- valid
    JSON, a plausible number, the wrong sign -- and NaN as `nan`, which no
    parser accepts, with nothing but a numpy warning on stderr.
    """

    def convert(self, movement):
        with tempfile.TemporaryDirectory() as tmp:
            out, published, summary = self.build(
                tmp, movement_rows=movement,
                manifest={"replay_version": "5.3.2"},
            )
            lines = (out / "movement.ndjson").read_text(
                encoding="utf-8").splitlines()
        return lines, published, summary

    def test_non_finite_values_are_encoded_and_counted(self):
        inf, nan = float("inf"), float("nan")
        movement = [
            {"time_ms": 1, "packet_id": 1, "char": 5, "pos_x": inf},
            {"time_ms": 2, "packet_id": 2, "char": 5, "vel_z": nan},
            {"time_ms": 3, "packet_id": 3, "char": 5, "yaw": -inf},
            {"time_ms": 4, "packet_id": 4, "char": 5, "pos_x": 1.5},
        ]
        lines, published, summary = self.convert(movement)
        rows = [json.loads(line) for line in lines]  # stdlib: non-strict
        self.assertEqual(len(rows), 4)
        self.assertIn('"position":{"x":Infinity,', lines[0])
        self.assertEqual(rows[0]["position"]["x"], inf)
        self.assertTrue(math.isnan(rows[1]["velocity"]["z"]))
        self.assertEqual(rows[2]["yaw"], -inf)
        self.assertEqual(rows[3]["position"]["x"], 1.5)
        # Rows, not values: three rows carry one non-finite value each.
        self.assertEqual(summary["tally"]["non_finite_movement_rows"], 3)
        self.assertEqual(
            published["adapter"]["losses"]["non_finite_movement_rows"], 3)
        self.assertTrue(any("non_finite_movement_rows" in line
                            for line in summary["tally"].lines()))

    def test_a_row_is_counted_once_however_many_of_its_values_are_bad(self):
        inf = float("inf")
        movement = [{"time_ms": 1, "packet_id": 1, "char": 5,
                     "pos_x": inf, "pos_y": -inf, "pitch": float("nan")}]
        _, _, summary = self.convert(movement)
        self.assertEqual(summary["tally"]["non_finite_movement_rows"], 1)

    def test_a_collapsed_sub_move_is_not_counted(self):
        """Only rows the bundle writes are counted; movement.parquet keeps the rest."""
        movement = [
            {"time_ms": 1, "packet_id": 1, "char": 5, "pos_x": float("inf")},
            {"time_ms": 1, "packet_id": 1, "char": 5, "pos_x": 2.0},
        ]
        lines, published, summary = self.convert(movement)
        self.assertEqual(len(lines), 1)
        self.assertEqual(summary["tally"]["non_finite_movement_rows"], 0)
        self.assertEqual(
            published["adapter"]["losses"]["non_finite_movement_rows"], 0)

    def test_finite_movement_counts_nothing(self):
        movement = [{"time_ms": 1, "packet_id": 1, "char": 5, "pos_x": 1e-05}]
        _, published, summary = self.convert(movement)
        self.assertEqual(summary["tally"]["non_finite_movement_rows"], 0)
        self.assertIn("non_finite_movement_rows", published["adapter"]["losses"])


class ServerTimelineEventTests(SeamTestCase):
    """Only non-private, structurally validated Event-chunk fields cross."""

    PRIVATE_MARKER = "DO-NOT-PUBLISH-ACCOUNT-SUBJECT"

    def convert_timeline(self, rows):
        with tempfile.TemporaryDirectory() as tmp:
            marked = []
            for row in rows:
                marked.append({
                    "id": self.PRIVATE_MARKER,
                    "metadata": self.PRIVATE_MARKER,
                    "raw_payload": self.PRIVATE_MARKER.encode(),
                    **row,
                })
            out, published, _ = self.build(
                tmp,
                # Use the authoritative actor table so this fixture does not
                # synthesize the legacy `last field + one packet` close after
                # every timeline row.  The ordering under test is the Event
                # table, not that old-table fallback.
                actor_rows=[{
                    "time_ms": 10, "packet_id": 1, "actor": 101,
                    "event": "open", "class_path": "/Game/Test/Test_C",
                    "archetype_path": "Default__Test_C",
                }],
                timeline_rows=marked,
                manifest=self.full_manifest(),
            )
            raw = (out / "events.ndjson").read_bytes()
            events = [json.loads(line) for line in raw.splitlines()]
            timeline = [e for e in events if e["type"] == "server_timeline_event"]
            return raw, timeline, published

    def test_private_event_columns_never_cross_the_allowlist(self):
        raw, timeline, _ = self.convert_timeline([
            {"group": "characterDeath", "time1": 500, "time2": 500,
             "word0": 101, "word1": 202, "payload_tag": 8,
             "payload_name": self.PRIVATE_MARKER, "payload_seconds": 0.5},
        ])
        self.assertNotIn(self.PRIVATE_MARKER.encode(), raw)
        self.assertEqual(timeline, [{
            "type": "server_timeline_event",
            "time_ms": 500,
            "time2_ms": 500,
            "event_group": "characterDeath",
            "word0": 101,
            "word1": 202,
        }])

    def test_public_structural_payload_fields_cross_as_neutral_values(self):
        _, timeline, _ = self.convert_timeline([
            {"group": "roundStarted", "time1": 100, "time2": 100,
             "word0": 4, "payload_tag": 2,
             "payload_name": "EReplayEventGroup::RoundStart",
             "payload_seconds": 0.1},
        ])
        self.assertEqual(timeline, [{
            "type": "server_timeline_event",
            "time_ms": 100,
            "time2_ms": 100,
            "event_group": "roundStarted",
            "word0": 4,
            "payload_tag": 2,
            "payload_name": "EReplayEventGroup::RoundStart",
            "payload_seconds": 0.1,
        }])

    def test_structural_payload_tuple_fails_closed_on_tag_or_time_drift(self):
        _, timeline, _ = self.convert_timeline([
            {"group": "roundStarted", "time1": 100, "time2": 100,
             "payload_tag": 99,
             "payload_name": "EReplayEventGroup::RoundStart",
             "payload_seconds": 0.1},
            {"group": "roundStarted", "time1": 100, "time2": 100,
             "payload_tag": 2,
             "payload_name": "EReplayEventGroup::RoundStart",
             "payload_seconds": 0.2},
        ])
        for event in timeline:
            self.assertNotIn("payload_tag", event)
            self.assertNotIn("payload_name", event)
            self.assertNotIn("payload_seconds", event)

    def test_words_are_forwarded_only_for_rusts_fixed_layout_groups(self):
        _, timeline, _ = self.convert_timeline([
            {"group": "roundStarted", "time1": 100, "time2": 100,
             "word0": 4, "word1": 999},
            {"group": "spikePlanted", "time1": 200, "time2": 200,
             "word0": 888, "word1": 999},
            {"group": "futureUnknown", "time1": 300, "time2": 301,
             "word0": 777, "word1": 999},
        ])
        self.assertEqual(timeline[0].get("word0"), 4)
        self.assertNotIn("word1", timeline[0])
        self.assertNotIn("word0", timeline[1])
        self.assertNotIn("word1", timeline[1])
        self.assertNotIn("word0", timeline[2])
        self.assertNotIn("word1", timeline[2])
        self.assertEqual(timeline[2]["time2_ms"], 301)

    def test_shuffled_timeline_rows_follow_packet_time_order(self):
        _, timeline, published = self.convert_timeline([
            {"group": "spikePlanted", "time1": 300, "time2": 300},
            {"group": "roundStarted", "time1": 100, "time2": 100,
             "word0": 1},
            {"group": "characterDeath", "time1": 200, "time2": 200,
             "word0": 10, "word1": 20},
        ])
        self.assertEqual([e["time_ms"] for e in timeline], [100, 200, 300])
        self.assertEqual(published["adapter"]["events_time_ms_regressions"], 0)


class EventOrderingContractTests(SeamTestCase):
    """The order events.ndjson is written in, pinned at the layer that sets it.

    valplay orders same-millisecond events by (time_ms, line index), which is
    only a total order if this file's output is in wire order and its time_ms
    column is non-decreasing. Neither was asserted anywhere, at any layer.
    """

    #: Two actors, three packets, deliberately supplied out of packet order in
    #: the parquet so a test that merely echoed input order would pass by luck.
    SHUFFLED = [
        {"time_ms": 30, "packet_id": 3, "actor": 303,
         "group_path": "PlayerState", "field_name": "Health",
         "bit_count": 32, "value_i64": 3},
        {"time_ms": 10, "packet_id": 1, "actor": 101,
         "group_path": "PlayerState", "field_name": "Health",
         "bit_count": 32, "value_i64": 1},
        {"time_ms": 20, "packet_id": 2, "actor": 202,
         "group_path": "PlayerState", "field_name": "Health",
         "bit_count": 32, "value_i64": 2},
    ]

    def read_events(self, out):
        text = (out / "events.ndjson").read_text(encoding="utf-8")
        return [json.loads(line) for line in text.splitlines()]

    def test_events_are_written_in_non_decreasing_time_order(self):
        with tempfile.TemporaryDirectory() as tmp:
            out, published, _ = self.build(
                tmp, field_rows=self.SHUFFLED, manifest=self.full_manifest()
            )
            events = self.read_events(out)
        times = [e["time_ms"] for e in events]
        self.assertEqual(times, sorted(times), events)
        self.assertEqual(published["adapter"]["events_time_ms_regressions"], 0)

    def test_a_spawn_precedes_the_property_event_at_the_same_millisecond(self):
        """The phase order (actors, properties, RPCs) is the tie-break.

        A property event for an actor that has not spawned yet is a document
        the consumer cannot read in one pass, so the stable sort's tie
        behaviour is a contract, not an implementation detail.
        """
        with tempfile.TemporaryDirectory() as tmp:
            out, _, _ = self.build(
                tmp, field_rows=self.SHUFFLED, manifest=self.full_manifest()
            )
            events = self.read_events(out)
        for actor in (101, 202, 303):
            same = [e for e in events if e.get("actor_net_guid") == actor]
            types = [e["type"] for e in same]
            self.assertEqual(
                types[0], "actor_spawned",
                f"actor {actor} is described before it exists: {types}",
            )

    def test_a_time_ms_regression_is_counted_and_published(self):
        """A frame whose time is not finite exports time_ms = 0.

        vrf-frame reads it with a bare read_f32 and substitutes 0 for anything
        non-finite, so one bad frame mid-replay makes time_ms non-monotonic
        while packet order stays correct. The adapter keeps packet order --
        that is the wire -- and reports the regression instead of hiding it by
        sorting on a value the replay does not guarantee.
        """
        rows = [
            {"time_ms": 10, "packet_id": 1, "actor": 101,
             "group_path": "PlayerState", "field_name": "Health",
             "bit_count": 32, "value_i64": 1},
            {"time_ms": 0, "packet_id": 2, "actor": 101,
             "group_path": "PlayerState", "field_name": "Health",
             "bit_count": 32, "value_i64": 2},
        ]
        with tempfile.TemporaryDirectory() as tmp:
            out, published, summary = self.build(
                tmp, field_rows=rows, manifest=self.full_manifest()
            )
            events = self.read_events(out)
        self.assertGreater(published["adapter"]["events_time_ms_regressions"], 0)
        self.assertGreater(summary["tally"]["events_time_ms_regressions"], 0)
        # Packet order is preserved: the regression is reported, not repaired.
        self.assertEqual(
            [e["time_ms"] for e in events if e["type"] == "export_group_received"],
            [10, 0],
        )


if __name__ == "__main__":
    unittest.main()
