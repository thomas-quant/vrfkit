"""Generate exact group/name/checksum field types from reviewed wire evidence.

Unlike donor checksum propagation, these entries never apply to another group
or another field name. The evidence fixture is explicit; this tool does not
infer types from widths or names. Run again after editing the fixture, and use
--check to detect stale generated Rust.

Every fixture type name maps to exactly one `FieldType` expression below. The
geometry shapes (`VectorNetQuantize100`, `RotationShort`, `RepMovementByte`,
`RepMovementShort`) and `EnumRemainingBits` exist for upstream descriptors whose
types are not primitives; each needs an independent decoder in
`validate_type_evidence.py` before an entry may use it. A `RepMovement` entry's
quantization is part of its type name because the checksum cannot carry it:
`ReplicatedMovement` has one checksum whether a class replicates byte or short
rotation components, which is why it is the dropped conflict in
`extract_checksum_types.py`. The exact group in the key is what separates them.

A `RepMovement` entry must also state its location level,
`location_quantization`: `RoundWholeNumber`, `RoundOneDecimal` or
`RoundTwoDecimals` (`VectorQuantization`). The level is a per-class choice the
wire does not carry, and exact consumption cannot catch a wrong one -- it
changes no width -- so it is measured, never defaulted here: join the actor's
first update to its `actors.parquet` spawn position (docs/DATA.md,
"`ReplicatedMovement.location` is world units, at a per-class level") and add
the group to `REP_MOVEMENT_LOCATION_EVIDENCE` in
crates/vrf-decode/src/tests/overlay.rs, which fails on a `RepMovement` type it
does not list. The key is refused on any other type.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path

from atomic_io import atomic_write_text

ROOT = Path(__file__).resolve().parents[1]
EVIDENCE = ROOT / "tools/fixtures/scoped_type_evidence.json"
OUTPUT = ROOT / "crates/vrf-decode/src/scoped_types.rs"

#: Fixture type name -> the rustfmt-stable lines of its `FieldType` expression.
#: The multi-line `RepMovement` form is the one rustfmt gives a struct literal
#: this long, so `cargo fmt --check` and `--check` here agree on one spelling.
TYPES = {
    "Byte": ("FieldType::Byte",),
    "Bool": ("FieldType::Bool",),
    "Int32": ("FieldType::Int32",),
    "UInt32": ("FieldType::UInt32",),
    "Float": ("FieldType::Float",),
    "Double": ("FieldType::Double",),
    "VectorDouble": ("FieldType::VectorDouble",),
    "FString": ("FieldType::FString",),
    "ObjectNetGuid": ("FieldType::ObjectNetGuid",),
    "EnumRemainingBits": ("FieldType::EnumRemainingBits",),
    "RotationShort": ("FieldType::RotationShort",),
    "VectorNetQuantize100": ("FieldType::VectorNetQuantize { scale: 100 }",),
    "RepMovementByte": (
        "FieldType::RepMovement {",
        "    rotation: RotatorQuantization::ByteComponents,",
        "}",
    ),
    "RepMovementShort": (
        "FieldType::RepMovement {",
        "    rotation: RotatorQuantization::ShortComponents,",
        "}",
    ),
}
#: Types whose expression names `RotatorQuantization` and `VectorQuantization`.
#: The import is emitted only when one is present: an unused `use` fails
#: clippy's `-D warnings`.
NEEDS_ROTATOR_QUANTIZATION = {"RepMovementByte", "RepMovementShort"}
#: The `VectorQuantization` variants a `RepMovement` entry's
#: `location_quantization` may name.
LOCATION_LEVELS = {"RoundWholeNumber", "RoundOneDecimal", "RoundTwoDecimals"}


def load(path: Path) -> list[dict]:
    document = json.loads(path.read_text(encoding="utf-8"))
    if document.get("schema_version") != 1:
        raise ValueError("expected scoped evidence schema_version 1")
    entries = document.get("entries")
    if not isinstance(entries, list) or not entries:
        raise ValueError("expected non-empty entries")
    seen = set()
    for entry in entries:
        for key in ("group", "field", "evidence"):
            if not isinstance(entry.get(key), str) or not entry[key].strip():
                raise ValueError(f"missing {key}")
        if not entry["group"].isascii() or not entry["field"].isascii():
            raise ValueError("Rust field identities must be ASCII")
        checksum = entry.get("checksum")
        if type(checksum) is not int or not 0 < checksum <= 0xFFFFFFFF:
            raise ValueError("checksum must be a nonzero u32")
        if entry.get("type") not in TYPES:
            raise ValueError(f"unsupported scoped type {entry.get('type')!r}")
        level = entry.get("location_quantization")
        if entry["type"] in NEEDS_ROTATOR_QUANTIZATION:
            if level not in LOCATION_LEVELS:
                raise ValueError(f"{entry['type']} needs a measured location_quantization, "
                                 f"one of {sorted(LOCATION_LEVELS)}")
        elif level is not None:
            raise ValueError("location_quantization applies only to RepMovement types")
        builds = entry.get("observed_builds")
        if not isinstance(builds, list) or not builds or any(not isinstance(b, str) or not b for b in builds):
            raise ValueError("observed_builds must record the replay evidence scope")
        key = (entry["field"], entry["group"], checksum)
        if key in seen:
            raise ValueError(f"duplicate scoped identity: {key}")
        seen.add(key)
    return sorted(entries, key=lambda e: (e["field"], e["group"], e["checksum"]))


def render(entries: list[dict]) -> str:
    imports = ["use crate::decode::FieldType;"]
    if any(entry["type"] in NEEDS_ROTATOR_QUANTIZATION for entry in entries):
        imports.append("use crate::types::{RotatorQuantization, VectorQuantization};")
    lines = [
        "//! Generated by `tools/generate_scoped_types.py`; do not edit by hand.",
        "//! Exact field identities whose names alone are ambiguous.",
        "//! Observed build scope and wire evidence live in the source fixture.",
        "", *imports, "",
        "/// Sorted by (field name, group path, compatible checksum).",
        f"pub(crate) static SCOPED_TYPES: [(&str, &str, u32, FieldType); {len(entries)}] = [",
    ]
    for entry in entries:
        expression = list(TYPES[entry["type"]])
        if entry["type"] in NEEDS_ROTATOR_QUANTIZATION:
            expression.insert(-1, "    location: VectorQuantization::"
                                  f"{entry['location_quantization']},")
        lines += ["    (", f"        {json.dumps(entry['field'])},",
                  f"        {json.dumps(entry['group'])},",
                  f"        {entry['checksum']},"]
        lines += [f"        {part}" for part in expression[:-1]]
        lines += [f"        {expression[-1]},", "    ),"]
    return "\n".join(lines + ["];", ""])


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--evidence", type=Path, default=EVIDENCE)
    parser.add_argument("--output", type=Path, default=OUTPUT)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args(argv)
    try:
        entries = load(args.evidence)
        text = render(entries)
        if args.check:
            if not args.output.is_file() or args.output.read_text(encoding="utf-8") != text:
                print("ERROR: scoped type table differs from reviewed evidence")
                return 1
        else:
            atomic_write_text(args.output, text)
        print(f"OK: {len(entries)} scoped field types")
        return 0
    except (ValueError, OSError, KeyError, TypeError) as exc:
        print(f"ERROR: {exc}")
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
