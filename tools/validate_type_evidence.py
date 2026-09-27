"""Validate proposed overlay types directly against exported raw field bits.

This is an evidence tool, not a type inference tool.  The caller supplies a
JSON map of exact ``group_path``/``field_name`` pairs and expected types.  Every
matching payload must be consumed in full, and floating point values must be
finite.  The report includes widths and value ranges so a reviewer can reject a
technically decodable but implausible interpretation.

Usage:
    python tools/validate_type_evidence.py EXPORT_DIR EVIDENCE.json

``EXPORT_DIR`` may be one export or a directory containing exports.  Both
``fields.parquet`` and ``checkpoint_fields.parquet`` are inspected when present.
The JSON shape is ``[{"group": "...", "field": "...", "type": "Bool"}]``.
Supported types are Bool, Byte, Int32, Float, Double, FString, ObjectNetGuid,
EnumRemainingBits, RotationShort, VectorNetQuantize100, RepMovementByte and
RepMovementShort -- the names ``generate_scoped_types.py`` accepts.

The geometry decoders are written from Unreal's wire layout, not from the Rust
readers: a bounded ``SerializeInt(128)`` header whose low six bits give the
component width and whose seventh selects scaled integers (or doubles, when the
width is zero); two's-complement components; one presence bit per rotator
component followed by 8 or 16 bits; and ``ReplicatedMovement``'s four flag bits
gating angular velocity, server frame and server physics handle.
``--compare-typed`` parses the exported ``value_str`` back into numbers rather
than comparing spellings: vectors as doubles, rotator components as the single
precision floats Rust prints them from.

``ReplicatedMovement`` locations are read at scale 100 because that is what
``FieldType::RepMovement`` does. Exact consumption cannot check that scale --
it changes no width. Measured against actors.parquet spawn locations
(docs/UPSTREAM_RAZE_WARDEN.md), pawns replicate hundredths of a centimetre but
projectiles and game objects replicate whole centimetres, so for those classes
a value this accepts is still 100 times too small.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import struct
from collections import Counter, defaultdict
from pathlib import Path

import pyarrow as pa
import pyarrow.compute as pc
import pyarrow.parquet as pq


TYPES = frozenset({
    "Bool", "Byte", "Int32", "Float", "Double", "FString", "ObjectNetGuid",
    "EnumRemainingBits", "RotationShort", "VectorNetQuantize100",
    "RepMovementByte", "RepMovementShort",
})

#: The export column each type's value lands in.
TYPED_COLUMN = {
    "Bool": "value_bool", "FString": "value_str",
    "Float": "value_f64", "Double": "value_f64",
    "Byte": "value_i64", "Int32": "value_i64",
    "ObjectNetGuid": "value_i64", "EnumRemainingBits": "value_i64",
    "RotationShort": "value_str", "VectorNetQuantize100": "value_str",
    "RepMovementByte": "value_str", "RepMovementShort": "value_str",
}
SHAPED_TYPES = frozenset({"RotationShort", "VectorNetQuantize100",
                          "RepMovementByte", "RepMovementShort"})


class _Bits:
    """Exactly ``bit_count`` bits, read least significant bit first -- the
    order Unreal's bit writer fills bytes in and assembles multi-bit values."""

    def __init__(self, raw: bytes, bit_count: int):
        self.raw, self.end, self.pos = raw, bit_count, 0

    def read(self, count: int) -> int:
        if self.pos + count > self.end:
            raise ValueError("truncated payload")
        value = 0
        for index in range(count):
            position = self.pos + index
            value |= ((self.raw[position >> 3] >> (position & 7)) & 1) << index
        self.pos += count
        return value

    def serialize_int(self, maximum: int) -> int:
        """Unreal's ``FBitReader::SerializeInt``: one bit per power of two
        below ``maximum``, stopping as soon as the next one would reach it."""
        value, mask = 0, 1
        while value + mask < maximum:
            if self.read(1):
                value |= mask
            mask <<= 1
        return value

    def int_packed(self) -> int:
        value = 0
        for index in range(5):
            byte = self.read(8)
            chunk = byte >> 1
            if index == 4:
                if byte & 1:
                    raise ValueError("runaway IntPacked")
                if chunk > 15:
                    raise ValueError("overflowing IntPacked")
            value |= chunk << (index * 7)
            if not byte & 1:
                return value
        raise AssertionError("unreachable IntPacked loop end")


def _quantized_vector(bits: _Bits, scale: int) -> tuple:
    header = bits.serialize_int(1 << 7)
    width, scaled = header & 63, header >> 6
    if width:
        sign = 1 << (width - 1)
        components = []
        for _ in range(3):
            value = bits.read(width)
            value = value - (1 << width) if value & sign else value
            components.append(value / scale if scaled else float(value))
        return tuple(components)
    size, fmt = (64, "<d") if scaled else (32, "<f")
    components = tuple(
        struct.unpack(fmt, bits.read(size).to_bytes(size // 8, "little"))[0]
        for _ in range(3)
    )
    if not all(math.isfinite(value) for value in components):
        raise ValueError("non-finite vector component")
    return components


def _rotator(bits: _Bits, width: int) -> tuple:
    return tuple(
        bits.read(width) * 360.0 / (1 << width) if bits.read(1) else 0.0
        for _ in range(3)
    )


def _rep_movement(bits: _Bits, rotation_width: int) -> dict:
    sleep, physics, frame, handle = (bool(bits.read(1)) for _ in range(4))
    location = _quantized_vector(bits, 100)
    rotation = _rotator(bits, rotation_width)
    velocity = _quantized_vector(bits, 1)
    angular = _quantized_vector(bits, 1) if physics else None
    server_frame = bits.int_packed() if frame else None
    server_handle = bits.int_packed() if handle else None
    return {
        "location": location, "rotation": rotation, "linear_velocity": velocity,
        "angular_velocity": angular, "simulated_physics_sleep": sleep,
        "rep_physics": physics, "server_frame": server_frame,
        "server_physics_handle": server_handle,
    }


def _decode_shaped(raw: bytes, bit_count: int, type_name: str):
    bits = _Bits(raw, bit_count)
    if type_name == "RotationShort":
        value = _rotator(bits, 16)
    elif type_name == "VectorNetQuantize100":
        value = _quantized_vector(bits, 100)
    else:
        value = _rep_movement(bits, 8 if type_name == "RepMovementByte" else 16)
    if bits.pos != bit_count:
        raise ValueError(f"{type_name} leaves {bit_count - bits.pos} residual bits")
    return value


def _as_f32(value) -> float:
    return struct.unpack("<f", struct.pack("<f", value))[0]


def _parse_triple(text, prefix: str):
    if not isinstance(text, str) or not text.startswith(prefix) or not text.endswith(")"):
        return None
    parts = text[len(prefix):-1].split(",")
    if len(parts) != 3:
        return None
    try:
        return tuple(float(part) for part in parts)
    except ValueError:
        return None


def _json_vector(document: dict, key: str):
    item = document.get(key)
    if item is None:
        return None
    return (item.get("x"), item.get("y"), item.get("z"))


def exported_matches(type_name: str, exported, decoded) -> bool:
    """Whether the exported column holds the independently decoded value.

    Scalars compare directly. Geometry is parsed out of ``value_str``:
    vectors are doubles and must be equal; rotator components are single
    precision in the model and are compared after rounding both sides to f32,
    because the shortest f32 spelling does not parse back to the exact value
    as a double.
    """
    if type_name == "VectorNetQuantize100":
        return _parse_triple(exported, "(") == decoded
    if type_name == "RotationShort":
        parsed = _parse_triple(exported, "rot(")
        return parsed is not None and all(
            _as_f32(a) == _as_f32(b) for a, b in zip(parsed, decoded))
    if type_name in {"RepMovementByte", "RepMovementShort"}:
        try:
            document = json.loads(exported)
        except (TypeError, ValueError):
            return False
        if not isinstance(document, dict) or set(document) != set(decoded):
            return False
        rotation = document.get("rotation") or {}
        return (
            _json_vector(document, "location") == decoded["location"]
            and _json_vector(document, "linear_velocity") == decoded["linear_velocity"]
            and _json_vector(document, "angular_velocity") == decoded["angular_velocity"]
            and set(rotation) == {"pitch", "yaw", "roll"}
            and all(_as_f32(rotation[axis]) == _as_f32(expected)
                    for axis, expected in zip(("pitch", "yaw", "roll"), decoded["rotation"]))
            and document["simulated_physics_sleep"] is decoded["simulated_physics_sleep"]
            and document["rep_physics"] is decoded["rep_physics"]
            and document["server_frame"] == decoded["server_frame"]
            and document["server_physics_handle"] == decoded["server_physics_handle"]
        )
    return exported == decoded


def _summary(type_name: str, value):
    """The numbers a range check can use: a scalar, or a vector's components."""
    if type_name in {"RepMovementByte", "RepMovementShort"}:
        return value["location"]
    if isinstance(value, tuple):
        return value
    if isinstance(value, (int, float)) and not isinstance(value, bool):
        return value
    return None


def decode_exact(raw: bytes, bit_count: int, type_name: str):
    """Decode one value while requiring the declared payload width."""
    if len(raw or b"") != (bit_count + 7) // 8:
        raise ValueError("raw byte length does not cover bit_count exactly")
    if type_name == "EnumRemainingBits":
        # Every remaining bit is the value, and a 0-bit field is the enum's
        # zero. Wider than 32 bits is more than the reader holds, so it is
        # refused rather than truncated.
        if bit_count > 32:
            raise ValueError("EnumRemainingBits wider than 32 bits")
        return _Bits(raw or b"", bit_count).read(bit_count)
    if type_name in SHAPED_TYPES:
        return _decode_shaped(raw, bit_count, type_name)
    if type_name == "Bool":
        if bit_count != 1:
            raise ValueError("Bool is not one bit")
        return bool(raw[0] & 1)
    if type_name == "Byte":
        if bit_count != 8:
            raise ValueError("Byte is not eight bits")
        return raw[0]
    if type_name == "Int32":
        if bit_count != 32:
            raise ValueError("Int32 is not 32 bits")
        return struct.unpack("<i", raw)[0]
    if type_name in {"Float", "Double"}:
        width, fmt = (32, "<f") if type_name == "Float" else (64, "<d")
        if bit_count != width:
            raise ValueError(f"{type_name} is not {width} bits")
        value = struct.unpack(fmt, raw)[0]
        if not math.isfinite(value):
            raise ValueError(f"{type_name} is non-finite")
        return value
    if type_name == "FString":
        if bit_count % 8 or bit_count < 32:
            raise ValueError("FString is not byte-aligned or lacks its length")
        length = struct.unpack_from("<i", raw)[0]
        unit = 1 if length >= 0 else 2
        if len(raw) != 4 + abs(length) * unit:
            raise ValueError("FString length does not consume the payload")
        if length == 0:
            return ""
        if raw[-unit:] != b"\0" * unit:
            raise ValueError("FString lacks its terminator")
        return raw[4:-unit].decode("utf-8" if length > 0 else "utf-16-le")
    if type_name == "ObjectNetGuid":
        value = 0
        consumed = 0
        for index in range(5):
            if consumed + 8 > bit_count:
                raise ValueError("truncated IntPacked ObjectNetGuid")
            byte = raw[consumed // 8]
            consumed += 8
            # Unreal's IntPacked stores continuation in the low bit; the upper
            # seven bits are payload, unlike conventional high-bit varints.
            chunk = byte >> 1
            if index == 4:
                if byte & 1:
                    raise ValueError("runaway IntPacked ObjectNetGuid")
                if chunk > 15:
                    raise ValueError("overflowing IntPacked ObjectNetGuid")
            value |= chunk << (index * 7)
            if not byte & 1:
                if consumed != bit_count:
                    raise ValueError("ObjectNetGuid leaves residual bits")
                return value
        raise AssertionError("unreachable IntPacked loop end")
    raise ValueError(f"unsupported evidence type {type_name!r}")


def parquet_files(root: Path, export_ids=None):
    if root.is_file():
        yield root
        return
    if export_ids:
        for export_id in export_ids:
            export = root / export_id
            for name in ("fields.parquet", "checkpoint_fields.parquet"):
                path = export / name
                if path.exists():
                    yield path
        return
    for name in ("fields.parquet", "checkpoint_fields.parquet"):
        yield from root.rglob(name)


def validate(export_root: Path, specifications: list[dict], export_ids=None, compare_typed=False) -> dict:
    if not export_root.exists():
        raise ValueError(f"export root does not exist: {export_root}")
    allowed = TYPES
    if not specifications:
        raise ValueError("evidence specification is empty")
    for spec in specifications:
        if set(spec) not in ({"group", "field", "type"}, {"group", "field", "type", "checksum"}) or not spec["group"] or not spec["field"]:
            raise ValueError(f"invalid evidence specification: {spec!r}")
        if "checksum" in spec and (type(spec["checksum"]) is not int or not 0 < spec["checksum"] <= 0xFFFFFFFF):
            raise ValueError("checksum scope must be a nonzero u32")
        if spec["type"] not in allowed:
            raise ValueError(f"unsupported evidence type: {spec['type']!r}")
    expected = {(s["group"], s["field"], s.get("checksum")): s["type"] for s in specifications}
    if len(expected) != len(specifications):
        raise ValueError("duplicate group/field specification")
    for group, field, checksum in expected:
        if checksum is not None and (group, field, None) in expected:
            raise ValueError("overlapping scoped and unscoped specification")
    def label_for(key):
        group, field, checksum = key
        suffix = f"::checksum={checksum}" if checksum is not None else ""
        return f"{group}::{field}{suffix}"
    counts = Counter()
    widths = defaultdict(Counter)
    wire_keys = defaultdict(Counter)
    minima, maxima = {}, {}
    failures = []
    failure_count = 0
    failure_counts = Counter()
    typed_mismatch_count = 0
    typed_mismatch_examples = []
    paths = list(parquet_files(export_root, export_ids))
    if not paths:
        raise ValueError(f"no field parquet files below {export_root}")
    wanted_groups = pa.array(sorted({group for group, _field, _checksum in expected}))
    for path in paths:
        parquet = pq.ParquetFile(path)
        columns = ["group_path", "field_name", "handle", "compatible_checksum",
                   "bit_count", "raw_bits"]
        if compare_typed:
            columns += ["value_str", "value_i64", "value_f64", "value_bool"]
        for batch in parquet.iter_batches(
            batch_size=131072,
            columns=columns,
            use_threads=False,
        ):
            table = pa.Table.from_batches([batch])
            table = table.filter(pc.is_in(pc.cast(table["group_path"], pa.string()), value_set=wanted_groups))
            for row in table.to_pylist():
                key = (row["group_path"], row["field_name"], row["compatible_checksum"])
                if key not in expected:
                    key = (row["group_path"], row["field_name"], None)
                type_name = expected.get(key)
                if type_name is None:
                    continue
                label = label_for(key)
                counts[label] += 1
                widths[label][row["bit_count"]] += 1
                wire_keys[label][
                    f"handle={row['handle']},checksum={row['compatible_checksum']},bits={row['bit_count']}"
                ] += 1
                try:
                    value = decode_exact(row["raw_bits"], row["bit_count"], type_name)
                    if compare_typed:
                        column = TYPED_COLUMN[type_name]
                        if not exported_matches(type_name, row[column], value):
                            typed_mismatch_count += 1
                            if len(typed_mismatch_examples) < 32:
                                typed_mismatch_examples.append({
                                    "file": str(path), "field": label,
                                    "decoded": value, "exported": row[column],
                                })
                    summary = _summary(type_name, value)
                    if isinstance(summary, tuple):
                        low = minima.get(label, summary)
                        high = maxima.get(label, summary)
                        minima[label] = tuple(map(min, low, summary))
                        maxima[label] = tuple(map(max, high, summary))
                    elif summary is not None:
                        minima[label] = min(minima.get(label, summary), summary)
                        maxima[label] = max(maxima.get(label, summary), summary)
                except (UnicodeError, ValueError) as exc:
                    failure_count += 1
                    failure_counts[label] += 1
                    if len(failures) < 32:
                        failures.append({"file": str(path), "field": label, "error": str(exc)})
    missing = [label_for(key) for key in expected if not counts[label_for(key)]]
    return {
        "fields": {
            label: {
                "rows": count,
                "widths": dict(sorted(widths[label].items())),
                "wire_keys": dict(sorted(wire_keys[label].items())),
                **({"min": minima[label], "max": maxima[label]} if label in minima else {}),
            }
            for label, count in sorted(counts.items())
        },
        "missing": missing,
        "failure_count": failure_count,
        "failure_counts": dict(sorted(failure_counts.items())),
        "failure_examples": failures,
        "typed_mismatch_count": typed_mismatch_count,
        "typed_mismatch_examples": typed_mismatch_examples,
    }


def main(argv=None):
    parser = argparse.ArgumentParser()
    parser.add_argument("export_root", type=Path)
    parser.add_argument("evidence", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--export-id", action="append", dest="export_ids")
    parser.add_argument("--compare-typed", action="store_true")
    args = parser.parse_args(argv)
    specifications = json.loads(args.evidence.read_text(encoding="utf-8"))
    report = validate(args.export_root, specifications, args.export_ids, args.compare_typed)
    report["provenance"] = {
        "export_root": str(args.export_root.resolve()),
        "parquet_files": len(list(parquet_files(args.export_root, args.export_ids))),
        "export_ids": sorted(args.export_ids) if args.export_ids else None,
        "specification": str(args.evidence.resolve()),
        "specification_sha256": hashlib.sha256(args.evidence.read_bytes()).hexdigest(),
    }
    rendered = json.dumps(report, indent=2, ensure_ascii=True) + "\n"
    if args.output:
        args.output.write_text(rendered, encoding="utf-8")
    print(rendered, end="")
    return 1 if (report["missing"] or report["failure_count"]
                 or report["typed_mismatch_count"]) else 0


if __name__ == "__main__":
    raise SystemExit(main())
