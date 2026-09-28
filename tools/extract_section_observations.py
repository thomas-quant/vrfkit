"""Serialize raw DamageableComponent section observations without state inference.

This deliberately records malformed, parentless and checkpoint rows.  A
validated relation only says that this RPC's serialized scalar agrees with its
serialized section deltas; it does not establish gameplay ordering, health
pools, death, or player credit.
"""
from __future__ import annotations

import argparse, collections, hashlib, json, math, os, re, struct, sys
from pathlib import Path

import pyarrow as pa
import pyarrow.compute as pc
import pyarrow.parquet as pq

if __package__:
    from .atomic_io import aliases, atomic_write_text, sha256_file
    from .extract_kill_observations import InputError, exact_ref, parse_array
else:
    from atomic_io import aliases, atomic_write_text, sha256_file
    from extract_kill_observations import InputError, exact_ref, parse_array

SCHEMA_VERSION = 1
OUTER_GROUP = "/Script/ShooterGame.DamageableComponent_ClassNetCache"
ROUTES = {
    "MulticastNotifyDamage_Point": (1, 457657198, "LifeChangeEvents", 9, "DamageTaken", 0, {10: "ChangedComponent", 11: "LifeResult", 12: "DeltaLife", 13: "bAliveAfterChange"}, "damage"),
    "MulticastNotifyDamage_Base": (0, 3601293819, "LifeChangeEvents", 9, "DamageTaken", 0, {10: "ChangedComponent", 11: "LifeResult", 12: "DeltaLife", 13: "bAliveAfterChange"}, "damage"),
    "MulticastNotifyHeal": (6, 791426194, "LifeChangeBySection", 1, "HealTaken", 0, {2: "ChangedComponent", 3: "LifeResult", 4: "DeltaLife", 5: "bAliveAfterChange"}, "positive"),
    "MulticastNotifyOverhealDecay": (7, 1072798296, "LifeChangeBySection", 1, "DecayApplied", 0, {2: "ChangedComponent", 3: "LifeResult", 4: "DeltaLife", 5: "bAliveAfterChange"}, "positive"),
    "MulticastSectionLifeChange": (8, 1280092974, "LifeChangeEvents", 0, None, None, {1: "ChangedComponent", 2: "LifeResult", 3: "DeltaLife", 4: "bAliveAfterChange"}, "reset"),
}
# Measured main declaration identities on builds 13.01/02/04/05. A matching
# name in a changed schema cannot authorize interpreting a member as f32/ref.
# Optional parameters outside this map remain uninterpreted raw evidence.
VALUE_CHECKSUMS = {
    "MulticastNotifyDamage_Point": {0: 373546733, 9: 314962236, 10: 1435614478, 11: 3234979, 12: 3709621854, 13: 3155865075},
    "MulticastNotifyDamage_Base": {0: 2316259323, 9: 771135212, 10: 1477578271, 11: 1537455328, 12: 4098809706, 13: 3883502598},
    "MulticastNotifyHeal": {0: 1894010429, 1: 163390906, 2: 432306714, 3: 1180204994, 4: 3211876783, 5: 2136162893},
    "MulticastNotifyOverhealDecay": {0: 1863096949, 1: 163390906, 2: 432306714, 3: 1180204994, 4: 3211876783, 5: 2136162893},
    "MulticastSectionLifeChange": {0: 785272984, 1: 1916404200, 2: 2548634715, 3: 1295195562, 4: 68895389},
}
COORDINATE_COLUMNS = ("time_ms", "packet_id", "channel_index", "actor_net_guid", "object_net_guid", "group_path", "handle")
FIELDS = ["time_ms", "packet_id", "channel_index", "actor_net_guid", "object_net_guid", "group_path", "handle", "field_name", "compatible_checksum", "bit_count", "raw_bits", "value_i64", "value_f64", "value_bool", "value_str"]
CP_FIELDS = ["checkpoint_index", "checkpoint_id", *FIELDS]
HEALTH_SECTION_PATH = "HealthDamageSection"
INPUT_NAMES = ("manifest.json", "fields.parquet", "checkpoint_fields.parquet", "net_guids.parquet")
HELPER_NAMES = ("extract_kill_observations.py", "atomic_io.py")


class IntegrityError(InputError):
    pass


sha = sha256_file


def raw(r, ordinal, population):
    return {"population": population, "physical_row_ordinal": ordinal,
            **{k: r.get(k) for k in FIELDS if k != "raw_bits"},
            "raw_bits_hex": r["raw_bits"].hex() if r.get("raw_bits") is not None else None,
            **{k: r[k] for k in ("checkpoint_index", "checkpoint_id") if k in r}}


def f32(r):
    if r.get("bit_count") != 32 or r.get("raw_bits") is None or len(r["raw_bits"]) != 4 or not isinstance(r.get("value_f64"), float) or not math.isfinite(r["value_f64"]):
        raise InputError("invalid f32 row")
    wire = struct.unpack("<f", r["raw_bits"])[0]
    if struct.pack("<d", wire) != struct.pack("<d", r["value_f64"]):
        raise IntegrityError("f32 typed/raw mismatch")
    if any(r.get(k) is not None for k in ("value_i64", "value_bool", "value_str")):
        raise InputError("f32 conflicting typed columns")
    return wire


def boolean(r):
    if r.get("bit_count") != 1 or r.get("raw_bits") not in (b"\0", b"\1") or type(r.get("value_bool")) is not bool or r["value_bool"] != (r["raw_bits"] == b"\1"):
        raise InputError("bool typed/raw mismatch")
    if any(r.get(k) is not None for k in ("value_i64", "value_f64", "value_str")):
        raise InputError("bool conflicting typed columns")
    return r["value_bool"]


def reference(r):
    bits, payload = r.get("bit_count"), r.get("raw_bits")
    if type(bits) is not int or payload is None or len(payload) != (bits + 7) // 8:
        raise InputError("invalid packed reference window")
    value = exact_ref(payload, bits)
    if type(r.get("value_i64")) is not int or r["value_i64"] != value:
        raise IntegrityError("packed reference typed/raw mismatch")
    if any(r.get(k) is not None for k in ("value_f64", "value_bool", "value_str")):
        raise InputError("packed reference conflicting typed columns")
    return value


def route_name(field):
    return (field or "").split(".", 1)[0]


def selected_rows(path, columns):
    """Arrow-filter route rows before materializing Python dictionaries."""
    base = 0
    for batch in pq.ParquetFile(path).iter_batches(batch_size=65536, columns=columns, use_threads=False):
        names = pc.cast(batch.column(batch.schema.get_field_index("field_name")), pa.string())
        mask = None
        for route in ROUTES:
            item = pc.starts_with(names, pattern=route + ".")
            mask = item if mask is None else pc.or_(mask, item)
        indices = pc.indices_nonzero(pc.fill_null(mask, False))
        for offset, row in zip(indices.to_pylist(), batch.take(indices).to_pylist()):
            yield base + offset, row
        base += batch.num_rows


def declarations(manifest):
    groups = collections.defaultdict(list)
    for group in manifest.get("net_field_export_groups", []): groups[group.get("path")].append(group)
    if len(groups[OUTER_GROUP]) != 1: raise InputError("outer declaration missing or duplicate")
    outer = {}
    for x in groups[OUTER_GROUP][0].get("fields", []):
        h = x.get("handle")
        if h in outer: raise InputError("duplicate outer declaration handle")
        outer[h] = (x.get("name"), x.get("compatible_checksum"))
    result = {}
    for route, (handle, crc, parent, parent_handle, scalar, scalar_handle, members, relation) in ROUTES.items():
        # A route can be absent from an export entirely.  It becomes an error
        # only if a selected row claims an absent or different outer entry.
        outer_entry = outer.get(handle)
        path = "/Script/ShooterGame.DamageableComponent:" + route
        if len(groups[path]) > 1: raise InputError("duplicate route declaration group: " + route)
        # Parameter groups are not universal across the measured exports.  An
        # absent group is valid only when no row later claims this route.
        fields = None if not groups[path] else {}
        if fields is not None:
            for x in groups[path][0].get("fields", []):
                h = x.get("handle")
                if h in fields: raise InputError("duplicate route declaration handle: " + route)
                fields[h] = (x.get("name"), x.get("compatible_checksum"))
        result[route] = {"outer": None if outer_entry is None else {"handle": handle, "name": outer_entry[0], "compatible_checksum": outer_entry[1]}, "parameters": None if fields is None else [{"handle": h, "name": n, "compatible_checksum": c} for h, (n, c) in sorted(fields.items())], "_fields": fields}
    return result


def parse_group(route, key, rows, paths, segments, declared):
    handle, crc, parent_name, parent_handle, scalar_name, scalar_handle, members, relation = ROUTES[route]
    expected_parent = route + "." + parent_name
    child_rx = re.compile("^" + re.escape(expected_parent) + r"\[(\d+)\]\.([A-Za-z0-9_]+)$")
    by = collections.defaultdict(list); errors = []
    for ordinal, r in rows:
        name = r.get("field_name") or ""
        if tuple(r.get(k) for k in COORDINATE_COLUMNS) != key:
            errors.append({"source_row": ordinal, "error": "row coordinate differs from group"})
        if r.get("group_path") != OUTER_GROUP or r.get("handle") != handle: errors.append({"source_row": ordinal, "error": "foreign route scope"})
        outer = declared[route]["outer"]
        if outer is None or (outer["name"], outer["compatible_checksum"]) != (route, crc):
            errors.append({"source_row": ordinal, "error": "observed route has no exact outer declaration"})
        fields = declared[route]["_fields"]
        if fields is None:
            errors.append({"source_row": ordinal, "error": "observed route has no scoped parameter declaration"})
        else:
            top_name = name[len(route) + 1:] if name.startswith(route + ".") else None
            child = child_rx.match(name)
            if child:
                child_name = child.group(2)
                member_handle = {v: h for h, v in members.items()}.get(child_name)
                if member_handle is None:
                    errors.append({"source_row": ordinal, "error": "unknown section child name"})
                elif fields.get(member_handle) != (child_name, VALUE_CHECKSUMS[route][member_handle]):
                    errors.append({"source_row": ordinal, "error": "section child declaration/handle differs"})
            elif top_name in (parent_name, scalar_name):
                expected_handle = parent_handle if top_name == parent_name else scalar_handle
                expected = (top_name, VALUE_CHECKSUMS[route][expected_handle])
                if fields.get(expected_handle) != expected or r.get("compatible_checksum") != expected[1]:
                    errors.append({"source_row": ordinal, "error": "interpreted parameter declaration differs"})
            elif top_name is None or not any(identity == (top_name, r.get("compatible_checksum")) for identity in fields.values()):
                errors.append({"source_row": ordinal, "error": "top-level declaration/checksum differs"})
        if name == expected_parent or name == route + "." + (scalar_name or ""):
            if r.get("compatible_checksum") is None: errors.append({"source_row": ordinal, "error": "top-level checksum absent"})
        elif child_rx.match(name):
            if r.get("compatible_checksum") is not None: errors.append({"source_row": ordinal, "error": "flattened child has checksum"})
        # Other declared top-level parameters and nested arrays are retained
        # raw. They are not section-state members, but neither are they schema
        # errors merely because this extractor does not interpret them.
        by[name].append((ordinal, r))
    reasons = []
    if segments > 1: reasons.append("disjoint_physical_segments")
    ordinals = [o for o, _ in rows]
    if any(type(o) is not int or o < 0 for o in ordinals) or ordinals != sorted(set(ordinals)):
        reasons.append("invalid_physical_row_order")
    if any(len(v) > 1 for v in by.values()): reasons.append("duplicate_same_coordinate_member")
    if errors: reasons.append("foreign_or_invalid_schema")
    report = {"status": "uninterpreted", "relation": "reset has no expected delta relation" if relation == "reset" else None, "sections": [], "scalar": None}
    try:
        if scalar_name is not None:
            scalar_rows = by.get(route + "." + scalar_name, [])
            if len(scalar_rows) == 1:
                report["scalar"] = f32(scalar_rows[0][1])
            elif len(scalar_rows) > 1:
                raise InputError("duplicate scalar")
        parent = by.get(expected_parent, [])
        if len(parent) == 0:
            orphan = [name for name in by if child_rx.match(name)]
            if orphan:
                report.update(status="invalid", error="orphan section children without parent")
                reasons.append("array_validation_failed")
            else:
                report.update(status="parentless_rpc", relation="no array parent; state uninterpreted")
            return result(route, key, rows, errors, reasons, report)
        if len(parent) != 1: raise InputError("missing or duplicate array parent")
        if any(parent[0][1].get(x) is not None for x in ("value_i64", "value_f64", "value_bool", "value_str")): raise IntegrityError("array parent has typed value")
        capacity, elements, leaves = parse_array(parent[0][1]["raw_bits"], parent[0][1]["bit_count"], set(members))
        emitted = {}
        for name, q in by.items():
            m = child_rx.match(name)
            if m:
                if len(q) != 1: raise InputError("duplicate emitted child")
                name_to_handle = {v: h for h, v in members.items()}
                if m.group(2) not in name_to_handle:
                    raise InputError("unknown section child name")
                emitted[(int(m.group(1)), name_to_handle[m.group(2)])] = q[0]
        parsed = {(i,h):(w,b) for i,h,w,b in leaves}
        if len(parsed) != len(leaves) or set(parsed) != set(emitted): raise InputError("parent/child leaf set mismatch")
        child_ordinals = sorted(x[0] for x in emitted.values())
        if child_ordinals and (child_ordinals != list(range(child_ordinals[0], child_ordinals[0]+len(child_ordinals))) or parent[0][0] != child_ordinals[-1]+1): raise InputError("children are not contiguous immediately before parent")
        order = []
        for _, r in sorted(rows):
            m = child_rx.match(r.get("field_name") or "")
            if m: order.append((int(m.group(1)), {v:h for h,v in members.items()}[m.group(2)]))
        if order != [(i,h) for i,h,_,_ in leaves]: raise InputError("child physical order differs from parent wire order")
        grouped = collections.defaultdict(dict)
        for (index, h), (width, payload) in parsed.items():
            ordinal, child = emitted[(index,h)]
            if child.get("bit_count") != width or child.get("raw_bits") != payload: raise InputError("parent/child raw mismatch")
            value = reference(child) if members[h] == "ChangedComponent" else f32(child) if members[h] in ("LifeResult", "DeltaLife") else boolean(child)
            grouped[index][members[h]] = value
        if elements != len(grouped) or capacity < elements or any(set(v) != set(members.values()) for v in grouped.values()): raise InputError("section cardinality differs")
        sections = [{"index": i, "changed_component_ref": x["ChangedComponent"], "changed_component_path": paths.get(x["ChangedComponent"]), "life_result": x["LifeResult"], "delta_life": x["DeltaLife"], "alive_after_change": x["bAliveAfterChange"]} for i,x in sorted(grouped.items())]
        report.update(status="validated_array", sections=sections, array_capacity=capacity)
        health = [s for s in sections if s["changed_component_path"] == HEALTH_SECTION_PATH]
        unknown = [s for s in sections if s["changed_component_path"] is None]
        # An unresolved component is not evidence that the array lacks Health.
        report["health_section_status"] = ("multiple_exact_health_sections" if len(health) > 1 else "present" if health else "unknown_section_identity" if unknown else "array_without_health_section")
        if scalar_name is not None:
            q = by.get(route + "." + scalar_name, [])
            if len(q) != 1: raise InputError("missing or duplicate scalar")
            scalar = f32(q[0][1])
            # Keep both arithmetic methods visible.  The f64 accumulation is
            # rounded once; iterative f32 is reported separately, never used
            # to silently choose a universal wire rule.
            values = [s["delta_life"] for s in sections]
            f64_sum = values[0] if values else 0.0
            for value in values[1:]: f64_sum += value
            rounded_once = struct.unpack("<f", struct.pack("<f", f64_sum))[0]
            iterative = values[0] if values else 0.0
            for value in values[1:]: iterative = struct.unpack("<f", struct.pack("<f", iterative + value))[0]
            expected = -rounded_once if relation == "damage" else rounded_once
            report["scalar"] = scalar
            report["relation"] = {"kind": "damage scalar equals negative rounded-once f64 delta sum" if relation == "damage" else "scalar equals rounded-once f64 delta sum", "f64_delta_sum": f64_sum, "f32_rounded_once_delta_sum": rounded_once, "iterative_f32_delta_sum": iterative, "expected_scalar": expected, "matches": struct.pack("<f", scalar) == struct.pack("<f", expected)}
            if not report["relation"]["matches"]: reasons.append("scalar_delta_relation_mismatch")
    except IntegrityError: raise
    except InputError as e:
        report.update(status="invalid", error=str(e))
        reasons.append("array_validation_failed")
    report["eligible_for_state_comparison"] = report["status"] == "validated_array" and not reasons
    return result(route, key, rows, errors, reasons, report)


def result(route, key, rows, errors, reasons, report):
    report.setdefault("eligible_for_state_comparison", report["status"] == "validated_array" and not reasons)
    tokens = {}
    for suffix in ("RespawnNumber", "LifeChangeEventIndex", "VictimRespawnNumber"):
        found = [(o, r) for o, r in rows if r.get("field_name") == route + "." + suffix]
        if found:
            tokens[suffix] = [raw(r, o, "main") for o, r in found]
    return {"population": "main", "route": route, "identity": dict(zip(("time_ms","packet_id","channel_index","actor_net_guid","object_net_guid","group_path","outer_handle"), key)), "identity_semantics": "coordinate-grouped serialized observation; physical row order retained; not a proven gameplay event", "source_rows": [raw(r,o,"main") for o,r in rows], "raw_lifecycle_tokens": tokens, "schema_errors": errors, "ambiguity_reasons": sorted(set(reasons)), "section_state": report}


def extract(export):
    inputs = [export / n for n in INPUT_NAMES]
    before = {p.name: sha(p) for p in inputs}
    source_files = [Path(__file__).resolve(), *(Path(__file__).with_name(n) for n in HELPER_NAMES)]
    impl_before = {p.name: sha(p) for p in source_files}
    manifest = json.loads((export / "manifest.json").read_text(encoding="utf-8"))
    decl = declarations(manifest)
    path_sets = collections.defaultdict(set)
    for r in pq.read_table(export / "net_guids.parquet", columns=["net_guid","path"], use_threads=False).to_pylist(): path_sets[r["net_guid"]].add(r["path"])
    paths = {guid: next(iter(values)) if len(values) == 1 else None for guid, values in path_sets.items()}
    groups = collections.OrderedDict(); segments = collections.Counter(); selected = []; last = None; last_o = None
    for o, r in selected_rows(export / "fields.parquet", FIELDS):
        route = route_name(r.get("field_name"))
        if route not in ROUTES: last = None; continue
        key = (r["time_ms"],r["packet_id"],r["channel_index"],r["actor_net_guid"],r["object_net_guid"],r["group_path"],r["handle"])
        groups.setdefault((route,key), []).append((o,r)); selected.append((o,r))
        if (route,key) != last or last_o is None or o != last_o + 1: segments[(route,key)] += 1
        last=(route,key); last_o=o
    checkpoint=[]
    for o,r in selected_rows(export / "checkpoint_fields.parquet", CP_FIELDS):
        x=raw(r,o,"checkpoint"); x["state_interpretation"]="checkpoint state-only row; not a main event"; checkpoint.append(x)
    observations=[parse_group(route,key,rows,paths,segments[(route,key)],decl) for (route,key),rows in groups.items()]
    after={p.name:sha(p) for p in inputs}; impl_after={p.name:sha(p) for p in source_files}
    if before != after: raise IntegrityError("input changed during extraction")
    if impl_before != impl_after: raise IntegrityError("implementation changed during extraction")
    public_decl={k:{n:v for n,v in x.items() if n != "_fields"} for k,x in decl.items()}
    by_route = {}
    for route in ROUTES:
        route_rows = [x for x in observations if x["route"] == route]
        by_route[route] = {"coordinate_groups": len(route_rows), "selected_rows": sum(len(x["source_rows"]) for x in route_rows), "validated_arrays": sum(x["section_state"]["status"] == "validated_array" for x in route_rows), "parentless_rpcs": sum(x["section_state"]["status"] == "parentless_rpc" for x in route_rows), "invalid_arrays": sum(x["section_state"]["status"] == "invalid" for x in route_rows), "schema_invalid": sum(bool(x["schema_errors"]) for x in route_rows), "ambiguous": sum(bool(x["ambiguity_reasons"]) for x in route_rows), "relation_mismatch": sum("scalar_delta_relation_mismatch" in x["ambiguity_reasons"] for x in route_rows)}
    counts={"main_coordinate_groups":len(observations),"main_selected_rows":len(selected),"checkpoint_selected_rows":len(checkpoint),"validated_arrays":sum(x["section_state"]["status"]=="validated_array" for x in observations),"invalid_arrays":sum(x["section_state"]["status"]=="invalid" for x in observations),"parentless_rpcs":sum(x["section_state"]["status"]=="parentless_rpc" for x in observations),"schema_invalid":sum(bool(x["schema_errors"]) for x in observations),"ambiguous_groups":sum(bool(x["ambiguity_reasons"]) for x in observations),"relation_mismatch":sum("scalar_delta_relation_mismatch" in x["ambiguity_reasons"] for x in observations),"by_route":by_route}
    return {"schema_version":SCHEMA_VERSION,"kind":"vrfkit_section_observations","export_id":export.name,"source":str(export.resolve()),"route_declarations":public_decl,"provenance":{"replay_build":manifest.get("replay_build"),"input_sha256_before":before,"input_sha256_after":after,"implementation_sha256_before":impl_before,"implementation_sha256_after":impl_after},"observations":observations,"checkpoint_observations":checkpoint,"counts":counts}


def main(argv=None):
    p=argparse.ArgumentParser(description=__doc__); p.add_argument("--export",required=True,type=Path); p.add_argument("--out",required=True,type=Path); a=p.parse_args(argv)
    protected=[a.export/n for n in INPUT_NAMES]+[Path(__file__),*(Path(__file__).with_name(n) for n in HELPER_NAMES)]
    try:
        if a.export.is_dir():
            protected.extend(p for p in a.export.iterdir() if p.is_file())
        if aliases(a.out, protected): raise InputError("output aliases an input or implementation file")
        data=extract(a.export); atomic_write_text(a.out,json.dumps(data,indent=2,sort_keys=True,allow_nan=False)+"\n")
    except (OSError, ValueError, json.JSONDecodeError) as e:
        print("FAILED: " + str(e),file=sys.stderr); return 1
    print("wrote %s (%d observations)" % (a.out,data["counts"]["main_coordinate_groups"])); return 0


if __name__ == "__main__": raise SystemExit(main())
