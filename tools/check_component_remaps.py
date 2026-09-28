#!/usr/bin/env python3
"""Check that the Blueprint-component remaps still match a build's replays.

`KNOWN_SUBOBJECT_CLASS_PATHS` in `crates/vrfkit/src/sink/paths.rs` maps bare
component instance names to the class groups a replay declares -- usually a
native `/Script/...` class, for three pairs a Blueprint `/Game/..._C` class.
Those pairs were read out of a shipped game
(`tools/extract_component_classes` lists them), and a later build can rename a
component without anything here noticing: the replay never named it either, so
no test can fail on its own.

The export baseline does pin `overlay_no_field_name` and would catch it -- on
the one replay that has a baseline. This works on any export, which is the point:
run it against a replay from a new build before trusting the output.

For each pair it compares the rows still bare under the leaf with the rows
that reached the class group, counting only the kind of block the pair remaps.
For a RepLayout pair that is RepLayout rows: ClassNetCache rows are dropped
(see `CNC_MARKERS`), because every RepLayout remap leaves its component's RPC
stream bare by design. For a ClassNetCache pair it is the other way round: the
leaf's ClassNetCache rows against the class's `_ClassNetCache` group.

    RepLayout pair, no RepLayout row left bare           -> ok      (the remap is doing work)
    RepLayout pair, any RepLayout row left bare          -> broken  (it did not fire for them)
    ClassNetCache pair, no ClassNetCache row left bare   -> ok
    ClassNetCache pair, any ClassNetCache row left bare  -> broken
    neither present                                      -> absent  (not in this replay; says nothing)

A RepLayout pair tolerates nothing because healthy is exactly zero. It used to
be judged by a ratio -- bare rows up to 5% of the target's -- on the grounds
that a leaf lingers beside a working target: on the reference replay
`ZoomStateMachine` keeps 70 rows. All 70 are ClassNetCache rows, which this
check has excluded since, and across 92 exports covering every supported build
(2026-09-28) not one of the 48 RepLayout pairs left a single RepLayout row
bare.

The ratio could not fail where it mattered most. Asking only "does the target
have rows" is not enough, because 26 leaves share
`EquippableStateMachineComponent` -- and 5% of a target that other leaves keep
busy is not enough either. Run against the same 92 replays exported before 29
pairs were added, it read 22 of those pairs `ok` in 773 (pair, replay) cases
where every one of their RepLayout rows was still bare, at up to 4.9% of the
target (`ShieldDamageSection` beside `ChildDamageSectionComponent`). The strict
rule reads those `broken`, and `ok` once the remap is in.

The four ClassNetCache pairs (the C# reference's effect components) are held to
the same zero, on the rows they route. They used to keep the ratio over
RepLayout rows -- the leaf's against the class's RepLayout group -- and neither
of those is what the pairs remap, so the verdict was wrong both ways. Over the
1,018 exports of the 2026-09-28 build audit, `LocationalEffectManager` read
`absent` on all 1,018 and `DamageHandlerComponent` on 1,008 while their
`_ClassNetCache` groups took 119.9M and 72.2M rows; and `DamageHandlerComponent`
read `broken` on the other 10 (12.03, 12.06, three 13.01, 13.02, four 13.05)
over one or two stray RepLayout rows, for a pair that does not remap RepLayout
blocks at all. A scratch build with that pair's target renamed moved 89,843
rows off `DamageableComponent_ClassNetCache` on a 13.05 export and left 4,563
payloads bare under the leaf, and this check's output did not change by a byte.

What a ClassNetCache remap that did not fire leaves is exactly those payloads:
with no function table the block is kept whole under the leaf, as a
`CNC_MARKERS` row. On a healthy export there are none -- 0 in all 4,072
(pair, export) cases of that audit -- so one is `broken`. RepLayout rows under
a ClassNetCache-only leaf are not the pair's to route; they are counted and
printed on every run, never failed on.

Only `DamageHandlerComponent` -> `DamageableComponent` depends on the table
today. The other three are also found by instance name
(`<leaf>Component_ClassNetCache`, `resolve_cnc_for_instance_name`): with all
four targets renamed in a scratch build, a 13.06 export still routed every one
of their rows (16,421 / 34,366 / 34,139) and lost only the damage handler's
17,982. For those three this verdict fires only when a build renames the class
out from under both routes.

What the verdicts DO NOT cover
-----------------------------

A game build renaming a component. This tool's failure text used to claim that
as the likely cause of a `broken` verdict, and a rename cannot produce one: the
replay stops declaring the old leaf altogether, so `bare_rows` is 0 -- which
passes the strict rule and the ratio alike -- and the pair reads `ok` for as
long as anything else keeps the target group busy (26 leaves map to
`EquippableStateMachineComponent`), or `absent` when nothing does. `broken`
means something else: the rows are still arriving under the leaf and are not
reaching the target group.

The renamed component does not leave the export, though. It arrives bare under
its NEW name, which no pair claims. `unmapped_bare_groups` lists exactly those,
worst first, and that list is where a rename is visible. It is reported rather
than failed on: a replay legitimately carries bare Blueprint components that
have no native remap at all, so their presence is not by itself a fault. Read
the list against the previous build's.

Two ways this run can tell you nothing, and they are not the same:
`SKIP` means there was no export to read. `FAILED: nothing checked` means the
export was read and not one pair in the table appears in it -- which is a fault,
because a real match export exercises many of them.

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

#: One `(leaf, class path, GroupKind)` tuple of the remap table, rustfmt'd.
#: The target used to be required to start with `/Script/`, and the first
#: Blueprint-class targets (`/Game/..._C`) were then silently skipped: the
#: table held 52 pairs and this read 49, with nothing reporting the three it
#: could not see. Any absolute path is accepted now, and `unparsed_entries`
#: turns a pair the pattern still cannot read into a failure.
PAIR_RE = re.compile(
    r'\(\s*"([^"]+)",\s*"(/[^"]+)",\s*GroupKind::(\w+)\s*,?\s*\)', re.S
)

#: Share of the target group's rows the bare leaf may still hold, for the
#: ClassNetCache pairs only; a RepLayout pair may hold none (see the module
#: docstring). A renamed component once measured 15.6%.
BARE_SHARE_LIMIT = 0.05

#: Field names that mark a ClassNetCache block rather than a RepLayout property.
#: Two of the remaps are RepLayout-only by design, so their RPC stream stays bare
#: and must not be read as the remap failing. For a ClassNetCache pair these are
#: the rows that count: see `cnc_bare_counts`.
CNC_MARKERS = ("_cnc_h", "__vrfkit_unresolved_class_net_cache_payload__")

#: Suffix of the group a ClassNetCache pair routes its leaf's blocks to: the
#: remap's target class path plus this is what `GroupKind::ClassNetCache`
#: accepts in `sink/paths.rs`.
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
    """Entries in the table that `PAIR_RE` did not read.

    Every entry carries exactly one `GroupKind::` variant, so the count of
    those is the number of pairs the table holds. A difference is a pair this
    check would never look at -- report it rather than check fewer.
    """
    return table.count("GroupKind::") - len(pairs)


def class_net_cache_verdict(leaf, native, rows_by_group, cnc_bare) -> Verdict:
    """A ClassNetCache pair: the leaf's ClassNetCache rows against the rows on
    `<native>_ClassNetCache`, strictly.

    Bare is tested first, and alone decides `broken`: the class group also
    takes blocks that reach it without the table -- 78 rows from 2 objects on
    a 13.01 export whose remap was broken in a scratch build, beside 5,247
    payloads from 108 objects left bare -- so rows on the target do not show
    that the remap fired. `rows_by_group[leaf]` holds the leaf's RepLayout
    rows, which the pair does not route; they go in the detail, never the
    state.
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


def verdicts(pairs, rows_by_group, kinds=None, cnc_bare=None) -> list[Verdict]:
    """Classify each pair against a `{group_path: row count}` map.

    `kinds` maps a leaf to its `GroupKind`. A `RepLayout` pair is judged
    strictly on RepLayout rows; a `ClassNetCache` pair strictly on the
    ClassNetCache rows `cnc_bare` holds for its leaf (see
    `class_net_cache_verdict`); a pair with no kind given by the ratio.
    """
    kinds = kinds or {}
    cnc_bare = cnc_bare or {}
    out = []
    for leaf, native in pairs:
        if kinds.get(leaf) == "ClassNetCache":
            out.append(class_net_cache_verdict(leaf, native, rows_by_group, cnc_bare))
            continue
        native_rows = rows_by_group.get(native, 0)
        bare_rows = rows_by_group.get(leaf, 0)
        if not native_rows and not bare_rows:
            out.append(Verdict(leaf, native, "absent", "not in this replay"))
            continue
        if kinds.get(leaf) == "RepLayout":
            if bare_rows:
                out.append(Verdict(
                    leaf, native, "broken",
                    f"{bare_rows} RepLayout rows still bare ({native_rows} on the "
                    f"class group); a working RepLayout remap leaves none"))
            else:
                out.append(Verdict(leaf, native, "ok",
                                   f"{native_rows} rows, 0 still bare"))
            continue
        share = bare_rows / native_rows if native_rows else float("inf")
        if share > BARE_SHARE_LIMIT:
            detail = (f"{bare_rows} rows still bare against {native_rows} on the "
                      f"native group")
            out.append(Verdict(leaf, native, "broken", detail))
        else:
            out.append(Verdict(
                leaf, native, "ok",
                f"{native_rows} rows, {bare_rows} still bare ({share:.1%})"))
    return out


def bare_counts(fields_by_group) -> dict:
    """Bare rows per group, counting RepLayout blocks only.

    `fields_by_group` maps a group path to a `Counter` of its field names.
    ClassNetCache rows are dropped because the RepLayout-only remaps leave those
    unresolved deliberately -- see `CNC_MARKERS`.
    """
    return {
        group: sum(n for name, n in names.items()
                   if not any(m in (name or "") for m in CNC_MARKERS))
        for group, names in fields_by_group.items()
    }


def cnc_bare_counts(fields_by_group) -> dict:
    """The rows `bare_counts` drops: ClassNetCache rows per bare group.

    What a ClassNetCache pair is judged on -- its component's blocks that did
    not reach the class's `_ClassNetCache` group. Together with `bare_counts`
    this accounts for every row of a bare group exactly once.
    """
    return {
        group: sum(n for name, n in names.items()
                   if any(m in (name or "") for m in CNC_MARKERS))
        for group, names in fields_by_group.items()
    }


def unmapped_bare_groups(rows_by_group, pairs, min_rows: int = 1) -> list:
    """`(group, rows)` for bare groups no pair claims, worst first.

    The rename signal. A build that renames a component makes its old leaf
    vanish from the replay -- which no ratio can see, because a vanished leaf
    holds zero rows -- and introduces a new bare group under the new name that
    the remap table does not know about.

    Native `/Script/...` paths are excluded: they are the targets, not
    candidates for renaming out from under the table. Groups at zero are
    excluded too, which is what keeps the RepLayout-only remaps out of the
    list: `bare_counts` has already dropped their ClassNetCache rows, so they
    arrive here at 0.
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
    """True when no pair appeared in this export, so the run verified nothing.

    Distinct from the `SKIP` path, which is "there was no export to read".
    """
    return all(v.state == "absent" for v in verdicts_)


def row_counts(export_dir: Path) -> tuple[dict, dict]:
    """`({group_path: rows}, {bare group: ClassNetCache rows})`.

    In the first map bare groups count RepLayout blocks only; the second holds
    the ClassNetCache rows that leaves out, for the ClassNetCache pairs.
    """
    import pyarrow.parquet as pq

    table = pq.read_table(export_dir / "fields.parquet",
                          columns=["group_path", "field_name"])
    groups = table.column("group_path").to_pylist()
    names = table.column("field_name").to_pylist()
    totals = collections.Counter(groups)
    by_group = collections.defaultdict(collections.Counter)
    for group, name in zip(groups, names):
        if not group.startswith("/"):
            by_group[group][name] += 1
    out = dict(totals)
    out.update(bare_counts(by_group))
    return out, cnc_bare_counts(by_group)


def class_net_cache_leaf_rep_layout_rows(rows_by_group, kinds) -> int:
    """RepLayout rows under the leaves of ClassNetCache pairs.

    Not a failure -- those pairs do not remap RepLayout blocks -- but a count,
    printed on every run with its zero, so the rows the old ratio called
    `broken` stay visible without failing a healthy export.
    """
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
    results = verdicts(pairs, rows, kinds, cnc_bare)
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

    # Where a RENAME shows up. No verdict above can see one -- see the module
    # docstring -- so the list is printed on every run, pass or fail.
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
