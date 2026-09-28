"""Decode ground-area volume cells (GroundVolumeComponent FragmentInfo items)
with the item schema each replay declares.

A ground-area patch actor (the `Patch_*` classes: molotov, slow, net-toss and
barbed-wire patches, ...) owns a `/Script/DynamicVolume.GroundVolumeComponent`.
The component replicates its cells as the FastArray property `FragmentInfo`,
handle 0 of `GroundVolumeComponent_ClassNetCache`. The parser does not decode
custom-delta properties, so it preserves those windows raw on two routes (see
ROUTES). This tool reads them back with the component's own declared names and
checksums and writes one record per changed item: a cell of the volume with
its polygon in world coordinates (declared as ConvexHullPoints, though not
every measured polygon is convex), floor and ceiling, integer grid
coordinates and state, plus the owning actor's class from actors.parquet.

Wire grammar, measured on every window of the 2026-09-28 corpus (see
docs/GROUND_VOLUMES.md for the counts):

    window  := entry+                      -- until the window's last bit
    entry   := handle:SerializeInt(max(slots, 2)) width:packed body[width]
    body    := support:1(=1) array_key:i32 base_key:i32 deletes:i32 changed:i32
               deleted_id:i32 * deletes
               (item_id:i32 members) * changed
    members := (handle+1:packed width:packed payload[width])* 0:packed
    array   := count:packed (index+1:packed members)* 0:packed

`slots` is the ClassNetCache group's declared slot count. `handle` inside
`members` is a GroundVolumeComponent RepLayout command index. It is mapped to
the (name, compatible_checksum) the SAME replay declares for it, never to a
fixed number: the handles of these members move between builds while their
names and checksums do not. MEMBERS then gives the type for that identity.

Everything that does not close exactly is a counted rejection with the raw
window preserved. That includes shapes never observed -- an array element
sent out of order or missing a member, a partial array, a non-finite float --
because decoding them would be a guess.

Names: item `fields` keep the names the replay declares. The receipt adds the
game's own member path for the identities whose declared name is something
else (RESOLVED_NAMES: `253` is `ID`, `X`/`Y` are `GridPos.X`/`GridPos.Y`),
and each item carries `status_name`, the `Status` enumerator's name, only for
the one build and declaration those names were read from (STATUS_NAMES).

What this does not establish: which ability or player a cell belongs to
beyond the owning actor's class, what the `Status` states do in play (only
their 13.06 names are known), and anything about the component's RepLayout
prefix rows, which stay as exported.
"""
from __future__ import annotations

import argparse
from collections import Counter
from dataclasses import dataclass
import hashlib
import json
import math
import os
from pathlib import Path
import shutil
import struct
import sys
import tempfile

import pyarrow as pa
import pyarrow.compute as pc
import pyarrow.parquet as pq

#: 2: items gained `status_name`, the receipt `declarations.resolved_names`.
SCHEMA_VERSION = 2
CLASS_GROUP = "/Script/DynamicVolume.GroundVolumeComponent"
CNC_GROUP = CLASS_GROUP + "_ClassNetCache"
UNRESOLVED_CNC = "__vrfkit_unresolved_class_net_cache_payload__"
REP_LAYOUT_TAIL = "__vrfkit_unparsed_rep_layout_tail__"
#: field_name -> window kind. Both windows start at the first ClassNetCache
#: field header: the whole payload of a ClassNetCache-only block, or the rest
#: of a block after its RepLayout terminator.
WINDOW_KINDS = {UNRESOLVED_CNC: "unresolved_cnc_payload", REP_LAYOUT_TAIL: "rep_layout_tail"}
#: route -> the exact group_path the parser exports. `PatchVolume` is the
#: component's subobject name: the parser cannot resolve that subobject's
#: class, so it groups the rows by the object name. Every one of its windows
#: decodes exactly under its replay's GroundVolumeComponent declaration --
#: the evidence is in docs/GROUND_VOLUMES.md, and it is re-checked on every
#: window here, because a member whose declared identity is not in MEMBERS
#: rejects the window.
ROUTES = {"bare_patch_volume": "PatchVolume", "declared_class": CLASS_GROUP}
ROUTE_BY_GROUP = {group: route for route, group in ROUTES.items()}
STREAMS = ("fields", "checkpoint_fields")
#: The CNC field that carries the items, by its declared identity.
FRAGMENT_INFO = ("FragmentInfo", 2225407835)


def _builds(*versions: str) -> frozenset[str]:
    return frozenset(f"++Ares-Core+release-{v}" for v in versions)


#: (route, stream) -> builds whose windows on that route and stream were all
#: decoded exactly AND passed the independent checks in docs/GROUND_VOLUMES.md.
#: A measured list, not a supported-builds list: a row from any other build is
#: rejected as `unvalidated_build`, and a stream with no measured build at all
#: rejects as `unvalidated_checkpoint_route`. 12.10, 12.11 and 13.00 have no
#: windows in the corpus; no checkpoint row carries either route.
ACCEPTED_BUILDS = {
    ("bare_patch_volume", "fields"): _builds(
        "11.06", "11.07", "11.08", "11.09", "11.10", "11.11",
        "12.00", "12.01", "12.02", "12.03", "12.04", "12.05", "12.06", "12.07",
        "12.08", "12.09", "13.01", "13.02", "13.04", "13.05", "13.06"),
    ("bare_patch_volume", "checkpoint_fields"): frozenset(),
    ("declared_class", "fields"): _builds("12.09", "13.01", "13.02", "13.04", "13.05"),
    ("declared_class", "checkpoint_fields"): frozenset(),
}
COLUMNS = ["time_ms", "packet_id", "channel_index", "actor_net_guid",
           "object_net_guid", "group_path", "handle", "field_name",
           "compatible_checksum", "bit_count", "raw_bits"]
#: Arrays larger than this are rejected before any element is read. The
#: largest measured hull has 8 points and the largest segment list 4.
MAX_ARRAY_COUNT = 4096


@dataclass(frozen=True)
class Scalar:
    """A fixed-width member. `kind` selects the reading in `read_scalar`."""
    kind: str
    width: int


@dataclass(frozen=True)
class Array:
    """A dynamic array. `elements` maps each element member's declared
    identity to its type. An element with one member whose name is the
    array's own name is a plain value; otherwise it is a struct."""
    elements: dict


@dataclass(frozen=True)
class Untyped:
    """Declared and measured at one width, never typed: kept as raw bits."""
    width: int


#: Declared (name, compatible_checksum) -> type. The checksum is Unreal's
#: compatible checksum: CRC-32 chained over the lower-cased member name, its
#: C++ type and its static array index, each struct member and array element
#: seeded with its parent's checksum. Along the chain
#: FragmentInfo:FGroundVolumeFragmentArray -> Items:TArray ->
#: Items:FGroundVolumeFragment (-> GridPos:FIntPoint for X and Y) -- struct
#: names from the 13.06 game executable's reflection data, read statically
#: on 2026-09-28 -- every identity below
#: reproduces except `Status` (an enum: the formula reproduces no enum type)
#: and `Begin`/`End`. For each one that reproduces, the C++ type it encodes is
#: the type it is read as here (int32, bool, float, FVector as three doubles,
#: TArray), except TJunctions (below); tests/test_extract_ground_volumes.py
#: recomputes every one. `Status`, `Begin` and `End` rest on the measured
#: widths, exact consumption and the relations in docs/GROUND_VOLUMES.md.
#:
#: `253` is the replay's rendering of a hardcoded engine name index. Its
#: checksum reproduces as `ID : int32`, so it is read signed; the name is
#: reported through RESOLVED_NAMES, and `fields` keeps "253".
#:
#: TJunctions (11.10 to 12.02 only) was 16 zero bits on every item. Its
#: checksum reproduces as a TArray, and 16 zero bits are exactly an empty
#: array's count and terminator; but no element was ever sent, so the element
#: identity and type are unknown and it stays raw. The ConvexHullCeilings and
#: ConvexHullTravelDistances arrays of 11.10 to 12.05 were always empty; their
#: element identities below were observed from 12.06 on, and an element with
#: any other identity rejects.
MEMBERS = {
    ("253", 1175316786): Scalar("int32", 32),
    ("bIsActive", 518428974): Scalar("bool", 1),
    ("Status", 2380676387): Scalar("uint", 3),
    ("ExteriorSegments", 3326329067): Array({
        ("Begin", 3658211664): Scalar("uint", 8),
        ("End", 1988330146): Scalar("uint", 8)}),
    ("ConvexHullPoints", 3039966384): Array({
        ("ConvexHullPoints", 2749781999): Scalar("vector3d", 192)}),
    ("ConvexHullCeilings", 3975907906): Array({
        ("ConvexHullCeilings", 1547370894): Scalar("float32", 32)}),
    ("ConvexHullTravelDistances", 1031017464): Array({
        ("ConvexHullTravelDistances", 1566128181): Scalar("float32", 32)}),
    ("TJunctions", 1038854951): Untyped(16),
    ("X", 2123226522): Scalar("int32", 32),
    ("Y", 2134384775): Scalar("int32", 32),
    ("TravelDistance", 956522941): Scalar("float32", 32),
    ("Ceiling", 1959526051): Scalar("float32", 32),
    ("Floor", 3454040167): Scalar("float32", 32),
}
#: Identities that belong inside an array element, never directly in an item.
ELEMENT_IDENTITIES = frozenset(i for spec in MEMBERS.values() if isinstance(spec, Array)
                               for i in spec.elements)
#: Declared identity -> the member's path in the game's own struct, for the
#: item members whose declared name is not that path. Keyed by the whole
#: (name, compatible_checksum) pair, never by the name: each key reproduces
#: from its path's chain (FGroundVolumeFragment's `ID : int32`, and
#: `GridPos : FIntPoint`'s `X`/`Y : int32`; see MEMBERS), so a replay of any
#: build that declares the same pair declares the same member, and one whose
#: checksum differs is not relabelled. Reported per replay in the receipt;
#: `fields` keeps the declared names.
RESOLVED_NAMES = {
    ("253", 1175316786): "ID",
    ("X", 2123226522): "GridPos.X",
    ("Y", 2134384775): "GridPos.Y",
}
#: `Status` enumerator names: EGroundVolumeFragmentStatus in the 13.06 game
#: executable's reflection data, read statically on 2026-09-28 -- AllInside 0,
#: PartiallyOutside 1, PartiallyBlocked 2, Invalid 3, Count 4. A compatible
#: checksum does not encode an enum's values (and this enum member's checksum
#: does not reproduce at all), so the names are given only to the build they
#: were read from AND the identity it declares; every other build keeps the
#: integer with a null name, since nothing here shows its enumerators are the
#: same. `Count` is the enum's count sentinel, not a state: a 4, like a 5-7
#: from the 3-bit field, gets no name and is counted. Measured values are 0-3.
STATUS_NAMES_BUILD = "++Ares-Core+release-13.06"
STATUS_IDENTITY = ("Status", 2380676387)
STATUS_NAMES = {0: "AllInside", 1: "PartiallyOutside", 2: "PartiallyBlocked", 3: "Invalid"}
#: Every per-export counter, printed and written even when zero.
COUNTERS = (
    "rows", "rows_fields", "rows_checkpoint_fields",
    "rows_bare_patch_volume", "rows_declared_class",
    "rows_unresolved_cnc_payload", "rows_rep_layout_tail",
    "windows_exact", "rejected", "entries", "deleted_items", "changed_items",
    "items_complete", "items_partial", "array_elements", "hulls",
    "untyped_members", "untyped_nonzero_members",
    "status_named", "status_unnamed_declaration", "status_unnamed_value",
    "owner_class_resolved", "owner_class_missing", "owner_class_ambiguous",
    "object_outer_is_actor", "object_outer_not_actor", "object_guid_unresolved",
)


class WireError(ValueError):
    """This window cannot be decoded exactly with the declared schema."""


class Bits:
    """LSB-first reader over a window, bounded to [pos, end)."""

    __slots__ = ("raw", "pos", "end")

    def __init__(self, raw: bytes, bit_count: int):
        if (not isinstance(raw, bytes) or type(bit_count) is not int or bit_count < 0
                or len(raw) != (bit_count + 7) // 8):
            raise WireError("invalid_window")
        self.raw, self.pos, self.end = raw, 0, bit_count

    def remaining(self) -> int:
        return self.end - self.pos

    def take(self, width: int) -> "Bits":
        """A reader over exactly the next `width` bits, which this one skips."""
        if width < 0 or width > self.remaining():
            raise WireError("payload_overrun")
        sub = Bits.__new__(Bits)
        sub.raw, sub.pos, sub.end = self.raw, self.pos, self.pos + width
        self.pos += width
        return sub

    def read(self, width: int) -> int:
        if width < 0 or width > 64 or width > self.remaining():
            raise WireError("truncated")
        start, shift = divmod(self.pos, 8)
        value = int.from_bytes(self.raw[start:(self.pos + width + 7) // 8], "little")
        self.pos += width
        return (value >> shift) & ((1 << width) - 1)

    def i32(self) -> int:
        value = self.read(32)
        return value - (1 << 32) if value >= 1 << 31 else value

    def packed(self) -> int:
        """Unreal SerializeIntPacked: 7 value bits and a continue bit per byte."""
        value = 0
        for index in range(5):
            byte = self.read(8)
            if index == 4 and byte >> 1 > 15:
                raise WireError("packed_overflow")
            value |= (byte >> 1) << (7 * index)
            if not byte & 1:
                return value
        raise WireError("packed_unterminated")

    def serialize_int(self, value_max: int) -> int:
        """Unreal FBitReader::SerializeInt: bits LSB first while
        value + mask < value_max, so the width depends on the bits read."""
        if value_max < 2:
            raise WireError("serialize_int_max")
        value, mask = 0, 1
        while value + mask < value_max:
            if self.read(1):
                value |= mask
            mask <<= 1
        return value


@dataclass(frozen=True)
class ReplaySchema:
    """One replay's declarations for the component and its ClassNetCache."""
    members: dict        # handle -> (name, checksum), class group
    cnc: dict            # handle -> (name, checksum), ClassNetCache group
    cnc_slots: int | None
    error: str | None    # a window-independent reason every window rejects with

    def item_identities(self) -> frozenset:
        return frozenset(i for i in self.members.values() if i in MEMBERS)


def resolved_names(schema: ReplaySchema) -> dict:
    """Declared name -> game member path, for each RESOLVED_NAMES identity
    this replay declares with exactly that checksum."""
    return {identity[0]: RESOLVED_NAMES[identity]
            for _, identity in sorted(schema.members.items()) if identity in RESOLVED_NAMES}


def status_label(build: str, identity, value: int) -> tuple:
    """(enumerator name or None, the counter it is tallied under).

    A name only for the declaration the names were read from: that build and
    that (name, checksum). Decoding admits no other Status identity today (it
    must be in MEMBERS), so the identity test is reached only by a direct
    call; it is here so a Status identity added to MEMBERS later is not named
    by default.
    """
    if build != STATUS_NAMES_BUILD or identity != STATUS_IDENTITY:
        return None, "status_unnamed_declaration"
    name = STATUS_NAMES.get(value)
    return name, "status_named" if name is not None else "status_unnamed_value"


def read_scalar(bits: Bits, spec: Scalar):
    if spec.kind == "bool":
        return bool(bits.read(1))
    if spec.kind == "uint":
        return bits.read(spec.width)
    if spec.kind == "int32":
        return bits.i32()
    if spec.kind == "float32":
        value = struct.unpack("<f", bits.read(32).to_bytes(4, "little"))[0]
        if not math.isfinite(value):
            raise WireError("nonfinite_value")
        return value
    if spec.kind == "vector3d":
        value = [struct.unpack("<d", bits.read(64).to_bytes(8, "little"))[0] for _ in range(3)]
        if not all(math.isfinite(v) for v in value):
            raise WireError("nonfinite_value")
        return value
    raise WireError("unknown_kind")  # a MEMBERS entry with no reader


def member_stream(bits: Bits, schema: ReplaySchema, allowed: dict, counts: Counter) -> dict:
    """Read members until the 0 handle. Returns {identity: decoded value}."""
    out = {}
    while True:
        encoded = bits.packed()
        if encoded == 0:
            return out
        identity = schema.members.get(encoded - 1)
        if identity is None:
            raise WireError("undeclared_member_handle")
        if identity not in allowed:
            known = identity in MEMBERS or identity in ELEMENT_IDENTITIES
            raise WireError("misplaced_member" if known else "unrecognized_member")
        if identity in out:
            raise WireError("duplicate_member")
        width = bits.packed()
        payload = bits.take(width)
        spec = allowed[identity]
        if isinstance(spec, Array):
            out[identity] = read_array(payload, schema, identity, spec, counts)
        elif isinstance(spec, Untyped):
            if width != spec.width:
                raise WireError("member_width_mismatch")
            raw = payload.read(width)
            counts["untyped_members"] += 1
            counts["untyped_nonzero_members"] += raw != 0
            out[identity] = {"untyped_bits": width, "value_hex": format(raw, "x")}
        else:
            if width != spec.width:
                raise WireError("member_width_mismatch")
            out[identity] = read_scalar(payload, spec)
        if payload.remaining():
            raise WireError("unconsumed_member")


def read_array(bits: Bits, schema: ReplaySchema, identity, spec: Array, counts: Counter) -> list:
    count = bits.packed()
    if count > MAX_ARRAY_COUNT:
        raise WireError("array_count_bounds")
    elements = []
    while True:
        encoded = bits.packed()
        if encoded == 0:
            break
        index = encoded - 1
        if index >= count:
            raise WireError("array_index_bounds")
        # Every measured array is sent whole, in order: index i is element i.
        if index != len(elements):
            raise WireError("array_index_order")
        members = member_stream(bits, schema, spec.elements, counts)
        if set(members) != set(spec.elements):
            raise WireError("partial_element")
        if len(spec.elements) == 1 and next(iter(spec.elements))[0] == identity[0]:
            elements.append(next(iter(members.values())))
        else:
            elements.append({name: value for (name, _), value in members.items()})
    if len(elements) != count:
        raise WireError("partial_array")
    if bits.remaining():
        raise WireError("unconsumed_array")
    counts["array_elements"] += count
    return elements


def decode_window(raw: bytes, bit_count: int, schema: ReplaySchema) -> tuple[list, list, Counter]:
    """Fully consume one window or raise WireError.

    Returns (entries, items, counts). Entry offsets are relative to the
    window. Item `fields` are keyed by the replay's declared member names.
    """
    if schema.error:
        raise WireError(schema.error)
    bits = Bits(raw, bit_count)
    if bits.remaining() == 0:
        raise WireError("empty_window")
    counts = Counter()
    entries, items = [], []
    while bits.remaining():
        handle = bits.serialize_int(max(schema.cnc_slots, 2))
        field = schema.cnc.get(handle)
        if field is None:
            raise WireError("undeclared_cnc_handle")
        if field != FRAGMENT_INFO:
            raise WireError("unrecognized_cnc_field")
        width = bits.packed()
        offset = bits.pos
        body = bits.take(width)
        if body.read(1) != 1:
            raise WireError("unsupported_support_bit")
        array_key, base_key, deletes, changed = [body.i32() for _ in range(4)]
        if min(deletes, changed) < 0:
            raise WireError("negative_count")
        # A deleted ID is 32 bits; a changed item at least 32 + 8.
        if deletes * 32 + changed * 40 > body.remaining():
            raise WireError("count_bounds")
        deleted = [body.i32() for _ in range(deletes)]
        entry_index = len(entries)
        for item_index in range(changed):
            item_id = body.i32()
            members = member_stream(body, schema, MEMBERS, counts)
            items.append({"entry_index": entry_index, "item_index": item_index,
                          "array_replication_key": array_key,
                          "base_replication_key": base_key, "item_id": item_id,
                          "identities": list(members),
                          "fields": {name: value for (name, _), value in members.items()}})
        if body.remaining():
            raise WireError("unconsumed_entry")
        entries.append({"cnc_handle": handle, "cnc_field": field[0], "bit_offset": offset,
                        "bit_count": width, "array_replication_key": array_key,
                        "base_replication_key": base_key, "deleted_item_ids": deleted,
                        "changed_items": changed})
    return entries, items, counts


def load_schema(export_dir: Path, manifest: dict) -> ReplaySchema:
    """Union of the main-stream and every checkpoint declaration of both groups.

    The main stream's declarations come from the manifest, which records each
    exported handle but not the group's slot count; the slot count, which sets
    the ClassNetCache handle width, is read from the checkpoint declarations.
    A handle declared with two identities, checkpoints that disagree on the
    slot count, or no declared slot count at all make every window of the
    export reject: the schema is not established, and picking one would be a
    guess.
    """
    declared = {CLASS_GROUP: {}, CNC_GROUP: {}}
    conflicts = []

    def add(group, handle, identity):
        seen = declared[group].setdefault(int(handle), identity)
        if seen != identity:
            conflicts.append((group, handle))

    for group in manifest.get("net_field_export_groups", []):
        if group.get("path") in declared:
            for field in group["fields"]:
                add(group["path"], field["handle"], (field["name"], int(field["compatible_checksum"])))
    wanted, slots = {}, set()
    groups = pq.read_table(export_dir / "checkpoint_export_groups.parquet",
                           columns=["checkpoint_index", "path_name_index", "group_path", "declared_slots"])
    for row in groups.to_pylist():
        if row["group_path"] in declared:
            wanted[(row["checkpoint_index"], row["path_name_index"])] = row["group_path"]
            if row["group_path"] == CNC_GROUP:
                slots.add(row["declared_slots"])
    if wanted:
        fields = pq.read_table(export_dir / "checkpoint_export_fields.parquet",
                               columns=["checkpoint_index", "path_name_index", "handle",
                                        "compatible_checksum", "rendered_name"])
        indexes = sorted({key[1] for key in wanted})
        fields = fields.filter(pc.is_in(fields["path_name_index"], value_set=pa.array(
            indexes, type=fields.schema.field("path_name_index").type)))
        for row in fields.to_pylist():
            group = wanted.get((row["checkpoint_index"], row["path_name_index"]))
            if group is not None:
                add(group, row["handle"], (row["rendered_name"], int(row["compatible_checksum"])))
    error = None
    if conflicts:
        error = "declaration_conflict"
    elif len(slots) > 1:
        error = "cnc_slots_conflict"
    elif not slots:
        error = "cnc_slots_undeclared"
    return ReplaySchema(declared[CLASS_GROUP], declared[CNC_GROUP],
                        next(iter(slots)) if len(slots) == 1 else None, error)


def sha(path: Path) -> str:
    with path.open("rb") as handle:
        return hashlib.file_digest(handle, "sha256").hexdigest()


def selected_rows(path: Path, checkpoint: bool):
    """(physical row ordinal, row) for every row on a route, any handle."""
    columns = [*COLUMNS, *(["checkpoint_index", "checkpoint_id"] if checkpoint else [])]
    ordinal = 0
    for batch in pq.ParquetFile(path).iter_batches(columns=columns, batch_size=65536, use_threads=False):
        groups = pc.cast(batch.column("group_path"), pa.string())
        names = pc.cast(batch.column("field_name"), pa.string())
        mask = pc.and_(pc.is_in(groups, value_set=pa.array(list(ROUTE_BY_GROUP))),
                       pc.is_in(names, value_set=pa.array(list(WINDOW_KINDS))))
        positions = pc.indices_nonzero(pc.fill_null(mask, False))
        for index, row in zip(positions.to_pylist(), batch.take(positions).to_pylist()):
            yield ordinal + index, row
        ordinal += batch.num_rows


def owner_classes(export_dir: Path) -> dict:
    """actor GUID -> set of class paths on its `open` events."""
    owners = {}
    table = pq.read_table(export_dir / "actors.parquet", columns=["actor_net_guid", "event", "class_path"])
    for row in table.to_pylist():
        if row["event"] == "open":
            owners.setdefault(row["actor_net_guid"], set()).add(row["class_path"])
    return owners


def object_paths(export_dir: Path) -> dict:
    table = pq.read_table(export_dir / "net_guids.parquet", columns=["net_guid", "path", "outer_net_guid"])
    return {row["net_guid"]: (row["path"], row["outer_net_guid"]) for row in table.to_pylist()}


def window_record(row: dict, ordinal: int, population: str, build: str, schema: ReplaySchema):
    """(window record, decoded items, per-window counts)."""
    route = ROUTE_BY_GROUP[row["group_path"]]
    record = {"population": population, "physical_row_ordinal": ordinal, "route": route,
              "window_kind": WINDOW_KINDS[row["field_name"]],
              "identity": {k: v for k, v in row.items() if k != "raw_bits"},
              "raw_bits_hex": row["raw_bits"].hex() if row["raw_bits"] is not None else None}
    items, counts = [], Counter()
    try:
        accepted = ACCEPTED_BUILDS[(route, population)]
        if not accepted:
            raise WireError("unvalidated_checkpoint_route")
        if build not in accepted:
            raise WireError("unvalidated_build")
        # REP_LAYOUT_TAIL rows carry handle 0 and UNRESOLVED_CNC rows u32::MAX.
        if row["handle"] != (0 if row["field_name"] == REP_LAYOUT_TAIL else 0xFFFFFFFF):
            raise WireError("route_identity")
        entries, items, counts = decode_window(row["raw_bits"], row["bit_count"], schema)
        record["status"] = "decoded_exact"
        record["entries"] = entries
    except WireError as error:
        record["status"] = str(error)
        record["entries"] = None
        items, counts = [], Counter()
    return record, items, counts


def item_record(window: dict, item: dict, owners: dict, objects: dict,
                declared_items: frozenset, counts: Counter, build: str) -> dict:
    identity = window["identity"]
    classes = owners.get(identity["actor_net_guid"], set())
    if len(classes) == 1 and None not in classes:
        owner, owner_status = next(iter(classes)), "resolved"
    else:
        owner, owner_status = None, ("missing" if not classes or classes == {None} else "ambiguous")
    counts[f"owner_class_{owner_status}"] += 1
    obj = objects.get(identity["object_net_guid"])
    if obj is None:
        counts["object_guid_unresolved"] += 1
    else:
        # The component should be a subobject of the actor whose channel
        # carries it; a different outer is surfaced, not repaired.
        counts["object_outer_is_actor" if obj[1] == identity["actor_net_guid"]
               else "object_outer_not_actor"] += 1
    fields = item["fields"]
    complete = set(item["identities"]) == declared_items
    counts["items_complete" if complete else "items_partial"] += 1
    status_name = None
    for member in item["identities"]:
        if member[0] == "Status":
            status_name, counter = status_label(build, member, fields["Status"])
            counts[counter] += 1
    hull = None
    if "ConvexHullPoints" in fields:
        counts["hulls"] += 1
        hull = {"points_xy": [[p[0], p[1]] for p in fields["ConvexHullPoints"]],
                "floor": fields.get("Floor"), "ceiling": fields.get("Ceiling")}
    return {"population": window["population"],
            "physical_row_ordinal": window["physical_row_ordinal"],
            "route": window["route"], "window_kind": window["window_kind"],
            "entry_index": item["entry_index"], "item_index": item["item_index"],
            **{k: identity[k] for k in ("time_ms", "packet_id", "channel_index",
                                        "actor_net_guid", "object_net_guid")},
            "object_path": obj[0] if obj else None,
            "object_outer_net_guid": obj[1] if obj else None,
            "owner_class_path": owner, "owner_class_status": owner_status,
            "array_replication_key": item["array_replication_key"],
            "base_replication_key": item["base_replication_key"],
            "item_id": item["item_id"], "complete": complete,
            "fields": fields, "status_name": status_name, "hull": hull}


def extract(export_dir: Path, out_dir: Path) -> dict:
    export_dir, out_dir = export_dir.resolve(), out_dir.resolve()
    if out_dir == export_dir or out_dir.is_relative_to(export_dir):
        raise ValueError("output must be outside the source export")
    if out_dir.exists():
        raise ValueError("output directory already exists")
    inputs = [export_dir / n for n in (
        "manifest.json", "fields.parquet", "checkpoint_fields.parquet", "actors.parquet",
        "net_guids.parquet", "checkpoint_export_groups.parquet", "checkpoint_export_fields.parquet")]
    before = {path.name: sha(path) for path in inputs}
    script_hash = sha(Path(__file__))
    manifest = json.loads(inputs[0].read_text(encoding="utf-8"))
    build = manifest["replay_build"]
    schema = load_schema(export_dir, manifest)
    owners, objects = owner_classes(export_dir), object_paths(export_dir)
    declared_items = schema.item_identities()
    out_dir.parent.mkdir(parents=True, exist_ok=True)
    stage = Path(tempfile.mkdtemp(prefix=".ground-volumes-", dir=out_dir.parent))
    counts = Counter({k: 0 for k in COUNTERS})
    reasons = Counter()
    try:
        with (stage / "windows.ndjson").open("w", encoding="utf-8", newline="\n") as windows, \
                (stage / "items.ndjson").open("w", encoding="utf-8", newline="\n") as items_out:
            for stream in STREAMS:
                for ordinal, row in selected_rows(export_dir / f"{stream}.parquet", stream != "fields"):
                    record, items, window_counts = window_record(row, ordinal, stream, build, schema)
                    counts["rows"] += 1
                    counts[f"rows_{stream}"] += 1
                    counts[f"rows_{record['route']}"] += 1
                    counts[f"rows_{record['window_kind']}"] += 1
                    if record["status"] == "decoded_exact":
                        counts["windows_exact"] += 1
                        counts.update(window_counts)
                        counts["entries"] += len(record["entries"])
                        counts["deleted_items"] += sum(len(e["deleted_item_ids"]) for e in record["entries"])
                        counts["changed_items"] += len(items)
                        for item in items:
                            out = item_record(record, item, owners, objects, declared_items, counts, build)
                            items_out.write(json.dumps(out, separators=(",", ":"), allow_nan=False) + "\n")
                    else:
                        counts["rejected"] += 1
                        reasons[record["status"]] += 1
                    windows.write(json.dumps(record, separators=(",", ":"), allow_nan=False) + "\n")
        after = {path.name: sha(path) for path in inputs}
        if before != after or sha(Path(__file__)) != script_hash:
            raise ValueError("input or extractor changed during read")
        receipt = {
            "schema_version": SCHEMA_VERSION, "replay_build": build,
            "counts": dict(counts), "rejection_reasons": dict(reasons),
            "declarations": {
                "class_group": {str(h): list(i) for h, i in sorted(schema.members.items())},
                "cnc_group": {str(h): list(i) for h, i in sorted(schema.cnc.items())},
                "cnc_declared_slots": schema.cnc_slots, "schema_error": schema.error,
                "resolved_names": resolved_names(schema)},
            "input_sha256_before": before, "input_sha256_after": after,
            "extractor_sha256": script_hash,
            "windows_sha256": sha(stage / "windows.ndjson"),
            "items_sha256": sha(stage / "items.ndjson"),
            "scope": ("GroundVolumeComponent FragmentInfo cells decoded with the replay's own "
                      "declared names; resolved_names by exact (name, checksum); Status "
                      "enumerator names for the 13.06 declaration only; no ability or player "
                      "semantics."),
        }
        (stage / "receipt.json").write_text(json.dumps(receipt, indent=2, sort_keys=True) + "\n",
                                            encoding="utf-8")
        os.rename(stage, out_dir)
        return receipt
    finally:
        if stage.exists():
            shutil.rmtree(stage)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0],
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--export-dir", type=Path, required=True)
    parser.add_argument("--out-dir", type=Path, required=True, help="New directory outside the export")
    args = parser.parse_args()
    try:
        receipt = extract(args.export_dir, args.out_dir)
    except (OSError, ValueError, KeyError, pa.ArrowException) as error:
        print(f"ground-volumes: {error}", file=sys.stderr)
        return 1
    print(json.dumps(receipt["counts"], sort_keys=True))
    if receipt["rejection_reasons"]:
        print(json.dumps(receipt["rejection_reasons"], sort_keys=True), file=sys.stderr)
    return 1 if receipt["counts"]["rejected"] else 0


if __name__ == "__main__":
    raise SystemExit(main())
