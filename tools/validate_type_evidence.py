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
Supported types are Bool, Byte, Int32, UInt32, Float, Double, VectorDouble,
FString, ObjectNetGuid, EnumByte, EnumRemainingBits, FName, FTextTree,
RotationShort, VectorNetQuantize100, RepMovementByte and RepMovementShort --
among them every type name ``generate_scoped_types.py`` accepts. UInt32 is
read unsigned, so a value with the high bit set must be exported positive;
an Int32 reading of the same bits would pass the width check and still be
wrong. VectorDouble is an ``FVector`` sent as three little-endian doubles,
exactly 192 bits, every component finite; with ``--compare-typed`` the
exported ``value_str`` ``(x,y,z)`` is parsed back into doubles, which the
shortest round-trip spelling Rust prints them in reproduces exactly.

All but the first nine are read bit by bit, because their payloads are not
byte multiples and some of them do not even start on a byte boundary:

* ``EnumByte`` -- 1..8 bits, the whole payload is the value. A byte-sized enum
  is sent with only its significant bits, so a 3-bit payload is the normal
  case, which is why ``Byte`` (exactly 8 bits) cannot check it.
* ``EnumRemainingBits`` -- every bit of the payload is the value, up to 32; a
  0-bit field is the enum's zero.
* ``FName`` -- one ``isHardcoded`` bit, then either an IntPacked name index
  (rendered as its decimal string) or an inline FString plus an i32 instance
  number (0 renders the bare name, ``N`` renders ``name_{N-1}``). Everything
  after the flag bit is one bit off byte alignment.
* ``FTextTree`` -- an ``FText``: u32 flags, a history byte and the history
  body, for the histories the Rust tree reader accepts (255 empty, 11 string
  table, 3 argument format, 4 as-number with a double source). With
  ``--compare-typed`` the exported JSON is parsed and must equal the
  independent read key for key; the double is compared exactly.
* ``RotationShort`` -- one presence bit per rotator component, each followed
  by 16 bits when set.
* ``VectorNetQuantize100`` -- a bounded ``SerializeInt(128)`` header whose low
  six bits give the component width and whose seventh selects scaled integers
  (or doubles, when the width is zero); two's-complement components, divided
  by 100 when scaled. With ``--compare-typed`` the exported ``value_str`` is
  parsed back into numbers rather than compared as a spelling: vectors as
  doubles, rotator components as the single precision floats Rust prints them
  from.
* ``RepMovementByte`` / ``RepMovementShort`` -- ``FRepMovement``: four flag
  bits, a quantized location, a rotator with 8- or 16-bit components, a
  quantized linear velocity, then the optional angular velocity and IntPacked
  server frame/handle. With ``--compare-typed`` the exported ``value_str`` JSON
  is parsed and compared numerically, rotator components after rounding to
  f32 -- a string compare would fail on ``1`` against ``1.0`` rather than on a
  wrong value.

  The location is compared WITHOUT assuming its scale. The packed header only
  says "scaled"; the scale itself is not on the wire. The Rust reader
  divides by the level each class's table or scoped entry states
  (``FieldType::RepMovement { location }``): whole units on every class but
  the two-decimal pawns (docs/DATA.md, "`ReplicatedMovement.location` is
  world units, at a per-class level"). So the check accepts the packed
  integers at /100 or at /1, requires everything else to match exactly, and
  reports which scale each row was exported at (``location_scales``): "/1"
  for a whole-unit class, "/100" for a two-decimal one. Which level a class
  must have is not this check's to decide; the Rust test
  ``every_rep_movement_entry_carries_its_measured_location_level`` pins it
  per class against spawn-position evidence.

The geometry decoders are written from Unreal's wire layout, not from the Rust
readers, so the two can disagree.

Bit-level payloads must also carry zero padding above ``bit_count``.

A directory is searched recursively. That search skips the staging and backup
directories ``vrfkit export`` leaves beside an interrupted export (listed under
``skipped_generated_dirs``; see ``export_scan.py``) and refuses a table with no
``manifest.json`` beside it, because vrfkit writes the manifest last.  A table
file or ``--export-id`` names its input explicitly and is read as given.
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

if __package__:
    from .export_scan import generated_ancestor, leftover_note, skipped_report
else:
    from export_scan import generated_ancestor, leftover_note, skipped_report


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
            raise ValueError("truncated payload: read past the end of the payload")
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


#: Nesting limit shared with the Rust tree reader; deeper input is refused.
FTEXT_MAX_DEPTH = 16
#: A format history's argument count must lie in 0..=this, as in Rust.
FTEXT_MAX_ARGUMENTS = 128
#: The seven `FNumberFormattingOptions` members, in wire order after the two
#: bools and the rounding byte.
FTEXT_DIGITS = ("minimum_integral_digits", "maximum_integral_digits",
                "minimum_fractional_digits", "maximum_fractional_digits")


def _ftext_bool(reader: _Bits) -> bool:
    """An archive bool: a whole u32 that must be 0 or 1."""
    value = reader.bits(32)
    if value not in (0, 1):
        raise ValueError(f"FText bool is {value}, not 0 or 1")
    return bool(value)


def _ftext_tree(reader: _Bits, depth: int = 0) -> dict:
    """``FText`` serialization: u32 flags, a history byte, then the history.

    Only the histories the Rust tree reader accepts, written from the Unreal
    layouts rather than from ``ftext.rs``:

    * 255 (none): zero flags and a zero u32 (no culture-invariant string);
    * 11 (string table): an inline-name bit that must be clear, the table's
      FName (FString + i32 number) and the key FString;
    * 3 (argument format): a nested source text, an i32 argument count, and
      per argument its name, a type byte and a value -- 0 an int64 kept as
      its unsigned bits, 4 a nested text;
    * 4 (as number): a type byte that must be 3 (double), the double, an
      archive bool, the seven ``FNumberFormattingOptions`` members when it is
      set, and the target culture FString.

    Returned in the shape the exporter's JSON takes, so ``--compare-typed``
    compares parsed JSON with this dict.
    """
    if depth >= FTEXT_MAX_DEPTH:
        raise ValueError("FText nesting exceeds the depth limit")
    flags = reader.bits(32)
    history = reader.bits(8)
    if history == 255:
        if flags != 0 or reader.bits(32) != 0:
            raise ValueError("FText empty history must have zero flags and zero length")
        return {"flags": flags, "history": 255, "kind": "empty"}
    if history == 11:
        if reader.bit():
            raise ValueError("FText string table name is not inline")
        name = reader.fstring()
        number = _signed(reader.bits(32), 32)
        if number < 0:
            raise ValueError("negative FText table name number")
        key = reader.fstring()
        return {"flags": flags, "history": 11, "kind": "string_table",
                "table": {"name": name, "number": number}, "key": key}
    if history == 3:
        source = _ftext_tree(reader, depth + 1)
        count = _signed(reader.bits(32), 32)
        if not 0 <= count <= FTEXT_MAX_ARGUMENTS:
            raise ValueError(f"FText argument count {count}")
        arguments = []
        for _ in range(count):
            name = reader.fstring()
            tag = reader.bits(8)
            if tag == 0:
                value = {"bits_u64": str(reader.bits(64))}
            elif tag == 4:
                value = _ftext_tree(reader, depth + 1)
            else:
                raise ValueError(f"FText argument tag {tag}")
            arguments.append({"name": name, "tag": tag, "value": value})
        return {"flags": flags, "history": 3, "kind": "format", "source": source,
                "arguments": arguments}
    if history == 4:
        tag = reader.bits(8)
        if tag != 3:
            raise ValueError(f"FText number source tag {tag}")
        number = struct.unpack("<d", reader.bits(64).to_bytes(8, "little"))[0]
        if not math.isfinite(number):
            raise ValueError("FText number is non-finite")
        options = None
        if _ftext_bool(reader):
            options = {"always_sign": _ftext_bool(reader), "use_grouping": _ftext_bool(reader),
                       "rounding_mode": _signed(reader.bits(8), 8)}
            for member in FTEXT_DIGITS:
                options[member] = _signed(reader.bits(32), 32)
        culture = reader.fstring()
        return {"flags": flags, "history": 4, "kind": "as_number",
                "source": {"tag": 3, "double": number}, "format": options, "culture": culture}
    raise ValueError(f"FText history {history}")


def _packed_vector(reader: _Bits) -> dict:
    """``ReadPackedVector``: a SerializeInt(128) header whose low six bits are
    the component width and whose seventh says "scaled"; width 0 falls back to
    three raw floats, or doubles when the seventh bit is set.

    Returned unscaled -- ``{"packed": ints, "scaled": flag}`` or
    ``{"floats": values}`` -- because the scale is the reader's choice, not
    the wire's.
    """
    header = reader.serialized_int(1 << 7)
    width, scaled = header & 63, bool(header >> 6)
    if width:
        return {"packed": tuple(_signed(reader.bits(width), width) for _ in range(3)),
                "scaled": scaled}
    size, fmt = (64, "<d") if scaled else (32, "<f")
    parts = [struct.unpack(fmt, reader.bits(size).to_bytes(size // 8, "little"))[0]
             for _ in range(3)]
    if not all(math.isfinite(p) for p in parts):
        raise ValueError("non-finite packed vector component")
    return {"floats": tuple(parts)}


def _unit_scale(vector: dict) -> tuple[float, float, float]:
    """A velocity: whole units, where dividing by its scale of 1 is a no-op."""
    if "floats" in vector:
        return vector["floats"]
    return tuple(float(p) for p in vector["packed"])


def _vector_net_quantize100(reader: _Bits) -> tuple[float, float, float]:
    """``FVector_NetQuantize100``: a packed vector whose scaled integers are
    hundredths. Unlike a ``ReplicatedMovement`` location, the type names its
    scale, so it is applied here."""
    vector = _packed_vector(reader)
    if "floats" in vector:
        return vector["floats"]
    if vector["scaled"]:
        return tuple(p / 100 for p in vector["packed"])
    return tuple(float(p) for p in vector["packed"])


def _rotator(reader: _Bits, width: int) -> tuple[float, float, float]:
    """A rotator: one presence bit per component, then `width` bits if set."""
    return tuple(
        reader.bits(width) * 360.0 / (1 << width) if reader.bit() else 0.0
        for _ in range(3)
    )


def _rep_movement(reader: _Bits, rotation_bits: int) -> dict:
    """``FRepMovement::NetSerialize``, in wire order."""
    sleep = reader.bit()
    physics = reader.bit()
    frame = reader.bit()
    handle = reader.bit()
    location = _packed_vector(reader)
    scale = 360.0 / (1 << rotation_bits)
    rotation = []
    for _axis in ("pitch", "yaw", "roll"):
        present = reader.bit()
        rotation.append(_f32(reader.bits(rotation_bits) * scale) if present else 0.0)
    velocity = _unit_scale(_packed_vector(reader))
    angular = _unit_scale(_packed_vector(reader)) if physics else None
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
BIT_LEVEL_TYPES = {"EnumByte": "value_i64", "EnumRemainingBits": "value_i64",
                   "FName": "value_str", "FTextTree": "value_str",
                   "RotationShort": "value_str",
                   "VectorNetQuantize100": "value_str",
                   "RepMovementByte": "value_str", "RepMovementShort": "value_str"}


def _decode_bits(raw: bytes, bit_count: int, type_name: str):
    reader = _Bits(raw, bit_count)
    if type_name == "EnumByte":
        if not 1 <= bit_count <= 8:
            raise ValueError("EnumByte is not 1..8 bits")
        value = reader.bits(bit_count)
    elif type_name == "EnumRemainingBits":
        # Every remaining bit is the value, and a 0-bit field is the enum's
        # zero. Wider than 32 bits is more than the reader holds, so it is
        # refused rather than truncated.
        if bit_count > 32:
            raise ValueError("EnumRemainingBits wider than 32 bits")
        value = reader.bits(bit_count)
    elif type_name == "FName":
        value = _fname(reader)
    elif type_name == "FTextTree":
        value = _ftext_tree(reader)
    elif type_name == "RotationShort":
        value = _rotator(reader, 16)
    elif type_name == "VectorNetQuantize100":
        value = _vector_net_quantize100(reader)
    else:
        value = _rep_movement(reader, 8 if type_name == "RepMovementByte" else 16)
    if reader.remaining():
        raise ValueError(f"{type_name} leaves {reader.remaining()} residual bits")
    return value


def decode_exact(raw: bytes, bit_count: int, type_name: str):
    """Decode one value while requiring the declared payload width."""
    raw = raw or b""  # a zero-bit payload is exported as null
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
    if type_name == "UInt32":
        if bit_count != 32:
            raise ValueError("UInt32 is not 32 bits")
        return struct.unpack("<I", raw)[0]
    if type_name in {"Float", "Double"}:
        width, fmt = (32, "<f") if type_name == "Float" else (64, "<d")
        if bit_count != width:
            raise ValueError(f"{type_name} is not {width} bits")
        value = struct.unpack(fmt, raw)[0]
        if not math.isfinite(value):
            raise ValueError(f"{type_name} is non-finite")
        return value
    if type_name == "VectorDouble":
        if bit_count != 192:
            raise ValueError("VectorDouble is not 192 bits")
        value = struct.unpack("<3d", raw)
        if not all(math.isfinite(component) for component in value):
            raise ValueError("VectorDouble is non-finite")
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
    "Float": "value_f64", "Double": "value_f64", "VectorDouble": "value_str",
    "Byte": "value_i64", "Int32": "value_i64",
    "UInt32": "value_i64", "ObjectNetGuid": "value_i64",
    **BIT_LEVEL_TYPES,
}


#: Location scales a RepMovement comparison accepts, in the order tried.
LOCATION_SCALES = (100, 1)


def rep_movement_location_scale(decoded: dict, exported) -> str | None:
    """How `exported` renders `decoded`'s location, or None if it does not.

    Every other member must be equal exactly. The location may be the packed
    integers divided by any scale in LOCATION_SCALES -- the same one on all
    three components, since the tuple is compared whole. A float fallback or
    an unscaled packing has only one rendering.
    """
    if not isinstance(exported, dict) or exported.keys() != decoded.keys():
        return None
    if any(decoded[k] != exported[k] for k in decoded if k != "location"):
        return None
    location, got = decoded["location"], exported["location"]
    if "floats" in location:
        return "raw floats" if location["floats"] == got else None
    if not location["scaled"]:
        return "unscaled" if tuple(float(p) for p in location["packed"]) == got else None
    for scale in LOCATION_SCALES:
        if tuple(p / scale for p in location["packed"]) == got:
            return f"/{scale}"
    return None


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


def exported_matches(type_name: str, exported, decoded) -> bool:
    """Whether the exported column holds the independently decoded value.

    Scalars compare directly. Geometry is parsed out of ``value_str``:
    vectors are doubles and must be equal; rotator components are single
    precision in the model and are compared after rounding both sides to f32,
    because the shortest f32 spelling does not parse back to the exact value
    as a double. A ``ReplicatedMovement`` export may be passed as its
    ``value_str`` or as `exported_value` parsed it, and matches at any location
    scale in `LOCATION_SCALES`; `values_match` also says which one.
    """
    if type_name.startswith("RepMovement"):
        if exported is None or isinstance(exported, str):
            exported = exported_value({TYPED_COLUMNS[type_name]: exported}, type_name)
        return rep_movement_location_scale(decoded, exported) is not None
    if type_name in {"VectorNetQuantize100", "VectorDouble"}:
        return _parse_triple(exported, "(") == decoded
    if type_name == "FTextTree":
        if not isinstance(exported, str):
            return False
        try:
            return json.loads(exported) == decoded
        except ValueError:
            return False
    if type_name == "RotationShort":
        parsed = _parse_triple(exported, "rot(")
        return parsed is not None and all(
            _f32(a) == _f32(b) for a, b in zip(parsed, decoded))
    return exported == decoded


def _summary(type_name: str, value):
    """The numbers a range check can use: a scalar, or a vector's components.

    None for ``ReplicatedMovement``: its location is accepted at more than one
    scale (see `rep_movement_location_scale`), so no one range describes it.
    """
    if type_name.startswith("RepMovement"):
        return None
    if isinstance(value, tuple):
        return value
    if isinstance(value, (int, float)) and not isinstance(value, bool):
        return value
    return None


def values_match(type_name: str, decoded, exported) -> tuple[bool, str | None]:
    """`(matches, detail)`; detail is a RepMovement row's location scale."""
    if type_name.startswith("RepMovement"):
        scale = rep_movement_location_scale(decoded, exported)
        return scale is not None, scale
    return exported_matches(type_name, exported, decoded), None


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


def parquet_files(root: Path, export_ids=None, skipped=None):
    """Yield the field tables to read below `root`.

    Only the recursive search filters: a generated staging/backup directory
    is skipped (and added to the `skipped` set when one is given), and a
    table without `manifest.json` beside it raises `ValueError`.
    """
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
        for path in root.rglob(name):
            leftover = generated_ancestor(path, root)
            if leftover is not None:
                if skipped is not None:
                    skipped.add(leftover)
                continue
            if not (path.parent / "manifest.json").is_file():
                raise ValueError(
                    f"{path} has no manifest.json beside it; vrfkit writes the manifest last, "
                    "so this is not a finished export (an interrupted export or a partial copy)")
            yield path


def spec_rows(table: pa.Table, groups: pa.Array, fields: pa.Array) -> pa.Table:
    """The rows whose group is in `groups` AND whose field name is in `fields`.

    Still a superset of the specified pairs -- a group from one entry with a
    field from another passes -- so `validate` resolves the exact (group,
    field[, checksum]) key afterwards, unchanged. What this saves is
    `to_pylist` materialising every other field of a specified group, which
    the group-only filter did. Measured 2026-09-28 on the largest export of
    11.10, 12.04, 13.01, 13.04 and 13.06 (259ed10, --checkpoints), with both
    shipped specifications: rows reaching Python fell from 53,945-241,085 to
    14,360-26,665 per export -- on 13.06 exactly the 22,989 / 17,601 rows the
    specified pairs match -- and the summed median `validate` time from
    20.3 s to 11.1 s, with all 120 reports identical. `Table.filter` keeps
    row order, so the 32-capped example lists are the same rows as before.
    """
    return table.filter(pc.and_(
        pc.is_in(pc.cast(table["group_path"], pa.string()), value_set=groups),
        pc.is_in(pc.cast(table["field_name"], pa.string()), value_set=fields),
    ))


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
    location_scales = defaultdict(Counter)
    skipped = set()
    paths = list(parquet_files(export_root, export_ids, skipped))
    if not paths:
        raise ValueError(f"no field parquet files below {export_root}{leftover_note(skipped)}")
    wanted_groups = pa.array(sorted({group for group, _field, _checksum in expected}))
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
            table = spec_rows(pa.Table.from_batches([batch]), wanted_groups, wanted_fields)
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
                        matched, detail = values_match(type_name, value, exported)
                        if detail is not None:
                            location_scales[label][detail] += 1
                        if not matched:
                            typed_mismatch_count += 1
                            if len(typed_mismatch_examples) < 32:
                                typed_mismatch_examples.append({
                                    "file": str(path), "field": label,
                                    "decoded": value,
                                    "exported": row[TYPED_COLUMNS[type_name]],
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
                **({"location_scales": dict(sorted(location_scales[label].items()))}
                   if label in location_scales else {}),
            }
            for label, count in sorted(counts.items())
        },
        "missing": missing,
        "failure_count": failure_count,
        "failure_counts": dict(sorted(failure_counts.items())),
        "failure_examples": failures,
        "typed_mismatch_count": typed_mismatch_count,
        "typed_mismatch_examples": typed_mismatch_examples,
        "skipped_generated_dirs": skipped_report(skipped),
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
