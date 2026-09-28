"""Extract versioned KillData serialized-update snapshots from one vrfkit export.

The output is an observation stream, not a deduplicated kill ledger. Missing
members remain null and all three serialized clocks remain independent.
"""

from __future__ import annotations
import argparse, hashlib, json, math, os, re, struct, sys
from pathlib import Path
import pyarrow as pa
import pyarrow.compute as pc
import pyarrow.parquet as pq

if __package__:
    from .atomic_io import atomic_write_text
else:
    from atomic_io import atomic_write_text

SCHEMA_VERSION = 1
GROUP = "/Script/ShooterGame.PlayerMatchStatsComponent"
PARENT = ("KillData", 1493759848)
#: Builds whose KillData identities and values were measured. This is a
#: measured list, not the parser's supported builds: any other build fails in
#: declarations() before a row is read, even when its names and checksums
#: agree, until it has been measured the same way.
#:
#: 2026-09-08: 13.01, 13.02, 13.04 and 13.05, the 714-export corpus in
#: docs/KILL_LEDGER.md.
#:
#: 2026-09-28: 13.06. All 38 13.06 exports that parser 259ed10 wrote with
#: --checkpoints for the 1,018-replay common audit exited 0 here. On each one
#: the main and checkpoint declarations equal PARENT and DECL, every child
#: matches its parent raw window and every typed value its raw decode. That is
#: 5,371 main and 6,434 checkpoint parent rows (5,381 and 53,563 element
#: updates); a separate Parquet recount agrees for every export and table.
MEASURED_BUILDS = {
    # 11.06-12.09: every available export passed on 2026-09-28; see
    # docs/KILL_OBSERVATIONS.md. 12.10, 12.11 and 13.00 export no KillData children.
    "++Ares-Core+release-11.06",
    "++Ares-Core+release-11.07",
    "++Ares-Core+release-11.08",
    "++Ares-Core+release-11.09",
    "++Ares-Core+release-11.10",
    "++Ares-Core+release-11.11",
    "++Ares-Core+release-12.00",
    "++Ares-Core+release-12.01",
    "++Ares-Core+release-12.02",
    "++Ares-Core+release-12.03",
    "++Ares-Core+release-12.04",
    "++Ares-Core+release-12.05",
    "++Ares-Core+release-12.06",
    "++Ares-Core+release-12.07",
    "++Ares-Core+release-12.08",
    "++Ares-Core+release-12.09",
    "++Ares-Core+release-13.01",
    "++Ares-Core+release-13.02",
    "++Ares-Core+release-13.04",
    "++Ares-Core+release-13.05",
    "++Ares-Core+release-13.06",
}
DECL = {
    3: ("Victim", 3990035472),
    4: ("KillingEquippableClass", 2071131011),
    5: ("WeaponTheme", 1839952321),
    6: ("AssistingPlayers", 1689463717),
    7: ("AssistingPlayers", 1417448159),
    9: ("DamageType", 2992423760),
    10: ("DamageTaken", 2001471495),
    11: ("DamageRegion", 3229265809),
    12: ("GameTimeElapsed", 3684431363),
    13: ("RoundTimestamp", 2328473242),
    14: ("RoundNumber", 843024485),
    15: ("bDidKillTriggerFinisher", 2795684046),
}
MEMBERS = {
    3: "victim_ref",
    4: "killing_equippable_class_ref",
    5: "weapon_theme",
    9: "damage_type_ref",
    10: "damage_taken",
    11: "damage_region",
    12: "game_time_elapsed",
    13: "round_timestamp",
    14: "round_number",
    15: "did_kill_trigger_finisher",
}
RX = re.compile(r"^KillData(?:\[[0-9]+\](?:\..*)?)?$")
MAX_ELEMENTS = 4096
MAX_FIELDS = 128


class InputError(ValueError):
    pass


def sha(path):
    h = hashlib.sha256()
    with path.open("rb") as f:
        for x in iter(lambda: f.read(1 << 20), b""):
            h.update(x)
    return h.hexdigest()


def packed(raw, pos, end):
    value = 0
    for i in range(5):
        if pos + 8 > end:
            raise InputError("truncated IntPacked")
        byte = sum(
            ((raw[(pos + j) // 8] >> ((pos + j) % 8)) & 1) << j for j in range(8)
        )
        pos += 8
        payload = byte >> 1
        if i == 4 and payload > 15:
            raise InputError("IntPacked exceeds u32")
        value |= payload << (7 * i)
        if not byte & 1:
            return value, pos
    raise InputError("unterminated IntPacked")


def slice_bits(raw, pos, width, end):
    if width <= 0 or pos + width > end:
        raise InputError("invalid leaf width")
    out = bytearray((width + 7) // 8)
    for i in range(width):
        out[i // 8] |= ((raw[(pos + i) // 8] >> ((pos + i) % 8)) & 1) << (i % 8)
    return bytes(out), pos + width


def parse_array(raw, bit_count, allowed=None):
    if (
        raw is None
        or type(bit_count) is not int
        or bit_count <= 0
        or len(raw) != (bit_count + 7) // 8
    ):
        raise InputError("invalid raw array window")
    pos = 0
    capacity, pos = packed(raw, pos, bit_count)
    if capacity > MAX_ELEMENTS:
        raise InputError("array capacity exceeds limit")
    leaves = []
    elements = 0
    seen_indices = set()
    while True:
        encoded, pos = packed(raw, pos, bit_count)
        if encoded == 0:
            if pos != bit_count:
                raise InputError("array has residual root bits")
            return capacity, elements, leaves
        index = encoded - 1
        if index >= capacity or elements >= MAX_ELEMENTS or index in seen_indices:
            raise InputError("array index is duplicate or exceeds bound")
        seen_indices.add(index)
        elements += 1
        fields = 0
        seen_handles = set()
        while True:
            encoded_handle, pos = packed(raw, pos, bit_count)
            if encoded_handle == 0:
                break
            if fields >= MAX_FIELDS:
                raise InputError("array field count exceeds limit")
            handle = encoded_handle - 1
            if handle in seen_handles:
                raise InputError(f"duplicate array handle {handle}")
            seen_handles.add(handle)
            if allowed is not None and handle not in allowed:
                raise InputError(f"unexpected nested handle {handle}")
            width, pos = packed(raw, pos, bit_count)
            payload, pos = slice_bits(raw, pos, width, bit_count)
            leaves.append((index, handle, width, payload))
            fields += 1


def exact_ref(raw, width):
    value, pos = packed(raw, 0, width)
    if pos != width:
        raise InputError("ObjectNetGuid did not consume its leaf")
    return value


def unsigned_bits(raw, pos, width):
    return sum(
        ((raw[(pos + i) // 8] >> ((pos + i) % 8)) & 1) << i for i in range(width)
    )


def weapon_theme(raw, width):
    if width < 33 or unsigned_bits(raw, 0, 1) != 1:
        raise InputError("WeaponTheme framing differs")
    encoded = unsigned_bits(raw, 1, 32)
    length = encoded - (1 << 32) if encoded & (1 << 31) else encoded
    units = abs(length)
    unit_bits = 16 if length < 0 else 8
    if units * (unit_bits // 8) > 64 * 1024 or 33 + units * unit_bits != width:
        raise InputError("WeaponTheme length/framing differs")
    if units == 0:
        return ""
    payload, _ = slice_bits(raw, 33, units * unit_bits, width)
    if length < 0:
        if payload[-2:] != b"\0\0":
            raise InputError("WeaponTheme lacks UTF-16 terminator")
        return payload[:-2].decode("utf-16-le")
    if payload[-1:] != b"\0":
        raise InputError("WeaponTheme lacks byte terminator")
    return payload[:-1].decode("utf-8")


def declarations(export):
    manifest = json.loads((export / "manifest.json").read_text(encoding="utf-8"))
    out = {}
    if manifest.get("replay_build") not in MEASURED_BUILDS:
        raise InputError(
            f"replay build is outside the measured KillData set: {manifest.get('replay_build')!r}"
        )
    groups = [
        g for g in manifest.get("net_field_export_groups", []) if g.get("path") == GROUP
    ]
    if len(groups) != 1:
        raise InputError("expected exactly one main KillData declaration group")
    main = {}
    for f in groups[0].get("fields", []):
        if f["handle"] in main:
            raise InputError("duplicate main declaration handle")
        main[f["handle"]] = (f.get("name"), f.get("compatible_checksum"))
    if PARENT not in main.values() or any(
        main.get(h) != identity for h, identity in DECL.items()
    ):
        raise InputError("main KillData declarations differ from measured identities")
    out[None] = main
    groups = pq.read_table(
        export / "checkpoint_export_groups.parquet",
        columns=["checkpoint_index", "ordinal", "group_path"],
        filters=[("group_path", "=", GROUP)],
        use_threads=False,
    ).to_pylist()
    owners = {}
    for g in groups:
        key = (g["checkpoint_index"], g["ordinal"])
        if key in owners or (g["checkpoint_index"] in out):
            raise InputError("duplicate checkpoint declaration group")
        owners[key] = g["checkpoint_index"]
        out.setdefault(g["checkpoint_index"], {})
    for f in pq.read_table(
        export / "checkpoint_export_fields.parquet",
        columns=[
            "checkpoint_index",
            "group_ordinal",
            "handle",
            "rendered_name",
            "compatible_checksum",
        ],
        use_threads=False,
    ).to_pylist():
        key = (f["checkpoint_index"], f["group_ordinal"])
        if key not in owners:
            continue
        scope = owners[key]
        target = out[scope]
        if f["handle"] in target:
            raise InputError("duplicate checkpoint declaration handle")
        target[f["handle"]] = (f["rendered_name"], f["compatible_checksum"])
    return manifest, out


def scoped_refs(export, table):
    if table == "fields":
        actors = set(
            pq.read_table(
                export / "actors.parquet", columns=["actor_net_guid"], use_threads=False
            )["actor_net_guid"].to_pylist()
        )
        guids = set(
            pq.read_table(
                export / "net_guids.parquet", columns=["net_guid"], use_threads=False
            )["net_guid"].to_pylist()
        )
        return {None: (actors, guids)}
    from collections import defaultdict

    out = defaultdict(lambda: (set(), set()))
    for r in pq.read_table(
        export / "checkpoint_actors.parquet",
        columns=["checkpoint_index", "actor_net_guid"],
        use_threads=False,
    ).to_pylist():
        out[r["checkpoint_index"]][0].add(r["actor_net_guid"])
    for r in pq.read_table(
        export / "checkpoint_net_guids.parquet",
        columns=["checkpoint_index", "net_guid"],
        use_threads=False,
    ).to_pylist():
        out[r["checkpoint_index"]][1].add(r["net_guid"])
    return out


def selected(path):
    offset = 0
    for batch in pq.ParquetFile(path).iter_batches(batch_size=65536, use_threads=False):
        mask = pc.and_(
            pc.equal(pc.cast(batch["group_path"], pa.string()), GROUP),
            pc.match_substring_regex(
                pc.cast(batch["field_name"], pa.string()),
                r"^KillData(?:\[[0-9]+\](?:\..*)?)?$",
            ),
        )
        indices = pc.indices_nonzero(pc.fill_null(mask, False)).to_pylist()
        if indices:
            for i, row in zip(
                indices, batch.take(pa.array(indices, type=pa.int64())).to_pylist()
            ):
                yield offset + i, row
        offset += batch.num_rows


def value(row, handle):
    return (
        row["value_i64"]
        if handle in (3, 4, 9, 11, 14)
        else (
            row["value_f64"]
            if handle in (10, 12, 13)
            else (
                row["value_bool"]
                if handle == 15
                else row["value_str"] if handle == 5 else None
            )
        )
    )


def validate_direct(row, handle, raw, width):
    expected_column = (
        "value_i64"
        if handle in (3, 4, 9, 11, 14)
        else (
            "value_f64"
            if handle in (10, 12, 13)
            else "value_bool" if handle == 15 else "value_str" if handle == 5 else None
        )
    )
    populated = {
        name
        for name in ("value_i64", "value_f64", "value_bool", "value_str")
        if row[name] is not None
    }
    if populated != ({expected_column} if expected_column else set()):
        raise InputError(f"KillData handle {handle} populated wrong typed columns")
    actual_value = value(row, handle)
    expected_type = {
        "value_i64": int,
        "value_f64": float,
        "value_bool": bool,
        "value_str": str,
    }.get(expected_column)
    if expected_type is not None and type(actual_value) is not expected_type:
        raise InputError(f"KillData handle {handle} populated the wrong primitive type")
    if handle in (3, 4, 9):
        expected = exact_ref(raw, width)
    elif handle in (10, 12, 13):
        if width != 32:
            raise InputError(f"KillData handle {handle} float width differs")
        expected = float(struct.unpack("<f", raw)[0])
        if not math.isfinite(expected):
            raise InputError(f"KillData handle {handle} is non-finite")
    elif handle == 11:
        if width != 8:
            raise InputError("DamageRegion byte width differs")
        expected = raw[0]
    elif handle == 14:
        if width != 32:
            raise InputError("RoundNumber Int32 width differs")
        expected = int.from_bytes(raw, "little", signed=True)
    elif handle == 15:
        if width != 1:
            raise InputError("finisher bool width differs")
        expected = bool(raw[0] & 1)
    elif handle == 5:
        expected = weapon_theme(raw, width)
    else:
        return
    if handle in (10, 12, 13):
        matches = struct.pack("<d", actual_value) == struct.pack("<d", expected)
    else:
        matches = actual_value == expected
    if not matches:
        raise InputError(f"KillData handle {handle} typed value differs from raw")


def extract_table(export, table, declared, refs):
    observations = []
    pending = []
    counts = {
        "parent_rows": 0,
        "element_updates": 0,
        "partial_updates": 0,
        "assistant_refs": 0,
        "unresolved_actor_refs": 0,
        "unresolved_guid_refs": 0,
    }
    for ordinal, row in selected(export / f"{table}.parquet"):
        if row["field_name"] == "KillData" and row["compatible_checksum"] == PARENT[1]:
            scope = row.get("checkpoint_index")
            scope_decl = declared.get(scope)
            if scope_decl is None or scope_decl.get(row["handle"]) != PARENT:
                raise InputError(
                    f"{table}: parent declaration mismatch in scope {scope}"
                )
            _, _, parts = parse_array(row["raw_bits"], row["bit_count"])
            expected = []
            for index, handle, width, raw in parts:
                if handle not in DECL:
                    raise InputError(f"{table}: unexpected KillData handle {handle}")
                if scope_decl.get(handle) != DECL[handle]:
                    raise InputError(
                        f"{table}: declaration mismatch for observed handle {handle} in scope {scope}"
                    )
                direct = f"KillData[{index}].{DECL[handle][0]}"
                expected.append(("direct", index, handle, width, raw, direct))
                if handle == 6:
                    if scope_decl.get(7) != DECL[7]:
                        raise InputError(
                            f"{table}: nested declaration mismatch in scope {scope}"
                        )
                    _, _, inner = parse_array(raw, width, {7})
                    for inner_index, inner_handle, inner_width, inner_raw in inner:
                        expected.append(
                            (
                                "assistant",
                                index,
                                inner_handle,
                                inner_width,
                                inner_raw,
                                f"{direct}[{inner_index}].AssistingPlayers",
                            )
                        )
            if len(pending) != len(expected):
                raise InputError(
                    f"{table}: child count before parent differs from raw array"
                )
            first = ordinal - len(expected)
            members = {}
            raw_members = {}
            assistants = {}
            for position, (
                (actual_ordinal, actual),
                (kind, index, handle, width, raw, name),
            ) in enumerate(zip(pending, expected)):
                if actual_ordinal != first + position:
                    raise InputError(
                        f"{table}: child is not physically adjacent to parent"
                    )
                if (
                    actual["field_name"],
                    actual["handle"],
                    actual["compatible_checksum"],
                    actual["bit_count"],
                    actual["raw_bits"],
                ) != (name, handle, None, width, raw):
                    raise InputError(
                        f"{table}: emitted child differs from parent raw window"
                    )
                if any(
                    actual.get(k) != row.get(k)
                    for k in (
                        "checkpoint_index",
                        "checkpoint_id",
                        "time_ms",
                        "packet_id",
                        "channel_index",
                        "actor_net_guid",
                        "object_net_guid",
                        "group_path",
                    )
                    if k in row
                ):
                    raise InputError(
                        f"{table}: child scope/coordinates differ from parent"
                    )
                if kind == "assistant":
                    ref = exact_ref(raw, width)
                    if (
                        type(actual["value_i64"]) is not int
                        or actual["value_i64"] != ref
                        or any(
                            actual[x] is not None
                            for x in ("value_f64", "value_bool", "value_str")
                        )
                    ):
                        raise InputError(
                            f"{table}: assistant ObjectNetGuid typing mismatch"
                        )
                    resolution = (
                        "null"
                        if ref == 0
                        else (
                            "resolved_actor"
                            if ref in refs[scope][0]
                            else "unresolved_actor"
                        )
                    )
                    assistants.setdefault(index, []).append(
                        {
                            "element_index": int(name.split("[")[2].split("]")[0]),
                            "ref": ref,
                            "actor_resolution": resolution,
                            "raw_bits_hex": raw.hex(),
                            "bit_count": width,
                        }
                    )
                    counts["assistant_refs"] += 1
                    if ref != 0:
                        counts["unresolved_actor_refs"] += ref not in refs[scope][0]
                else:
                    validate_direct(actual, handle, raw, width)
                    if handle == 6:
                        assistants.setdefault(index, [])
                    members.setdefault(index, {})[
                        MEMBERS.get(handle, "assisting_players_raw")
                    ] = value(actual, handle)
                    raw_members.setdefault(index, {})[DECL[handle][0]] = {
                        "handle": handle,
                        "bit_count": width,
                        "raw_bits_hex": raw.hex(),
                    }
            wire_indices = []
            for _, index, _, _, _, _ in expected:
                if index not in wire_indices:
                    wire_indices.append(index)
            for index in wire_indices:
                item = {name: None for name in MEMBERS.values()}
                item.update(members.get(index, {}))
                actor_refs = refs[scope][0]
                guid_refs = refs[scope][1]
                victim = item.get("victim_ref")
                item["victim_ref_resolution"] = (
                    "missing"
                    if victim is None
                    else (
                        "null"
                        if victim == 0
                        else (
                            "resolved_actor"
                            if victim in actor_refs
                            else "unresolved_actor"
                        )
                    )
                )
                if victim not in (None, 0):
                    counts["unresolved_actor_refs"] += victim not in actor_refs
                for key in ("killing_equippable_class_ref", "damage_type_ref"):
                    ref = item.get(key)
                    item[key + "_resolution"] = (
                        "missing"
                        if ref is None
                        else (
                            "null"
                            if ref == 0
                            else (
                                "resolved_net_guid"
                                if ref in guid_refs
                                else "unresolved_net_guid"
                            )
                        )
                    )
                    if ref not in (None, 0):
                        counts["unresolved_guid_refs"] += ref not in guid_refs
                item["assisting_players"] = (
                    assistants.get(index) if "assisting_players_raw" in item else None
                )
                item["raw_members"] = raw_members.get(index, {})
                complete = set(raw_members.get(index, {})) >= {
                    "Victim",
                    "KillingEquippableClass",
                    "WeaponTheme",
                    "DamageType",
                    "DamageTaken",
                    "DamageRegion",
                    "GameTimeElapsed",
                    "RoundTimestamp",
                    "RoundNumber",
                    "bDidKillTriggerFinisher",
                }
                counts["partial_updates"] += not complete
                counts["element_updates"] += 1
                observations.append(
                    {
                        "schema_version": SCHEMA_VERSION,
                        "kind": "killdata_serialized_update",
                        "source_table": table,
                        "checkpoint_index": scope,
                        "checkpoint_id": row.get("checkpoint_id"),
                        "physical_parent_row_ordinal": ordinal,
                        "element_index": index,
                        "time_ms": row["time_ms"],
                        "packet_id": row["packet_id"],
                        "channel_index": row["channel_index"],
                        "actor_net_guid": row["actor_net_guid"],
                        "object_net_guid": row["object_net_guid"],
                        "parent_raw_sha256": hashlib.sha256(
                            row["raw_bits"]
                        ).hexdigest(),
                        "members_complete": complete,
                        "members": item,
                    }
                )
            counts["parent_rows"] += 1
            pending = []
        else:
            pending.append((ordinal, row))
    if pending:
        raise InputError(f"{table}: unlinked KillData child rows")
    return observations, counts


def reject_overwrite(export, out):
    target = out.resolve()
    for p in export.iterdir():
        if p.is_file() and p.resolve() == target:
            raise InputError(
                f"--out aliases source export file {p}; refusing to overwrite input"
            )


def extract(export):
    inputs = (
        "fields",
        "checkpoint_fields",
        "actors",
        "net_guids",
        "checkpoint_actors",
        "checkpoint_net_guids",
        "checkpoint_export_groups",
        "checkpoint_export_fields",
    )
    input_before = {f"{t}.parquet": sha(export / f"{t}.parquet") for t in inputs}
    manifest_before = sha(export / "manifest.json")
    manifest, declared = declarations(export)
    tables = {}
    counts = {}
    for table in ("fields", "checkpoint_fields"):
        tables[table], counts[table] = extract_table(
            export, table, declared, scoped_refs(export, table)
        )
    input_after = {f"{t}.parquet": sha(export / f"{t}.parquet") for t in inputs}
    manifest_after = sha(export / "manifest.json")
    if input_after != input_before or manifest_after != manifest_before:
        raise InputError("source export changed while it was being read")
    return {
        "schema_version": SCHEMA_VERSION,
        "kind": "vrfkit_killdata_observation_export",
        "provenance": {
            "export_id": export.name,
            "replay_build": manifest["replay_build"],
            "manifest_sha256": manifest_before,
            "input_sha256": input_before,
            "extractor_sha256": sha(Path(__file__)),
        },
        "counts": counts,
        "observations": tables["fields"] + tables["checkpoint_fields"],
    }


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--export", type=Path, required=True)
    ap.add_argument("--out", type=Path, required=True)
    a = ap.parse_args(argv)
    try:
        if not a.export.is_dir():
            raise InputError(f"not an export directory: {a.export}")
        reject_overwrite(a.export, a.out)
        result = extract(a.export)
        atomic_write_text(
            a.out,
            json.dumps(
                result, ensure_ascii=True, separators=(",", ":"), allow_nan=False
            )
            + "\n",
        )
    except (OSError, KeyError, TypeError, json.JSONDecodeError, InputError) as exc:
        print(f"FAILED: {exc}", file=sys.stderr)
        return 1
    print(f"wrote {a.out}: {len(result['observations'])} serialized updates")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
