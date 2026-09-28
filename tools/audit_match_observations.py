#!/usr/bin/env python3
"""Audit whether ammo-decrease observations have independently scoped RPC evidence.

``extract_match_observations.py`` intentionally describes an ammo decrease as
a replicated-property transition and gives it only a *global* EffectID timing
hint.  This tool does not turn that hint into a shot count.  It measures the
stronger join available in current exports: a gun-scoped
``MulticastPlayContinuousEffectFromClient`` RPC whose actor GUID is the same
weapon GUID as the ammo component's ``outer_net_guid``.

The result retains the failure modes of the join.  In particular, a missing
or non-unique RPC is not guessed into a shot, and a first ammo sample is
reported as left-censored rather than treated as a transition.

Usage:
    python tools/audit_match_observations.py --export out/replay --out audit.json
    python tools/audit_match_observations.py --exports out/exports --out audit.json
"""

from __future__ import annotations

import argparse
from concurrent.futures import ThreadPoolExecutor
import hashlib
import json
from collections import Counter, defaultdict
from pathlib import Path

import pyarrow as pa
import pyarrow.compute as pc
import pyarrow.parquet as pq

if __package__:
    from .atomic_io import atomic_write_text
    from .export_scan import child_exports, leftover_note, skipped_report
else:
    from atomic_io import atomic_write_text
    from export_scan import child_exports, leftover_note, skipped_report


AMMO_FIELD = "AuthResourceAmount"
EFFECT_FIELD = "MulticastPlayContinuousEffectFromClient.EffectID"
MAGAZINE_PATH = "MagazineAmmo"
GUN_PATH_PREFIX = "/Game/Equippables/Guns/"


def _collapse(samples: list[tuple[int, int, int, int]]) -> tuple[list[tuple[int, int, int | None]], int]:
    """Collapse scalar samples without inventing an order inside one packet."""
    by_position: dict[tuple[int, int], set[int]] = defaultdict(set)
    for time_ms, packet_id, _row, value in samples:
        by_position[(time_ms, packet_id)].add(value)
    collapsed: list[tuple[int, int, int | None]] = []
    ambiguous = 0
    previous = object()
    for (time_ms, packet_id), values in sorted(by_position.items()):
        if len(values) != 1:
            ambiguous += 1
            collapsed.append((time_ms, packet_id, None))
            previous = object()
            continue
        value = next(iter(values))
        if value != previous:
            collapsed.append((time_ms, packet_id, value))
            previous = value
    return collapsed, ambiguous


def _read_relevant_fields(path: Path):
    """Yield only target rows without retaining a whole fields table.

    The retained corpus has exports with more than a million flattened field
    rows.  Parquet's dictionary predicate reduces the output, but not every
    writer can use it to skip a row group.  Batching therefore bounds this
    audit's memory when it scans the whole corpus.
    """
    columns = ["time_ms", "packet_id", "actor_net_guid", "object_net_guid",
               "group_path", "field_name", "value_i64"]
    for batch in pq.ParquetFile(path).iter_batches(columns=columns, batch_size=65_536, use_threads=False):
        table = pa.Table.from_batches([batch])
        selected = table.filter(pc.is_in(table["field_name"], value_set=pa.array([AMMO_FIELD, EFFECT_FIELD])))
        yield from selected.to_pylist()


def audit_export(export_dir: Path, *, window_ms: int = 300) -> dict:
    """Return a conservative per-export ammo/RPC corroboration audit."""
    fields_path = export_dir / "fields.parquet"
    net_guids_path = export_dir / "net_guids.parquet"
    missing = [str(path.name) for path in (fields_path, net_guids_path) if not path.is_file()]
    if missing:
        raise ValueError(f"missing required export tables: {', '.join(missing)}")
    if window_ms < 0:
        raise ValueError("window_ms must be non-negative")

    net = pq.read_table(net_guids_path, columns=["net_guid", "path", "outer_net_guid"])
    identities = defaultdict(set)
    for guid, path, outer in zip(net["net_guid"].to_pylist(), net["path"].to_pylist(), net["outer_net_guid"].to_pylist()):
        identities[guid].add((path, outer))
    stable = {guid: next(iter(options)) for guid, options in identities.items() if len(options) == 1}
    path_of = {guid: value[0] for guid, value in stable.items()}
    outer_of = {guid: value[1] for guid, value in stable.items()}

    magazines: dict[int, list[tuple[int, int, int, int]]] = defaultdict(list)
    effects: dict[int, list[tuple[int, int]]] = defaultdict(list)
    for row_number, row in enumerate(_read_relevant_fields(fields_path)):
        name = row["field_name"]
        group = row["group_path"] or ""
        value = row["value_i64"]
        if (name == AMMO_FIELD and value is not None
                and group.endswith("AmmoComponent")
                and path_of.get(row["object_net_guid"]) == MAGAZINE_PATH):
            magazines[int(row["object_net_guid"])].append((
                int(row["time_ms"]), int(row["packet_id"]), row_number, int(value)
            ))
        elif (name == EFFECT_FIELD and group.startswith(GUN_PATH_PREFIX)
              and group.endswith("_ClassNetCache")):
            # actor_net_guid is the gun actor for this class-net-cache RPC.
            effects[int(row["actor_net_guid"])].append((
                int(row["time_ms"]), int(row["packet_id"])
            ))

    status = Counter()
    offset_ms = Counter()
    ambiguous_ammo_packets = 0
    left_censored_streams = 0
    events_examined = 0
    for component, samples in magazines.items():
        compact, ambiguous = _collapse(samples)
        ambiguous_ammo_packets += ambiguous
        if any(value is not None for _, _, value in compact):
            left_censored_streams += 1
        weapon = outer_of.get(component)
        for (before_time, _before_packet, before), (time_ms, packet_id, after) in zip(compact, compact[1:]):
            if before is None or after is None:
                continue
            if after >= before:
                continue
            events_examined += 1
            # Dynamic actor GUIDs can be introduced by actor opens without a
            # net_guids row. The RPC itself supplies the matching actor GUID;
            # lack of a static path registration does not make it unresolved.
            if not weapon or (weapon in identities and weapon not in stable):
                status["identity_unresolved"] += 1
                continue
            matches = [(effect_time, effect_packet) for effect_time, effect_packet in effects.get(int(weapon), [])
                       if abs(effect_time - time_ms) <= window_ms]
            if not matches:
                status["unmatched"] += 1
            elif len(matches) != 1:
                status["ambiguous_multiple_rpc"] += 1
            else:
                effect_time, effect_packet = matches[0]
                status["corroborated_unique_rpc"] += 1
                offset_ms[effect_time - time_ms] += 1
                if effect_packet == packet_id:
                    status["corroborated_same_packet"] += 1

    # These are always emitted, including zeroes, so a report distinguishes a
    # measured absence from a code path that did not run.
    return {
        "schema_version": 1,
        "source": str(export_dir.resolve()),
        "provenance": {
            "tool_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
            "manifest_sha256": hashlib.sha256((export_dir / "manifest.json").read_bytes()).hexdigest()
                if (export_dir / "manifest.json").is_file() else None,
            "replay_build": json.loads((export_dir / "manifest.json").read_text(encoding="utf-8")).get("replay_build")
                if (export_dir / "manifest.json").is_file() else None,
        },
        "observation": "AmmoComponent.AuthResourceAmount decrease",
        "independent_evidence": {
            "kind": "weapon_scoped_explicit_rpc",
            "field_name": EFFECT_FIELD,
            "group_path_prefix": GUN_PATH_PREFIX,
            "identity_join": "AmmoComponent object outer_net_guid == RPC actor_net_guid",
            "time_window_ms": window_ms,
        },
        "scope_caveats": [
            "The RPC is an explicit gun-scoped continuous-effect invocation, not a labelled shot event.",
            "Ammo replication is sampled; unmatched decreases can be delayed, dropped, or occur without this RPC.",
            "A non-unique time-window match is retained as ambiguous rather than selected by proximity.",
            "The first scalar sample of each magazine stream is left-censored and cannot establish a change.",
            "Conflicting same-packet samples break the transition chain; no decrease is inferred across that gap.",
            "Conflicting NetGUID mappings are excluded; unique RPC matches are per decrease and are not a one-to-one event assignment.",
        ],
        "counts": {
            "magazine_streams": len(magazines),
            "left_censored_magazine_streams": left_censored_streams,
            "weapon_scoped_rpc_rows": sum(len(rows) for rows in effects.values()),
            "ammo_decreases_examined": events_examined,
            "corroborated_unique_rpc": status["corroborated_unique_rpc"],
            "corroborated_same_packet": status["corroborated_same_packet"],
            "unmatched": status["unmatched"],
            "ambiguous_multiple_rpc": status["ambiguous_multiple_rpc"],
            "identity_unresolved": status["identity_unresolved"],
            "ambiguous_ammo_packets": ambiguous_ammo_packets,
            "conflicting_net_guid_mappings": sum(len(options) != 1 for options in identities.values()),
        },
        "unique_rpc_time_offset_ms": {str(key): offset_ms[key] for key in sorted(offset_ms)},
    }


def audit_exports(exports_dir: Path, *, window_ms: int = 300,
                  sample_size: int | None = None, jobs: int = 1) -> dict:
    """Audit direct export children and aggregate only additive counters.

    A sample is evenly spaced through sorted export names, which makes it
    reproducible and prevents a quick check from only describing the earliest
    files in a corpus directory.

    The staging and backup directories `vrfkit export` leaves beside an
    interrupted export are never candidates; they are listed under
    `skipped_generated_dirs` (see `export_scan.py`). A candidate without
    `manifest.json` -- which vrfkit writes last -- is a counted failure, not
    an export.
    """
    records = []
    failures = []
    if not 1 <= jobs <= 16:
        raise ValueError("jobs must be between 1 and 16")
    if window_ms < 0:
        raise ValueError("window_ms must be non-negative")
    children, skipped = child_exports(exports_dir)
    candidate_exports = len(children)
    if not children:
        raise ValueError(f"no export directories found{leftover_note(skipped)}")
    if sample_size is not None:
        if sample_size <= 0:
            raise ValueError("sample_size must be positive")
        if sample_size < len(children):
            indices = {index * (len(children) - 1) // (sample_size - 1)
                       for index in range(sample_size)} if sample_size > 1 else {0}
            children = [child for index, child in enumerate(children) if index in indices]
    def one(child):
        if not (child / "manifest.json").is_file():
            return None, {"export": str(child.resolve()),
                          "error": "manifest.json is missing; vrfkit writes it last, so this is "
                                   "not a finished export (an interrupted export or a partial copy)"}
        try:
            return audit_export(child, window_ms=window_ms), None
        except (OSError, ValueError) as exc:
            return None, {"export": str(child.resolve()), "error": str(exc)}
    with ThreadPoolExecutor(max_workers=jobs) as pool:
        for record, error in pool.map(one, children):
            if error is not None:
                failures.append(error)
            else:
                records.append(record)
    totals = Counter()
    for record in records:
        totals.update(record["counts"])
    return {
        "schema_version": 1,
        "sampling": {
            "method": "all_direct_exports" if sample_size is None else "evenly_spaced_sorted_export_names",
            "requested_sample_size": sample_size,
        },
        "candidate_exports": candidate_exports,
        "skipped_generated_dirs": skipped_report(skipped),
        "exports_scanned": len(records),
        "exports_failed": len(failures),
        "aggregate_counts": {key: totals[key] for key in sorted(totals)},
        "records": records,
        "failures": failures,
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    source = parser.add_mutually_exclusive_group(required=True)
    source.add_argument("--export", type=Path)
    source.add_argument("--exports", type=Path,
                        help="directory whose direct children are exports")
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--window-ms", type=int, default=300)
    parser.add_argument("--jobs", type=int, default=1, help="parallel export scans, 1..16")
    parser.add_argument("--sample-size", type=int, default=None,
                        help="evenly spaced count for --exports; omit for full scan")
    args = parser.parse_args(argv)
    try:
        if args.export and args.sample_size is not None:
            raise ValueError("--sample-size requires --exports")
        result = (audit_export(args.export, window_ms=args.window_ms)
                  if args.export else audit_exports(args.exports, window_ms=args.window_ms,
                                                     sample_size=args.sample_size, jobs=args.jobs))
        atomic_write_text(args.out, json.dumps(result, indent=2, ensure_ascii=True) + "\n")
    except (OSError, ValueError) as exc:
        parser.error(str(exc))
    print(f"wrote {args.out}")
    return int(bool(result.get("exports_failed", 0)))


if __name__ == "__main__":
    raise SystemExit(main())
