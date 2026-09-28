#!/usr/bin/env python3
"""Check that the Blueprint-component remaps still match a build's replays.

`KNOWN_SUBOBJECT_CLASS_PATHS` in `crates/vrfkit/src/sink/paths.rs` maps bare
component instance names to the class groups a replay declares -- usually a
native `/Script/...` class, for four pairs a Blueprint `/Game/..._C` class.
The pairs were read out of a shipped game (`tools/extract_component_classes`
lists them), and a later build can break one without any test failing. The
export baseline would catch it only on the one replay that has a baseline;
this works on any export, so run it on a replay from a new build before
trusting the output.

Each pair is judged on the kind of block it remaps. A RepLayout pair counts
RepLayout rows only: ClassNetCache rows are dropped (`CNC_MARKERS`), because
every RepLayout remap leaves its component's RPC stream bare by design. A
ClassNetCache pair counts the leaf's ClassNetCache rows against the class's
`_ClassNetCache` group:

    RepLayout pair, no RepLayout row left bare           -> ok      (the remap is doing work)
    RepLayout pair, any RepLayout row left bare          -> broken  (it did not fire for them)
    ClassNetCache pair, no ClassNetCache row left bare   -> ok
    ClassNetCache pair, any ClassNetCache row left bare  -> broken
    neither present                                      -> absent  (not in this replay; says nothing)

Healthy is exactly zero, so nothing is tolerated. Across 92 exports of every
supported build (2026-09-28) none of the 48 RepLayout pairs left a RepLayout
row bare. A ClassNetCache remap that did not fire keeps each block whole under
the leaf as a `CNC_MARKERS` row, and there were 0 of those in all 4,072
(pair, export) cases of the 1,018-export build audit (2026-09-28). RepLayout
rows under a ClassNetCache-only leaf are not the pair's to route: they are
counted and printed on every run, never failed on.

Only `DamageHandlerComponent` -> `DamageableComponent` depends on the table.
The other three ClassNetCache pairs are also routed by instance name
(`<leaf>Component_ClassNetCache`, `resolve_cnc_for_instance_name`): with all
four targets renamed in a scratch build, a 13.06 export still routed every row
of those three (16,421 / 34,366 / 34,139) and lost only the damage handler's
17,982. For them a verdict fires only when a build renames the class out from
under both routes.

A renamed component cannot produce `broken`: the replay stops declaring the old
leaf, so it holds no bare rows and the pair reads `ok` while other leaves keep
the target busy (26 map to `EquippableStateMachineComponent`), or `absent`.
`broken` means rows still arrive under the leaf and do not reach the target.
A rename appears under its NEW name as a bare group no pair claims:
`unmapped_bare_groups` lists those, worst first, reported rather than failed
on (a replay legitimately carries bare Blueprint components with no remap).
Read the list against the previous build's.

`SKIP` means there was no export to read; `FAILED: nothing checked` means the
export held not one pair of the table, a fault, since a real match exercises
many of them.

Usage:
    python tools/check_component_remaps.py --export out/probe
"""

from __future__ import annotations

import argparse
import collections
import re
import sys
from pathlib import Path
from typing import NamedTuple

REPO = Path(__file__).resolve().parents[1]
PATHS_RS = REPO / "crates" / "vrfkit" / "src" / "sink" / "paths.rs"

#: One `(leaf, class path, GroupKind)` tuple of the remap table, rustfmt'd. Any
#: absolute target path is read (`/Script/...` and Blueprint `/Game/..._C`);
#: `unparsed_entries` turns a pair the pattern cannot read into a failure.
PAIR_RE = re.compile(
    r'\(\s*"([^"]+)",\s*"(/[^"]+)",\s*GroupKind::(\w+)\s*,?\s*\)', re.S
)

#: Field names that mark a ClassNetCache block rather than a RepLayout property.
#: A RepLayout remap leaves its RPC stream bare by design, so these rows must not
#: read as it failing; for a ClassNetCache pair they are the rows that count.
CNC_MARKERS = ("_cnc_h", "__vrfkit_unresolved_class_net_cache_payload__")

#: Suffix of the group a ClassNetCache pair routes its leaf's blocks to (the
#: target class path plus this, as `GroupKind::ClassNetCache` in paths.rs).
CNC_GROUP_SUFFIX = "_ClassNetCache"


class Verdict(NamedTuple):
    leaf: str
    native: str
    state: str
    detail: str


def table_source() -> str:
    """The body of the Rust remap table, as text."""
    src = PATHS_RS.read_text(encoding="utf-8")
    start = src.index("const KNOWN_SUBOBJECT_CLASS_PATHS")
    end = src.index("\n];", start)
    return src[start:end]


def remap_entries(table: str | None = None) -> list[tuple[str, str, str]]:
    """`(leaf, class path, GroupKind)` for every entry of the Rust table."""
    return PAIR_RE.findall(table_source() if table is None else table)


def remap_pairs(table: str | None = None) -> list[tuple[str, str]]:
    """The `(leaf, class path)` pairs, read from the Rust table itself."""
    return [(leaf, target) for leaf, target, _ in remap_entries(table)]


def remap_kinds(table: str | None = None) -> dict[str, str]:
    """`{leaf: "RepLayout" | "ClassNetCache"}`, from the same table."""
    return {leaf: kind for leaf, _, kind in remap_entries(table)}


def unparsed_entries(table: str, pairs) -> int:
    """Entries in the table that `PAIR_RE` did not read: every entry carries
    exactly one `GroupKind::` variant, so a difference is a pair this check
    would never look at."""
    return table.count("GroupKind::") - len(pairs)


def class_net_cache_verdict(leaf, native, rows_by_group, cnc_bare) -> Verdict:
    """A ClassNetCache pair: the leaf's ClassNetCache rows against the rows on
    `<native>_ClassNetCache`, strictly.

    Bare is tested first and alone decides `broken`: the class group also
    takes blocks that reach it without the table (78 rows from 2 objects on a
    13.01 export whose remap was broken in a scratch build, beside 5,247
    payloads from 108 objects left bare). The leaf's RepLayout rows, which the
    pair does not route, go in the detail, never the state.
    """
    routed = rows_by_group.get(native + CNC_GROUP_SUFFIX, 0)
    bare = cnc_bare.get(leaf, 0)
    rep_layout = rows_by_group.get(leaf, 0)
    note = (f"; {rep_layout} RepLayout rows under the leaf, which a ClassNetCache "
            f"pair does not remap" if rep_layout else "")
    if bare:
        return Verdict(
            leaf, native, "broken",
            f"{bare} ClassNetCache rows still bare ({routed} on the class's "
            f"{CNC_GROUP_SUFFIX} group); a working ClassNetCache remap leaves none"
            + note)
    if routed:
        return Verdict(leaf, native, "ok",
                       f"{routed} rows on the {CNC_GROUP_SUFFIX} group, 0 still bare"
                       + note)
    return Verdict(leaf, native, "absent",
                   "no ClassNetCache rows in this replay" + note)


def verdicts(pairs, rows_by_group, kinds, cnc_bare=None) -> list[Verdict]:
    """Classify each pair against a `{group_path: row count}` map.

    `kinds` maps a leaf to its `GroupKind`: a `RepLayout` pair is judged on
    RepLayout rows, a `ClassNetCache` pair on `cnc_bare` (see
    `class_net_cache_verdict`). Any other kind, or none, raises ValueError, so
    a `GroupKind` added in paths.rs cannot pass its pairs unjudged.
    """
    cnc_bare = cnc_bare or {}
    out = []
    for leaf, native in pairs:
        kind = kinds.get(leaf)
        if kind == "ClassNetCache":
            out.append(class_net_cache_verdict(leaf, native, rows_by_group, cnc_bare))
            continue
        if kind != "RepLayout":
            raise ValueError(f"{leaf} has GroupKind {kind!r}, which no verdict rule covers")
        native_rows = rows_by_group.get(native, 0)
        bare_rows = rows_by_group.get(leaf, 0)
        if not native_rows and not bare_rows:
            out.append(Verdict(leaf, native, "absent", "not in this replay"))
        elif bare_rows:
            out.append(Verdict(
                leaf, native, "broken",
                f"{bare_rows} RepLayout rows still bare ({native_rows} on the "
                f"class group); a working RepLayout remap leaves none"))
        else:
            out.append(Verdict(leaf, native, "ok", f"{native_rows} rows, 0 still bare"))
    return out


def unmapped_bare_groups(rows_by_group, pairs, min_rows: int = 1) -> list:
    """`(group, rows)` for bare groups no pair claims, worst first: where a
    renamed component appears. Native `/Script/...` paths are the targets,
    so they are excluded; so are groups at zero, which keeps the RepLayout-only
    remaps out (`row_counts` already dropped their ClassNetCache rows).
    """
    leaves = {leaf for leaf, _ in pairs}
    return sorted(
        ((group, rows) for group, rows in rows_by_group.items()
         if not group.startswith("/") and group not in leaves and rows >= min_rows),
        key=lambda kv: (-kv[1], kv[0]),
    )


def exit_code(verdicts_) -> int:
    return 1 if any(v.state == "broken" for v in verdicts_) else 0


def nothing_checked(verdicts_) -> bool:
    """True when no pair appeared in this export, so the run verified nothing
    (unlike `SKIP`, which had no export to read)."""
    return all(v.state == "absent" for v in verdicts_)


def row_counts(export_dir: Path) -> tuple[dict, dict]:
    """`({group_path: rows}, {bare group: ClassNetCache rows})`.

    A native (`/`-rooted) group counts every row. A bare group counts its
    RepLayout rows in the first map and its ClassNetCache rows (`CNC_MARKERS`)
    in the second, so each row counts exactly once. Grouped in Arrow: on the
    13.01 reference export (1,296,660 rows) 0.16 s where a Python loop over
    the rows took 4.2 (best of 5, 2026-09-28).
    """
    import pyarrow as pa
    import pyarrow.compute as pc
    import pyarrow.parquet as pq

    table = pq.read_table(export_dir / "fields.parquet",
                          columns=["group_path", "field_name"])
    names = pc.fill_null(table.column("field_name").cast(pa.string()), "")
    is_cnc = pc.match_substring(names, CNC_MARKERS[0])
    for marker in CNC_MARKERS[1:]:
        is_cnc = pc.or_(is_cnc, pc.match_substring(names, marker))
    grouped = pa.table({
        "group": table.column("group_path").cast(pa.string()), "cnc": is_cnc,
    }).group_by("group").aggregate([([], "count_all"), ("cnc", "sum")])
    out, cnc_bare = {}, {}
    for group, rows, cnc_rows in zip(grouped["group"].to_pylist(),
                                     grouped["count_all"].to_pylist(),
                                     grouped["cnc_sum"].to_pylist()):
        if group.startswith("/"):
            out[group] = rows
        else:
            out[group] = rows - cnc_rows
            cnc_bare[group] = cnc_rows
    return out, cnc_bare


def class_net_cache_leaf_rep_layout_rows(rows_by_group, kinds) -> int:
    """RepLayout rows under the leaves of ClassNetCache pairs: not a failure
    (those pairs do not remap RepLayout blocks), but printed on every run,
    zero included, so the rows stay visible."""
    return sum(rows_by_group.get(leaf, 0)
               for leaf, kind in kinds.items() if kind == "ClassNetCache")


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--export", type=Path, required=True,
                    help="directory written by `vrfkit export`")
    ap.add_argument("--verbose", action="store_true",
                    help="list every pair, not just the problems")
    args = ap.parse_args()

    fields = args.export / "fields.parquet"
    if not fields.is_file():
        print(f"SKIP: no fields.parquet in {args.export}", file=sys.stderr)
        return 0

    table = table_source()
    pairs = remap_pairs(table)
    if not pairs:
        print(f"FAILED: parsed no pairs out of {PATHS_RS.name}", file=sys.stderr)
        return 1
    missing = unparsed_entries(table, pairs)
    if missing:
        print(f"FAILED: {missing} entr{'y' if missing == 1 else 'ies'} of the "
              f"remap table in {PATHS_RS.name} did not parse, so nothing here "
              f"would check them", file=sys.stderr)
        return 1

    rows, cnc_bare = row_counts(args.export)
    kinds = remap_kinds(table)
    try:
        results = verdicts(pairs, rows, kinds, cnc_bare)
    except ValueError as exc:
        print(f"FAILED: {exc}", file=sys.stderr)
        return 1
    tally = collections.Counter(v.state for v in results)

    for v in results:
        if v.state == "broken" or args.verbose:
            print(f"  {v.state:<7} {v.leaf} -> {v.native.split('.')[-1]}: {v.detail}")

    print(f"{len(pairs)} remaps: {tally['ok']} ok, {tally['absent']} absent, "
          f"{tally['broken']} broken")
    # Unconditional, zero included: see class_net_cache_leaf_rep_layout_rows.
    print(f"{class_net_cache_leaf_rep_layout_rows(rows, kinds)} RepLayout row(s) "
          f"under the leaves of the ClassNetCache pairs, which do not remap "
          f"RepLayout blocks (reported, not a failure)")

    # Where a rename shows up (no verdict can see one), printed pass or fail.
    suspects = unmapped_bare_groups(rows, pairs)
    print(f"\n{len(suspects)} bare group(s) no remap claims"
          + (" (a renamed component appears here, not above):" if suspects else ""))
    for group, n in suspects[:10]:
        print(f"  {n:>8,}  {group}")
    if len(suspects) > 10:
        print(f"  ... and {len(suspects) - 10} more")

    if tally["broken"]:
        print("\nFAILED: a remap stopped matching -- its rows are still "
              "arriving under the bare leaf instead of reaching the native "
              "group. Check that the pair still spells the leaf the way this "
              "replay declares it; if the component was RENAMED the leaf would "
              "be gone from the export entirely and would show up in the "
              "unclaimed list above, not here. Re-derive the pair from the "
              "cooked asset (docs/DATA.md) rather than guessing a new name.",
              file=sys.stderr)
        return 1
    if nothing_checked(results):
        print(f"\nFAILED: nothing checked -- not one of the {len(pairs)} remap "
              f"pairs appears in this export. A real match exercises many of "
              f"them, so this is an empty or wrong export rather than a clean "
              f"result.", file=sys.stderr)
        return 1
    print(f"\nOK: {tally['ok']} remap(s) are doing work; {tally['absent']} do "
          f"not appear in this replay, which says nothing about them. A "
          f"renamed component is not covered by this verdict -- read the "
          f"unclaimed bare groups above against the previous build.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
