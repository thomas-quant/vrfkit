"""Validate proposed overlay types directly against exported raw field bits.

This is an evidence tool, not a type inference tool.  The caller supplies a
JSON map of exact ``group_path``/``field_name`` pairs and expected primitive
types.  Every matching payload must be consumed in full, and numeric floating
point values must be finite.  The report includes widths and value ranges so a
reviewer can reject a technically decodable but implausible interpretation.

Usage:
    python tools/validate_type_evidence.py EXPORT_DIR EVIDENCE.json

``EXPORT_DIR`` may be one export or a directory containing exports.  Both
``fields.parquet`` and ``checkpoint_fields.parquet`` are inspected when present.
The JSON shape is ``[{"group": "...", "field": "...", "type": "Bool"}]``.
Supported types are Bool, Byte, Int32, Float, Double, FString, ObjectNetGuid,
EnumByte, FName, RepMovementByte and RepMovementShort.

The last four are read bit by bit, because their payloads are not byte
multiples and one of them does not even start on a byte boundary:

* ``EnumByte`` -- 1..8 bits, the whole payload is the value. A byte-sized enum
  is sent with only its significant bits, so a 3-bit payload is the normal
  case, which is why ``Byte`` (exactly 8 bits) cannot check it.
* ``FName`` -- one ``isHardcoded`` bit, then either an IntPacked name index
  (rendered as its decimal string) or an inline FString plus an i32 instance
  number (0 renders the bare name, ``N`` renders ``name_{N-1}``). Everything
  after the flag bit is one bit off byte alignment.
* ``RepMovementByte`` / ``RepMovementShort`` -- ``FRepMovement``: four flag
  bits, a quantized location (scaled by 100 when the header says so), a
  rotator with 8- or 16-bit components, a quantized linear velocity, then the
  optional angular velocity and IntPacked server frame/handle. With
  ``--compare-typed`` the exported ``value_str`` JSON is parsed and compared
  numerically, rotator components after rounding to f32 -- a string compare
  would fail on ``1`` against ``1.0`` rather than on a wrong value.

Bit-level payloads must also carry zero padding above ``bit_count``.
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


def _signed(value: int, width: int) -> int:
    """Two's-complement sign extension of a `width`-bit field."""
    sign = 1 << (width - 1)
    return (value ^ sign) - sign


class _Bits:
    """LSB-first reader over exactly ``bit_count`` bits of one payload.

    Written from the Unreal layouts rather than from ``vrf-bitio``, so the two
    readers can disagree. Reading past ``bit_count`` raises, and so does
    nonzero padding above it: the exporter zero-fills that padding, so a set
    bit there means the payload window is not the one the type describes.
    """

    def __init__(self, raw: bytes, bit_count: int):
        self._value = int.from_bytes(raw, "little")
        if self._value >> bit_count:
            raise ValueError("nonzero padding above bit_count")
        self._end = bit_count
        self.pos = 0

    def bits(self, count: int) -> int:
        if self.pos + count > self._end:
            raise ValueError("read past the end of the payload")
        value = (self._value >> self.pos) & ((1 << count) - 1)
        self.pos += count
        return value

    def bit(self) -> bool:
        return bool(self.bits(1))

    def remaining(self) -> int:
        return self._end - self.pos

    def int_packed(self) -> int:
        """``SerializeIntPacked``: 7 payload bits per byte, low bit continues."""
        value = 0
        for index in range(5):
            byte = self.bits(8)
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

    def serialized_int(self, max_value: int) -> int:
        """``FBitReader::SerializeInt``: floor(log2(max)) bits, then one more
        only while the value could still be raised without reaching max."""
        width = max_value.bit_length() - 1
        value = self.bits(width)
        mask = 1 << width
        if value + mask < max_value and self.bit():
            value |= mask
        return value

    def fstring(self) -> str:
        length = _signed(self.bits(32), 32)
        if length == 0:
            return ""
        unit = 1 if length > 0 else 2
        data = bytes(self.bits(8) for _ in range(abs(length) * unit))
        if data[-unit:] != b"\0" * unit:
            raise ValueError("FString lacks its terminator")
        return data[:-unit].decode("utf-8" if unit == 1 else "utf-16-le")


def _fname(reader: _Bits) -> str:
    if reader.bit():
        return str(reader.int_packed())
    name = reader.fstring()
    number = _signed(reader.bits(32), 32)
    if number < 0:
        raise ValueError("negative FName instance number")
    return name if number == 0 else f"{name}_{number - 1}"


def _quantized_vector(reader: _Bits, scale: int) -> tuple[float, float, float]:
    """``ReadPackedVector``: a SerializeInt(128) header whose low six bits are
    the component width and whose seventh says "scaled"; width 0 falls back to
    three raw floats, or doubles when the seventh bit is set."""
    header = reader.serialized_int(1 << 7)
    width, scaled = header & 63, header >> 6
    if width:
        parts = [_signed(reader.bits(width), width) for _ in range(3)]
        return tuple(p / scale if scaled else float(p) for p in parts)
    size, fmt = (64, "<d") if scaled else (32, "<f")
    parts = [struct.unpack(fmt, reader.bits(size).to_bytes(size // 8, "little"))[0]
             for _ in range(3)]
    if not all(math.isfinite(p) for p in parts):
        raise ValueError("non-finite packed vector component")
    return tuple(parts)


def _rep_movement(reader: _Bits, rotation_bits: int) -> dict:
    """``FRepMovement::NetSerialize``, in wire order."""
    sleep = reader.bit()
    physics = reader.bit()
    frame = reader.bit()
    handle = reader.bit()
    location = _quantized_vector(reader, 100)
    scale = 360.0 / (1 << rotation_bits)
    rotation = []
    for _axis in ("pitch", "yaw", "roll"):
        present = reader.bit()
        rotation.append(_f32(reader.bits(rotation_bits) * scale) if present else 0.0)
    velocity = _quantized_vector(reader, 1)
    angular = _quantized_vector(reader, 1) if physics else None
    server_frame = reader.int_packed() if frame else None
    server_handle = reader.int_packed() if handle else None
    return {
        "linear_velocity": velocity,
        "angular_velocity": angular,
        "location": location,
        "rotation": tuple(rotation),
        "simulated_physics_sleep": sleep,
        "rep_physics": physics,
        "server_frame": server_frame,
        "server_physics_handle": server_handle,
    }


def _f32(value: float) -> float:
    return struct.unpack("<f", struct.pack("<f", value))[0]


def _exported_rep_movement(text: str) -> dict:
    """The exporter's ``value_str`` JSON in the shape ``_rep_movement`` returns."""
    obj = json.loads(text)

    def vector(v):
        return None if v is None else (v["x"], v["y"], v["z"])

    rotation = obj["rotation"]
    return {
        "linear_velocity": vector(obj["linear_velocity"]),
        "angular_velocity": vector(obj["angular_velocity"]),
        "location": vector(obj["location"]),
        "rotation": tuple(_f32(rotation[k]) for k in ("pitch", "yaw", "roll")),
        "simulated_physics_sleep": obj["simulated_physics_sleep"],
        "rep_physics": obj["rep_physics"],
        "server_frame": obj["server_frame"],
        "server_physics_handle": obj["server_physics_handle"],
    }


#: Types read with `_Bits`, and the column their typed value is exported in.
BIT_LEVEL_TYPES = {"EnumByte": "value_i64", "FName": "value_str",
                   "RepMovementByte": "value_str", "RepMovementShort": "value_str"}


def _decode_bits(raw: bytes, bit_count: int, type_name: str):
    reader = _Bits(raw, bit_count)
    if type_name == "EnumByte":
        if not 1 <= bit_count <= 8:
            raise ValueError("EnumByte is not 1..8 bits")
        value = reader.bits(bit_count)
    elif type_name == "FName":
        value = _fname(reader)
    else:
        value = _rep_movement(reader, 8 if type_name == "RepMovementByte" else 16)
    if reader.remaining():
        raise ValueError(f"{type_name} leaves {reader.remaining()} residual bits")
    return value


def decode_exact(raw: bytes, bit_count: int, type_name: str):
    """Decode one primitive while requiring the declared payload width."""
    if len(raw) != (bit_count + 7) // 8:
        raise ValueError("raw byte length does not cover bit_count exactly")
    if type_name in BIT_LEVEL_TYPES:
        return _decode_bits(raw, bit_count, type_name)
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


#: Every supported evidence type, and the column its exported value lives in.
TYPED_COLUMNS = {
    "Bool": "value_bool", "FString": "value_str",
    "Float": "value_f64", "Double": "value_f64",
    "Byte": "value_i64", "Int32": "value_i64",
    "ObjectNetGuid": "value_i64",
    **BIT_LEVEL_TYPES,
}


def exported_value(row: dict, type_name: str):
    """The exported typed value, in the shape `decode_exact` returns.

    A RepMovement `value_str` that does not parse is returned as a marker that
    equals no decoded value, so it counts as a mismatch instead of stopping the
    run.
    """
    value = row[TYPED_COLUMNS[type_name]]
    if type_name.startswith("RepMovement") and value is not None:
        try:
            return _exported_rep_movement(value)
        except (ValueError, KeyError, TypeError):
            return {"unparseable value_str": value}
    return value


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
    allowed = set(TYPED_COLUMNS)
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
    # Narrowing by field name as well is only a speed-up -- a row whose name no
    # specification lists is skipped below either way -- but a group such as a
    # weapon's `_ClassNetCache` carries millions of rows of other parameters.
    wanted_fields = pa.array(sorted({field for _group, field, _checksum in expected}))
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
            table = table.filter(pc.is_in(pc.cast(table["field_name"], pa.string()), value_set=wanted_fields))
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
                        exported = exported_value(row, type_name)
                        if exported != value:
                            typed_mismatch_count += 1
                            if len(typed_mismatch_examples) < 32:
                                typed_mismatch_examples.append({
                                    "file": str(path), "field": label,
                                    "decoded": value,
                                    "exported": row[TYPED_COLUMNS[type_name]],
                                })
                    if isinstance(value, (int, float)) and not isinstance(value, bool):
                        minima[label] = min(minima.get(label, value), value)
                        maxima[label] = max(maxima.get(label, value), value)
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
