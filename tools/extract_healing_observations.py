"""Extract conservative healing observations from one vrfkit export."""

from __future__ import annotations
import argparse, collections, hashlib, json, math, re, struct, sys
from pathlib import Path
import pyarrow as pa
import pyarrow.compute as pc
import pyarrow.parquet as pq

if __package__:
    from .atomic_io import atomic_write_text, sha256_file
    from .extract_kill_observations import InputError, parse_array, exact_ref
    from .player_identity import load_player_bodies
else:
    from atomic_io import atomic_write_text, sha256_file
    from extract_kill_observations import InputError, parse_array, exact_ref
    from player_identity import load_player_bodies
SCHEMA_VERSION = 1
OUTER_GROUP = "/Script/ShooterGame.DamageableComponent_ClassNetCache"
PARAM_GROUP = "/Script/ShooterGame.DamageableComponent:MulticastNotifyHeal"
OUTER = (6, "MulticastNotifyHeal", 791426194)
DECL = {
    0: ("HealTaken", 1894010429),
    1: ("LifeChangeBySection", 163390906),
    2: ("ChangedComponent", 432306714),
    3: ("LifeResult", 1180204994),
    4: ("DeltaLife", 3211876783),
    5: ("bAliveAfterChange", 2136162893),
    7: ("EventInstigator", 3087885251),
    8: ("EventInstigatorPawn", 3901949544),
    9: ("HealCauser", 546618027),
}
TOP = {
    f"MulticastNotifyHeal.{n}": (h, c)
    for h, (n, c) in DECL.items()
    if h in (0, 1, 7, 8, 9)
}
MEMBERS = {
    2: "ChangedComponent",
    3: "LifeResult",
    4: "DeltaLife",
    5: "bAliveAfterChange",
}
RX = re.compile(
    r"^MulticastNotifyHeal\.LifeChangeBySection\[(\d+)\]\.(ChangedComponent|LifeResult|DeltaLife|bAliveAfterChange)$"
)
FIELD_COLS = [
    "time_ms",
    "packet_id",
    "channel_index",
    "actor_net_guid",
    "object_net_guid",
    "group_path",
    "handle",
    "field_name",
    "compatible_checksum",
    "bit_count",
    "raw_bits",
    "value_i64",
    "value_f64",
    "value_bool",
    "value_str",
]
CHECKPOINT_FIELD_COLS = ["checkpoint_index", "checkpoint_id", *FIELD_COLS]


class IntegrityError(InputError):
    pass


sha = sha256_file

def aliases(path, protected):
    target = path.resolve()
    for candidate in protected:
        if target == candidate.resolve():
            return True
        try:
            if path.exists() and candidate.exists() and path.samefile(candidate):
                return True
        except OSError:
            continue
    return False


def iter_rows(path, columns):
    ordinal = 0
    for batch in pq.ParquetFile(path).iter_batches(
        batch_size=65536, columns=columns, use_threads=False
    ):
        for row in batch.to_pylist():
            yield ordinal, row
            ordinal += 1


def iter_selected_fields(path, include_references, columns=FIELD_COLS):
    base = 0
    for batch in pq.ParquetFile(path).iter_batches(
        batch_size=65536, columns=columns, use_threads=False
    ):
        names = pc.cast(
            batch.column(batch.schema.get_field_index("field_name")), pa.string()
        )
        mask = pc.starts_with(names, pattern="MulticastNotifyHeal.")
        if include_references:
            mask = pc.or_(
                mask, pc.is_in(names, value_set=pa.array(["Owner", "Instigator"]))
            )
        indices = pc.indices_nonzero(pc.fill_null(mask, False))
        rows = batch.take(indices).to_pylist()
        for index, row in zip(indices.to_pylist(), rows):
            yield base + index, row
        base += batch.num_rows


def raw_record(r, ordinal, population):
    record = {
        "population": population,
        "physical_row_ordinal": ordinal,
        **{k: r.get(k) for k in FIELD_COLS if k != "raw_bits"},
        "raw_bits_hex": r["raw_bits"].hex() if r.get("raw_bits") is not None else None,
    }
    for name in ("checkpoint_index", "checkpoint_id"):
        if name in r:
            record[name] = r[name]
    return record


def exact_f32(r):
    if (
        r["bit_count"] != 32
        or r["raw_bits"] is None
        or len(r["raw_bits"]) != 4
        or r["value_f64"] is None
        or not math.isfinite(r["value_f64"])
    ):
        raise InputError("invalid f32 row")
    v = struct.unpack("<f", r["raw_bits"])[0]
    if struct.pack("<d", float(r["value_f64"])) != struct.pack("<d", float(v)):
        raise IntegrityError("f32 typed/raw mismatch")
    if any(r[x] is not None for x in ("value_i64", "value_bool", "value_str")):
        raise InputError("f32 conflicting typed columns")
    return v


def exact_bool(r):
    if (
        r["bit_count"] != 1
        or r["raw_bits"] is None
        or len(r["raw_bits"]) != 1
        or r["raw_bits"][0] not in (0, 1)
        or type(r["value_bool"]) is not bool
        or r["value_bool"] != (r["raw_bits"][0] == 1)
    ):
        raise InputError("bool typed/raw mismatch")
    if any(r[x] is not None for x in ("value_i64", "value_f64", "value_str")):
        raise InputError("bool conflicting typed columns")
    return r["value_bool"]


def ref_value(r, typed):
    if r["raw_bits"] is None or len(r["raw_bits"]) != (r["bit_count"] + 7) // 8:
        raise InputError("invalid reference window")
    v = exact_ref(r["raw_bits"], r["bit_count"])
    if typed:
        if type(r["value_i64"]) is not int or r["value_i64"] != v:
            raise IntegrityError("reference typed/raw mismatch")
    elif r["value_i64"] is not None:
        raise InputError("unexpected typed reference")
    if any(r[x] is not None for x in ("value_f64", "value_bool", "value_str")):
        raise InputError("reference conflicting typed columns")
    return v


def declarations(manifest):
    groups = collections.defaultdict(list)
    for g in manifest.get("net_field_export_groups", []):
        groups[g.get("path")].append(g)
    if len(groups[OUTER_GROUP]) != 1 or len(groups[PARAM_GROUP]) != 1:
        raise InputError("heal declaration group missing or duplicate")
    outer = [
        (x.get("handle"), x.get("name"), x.get("compatible_checksum"))
        for x in groups[OUTER_GROUP][0].get("fields", [])
    ]
    if OUTER not in outer:
        raise InputError("outer heal declaration differs")
    got = {}
    for x in groups[PARAM_GROUP][0].get("fields", []):
        h = x.get("handle")
        if h in got:
            raise InputError("duplicate heal parameter declaration")
        got[h] = (x.get("name"), x.get("compatible_checksum"))
    for h, identity in DECL.items():
        if h <= 5 and got.get(h) != identity:
            raise InputError("required heal parameter declarations differ")
        if h >= 7 and h in got and got[h] != identity:
            raise InputError("optional heal parameter declaration differs")
    return got


def validate_row_schema(r):
    name = r["field_name"]
    if r["group_path"] != OUTER_GROUP or r["handle"] != 6:
        raise InputError("foreign heal row scope")
    if name in TOP and r["compatible_checksum"] != TOP[name][1]:
        raise InputError("heal parameter checksum differs")
    if RX.match(name or "") and r["compatible_checksum"] is not None:
        raise InputError("flattened child unexpectedly has checksum")
    if name not in TOP and not RX.match(name or ""):
        raise InputError("unknown heal parameter row")


def active_instance(rows, guid, event):
    seq = rows.get(guid, [])
    if any(
        (b["time_ms"], b["packet_id"]) < (a["time_ms"], a["packet_id"])
        for a, b in zip(seq, seq[1:])
    ):
        return None, "lifecycle_time_regression"
    if len({(x["time_ms"], x["packet_id"]) for x in seq}) != len(seq):
        return None, "ambiguous_actor_lifecycle"
    if any(x["time_ms"] == event[0] for x in seq):
        return None, "lifecycle_boundary_same_time"
    prior = [x for x in seq if (x["time_ms"], x["packet_id"]) < event]
    if not prior:
        return None, "no_prior_actor_open"
    active = None
    for boundary in prior:
        if boundary["event"] == "open":
            if active is not None:
                return None, "actor_reopened_without_unique_active_instance"
            active = boundary
        elif boundary["event"] == "close":
            if active is None:
                return None, "actor_close_without_open"
            active = None
        else:
            return None, "unknown_actor_lifecycle_event"
    return (active, "active") if active is not None else (None, "actor_closed")


def edge(name, by, typed=False):
    q = by.get(name, [])
    if not q:
        return {"status": "absent", "value": None, "source_rows": []}
    if len(q) != 1:
        return {"status": "duplicate", "value": None, "source_rows": [x[0] for x in q]}
    try:
        v = ref_value(q[0][1], typed)
    except IntegrityError:
        raise
    except InputError as e:
        return {
            "status": "invalid",
            "value": None,
            "source_rows": [q[0][0]],
            "error": str(e),
        }
    return {
        "status": "null" if v == 0 else "present",
        "value": v,
        "source_rows": [q[0][0]],
    }


def parse_observation(key, items, guid_paths, actors, refs, players, disjoint=False):
    by = collections.defaultdict(list)
    schema_errors = []
    for ordinal, r in items:
        try:
            validate_row_schema(r)
        except InputError as e:
            schema_errors.append({"source_row": ordinal, "error": str(e)})
        by[r["field_name"]].append((ordinal, r))
    duplicates = {n: [x[0] for x in q] for n, q in by.items() if len(q) > 1}
    top = {
        n: edge(n, by, n.endswith("HealCauser"))
        for n in (
            "MulticastNotifyHeal.EventInstigator",
            "MulticastNotifyHeal.EventInstigatorPawn",
            "MulticastNotifyHeal.HealCauser",
        )
    }
    reasons = []
    if duplicates:
        reasons.append("duplicate_same_coordinate_member")
    if disjoint:
        reasons.append("disjoint_same_coordinate_group")
    if schema_errors:
        reasons.append("foreign_or_invalid_schema")
    amount = None
    sections = []
    healq = by.get("MulticastNotifyHeal.HealTaken", [])
    parentq = by.get("MulticastNotifyHeal.LifeChangeBySection", [])
    try:
        if schema_errors:
            raise InputError("foreign or invalid schema")
        if len(healq) != 1 or len(parentq) != 1:
            raise InputError("missing or duplicate amount parent")
        heal = exact_f32(healq[0][1])
        parent = parentq[0][1]
        if any(
            parent[x] is not None
            for x in ("value_i64", "value_f64", "value_bool", "value_str")
        ):
            raise IntegrityError("array parent has typed value")
        capacity, elements, leaves = parse_array(
            parent["raw_bits"], parent["bit_count"], set(MEMBERS)
        )
        emitted = {}
        for name, q in by.items():
            m = RX.match(name or "")
            if m:
                if len(q) != 1:
                    raise InputError("duplicate emitted child")
                emitted[
                    (
                        int(m.group(1)),
                        next(h for h, v in MEMBERS.items() if v == m.group(2)),
                    )
                ] = q[0][1]
        parsed = {(i, h): (w, raw) for i, h, w, raw in leaves}
        if len(parsed) != len(leaves) or set(parsed) != set(emitted):
            raise InputError("parent/child leaf set mismatch")
        child_ordinals = sorted(
            q[0][0] for name, q in by.items() if RX.match(name or "")
        )
        if child_ordinals and (
            child_ordinals
            != list(range(child_ordinals[0], child_ordinals[0] + len(child_ordinals)))
            or parentq[0][0] != child_ordinals[-1] + 1
        ):
            raise InputError(
                "emitted children are not contiguous immediately before parent"
            )
        emitted_order = []
        for name, q in sorted(by.items(), key=lambda item: item[1][0][0]):
            match = RX.match(name or "")
            if match:
                emitted_order.append(
                    (
                        int(match.group(1)),
                        next(
                            h for h, value in MEMBERS.items() if value == match.group(2)
                        ),
                    )
                )
        if emitted_order != [(index, handle) for index, handle, _, _ in leaves]:
            raise InputError("emitted child order differs from parent wire order")
        grouped = collections.defaultdict(dict)
        for (i, h), (w, raw) in parsed.items():
            r = emitted[(i, h)]
            if r["bit_count"] != w or r["raw_bits"] != raw:
                raise InputError("parent/child raw mismatch")
            grouped[i][MEMBERS[h]] = (
                r,
                (
                    ref_value(r, True)
                    if h == 2
                    else exact_f32(r) if h in (3, 4) else exact_bool(r)
                ),
            )
        if (
            elements != len(grouped)
            or capacity < elements
            or any(set(x) != set(MEMBERS.values()) for x in grouped.values())
        ):
            raise InputError("section cardinality mismatch")
        sections = [
            {
                "index": i,
                "changed_component_ref": x["ChangedComponent"][1],
                "changed_component_path": guid_paths.get(x["ChangedComponent"][1]),
                "life_result": x["LifeResult"][1],
                "delta_life": x["DeltaLife"][1],
                "alive_after_change": x["bAliveAfterChange"][1],
            }
            for i, x in sorted(grouped.items())
        ]
        total = sections[0]["delta_life"] if sections else 0.0
        for section in sections[1:]:
            total += section["delta_life"]
        if struct.pack("<f", total) != struct.pack("<f", heal):
            raise InputError("HealTaken does not equal section delta sum")
        amount = {
            "status": "validated",
            "heal_taken": heal,
            "sections": sections,
            "array_capacity": capacity,
        }
    except IntegrityError:
        raise
    except InputError as e:
        amount = {
            "status": "invalid",
            "error": str(e),
            "heal_taken": None,
            "sections": [],
        }
        reasons.append("amount_invalid")
    event = key[:2]
    causer = top["MulticastNotifyHeal.HealCauser"]
    source = {
        "status": causer["status"],
        "causer": causer,
        "event_instigator": top["MulticastNotifyHeal.EventInstigator"],
        "event_instigator_pawn": top["MulticastNotifyHeal.EventInstigatorPawn"],
        "actor": None,
        "owner": {"status": "not_evaluated"},
        "instigator": {"status": "not_evaluated"},
        "manifest_character_candidates": [],
    }
    if causer["status"] == "present":
        inst, st = active_instance(actors, causer["value"], event)
        source["status"] = st
        source["lifecycle_evidence"] = [
            {
                "physical_row_ordinal": x["_ordinal"],
                "time_ms": x["time_ms"],
                "packet_id": x["packet_id"],
                "channel_index": x["channel_index"],
                "event": x["event"],
                "class_path": x["class_path"],
            }
            for x in actors.get(causer["value"], [])
            if (x["time_ms"], x["packet_id"]) <= event
        ]
        if inst:
            source["actor"] = {
                "actor_net_guid": causer["value"],
                "channel_index": inst["channel_index"],
                "class_path": inst["class_path"],
                "open_source_row": inst["_ordinal"],
            }
            scoped = [
                x
                for x in refs.get(causer["value"], [])
                if x[1]["channel_index"] == inst["channel_index"]
                and x[1]["group_path"] == inst["class_path"]
                and (inst["time_ms"], inst["packet_id"])
                <= (x[1]["time_ms"], x[1]["packet_id"])
                < event
            ]
            source["reference_history"] = [
                raw_record(row, o, "main_reference_evidence") for o, row in scoped
            ]
            if any(x[1]["time_ms"] == event[0] for x in refs.get(causer["value"], [])):
                source["status"] = "reference_update_same_time"
            else:
                for field in ("Owner", "Instigator"):
                    q = [x for x in scoped if x[1]["field_name"] == field]
                    if not q:
                        source[field.lower()] = {
                            "status": "absent",
                            "value": None,
                            "source_rows": [],
                        }
                        continue
                    p = max((x[1]["time_ms"], x[1]["packet_id"]) for x in q)
                    q = [x for x in q if (x[1]["time_ms"], x[1]["packet_id"]) == p]
                    vals = set()
                    for _, row in q:
                        vals.add(ref_value(row, True))
                    source[field.lower()] = {
                        "status": "present" if len(vals) == 1 else "conflict",
                        "value": next(iter(vals)) if len(vals) == 1 else None,
                        "source_rows": [
                            raw_record(row, o, "main_reference_evidence")
                            for o, row in q
                        ],
                    }
                candidates = {
                    e["value"]
                    for e in (source["owner"], source["instigator"])
                    if e["status"] == "present" and e["value"] in players
                }
                source["manifest_character_candidates"] = sorted(candidates)
                source["status"] = (
                    "corroborated_static_manifest_character"
                    if len(candidates) == 1
                    else (
                        "conflicting_manifest_characters"
                        if len(candidates) > 1
                        else "no_manifest_character_reference"
                    )
                )
    pawn = source["event_instigator_pawn"]
    pawn["static_manifest_character"] = pawn.get("value") in players
    pawn["static_manifest_subject"] = players.get(pawn.get("value"))
    source["event_instigator"][
        "semantics"
    ] = "opaque packed reference candidate; no target type established"
    recipient_instance, recipient_status = active_instance(actors, key[3], event)
    recipient = {
        "actor_net_guid": key[3],
        "lifecycle_status": recipient_status,
        "static_manifest_character": key[3] in players,
        "subject": players.get(key[3]),
    }
    if recipient_instance:
        recipient.update(
            channel_index=recipient_instance["channel_index"],
            class_path=recipient_instance["class_path"],
            open_source_row=recipient_instance["_ordinal"],
        )
    return {
        "population": "main",
        "identity": {
            "time_ms": key[0],
            "packet_id": key[1],
            "channel_index": key[2],
            "actor_net_guid": key[3],
            "object_net_guid": key[4],
            "group_path": key[5],
            "outer_handle": key[6],
        },
        "identity_semantics": "coordinate-grouped observation; not a proven gameplay call",
        "source_rows": [raw_record(r, o, "main") for o, r in items],
        "schema_errors": schema_errors,
        "ambiguity_reasons": sorted(set(reasons)),
        "amount": amount,
        "source_corroboration": source,
        "recipient_corroboration": recipient,
    }


def extract(export):
    inputs = [
        export / n
        for n in (
            "manifest.json",
            "fields.parquet",
            "checkpoint_fields.parquet",
            "actors.parquet",
            "net_guids.parquet",
        )
    ]
    before = {p.name: sha(p) for p in inputs}
    source_files = [
        Path(__file__).resolve(),
        Path(__file__).with_name("extract_kill_observations.py"),
        Path(__file__).with_name("atomic_io.py"),
        Path(__file__).with_name("player_identity.py"),
    ]
    source_before = {p.name: sha(p) for p in source_files}
    manifest = json.loads((export / "manifest.json").read_text())
    declared = declarations(manifest)
    # Every pawn a SpawnedCharacter value names, not only the manifest's last
    # one: a reconnected player's earlier pawn is still that player's body.
    bodies = load_player_bodies(export, manifest)
    players = bodies.subjects
    paths = {
        x["net_guid"]: x["path"]
        for _, x in iter_rows(export / "net_guids.parquet", ["net_guid", "path"])
    }
    actors = collections.defaultdict(list)
    for o, r in iter_rows(
        export / "actors.parquet",
        [
            "time_ms",
            "packet_id",
            "channel_index",
            "actor_net_guid",
            "event",
            "class_path",
        ],
    ):
        if r["event"] in ("open", "close"):
            r["_ordinal"] = o
            actors[r["actor_net_guid"]].append(r)
    refs = collections.defaultdict(list)
    groups = collections.defaultdict(list)
    selected = []
    segments = collections.Counter()
    last_key = None
    last_selected_ordinal = None
    for o, r in iter_selected_fields(export / "fields.parquet", True):
        n = r["field_name"] or ""
        if n in ("Owner", "Instigator"):
            refs[r["actor_net_guid"]].append((o, r))
        if n.startswith("MulticastNotifyHeal."):
            optional = {
                "MulticastNotifyHeal.EventInstigator": 7,
                "MulticastNotifyHeal.EventInstigatorPawn": 8,
                "MulticastNotifyHeal.HealCauser": 9,
            }
            if (
                n in optional
                and r["group_path"] == OUTER_GROUP
                and r["handle"] == 6
                and optional[n] not in declared
            ):
                raise IntegrityError("observed optional heal row lacks its declaration")
            selected.append((o, r))
            k = (
                r["time_ms"],
                r["packet_id"],
                r["channel_index"],
                r["actor_net_guid"],
                r["object_net_guid"],
                r["group_path"],
                r["handle"],
            )
            groups[k].append((o, r))
            if (
                k != last_key
                or last_selected_ordinal is None
                or o != last_selected_ordinal + 1
            ):
                segments[k] += 1
            last_key = k
            last_selected_ordinal = o
        else:
            last_key = None
            last_selected_ordinal = o
    cp = []
    for o, r in iter_selected_fields(
        export / "checkpoint_fields.parquet", False, CHECKPOINT_FIELD_COLS
    ):
        record = raw_record(r, o, "checkpoint")
        try:
            validate_row_schema(r)
            record["schema_status"] = "matches_main_route"
        except InputError as e:
            record["schema_status"] = "foreign_or_invalid"
            record["schema_error"] = str(e)
        cp.append(record)
    observations = [
        parse_observation(k, v, paths, actors, refs, players, segments[k] > 1)
        for k, v in sorted(groups.items())
    ]
    valid = [
        x
        for x in observations
        if x["amount"]["status"] == "validated" and not x["ambiguity_reasons"]
    ]
    by_section = collections.Counter()
    by_recipient = collections.Counter()
    for x in valid:
        for s in x["amount"]["sections"]:
            by_section[s["changed_component_path"] or "<unresolved>"] += s["delta_life"]
            by_recipient[str(x["identity"]["actor_net_guid"])] += s["delta_life"]
    after = {p.name: sha(p) for p in inputs}
    if before != after:
        raise IntegrityError("input changed during extraction")
    source_after = {p.name: sha(p) for p in source_files}
    if source_before != source_after:
        raise IntegrityError("implementation changed during extraction")
    return {
        "schema_version": SCHEMA_VERSION,
        "kind": "vrfkit_healing_observations",
        "export_id": export.name,
        "source": str(export.resolve()),
        "source_hashes": before,
        "player_identity": bodies.counts,
        "provenance": {
            "replay_build": manifest.get("replay_build"),
            "input_sha256_before": before,
            "input_sha256_after": after,
            "implementation_sha256_before": source_before,
            "implementation_sha256_after": source_after,
        },
        "observations": observations,
        "checkpoint_observations": cp,
        "summaries": {
            "population": "main validated unambiguous serialized HealTaken/DeltaLife observations; no effective-HP or player-credit semantics",
            "validated_observations": len(valid),
            "serialized_heal_amount_sum_by_recipient_actor_net_guid": dict(
                by_recipient
            ),
            "serialized_heal_amount_sum_by_section_path": dict(by_section),
        },
        "counts": {
            "main_coordinate_groups": len(observations),
            "main_selected_rows": len(selected),
            "checkpoint_selected_rows": len(cp),
            "amount_validated": sum(
                x["amount"]["status"] == "validated" for x in observations
            ),
            "amount_invalid": sum(
                x["amount"]["status"] != "validated" for x in observations
            ),
            "ambiguous_groups": sum(bool(x["ambiguity_reasons"]) for x in observations),
        },
    }


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--export", required=True, type=Path)
    p.add_argument("--out", required=True, type=Path)
    a = p.parse_args(argv)
    try:
        protected = [
            a.export / n
            for n in (
                "manifest.json",
                "fields.parquet",
                "checkpoint_fields.parquet",
                "actors.parquet",
                "net_guids.parquet",
            )
        ] + [
            Path(__file__),
            Path(__file__).with_name("extract_kill_observations.py"),
            Path(__file__).with_name("atomic_io.py"),
            Path(__file__).with_name("player_identity.py"),
        ]
        if aliases(a.out, protected):
            raise InputError("output aliases an input or implementation file")
        d = extract(a.export)
        atomic_write_text(
            a.out, json.dumps(d, indent=2, sort_keys=True, allow_nan=False) + "\n"
        )
    except (OSError, ValueError, json.JSONDecodeError) as e:
        print(f"FAILED: {e}", file=sys.stderr)
        return 1
    print(f"wrote {a.out} ({d['counts']['main_coordinate_groups']} observations)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
