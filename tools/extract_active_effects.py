#!/usr/bin/env python3
"""Derive a table of persistent ability effects from an export.

VALORANT's persistent abilities -- smokes, walls, slows, traps, molly/decay
zones, recon bolts, ult orbs -- spawn an actor with a known class, a spawn
location, and an open/close lifetime. `actors.parquet` already carries all of
that; this script filters the effect actors out, pairs each actor's open and
close events into a lifetime, classifies the effect, and writes one row per
effect instance.

This is a *derived* view over the raw export, not a new wire decode: the data
is already in `actors.parquet` (class + spawn xyz + open/close). vrfkit itself
exports raw tables; analytical joins live here so the parser stays focused.

Position note: `spawn_x/y/z` are the actor's spawn transform, which for a
placed effect (smoke, wall segment, trap) is its world location. For the few
effects that relocate, `fields.parquet` carries the live `ReplicatedMovement`
location or `MulticastAddSmokeScreenPoint.Translation`, both in the same world
units as the spawn. (Exports made before 2026-09-28 wrote that location 100x
too small on every class but one; see docs/DATA.md, "`ReplicatedMovement.location`
is world units, at a per-class level".)

Usage:
    python tools/extract_active_effects.py --export <out_dir> --out active_effects.parquet
"""

from __future__ import annotations

import argparse
import re
from collections import Counter
from pathlib import Path

import pyarrow as pa
import pyarrow.parquet as pq

# Substrings that mark a class as a persistent ability effect. Matched
# case-insensitively against the full class_path. The list is broad on
# purpose -- a missed effect simply does not appear -- but a false positive is
# not harmless, and this file used to say it was ("a short-lived actor whose
# open/close still reads sensibly"). Measured over the 1,018-export audit
# corpus (parser 259ed10, 2026-09-28; actors.parquet opens), three names
# matched a keyword they do not mean, each handled explicitly below:
#   Gun_Deadeye_X_Giantslayer_Prototype_FIreRatePrototype -- Chamber's ult gun,
#     "fire" in "FIreRate": 17,304 of 32,714 damage_zone rows, median lifetime
#     ~100 s. An equippable, not an effect: `gun_` leaves are excluded.
#   Projectile_Breach_Q_ThroughWalls_Flash -- "wall" in "ThroughWalls": 1,416
#     rows filed as walls. A flash projectile, not a persistent effect; no
#     other flash projectile is in this table (Vyse's placed flash trap is, as
#     a trap). See NOT_EFFECT_TOKENS.
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

# Name fragments that contain a keyword without naming an effect, removed
# before any keyword is matched. "ThroughWall" describes a projectile that
# passes through walls: it is the whole of Breach's flash's claim to "wall",
# while Phoenix's `FlameWall_ThroughWall` stays a wall through "FlameWall".
NOT_EFFECT_TOKENS = ("throughwall",)

#: Every `effect_type` value, in the order the summary prints them.
EFFECT_TYPES = ("smoke", "wall", "slow", "trap", "damage_zone", "orb", "recon", "other")


def _keyword_text(class_path: str) -> str:
    """The lower-cased class path with `NOT_EFFECT_TOKENS` removed."""
    c = class_path.lower()
    for token in NOT_EFFECT_TOKENS:
        c = c.replace(token, "")
    return c


# Map a class to a coarse effect family. Order matters: check the more
# specific tokens first so "SlowField" is a slow, not a field, and so
# "OrbitalStrike" is a damage zone before "orb" can claim it.
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


#: Leaf-name prefix -> `actor_kind`. The table keeps a projectile and the zone
#: it places as two rows, deliberately (see `is_effect_class`): on 0002c486 an
#: Omen smoke is a `Projectile_Wraith_4_Smoke` (median 2.3 s) overlapping a
#: `Zone_Wraith_4_Smoke` (median 16 s). The kind lets a consumer count either
#: without this tool choosing for it.
ACTOR_KINDS = {"projectile": "projectile", "gameobject": "game_object",
               "zone": "zone", "patch": "patch", "pawn": "pawn"}
#: Every `actor_kind` value, in the order the summary prints them.
ACTOR_KIND_ORDER = ("projectile", "game_object", "zone", "patch", "pawn", "other")


def actor_kind(class_path: str) -> str:
    """The leaf's first `_`-separated token as a kind; `other` if unlisted."""
    leaf = class_path.rsplit("/", 1)[-1].split(".", 1)[0]
    return ACTOR_KINDS.get(leaf.split("_", 1)[0].lower(), "other")


# Internal agent codename, when the class lives under /Game/Characters/<name>/.
# These are VALORANT's internal names (Sarge = Brimstone, Smonk = Clove,
# Pandemic = Viper, ...; the vendored descriptors say so in
# third_party/vrp/.../Agents/Sarge/SargeAgentDescriptor.cs and
# .../Agents/Smonk/SmonkAbilityDescriptors.cs); they are left as-is rather than
# mapped to display names, which `equippable_table.py` already owns.
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
    # Exclude ability *controllers*: classes whose leaf starts with "Ability_"
    # are the ability actor itself (or a post-death variant), which lives across
    # the whole match. The transient effect instance is the GameObject_ /
    # Projectile_ / Patch_ actor it spawns, and that is what we want here.
    # Exclude equippables too: a "Gun_" leaf is a weapon, whatever its name
    # happens to contain (Chamber's ult gun, see EFFECT_KEYWORDS).
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

    # Collect events per GUID, then pair each open with the close that follows
    # it -- not first-open to last-close, which would span unrelated lifetimes
    # and report absurd durations if a GUID were ever reused. This comment used
    # to say GUIDs are recycled across rounds; measured over the 1,018-export
    # audit corpus (parser 259ed10, 2026-09-28; every actors.parquet `open`,
    # all classes), none is: 2,326,969 opens, 0 GUIDs opened twice in one
    # export. The pairing stays because nothing shows another build cannot
    # reuse a GUID, and it costs nothing when none does.
    events: dict[int, list[tuple]] = {}
    for i in range(len(guid)):
        cp = class_path[i]
        if not is_effect_class(cp):
            continue
        events.setdefault(guid[i], []).append(
            (time_ms[i], event[i], sx[i], sy[i], sz[i], cp)
        )

    # `went_dormant` counts INSTANCES (a pending open that saw at least one
    # `dormant` event), not raw `dormant` events: an instance that toggles
    # dormant more than once before it finally closes or the export ends is
    # one dormancy, not several. It is a general diagnostic on how many
    # instances ever went dormant -- it does NOT gate on how the instance
    # ends, so it counts both ones that later close normally and ones that
    # end up open-ended. See `main()` below for why it must not be printed as
    # if it were a decomposition of the open-ended count: it can exceed it.
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
                # Dormancy is NOT destruction. The actor stopped replicating --
                # which for a settled smoke or wall is its normal steady state --
                # so ending the instance here would make persistent effects
                # vanish early in anything built on this table. The instance
                # stays pending and, absent a later close, ends up open-ended.
                #
                # That is also what the code did before `dormant` existed as a
                # value, purely because `elif ev == "close"` did not match it.
                # The behaviour was right and unstated, which is the same shape
                # as the bug this whole pass was fixing: an open-ended row
                # because the actor went dormant and an open-ended row because
                # the export window ended are indistinguishable in the table.
                # Hence the tally. Counted once per instance, on the first
                # dormant event it sees, regardless of how it later ends.
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
    args.out.parent.mkdir(parents=True, exist_ok=True)
    pq.write_table(table, args.out, compression="zstd")

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
    # Printed with its zero. An open-ended row can mean "the actor went dormant"
    # or "the export window ended first", and the table cannot tell them apart.
    # `went_dormant` does NOT decompose `open_ended`: it counts every instance
    # that ever went dormant, including ones that later closed normally and so
    # are not open-ended, and it can exceed `open_ended`. Printed on its own
    # line rather than as a parenthetical on `open_ended` so it does not read
    # as "this many of these rows are because of dormancy".
    open_ended = sum(1 for r in rows if r["close_ms"] is None)
    print(f"  {'open-ended':12s} {open_ended}")
    print(f"  {'':12s} ({tally['went_dormant']} instance(s) went dormant at "
          f"some point, open-ended or not)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
