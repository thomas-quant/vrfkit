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
Supported types are Bool, Byte, Int32, Float, Double, FString and ObjectNetGuid.

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


def decode_exact(raw: bytes, bit_count: int, type_name: str):
    """Decode one primitive while requiring the declared payload width."""
    if len(raw) != (bit_count + 7) // 8:
        raise ValueError("raw byte length does not cover bit_count exactly")
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


def validate(export_root: Path, specifications: list[dict], export_ids=None, compare_typed=False) -> dict:
    if not export_root.exists():
        raise ValueError(f"export root does not exist: {export_root}")
    allowed = {"Bool", "Byte", "Int32", "Float", "Double", "FString", "ObjectNetGuid"}
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
    skipped = set()
    paths = list(parquet_files(export_root, export_ids, skipped))
    if not paths:
        raise ValueError(f"no field parquet files below {export_root}{leftover_note(skipped)}")
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
                        column = {
                            "Bool": "value_bool", "FString": "value_str",
                            "Float": "value_f64", "Double": "value_f64",
                            "Byte": "value_i64", "Int32": "value_i64",
                            "ObjectNetGuid": "value_i64",
                        }[type_name]
                        if row[column] != value:
                            typed_mismatch_count += 1
                            if len(typed_mismatch_examples) < 32:
                                typed_mismatch_examples.append({
                                    "file": str(path), "field": label,
                                    "decoded": value, "exported": row[column],
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
