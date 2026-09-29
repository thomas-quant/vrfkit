"""Compare CombatReport values as multisets per path shape.

A leaf-by-leaf path join under-counts: C# writes array positions into its own
`Index` field while we encode the wire index in the path brackets, so two records
carrying identical data can spell their paths differently. Comparing the multiset
of values for each *shape* (indices collapsed) sidesteps that and still catches a
wrong decoder -- a bit-level error would change the values themselves, not just
their addresses.

Restricted to the fields valplay actually derives metrics from.

`fields.parquet` labels each array leaf with the name the REPLAY declares for
its handle, which for six of the ten shapes below is not the C# reference's:
the wire says `DamageRecieved` and `HitsRecieved` (Riot's typos), `bDidKill`,
`bIsWallPen`, and `ParticipantSubject`. So our side is relabelled through the
bundle adapter's own handle -> reference-name table before the shapes are
compared; sharing it is deliberate, since a second copy could drift and this
comparison would quietly stop testing anything. `INTERESTING` stays in the C#
spelling, as the reference's own `events.ndjson` has it.

The reference is the C# reference parser's `export` of the 13.01 reference
replay, kept machine-local because it carries per-player values; docs/USAGE.md
section 6 has the commands that produce it.

Usage:
    python tools/compare_combat_report.py [--reference EVENTS] [--ours PARQUET]
"""

import argparse
import collections
import json
import os
import sys
from pathlib import Path

import pyarrow as pa
import pyarrow.compute as pc
import pyarrow.parquet as pq

sys.path.insert(0, str(Path(__file__).resolve().parent))
from to_valplay_bundle import _combat_report_leaf_name  # noqa: E402

#: The CombatReport lines of the C# reference parser's export of replay
#: 02d4d478. The export must come from a C# build that binds its
#: CombatRoundReportsDecoder to Rounds; a build that declares Rounds a raw
#: payload has no values to compare. The whole events.ndjson works too; only
#: lines naming CombatReportComponent are read.
DEFAULT_REFERENCE = (r"%LOCALAPPDATA%\vrfkit\csharp-reference"
                     r"\02d4d478-1dfb-4412-9a77-29ca29105a9d\combat_report.ndjson")
#: vrfkit's export of the same replay.
DEFAULT_OURS = "out/nested/fields.parquet"

# The leaves that drive K/D/A, ADR, HS%, multikills and wallbangs.
INTERESTING = {
    "Rounds[].Reports[].Interactions[].DamageDealt",
    "Rounds[].Reports[].Interactions[].HitsDealt",
    "Rounds[].Reports[].Interactions[].DamageReceived",
    "Rounds[].Reports[].Interactions[].HitsReceived",
    "Rounds[].Reports[].Interactions[].DidKill",
    "Rounds[].Reports[].Interactions[].AssistType",
    "Rounds[].Reports[].Interactions[].DealtInteractions[].Regions[].Hits",
    "Rounds[].Reports[].Interactions[].DealtInteractions[].Regions[].Damage",
    "Rounds[].Reports[].Interactions[].ReceivedInteractions[].Regions[].Hits",
    "Rounds[].Reports[].Interactions[].ReceivedInteractions[].Regions[].Damage",
}


def flatten(prefix, node, out):
    if isinstance(node, list):
        for i, item in enumerate(node):
            flatten(f"{prefix}[{i}]", item, out)
    elif isinstance(node, dict):
        for k, v in node.items():
            flatten(f"{prefix}.{k}" if prefix else k, v, out)
    else:
        out[prefix] = node


def shape(path):
    out, i = [], 0
    while i < len(path):
        if path[i] == "[":
            out.append("[]")
            i = path.index("]", i) + 1
        else:
            out.append(path[i])
            i += 1
    return "".join(out)


#: Decimal places floats are rounded to before the multisets are compared:
#: `IDENTICAL multiset` means identical to this precision and no further.
FLOAT_PLACES = 3


def norm(v):
    """Normalise so 1/True and 35.0/35 compare equal across JSON and Parquet."""
    if isinstance(v, bool):
        return int(v)
    if isinstance(v, float):
        return round(v, FLOAT_PLACES)
    if isinstance(v, int):
        return v
    return str(v)


def load_cs(path):
    cs = collections.defaultdict(collections.Counter)
    with path.open("rb") as f:
        for line in f:
            if b"CombatReportComponent" not in line:
                continue
            o = json.loads(line)
            rounds = (o.get("payload") or {}).get("Rounds")
            if not rounds:
                continue
            leaves = {}
            flatten("Rounds", rounds, leaves)
            for p, v in leaves.items():
                s = shape(p)
                if s in INTERESTING and v is not None:
                    cs[s][norm(v)] += 1
    return cs


def load_ours(parquet=DEFAULT_OURS):
    columns = ["group_path", "field_name", "handle",
               "value_i64", "value_f64", "value_bool", "value_str"]
    t = pq.read_table(parquet, columns=columns)
    # Only the CombatReport Rounds rows become Python objects (of ~1.3M).
    keep = pc.and_(
        pc.match_substring(t.column("group_path").cast(pa.string()), "CombatReportComponent"),
        pc.starts_with(t.column("field_name").cast(pa.string()), "Rounds"))
    t = t.filter(pc.fill_null(keep, False))
    cols = {n: t.column(n).to_pylist() for n in columns}
    ours = collections.defaultdict(collections.Counter)
    for i, (g, n) in enumerate(zip(cols["group_path"], cols["field_name"])):
        s = shape(_combat_report_leaf_name(g, n, cols["handle"][i]))
        if s not in INTERESTING:
            continue
        for c in ("value_i64", "value_f64", "value_bool", "value_str"):
            v = cols[c][i]
            if v is not None:
                ours[s][norm(v)] += 1
                break
    return ours


def compare(cs, ours, interesting):
    """`(printable rows, everything matched)`."""
    rows, all_match = [], True
    for s in sorted(interesting):
        a, b = cs.get(s, collections.Counter()), ours.get(s, collections.Counter())
        # Emptiness first, because empty Counters are ==; `all_match` stays
        # True (no disagreement), and `compared_shapes` says it was no comparison.
        if not a and not b:
            verdict = "absent both sides"
        elif a == b:
            verdict = "IDENTICAL multiset"
        else:
            extra_ours = sum((b - a).values())
            extra_cs = sum((a - b).values())
            verdict = f"DIFFER (+{extra_ours} ours / +{extra_cs} C#)"
            all_match = False
        label = s.replace("Rounds[].Reports[].Interactions[]", "..Interactions[]")
        rows.append(f"{label:<66} {sum(a.values()):>7,} {sum(b.values()):>7,}  "
                    f"{verdict}")
    return rows, all_match


def compared_shapes(cs, ours, interesting) -> int:
    """How many of the interesting shapes actually had something to compare:
    without it a run that compared nothing (a wrong parquet path or reference,
    a CombatReport decoder that stopped emitting) would read as a match."""
    return sum(1 for s in interesting
               if cs.get(s, collections.Counter()) or ours.get(s, collections.Counter()))


def parse_args(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--reference", default=DEFAULT_REFERENCE,
                        help="C# export events.ndjson, or its CombatReport lines "
                             "(default: %(default)s)")
    parser.add_argument("--ours", default=DEFAULT_OURS,
                        help="vrfkit fields.parquet of the same replay "
                             "(default: %(default)s)")
    return parser.parse_args(argv)


def main(cs=None, ours=None, interesting=None, argv=None):
    """Exit 0 only if every interesting shape matches and every one was there;
    1 when the CombatReport decoder disagrees with the C# reference on values;
    2 when a shape carried nothing on either side and so was not compared
    (every shape is present in the reference replay).
    """
    if cs is None or ours is None:
        args = parse_args(argv)
    if cs is None:
        reference = Path(os.path.expandvars(args.reference))
        if not reference.is_file():
            print(f"C# reference not found at {reference}; produce it with the "
                  f"commands in docs/USAGE.md section 6, or pass --reference",
                  file=sys.stderr)
            return 2
        cs = load_cs(reference)
    if ours is None:
        if not Path(args.ours).is_file():
            print(f"vrfkit fields.parquet not found at {args.ours}; export the "
                  f"same replay, or pass --ours", file=sys.stderr)
            return 2
        ours = load_ours(args.ours)

    shapes = interesting or INTERESTING
    rows, all_match = compare(cs, ours, shapes)
    checked = compared_shapes(cs, ours, shapes)
    print(f"{'shape':<66} {'C#':>7} {'ours':>7}  verdict")
    print("-" * 96)
    for row in rows:
        print(row)
    print()
    if not all_match:
        print("SOME SHAPES DIFFER -- see above")
        return 1
    if checked < len(shapes):
        print(f"INCOMPLETE: {len(shapes) - checked} of the {len(shapes)} "
              f"interesting shapes carry no value on either side, so they were "
              f"not compared. This is not agreement -- check the parquet path "
              f"and the reference.")
        return 2
    print(f"ALL {checked} INTERESTING SHAPES MATCH "
          f"(values to {FLOAT_PLACES} decimal places)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(argv=sys.argv[1:]))
