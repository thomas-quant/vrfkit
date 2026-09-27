"""Export numeric AbilitiesAndBuffs FastArray updates with their original bits.

The measured route uses a support bit, four signed little-endian i32 header
words, deleted item IDs, and changed item IDs followed by packed handle/width
property streams WITHOUT a per-item checksum bit. This recovers boundaries;
it does not identify abilities, effects, player actions, or property meanings.
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

SCHEMA_VERSION = 1
GROUP = "AbilitiesAndBuffsComponent"
INNER_NAMES = ("_cnc_h1", "__vrfkit_chained_cnc_h1__")
#: Builds whose main-route windows were all walked exactly -- a measured list,
#: not a supported-builds list. A build missing here is rejected per row with
#: `unvalidated_build`, which keeps the exit nonzero, until someone measures it.
#:
#: 2026-09-09: 13.01, 13.02, 13.04 and 13.05 -- 2,882,152 windows in 714
#: exports; see docs/GAS_AND_PATCHVOLUME_INVESTIGATION.md.
#:
#: 2026-09-28: every export of the 1,018-replay common audit made by parser
#: 259ed10. Selection: both field tables, group AbilitiesAndBuffsComponent,
#: field _cnc_h1 or __vrfkit_chained_cnc_h1__, any handle. 3,999,493 of
#: 3,999,493 windows closed exactly under decode(). All were main rows with
#: handle 1. An independent reader agreed on every header word, ID and field
#: boundary. No checkpoint row carries this group, so the checkpoint route
#: stays unvalidated.
#:
#: 12.10 and 12.11 (one replay each) have no AbilitiesAndBuffsComponent rows
#: at all. Nothing was observed, and unobserved is not validated, so they stay
#: out. Their AbilitiesAndBuffs bodies exist only as __vrfkit_chained_cnc_h1__
#: rows, filed under /Script/ShooterGame.AresAbilitySystemComponent. GROUP
#: does not match that group on any build (see the investigation doc).
#:
#: 13.00 is thin: six windows in one replay. Only one of them tells this
#: variant apart from the one-flag-bit-per-item variant (ChecksumMode::Present
#: in crates/vrf-decode/src/fastarray.rs); every other build has at least
#: 6,302 such windows. Its three changed items do carry the one handle
#: sequence that all 3,403,315 changed items in the 22 builds share.
BUILDS = {f"++Ares-Core+release-{v}" for v in (
    "11.06", "11.07", "11.08", "11.09", "11.10", "11.11",
    "12.00", "12.01", "12.02", "12.03", "12.04", "12.05", "12.06", "12.07", "12.08", "12.09",
    "13.00", "13.01", "13.02", "13.04", "13.05", "13.06")}
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

    Returned field offsets are relative to the original inner window. The
    numeric handle is encoded_handle - 1. IDs and replication keys are signed
    i32 wire values, not actor GUIDs. No monotonicity or key arithmetic is
    required: a serialized delta can span updates missing from this stream.
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


def selected_rows(path: Path, checkpoint: bool):
    columns = [*COLUMNS, *(["checkpoint_index", "checkpoint_id"] if checkpoint else [])]
    ordinal = 0
    for batch in pq.ParquetFile(path).iter_batches(columns=columns, batch_size=65536, use_threads=False):
        groups = pc.cast(batch.column("group_path"), pa.string())
        names = pc.cast(batch.column("field_name"), pa.string())
        mask = pc.and_(pc.equal(groups, GROUP), pc.is_in(names, value_set=pa.array(INNER_NAMES)))
        positions = pc.indices_nonzero(pc.fill_null(mask, False))
        for index, row in zip(positions.to_pylist(), batch.take(positions).to_pylist()):
            yield ordinal + index, row
        ordinal += batch.num_rows


def observation(row: dict, ordinal: int, population: str, build: str) -> dict:
    result = {"population": population, "physical_row_ordinal": ordinal,
              "identity": {k: v for k, v in row.items() if k != "raw_bits"},
              "raw_bits_hex": row["raw_bits"].hex() if row["raw_bits"] is not None else None}
    try:
        if build not in BUILDS:
            raise WireError("unvalidated_build")
        if population != "fields":
            raise WireError("unvalidated_checkpoint_route")
        if row["group_path"] != GROUP or row["field_name"] not in INNER_NAMES or row["handle"] != 1:
            raise WireError("route_identity")
        result["structure"] = decode(row["raw_bits"], row["bit_count"])
        result["status"] = "structural_exact"
    except WireError as error:
        result["structure"] = None
        result["status"] = str(error)
    return result


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
    counts = Counter({k: 0 for k in ("rows", "structural_exact", "rejected", "deleted_items", "changed_items", "raw_fields", "fields_rows", "checkpoint_fields_rows")})
    reasons = Counter()
    try:
        output = stage / "observations.ndjson"
        with output.open("w", encoding="utf-8", newline="\n") as handle:
            for path in paths[1:]:
                for ordinal, row in selected_rows(path, path.stem == "checkpoint_fields"):
                    record = observation(row, ordinal, path.stem, build)
                    counts["rows"] += 1
                    counts[path.stem + "_rows"] += 1
                    if record["structure"] is not None:
                        structure = record["structure"]
                        counts["structural_exact"] += 1
                        counts["deleted_items"] += structure["num_deletes"]
                        counts["changed_items"] += structure["num_changed"]
                        counts["raw_fields"] += sum(len(item["fields"]) for item in structure["changed_items"])
                    else:
                        counts["rejected"] += 1
                        reasons[record["status"]] += 1
                    handle.write(json.dumps(record, separators=(",", ":")) + "\n")
        after = {str(path): sha(path) for path in paths}
        if before != after or sha(Path(__file__)) != script_hash:
            raise ValueError("input or extractor changed during read")
        receipt = {"schema_version": SCHEMA_VERSION, "replay_build": build,
                   "counts": dict(counts), "rejection_reasons": dict(reasons),
                   "input_sha256_before": before, "input_sha256_after": after,
                   "extractor_sha256": script_hash, "observations_sha256": sha(output),
                   "scope": "Numeric FastArray boundaries; no field names, gameplay meanings, casts, or player attribution."}
        (stage / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n", encoding="utf-8")
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
