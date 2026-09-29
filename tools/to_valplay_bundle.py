"""Convert a vrfkit Parquet export into a valplay-compatible NDJSON bundle.

Writes the events.ndjson, movement.ndjson and manifest.json that valplay's
compute_metrics.py consumes; only the serialization is bridged, no metric is
reimplemented. Whole unresolved ClassNetCache block rows are excluded first.
Replicated properties group by (packet_id, actor, object, group_path) into
export_group_received events; RPCs (group_path contains '_ClassNetCache')
group by (packet_id, actor, group_path, handle) into rpc_received events.

Shots: ReplayPlayContinuousEffectAtLocation's FloatValues, ObjectValues and
VectorValues blobs are decoded here with the manifest's gameplay-tag table
into valorant_shot_received events. The gun is the FiringState subobject's
outer (net_guids.parquet), named from its class path (actors.parquet,
equippable_table.py): the C# parser's second tier,
ValorantShotEventEnricher.ResolveFromFiringState. Its first tier, an
equippable GUID in the blob, is never populated (0 of 2,647 shots, 02d4d478).

Usage:
    python tools/to_valplay_bundle.py <vrfkit_export_dir> [-o <output_dir>]
"""

from __future__ import annotations

import argparse
import base64
import gc
import json
import math
import os
import re
import struct as _struct
import sys
import tempfile
import time
from bisect import bisect_right
from collections import Counter, defaultdict
from functools import lru_cache
from pathlib import Path
from typing import NamedTuple

try:
    import pyarrow as pa
    import pyarrow.parquet as pq
    import pyarrow.compute as pc
    import numpy  # noqa: F401 -- required by Array.to_numpy() in _load_field_columns
except ImportError:
    sys.exit("pyarrow and numpy are required: pip install pyarrow numpy")

sys.path.insert(0, str(Path(__file__).parent))
from equippable_table import EQUIPPABLE_BY_PATH  # noqa: E402
from atomic_io import remove_tree, require_descendant  # noqa: E402


# ---------------------------------------------------------------------------
# Constants and lookup tables
# ---------------------------------------------------------------------------

# UE's FNetGUIDCache traversal cap, as in the C# resolver. Real chains on
# 02d4d478 are one hop; the bound keeps a self-referential chain from hanging.
MAX_OUTER_DEPTH = 16

# vrf-export's reserved field name for a whole unresolved ClassNetCache block:
# preservation data, not a field or RPC. Excluded before lifetime tracking and
# grouping, because an unresolved path need not carry the CNC suffix.
UNRESOLVED_CLASS_NET_CACHE_PAYLOAD_FIELD_NAME = (
    "__vrfkit_unresolved_class_net_cache_payload__"
)

# Separates an RPC group from a property group. Both constants are pinned to
# vrf-export/vrf-schema by crates/vrfkit/tests/adapter_contract.rs: a drift
# reclassifies every RPC as a property (no kills, damage or abilities).
CLASS_NET_CACHE_SUFFIX = "_ClassNetCache"

# ValorantShotFireModeResolver.AlternateMarkers, matched case-insensitively on
# every path of the FiringState outer chain. spray_control drops alternate
# fire outright (ADS and burst recoil differ).
ALTERNATE_FIRE_MARKERS = (
    "altfire",
    "zoomedfire",
    "zoomedfiring",
    "firingstateburst",
    "burstfiringstate",
    "burstmode",
)

# Array fields whose consumer decodes the undecoded blob itself, by wire name,
# mapped to the TypeName the reference labels the blob with. vrfkit emits both
# the container blob and the decoded elements; the general rule keeps the
# elements (CombatReport's `Rounds` needs them), these keep the blob.
#
# The raw-blob rule, for these and `_RAW_SOURCED_RPC_PARAMS`: the blob is built
# from raw_bits whenever the row has them, typed or not, because raw_bits rides
# beside a typed value by design and `_get_value` reports raw only when every
# typed column is null.
#
# RoundInfos: valplay's _roundinfo.collect_round_infos bit-decodes
# {Data, BitCount} and skips any non-dict, so our decoded list gave
# events_with_roundinfos 0 and a null credits_actual. Our RoundInfos bits are
# byte-identical to the reference's.
RAW_BLOB_PREFERRED = {
    "RoundInfos": "TArray<FAresPlayerRoundInfo>",
}

# The two damage RPCs, by the function name `_split_rpc_field` returns.
DAMAGE_RPC_NAMES = frozenset({
    "MulticastNotifyDamage_Point",
    "MulticastNotifyDamage_Base",
})

# Damage RPC parameters the reference bundle emits as labelled blobs, and
# rpc_received keeps as blobs, although the parser types them (ObjectNetGuid).
DEATH_MONTAGE_BLOB_PARAMS = frozenset({
    "DeathMontageEffectOverride",
    "DeathMontageEffectOverrideContext",
})

# RPC parameters, by function, built from raw_bits by the RAW_BLOB_PREFERRED
# rule, so typing one upstream can neither drop its blob nor swap it for a
# value its consumer cannot read:
# * the shot effect arrays feed this file's effect decoder
#   (`_decode_effect_elements`), which needs the exact payload window;
# * a damage RPC's LifeChangeEvents feeds valplay's `_decode_remaining_hp`
#   (weapon_stats.py), which reads only the blob's bits;
# * the death-montage pair is typed ObjectNetGuid in fields.parquet, but the
#   reference carries each as a labelled blob and rpc_received keeps that.
# The rest of valplay's RETAINED_RAW_BLOB_KEYS (AggregateKills/Deaths/Assists,
# Score) take the generic path; valplay reads either shape of them.
_RAW_SOURCED_RPC_PARAMS = {
    "ReplayPlayContinuousEffectAtLocation":
        frozenset({"FloatValues", "ObjectValues", "VectorValues"}),
    **dict.fromkeys(DAMAGE_RPC_NAMES,
                    DEATH_MONTAGE_BLOB_PARAMS | {"LifeChangeEvents"}),
}


# Replicated properties whose value_str is the parser's compact "(x,y,z)"
# vector, which the reference emits as {x, y, z}. The complete set on 02d4d478:
# a scan for fields the reference emits as {x,y,z} where we emit "(x,y,z)"
# finds only this one, 9 occurrences. Listed by name, never sniffed: a value
# that merely LOOKS like a vector is not evidence that it is one.
VECTOR_PROPERTIES = frozenset({
    "ReplicatedGravityDirection",
})


# Replicated properties the parser writes as a JSON object in value_str:
# FRepMovement's eight members fit no single column (types.rs, its Display),
# and the reference emits them as an object (ReplayJsonNormalizer.cs:255).
# Listed by name, not sniffed with startswith("{"). ReplicatedMovement is the
# only FieldType::RepMovement name in the generated table (26 entries).
# Passed through untouched: `location` is world units on every class the table
# types (docs/DATA.md has the per-class evidence). Exports from before
# 2026-09-28 carry location/100 on all classes but one: regenerate, not rescale.
JSON_OBJECT_PROPERTIES = frozenset({
    "ReplicatedMovement",
})


# Damage RPC parameters carrying an FVector_NetQuantize* payload (C# call
# sites: DamageParameters.cs:50, MulticastNotifyDamagePointParameters.cs:40-46).
DAMAGE_VECTOR_PARAMS = frozenset({
    "DamageOrigin",
    "DamageImpactLocation",
    "DamageImpactBoneRelativeLocation",
    "DamageDirection",
    "DamageImpactNormal",
})


# Enum ordinal (int in fields.parquet) -> the reference's string, from the C#
# enums verbatim; an unmapped ordinal becomes a loud *_unknown_{n}.
#
# EAresAlliance.cs: AllianceAlly = 0, AllianceEnemy = 1, AllianceNeutral = 2,
# AllianceAny = 3, AllianceCount = 4, AllianceMax = 5. On 02d4d478 ordinal 1
# occurs 30 times, and the reference says alliance_enemy.
ALLIANCE_MAP = {
    0: "alliance_ally",
    1: "alliance_enemy",
    2: "alliance_neutral",
    3: "alliance_any",
    4: "alliance_count",
    5: "alliance_max",
}

# EAresRegionalDamage.cs: RegionalDamage_Normal = 0, _Headshot = 1,
# _Legshot = 2, _RegionCount = 3, _Invalid_Radial = 4, _Invalid = 5,
# _CountPlusOne = 6. The four strings seen on the wire are verified against
# 02d4d478's reference bundle (counts inline); the three unobserved sentinels
# follow the name-to-string rule those four confirm (insert "_" before each
# capital, lowercase).
REGIONAL_DAMAGE_MAP = {
    0: "regional_damage__normal",           # verified: 446 occurrences
    1: "regional_damage__headshot",         # verified: 83
    2: "regional_damage__legshot",          # verified: 26
    3: "regional_damage__region_count",     # derived, sentinel
    4: "regional_damage__invalid__radial",  # derived, not observed
    5: "regional_damage__invalid",          # verified: 76
    6: "regional_damage__count_plus_one",   # derived, sentinel
}


# Nested path parser: "Rounds[0].Reports[1].Interactions[2].DamageDealt"
# -> [("Rounds", 0), ("Reports", 1), ("Interactions", 2), ("DamageDealt", None)]
_PATH_RE = re.compile(r'([A-Za-z_][A-Za-z0-9_]*)(?:\[(\d+)\])?')


# ---------------------------------------------------------------------------
# Loss accounting
# ---------------------------------------------------------------------------
class _Tally(dict):
    """Every row this conversion dropped and every value it invented.

    Counted, neither raised nor skipped: each shape is one the adapter can go
    past (property key collisions are structural, tens of thousands per
    replay), and this is the last hop before consumption, so an uncounted drop
    is invisible to every upstream check while the bundle still looks complete.
    The key set is fixed: `bump` on an unknown name raises KeyError.
    """

    #: counter -> the wording its summary line uses.
    REASONS = {
        "unnamed_property_rows":
            "property rows with no field name (value dropped)",
        "unnamed_rpc_rows":
            "RPC rows with no field name (parameter dropped)",
        "unnamed_rpc_invocations":
            "RPC invocations dropped whole (no row supplied a name)",
        "rpc_param_collisions":
            "RPC parameters overwritten inside one invocation group",
        "property_key_collisions":
            "property values overwritten by a same-named row in one event",
        "unparsable_path_segments":
            "field-path segments kept as literal object keys",
        "payload_shape_conflicts":
            "payload values destroyed by a row of an incompatible shape",
        "multi_typed_rows":
            "rows with more than one typed column set (extras discarded)",
        "fabricated_shot_locations":
            "shot locations fabricated as the world origin",
        "fabricated_shot_rotations":
            "shot rotations fabricated as (0,0,0), indistinguishable from a "
            "real aim of world +X",
        "effect_half_read_pairs":
            "shot effect (tag, value) pairs with one half unreadable "
            "(pair dropped, e.g. a missing FiringState.AttackVector.N)",
        "effect_array_residual_bits":
            "shot effect blobs with more than a byte left over after "
            "decoding (framing did not end where this parse expected)",
        "missing_manifest":
            "manifest.json absent (replay metadata substituted)",
        "empty_gameplay_tag_table":
            "shots decoded with no gameplay-tag table (fields keyed by index)",
        "events_time_ms_regressions":
            "events whose time_ms is lower than the line before (tie-break unsound)",
        "upstream_row_count_disagreement":
            "row counts the export declared that its own tables contradict",
        "unknown_actor_lifecycle_events":
            "actors.event values this adapter has no event type for "
            "(published as actor_lifecycle_unknown)",
        "non_finite_movement_rows":
            "movement rows written with a non-finite value (spelled "
            "Infinity/NaN; strict JSON parsers reject the line)",
        "raw_blobs_unavailable":
            "wire blobs a consumer decodes itself (RoundInfos, damage "
            "LifeChangeEvents, shot effect arrays) that could not be built: "
            "the row had no raw bits, or only its decoded members arrived",
        "damaged_bone_undecoded":
            "DamagedBone values the parser could not decode (published as "
            "null, never guessed from the raw bytes)",
    }

    def __init__(self):
        super().__init__((name, 0) for name in self.REASONS)

    def bump(self, name: str, n: int = 1) -> None:
        self[name] = self[name] + n

    @property
    def total(self) -> int:
        return sum(self.values())

    def lines(self) -> list[str]:
        """One line per counter that fired, in REASONS order. Zeros stay off
        the console on purpose, despite CLAUDE.md's print-zeros rule: a block
        of zeros trains the reader to skip it. The manifest's `losses` carries
        every counter, zeros included (docs/USAGE.md)."""
        return [f"  {name}: {self[name]:,} -- {reason}"
                for name, reason in self.REASONS.items() if self[name]]


def _bump(tally, name: str, n: int = 1) -> None:
    """Increment a counter; a `None` tally is a no-op, so the leaf helpers stay
    callable and testable alone. The conversion phases always pass one."""
    if tally is not None:
        tally.bump(name, n)


# ---------------------------------------------------------------------------
# Scalar and vector formatting: three load-bearing precision policies, each
# named by its function.
# ---------------------------------------------------------------------------
def _f32_shortest(value):
    """Shortest decimal that round-trips through float32.

    What System.Text.Json writes for a float: the Float32 spawn coordinate
    2382.2f is "2382.2", where widening it to a Python float prints
    2382.199951171875.
    """
    if value is None:
        return None
    packed = _struct.unpack("f", _struct.pack("f", value))[0]
    for digits in range(1, 10):
        candidate = float(f"{packed:.{digits}g}")
        # Rounding can carry a candidate past FLT_MAX (3.403e+38 at four
        # digits), which cannot round-trip: Python 3.12 packs it as inf and
        # the comparison rejects it, 3.13 raises OverflowError; both mean no.
        try:
            back = _struct.unpack("f", _struct.pack("f", candidate))[0]
        except OverflowError:
            continue
        if back == packed:
            return int(candidate) if candidate.is_integer() else candidate
    return int(packed) if float(packed).is_integer() else packed


#: One encoder, reused: `json.dumps` with non-default kwargs builds a new
#: JSONEncoder per call (2.4 million here, 1.8 s of setup); same output.
_JSON = json.JSONEncoder(separators=(',', ':'), ensure_ascii=True)

#: The movement record, one `%s` per slot, in the key order the record dict
#: had, each slot filled with `_JSON.encode`'s text: byte-for-byte what encoding
#: the dict wrote. The only place the line is spelled; `_write_movement` splits
#: it on `%s` into the literal fragments.
_MOVEMENT_LINE = (
    '{"time_ms":%s,"shooter_character_net_guid":%s,'
    '"position":{"x":%s,"y":%s,"z":%s},'
    '"velocity":{"x":%s,"y":%s,"z":%s},'
    '"yaw":%s,"pitch":%s}\n'
)

#: Rows per movement.ndjson block assembled in Arrow and written at once: holds
#: one block's text (~48 MB at ~184 bytes a line), not the file's. f73d4475's
#: writer alone, 3 runs each: 2**16 and 2**18 equal within noise (~3.4 s,
#: ~665 MB peak, set by the per-column dedup), 2**14 ~4% slower, 2**20 +50-80 MB.
_MOVEMENT_BLOCK_ROWS = 1 << 18


#: Below this magnitude every integer is exact in float32, so an integral
#: float32's shortest round-trip text IS its integer text. Above, they part:
#: 123456792.0 round-trips as 123456790.
_F32_EXACT_INT_LIMIT = 2 ** 24

#: numpy's float32 text is positional only for 1e-4 <= |v| < 1e6, judged on
#: the binary value; Python's repr (the per-value rule) is positional from 1e-4
#: to 1e16, judged on the decimal. Outside this band they disagree: 1234567.5
#: is '1.2345675e+06' to numpy, and float32(1e-4) (9.99999975e-05) is '1e-04'
#: to numpy but '0.0001' to repr. Compared in float64: 1e-4 is not a float32.
_F32_POSITIONAL_BAND = (1e-4, 1e6)


def _json_scalar_column(arr, *, shorten=False):
    """The JSON text of a 1-D numpy float array, once per distinct value.

    Returns ``(texts, inverse)``: `texts` is a pa.string() array of each
    distinct value's text, `inverse` a pa.int32() array mapping every row to
    its entry, so `texts.take(inverse)` is the column's text row by row.
    numpy.unique collapses the column in C and the caller fans the texts back
    out with Arrow's take, one write block at a time, so no Python object is
    made per row. The fan-out is the caller's; the text rule lives here only.

    The contract is per value, and checked (`MovementTextRuleTests`): each
    row's text is `_JSON.encode(_f32_shortest(v))` with `shorten=True`, else
    `_JSON.encode(v)` -- `Infinity`/`NaN` included, which an f-string would
    spell as invalid JSON.

    Unique over BIT PATTERNS, not values: -0.0 == 0.0 would merge the zeros
    under whichever sign sorted first. No movement column holds a -0.0 (0 of
    1,973,922,078 rows x 8 columns, 1,018 exports, 2026-09-28).

    `shorten=True` vectorises only where proven equal to the per-value rule:
    an integral value below `_F32_EXACT_INT_LIMIT` is its int text, and a
    non-integral one inside `_F32_POSITIONAL_BAND` is numpy's astype(str),
    the same Dragon4 shortest round-trip in positional notation. Both checked
    exhaustively, not sampled: all 556,160,338 non-integral float32 in the
    band (278,080,169 per sign) and all 33,554,430 integral ones with
    0 < |v| < 2**24, 0 mismatches (numpy 2.5.2, 2026-09-28); just outside,
    float32(+/-1e-4) and every non-integral value in 1e6 <= |v| < 2**20
    differ. Every other value takes the per-value encoder: applied to all,
    the shortcut wrote +/-inf as -9223372036854775808 and NaN as `nan`.
    Finiteness is not guaranteed upstream (vrf-movement reads raw f32/f64
    unchecked; stream.rs narrows f64 with a bare `as f32`), although the
    corpus holds 0 non-finite values, 0 with |v| >= 2**24 and 0 non-integral
    ones outside the band in any shortened column. `_write_movement` says how
    a non-finite value reaches the consumer.

    Worth it because the columns are quantized and repeat: on 02d4d478's
    1,837,220 kept rows the six shortened columns format 1,499,222 distinct
    values instead of 11,023,320 (pos_x 691,850, pos_y 696,435, pos_z 70,260,
    vel_x 17,358, vel_y 17,248, vel_z 6,071). yaw and pitch dedup (65,491 and
    16,943 distinct) but are not shortened; see `_write_movement`.
    """
    if arr.dtype.kind != "f" or arr.dtype.itemsize not in (4, 8):
        # The bit-pattern view needs a same-width unsigned type; a silent
        # mis-view would print plausible numbers.
        raise TypeError(f"_json_scalar_column wants float32/float64, got {arr.dtype}")
    if arr.shape[0] > numpy.iinfo(numpy.int32).max:
        # The int32 inverse would wrap and point rows at wrong texts.
        raise ValueError(f"{arr.shape[0]:,} rows is more than an int32 index can address")
    ubits, inverse = numpy.unique(
        arr.view(numpy.dtype(f"u{arr.dtype.itemsize}")), return_inverse=True
    )
    # int32 halves what the caller holds per column until the write: peak
    # memory is this function's acceptance bar as much as speed.
    inverse = pa.array(inverse.astype(numpy.int32))
    uniq = ubits.view(arr.dtype)
    if shorten:
        # An object array, so a wide int text cannot truncate to the float
        # column's narrower `<U` width.
        magnitude = numpy.abs(uniq.astype(numpy.float64))
        finite = numpy.isfinite(uniq)
        integral = finite & (uniq == numpy.trunc(uniq))
        low, high = _F32_POSITIONAL_BAND
        as_int = integral & (magnitude < _F32_EXACT_INT_LIMIT)
        as_dragon4 = finite & ~integral & (magnitude >= low) & (magnitude < high)
        texts = numpy.empty(uniq.shape[0], dtype=object)
        if as_dragon4.any():
            texts[as_dragon4] = uniq[as_dragon4].astype(str)
        if as_int.any():
            texts[as_int] = uniq[as_int].astype(numpy.int64).astype(str)
        per_value = ~(as_int | as_dragon4)
        if per_value.any():
            encode = _JSON.encode
            texts[per_value] = [encode(_f32_shortest(v))
                                for v in uniq[per_value].tolist()]
    else:
        encode = _JSON.encode
        texts = [encode(float(v)) for v in uniq.tolist()]
    return pa.array(texts, type=pa.string()), inverse


def _vec3(x, y, z) -> dict:
    """Build an {x, y, z} dict with integral components as ints, as
    System.Text.Json writes a double (0.0 as `0`)."""
    return {
        axis: (int(n) if float(n).is_integer() else n)
        for axis, n in zip(("x", "y", "z"), (x, y, z))
    }


def _parse_vector_or_none(val):
    """Parse a "(x,y,z)" vector at full precision; None if unparseable.

    Full precision: the reference emits the damage direction's unit vector as
    0.055482650227362894. None, not a zero vector: for damage geometry a zero
    vector would be a silent wrong value. Integral components become ints.
    """
    if isinstance(val, dict):
        return val
    if not isinstance(val, str):
        return None
    parts = val.strip("()").split(",")
    if len(parts) != 3:
        return None
    try:
        nums = [float(p) for p in parts]
    except ValueError:
        return None
    return _vec3(*nums)


def _parse_vector_or_zero(val, tally=None) -> dict:
    """Parse a shot Location into {x, y, z} at full precision.

    The reference emits the raw double (559.962145690918), and
    shot_rays.sample_rays matches it only unrounded. An absent or unparseable
    value still yields the world origin, because callers index into it, and
    is counted as `fabricated_shot_locations`: every effect RPC emits a shot
    ("No blob guard" in `_build_rpc_events`), so this is reachable, and an
    origin is indistinguishable from a real coordinate downstream.
    """
    parsed = _parse_vector_or_none(val)
    if parsed is None:
        _bump(tally, "fabricated_shot_locations")
        return {"x": 0, "y": 0, "z": 0}
    return parsed


def _parse_rotation(val, tally=None) -> dict:
    """Parse a Rotation into {pitch, yaw, roll}. As in `_parse_vector_or_zero`,
    the (0,0,0) fallback is counted, as `fabricated_shot_rotations`: valplay's
    spray_control reads `shot.rotation` as the real aim."""
    if val is None:
        _bump(tally, "fabricated_shot_rotations")
        return {"pitch": 0, "yaw": 0, "roll": 0}
    if isinstance(val, dict):
        return val
    if isinstance(val, str):
        # Compact rotator format: "rot(pitch,yaw,roll)"
        s = val
        if s.startswith("rot(") and s.endswith(")"):
            s = s[4:-1]
        else:
            s = s.strip("()")
        parts = s.split(",")
        if len(parts) == 3:
            try:
                # Rust writes shortest-round-trip f32 decimals; widening each
                # back through f32 matches the raw-wire decoder and C# exactly.
                components = [
                    _struct.unpack("<f", _struct.pack("<f", float(part)))[0]
                    for part in parts
                ]
                return {"pitch": components[0],
                        "yaw": components[1],
                        "roll": components[2]}
            except ValueError:
                pass
    _bump(tally, "fabricated_shot_rotations")
    return {"pitch": 0, "yaw": 0, "roll": 0}


# ---------------------------------------------------------------------------
# NetGUID outer-chain resolution (weapon identity and fire mode)
# ---------------------------------------------------------------------------
def _has_alternate_marker(value) -> bool:
    if not value:
        return False
    lowered = str(value).lower()
    return any(marker in lowered for marker in ALTERNATE_FIRE_MARKERS)


def _resolve_fire_mode(firing_state_guid, source_id, guid_outer, guid_path):
    """Classify a shot as primary / alternate fire: ``(fire_mode, evidence)``,
    mirroring ValorantShotFireModeResolver.

    The signal is the NAME of the firing-state subobject: "FiringState" for
    the primary cycle, "ZoomedFiringState", "FiringStateBurst", ... for the
    secondary. burst_shot_number only indexes shots within a spray: reading a
    non-zero one as alternate misclassified 1,462 of 2,475 shots on 02d4d478.
    "unknown" means no path resolved: an effect with no firing state at all.
    """
    if _has_alternate_marker(source_id):
        return "alternate", f"source:{source_id}"

    paths = []
    current = firing_state_guid
    for _ in range(MAX_OUTER_DEPTH):
        if not current:
            break
        path = guid_path.get(current)
        if path and path.strip():
            paths.append(path)
        nxt = guid_outer.get(current)
        if nxt is None or nxt == current:
            break
        current = nxt

    if not paths:
        return "unknown", None

    evidence = "firing-state:" + " -> ".join(paths)
    if any(_has_alternate_marker(p) for p in paths):
        return "alternate", evidence
    return "primary", evidence


def _resolve_equippable(net_guid, guid_outer, guid_path, guid_class):
    """Walk a GUID's outer chain to the equippable actor containing it:
    ``(owner_net_guid, name, category, class_path)`` or ``None``.

    Two lookups per hop: ``guid_class`` (actors.parquet: channel opens, with
    the spawn class path) and ``guid_path`` (net_guids.parquet: every
    registered GUID, subobjects included). A weapon appears in the first, its
    FiringState only in the second.
    """
    current = net_guid
    for _ in range(MAX_OUTER_DEPTH):
        if not current:
            return None
        for table in (guid_class, guid_path):
            path = table.get(current)
            if path:
                hit = EQUIPPABLE_BY_PATH.get(path)
                if hit:
                    name, category, canonical = hit
                    return current, name, category, canonical
        nxt = guid_outer.get(current)
        if nxt is None or nxt == current:
            return None
        current = nxt
    return None


def _load_net_guids(export_dir):
    """Read net_guids.parquet into (guid -> outer, guid -> path, row count).

    An absent file gives empty dicts and a `None` row count: older exports
    still convert, with weapon identity unresolved, and None is not 0 because
    only an empty table can be compared with a declared count. The row count
    is the table's height, not either dict's size: both drop rows.
    """
    path = export_dir / "net_guids.parquet"
    if not path.exists():
        return {}, {}, None
    table = pq.read_table(path)
    guids = table.column("net_guid").to_pylist()
    paths = table.column("path").cast("string").to_pylist()
    outers = table.column("outer_net_guid").to_pylist()
    guid_outer = {g: o for g, o in zip(guids, outers) if o is not None}
    guid_path = {g: p for g, p in zip(guids, paths) if p}
    return guid_outer, guid_path, len(table)


# ---------------------------------------------------------------------------
# Effect blob decoder (Python port of the Rust vrf-decode/src/effect.rs logic)
# ---------------------------------------------------------------------------
class _BitReader:
    """Minimal bit-level reader for UE4 IntPacked, f32, f64."""

    def __init__(self, data: bytes, bit_len: int):
        self._data = data
        self._bit_len = bit_len
        self._pos = 0

    def at_end(self) -> bool:
        return self._pos >= self._bit_len

    def bits_remaining(self) -> int:
        return max(0, self._bit_len - self._pos)

    def tell(self) -> int:
        return self._pos

    def read_bit(self) -> int:
        return self.read_bits(1)

    def read_bits(self, n: int) -> int:
        """Read `n` bits LSB first from one slice (a per-bit loop made 2.8M
        calls on one replay's shot blobs), with that loop's contract: a short
        read leaves the position at the end and raises EOFError, and a declared
        length past the buffer raises IndexError, aborting the conversion,
        where a slice alone would silently pad with zeros.
        """
        start = self._pos
        stop = min(start + n, self._bit_len)
        if stop > start and stop > len(self._data) * 8:
            raise IndexError("index out of range")
        if start + n > self._bit_len:
            self._pos = self._bit_len
            raise EOFError
        self._pos = start + n
        chunk = self._data[start >> 3:(start + n + 7) >> 3]
        return (int.from_bytes(chunk, "little") >> (start & 7)) & ((1 << n) - 1)

    def read_int_packed(self) -> int:
        """Read a UE4 IntPacked value (7-bit variable-length, LSB first)."""
        value = 0
        shift = 0
        while True:
            if self._pos + 8 > self._bit_len:
                raise EOFError
            byte_val = self.read_bits(8)
            has_more = byte_val & 1
            value |= (byte_val >> 1) << shift
            shift += 7
            if not has_more:
                break
            if shift > 35:
                raise ValueError("IntPacked overflow")
        return value

    def read_f32(self) -> float:
        bits = self.read_bits(32)
        return _struct.unpack('<f', _struct.pack('<I', bits))[0]

    def read_f64(self) -> float:
        return _struct.unpack('<d', _struct.pack('<Q', self.read_bits(64)))[0]

    def skip_bits(self, n: int):
        self._pos = min(self._pos + n, self._bit_len)


def _read_effect_vector(r: _BitReader):
    # Left to right: a short read on y or z stops where sequential reads would.
    return (r.read_f64(), r.read_f64(), r.read_f64())


class _EffectArraySpec(NamedTuple):
    """How to read one of the three effect value arrays: same wire shape,
    different tag/value handles (the containing struct's own, hence not
    contiguous) and value reader."""

    tag_handle: int
    value_handle: int
    read_value: object


_EFFECT_FLOATS = _EffectArraySpec(7, 8, _BitReader.read_f32)
_EFFECT_VECTORS = _EffectArraySpec(11, 12, _read_effect_vector)
_EFFECT_OBJECTS = _EffectArraySpec(15, 16, _BitReader.read_int_packed)


def _decode_effect_elements(data: bytes, bit_count: int, spec: _EffectArraySpec,
                            tally=None):
    """Decode one effect value array -> list of (tag_index, value) tuples.

    ``spec.read_value`` must raise on a short read, not return a sentinel: the
    ``consumed``/``skip_bits`` resync relies on a failed read leaving the
    element's earlier value untouched and the position advanced by what it
    consumed.

    Decodes the preserved raw blob, independently of effect.rs's additive
    JSON. Where effect.rs rejects a blob outright (`PayloadUnderread`,
    `PayloadOverread`, `ResidualBits`, ...), this port keeps what it decoded,
    so a shot is not lost whole, and counts the two shapes that fabricate
    downstream values: a pair with one half unreadable (dropped by
    `_decode_effect_blob`, like a missing `FiringState.AttackVector.N`; see
    spray_control.py), and more than 7 bits left after the element loop
    (Rust's `ResidualBits` threshold: the framing did not end where this
    parse expected).
    """
    r = _BitReader(data, bit_count)
    try:
        count = r.read_int_packed()
    except (EOFError, ValueError):
        count = None
    # No readable count, or one past Rust's MAX_ARRAY_COUNT (256): framing
    # broke at the first IntPacked, but the window still reaches the residual
    # check. Count 0 enters the loop, which consumes the terminator Rust accepts.
    framed = count is not None and count <= 256
    elements = [(None, None)] * count if framed else []
    while framed and not r.at_end():
        try:
            enc_idx = r.read_int_packed()
        except (EOFError, ValueError):
            break
        if enc_idx == 0:
            if r.bits_remaining() == 8:
                try:
                    r.read_int_packed()
                except (EOFError, ValueError):
                    pass
            break
        idx = enc_idx - 1
        if idx >= count:
            break
        tag = None
        val = None
        while not r.at_end():
            try:
                enc_h = r.read_int_packed()
            except (EOFError, ValueError):
                break
            if enc_h == 0:
                break
            handle = enc_h - 1
            try:
                payload_bits = r.read_int_packed()
            except (EOFError, ValueError):
                break
            if payload_bits > r.bits_remaining():
                break
            start = r.tell()
            if handle == spec.tag_handle:
                try:
                    tag = r.read_int_packed()
                except (EOFError, ValueError):
                    pass
            elif handle == spec.value_handle:
                try:
                    val = spec.read_value(r)
                except (EOFError, ValueError):
                    pass
            consumed = r.tell() - start
            if consumed < payload_bits:
                r.skip_bits(payload_bits - consumed)
        elements[idx] = (tag, val)
    # A declared slot missing either half, never visited or lost mid-read, is
    # a pair `_decode_effect_blob` drops.
    half_read = sum(1 for tag, val in elements if tag is None or val is None)
    if half_read:
        _bump(tally, "effect_half_read_pairs", half_read)
    # However the loop ended, more than 7 bits left is Rust's `ResidualBits`;
    # sub-byte padding is normal, as the Rust guard also tolerates.
    if r.bits_remaining() > 7:
        _bump(tally, "effect_array_residual_bits")
    return elements


def _decode_effect_blob(blob: _EffectBlob | None, spec: _EffectArraySpec,
                        tag_table: dict, tally=None) -> dict:
    """Decode one effect blob into {tag_name: value}; an absent blob is {}.

    Half-read pairs are dropped, counted by `_decode_effect_elements`: a
    missing `FiringState.AttackVector.N` is how `_build_shot_event` notices.
    """
    if blob is None:
        return {}
    decoded = {}
    for tag_idx, val in _decode_effect_elements(blob.data, blob.bit_count, spec,
                                                tally):
        if tag_idx is not None and val is not None:
            decoded[tag_table.get(tag_idx, str(tag_idx))] = val
    return decoded


def _build_tag_table(manifest: dict) -> dict:
    """Build tag_index -> tag_name from the manifest's gameplay tag group."""
    groups = manifest.get("net_field_export_groups", [])
    for g in groups:
        if g.get("path") == "NetworkGameplayTagNodeIndex":
            return {f["handle"]: f["name"] for f in g.get("fields", [])}
    return {}


def _decode_rotation_short(data: bytes, bit_count: int) -> dict:
    """Decode a UE4 RotationShort from raw bits.

    Wire format: for each of pitch/yaw/roll:
      1 bit: is_non_zero
      if non_zero: 16 bits LE unsigned -> degrees = value * 360 / 65536
    """
    r = _BitReader(data, bit_count)
    result = {"pitch": 0.0, "yaw": 0.0, "roll": 0.0}
    for axis in ("pitch", "yaw", "roll"):
        try:
            is_non_zero = r.read_bit()
            if is_non_zero:
                val = r.read_bits(16)
                degrees = val * 360.0 / 65536.0
                result[axis] = degrees
        except (EOFError, ValueError):
            break
    return result


# ---------------------------------------------------------------------------
# Shot events
# ---------------------------------------------------------------------------
class _EffectBlob(NamedTuple):
    """One undecoded value array with the bit length the parser declared.

    Parquet stores whole bytes, so N bits arrive as ceil(N/8) bytes; taking
    the length as len(data) * 8 would decode up to 7 padding bits as data.
    The two agree on all 692,840 effect blobs of the 11 cross-validated
    replays, so only a test tells the readings apart.
    """

    data: bytes
    bit_count: int


class _EffectBlobs(NamedTuple):
    """The three undecoded value arrays a shot RPC may carry. Any may be absent."""

    floats: _EffectBlob | None = None
    objects: _EffectBlob | None = None
    vectors: _EffectBlob | None = None


class _ShotContext(NamedTuple):
    """Per-replay lookups every shot event needs. Without the two lookups an
    event still comes out, with a null equippable and fire_mode "unknown", as
    the reference emits a server-world effect."""

    tag_table: dict
    equippable_lookup: object = None
    fire_mode_lookup: object = None


def _build_shot_event(
    ctx: _ShotContext,
    time_ms, packet_id, actor_net_guid, object_net_guid, channel_index,
    scalar_params: dict, blobs: _EffectBlobs, tally=None,
) -> dict:
    """Build a valorant_shot_received event from decoded RPC params.

    Always returns one. 172 of 02d4d478's 2,647 effect RPCs are server-world
    effects (source_id DedicatedServerWorldSourceID) with no firing state,
    attack vectors or weapon. They come back with a null equippable and
    fire_mode "unknown", as the reference emits them, for valplay's "unknown"
    weapon bucket and shots_without_equippable diagnostic; every section they
    would distort already guards on firing_player_state or attack_vectors.
    """
    tag_table = ctx.tag_table
    floats = _decode_effect_blob(blobs.floats, _EFFECT_FLOATS, tag_table, tally)
    objects = _decode_effect_blob(blobs.objects, _EFFECT_OBJECTS, tag_table, tally)
    vectors = _decode_effect_blob(blobs.vectors, _EFFECT_VECTORS, tag_table, tally)

    location = scalar_params.get("Location")
    rotation = scalar_params.get("Rotation")
    # Location/Rotation arrive under their bare handles "248"/"249", the only
    # spelling observed (typed on 2,647 of 2,647 shots on 02d4d478). A
    # {BitCount, Data} blob there means the overlay failed to type it: the
    # dict branches and _decode_rotation_short decode the wire. Keep them.
    if location is None:
        raw248 = scalar_params.get("248")
        if isinstance(raw248, dict) and "Data" in raw248:
            raw_bytes = base64.b64decode(raw248["Data"])
            if len(raw_bytes) >= 24:
                x = _struct.unpack_from('<d', raw_bytes, 0)[0]
                y = _struct.unpack_from('<d', raw_bytes, 8)[0]
                z = _struct.unpack_from('<d', raw_bytes, 16)[0]
                location = _vec3(x, y, z)
        elif raw248 is not None:
            location = raw248
    if rotation is None:
        raw249 = scalar_params.get("249")
        if isinstance(raw249, dict) and "Data" in raw249:
            raw_bytes = base64.b64decode(raw249["Data"])
            bit_count_rot = raw249.get("BitCount", len(raw_bytes) * 8)
            rotation = _decode_rotation_short(raw_bytes, bit_count_rot)
        elif raw249 is not None:
            rotation = raw249
    effect_id = scalar_params.get("EffectID")
    source_id = scalar_params.get("SourceID")
    start_time = scalar_params.get("StartMovementTime")
    is_local = scalar_params.get("bLocalEffect") or scalar_params.get("LocalEffect")
    is_transient = scalar_params.get("bTransient") or scalar_params.get("Transient")
    wait_on = scalar_params.get("WaitOnReplicationActor")
    alliance = scalar_params.get("AllianceFilter")

    loc_obj = _parse_vector_or_zero(location, tally)
    rot_obj = _parse_rotation(rotation, tally)

    attack_keys = (f"FiringState.AttackVector.{i}" for i in range(1, 16))
    attack_vectors = [{"x": x, "y": y, "z": z} for x, y, z in
                      (vectors[key] for key in attack_keys if key in vectors)]

    burst = floats.get("FiringState.BurstShotNumber")
    yaw_switch = floats.get("FiringState.YawSwitch")

    ammo = floats.get("FiringState.AmmoRemaining")
    if ammo is not None:
        ammo = int(ammo)

    num_proj = floats.get("FiringState.NumProjectiles")
    if num_proj is not None:
        num_proj = int(num_proj)

    random_seed = floats.get("FiringState.RandomSeed")
    tracer_opt = floats.get("FiringState.TracerOption")
    if tracer_opt is not None:
        tracer_opt = int(tracer_opt) if tracer_opt == int(tracer_opt) else tracer_opt

    firing_player = objects.get("FiringState.FiringPlayerState")
    firing_state = objects.get("FiringState.FiringState")

    # FiringState's outer is the gun; null, never guessed, when the chain does
    # not reach a known equippable.
    equippable = None
    if ctx.equippable_lookup is not None and firing_state:
        hit = ctx.equippable_lookup(firing_state)
        if hit is not None:
            owner_guid, name, category, class_path = hit
            equippable = {
                "net_guid": owner_guid,
                "name": name,
                "category": category,
                "class_path": class_path,
            }

    # Fire mode comes from the same chain: the firing-state subobject's own name.
    if ctx.fire_mode_lookup is not None:
        fire_mode, fire_mode_evidence = ctx.fire_mode_lookup(firing_state, source_id)
    else:
        fire_mode, fire_mode_evidence = "unknown", None

    # Only an int is an enum ordinal; anything else (the {BitCount, Data} blob
    # of an untyped field) passes through unchanged, like every shot param.
    alliance_str = alliance
    if isinstance(alliance, int):
        alliance_str = ALLIANCE_MAP.get(alliance, f"alliance_unknown_{alliance}")

    shot = {
        "effect_id": effect_id,
        # Float32 (as is random_seed): the reference prints the shortest
        # round-trip (12.780108), not the widened value.
        "start_movement_time": _f32_shortest(start_time)
        if isinstance(start_time, float) else start_time,
        "source_id": source_id,
        "is_local_effect": bool(is_local),
        "is_transient": bool(is_transient) if is_transient is not None else True,
        "wait_on_replication_actor": wait_on or 0,
        # Null when absent, as the reference on 101 of 02d4d478's 2,647
        # effects; a default would merge two input states.
        "alliance_filter": alliance_str,
        "location": loc_obj,
        "rotation": rot_obj,
        "ammo_remaining": ammo,
        # Null when absent, as the reference on 172 of 2,647 shots: a default
        # of 1 rewrote a genuine 0, and compute_metrics.py:1560 reads it
        # without a default of its own.
        "num_projectiles": num_proj,
        "random_seed": _f32_shortest(random_seed)
        if isinstance(random_seed, float) else random_seed,
        "tracer_option": tracer_opt,
        "burst_shot_number": burst,
        "yaw_switch": yaw_switch,
        "firing_player_state": firing_player,
        "firing_state": firing_state,
        "attack_vectors": attack_vectors,
        # The C# parser's tier-1 source; never populated in any observed replay.
        "effect_equippable": None,
        "equippable": equippable,
        "fire_mode": fire_mode,
        "fire_mode_evidence": fire_mode_evidence,
    }

    return {
        "type": "valorant_shot_received",
        "time_ms": time_ms,
        "packet_id": packet_id,
        "actor_net_guid": actor_net_guid,
        # fields.parquet's real values, as the reference has them on all 2,647
        # events (object 22 where the actor is 2, channel 1).
        "object_net_guid": object_net_guid,
        "channel": channel_index,
        "shot": shot,
    }


# ---------------------------------------------------------------------------
# Combat report leaf labels
#
# The parser labels each flattened array leaf with the name the replay
# declares: right for fields.parquet, wrong for this bundle twice over.
#
# 1. compute_metrics.py reads the reference's names (Subject, Team, DidKill,
#    Died, AssistType, DamageDealt, DamageReceived, HitsDealt, HitsReceived,
#    DealtInteractions[].Regions[].{Region,Hits,IsWallPen}), and the wire
#    spells six differently: bDidKill, bDied, bIsWallPen, ParticipantSubject,
#    and Riot's typos DamageRecieved and HitsRecieved.
# 2. Declared names are not unique within one flattened element: the
#    HUDConfig/StateRemainingTime/GameTime/GamePhase struct is flattened at
#    eight positions (handles 6, 99 and 105 all declare HUDConfig at the
#    Reports level; 27/31 and 62/66 one level down). Keyed by name, 3,405 of
#    20,298 distinct payload paths on 02d4d478 would merge, uncounted.
#
# So the bundle keys on the handle: the reference's member name where the C#
# parser has one, else `_h{handle}`, the label this bundle already carried
# (events.ndjson stayed byte-identical). The parser emits what the wire says;
# relabelling for a consumer is this adapter's presentation concern.
# ---------------------------------------------------------------------------
COMBAT_REPORT_GROUP = "CombatReportComponent"

# handle -> the member name the C# reference emits. Leaf handles only: the
# containers (4 Reports, 10 Interactions, 26 DealtInteractions,
# 61 ReceivedInteractions, 44/79 Regions) keep the parser's schema names.
COMBAT_REPORT_REFERENCE_NAMES = {
    3: "RoundNumber",              # wire: RoundNum
    5: "RoundNumber",
    11: "Subject",                 # wire: ParticipantSubject
    12: "Team",                    # wire: ParticipantTeamName
    13: "CharacterIcon",           # wire: ParticipantCharacterIcon
    18: "DamageDealt",
    19: "HitsDealt",
    20: "DamageReceived",          # wire: DamageRecieved (Riot's typo)
    21: "HitsReceived",            # wire: HitsRecieved (Riot's typo)
    22: "DidKill",                 # wire: bDidKill
    23: "AssistType",
    24: "KillerPlayerState",       # wire: ParticipantsKillerState
    25: "WasKiller",               # wire: bWasKiller
    45: "Region",
    46: "Hits",
    47: "Damage",
    48: "IsWallPen",               # wire: bIsWallPen
    49: "IsKill",                  # wire: bIsKill
    50: "DestroyedArmor",
    80: "Region",
    81: "Hits",
    82: "Damage",
    83: "IsWallPen",               # wire: bIsWallPen
    84: "IsKill",                  # wire: bIsKill
    85: "DestroyedArmor",
    96: "CombatReportIndex",
    98: "ResurrectorPlayerState",
    103: "Died",                   # wire: bDied
}


# Sub-array container handles: such a row already carries the walker's schema
# name, the reference's spelling, and is left alone.
COMBAT_REPORT_CONTAINER_HANDLES = frozenset({4, 10, 26, 44, 61, 79})


def _combat_report_leaf_name(group_path: str, field_name: str, handle) -> str:
    """Relabel the LAST segment of one combat-report array leaf for the bundle.

    Two synthesised rows keep the parser's label: `emit_remaining_raw`'s
    `..._raw` (handle u32::MAX, else `_h4294967295`) and the depth-limit row
    carrying a container handle. Neither occurs on the corpus (MAX_ELEMENTS
    4096 against a peak near 50; MAX_RECURSION_DEPTH 12 against a depth of 5).
    """
    if not group_path or COMBAT_REPORT_GROUP not in group_path:
        return field_name
    if not field_name.startswith("Rounds["):
        return field_name
    head, dot, leaf = field_name.rpartition(".")
    if not dot:
        return field_name
    if leaf == "_raw" or handle in COMBAT_REPORT_CONTAINER_HANDLES:
        return field_name
    name = COMBAT_REPORT_REFERENCE_NAMES.get(handle)
    if name is None:
        name = f"_h{handle}"
    return head + dot + name


# ---------------------------------------------------------------------------
# Field rows -> nested payloads
# ---------------------------------------------------------------------------
def _normalize_prop_field_name(field_name: str, is_bool: bool) -> str:
    """Strip the 'b' of a boolean property name ('bUltimateActive' ->
    'UltimateActive'), as the C# parser does and compute_metrics.py expects."""
    if is_bool and field_name.startswith('b') and len(field_name) > 1 and field_name[1].isupper():
        return field_name[1:]
    return field_name


@lru_cache(maxsize=None)
def _parse_field_path_cached(path: str):
    """Parse one path: `(parts, unparsable_segment_count)`, memoised (a
    replay's ~520k parses cover a few thousand distinct paths). The caller
    bumps the tally on every call, so cache hits cannot under-count."""
    parts = []
    unparsable = 0
    for seg in path.split('.'):
        m = _PATH_RE.fullmatch(seg)
        if m:
            name = m.group(1)
            idx = int(m.group(2)) if m.group(2) is not None else None
            parts.append((name, idx))
        else:
            # A literal key is right for a leaf that is not an identifier: a
            # bare handle number ("248") or a Blueprint name with spaces (449
            # rows, 41 spellings on out/baseline, none with a bracket). A
            # bracket means unread subscripts: "Rounds[0][1]" flattens two
            # levels into one key, so it is counted.
            if '[' in seg or ']' in seg:
                unparsable += 1
            parts.append((seg, None))
    return tuple(parts), unparsable


def _parse_field_path(path: str, tally=None):
    """Parse a dot-separated field path with optional array indices."""
    parts, unparsable = _parse_field_path_cached(path)
    if unparsable:
        _bump(tally, "unparsable_path_segments", unparsable)
    return list(parts)


class _Node(dict):
    """A dict `_set_nested` built to hold rows' members. The type tells it
    from a row's own dict value (a raw blob, a vector): a row replacing
    members restructures the key, one replacing a value overwrites it."""

    __slots__ = ()


def _count_overwrite(tally, previous) -> None:
    """Count a row's value replacing `previous`, something already present,
    under the one reason that describes it: members `_set_nested` built (a
    `_Node` or an array) are a shape conflict, any row's own value, typed or
    a raw blob, a same-named overwrite. An empty `_Node` is an array filler
    and holds nothing."""
    if isinstance(previous, (_Node, list)):
        if previous:
            _bump(tally, "payload_shape_conflicts")
    else:
        _bump(tally, "property_key_collisions")


def _set_nested(root: dict, parts: list, value, tally=None):
    """Set `value` at `parts` (a parsed flat path such as
    'Rounds[0].Reports[0].DamageDealt') in a nested dict/list payload.

    Arrays grow with None/_Node fillers, and each array element gets an
    'Index' equal to its subscript, as the C# parser writes (compute_metrics
    dedups on inter.get("Index")).

    A replacement counts once. Rows that disagree about a key's shape ('Foo'
    scalar, 'Foo.Bar' nested) are `payload_shape_conflicts`: the second wins
    and the first's value is gone. A value landing on another row's value is
    `property_key_collisions`, a raw blob included: each of 02d4d478's 4,785
    `TrackedRewards[i].Rewards` overwrites comes from a different handle. A
    deeper row reaching a row's own dict value adds its members beside it and
    loses nothing, so it is not counted. Fillers and the injected Index hold
    nothing of the export's, so a real `Index` row replacing the injected one
    (TeamEconomy[i].Index on 13.01) is not counted.
    """
    obj = root
    element = None  # the subscript `obj` sits at, when it is an array element
    last = len(parts) - 1
    for i, (name, idx) in enumerate(parts):
        is_last = i == last
        if idx is not None:
            arr = obj.setdefault(name, [])
            if not isinstance(arr, list):
                _bump(tally, "payload_shape_conflicts")
                arr = obj[name] = []
            while len(arr) <= idx:
                arr.append(None if is_last else _Node())
            if is_last:
                if arr[idx] is not None:
                    _count_overwrite(tally, arr[idx])
                arr[idx] = value
            else:
                if not isinstance(arr[idx], dict):
                    if arr[idx] is not None:
                        _bump(tally, "payload_shape_conflicts")
                    arr[idx] = _Node()
                arr[idx].setdefault("Index", idx)
                obj = arr[idx]
                element = idx
        elif is_last:
            # An Index equal to the element's subscript is the injected one.
            if name in obj and not (name == "Index" and obj[name] == element):
                _count_overwrite(tally, obj[name])
            obj[name] = value
        else:
            nxt = obj.setdefault(name, _Node())
            if not isinstance(nxt, dict):
                _bump(tally, "payload_shape_conflicts")
                nxt = obj[name] = _Node()
            obj = nxt
            element = None


def _drop_padding_elements(node):
    """Remove the empty `_Node` fillers `_set_nested` appends to reach a
    sparse index.

    The reference emits only elements present, and a filler is harmful:
    compute_metrics sorts `{t["Index"]: t for t in teams}`, and a filler's
    None key raised TypeError, so the replay produced no metrics. Every
    genuine element carries at least its injected `Index`, so only fillers
    match. `None` fillers in scalar arrays stay: the position IS the index.
    """
    if isinstance(node, dict):
        for value in node.values():
            _drop_padding_elements(value)
    elif isinstance(node, list):
        node[:] = [e for e in node if e != {}]
        for value in node:
            _drop_padding_elements(value)
    return node


def _get_value(row_i64, row_f64, row_bool, row_str, row_raw, row_bits,
               tally=None):
    """A fields.parquet row's typed value, else its raw_bits blob:
    ``(value, is_raw)``.

    Exactly one typed column should be set. With more (value_i64 = 1 beside
    value_bool = False) the priority chain keeps the first and the row looks
    ordinary downstream, so it is counted as `multi_typed_rows`. raw_bits is
    not a typed column: it rides beside decoded values by design.
    """
    # Bools summed, not a generator: this runs per row (1.4 M on a replay).
    if ((row_i64 is not None) + (row_f64 is not None)
            + (row_bool is not None) + (row_str is not None)) > 1:
        _bump(tally, "multi_typed_rows")
    if row_i64 is not None:
        return row_i64, False
    if row_f64 is not None:
        return row_f64, False
    if row_bool is not None:
        return row_bool, False
    if row_str is not None:
        return row_str, False
    if row_raw is not None:
        return _raw_blob(row_raw, row_bits), True
    return None, False


def _raw_blob(row_raw, row_bits) -> dict:
    """The {BitCount, Data} blob of one row's raw bits, as the C# output has
    it. The one builder, so the key order (part of the bytes) has one source;
    a caller that labels the blob adds `TypeName` after these two."""
    return {"BitCount": row_bits,
            "Data": base64.b64encode(row_raw).decode('ascii')}


def _count_unbuilt_blobs(blob_state, tally) -> None:
    """Count the raw-sourced fields one event needed and did not get.

    `blob_state` maps each raw-sourced field the event touched (container row
    or decoded member) to whether its blob was built, or is `None`. Counted
    per event and field, so members arriving without their blob count once.
    """
    if blob_state:
        unbuilt = sum(1 for built in blob_state.values() if not built)
        if unbuilt:
            tally.bump("raw_blobs_unavailable", unbuilt)


def _split_rpc_field(field_name: str):
    """Split "Rpc.Param" into (rpc_name, param_name); param_name is None when
    the field_name is the function itself (no dot)."""
    name, dot, param = field_name.partition('.')
    return name, (param if dot else None)


# ---------------------------------------------------------------------------
# Actor class inference: map group_path to replication_class_path
# ---------------------------------------------------------------------------
def _group_path_to_class(gp: str) -> str:
    """An actor's class from its first group_path, for the legacy fallback
    without actors.parquet: a property group_path IS the class path, and an
    RPC group's is the class plus the _ClassNetCache suffix."""
    return gp.replace(CLASS_NET_CACHE_SUFFIX, '')


def _to_package_path(class_path: str) -> str:
    """Drop the `.ClassName_C` suffix, leaving the UE package path.

    actors.parquet has ".../Hunter_PC.Hunter_PC_C"; the reference's
    actor_spawned.replication_class_path is ".../Hunter_PC". Splitting on "."
    hides the difference, but ability_usage.top_classes and
    ability_detail.by_ability key on `path.split("/")[-1]` ("Foo.Foo_C" vs
    "Foo"). After stripping, every path shared with the reference on 02d4d478
    matches, with zero count mismatches.
    """
    slash = class_path.rfind("/")
    tail = class_path[slash + 1:]
    if "." not in tail:
        return class_path
    return class_path[: slash + 1] + tail.split(".", 1)[0]


def _group_path_to_archetype(gp: str) -> str:
    """'.../Wushu_PC.Wushu_PC_C' -> 'Default__Wushu_PC_C' (legacy fallback)."""
    if '.' in gp:
        leaf = gp.rsplit('.', 1)[-1]
        return f"Default__{leaf}"
    return f"Default__{gp.rsplit('/', 1)[-1]}"


# ---------------------------------------------------------------------------
# RPC parameter normalization
# ---------------------------------------------------------------------------
#: Damage RPC booleans the C# reference spells without vrfkit's 'b' prefix.
_DAMAGE_PARAM_RENAMES = {
    "bDamageKilledTarget": "DamageKilledTarget",
    "bAliveAfterDamage": "AliveAfterDamage",
    "bIsWallPenetration": "IsWallPenetration",
    "bEquippableUsedZoomed": "EquippableUsedZoomed",
    "bEquippableUsedInFocusMode": "EquippableUsedInFocusMode",
}


def _normalize_rpc_param(rpc_name: str, param: str, value, is_raw: bool,
                         tally=None) -> dict | None:
    """Rename/reshape one RPC parameter to the C# output; None drops it.

    Only the damage RPCs are reshaped ('b' booleans, RegionalDamage ordinals,
    vectors, EquippableUsed, blobs, DamagedBone); every other RPC's
    parameters pass through. Members of `LifeChangeEvents[]` and
    `LifeChangeBySection[]` (`LifeChangeEvents[0].LifeResult`, ...) are
    dropped: the parent blob row carries them in the expected shape, and on
    the damage RPCs a member arriving without it is counted
    (`raw_blobs_unavailable`, `_build_rpc_events`).
    """
    if "[" in param and param.split("[", 1)[0] in (
        "LifeChangeEvents",
        "LifeChangeBySection",
    ):
        return None
    if rpc_name not in DAMAGE_RPC_NAMES:
        return {param: value}

    out_name = _DAMAGE_PARAM_RENAMES.get(param, param)
    if param == "RegionalDamage" and not is_raw:
        value = REGIONAL_DAMAGE_MAP.get(value, f"regional_damage__unknown_{value}")
    elif param in DAMAGE_VECTOR_PARAMS:
        # Quantized vectors as "(x,y,z)"; the reference emits {x, y, z}. An
        # unparseable one stays as it came: visibly absent beats a zero vector.
        parsed = _parse_vector_or_none(value)
        if parsed is not None:
            value = parsed
    elif param == "EquippableUsed":
        # The decoded ObjectNetGuid -> the C# ValorantEquippable shape; an
        # undecoded value passes its bits through, never a guess. Name and
        # ClassPath null and Category "unknown", as C# emits them: weapon
        # instances are dynamic actors with no NetGuidCache path, and valplay
        # resolves the gun from actor_spawned (_actorindex).
        if isinstance(value, int) and not is_raw:
            value = {
                "NetGuid": value,
                "Name": None,
                "ClassPath": None,
                "Category": "unknown",
            }
    elif param == "LifeChangeEvents" or param in DEATH_MONTAGE_BLOB_PARAMS:
        # The reference's labelled blob {BitCount, Data, TypeName}; the RPC
        # loop builds it from raw_bits typed or not, so `is_raw` means "the
        # blob exists". The death-montage pair is typed ObjectNetGuid (an FXC_*
        # effect class, a pawn) in fields.parquet, and the loop hands its wire
        # bits back so the event keeps the reference's shape. Exact names: a
        # startswith also caught the 1-bit ...IsQueued bool in 632 events.
        if is_raw and isinstance(value, dict):
            value["TypeName"] = param
    elif param == "DamagedBone" and is_raw:
        # An FName the overlay decodes: all 632,906 MulticastNotifyDamage_Point
        # rows on the 1,018-export corpus are typed (2026-09-28; _Base carries
        # none). A raw one is published as null and counted, never rendered
        # from the bytes (that shipped mojibake once): valplay's `_bone_region`
        # files None under "other" but raises TypeError on a dict.
        _bump(tally, "damaged_bone_undecoded")
        value = None
    return {out_name: value}


# ---------------------------------------------------------------------------
# Conversion phases
#
# `events.sort` at the end is stable, so events tying on (packet_id, time_ms)
# keep the order the phases appended them in: keep the phase order (actors,
# properties, RPCs, server timeline) and the append order inside each phase.
# ---------------------------------------------------------------------------
#: The bundle manifest's shape version; valplay's resume marker records it and
#: rebuilds an older bundle. valplay's gate is an equality check
#: (`scripts/parse_replays.py` ADAPTER_SCHEMA_VERSION;
#: `valplay/tests/ci-stack-smoke.mjs` pins its worker image's constant to it),
#: so a bump refuses every existing bundle until valplay re-pins `VRFKIT_REF`.
#: Bump only when a bundle would be MISREAD by a consumer that ignores the
#: change.
#:
#: 1 -> 2 added `quality`, `net_field_export_groups` and `adapter`: a bundle
#: at 1 cannot tell "the export was complete" from "nobody counted".
#:
#: Additive, not bumped (valplay matches exact event types, keeps no type
#: allowlist and ignores unknown keys, so neither generation misreads the
#: other's bundle):
#: * `actor_dormant`, and `actor_event` beside `actor_closed`'s unchanged
#:   fields. Existing consumers become more correct: `actor_closed` no longer
#:   carries dormancies. Measured with valplay's `compute_ability_detail` on 8
#:   Sage walls (6 destroyed at 3,000 ms, 2 dormant at 40,000 ms): before, 8
#:   instances, cap 40,000 ms, 6 destroyed (75.0%); after, 6, cap 3,000 ms,
#:   0 (0.0%). valplay now reads `actor_dormant` as a right-censored lifetime.
#: * `level_names_and_times`: both generations fall back to the event path
#:   when it is absent.
#: * `server_timeline_event`: independent, server-labelled corroboration.
BUNDLE_SCHEMA_VERSION = 2


def _upstream_row_check(declared, observed) -> dict:
    """One declared-vs-observed row count. `agrees` is `None`, not `True`,
    when nothing was declared: a manifest without accounting must not
    certify itself."""
    return {
        "declared": declared,
        "observed": observed,
        "agrees": None if declared is None else declared == observed,
    }


def _public_level_names(value):
    """The level roots (the map URL) through a two-field allowlist: the
    neighbouring ``game_specific_data`` header can hold account subjects and
    loadouts, so nothing else of the header crosses."""
    if not isinstance(value, list):
        return None
    levels = []
    for row in value:
        if not isinstance(row, dict):
            continue
        name = row.get("name")
        time_ms = row.get("time_ms")
        if not isinstance(name, str) or not name:
            continue
        if isinstance(time_ms, bool) or not isinstance(time_ms, int):
            continue
        if not 0 <= time_ms <= 0xFFFF_FFFF:
            continue
        levels.append({"name": name, "time_ms": time_ms})
    return levels


def _write_manifest(manifest: dict, output_dir: Path, adapter: dict):
    """Write the bundle manifest, forwarding vrfkit's own accounting.

    Four things cross this seam (docs/USAGE.md, "What the bundle manifest
    carries", which also says why `players` does not): `quality` and
    `net_field_export_groups`, VERBATIM and null when absent, never a
    plausible zero (the handle table turns valplay's hardcoded `_roundinfo`
    names for handles 41-45 into a check); the public `level_names_and_times`,
    whose first root is the authoritative map URL; and `adapter`, this
    process's own measurements, kept apart so they cannot pass for upstream
    figures.
    """
    quality = manifest.get("quality")
    groups = manifest.get("net_field_export_groups")
    levels = _public_level_names(manifest.get("level_names_and_times"))
    out_manifest = {
        "replay_version": manifest.get("replay_version", "unknown"),
        # No default: compute_metrics.py reads it without one, so a 0 would
        # pass for a real match length.
        "duration_ms": manifest.get("duration_ms"),
        "replay_build": manifest.get("replay_build", ""),
        "replay_changelist": manifest.get("replay_changelist", 0),
        "source_file": manifest.get("source_file", ""),
        "source_size_bytes": manifest.get("source_size_bytes"),
        "converter": "vrfkit/tools/to_valplay_bundle.py",
        "bundle_schema_version": BUNDLE_SCHEMA_VERSION,
        "quality": quality if isinstance(quality, dict) else None,
        "net_field_export_groups": groups if isinstance(groups, list) else None,
        "level_names_and_times": levels,
        "adapter": adapter,
    }
    (output_dir / "manifest.json").write_text(
        json.dumps(out_manifest, indent=2), encoding='utf-8'
    )


def _dict_column_to_pylist(column):
    """A dictionary-encoded column as a list SHARING its string objects
    (`to_pylist` for any other column).

    `cast('string').to_pylist()` makes one str per row: on 02d4d478's
    fields.parquet 1,246,812 for `group_path`'s 443 values and 1,207,778 for
    `field_name`'s 3,954, a hundred-odd MB of duplicates, 2.3x slower.
    """
    arr = column.combine_chunks()
    if not pa.types.is_dictionary(arr.type):
        return column.to_pylist()
    values = arr.dictionary.to_pylist()
    return [values[i] if i is not None else None
            for i in arr.indices.to_pylist()]


def _numeric_column_to_pylist(column):
    """A numeric column with NO nulls as a Python list, via numpy: ~14x
    faster than `to_pylist` on 02d4d478's 1,246,812-row uint32 columns
    (250 ms -> 16 ms each). numpy has no integer NaN and would widen a
    nullable column to float64; use `_nullable_numeric_to_pylist` for those.
    """
    return column.to_numpy(zero_copy_only=False).tolist()


def _nullable_numeric_to_pylist(column):
    """A nullable numeric column as a list of (value | None), via numpy.

    Nulls are filled with a type-matched sentinel so `to_numpy` does not widen
    to float64, then the validity mask puts None back: equal to `to_pylist`
    in value and Python type, ~3x faster.
    """
    arr = column.combine_chunks()
    n = len(arr)
    if n == 0:
        return []
    mask = arr.is_valid().to_numpy(zero_copy_only=False).tolist()
    t = arr.type
    if pa.types.is_boolean(t):
        fill = False
    elif pa.types.is_floating(t):
        fill = 0.0
    else:
        fill = 0
    values = pc.fill_null(arr, fill).to_numpy(zero_copy_only=False).tolist()
    return [v if m else None for v, m in zip(values, mask)]


class _FieldColumns(NamedTuple):
    """fields.parquet as one Python list per column (row iteration is slow)."""

    n_rows: int
    time_ms: list
    packet_id: list
    actor: list
    obj: list
    channel: list
    group_path: list
    handle: list
    field_name: list
    bit_count: list
    raw_bits: list
    value_i64: list
    value_f64: list
    value_bool: list
    value_str: list


def _load_field_columns(fields_path: Path, verbose: bool) -> _FieldColumns:
    """Read fields.parquet and extract every column we consume."""
    t0 = time.time()
    if verbose:
        print("Loading fields.parquet...")
    table = pq.read_table(fields_path)
    n_rows = len(table)
    if verbose:
        print(f"  {n_rows:,} rows loaded in {time.time()-t0:.1f}s")

    t0 = time.time()
    if verbose:
        print("Extracting columns...")

    cols = _FieldColumns(
        n_rows=n_rows,
        time_ms=_numeric_column_to_pylist(table.column('time_ms')),
        packet_id=_numeric_column_to_pylist(table.column('packet_id')),
        actor=_numeric_column_to_pylist(table.column('actor_net_guid')),
        # Subobject identity. Null for actor blocks; the C# reference then
        # repeats the actor guid, so mirror that when emitting.
        obj=(
            _nullable_numeric_to_pylist(table.column('object_net_guid'))
            if 'object_net_guid' in table.schema.names
            else [None] * n_rows
        ),
        channel=_numeric_column_to_pylist(table.column('channel_index')),
        group_path=_dict_column_to_pylist(table.column('group_path')),
        handle=_numeric_column_to_pylist(table.column('handle')),
        field_name=_dict_column_to_pylist(table.column('field_name')),
        bit_count=_numeric_column_to_pylist(table.column('bit_count')),
        raw_bits=table.column('raw_bits').to_pylist(),
        value_i64=_nullable_numeric_to_pylist(table.column('value_i64')),
        value_f64=_nullable_numeric_to_pylist(table.column('value_f64')),
        value_bool=_nullable_numeric_to_pylist(table.column('value_bool')),
        value_str=_dict_column_to_pylist(table.column('value_str')),
    )

    if verbose:
        print(f"  Columns extracted in {time.time()-t0:.1f}s")
    return cols


def _group_rows(cols: _FieldColumns):
    """Group field rows into property and RPC groups (row indices by key),
    plus each actor's first/last appearance.

    RPC rows are those whose group_path contains '_ClassNetCache'. One pass:
    the dicts' insertion order decides how events tying on (packet_id,
    time_ms) are written, and splitting the loop would reshuffle them.
    """
    # First/last appearance: the lifetime fallback without actors.parquet.
    actor_first = {}  # actor_net_guid -> (time_ms, packet_id, group_path)
    actor_last = {}   # actor_net_guid -> (time_ms, packet_id)
    prop_groups = defaultdict(list)
    rpc_groups = defaultdict(list)
    # Counted, to compare with quality.net.unresolved_rpc_payloads_preserved.
    unresolved_cnc_rows = 0

    col_time = cols.time_ms
    col_pid = cols.packet_id
    col_actor = cols.actor
    col_obj = cols.obj
    col_gp = cols.group_path
    col_handle = cols.handle
    col_fn = cols.field_name

    for i in range(cols.n_rows):
        if col_fn[i] == UNRESOLVED_CLASS_NET_CACHE_PAYLOAD_FIELD_NAME:
            unresolved_cnc_rows += 1
            continue

        actor = col_actor[i]
        gp = col_gp[i]
        pid = col_pid[i]
        ms = col_time[i]

        if actor not in actor_first:
            actor_first[actor] = (ms, pid, gp)
        actor_last[actor] = (ms, pid)

        is_rpc = CLASS_NET_CACHE_SUFFIX in gp
        if is_rpc:
            handle = col_handle[i]
            rpc_groups[(pid, actor, gp, handle)].append(i)
        else:
            # Keyed by subobject too: a character's several ItemSlot
            # subobjects would otherwise merge into one slot.
            prop_groups[(pid, actor, col_obj[i], gp)].append(i)

    return actor_first, actor_last, prop_groups, rpc_groups, unresolved_cnc_rows


#: `actors.event` -> the bundle event type (`open` is actor_spawned). THREE
#: values: `dormant` is the server suspending replication of a live actor, NOT
#: a despawn (CLAUDE.md; extract_active_effects.py pairs the same column), and
#: valplay's ability_detail.py pairs spawn/close into a lifetime, so a dormant
#: settled smoke or wall published as a close read as destroyed. A type of its
#: own, not a flag on actor_closed, so consumers that do not know it ignore
#: it. An unmapped value becomes `actor_lifecycle_unknown`, counted.
_ACTOR_EVENT_TYPES = {
    "close": "actor_closed",
    "dormant": "actor_dormant",
}


# Event-chunk payload words are not self-describing: the closed vocabulary of
# crates/vrfkit/src/driver/mod.rs's residual-zero layout check. An unknown
# group still crosses as a labelled timestamp, with no words.
_SERVER_TIMELINE_WORD_COUNTS = {
    "characterDeath": 2,
    "characterUltimateUsed": 1,
    "roundStarted": 1,
    "switchTeams": 1,
    "spikePlanted": 0,
    "spikeDefused": 0,
    "spikeExploded": 0,
}

# Structural payload values measured over all 109,126 Event chunks of the
# 527-replay, three-build corpus: public Unreal enum constants, rechecked here
# before `payload_name` may cross. Tag, name and seconds cross only as a whole
# agreeing tuple; other rows keep the public group/times/words.
_SERVER_TIMELINE_PAYLOAD_TAGS = {
    "characterDeath": 8,
    "characterUltimateUsed": 11,
    "roundStarted": 2,
    "switchTeams": 3,
    "spikePlanted": 4,
    "spikeDefused": 5,
    "spikeExploded": 6,
}
_SERVER_TIMELINE_PAYLOAD_NAMES = {
    "characterDeath": "EReplayEventGroup::CharacterDeath",
    "characterUltimateUsed": "EReplayEventGroup::CharacterUltimateUsed",
    "roundStarted": "EReplayEventGroup::RoundStart",
    "switchTeams": "EReplayEventGroup::SwitchTeams",
    "spikePlanted": "EReplayEventGroup::SpikePlanted",
    "spikeDefused": "EReplayEventGroup::SpikeDefused",
    "spikeExploded": "EReplayEventGroup::SpikeExploded",
}
EVENT_PAYLOAD_TIME_TOLERANCE_MS = 1.001


class _PacketTimeIndex:
    """Order packet-less Event chunks among packet-ordered rows: an event time
    maps to the largest packet id seen at or before it. The prefix maximum
    stays monotone even when a bad frame gives a later packet an earlier time
    (the regression counter reports that). Ordering only, never published."""

    def __init__(self, cols: "_FieldColumns"):
        max_packet_at_time = {}
        for time_ms, packet_id in zip(cols.time_ms, cols.packet_id):
            previous = max_packet_at_time.get(time_ms)
            if previous is None or packet_id > previous:
                max_packet_at_time[time_ms] = packet_id
        self.times = sorted(max_packet_at_time)
        self.prefix_max_packets = []
        highest = 0
        for time_ms in self.times:
            highest = max(highest, max_packet_at_time[time_ms])
            self.prefix_max_packets.append(highest)

    def packet_at_or_before(self, time_ms: int) -> int:
        index = bisect_right(self.times, time_ms) - 1
        return self.prefix_max_packets[index] if index >= 0 else 0


def _build_server_timeline_events(export_dir: Path, cols: "_FieldColumns",
                                  verbose: bool) -> tuple[list, int | None]:
    """Publish the Event-chunk timeline through a privacy-safe allowlist.

    events.parquet also holds a replay-scoped id, free-form metadata and raw
    payload bytes, any of which may identify an account or match, so none
    crosses. The payload FString crosses only when it equals the group's
    public enum constant and the tag/time tuple matches the measured layout;
    older exports lack those columns. Returns ``(events, rows_read)``,
    rows_read None when the table is absent (not 0: it is reconciled).
    """
    path = export_dir / "events.parquet"
    if not path.exists():
        return [], None

    table = pq.read_table(path)
    groups = _dict_column_to_pylist(table.column("group"))
    time1 = _numeric_column_to_pylist(table.column("time1"))
    time2 = _numeric_column_to_pylist(table.column("time2"))
    word0 = _nullable_numeric_to_pylist(table.column("word0"))
    word1 = _nullable_numeric_to_pylist(table.column("word1"))

    def optional(name, read):  # an additive column an older export lacks
        if name in table.column_names:
            return read(table.column(name))
        return [None] * len(table)

    payload_tag = optional("payload_tag", _nullable_numeric_to_pylist)
    payload_name = optional("payload_name", pa.ChunkedArray.to_pylist)
    payload_seconds = optional("payload_seconds", _nullable_numeric_to_pylist)
    packet_index = _PacketTimeIndex(cols)
    events = []

    for (group, first_time, second_time, first_word, second_word, tag,
         payload_enum_name, seconds) in zip(
        groups, time1, time2, word0, word1, payload_tag, payload_name,
        payload_seconds
    ):
        event = {
            "type": "server_timeline_event",
            "time_ms": first_time,
            "time2_ms": second_time,
            "event_group": group,
        }
        word_count = _SERVER_TIMELINE_WORD_COUNTS.get(group, 0)
        if word_count >= 1 and first_word is not None:
            event["word0"] = first_word
        if word_count >= 2 and second_word is not None:
            event["word1"] = second_word
        expected_tag = _SERVER_TIMELINE_PAYLOAD_TAGS.get(group)
        expected_name = _SERVER_TIMELINE_PAYLOAD_NAMES.get(group)
        seconds_matches = (
            seconds is not None
            and math.isfinite(seconds)
            and abs(seconds * 1000.0 - first_time)
            <= EVENT_PAYLOAD_TIME_TOLERANCE_MS
        )
        if (
            expected_tag is not None
            and tag == expected_tag
            and payload_enum_name == expected_name
            and seconds_matches
        ):
            event["payload_tag"] = tag
            event["payload_name"] = payload_enum_name
            event["payload_seconds"] = _f32_shortest(seconds)
        events.append((packet_index.packet_at_or_before(first_time), first_time, event))

    if verbose:
        print(f"  {len(events):,} server timeline events from events.parquet")
    return events, len(table)


def _build_actor_events(export_dir: Path, actor_first: dict, actor_last: dict,
                        verbose: bool, tally: "_Tally"):
    """Build actor_spawned / actor_closed / actor_dormant events.

    Returns ``(events, guid_class)``; `guid_class` (actor GUID -> spawn class
    path) is filled in the same pass for the shots' weapon identity. `tally`
    is required: only its unknown-value counter says a fourth value appeared.
    """
    events = []
    guid_class = {}  # actor net guid -> spawn class path
    actor_event_counts = Counter()

    # actors.parquet is authoritative: class, archetype and location come from
    # the spawn data itself.
    actors_path = export_dir / "actors.parquet"
    if actors_path.exists():
        actors_table = pq.read_table(actors_path)
        a_time = actors_table.column('time_ms').to_pylist()
        a_pid = actors_table.column('packet_id').to_pylist()
        a_chan = actors_table.column('channel_index').to_pylist()
        a_guid = actors_table.column('actor_net_guid').to_pylist()
        a_event = actors_table.column('event').to_pylist()
        a_class = _dict_column_to_pylist(actors_table.column('class_path'))
        a_arch = _dict_column_to_pylist(actors_table.column('archetype_path'))
        a_sx = actors_table.column('spawn_x').to_pylist()
        a_sy = actors_table.column('spawn_y').to_pylist()
        a_sz = actors_table.column('spawn_z').to_pylist()
        a_spitch = actors_table.column('spawn_pitch').to_pylist()
        a_syaw = actors_table.column('spawn_yaw').to_pylist()
        a_sroll = actors_table.column('spawn_roll').to_pylist()

        for i in range(len(actors_table)):
            if a_event[i] == 'open':
                # Float32 spawn coordinates, written shortest. A missing one
                # stays null: only static actors lack spawn data (27 opens on
                # 02d4d478; for a dynamic actor with its location bit clear the
                # parser writes the wire's (0,0,0) default, pipeline.rs
                # read_optional_quantized_vector), and {0,0,0} would mix them
                # with the 66 that really spawn at the origin.
                has_loc = a_sx[i] is not None or a_sy[i] is not None or a_sz[i] is not None
                location = _vec3(
                    _f32_shortest(a_sx[i]) if a_sx[i] is not None else 0,
                    _f32_shortest(a_sy[i]) if a_sy[i] is not None else 0,
                    _f32_shortest(a_sz[i]) if a_sz[i] is not None else 0,
                ) if has_loc else None
                # Rotation is independent of location (a projectile at the
                # origin may carry a direction); null when all three are null.
                has_rotation = (
                    a_spitch[i] is not None
                    or a_syaw[i] is not None
                    or a_sroll[i] is not None
                )
                rotation = {
                    axis: _f32_shortest(value) if value is not None else 0
                    for axis, value in (
                        ("pitch", a_spitch[i]),
                        ("yaw", a_syaw[i]),
                        ("roll", a_sroll[i]),
                    )
                } if has_rotation else None
                class_path = a_class[i]
                if class_path:
                    # First open wins: a GUID reused after a close still
                    # belongs, for the shots, to its first life.
                    guid_class.setdefault(a_guid[i], class_path)
                # Null for a static actor, as in the reference; never a bare
                # "Default__".
                archetype = a_arch[i]
                # guid_class keeps the full object path the weapon lookup
                # matches; the event carries the reference's package path.
                event = {
                    "type": "actor_spawned",
                    "time_ms": a_time[i],
                    "actor_net_guid": a_guid[i],
                    "channel": a_chan[i],
                    "replication_class_path": _to_package_path(class_path) if class_path else None,
                    "archetype_path": archetype,
                    "location": location,
                    "rotation": rotation,
                }
                events.append((a_pid[i], a_time[i], event))
            else:
                # close, dormant, or a value this adapter does not know: see
                # _ACTOR_EVENT_TYPES, and BUNDLE_SCHEMA_VERSION for the bump.
                raw_event = a_event[i]
                event_type = _ACTOR_EVENT_TYPES.get(raw_event)
                if event_type is None:
                    # Its own type with the raw value, and counted: a visible
                    # unknown, never a plausible close.
                    tally.bump("unknown_actor_lifecycle_events")
                    event_type = "actor_lifecycle_unknown"
                actor_event_counts[event_type] += 1
                event = {
                    "type": event_type,
                    "time_ms": a_time[i],
                    "actor_net_guid": a_guid[i],
                    "channel": a_chan[i],
                    # The wire's value, so a consumer can audit the mapping.
                    "actor_event": raw_event,
                }
                events.append((a_pid[i], a_time[i], event))

        if verbose:
            print(f"  {len(actors_table):,} actor lifecycle events from actors.parquet")
            for name, count in sorted(actor_event_counts.items()):
                print(f"    {name}: {count:,}")
    else:
        # Legacy fallback: lifetimes from the first/last field row.
        for actor, (ms, pid, gp) in actor_first.items():
            class_path = _group_path_to_class(gp)
            archetype = _group_path_to_archetype(gp)
            event = {
                "type": "actor_spawned",
                "time_ms": ms,
                "actor_net_guid": actor,
                "replication_class_path": class_path,
                "archetype_path": archetype,
                "location": {"x": 0, "y": 0, "z": 0},
            }
            events.append((pid, ms, event))

        # No `event` column here: the last field row cannot tell a despawn
        # from dormancy, so `actor_event` is null, not "close".
        for actor, (ms, pid) in actor_last.items():
            event = {
                "type": "actor_closed",
                "time_ms": ms,
                "actor_net_guid": actor,
                "actor_event": None,
            }
            events.append((pid + 1, ms, event))

    return events, guid_class


def _build_property_events(cols: _FieldColumns, prop_groups: dict, tally: _Tally):
    """Build export_group_received events from the replicated-property groups.

    A row with no field name has no key to go under: dropped and counted. A
    group whose rows were all unnamed still emits an event, whose empty
    payload only the tally tells apart from a genuine existence signal.
    """
    col_time = cols.time_ms
    col_fn = cols.field_name
    col_handle = cols.handle
    col_bits = cols.bit_count
    col_raw = cols.raw_bits
    col_i64 = cols.value_i64
    col_f64 = cols.value_f64
    col_bool = cols.value_bool
    col_str = cols.value_str

    events = []
    for (pid, actor, obj, gp), row_indices in prop_groups.items():
        ms = col_time[row_indices[0]]

        # Two passes: vrfkit emits both the container row ("Rounds", the whole
        # array) and its decoded sub-fields ("Rounds[0].Reports[0]..."), the
        # elements first (stream.rs). Collect the names that have indexed
        # sub-fields, so the second pass skips their containers.
        indexed_names = set()
        for ri in row_indices:
            fn = col_fn[ri]
            if fn and '[' in fn:
                indexed_names.add(fn[:fn.index('[')])

        payload = {}
        # {RAW_BLOB_PREFERRED name: blob built?}, only once one is touched.
        blob_state = None
        for ri in row_indices:
            fn = col_fn[ri]
            if fn is None:
                tally.bump("unnamed_property_rows")
                continue
            value, is_raw = _get_value(
                col_i64[ri], col_f64[ri], col_bool[ri], col_str[ri],
                col_raw[ri], col_bits[ri], tally
            )
            type_name = RAW_BLOB_PREFERRED.get(fn)
            if type_name is not None:
                # A second row of this name replaces the first one's blob or
                # stand-in below, or is dropped behind its blob.
                if fn in payload:
                    tally.bump("property_key_collisions")
                # The RAW_BLOB_PREFERRED rule; `_get_value` ran above, so
                # multi_typed_rows still sees the row.
                if blob_state is None:
                    blob_state = {}
                raw = col_raw[ri]
                if raw is not None:
                    blob = _raw_blob(raw, col_bits[ri])
                    blob["TypeName"] = type_name
                    payload[fn] = blob
                    blob_state[fn] = True
                else:
                    # No bits, no blob: counted when the event is complete.
                    # A typed value goes in the blob's place (the consumer
                    # skips a non-blob, visibly) unless a blob was built.
                    blob_state.setdefault(fn, False)
                    if value is not None and not blob_state[fn]:
                        payload[fn] = value
                continue
            if value is None and not is_raw:
                continue

            # See "Combat report leaf labels".
            fn = _combat_report_leaf_name(gp, fn, col_handle[ri])

            is_bool = col_bool[ri] is not None
            fn = _normalize_prop_field_name(fn, is_bool)

            if fn in VECTOR_PROPERTIES and isinstance(value, str):
                parsed_vec = _parse_vector_or_none(value)
                if parsed_vec is not None:
                    value = parsed_vec
            elif fn in JSON_OBJECT_PROPERTIES and isinstance(value, str):
                # No try/except: the parser writes this column, so a value
                # that will not parse is a parser bug and stops the run.
                value = json.loads(value)

            parts = _parse_field_path(fn, tally)
            if len(parts) == 1 and parts[0][1] is None:
                # A container of indexed sub-fields is skipped, raw OR typed:
                # its elements carry the data. No typed container occurs on
                # the 1,018-export corpus (2026-09-28); typing one upstream
                # must not change the bundle.
                bare_name = parts[0][0]
                if bare_name in indexed_names:
                    continue
                # The parser flattens struct members and static-array elements
                # under one name only `handle` tells apart, so a repeat here is
                # a different property, not a newer copy: 24,060 of 02d4d478's
                # 28,845 property_key_collisions. Keyed by name, not handle:
                # valplay consumes names, and a handle key would change which
                # value survives.
                if bare_name in payload:
                    _count_overwrite(tally, payload[bare_name])
                payload[bare_name] = value
            elif parts[0][0] in RAW_BLOB_PREFERRED:
                # A decoded member of a raw-blob field is not published (the
                # blob carries it), so one whose event built no blob is a loss.
                if blob_state is None:
                    blob_state = {}
                blob_state.setdefault(parts[0][0], False)
            else:
                _set_nested(payload, parts, value, tally)

        _count_unbuilt_blobs(blob_state, tally)
        _drop_padding_elements(payload)

        # Emitted even when empty: some events are existence signals.
        event = {
            "type": "export_group_received",
            "time_ms": ms,
            "export_group_path": gp,
            "actor_net_guid": actor,
            "object_net_guid": obj if obj is not None else actor,
            "payload": payload,
        }
        events.append((pid, ms, event))

    return events


def _build_rpc_events(cols: _FieldColumns, rpc_groups: dict,
                      shot_ctx: _ShotContext, tally: _Tally):
    """Build rpc_received events, plus a valorant_shot_received for each shot RPC.

    Returns ``(events, shot_count, resolved_weapon_count)``. A shot RPC emits
    both, the shot first; the stable sort keeps that order on their tie.

    Two losses are counted, not fixed, because the export cannot say how:
    * one actor invoking a function TWICE in one packet makes one group (the
      key is packet_id, actor, group_path, handle), the second call's
      parameters overwriting the first's. Nothing marks where an invocation
      ends, and a guessed boundary would split real single invocations;
    * an unnamed row cannot name its parameter, and a group of only unnamed
      rows cannot name its function, so it is dropped whole.
    """
    col_time = cols.time_ms
    col_fn = cols.field_name
    col_bits = cols.bit_count
    col_raw = cols.raw_bits
    col_i64 = cols.value_i64
    col_f64 = cols.value_f64
    col_bool = cols.value_bool
    col_str = cols.value_str

    events = []
    shot_count = 0
    resolved_weapon_count = 0

    for (pid, actor, gp, handle), row_indices in rpc_groups.items():
        ms = col_time[row_indices[0]]

        rpc_name = None
        payload = {}
        effect_blobs = {}  # shot array parameter -> _EffectBlob
        # {raw-sourced parameter: blob built?}, only once one is touched.
        blob_state = None
        for ri in row_indices:
            fn = col_fn[ri]
            if fn is None:
                tally.bump("unnamed_rpc_rows")
                continue
            name, param = _split_rpc_field(fn)
            if rpc_name is None:
                rpc_name = name
            if param is None:
                # The row is the function itself: usually a zero-parameter RPC,
                # but 608 rows on 02d4d478 carry the whole parameter block as
                # undecoded bits (the descriptor bound no handles). Carried,
                # raw or typed (none is typed on the 1,018-export corpus,
                # 2026-09-28), so "payload: null" means only "no parameters".
                # Keyed under the function's name, a vrfkit-only convention:
                # the reference emits none of these (its 241 unbound groups).
                value, is_raw = _get_value(
                    col_i64[ri], col_f64[ri], col_bool[ri], col_str[ri],
                    col_raw[ri], col_bits[ri], tally
                )
                if value is not None:
                    if name in payload:
                        tally.bump("rpc_param_collisions")
                    payload[name] = value
                continue
            value, is_raw = _get_value(
                col_i64[ri], col_f64[ri], col_bool[ri], col_str[ri],
                col_raw[ri], col_bits[ri], tally
            )
            # The RAW_BLOB_PREFERRED rule for `_RAW_SOURCED_RPC_PARAMS`;
            # `_get_value` ran above, so multi_typed_rows still sees the row.
            sourced = _RAW_SOURCED_RPC_PARAMS.get(name)
            if sourced is not None:
                if param in sourced:
                    if blob_state is None:
                        blob_state = {}
                    raw = col_raw[ri]
                    if raw is not None:
                        bits = col_bits[ri]
                        if name == "ReplayPlayContinuousEffectAtLocation":
                            effect_blobs[param] = _EffectBlob(bytes(raw), bits)
                        value = _raw_blob(raw, bits)
                        is_raw = True
                        blob_state[param] = True
                    else:
                        # Counted when the invocation is complete; any typed
                        # value still goes out below.
                        blob_state.setdefault(param, False)
                elif "[" in param:
                    member_of = param.split("[", 1)[0]
                    if member_of in sourced:
                        # A decoded member, dropped by `_normalize_rpc_param`
                        # because the blob carries it: the blob is expected.
                        if blob_state is None:
                            blob_state = {}
                        blob_state.setdefault(member_of, False)
            if value is None and not is_raw:
                continue
            param_out = _normalize_rpc_param(rpc_name, param, value, is_raw, tally)
            if param_out is not None:
                for k, v in param_out.items():
                    if k in payload:
                        tally.bump("rpc_param_collisions")
                    payload[k] = v

        _count_unbuilt_blobs(blob_state, tally)

        if rpc_name is None:
            tally.bump("unnamed_rpc_invocations")
            continue

        if rpc_name == "ReplayPlayContinuousEffectAtLocation":
            # No blob guard: 7 of 02d4d478's 2,647 invocations carry only
            # scalar params and an undecoded EffectContainer, effect events
            # the reference emits and valplay's "unknown" bucket expects.
            first = row_indices[0]
            shot_event = _build_shot_event(
                shot_ctx, ms, pid, actor,
                cols.obj[first] if cols.obj[first] is not None else actor,
                cols.channel[first], payload,
                _EffectBlobs(*map(effect_blobs.get, (
                    "FloatValues", "ObjectValues", "VectorValues"))),
                tally=tally,
            )
            events.append((pid, ms, shot_event))
            shot_count += 1
            if shot_event["shot"]["equippable"] is not None:
                resolved_weapon_count += 1

        event = {
            "type": "rpc_received",
            "time_ms": ms,
            "function_name": rpc_name,
            "actor_net_guid": actor,
            "payload": payload if payload else None,
        }
        events.append((pid, ms, event))

    return events, shot_count, resolved_weapon_count


def _write_events(events: list, output_dir: Path, verbose: bool) -> tuple[int, int]:
    """Stable-sort on (packet_id, time_ms) and write events.ndjson.

    Returns ``(events_written, time_ms_regressions)``. packet_id is the wire's
    only total order (driver/mod.rs `total_packets`, never sorted), so the
    sort re-asserts it and ties keep the phase order ("Conversion phases").
    time_ms is not guaranteed monotonic: vrf-frame reads it with a bare
    `read_f32` and a non-finite frame gives 0. valplay's (time_ms, line)
    tie-break needs it non-decreasing, so a regression is counted and
    published, never repaired away from packet order (docs/USAGE.md, "What
    the bundle manifest carries").
    """
    t0 = time.time()
    if verbose:
        print("Sorting and writing events.ndjson...")
    events.sort(key=lambda x: (x[0], x[1]))

    events_written = 0
    regressions = 0
    previous_ms = None
    encode = _JSON.encode
    with open(output_dir / "events.ndjson", 'w', encoding='utf-8') as f:
        write = f.write
        for _, time_ms, evt in events:
            if previous_ms is not None and time_ms < previous_ms:
                regressions += 1
            previous_ms = time_ms
            write(encode(evt))
            write('\n')
            events_written += 1

    if verbose:
        print(f"  {events_written:,} events written in {time.time()-t0:.1f}s")
        if regressions:
            print(f"  {regressions:,} time_ms regressions in written order")
    return events_written, regressions


#: Every movement.parquet column `_write_movement` reads. The export declares
#: all of them non-null (vrf-export/src/tables/movement.rs).
_MOVEMENT_COLUMNS = ("time_ms", "packet_id", "character_net_guid",
                     "pos_x", "pos_y", "pos_z", "vel_x", "vel_y", "vel_z",
                     "yaw", "pitch")


def _join_movement_block(pieces) -> memoryview:
    """Join one block's pieces (literal fragments and slot text arrays) row by
    row and return the lines' bytes, back to back in the data buffer.

    A NULL line is refused: `binary_join_element_wise` makes one for any null
    input, and it adds no bytes, so the line would vanish while still counted
    as written. `_write_movement` refuses null columns first; this is the
    second line of defence.
    """
    lines = pc.binary_join_element_wise(*pieces, "")
    if lines.null_count:
        raise RuntimeError(
            f"{lines.null_count:,} movement lines came out NULL; writing the "
            "block would drop them while still counting them as written")
    return _string_bytes(lines)


def _string_bytes(arr) -> memoryview:
    """A pa.string() array's values as one span of bytes: the data buffer
    between its first and last offsets, not the whole buffer (a slice starts
    earlier, and Arrow may allocate more than it fills)."""
    if arr.type != pa.string():
        raise RuntimeError(f"expected a string array, got {arr.type}")
    offsets = numpy.frombuffer(arr.buffers()[1], dtype=numpy.int32,
                               count=len(arr) + 1, offset=arr.offset * 4)
    return memoryview(arr.buffers()[2])[offsets[0]:offsets[-1]]


def _write_movement(movement_path: Path, output_dir: Path, verbose: bool) -> tuple:
    """Write movement.ndjson, keeping the last sub-move per (packet, character).

    Returns ``(rows_read, rows_written, non_finite_rows)``. Only rows_read
    (None when the table is absent) compares with the export's declared
    count; rows_written is short by the intended collapse. non_finite_rows
    counts WRITTEN rows with a non-finite value, spelled Infinity/-Infinity/
    NaN as everywhere in this bundle: Python's json reads them, a strict
    parser rejects the line, and valplay (orjson when installed) then
    recounts fewer rows than `adapter.movement_rows_written` and refuses to
    publish. The count names the cause.
    """
    if not movement_path.exists():
        # Written empty, not skipped: `convert` requires movement.ndjson, and
        # an empty file says "this replay has no movement".
        (output_dir / "movement.ndjson").write_text("", encoding='utf-8')
        if verbose:
            print("  movement.parquet not found, movement.ndjson written empty")
        return None, 0, 0

    t0 = time.time()
    if verbose:
        print("Converting movement.parquet...")
    mv_table = pq.read_table(movement_path, columns=list(_MOVEMENT_COLUMNS))
    n_mv = len(mv_table)

    # Declared non-null, and load-bearing: `to_numpy` turns a uint32 column
    # with a null into float64 NaN, so every line would carry `1000.0`-style
    # times or `nan`. Refused, naming the column.
    for name in _MOVEMENT_COLUMNS:
        nulls = mv_table.column(name).null_count
        if nulls:
            raise ValueError(f"movement.parquet column {name!r} carries "
                             f"{nulls:,} nulls; the export declares it non-null")

    # numpy arrays, as in _numeric_column_to_pylist; no nulls, so dtypes stay.
    mv_time = mv_table.column('time_ms').to_numpy(zero_copy_only=False)
    mv_pid = mv_table.column('packet_id').to_numpy(zero_copy_only=False)
    mv_char = mv_table.column('character_net_guid').to_numpy(zero_copy_only=False)
    mv_px = mv_table.column('pos_x').to_numpy(zero_copy_only=False)
    mv_py = mv_table.column('pos_y').to_numpy(zero_copy_only=False)
    mv_pz = mv_table.column('pos_z').to_numpy(zero_copy_only=False)
    mv_yaw = mv_table.column('yaw').to_numpy(zero_copy_only=False)
    mv_pitch = mv_table.column('pitch').to_numpy(zero_copy_only=False)
    mv_vx = mv_table.column('vel_x').to_numpy(zero_copy_only=False)
    mv_vy = mv_table.column('vel_y').to_numpy(zero_copy_only=False)
    mv_vz = mv_table.column('vel_z').to_numpy(zero_copy_only=False)

    # Keep only the last sub-move per (packet_id, character). The decoder
    # emits every sub-move of a packet's move chain, all at one time_ms: 1,687
    # of 02d4d478's 2,387 extra rows carry distinct positions, none of the
    # reference's rows is missing, and movement.parquet keeps them all. The
    # bundle cannot: valplay's posture.py skips a dt=0 leg but still advances
    # last_sample, so distance_m came out 3.1-5.2 m LOW for every player. The
    # reference keeps the final move per packet; so does this, matching its
    # movement_detail on 60/60 values with no rounding.
    #
    # Keyed on the PACKET, never the millisecond: stream.rs
    # (decode_movement_rpc) hoists time_ms and packet_id before walking the
    # chain, so both collapse the same sub-moves, but the millisecond also
    # merges two packets landing in one ms, whose earlier final move is a
    # real sample. packet_id is therefore required (vrf-export's movement
    # table always had it): an export without it raises, never silently
    # falls back to the millisecond.
    #
    # Vectorised: the first index of each packed (packet << 32 | char) uint64
    # in the reversed array is its last in the original; np.sort restores row
    # order. Both keys are uint32, so the pack cannot collide.
    key64 = (mv_pid.astype(numpy.uint64) << numpy.uint64(32)) | mv_char.astype(numpy.uint64)
    first_in_rev = numpy.unique(key64[::-1], return_index=True)[1]
    keep = numpy.sort((n_mv - 1) - first_in_rev)
    del key64, first_in_rev
    movement_collapsed = n_mv - len(keep)

    # Positions and velocities are float32: widening prints the binary
    # artefact (349.989990234375 where the reference shows 349.99; it
    # surfaced in position_bbox). yaw and pitch are NOT shortened: the
    # reference writes them widened (253.289794921875); over 4,000 reference
    # rows yaw and pitch are float32-exact 4000/4000, position.x 25/4000.
    # Shortening them (3d37c68) moved 1,821,648 yaw and 1,699,418 pitch rows
    # and changed no metric. One float column alive at a time, in
    # _MOVEMENT_LINE's slot order: all eight cost +35-83 MB of peak working
    # set across 11 exports.
    non_finite = numpy.zeros(len(keep), dtype=bool)
    float_texts = []
    for column, shorten in ((mv_px, True), (mv_py, True), (mv_pz, True),
                            (mv_vx, True), (mv_vy, True), (mv_vz, True),
                            (mv_yaw, False), (mv_pitch, False)):
        kept = column[keep]
        non_finite |= ~numpy.isfinite(kept)
        float_texts.append(_json_scalar_column(kept, shorten=shorten))
        del kept
    non_finite_rows = int(numpy.count_nonzero(non_finite))
    movement_written = len(keep)
    # The uint32 time_ms and character_net_guid are nearly all distinct, so
    # not deduplicated: Arrow's integer-to-string cast writes the same digits
    # the JSON encoder gives an int.
    int_slots = (pa.array(mv_time[keep]), pa.array(mv_char[keep]))
    # Dropped now, to stay out of the write loop's peak.
    del (mv_table, mv_time, mv_pid, mv_char, mv_px, mv_py, mv_pz,
         mv_vx, mv_vy, mv_vz, mv_yaw, mv_pitch, keep, non_finite)

    # Lines are assembled in Arrow one block at a time (per-slot texts
    # joined with _MOVEMENT_LINE's fragments; see _json_scalar_column), byte
    # for byte what the per-row join wrote (MovementLineAssemblyTests;
    # sha256-identical on 11 exports). Written in binary mode, with the line
    # ending the text-mode file had: os.linesep, deliberately.
    fragments = [pa.scalar(text, type=pa.string()) for text in
                 _MOVEMENT_LINE.replace("\n", os.linesep).split("%s")]
    if len(fragments) != 2 + len(float_texts) + 1:
        raise RuntimeError("_MOVEMENT_LINE's slots no longer match the columns")
    with open(output_dir / "movement.ndjson", "wb") as f:
        for start in range(0, movement_written, _MOVEMENT_BLOCK_ROWS):
            length = min(_MOVEMENT_BLOCK_ROWS, movement_written - start)
            slots = [pc.cast(values.slice(start, length), pa.string())
                     for values in int_slots]
            slots += [texts.take(inverse.slice(start, length))
                      for texts, inverse in float_texts]
            pieces = [fragments[0]]
            for slot, fragment in zip(slots, fragments[1:]):
                pieces += (slot, fragment)
            f.write(_join_movement_block(pieces))

    if verbose and movement_collapsed:
        print(f"  {movement_collapsed:,} intra-packet sub-moves collapsed "
              f"(kept in movement.parquet)")

    if verbose:
        print(f"  {movement_written:,} movement rows written in {time.time()-t0:.1f}s")
    return n_mv, movement_written, non_finite_rows


# ---------------------------------------------------------------------------
# Main conversion
# ---------------------------------------------------------------------------
def _convert_into(export_dir: Path, output_dir: Path, *, verbose: bool = False):
    """Read vrfkit Parquet export and write valplay-compatible bundle."""
    fields_path = export_dir / "fields.parquet"
    movement_path = export_dir / "movement.parquet"
    manifest_path = export_dir / "manifest.json"

    tally = _Tally()

    # An absent manifest leaves a bundle that does not look damaged: plausible
    # header defaults ("unknown", 0, "") and an empty tag table, so every
    # shot reports null ammo, firing state, player and attack vectors. So it
    # is counted, here and below once it is known whether any shot paid.
    manifest = {}
    if manifest_path.exists():
        manifest = json.loads(manifest_path.read_text(encoding='utf-8'))
    else:
        tally.bump("missing_manifest")
    upstream_quality = manifest.get("quality")
    if not isinstance(upstream_quality, dict):
        # Not a loss (older vrfkit emitted none), so not counted; published
        # as `"quality": null`, "nobody counted", never a plausible zero.
        upstream_quality = None

    cols = _load_field_columns(fields_path, verbose)

    t0 = time.time()
    if verbose:
        print("Grouping rows into events...")
    (actor_first, actor_last, prop_groups, rpc_groups,
     unresolved_cnc_rows) = _group_rows(cols)
    if verbose:
        print(f"  {len(prop_groups):,} property events, {len(rpc_groups):,} RPC invocations")
        print(f"  Grouped in {time.time()-t0:.1f}s")

    t0 = time.time()
    if verbose:
        print("Building event records...")

    # The containment chain for weapon identity, before the actor pass so
    # guid_class fills in the loop that emits actor_spawned. The phases run
    # in "Conversion phases" order: actors, properties, RPCs, timeline.
    guid_outer, guid_path, net_guid_rows_read = _load_net_guids(export_dir)

    events, guid_class = _build_actor_events(
        export_dir, actor_first, actor_last, verbose, tally
    )

    events += _build_property_events(cols, prop_groups, tally)

    def equippable_lookup(firing_state_guid):
        return _resolve_equippable(firing_state_guid, guid_outer, guid_path, guid_class)

    def fire_mode_lookup(firing_state_guid, source_id):
        return _resolve_fire_mode(firing_state_guid, source_id, guid_outer, guid_path)

    shot_ctx = _ShotContext(
        _build_tag_table(manifest), equippable_lookup, fire_mode_lookup,
    )
    rpc_events, shot_count, resolved_weapon_count = _build_rpc_events(
        cols, rpc_groups, shot_ctx, tally
    )
    events += rpc_events

    timeline_events, server_timeline_rows_read = _build_server_timeline_events(
        export_dir, cols, verbose
    )
    events += timeline_events

    # An empty tag table costs only a replay with shots.
    if shot_count and not shot_ctx.tag_table:
        tally.bump("empty_gameplay_tag_table")

    if verbose:
        print(f"  {len(events):,} total events built in {time.time()-t0:.1f}s")
        print(f"  {shot_count:,} valorant_shot_received events")
        pct = 100 * resolved_weapon_count / shot_count if shot_count else 0
        print(f"  {resolved_weapon_count:,} with a resolved weapon ({pct:.2f}%)")

    events_written, time_ms_regressions = _write_events(events, output_dir, verbose)
    if time_ms_regressions:
        # Not a loss (every event is written), but it breaks the consumer's
        # (time_ms, line index) tie-break, so it is surfaced with the losses.
        tally.bump("events_time_ms_regressions", time_ms_regressions)

    movement_rows_read, movement_written, non_finite_movement_rows = _write_movement(
        movement_path, output_dir, verbose
    )
    # Not a loss either, but a strict parser rejects the line.
    tally.bump("non_finite_movement_rows", non_finite_movement_rows)

    # Declared vs read back: only exact table-height identities are judged
    # (movement_rows, net_guid_rows, event_rows). `quality.net.fields` is not
    # one: fields.parquet adds array leaves, struct sub-fields and
    # preservation rows (1,277,658 rows against 429,637 fields on 02d4d478),
    # and a permanent false alarm is how a real one stops being read.
    declared = upstream_quality or {}
    upstream_row_counts = {
        "movement_rows": _upstream_row_check(
            declared.get("movement_rows"), movement_rows_read
        ),
        "net_guid_rows": _upstream_row_check(
            declared.get("net_guid_rows"), net_guid_rows_read
        ),
        "event_rows": _upstream_row_check(
            declared.get("event_rows"), server_timeline_rows_read
        ),
    }
    disagreements = sum(
        1 for check in upstream_row_counts.values() if check["agrees"] is False
    )
    if disagreements:
        # Which side is wrong is not knowable here: counted, never repaired
        # or refused; the consumer decides what such a bundle may publish.
        tally.bump("upstream_row_count_disagreement", disagreements)

    adapter_accounting = {
        "schema_version": BUNDLE_SCHEMA_VERSION,
        "events_written": events_written,
        "events_time_ms_regressions": time_ms_regressions,
        "movement_rows_read": movement_rows_read,
        "movement_rows_written": movement_written,
        "net_guid_rows_read": net_guid_rows_read,
        "server_timeline_rows_read": server_timeline_rows_read,
        "server_timeline_events_written": len(timeline_events),
        "field_rows_read": cols.n_rows,
        "field_rows_unresolved_class_net_cache": unresolved_cnc_rows,
        "upstream_row_counts": upstream_row_counts,
        # Every counter, zeros included: an absent key could not tell a clean
        # run from one that never counted.
        "losses": dict(tally),
    }
    _write_manifest(manifest, output_dir, adapter_accounting)

    return {
        "events_written": events_written,
        "movement_written": movement_written,
        "tally": tally,
    }


def _print_summary(output_dir: Path, result: dict) -> None:
    """The console summary, from `convert` after the publish commit, so it
    names the final path; "complete" only when nothing was lost."""
    tally = result["tally"]
    if tally.total:
        print(f"\nConversion finished WITH LOSSES: {output_dir}")
    else:
        print(f"\nConversion complete: {output_dir}")
    print(f"  events.ndjson:   {result['events_written']:,} lines")
    print(f"  movement.ndjson: {result['movement_written']:,} lines")
    print("  manifest.json:   written")
    for line in tally.lines():
        print(line)


def _validate_separate_trees(export_dir: Path, output_dir: Path) -> tuple[Path, Path]:
    """Reject equal, nested, and aliased input/output trees before any write."""
    source = export_dir.resolve()
    destination = output_dir.resolve()
    if (
        source == destination
        or source.is_relative_to(destination)
        or destination.is_relative_to(source)
    ):
        raise ValueError(
            f"export and output directories must not overlap: {source} / {destination}"
        )
    return source, destination


def _publish_bundle(staging: Path, output_dir: Path) -> None:
    """Publish a verified staged bundle, rolling back an existing bundle."""
    parent = output_dir.parent.resolve()
    require_descendant(staging, parent)
    require_descendant(output_dir, parent)
    backup = Path(tempfile.mkdtemp(prefix=f".{output_dir.name}.backup.", dir=parent))
    # mkdtemp reserves a collision-free name; os.replace requires it absent.
    backup.rmdir()
    moved_old = False
    try:
        if output_dir.exists():
            os.replace(output_dir, backup)
            moved_old = True
        os.replace(staging, output_dir)
    except BaseException:
        if moved_old and backup.exists() and not output_dir.exists():
            os.replace(backup, output_dir)
        raise
    if backup.exists():
        try:
            remove_tree(backup, parent)
        except OSError as exc:
            # Past the commit point, os.replace(staging, output_dir), failure
            # would be false and invite a retry over the new bundle; the old
            # one is left recoverable.
            print(
                f"warning: published {output_dir}, but could not remove old "
                f"backup {backup}: {exc}",
                file=sys.stderr,
            )


def convert(export_dir: Path, output_dir: Path, *, verbose: bool = False):
    """Transactionally convert one export without modifying either old tree."""
    export_dir, output_dir = _validate_separate_trees(export_dir, output_dir)
    fields_path = export_dir / "fields.parquet"
    if not fields_path.is_file():
        raise FileNotFoundError(f"fields.parquet not found in {export_dir}")

    output_dir.parent.mkdir(parents=True, exist_ok=True)
    parent = output_dir.parent.resolve()
    require_descendant(output_dir, parent)
    staging = Path(tempfile.mkdtemp(prefix=f".{output_dir.name}.", dir=parent))
    try:
        result = _convert_into(export_dir, staging, verbose=verbose)
        manifest = json.loads((staging / "manifest.json").read_text(encoding="utf-8"))
        if not isinstance(manifest, dict):
            raise ValueError("generated manifest.json is not a JSON object")
        for required in ("events.ndjson", "movement.ndjson"):
            if not (staging / required).is_file():
                raise ValueError(f"generated bundle is missing {required}")
        _publish_bundle(staging, output_dir)
    except BaseException:
        if staging.exists():
            remove_tree(staging, parent)
        raise
    _print_summary(output_dir, result)
    return result


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------
def main():
    # ~680k event dicts live until the sort, and the cyclic collector rescans
    # them for cycles that do not exist: disabled, a 53 MB replay went from
    # 6.58 s to 5.43 s, peak RSS unchanged at 2.2 GB. Here, not at import:
    # the test suite imports this module.
    gc.disable()

    parser = argparse.ArgumentParser(
        description="Convert vrfkit Parquet export to valplay NDJSON bundle"
    )
    parser.add_argument("export_dir", type=Path,
                        help="vrfkit export directory (contains fields.parquet)")
    parser.add_argument("-o", "--output", type=Path, default=None,
                        help="Output bundle directory (default: out/valplay_bundle/<stem>)")
    parser.add_argument("-v", "--verbose", action="store_true",
                        help="Print progress messages")
    args = parser.parse_args()

    export_dir = args.export_dir.resolve()
    if args.output:
        output_dir = args.output.resolve()
    else:
        manifest_path = export_dir / "manifest.json"
        if manifest_path.exists():
            m = json.loads(manifest_path.read_text(encoding='utf-8'))
            source = m.get("source_file", "")
            stem = Path(source).stem if source else export_dir.name
        else:
            stem = export_dir.name
        output_dir = Path(__file__).resolve().parent.parent / "out" / "valplay_bundle" / stem

    convert(export_dir, output_dir, verbose=args.verbose)


if __name__ == "__main__":
    main()
