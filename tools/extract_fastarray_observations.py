"""Export numeric AbilitiesAndBuffs FastArray updates with their original bits.

The measured grammar is a support bit, four signed little-endian i32 header
words, deleted item IDs, and changed item IDs followed by packed handle/width
property streams WITHOUT a per-item checksum bit. This recovers boundaries;
it does not identify abilities, effects, player actions, or property meanings.

The parser exports that window on two routes (see ROUTES). Every record names
its route and its stream (`population`: fields or checkpoint_fields).
"""
from __future__ import annotations

import argparse
from collections import Counter
import hashlib
import json
import os
from pathlib import Path
import shutil
import sys
import tempfile

import pyarrow as pa
import pyarrow.compute as pc
import pyarrow.parquet as pq

SCHEMA_VERSION = 2
#: Every route's rows carry handle 1. A selected row with another handle is
#: rejected as `route_identity`, so it is counted rather than skipped.
ROUTE_HANDLE = 1
#: route -> the exact (group_path, field_name) pair, not a cross product. Both
#: are the handle-1 payload of an AbilitiesAndBuffs ClassNetCache stream after
#: the same fc=34 outer walk (decode_cnc_payload), starting at the FastArray
#: support bit. In crates/vrfkit/src/sink/stream.rs:
#:   cnc_h1          emit_brute_forced_cnc_rpcs: whole unresolved CNC payloads,
#:                   group AbilitiesAndBuffsComponent.
#:   chained_cnc_h1  on_rep_layout_tail: a CNC tail after a RepLayout prefix,
#:                   only for the pre-remap identity, exactly one handle-1 RPC
#:                   and a set first bit (checked on a clone, so it stays in
#:                   raw_bits); group /Script/ShooterGame.AresAbilitySystemComponent.
ROUTES = {
    "cnc_h1": ("AbilitiesAndBuffsComponent", "_cnc_h1"),
    "chained_cnc_h1": ("/Script/ShooterGame.AresAbilitySystemComponent",
                       "__vrfkit_chained_cnc_h1__"),
}
ROUTE_BY_IDENTITY = {identity: route for route, identity in ROUTES.items()}
STREAMS = ("fields", "checkpoint_fields")


def _builds(*versions: str) -> frozenset[str]:
    return frozenset(f"++Ares-Core+release-{v}" for v in versions)


#: (route, stream) -> builds whose windows were all walked exactly: measured,
#: not supported. Another build rejects as `unvalidated_build`, an empty set as
#: `unvalidated_checkpoint_route`/`unvalidated_main_route`; each keeps the exit
#: nonzero until measured. 2026-09-28, parser 259ed10, 1,018 exports, each
#: route's pair scanned in both tables with any handle; decode() and an
#: independent reader agreed on every header word, ID and field boundary
#: (more, and the 2026-09-09 first run: docs/GAS_AND_PATCHVOLUME_INVESTIGATION.md):
#:   cnc_h1: 3,999,493 of 3,999,493 windows exact, all main, handle 1; none on
#:     12.10/12.11 or in checkpoints. 13.00 is thin: six windows in one replay,
#:     one telling this variant from the one-flag-bit-per-item one
#:     (ChecksumMode::Present, crates/vrf-decode/src/fastarray.rs), against at
#:     least 6,302 on every other build.
#:   chained_cnc_h1: 250,053 main and 181,108 checkpoint windows exact, handle
#:     1, both streams of all 24 builds, each with at least five the
#:     one-flag-bit variant rejects (12.10, 12.11, 13.00: 5 to 7 per stream).
#: The same fifteen handles on every changed item of both routes is an
#: alignment check, not a property schema.
_LEGACY = ("11.06", "11.07", "11.08", "11.09", "11.10", "11.11", "12.00", "12.01",
           "12.02", "12.03", "12.04", "12.05", "12.06", "12.07", "12.08", "12.09")
ACCEPTED_BUILDS = {
    ("cnc_h1", "fields"): _builds(
        *_LEGACY, "13.00", "13.01", "13.02", "13.04", "13.05", "13.06"),
    ("cnc_h1", "checkpoint_fields"): frozenset(),
    ("chained_cnc_h1", "fields"): _builds(
        *_LEGACY, "12.10", "12.11", "13.00", "13.01", "13.02", "13.04", "13.05", "13.06"),
    ("chained_cnc_h1", "checkpoint_fields"): _builds(
        *_LEGACY, "12.10", "12.11", "13.00", "13.01", "13.02", "13.04", "13.05", "13.06"),
}
#: Per-observation totals, reported overall and for every route and stream.
METRICS = ("rows", "structural_exact", "rejected", "deleted_items", "changed_items", "raw_fields")
COLUMNS = ["time_ms", "packet_id", "channel_index", "actor_net_guid",
           "object_net_guid", "group_path", "handle", "field_name",
           "compatible_checksum", "bit_count", "raw_bits"]


class WireError(ValueError):
    """A numeric structure cannot be established for this window."""


class Bits:
    def __init__(self, raw: bytes, bit_count: int):
        if not isinstance(raw, bytes) or type(bit_count) is not int or bit_count < 0 or len(raw) != (bit_count + 7) // 8:
            raise WireError("invalid_window")
        self.raw, self.end, self.pos = raw, bit_count, 0

    def read(self, width: int) -> int:
        if width < 0 or width > 32 or self.pos + width > self.end:
            raise WireError("truncated_scalar")
        start, shift = divmod(self.pos, 8)
        value = int.from_bytes(self.raw[start:(self.pos + width + 7) // 8], "little")
        self.pos += width
        return (value >> shift) & ((1 << width) - 1)

    def i32(self) -> int:
        value = self.read(32)
        return value if value < (1 << 31) else value - (1 << 32)

    def packed(self) -> int:
        value = 0
        for index in range(5):
            byte = self.read(8)
            if index == 4 and byte >> 1 > 15:
                raise WireError("packed_overflow")
            value |= (byte >> 1) << (7 * index)
            if not byte & 1:
                return value
        raise WireError("packed_unterminated")

    def skip(self, width: int) -> None:
        if self.pos + width > self.end:
            raise WireError("field_overrun")
        self.pos += width


def decode(raw: bytes, bit_count: int) -> dict:
    """Fully consume the measured no-checksum-item variant, or raise.

    IDs and replication keys are signed i32 wire values, not actor GUIDs, and
    need no monotonicity: a delta can span updates missing from this stream.
    """
    reader = Bits(raw, bit_count)
    if reader.read(1) != 1:
        raise WireError("unsupported_support_bit")
    array_key, base_key, deletes, changed = [reader.i32() for _ in range(4)]
    if min(deletes, changed) < 0:
        raise WireError("negative_count")
    # Every changed item needs an i32 ID and at least an 8-bit terminator.
    if deletes * 32 + changed * 40 > reader.end - reader.pos:
        raise WireError("count_bounds")
    deleted_ids = [reader.i32() for _ in range(deletes)]
    entries = []
    for _ in range(changed):
        item_id = reader.i32()
        fields = []
        while True:
            encoded = reader.packed()
            if encoded == 0:
                break
            width = reader.packed()
            fields.append({"handle": encoded - 1, "bit_offset": reader.pos,
                           "bit_count": width})
            reader.skip(width)
        entries.append({"item_id": item_id, "fields": fields})
    if reader.pos != reader.end:
        raise WireError("unconsumed_suffix")
    return {"supports_delta_struct_serialization": True,
            "array_replication_key": array_key, "base_replication_key": base_key,
            "num_deletes": deletes, "num_changed": changed,
            "deleted_item_ids": deleted_ids, "changed_items": entries,
            "consumed_bits": reader.pos}


def sha(path: Path) -> str:
    with path.open("rb") as handle:
        return hashlib.file_digest(handle, "sha256").hexdigest()


def route_mask(groups: pa.Array, names: pa.Array) -> pa.Array:
    """True where a row's (group_path, field_name) is exactly one route's pair."""
    pairs = [pc.fill_null(pc.and_(pc.equal(groups, group), pc.equal(names, name)), False)
             for group, name in ROUTES.values()]
    mask = pairs[0]
    for pair in pairs[1:]:
        mask = pc.or_(mask, pair)
    return mask


def selected_rows(path: Path, checkpoint: bool):
    columns = [*COLUMNS, *(["checkpoint_index", "checkpoint_id"] if checkpoint else [])]
    ordinal = 0
    for batch in pq.ParquetFile(path).iter_batches(columns=columns, batch_size=65536, use_threads=False):
        groups = pc.cast(batch.column("group_path"), pa.string())
        names = pc.cast(batch.column("field_name"), pa.string())
        positions = pc.indices_nonzero(route_mask(groups, names))
        for index, row in zip(positions.to_pylist(), batch.take(positions).to_pylist()):
            yield ordinal + index, row
        ordinal += batch.num_rows


def unselected_route_name_rows(path: Path) -> int:
    """Rows carrying a route's field name under a group no route pairs it with.

    Diagnostic only, neither selected nor decoded: a route once went
    unselected on every export because nothing counted this. On the
    2026-09-28 corpus the count is 0 in both streams.
    """
    route_names = pa.array(sorted({name for _, name in ROUTES.values()}))
    total = 0
    for batch in pq.ParquetFile(path).iter_batches(columns=["group_path", "field_name"],
                                                   batch_size=65536, use_threads=False):
        groups = pc.cast(batch.column("group_path"), pa.string())
        names = pc.cast(batch.column("field_name"), pa.string())
        named = pc.fill_null(pc.is_in(names, value_set=route_names), False)
        unselected = pc.and_(named, pc.invert(route_mask(groups, names)))
        total += pc.sum(pc.cast(unselected, pa.int64())).as_py() or 0
    return total


def observation(row: dict, ordinal: int, population: str, build: str) -> dict:
    route = ROUTE_BY_IDENTITY.get((row["group_path"], row["field_name"]))
    result = {"population": population, "route": route, "physical_row_ordinal": ordinal,
              "identity": {k: v for k, v in row.items() if k != "raw_bits"},
              "raw_bits_hex": row["raw_bits"].hex() if row["raw_bits"] is not None else None}
    try:
        if route is None or population not in STREAMS:
            raise WireError("route_identity")
        accepted = ACCEPTED_BUILDS[(route, population)]
        if not accepted:
            raise WireError("unvalidated_checkpoint_route" if population == "checkpoint_fields"
                            else "unvalidated_main_route")
        if build not in accepted:
            raise WireError("unvalidated_build")
        if row["handle"] != ROUTE_HANDLE:
            raise WireError("route_identity")
        result["structure"] = decode(row["raw_bits"], row["bit_count"])
        result["status"] = "structural_exact"
    except WireError as error:
        result["structure"] = None
        result["status"] = str(error)
    return result


def empty_counts() -> Counter:
    """Every count key, zero-valued, so an absent route prints as 0, not as nothing."""
    keys = [*METRICS, *(f"{stream}_rows" for stream in STREAMS)]
    keys += [f"{route}.{stream}.{metric}" for route in ROUTES for stream in STREAMS for metric in METRICS]
    keys += [f"unselected_route_name.{stream}.rows" for stream in STREAMS]
    return Counter({key: 0 for key in keys})


def tally(counts: Counter, prefix: str, record: dict) -> None:
    counts[prefix + "rows"] += 1
    structure = record["structure"]
    if structure is None:
        counts[prefix + "rejected"] += 1
        return
    counts[prefix + "structural_exact"] += 1
    counts[prefix + "deleted_items"] += structure["num_deletes"]
    counts[prefix + "changed_items"] += structure["num_changed"]
    counts[prefix + "raw_fields"] += sum(len(item["fields"]) for item in structure["changed_items"])


def extract(export_dir: Path, out_dir: Path) -> dict:
    export_dir, out_dir = export_dir.resolve(), out_dir.resolve()
    if out_dir == export_dir or out_dir.is_relative_to(export_dir):
        raise ValueError("output must be outside the source export")
    if out_dir.exists():
        raise ValueError("output directory already exists")
    paths = [export_dir / n for n in ("manifest.json", "fields.parquet", "checkpoint_fields.parquet")]
    before = {str(path): sha(path) for path in paths}
    build = json.loads(paths[0].read_text(encoding="utf-8"))["replay_build"]
    script_hash = sha(Path(__file__))
    out_dir.parent.mkdir(parents=True, exist_ok=True)
    stage = Path(tempfile.mkdtemp(prefix=".fastarray-", dir=out_dir.parent))
    counts = empty_counts()
    reasons = Counter()
    try:
        output = stage / "observations.ndjson"
        with output.open("w", encoding="utf-8", newline="\n") as handle:
            for path in paths[1:]:
                stream = path.stem
                counts[f"unselected_route_name.{stream}.rows"] += unselected_route_name_rows(path)
                for ordinal, row in selected_rows(path, stream == "checkpoint_fields"):
                    record = observation(row, ordinal, stream, build)
                    counts[stream + "_rows"] += 1
                    tally(counts, "", record)
                    tally(counts, f"{record['route']}.{stream}.", record)
                    if record["structure"] is None:
                        reasons[record["status"]] += 1
                    handle.write(json.dumps(record, separators=(",", ":")) + "\n")
        after = {str(path): sha(path) for path in paths}
        if before != after or sha(Path(__file__)) != script_hash:
            raise ValueError("input or extractor changed during read")
        receipt = {"schema_version": SCHEMA_VERSION, "replay_build": build,
                   "routes": {route: {"group_path": group, "field_name": name, "handle": ROUTE_HANDLE}
                              for route, (group, name) in ROUTES.items()},
                   "counts": dict(counts), "rejection_reasons": dict(reasons),
                   "input_sha256_before": before, "input_sha256_after": after,
                   "extractor_sha256": script_hash, "observations_sha256": sha(output),
                   "scope": "Numeric FastArray boundaries; no field names, gameplay meanings, casts, or player attribution."}
        (stage / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n", encoding="utf-8",
                                            newline="\n")
        os.rename(stage, out_dir)
        return receipt
    finally:
        if stage.exists():
            shutil.rmtree(stage)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--export-dir", type=Path, required=True)
    parser.add_argument("--out-dir", type=Path, required=True, help="New directory outside the export")
    args = parser.parse_args()
    try:
        receipt = extract(args.export_dir, args.out_dir)
    except (OSError, ValueError, KeyError, pa.ArrowException) as error:
        print(f"fastarray: {error}", file=sys.stderr)
        return 1
    print(json.dumps(receipt["counts"], sort_keys=True))
    return 1 if receipt["counts"]["rejected"] else 0


if __name__ == "__main__":
    raise SystemExit(main())
