#!/usr/bin/env python3
"""Derive a table of persistent ability effects from an export.

Persistent abilities (smokes, walls, slows, traps, molly/decay zones, recon
bolts, ult orbs) spawn an actor with a class, a spawn location and an
open/close lifetime, all in actors.parquet. This filters the effect actors,
pairs each open with its close into a lifetime, classifies the effect and
writes one row per effect instance.

`spawn_x/y/z` is the spawn transform: a placed effect's world location. For
the few that relocate, fields.parquet carries the live `ReplicatedMovement`
location or `MulticastAddSmokeScreenPoint.Translation`, in the same units.
(Exports before 2026-09-28 wrote that location 100x too small on every class
but one; see docs/DATA.md, "`ReplicatedMovement.location` is world units, at
a per-class level".)
"""

from __future__ import annotations

import argparse
import re
from collections import Counter
from pathlib import Path

import pyarrow as pa
import pyarrow.parquet as pq

if __package__:
    from .atomic_io import atomic_write_file
else:
    from atomic_io import atomic_write_file

# Substrings marking a persistent ability effect, matched case-insensitively
# on the full class_path. Broad on purpose (a missed effect just does not
# appear), but a false positive is not harmless. On the 1,018-export corpus
# (parser 259ed10, 2026-09-28; actors.parquet opens) three names matched a
# keyword they do not mean, each handled below:
#   Gun_Deadeye_X_Giantslayer_Prototype_FIreRatePrototype -- Chamber's ult gun,
#     "fire" in "FIreRate": 17,304 of 32,714 damage_zone rows, median lifetime
#     ~100 s. An equippable: `gun_` leaves are excluded.
#   Projectile_Breach_Q_ThroughWalls_Flash -- "wall" in "ThroughWalls": 1,416
#     rows filed as walls. A flash projectile (no other is in this table;
#     Vyse's placed flash trap is, as a trap). See NOT_EFFECT_TOKENS.
#   GameObject_Sarge_X_OrbitalStrike_Production -- "orb" in "OrbitalStrike":
#     174 rows filed as orbs. Brimstone's ult is 4-9 s of area damage, so
#     `classify` files it as a damage_zone.
# Every other class kept its type across the corpus when these three changed.
EFFECT_KEYWORDS = (
    "smoke",
    "smokezone",
    "wall",
    "barrier",
    "toxicscreen",
    "slowfield",
    "slow",
    "trap",
    "cage",
    "molotov",
    "fire",
    "decayexplosion",
    "decaynade",
    "orb",
    "drone",
    "scout",
    "recon",
    "nanoswarm",
    "alarmbot",
)

# Fragments that contain a keyword without naming an effect, removed before
# matching: "ThroughWall" is Breach's flash's whole claim to "wall", while
# Phoenix's `FlameWall_ThroughWall` stays a wall through "FlameWall".
NOT_EFFECT_TOKENS = ("throughwall",)

#: Every `effect_type` value, in the order the summary prints them.
EFFECT_TYPES = ("smoke", "wall", "slow", "trap", "damage_zone", "orb", "recon", "other")


def _keyword_text(class_path: str) -> str:
    """The lower-cased class path with `NOT_EFFECT_TOKENS` removed."""
    c = class_path.lower()
    for token in NOT_EFFECT_TOKENS:
        c = c.replace(token, "")
    return c


# A class's coarse effect family. Order matters: "SlowField" is a slow, and
# "OrbitalStrike" a damage zone before "orb" can claim it.
def classify(class_path: str) -> str:
    c = _keyword_text(class_path)
    if "smoke" in c or "smokezone" in c:
        return "smoke"
    if "wall" in c or "barrier" in c or "toxicscreen" in c:
        return "wall"
    if "slow" in c:
        return "slow"
    if "trap" in c or "cage" in c:
        return "trap"
    if ("molotov" in c or "fire" in c or "decay" in c or "nanoswarm" in c
            or "orbitalstrike" in c):
        return "damage_zone"
    if "orb" in c:
        return "orb"
    if "drone" in c or "scout" in c or "recon" in c or "alarmbot" in c:
        return "recon"
    return "other"


#: Leaf-name prefix -> `actor_kind`. A projectile and the zone it places are
#: two rows by design: on 0002c486 an Omen smoke is a `Projectile_Wraith_4_Smoke`
#: (median 2.3 s) overlapping a `Zone_Wraith_4_Smoke` (median 16 s). The kind
#: lets a consumer count either.
ACTOR_KINDS = {"projectile": "projectile", "gameobject": "game_object",
               "zone": "zone", "patch": "patch", "pawn": "pawn"}
#: Every `actor_kind` value, in the order the summary prints them.
ACTOR_KIND_ORDER = ("projectile", "game_object", "zone", "patch", "pawn", "other")


def actor_kind(class_path: str) -> str:
    """The leaf's first `_`-separated token as a kind; `other` if unlisted."""
    leaf = class_path.rsplit("/", 1)[-1].split(".", 1)[0]
    return ACTOR_KINDS.get(leaf.split("_", 1)[0].lower(), "other")


# The internal agent codename from /Game/Characters/<name>/ (Sarge = Brimstone,
# Smonk = Clove, Pandemic = Viper, ...), left as-is: display names are
# equippable_table.py's.
AGENT_RE = re.compile(r"/Game/Characters/(\w+)/")


def agent_codename(class_path: str) -> str:
    m = AGENT_RE.search(class_path)
    return m.group(1) if m else ""


def is_effect_class(class_path: str) -> bool:
    if not class_path:
        return False
    c = _keyword_text(class_path)
    if not any(k in c for k in EFFECT_KEYWORDS):
        return False
    # "Ability_" leaves are the ability controllers, alive all match; the
    # effect is the GameObject_/Projectile_/Patch_ actor they spawn. "Gun_"
    # leaves are weapons (Chamber's ult gun, see EFFECT_KEYWORDS).
    leaf = class_path.rsplit("/", 1)[-1].lower()
    if leaf.startswith(("ability_", "gun_")):
        return False
    return True


def build_with_tally(out_dir: Path) -> tuple[list[dict], dict]:
    actors_path = out_dir / "actors.parquet"
    if not actors_path.exists():
        raise SystemExit(f"no actors.parquet in {out_dir} -- run `vrfkit export` first")

    table = pq.read_table(actors_path)
    cols = {c: table.column(c).to_pylist() for c in table.column_names}
    guid = cols["actor_net_guid"]
    event = cols["event"]
    time_ms = cols["time_ms"]
    class_path = cols["class_path"]
    sx = cols["spawn_x"]
    sy = cols["spawn_y"]
    sz = cols["spawn_z"]

    # Pair each open with the close that follows it, never first open to last
    # close. On the 1,018-export corpus (parser 259ed10, 2026-09-28; every
    # actors.parquet `open`, all classes) no GUID reopens: 2,326,969 opens, 0
    # opened twice in one export. The pairing stays for builds that might.
    events: dict[int, list[tuple]] = {}
    for i in range(len(guid)):
        cp = class_path[i]
        if not is_effect_class(cp):
            continue
        events.setdefault(guid[i], []).append(
            (time_ms[i], event[i], sx[i], sy[i], sz[i], cp)
        )

    # `went_dormant` counts INSTANCES that saw at least one `dormant` event,
    # once each, however they end: it is not a share of the open-ended rows
    # and can exceed them.
    tally = {"went_dormant": 0}
    rows: list[dict] = []
    for g, evs in events.items():
        evs.sort(key=lambda e: e[0])
        pending = None  # (open_ms, sx, sy, sz, class_path) of the current open instance
        pending_went_dormant = False  # did THIS instance see a dormant event
        for t, ev, x, y, z, cp in evs:
            if ev == "open":
                if pending is not None:
                    # Reopened before closing: the prior instance never closed
                    # in this export. Emit it open-ended so it is not lost.
                    rows.append(_row(g, pending, None))
                pending = (t, x, y, z, cp)
                pending_went_dormant = False
            elif ev == "close":
                if pending is not None:
                    rows.append(_row(g, pending, t))
                    pending = None
                    pending_went_dormant = False
                # A close with no pending open is an orphan (actor opened before
                # the export window); drop it rather than invent an open time.
            elif ev == "dormant":
                # Dormancy is NOT destruction: a settled smoke or wall stops
                # replicating as its steady state. The instance stays pending,
                # open-ended absent a later close; since that row looks like
                # one the export window cut off, the tally counts it.
                if pending is not None and not pending_went_dormant:
                    pending_went_dormant = True
                    tally["went_dormant"] += 1
        if pending is not None:
            rows.append(_row(g, pending, None))

    rows.sort(key=lambda r: (r["open_ms"], r["actor_net_guid"]))
    return rows, tally


def _row(guid: int, open_rec: tuple, close_ms):
    open_ms, x, y, z, cp = open_rec
    return {
        "actor_net_guid": guid,
        "class_path": cp,
        "effect_type": classify(cp),
        "actor_kind": actor_kind(cp),
        "agent": agent_codename(cp),
        "spawn_x": x,
        "spawn_y": y,
        "spawn_z": z,
        "open_ms": open_ms,
        "close_ms": close_ms,
        "duration_ms": (close_ms - open_ms) if close_ms is not None else None,
    }


SCHEMA = pa.schema([
    pa.field("actor_net_guid", pa.int32()),
    pa.field("class_path", pa.string()),
    pa.field("effect_type", pa.string()),
    pa.field("actor_kind", pa.string()),
    pa.field("agent", pa.string()),
    pa.field("spawn_x", pa.float32()),
    pa.field("spawn_y", pa.float32()),
    pa.field("spawn_z", pa.float32()),
    pa.field("open_ms", pa.int64()),
    pa.field("close_ms", pa.int64()),
    pa.field("duration_ms", pa.int64()),
])


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--export", type=Path, required=True,
                    help="directory written by `vrfkit export` (must hold actors.parquet)")
    ap.add_argument("--out", type=Path, required=True,
                    help="output active_effects.parquet path")
    args = ap.parse_args()

    rows, tally = build_with_tally(args.export)
    cols = {name: [r[name] for r in rows] for name in SCHEMA.names}
    table = pa.Table.from_pydict(cols, schema=SCHEMA)
    atomic_write_file(args.out, lambda out: pq.write_table(table, out, compression="zstd"))

    by_type = Counter(r["effect_type"] for r in rows)
    by_kind = Counter(r["actor_kind"] for r in rows)
    print(f"wrote {args.out} ({len(rows)} effect instances)")
    # Every type and kind is printed, zeros included: a family that stopped
    # matching must read as 0, not as a line that is no longer there.
    for t in EFFECT_TYPES:
        print(f"  {t:12s} {by_type[t]}")
    print("  by actor kind (class leaf prefix):")
    for k in ACTOR_KIND_ORDER:
        print(f"    {k:12s} {by_kind[k]}")
    # Printed with its zero, on its own line: went_dormant is not a share of
    # open_ended (see build_with_tally).
    open_ended = sum(1 for r in rows if r["close_ms"] is None)
    print(f"  {'open-ended':12s} {open_ended}")
    print(f"  {'':12s} ({tally['went_dormant']} instance(s) went dormant at "
          f"some point, open-ended or not)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
