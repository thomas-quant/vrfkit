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
        """One line per counter that fired, in REASONS order; the manifest's
        `losses` carries every counter, zeros included (docs/USAGE.md)."""
        return [f"  {name}: {self[name]:,} -- {reason}"
                for name, reason in self.REASONS.items() if self[name]]


def _bump(tally, name: str, n: int = 1) -> None:
    """Increment a counter; a `None` tally is a no-op, so the leaf helpers stay
    callable and testable alone. The conversion phases always pass one."""
    if tally is not None:
        tally.bump(name, n)


# ---------------------------------------------------------------------------
# Scalar and vector formatting
#
# Three distinct rounding/precision policies live here and the differences are
# load-bearing; the names say which is which.
# ---------------------------------------------------------------------------
def _f32_shortest(value):
    """Shortest decimal that round-trips through float32.

    actors.parquet stores spawn coordinates as Float32. Widening one to a
    Python float exposes the binary artefact -- 2382.2f becomes
    2382.199951171875 -- while the C# reference serializes the float itself,
    which System.Text.Json writes with float (not double) round-trip
    precision: "2382.2".

    Reproducing that means finding the fewest significant digits that still
    round-trip as a float32, which is exactly what a shortest-round-trip
    float formatter does.
    """
    if value is None:
        return None
    packed = _struct.unpack("f", _struct.pack("f", value))[0]
    for digits in range(1, 10):
        candidate = float(f"{packed:.{digits}g}")
        # Rounding up can carry a candidate past the float32 range: FLT_MAX at
        # four digits is 3.403e+38. Such a candidate cannot round-trip, so it
        # is not the answer. Python 3.12 packs it as inf and the comparison
        # below rejects it; Python 3.13 raises OverflowError instead, which
        # must mean the same thing here rather than abort the conversion.
        try:
            back = _struct.unpack("f", _struct.pack("f", candidate))[0]
        except OverflowError:
            continue
        if back == packed:
            return int(candidate) if candidate.is_integer() else candidate
    return int(packed) if float(packed).is_integer() else packed


#: One encoder, reused. `json.dumps` with non-default kwargs cannot use the
#: module's cached encoder and CONSTRUCTS A NEW JSONEncoder on every call --
#: 2.4 million of them here, 1.8 s of pure setup. `.encode()` on a single
#: instance is the same code path with the same output.
_JSON = json.JSONEncoder(separators=(',', ':'), ensure_ascii=True)

#: The movement record, as a template with one `%s` per slot. Key order is the
#: order the record dict used, which is what `json.dumps` emitted; every slot
#: is filled with text `_JSON.encode` produced for that value, so the line is
#: byte-for-byte what encoding the dict would have written. `_write_movement`
#: splits it on `%s` into the literal fragments it interleaves with the slot
#: texts, so this stays the one place the line is spelled.
_MOVEMENT_LINE = (
    '{"time_ms":%s,"shooter_character_net_guid":%s,'
    '"position":{"x":%s,"y":%s,"z":%s},'
    '"velocity":{"x":%s,"y":%s,"z":%s},'
    '"yaw":%s,"pitch":%s}\n'
)

#: Rows per block of movement.ndjson assembled in Arrow and written at once.
#: Bounds the text held at any moment to one block's worth -- ~48 MB at the
#: ~184 bytes a line measures on the reference exports -- instead of the
#: whole file's. Measured on f73d4475's writer alone, 3 runs each: 2**16 and
#: 2**18 are equal within noise (~3.4 s, ~665 MB peak, which the per-column
#: dedup sets, not the block), 2**14 is ~4% slower, 2**20 adds 50-80 MB.
_MOVEMENT_BLOCK_ROWS = 1 << 18


#: Below this magnitude every integer is exactly representable in float32, so
#: the shortest round-trip text of an integral float32 IS its integer text.
#: From here up the two part ways: 123456792.0 round-trips as 123456790.
_F32_EXACT_INT_LIMIT = 2 ** 24

#: numpy's float32 text is positional only for 1e-4 <= |v| < 1e6, judged on
#: the binary value; Python's float repr -- where the per-value rule ends --
#: is positional from 1e-4 to 1e16, judged on the decimal. Outside the
#: narrower band they disagree: 1234567.5 is '1.2345675e+06' to numpy, and
#: float32(1e-4), which is 9.99999975e-05, is '1e-04' to numpy and '0.0001'
#: to repr. Compared in float64, because 1e-4 is not a float32.
_F32_POSITIONAL_BAND = (1e-4, 1e6)


def _json_scalar_column(arr, *, shorten=False):
    """The JSON TEXT of each value, computed once per distinct value.

    `arr` is a 1-D numpy float array. Returns ``(texts, inverse)``: `texts`
    is a pa.string() array with the text of each distinct value, `inverse` a
    pa.int32() array mapping every row of `arr` to its entry, so
    `texts.take(inverse)` is the column's text row by row. `numpy.unique`
    collapses the column in C and the caller fans the text back out with
    Arrow's `take`, one write block at a time -- there is no per-row Python
    loop anywhere, which is what lets `_write_movement` skip building 1.8
    million dicts and calling the encoder 1.8 million times. The fan-out is
    the caller's; the text rule is here, and only here.

    The contract is per value, and it is checked, not assumed: every row's
    text is what the per-element version this replaced wrote for that row's
    own value -- `_JSON.encode(_f32_shortest(v))` for `shorten=True`,
    `_JSON.encode(v)` for `shorten=False` (`MovementTextRuleTests`).

    Distinct BIT PATTERNS, not distinct values. -0.0 == 0.0, so a value-level
    unique merged the two zeros into one entry and wrote whichever sign the
    sort put first for every zero in the column -- a genuine `0.0` yaw could
    come out `-0.0`. Unique over the raw bits keeps them apart, and gives
    each NaN payload its own entry (all spelled `NaN`). No movement column on
    the corpus holds a -0.0 (0 in 1,973,922,078 rows x 8 columns, 1,018
    exports, 2026-09-28), so this moved no line: it closes the case instead
    of depending on its absence.

    `shorten=False` encodes each distinct value, `Infinity` and `NaN`
    included -- an f-string would spell those `inf` and `nan`, invalid JSON.

    `shorten=True` takes a vectorised shortcut only where it is proven equal
    to the per-value rule, and the per-value encoder everywhere else:

    * an integral value below `_F32_EXACT_INT_LIMIT` is written as its int,
      the way `_f32_shortest` returns an `int` for it;
    * a non-integral value inside `_F32_POSITIONAL_BAND` is numpy's
      `astype(str)` -- the same Dragon4 shortest round-trip, in the same
      positional notation.

    Both are checked EXHAUSTIVELY against the per-value rule, not sampled:
    all 556,160,338 non-integral float32 inside the band (278,080,169 of each
    sign) and all 33,554,430 integral ones with 0 < |v| < 2**24, 0
    mismatches (numpy 2.5.2, 2026-09-28). The same run shows the edges are
    real: just outside the band, float32(+/-1e-4) and every non-integral
    value in 1e6 <= |v| < 2**20 differ.

    The shortcut used to be applied to every value, and outside that domain
    it is wrong:

    * +/-inf pass `v == trunc(v)` and went through the int64 cast, which
      wrote -9223372036854775808 -- valid JSON, a plausible number, the wrong
      sign -- with a numpy RuntimeWarning on stderr as the only signal;
    * NaN fell through to `astype(str)` and was written `nan`, which no JSON
      parser accepts;
    * an integral value at or above 2**24 was written as its exact integer
      rather than its shortest round-trip (123456792 for 123456790), and one
      at or above 2**63 (1e20) overflowed the cast the same way inf did;
    * a non-integral value outside the band got numpy's scientific notation
      where the rule writes positional (see `_F32_POSITIONAL_BAND`).

    Those distinct values now take the per-value encoder. The old docstring
    excused the first two as "not produced by the decoder"; nothing enforces
    that -- vrf-movement reads raw f32/f64 components with no finiteness
    check and stream.rs narrows f64 with a bare `as f32`. None of the four
    occurs on the corpus (0 non-finite and 0 with |v| >= 2**24 in the scan
    above; 0 distinct non-integral values outside the band in any shortened
    column of any export), so this moved no line either. How a non-finite
    value reaches the consumer, and why it is counted, is `_write_movement`'s
    to say.

    Worth doing per-distinct because these columns are quantized on the wire
    and repeat heavily. Measured on 02d4d478's 1,837,220 kept movement rows:

        pos_x 691,850 distinct    pos_z  70,260    vel_y 17,248
        pos_y 696,435             vel_x  17,358    vel_z  6,071

    so the six shortened columns format 1,499,222 values total instead of
    11,023,320 -- 7.4x fewer. yaw and pitch dedup too (65,491 and 16,943
    distinct) but are NOT shortened; see `_write_movement`.
    """
    if arr.dtype.kind != "f" or arr.dtype.itemsize not in (4, 8):
        # The bit-pattern view below needs a same-width unsigned type, and a
        # silent mis-view would print plausible numbers. Refuse instead.
        raise TypeError(f"_json_scalar_column wants float32/float64, got {arr.dtype}")
    if arr.shape[0] > numpy.iinfo(numpy.int32).max:
        # The int32 inverse below would wrap and point rows at wrong texts.
        raise ValueError(f"{arr.shape[0]:,} rows is more than an int32 index can address")
    ubits, inverse = numpy.unique(
        arr.view(numpy.dtype(f"u{arr.dtype.itemsize}")), return_inverse=True
    )
    # int32 halves what the caller holds per column until the write, and the
    # int64 original is dropped at once: peak memory is the acceptance bar
    # for this function as much as speed is.
    inverse = pa.array(inverse.astype(numpy.int32))
    uniq = ubits.view(arr.dtype)
    if shorten:
        # The two shortcut domains, then the per-value encoder for the rest.
        # An object array holds the texts so a wide int can never truncate
        # against the float column's narrower `<U` width.
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
    """Build an {x, y, z} dict, emitting integral components as ints.

    Matches how the C# reference serializes a double: System.Text.Json writes
    0.0 as `0`, so keeping Python floats here would put `0.0` where the
    reference has `0`.
    """
    return {
        axis: (int(n) if float(n).is_integer() else n)
        for axis, n in zip(("x", "y", "z"), (x, y, z))
    }


def _parse_vector_or_none(val):
    """Parse a "(x,y,z)" vector without losing precision; None if unparseable.

    Full precision matters: the damage direction is a unit vector the reference
    emits at full float precision (0.055482650227362894). This function used to
    be contrasted with a _parse_location that rounded to 2 decimals -- that
    rounding is gone from both, and the only remaining difference is the
    failure mode, which is what the two names now say.

    Returning None rather than a zero vector is the point of this variant: for
    damage geometry a zero vector would be a silent wrong value rather than a
    visible absence.

    Integral components come back as ints so the output matches the C#
    reference exactly -- it emits {"x": 0, "y": 1, "z": 0}, not 0.0/1.0/0.0.
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
    """Parse a Location value into {x, y, z}, preserving full precision.

    This used to round to 2 decimals, which was the last thing keeping
    shot_rays.sample_rays from matching the reference -- it emits the raw
    double (559.962145690918).

    Callers expect a dict, so an unparseable value still yields a zero vector
    rather than None. That is a fabricated value, tallied as
    `fabricated_shot_locations` -- `_parse_rotation` below does the same for
    the paired Rotation parameter, under `fabricated_shot_rotations`.

    It used to be justified by "the shot filter upstream already guarantees a
    location is present, so it should be unreachable". There is no such
    filter. `_build_rpc_events` emits a shot for EVERY
    ReplayPlayContinuousEffectAtLocation invocation and says so in its own
    comment ("No blob guard"), deliberately, so that effects carrying only
    scalar params reach the "unknown" bucket instead of being dropped. A
    fabricated origin is therefore reachable, and an origin is a plausible
    coordinate -- nothing downstream can tell it from a real one. So it is
    counted. The value still ships, because a null would break the callers
    that index into it; what changes is that the run no longer claims it
    invented nothing.
    """
    parsed = _parse_vector_or_none(val)
    if parsed is None:
        _bump(tally, "fabricated_shot_locations")
        return {"x": 0, "y": 0, "z": 0}
    return parsed


def _parse_rotation(val, tally=None) -> dict:
    """Parse a Rotation value into {pitch, yaw, roll}.

    Mirrors `_parse_vector_or_zero`: an absent or unparseable Rotation still
    yields {pitch:0, yaw:0, roll:0} rather than None, because callers (and
    valplay's spray_control, which reads `shot.rotation` as the real aim
    direction) expect a dict. That is a fabricated value indistinguishable
    from a genuine (0,0,0) aim, so both fallback paths below tally it as
    `fabricated_shot_rotations` rather than shipping it uncounted.
    """
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
                # Rust's compact rotator strings are shortest-round-trip f32
                # decimals. Widen each parsed component back from f32 so the
                # typed representation matches the legacy raw-wire decoder and
                # the C# JSON numbers exactly.
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
    """Classify a shot as primary / alternate fire.

    Returns ``(fire_mode, evidence)``, mirroring ValorantShotFireModeResolver.

    The signal is the *name* of the firing-state subobject: a gun replicates
    "FiringState" for its primary cycle and "ZoomedFiringState",
    "FiringStateBurst" etc. for its secondary one. Neither the ammo counters
    nor burst_shot_number carry this -- burst_shot_number just indexes shots
    within any spray, so treating a non-zero value as "alternate" (as this
    adapter previously did) misclassified 1,462 of 2,475 shots on 02d4d478.

    "unknown" when no path resolves: those are effects with no firing state at
    all, not shots whose mode we failed to determine.
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
    """Walk a GUID's outer chain to the equippable actor that contains it.

    Returns ``(owner_net_guid, name, category, class_path)`` or ``None``.

    Two lookups per hop, because the two tables cover different populations:
    ``guid_class`` comes from actors.parquet (channel opens, carrying the spawn
    class path) while ``guid_path`` comes from net_guids.parquet (every GUID the
    replay registered, including subobjects that never opened a channel).
    A weapon appears in the first; its FiringState only in the second.
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

    Returns empty dicts and a `None` row count when the file is absent, so
    bundles produced by an older vrfkit still convert -- weapon identity is
    simply left unresolved rather than the run failing. `None` rather than 0
    because "there was no table" and "the table was empty" are different
    facts, and only the second one can be compared with a declared count.
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
    # The row count is returned separately from the two dicts: both drop rows
    # (a null outer, an empty path), so `len(guid_path)` is NOT the table's
    # height and cannot be compared with what the export manifest declared.
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
        """Read `n` bits LSB first from one slice, as a per-bit loop would.

        That loop made 2.8M calls on one replay's shot blobs. Its contract is
        kept: a short read leaves the position at the end and raises EOFError,
        and a declared length past the buffer raises IndexError -- aborting
        the conversion -- where a slice alone would silently pad with zeros.
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
    # Evaluated left to right, so a short read on y or z leaves the reader in
    # exactly the position the three-statement version left it in.
    return (r.read_f64(), r.read_f64(), r.read_f64())


class _EffectArraySpec(NamedTuple):
    """How to read one of the three effect value arrays.

    The arrays share their whole wire shape and differ only in which two
    property handles carry the gameplay-tag index and the value, and in how the
    value itself is read. The handle numbers are the containing struct's own,
    which is why they are not contiguous across the three.
    """

    tag_handle: int
    value_handle: int
    read_value: object


_EFFECT_FLOATS = _EffectArraySpec(7, 8, _BitReader.read_f32)
_EFFECT_VECTORS = _EffectArraySpec(11, 12, _read_effect_vector)
_EFFECT_OBJECTS = _EffectArraySpec(15, 16, _BitReader.read_int_packed)


def _decode_effect_elements(data: bytes, bit_count: int, spec: _EffectArraySpec,
                            tally=None):
    """Decode one effect value array -> list of (tag_index, value) tuples.

    ``spec.read_value`` must raise on a short read rather than return a
    sentinel: a failed read has to leave whatever value a previous element
    handle already stored untouched, and the reader's position is still
    advanced by however much the partial read consumed. Both are relied on by
    the ``consumed``/``skip_bits`` resynchronisation below.

    This decodes the preserved raw shot blobs independently of the additive
    JSON written by crates/vrf-decode/src/effect.rs. The adapter keeps the raw
    source even when a typed value is present.
    Where the Rust decoder rejects a blob outright on a shape it names
    (`PayloadUnderread`, `PayloadOverread`, `ResidualBits`, ...), this port
    keeps going and returns what it already decoded -- correct for a shot
    stream that must not go missing whole, but only if the two shapes that
    correspond to fabricated *downstream* values are counted rather than
    absorbed: a (tag, value) pair where one half's read failed (dropped
    silently by `_decode_effect_blob`, same as a missing `FiringState
    .AttackVector.N` -- see `spray_control.py`), and bits left over after the
    element loop ends that are too many to be sub-byte padding (Rust's own
    `ResidualBits` threshold: more than a byte means the framing did not end
    where this parse thinks it did).
    """
    r = _BitReader(data, bit_count)
    try:
        count = r.read_int_packed()
    except (EOFError, ValueError):
        count = None
    # No readable count, or one past Rust's MAX_ARRAY_COUNT (256): the framing
    # broke at its first IntPacked. Nothing is decoded, but the window it left
    # still reaches the residual check below, which an early return skipped.
    # Count 0 enters the loop, which consumes the array terminator the Rust
    # decoder also accepts after it.
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
    # Every declared slot with one or both halves still missing -- whether an
    # index the array never got around to visiting before truncating, or one
    # that was reached but lost its tag or value mid-read -- is a pair
    # `_decode_effect_blob` drops silently below. Tallied once, by count, in
    # one place, rather than re-deriving the same "one half missing" test
    # (and risking a second, disagreeing count) at every call site.
    half_read = sum(1 for tag, val in elements if tag is None or val is None)
    if half_read:
        _bump(tally, "effect_half_read_pairs", half_read)
    # Rust's `ResidualBits`: more than a byte of unconsumed window after the
    # element loop ends -- by a clean terminator, an index/handle out of
    # range, or simply running out of bits -- means this blob's framing did
    # not end where this parse thinks it did. Sub-byte padding is normal and
    # not counted, matching the Rust guard's own tolerance.
    if r.bits_remaining() > 7:
        _bump(tally, "effect_array_residual_bits")
    return elements


def _decode_effect_blob(blob: _EffectBlob | None, spec: _EffectArraySpec,
                        tag_table: dict, tally=None) -> dict:
    """Decode one effect blob into {tag_name: value}, dropping half-read pairs.

    An absent blob and a blob that decodes to nothing are the same thing to
    every caller: an empty mapping. A half-read pair and residual framing bits
    are NOT the same thing to every caller -- see `_decode_effect_elements`,
    which tallies both -- so this still drops them from the returned mapping
    (a `FiringState.AttackVector.N` a caller cannot find is exactly how
    `_build_shot_event` is meant to notice one is missing) but no longer
    without a count reaching the summary.

    The bit length comes from the parser's `bit_count` column, not from
    `len(data) * 8`. Those two agree on every effect blob measured -- 692,840
    across the 11 cross-validated replays, zero disagreements -- so this is a
    no-op on today's data. It is not a no-op on the contract: a payload whose
    declared length is not a whole number of bytes would otherwise have its
    padding bits decoded as data, and nothing downstream would report it.
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
    """One undecoded value array, with the bit length the parser declared.

    The two are carried together because `data` alone is not enough. Parquet
    stores whole bytes, so a payload of N bits arrives as ceil(N/8) bytes with
    up to 7 padding bits in the last one. Deriving the length as
    `len(data) * 8` hands those padding bits to the decoder as if they were
    data.
    """

    data: bytes
    bit_count: int


class _EffectBlobs(NamedTuple):
    """The three undecoded value arrays a shot RPC may carry. Any may be absent."""

    floats: _EffectBlob | None = None
    objects: _EffectBlob | None = None
    vectors: _EffectBlob | None = None


class _ShotContext(NamedTuple):
    """Per-replay lookups every shot event needs, resolved once per conversion.

    The two lookups default to None so a caller that has neither still produces
    an event -- with a null equippable and fire_mode "unknown", which is what
    the reference emits for a server-world effect anyway.
    """

    tag_table: dict
    equippable_lookup: object = None
    fire_mode_lookup: object = None


def _build_shot_event(
    ctx: _ShotContext,
    time_ms, packet_id, actor_net_guid, object_net_guid, channel_index,
    scalar_params: dict, blobs: _EffectBlobs, tally=None,
) -> dict:
    """Build a valorant_shot_received event from decoded RPC params.

    Always returns an event. Effects with no firing state -- server-world
    effects rather than weapon shots -- come back with a null equippable and
    fire_mode "unknown", which is how the reference emits them and what
    valplay's "unknown" weapon bucket exists to receive.
    """
    # Decode blobs: tag_name -> value
    tag_table = ctx.tag_table
    floats = _decode_effect_blob(blobs.floats, _EFFECT_FLOATS, tag_table, tally)
    objects = _decode_effect_blob(blobs.objects, _EFFECT_OBJECTS, tag_table, tally)
    vectors = _decode_effect_blob(blobs.vectors, _EFFECT_VECTORS, tag_table, tally)

    # Events with no firing state are emitted too, not filtered out.
    #
    # 172 of 02d4d478's 2,647 effect RPCs carry no FiringPlayerState, no
    # attack vectors and no weapon -- they are server-world effects
    # (source_id = DedicatedServerWorldSourceID), not weapon shots. Dropping
    # them looked cleaner and was the wrong call: valplay's weapons section
    # has an "unknown" bucket and weapon_stats has a
    # shots_without_equippable diagnostic, both built precisely to surface
    # these. Filtering them here hid information the consumer was designed to
    # report, which is the same silent-drop mistake the parser invariants
    # exist to prevent.
    #
    # Every downstream section that would be distorted by them already guards
    # on firing_player_state or attack_vectors, so they land in the buckets
    # meant for them rather than polluting any metric.

    # Extract scalar params from the RPC payload
    location = scalar_params.get("Location")
    rotation = scalar_params.get("Rotation")
    # Fallback: Location/Rotation may arrive as unnamed params "248"/"249"
    if location is None:
        raw248 = scalar_params.get("248")
        if isinstance(raw248, dict) and "Data" in raw248:
            raw_bytes = base64.b64decode(raw248["Data"])
            if len(raw_bytes) >= 24:
                x = _struct.unpack_from('<d', raw_bytes, 0)[0]
                y = _struct.unpack_from('<d', raw_bytes, 8)[0]
                z = _struct.unpack_from('<d', raw_bytes, 16)[0]
                # Full precision: the reference emits the raw double
                # (559.962145690918), and rounding here was the only thing
                # keeping shot_rays.sample_rays from matching.
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

    # Parse location/rotation from value_str compact format if needed
    loc_obj = _parse_vector_or_zero(location, tally)
    rot_obj = _parse_rotation(rotation, tally)

    # Build attack vectors
    attack_keys = (f"FiringState.AttackVector.{i}" for i in range(1, 16))
    attack_vectors = [{"x": x, "y": y, "z": z} for x, y, z in
                      (vectors[key] for key in attack_keys if key in vectors)]

    burst = floats.get("FiringState.BurstShotNumber")
    yaw_switch = floats.get("FiringState.YawSwitch")

    # Ammo
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

    # Weapon identity: FiringState is a subobject of the gun, so its outer is
    # the equippable actor. Null when the chain does not reach a known
    # equippable -- never guessed.
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

    # Only an int is an enum ordinal. Anything else -- the {BitCount, Data}
    # blob of a field the overlay could not type -- passes through unchanged,
    # like every other shot param; str() published its Python repr.
    alliance_str = alliance
    if isinstance(alliance, int):
        alliance_str = ALLIANCE_MAP.get(alliance, f"alliance_unknown_{alliance}")

    shot = {
        "effect_id": effect_id,
        # Float32 on the wire; the reference prints its shortest round-trip
        # (12.780108) rather than the widened value. Same treatment as the
        # spawn and position coordinates -- it was simply missed there.
        "start_movement_time": _f32_shortest(start_time)
        if isinstance(start_time, float) else start_time,
        "source_id": source_id,
        "is_local_effect": bool(is_local),
        "is_transient": bool(is_transient) if is_transient is not None else True,
        "wait_on_replication_actor": wait_on or 0,
        # Absent means absent. The reference emits null on 101 of 02d4d478's
        # 2,647 effects; defaulting to "alliance_any" collapsed two distinct
        # input states into one output.
        "alliance_filter": alliance_str,
        "location": loc_obj,
        "rotation": rot_obj,
        "ammo_remaining": ammo,
        # Absent means absent. The reference emits null on 172 of 2,647
        # shots; defaulting to 1 also rewrote a genuine 0, and one consumer
        # (compute_metrics.py:1560) reads the field without its own default.
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
        # Both were previously substitutes: object_net_guid repeated the actor
        # guid and channel was hardcoded 0. fields.parquet carries the real
        # values on every shot row, and the reference disagrees with both
        # substitutes on all 2,647 events (object 22 vs actor 2, channel 1).
        "object_net_guid": object_net_guid,
        "channel": channel_index,
        "shot": shot,
    }


# ---------------------------------------------------------------------------
# Combat report leaf labels
#
# The parser labels each flattened array leaf with the name the REPLAY declares
# for that handle, which is the wire's own statement and the right thing for
# fields.parquet to archive. It is NOT the right thing for this bundle, for two
# independent reasons, and both are load-bearing.
#
# 1. compute_metrics.py is read-only and reads these keys by name: Subject,
#    Team, DidKill, Died, AssistType, DamageDealt, DamageReceived, HitsDealt,
#    HitsReceived, DealtInteractions[].Regions[].{Region,Hits,IsWallPen}. The
#    wire spells six of those differently -- `bDidKill`, `bDied`, `bIsWallPen`,
#    `ParticipantSubject`, and Riot's own typos `DamageRecieved` and
#    `HitsRecieved`. Passing those through silently zeroes every metric that
#    reads them.
#
# 2. The declared names are NOT unique within one flattened element. UE flattens
#    the same four-member struct (HUDConfig / StateRemainingTime / GameTime /
#    GamePhase) at eight nesting positions, so handles 6, 99 and 105 all declare
#    `HUDConfig` at the Reports level, and 27/31, 62/66 pair up one level down.
#    A payload is a JSON object built by last-wins assignment, so under the
#    declared names those keys merge: measured on 02d4d478, 3,405 of 20,298
#    distinct payload paths would collapse and their values would be destroyed
#    with no counter moving. fields.parquet keeps the `handle` column and loses
#    nothing; this projection has no such escape hatch.
#
# So the bundle keys on the handle, not on the emitted name: the reference's
# member name where the C# parser has one, `_h{handle}` -- the label this bundle
# already carried -- where it does not. That keeps events.ndjson byte-identical
# across this change, which is what makes the metrics comparison meaningful.
#
# This is the "no hardcoded names in the parser" invariant working as intended:
# the Rust side emits what the wire says, and the table that renames it for a
# downstream consumer lives here, where labelling is a presentation concern.
# ---------------------------------------------------------------------------
COMBAT_REPORT_GROUP = "CombatReportComponent"

# handle -> the member name the C# reference emits for it. Leaf handles only;
# the container handles (4 Reports, 10 Interactions, 26 DealtInteractions,
# 61 ReceivedInteractions, 44/79 Regions) are path segments the parser takes
# from its own schema and never renames.
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


# Handles the parser treats as sub-array containers, not leaves. A row carrying
# one of these is a container the walker gave its schema name to, so it is
# already the reference's spelling and must be left alone.
COMBAT_REPORT_CONTAINER_HANDLES = frozenset({4, 10, 26, 44, 61, 79})


def _combat_report_leaf_name(group_path: str, field_name: str, handle) -> str:
    """Relabel one combat-report array leaf for the bundle.

    Only the LAST path segment is touched: everything before it is a container
    segment the parser names from its own schema, so it already matches.

    Two rows the walker SYNTHESISES rather than reads are excluded, because for
    them the parser's own label is already right and rewriting it would be a
    regression: `emit_remaining_raw`'s `..._raw` row (handle u32::MAX, which
    would otherwise become `_h4294967295`), and the depth-limit row that carries
    a container handle. Neither occurs on the corpus -- MAX_ELEMENTS is 4096
    against a real peak near 50, and MAX_RECURSION_DEPTH is 12 against a real
    depth of 5 -- so this is an unreachable edge being closed, not a fix.
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
        # No reference name for this handle: keep the handle-derived label the
        # bundle has always used. Its uniqueness is the whole point -- the
        # declared name is not unique at this nesting level.
        name = f"_h{handle}"
    return head + dot + name


# ---------------------------------------------------------------------------
# Field rows -> nested payloads
# ---------------------------------------------------------------------------
def _normalize_prop_field_name(field_name: str, is_bool: bool) -> str:
    """Normalize a replicated property field name to match C# parser output.

    WHY: The C# parser strips the 'b' prefix from UE4 boolean property names
    (e.g. 'bUltimateActive' -> 'UltimateActive', 'bLoadoutFinalized' ->
    'LoadoutFinalized'). vrfkit preserves the raw UE4 names. We normalize
    to match compute_metrics.py's expectations.
    """
    if is_bool and field_name.startswith('b') and len(field_name) > 1 and field_name[1].isupper():
        return field_name[1:]
    return field_name


@lru_cache(maxsize=None)
def _parse_field_path_cached(path: str):
    """Parse one path. Returns `(parts, unparsable_segment_count)`.

    Split out from `_parse_field_path` so the regex work can be memoised.
    Field paths come from a dictionary-encoded Parquet column, so a replay's
    ~520k parses cover only a few thousand distinct strings.

    The tally is deliberately NOT bumped here. Doing so would make the counter
    depend on cache hits: the first "Rounds[0][1]" would count and every later
    one would not, silently under-reporting a fault the caller is meant to see.
    The count is returned instead and the caller bumps on every call.
    """
    parts = []
    unparsable = 0
    for seg in path.split('.'):
        m = _PATH_RE.fullmatch(seg)
        if m:
            name = m.group(1)
            idx = int(m.group(2)) if m.group(2) is not None else None
            parts.append((name, idx))
        else:
            # Unresolved handle names like "_h27" match the pattern above.
            # Two kinds of segment land here instead, and only one is a fault.
            #
            # NOT a fault: a leaf whose name is simply not an identifier --
            # bare numbers like "248" (the documented spelling of an unnamed
            # handle) and Blueprint display names with spaces in them. A
            # literal key is the correct representation of a leaf. Measured on
            # out/baseline: 449 rows, 41 distinct spellings, every one of them
            # a name like "Victim FXC" or "Set skeletal Collision" and not one
            # containing a bracket. Counting those would put a three-figure
            # number in every clean summary and teach the reader to skip it.
            #
            # A fault: a segment carrying subscripts this parser could not
            # read. "Rounds[0][1]" becomes the literal object key
            # "Rounds[0][1]" -- valid JSON, reads like a name, and nests
            # nothing, so the two levels of structure it describes are gone
            # with no other trace. A bracket is what tells the two apart.
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


def _set_nested(root: dict, parts: list, value, tally=None):
    """Set a value deep in a nested dict/list structure using parsed path parts.

    WHY: vrfkit stores each field as a flat row with full path like
    'Rounds[0].Reports[0].Interactions[0].DamageDealt = 30'. We need to
    reconstruct the nested JSON object that compute_metrics.py expects.
    Array elements are auto-extended with None/empty dicts as needed.

    Array elements get an 'Index' field set to their subscript position,
    matching the C# parser's behavior (compute_metrics uses inter.get("Index")
    for deduplication).

    Two rows can disagree about a key's SHAPE -- 'Foo' carrying a scalar and
    'Foo.Bar' carrying a nested one. Whichever arrives second wins and the
    other row's value is gone, with valid JSON and a full row count. The five
    `payload_shape_conflicts` sites below count it when this function does the
    replacing; 'Foo' after 'Foo.Bar' is a one-segment path, which
    `_build_property_events` assigns itself and counts as
    `property_key_collisions`. A leaf replaced by a same-named row is counted
    here under that name too. Growing an array with `{}`/`None` fillers and
    injecting `Index` are NOT losses: those placeholders are ours and hold
    nothing to lose, so a real `Index` row that replaces the injected one
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
                arr.append(None if is_last else {})
            if is_last:
                if isinstance(arr[idx], (dict, list)) and arr[idx]:
                    # A populated element replaced by a scalar.
                    _bump(tally, "payload_shape_conflicts")
                elif arr[idx] is not None and not isinstance(arr[idx], (dict, list)):
                    # A value replaced by a same-named row.
                    _bump(tally, "property_key_collisions")
                arr[idx] = value
            else:
                if not isinstance(arr[idx], dict):
                    if arr[idx] is not None:
                        # A scalar element descended into as a container.
                        _bump(tally, "payload_shape_conflicts")
                    arr[idx] = {}
                # The C# parser's Index field, set to the subscript.
                arr[idx].setdefault("Index", idx)
                obj = arr[idx]
                element = idx
        elif is_last:
            previous = obj.get(name)
            if isinstance(previous, (dict, list)) and previous:
                # A populated subtree replaced by a scalar.
                _bump(tally, "payload_shape_conflicts")
            elif (name in obj and not isinstance(previous, (dict, list))
                  and not (name == "Index" and previous == element)):
                # A value replaced by a same-named row. An Index equal to
                # the element's subscript is the one injected above.
                _bump(tally, "property_key_collisions")
            obj[name] = value
        else:
            nxt = obj.setdefault(name, {})
            if not isinstance(nxt, dict):
                _bump(tally, "payload_shape_conflicts")
                nxt = obj[name] = {}
            obj = nxt
            element = None


def _drop_padding_elements(node):
    """Remove `{}` placeholders that array extension left behind.

    Replication is sparse: a packet can carry element [1] of an array without
    resending [0]. `_set_nested` has to extend the list to reach index 1, and
    the filler it appends is a bare `{}`.

    The reference emits only the elements actually present, so those fillers
    are ours alone -- and they are not harmless. compute_metrics builds
    `{t["Index"]: t for t in teams}` and then sorts the keys; a filler has no
    Index, so the None key made sorting raise TypeError and the whole replay
    failed to produce metrics.

    Only fully empty dicts are dropped. Every genuine element carries at least
    the `Index` that `_set_nested` injects, so nothing real matches. `None`
    fillers in scalar arrays are left alone: there the position IS the index,
    and removing one would silently renumber the rest.
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
    """Extract the typed value from a fields.parquet row.

    Exactly one of the typed columns is non-null when decoded, otherwise the
    raw_bits blob is the value. Returns (value, is_raw).

    That "exactly one" is an assumption about the writer, and the priority
    chain below cannot tell a satisfied assumption from a violated one: with
    value_i64 = 1 and value_bool = False both set, this returns the integer 1,
    discards the boolean, and the row looks entirely ordinary downstream. So
    the violation is counted where it can still be seen. raw_bits is excluded
    -- it travels alongside decoded values by design and is only consulted
    when every typed column is null.
    """
    # Summed as bools rather than `sum(v is not None for v in ...)`: this runs
    # once per field row (1.4 M on a full replay) and the generator plus the
    # `sum` call dominated the check itself. Same arithmetic, same counter.
    if ((row_i64 is not None) + (row_f64 is not None)
            + (row_bool is not None) + (row_str is not None)) > 1:
        _bump(tally, "multi_typed_rows")
    if row_i64 is not None:
        return row_i64, False
    if row_f64 is not None:
        # Truncate float to reasonable precision to match C# output
        return row_f64, False
    if row_bool is not None:
        return row_bool, False
    if row_str is not None:
        return row_str, False
    if row_raw is not None:
        return _raw_blob(row_raw, row_bits), True
    return None, False


def _raw_blob(row_raw, row_bits) -> dict:
    """The {BitCount, Data} blob of one row's raw bits, as the C# output has it.

    One builder for every site that publishes raw bits -- `_get_value` and the
    raw-sourced fields -- so the key order, which is part of the bytes, has
    one source. A caller that labels the blob adds `TypeName` after these two.
    """
    return {"BitCount": row_bits,
            "Data": base64.b64encode(row_raw).decode('ascii')}


def _count_unbuilt_blobs(blob_state, tally) -> None:
    """Count the raw-sourced fields one event needed and did not get.

    `blob_state` maps each raw-sourced field the event touched -- by its
    container row or by a decoded member -- to whether its blob was built;
    `None` when it touched none. Counted per event and field, not per row, so
    a blob whose members all arrived without it counts once.
    """
    if blob_state:
        unbuilt = sum(1 for built in blob_state.values() if not built)
        if unbuilt:
            tally.bump("raw_blobs_unavailable", unbuilt)


def _split_rpc_field(field_name: str):
    """Split an RPC field_name into (rpc_name, param_name).

    "MulticastNotifyDamage_Point.DamageTaken" -> ("MulticastNotifyDamage_Point",
    "DamageTaken"). For zero-param RPCs, the field_name IS the RPC name with no
    dot.
    """
    name, dot, param = field_name.partition('.')
    return name, (param if dot else None)


# ---------------------------------------------------------------------------
# Actor class inference: map group_path to replication_class_path
# ---------------------------------------------------------------------------
def _group_path_to_class(gp: str) -> str:
    """Convert a group_path to an approximate replication_class_path.

    WHY: vrfkit does not export explicit actor_spawned events. We infer the
    class from the first group_path seen for each actor. The group_path for
    replicated properties IS the class path (e.g.
    '/Game/GameModes/Bomb/BombPlayerState.BombPlayerState_C').
    For ClassNetCache RPCs, strip the _ClassNetCache suffix to get the class.
    """
    # e.g. '/Script/ShooterGame.DamageableComponent_ClassNetCache'
    # -> '/Script/ShooterGame.DamageableComponent'
    return gp.replace(CLASS_NET_CACHE_SUFFIX, '')


def _to_package_path(class_path: str) -> str:
    """Drop the `.ClassName_C` suffix, leaving the UE package path.

    actors.parquet carries the full object path
    ("/Game/Characters/Hunter/Hunter_PC.Hunter_PC_C") but the C# reference
    emits actor_spawned.replication_class_path as the package path alone
    ("/Game/Characters/Hunter/Hunter_PC"), and valplay's own docstrings
    document that as the spawn shape.

    The distinction is invisible to consumers that split on "." -- which is
    why weapon_stats was already correct -- but ability_usage.top_classes and
    ability_detail.by_ability take `path.split("/")[-1]` verbatim, so with the
    suffix attached every ability key read as "Foo.Foo_C" instead of "Foo".

    Verified against the reference on 02d4d478: after stripping, every shared
    path matches with zero count mismatches.
    """
    slash = class_path.rfind("/")
    tail = class_path[slash + 1:]
    if "." not in tail:
        return class_path
    return class_path[: slash + 1] + tail.split(".", 1)[0]


def _group_path_to_archetype(gp: str) -> str:
    """Derive a Default__X_PC_C archetype path from the group_path."""
    # e.g. '/Game/Characters/Wushu/Wushu_PC.Wushu_PC_C'
    # archetype = 'Default__Wushu_PC_C'
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
    """Normalize an RPC parameter name and value to match C# parser output.

    WHY: vrfkit uses prefixed 'b' for booleans (e.g. 'bDamageKilledTarget')
    while C# emits 'DamageKilledTarget'. Also, RegionalDamage is stored as
    int enum in vrfkit but as string in C# output. Only the damage RPCs are
    renamed or reshaped; every other RPC's parameters pass through unchanged.

    Members of a life-change array are dropped. vrfkit now emits one row per
    member of `LifeChangeEvents[]`/`LifeChangeBySection[]` alongside the parent
    blob row, and those rows arrive here spelled `LifeChangeEvents[0].LifeResult`
    -- which no branch below matches, so they fell through to the generic
    assignment and put flat keys like that straight into the event payload.
    Confirmed by injecting two such rows and reading them back out of
    `events.ndjson`, not inferred. The parent blob row still carries the same
    information in the shape this adapter expects, so skipping the children
    loses nothing here -- as long as the parent is there. On the damage RPCs,
    whose blob valplay decodes, a member that arrives without it is counted
    (`raw_blobs_unavailable`, see `_build_rpc_events`).

    `tally` is optional so the function stays callable on its own; the
    conversion passes the real one for `damaged_bone_undecoded`.
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
        # Damage geometry: the parser now decodes these as quantized vectors
        # (previously raw blobs, because the C# custom decoder hid the type).
        # They arrive as the compact "(x,y,z)" string and the reference emits
        # {x, y, z}. Left as the raw payload if a value ever fails to parse --
        # visibly absent beats a fabricated zero vector.
        parsed = _parse_vector_or_none(value)
        if parsed is not None:
            value = parsed
    elif param == "EquippableUsed":
        # EquippableUsed: net GUID -> the C# ValorantEquippable shape. An
        # undecodable one passes its bits through rather than a guess.
        #
        # Name/ClassPath stay null and Category stays "unknown" because that is
        # what the C# parser emits: its resolver looks the GUID up in the
        # NetGuidCache path table, and weapon instances are dynamic actors that
        # never register a path there. valplay resolves the gun downstream from
        # actor_spawned instead (_actorindex.build_actor_class_index), so
        # filling these in here would diverge from the reference for no gain.
        #
        # This used to read the raw bits as a fixed little-endian uint16, which
        # was wrong twice over: the field is IntPacked (8/16/24 bits wide
        # depending on the value), and the low bit of the first byte is
        # IntPacked's continuation flag, so every multi-byte value came out odd
        # and could never be a valid dynamic NetGUID. The overlay now types the
        # field as ObjectNetGuid, so the parser hands us the decoded integer.
        if isinstance(value, int) and not is_raw:
            value = {
                "NetGuid": value,
                "Name": None,
                "ClassPath": None,
                "Category": "unknown",
            }
    elif param == "LifeChangeEvents" or param in DEATH_MONTAGE_BLOB_PARAMS:
        # Kept as the reference's labelled blob, {BitCount, Data, TypeName}.
        # `_build_rpc_events` builds these from raw_bits whenever the row has
        # them, typed or not (see `_RAW_SOURCED_RPC_PARAMS`), so `is_raw` here
        # means "the blob exists".
        #
        # DeathMontageEffectOverride and ...Context: the reference labels them
        # blobs with exactly these TypeNames. The parser now types both as
        # ObjectNetGuid -- an FXC_* effect class and a pawn actor reference --
        # and the RPC loop hands their wire bits back here as the blob, so the
        # event keeps the reference's shape while fields.parquet carries the
        # GUID. A startswith match also swallowed
        # DeathMontageEffectOverrideIsQueued, which is a 1-bit bool: 632 events
        # shipped it as a blob with an invented TypeName where the reference
        # emits plain false.
        if is_raw and isinstance(value, dict):
            value["TypeName"] = param
    elif param == "DamagedBone" and is_raw:
        # DamagedBone is an FName the overlay decodes into value_str: all
        # 632,906 MulticastNotifyDamage_Point rows on the 1,018-export corpus
        # are typed (2026-09-28; _Base carries none). A raw one is a decode
        # the parser could not do -- it counts that on its side -- and it is
        # published as null and counted, not guessed at.
        #
        # This branch used to ASCII-decode the wire bytes with
        # errors='replace' inside a bare `except`, so it could not fail: the
        # FName "Head" came out as '\n\x00\x00\x00' plus replacement
        # characters, the same mojibake apply_type_corrections.py records
        # shipping for all 581 payloads when the field was forced to Raw.
        # Null rather than the raw blob because valplay's `_bone_region`
        # files None under "other", and would raise TypeError on a dict
        # (`bone in HEAD_BONES`, a frozenset).
        _bump(tally, "damaged_bone_undecoded")
        value = None
    return {out_name: value}


# ---------------------------------------------------------------------------
# Conversion phases
#
# Every phase that appends to `events` is order-sensitive: `events.sort` at the
# end is stable, so events that tie on (packet_id, time_ms) come out in the
# order the phases produced them. Keep the phase order (actors, properties,
# RPCs, server timeline) and the append order inside each phase.
# ---------------------------------------------------------------------------
#: Bump when the bundle manifest's shape changes in a way a consumer must
#: notice. valplay's resume marker records it, so an older bundle is rebuilt
#: instead of being read under the newer contract.
#:
#: 1 -> 2: `quality`, `net_field_export_groups` and `adapter` were added. A
#: bundle at 1 carries no upstream accounting at all, so a consumer cannot
#: distinguish "the export was complete" from "nobody counted".
#:
#: NOT bumped for `actor_dormant`, deliberately. The version gate in valplay
#: (`scripts/parse_replays.py`, `ADAPTER_SCHEMA_VERSION`) is an equality check
#: that REFUSES any bundle whose version it does not match, and
#: `valplay/tests/ci-stack-smoke.mjs` asserts the built worker image's adapter
#: constant equals it -- so a bump breaks valplay's CI and refuses every
#: existing bundle until that repo re-pins `VRFKIT_REF` and rebuilds its worker
#: image. The version exists to force a rebuild when a bundle would otherwise
#: be MISREAD, and that is not the case here:
#:
#: * `actor_dormant` is an ADDITIVE type. valplay filters events by exact
#:   `type` equality and keeps no event-type allowlist, so a consumer that does
#:   not know it simply does not match it -- no crash, no misread.
#: * Every field on `actor_closed` is unchanged; `actor_event` is added
#:   alongside them, and an unknown key is ignored by every reader here.
#: * The change makes existing consumers MORE correct with no edit on their
#:   side: `actor_closed` stops carrying dormancies, so a spawn/close pairing
#:   that used to truncate a settled ability's lifetime now leaves that
#:   instance unpaired instead of wrongly ended.
#:
#: What DOES change for valplay is which instances it measures, and this was
#: MEASURED against its real `compute_ability_detail`, not assumed. On 8 Sage
#: walls where 6 are destroyed at 3,000ms and 2 settle dormant at 40,000ms:
#:
#:   before: instances 8, cap 40,000ms, destroyed 6 (75.0%)
#:   after : instances 6, cap  3,000ms, destroyed 0 (0.0%)
#:
#: Valplay now consumes `actor_dormant` as a right-censored lifetime: a later
#: real close remains an exact end, while a dormant actor with no close carries
#: only the observed lower bound and never counts as destroyed. That preserves
#: this adapter's wire-faithful distinction all the way into the metric instead
#: of choosing either of the two imperfect measurements above.
#:
#: A version bump could not have communicated any of that -- it would only have
#: refused the bundle outright. Bump this when a bundle would be MISREAD by a
#: consumer that ignores the change, which is the opposite of this case.
#:
#: ``level_names_and_times`` is likewise additive. An older consumer ignores
#: it and retains its existing event-path fallback; a newer consumer treats an
#: absent value as unavailable and uses that same fallback. Neither generation
#: can misread the other one's bundle, so this addition does not justify
#: refusing every schema-2 bundle already on disk.
#:
#: ``server_timeline_event`` is additive for the same reason: older consumers
#: match exact event types and ignore it, while newer consumers can use the
#: server-labelled timeline as independent corroboration.  No existing event's
#: shape or meaning changes.
BUNDLE_SCHEMA_VERSION = 2


def _upstream_row_check(declared, observed) -> dict:
    """One declared-vs-observed row count, and whether it can be judged.

    `agrees` is `None`, not `True`, when the export declared nothing. An
    absent declaration is a fact about the export, and reporting it as
    agreement would let a manifest with no accounting in it certify itself.
    """
    return {
        "declared": declared,
        "observed": observed,
        "agrees": None if declared is None else declared == observed,
    }


def _public_level_names(value):
    """Copy only the public, typed portion of replay level metadata.

    The neighbouring ``game_specific_data`` header can contain account
    subjects and loadout data, so forwarding the whole header is not an
    option.  Level roots are independently useful (they are the map URL), but
    even those cross the adapter seam as an explicit two-field allowlist.
    """
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

    Three things cross this seam that used to stop here:

    * ``quality`` -- vrfkit's complete loss/fallback accounting, copied
      through VERBATIM. Not re-derived, not summarised, not defaulted: the
      point of forwarding it is that the consumer reads the producer's own
      numbers, and a value this adapter invented would be the opposite of
      that. Absent upstream, it is written as ``null`` -- a visible absence,
      never a plausible zero.
    * ``net_field_export_groups`` -- the replay's handle -> field-name
      dictionary, also verbatim. It is the only thing in the export that can
      turn a wire handle back into a name, and valplay's `_roundinfo` decoder
      has been naming handles 41-45 from a hardcoded list while citing this
      table as its source. With the table present the naming becomes a check
      instead of an assumption.
    * ``level_names_and_times`` -- the public level roots and their times,
      copied through a strict two-field allowlist. The first root is the
      replay's authoritative map URL; unlike event-path inference it is
      present even when no map actor path reaches the converted event stream.
    * ``adapter`` -- what THIS process measured, kept in its own object so it
      can never be mistaken for an upstream figure. It carries the exact
      identities a consumer can re-verify (line counts) and the declared-vs-
      observed comparisons this adapter was able to make.

    ``players`` is deliberately NOT forwarded; see the note in `_convert_into`.
    """
    quality = manifest.get("quality")
    groups = manifest.get("net_field_export_groups")
    levels = _public_level_names(manifest.get("level_names_and_times"))
    out_manifest = {
        "replay_version": manifest.get("replay_version", "unknown"),
        # No default: an absent duration_ms is a visible null, not a
        # fabricated 0 ms match -- see `source_size_bytes` below and the
        # `quality` note above for the same rule. valplay's
        # pipeline/metrics/compute_metrics.py reads `duration_ms` with no
        # default of its own, so a 0 here would ship as a real, plausible
        # match length instead of surfacing as missing.
        "duration_ms": manifest.get("duration_ms"),
        "replay_build": manifest.get("replay_build", ""),
        "replay_changelist": manifest.get("replay_changelist", 0),
        "source_file": manifest.get("source_file", ""),
        "source_size_bytes": manifest.get("source_size_bytes"),
        # Mark as vrfkit-converted
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
    """A dictionary-encoded column as a list that SHARES its string objects.

    `column.cast('string').to_pylist()` builds one Python str per ROW. On
    02d4d478's fields.parquet that is 1,246,812 objects for `group_path`'s 443
    distinct values and 1,207,778 for `field_name`'s 3,954 -- a hundred-odd MB
    of duplicates, and 2.3x slower than reading the dictionary and indexing it.

    Falls back to `to_pylist` for a column that is not dictionary-encoded, so
    the caller does not have to care which it got.
    """
    arr = column.combine_chunks()
    if not pa.types.is_dictionary(arr.type):
        return column.to_pylist()
    values = arr.dictionary.to_pylist()
    return [values[i] if i is not None else None
            for i in arr.indices.to_pylist()]


def _numeric_column_to_pylist(column):
    """A non-nullable numeric column as a Python list, via numpy.

    `to_pylist()` boxes one Python int per row through pyarrow's per-element
    type dispatch; `to_numpy()` hands over the raw buffer and `.tolist()`
    re-boxes it in C. On 02d4d478's 1,246,812-row fields.parquet that is ~14x
    faster for the uint32 columns (250 ms -> 16 ms each).

    Only safe on a column with NO nulls: numpy has no integer NaN, so a nullable
    integer column would be widened to float64 with NaN where the holes were.
    Use `_nullable_numeric_to_pylist` for those.
    """
    return column.to_numpy(zero_copy_only=False).tolist()


def _nullable_numeric_to_pylist(column):
    """A nullable numeric column as a list of (value | None), via numpy.

    Reads the validity bitmap and the primitive buffer separately. The nulls are
    filled with a type-matched sentinel only so the buffer survives `to_numpy`
    without being widened to float64; the mask then punches None back in, so the
    result matches `to_pylist` element-for-element (same value AND same Python
    type) while being ~3x faster.
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
    """fields.parquet held column-wise.

    pyarrow iteration row-by-row is slow; batch-extracting each column to a
    Python list once and indexing it is much faster.
    """

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
    """Classify every field row: RPC vs replicated property, plus actor lifetimes.

    RPCs are the rows whose group_path contains '_ClassNetCache'; properties are
    everything else.

    One pass, not two: `prop_groups` and `rpc_groups` are dicts, so they iterate
    in insertion order, and that order decides how events tying on
    (packet_id, time_ms) are ordered in the written bundle. Splitting this loop
    would reshuffle them.
    """
    # Track actor first/last appearance for actor_spawned/actor_closed
    # (fallback when actors.parquet is absent).
    actor_first = {}  # actor_net_guid -> (time_ms, packet_id, group_path)
    actor_last = {}   # actor_net_guid -> (time_ms, packet_id)

    # Group key -> list of row indices
    # For properties: (packet_id, actor_net_guid, object_net_guid, group_path)
    # For RPCs: (packet_id, actor_net_guid, group_path, handle)
    prop_groups = defaultdict(list)
    rpc_groups = defaultdict(list)
    # Counted, not merely skipped. vrfkit reports the same quantity as
    # `quality.net.unresolved_rpc_payloads_preserved`; recording this
    # adapter's own count of the rows it actually saw makes the two
    # comparable instead of leaving the skip invisible.
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

        # Track actor lifecycle
        if actor not in actor_first:
            actor_first[actor] = (ms, pid, gp)
        actor_last[actor] = (ms, pid)

        is_rpc = CLASS_NET_CACHE_SUFFIX in gp
        if is_rpc:
            handle = col_handle[i]
            rpc_groups[(pid, actor, gp, handle)].append(i)
        else:
            # Keyed by subobject too: a character replicates several
            # ItemSlot subobjects, and merging them into one event makes
            # the inventory look like a single slot.
            prop_groups[(pid, actor, col_obj[i], gp)].append(i)

    return actor_first, actor_last, prop_groups, rpc_groups, unresolved_cnc_rows


#: `actors.event` -> the bundle event type it publishes as. THREE values, not
#: two: `dormant` is the server suspending replication of an actor that is
#: still alive, so it is NOT a despawn and must not share a type with one. See
#: CLAUDE.md ("Dormancy is not destruction; only `close` is a despawn") and
#: `tools/extract_active_effects.py`, which pairs the same column.
#:
#: A value absent from this map is published as `actor_lifecycle_unknown` and
#: counted, never folded into the nearest known type.
_ACTOR_EVENT_TYPES = {
    "close": "actor_closed",
    "dormant": "actor_dormant",
}


# Event-chunk payload words are NOT self-describing.  This is the same closed
# vocabulary used by ``crates/vrfkit/src/driver/mod.rs`` after its residual-zero
# layout check: only these groups have a structurally established word count.
# An unknown future group still crosses as a labelled timestamp, but none of
# its payload words are assigned a meaning here.
_SERVER_TIMELINE_WORD_COUNTS = {
    "characterDeath": 2,
    "characterUltimateUsed": 1,
    "roundStarted": 1,
    "switchTeams": 1,
    "spikePlanted": 0,
    "spikeDefused": 0,
    "spikeExploded": 0,
}

# Structural Event-payload values measured over all 109,126 Event chunks in
# the 527-replay, three-build corpus. These are public Unreal enum constants,
# not replay-provided identities. The adapter rechecks them rather than trusting
# an arbitrary Parquet string before allowing `payload_name` across the privacy
# seam. Tag and seconds travel only when the whole tuple agrees; partial or
# third-party rows fall back to the already-public group/times/words.
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
    """Place packet-less Event chunks among packet-addressed replication rows.

    Event chunks carry authoritative replay-relative times but no packet id;
    the rest of ``events.ndjson`` is sorted in packet order because that is the
    wire order.  For each event time we therefore use the largest packet id
    observed at or before that time.  The prefix maximum keeps the derived key
    monotone even if one malformed frame made a later packet report an earlier
    timestamp -- the existing regression counter remains responsible for
    surfacing that damaged time axis.

    This key is ordering metadata only.  It is never published as if the Event
    chunk itself declared a packet association.
    """

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

    ``events.parquet`` also holds a replay-scoped id, free-form metadata and
    the original payload bytes. Any of those may contain account or match
    identifiers, so none crosses this adapter seam. The structural payload
    FString crosses only when it equals the fixed public enum constant for the
    group and its tag/time tuple also matches the measured layout. Older
    Parquet schemas simply omit those additive columns.

    Returns ``(events, rows_read)``.  ``None`` means the table was absent;
    zero means it was present and empty.  That distinction is needed for the
    producer-declared row-count reconciliation in the bundle manifest.
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

    Returns ``(events, guid_class)``. `guid_class` maps an actor GUID to its
    spawn class path and is filled from the same pass, because the shot events
    built later need it for weapon identity.

    `tally` is required, not optional: the unknown-lifecycle-value counter is
    the only thing that distinguishes "this export used the three values this
    adapter knows" from "a fourth appeared and was published as an unknown".
    """
    events = []
    guid_class = {}  # actor net guid -> spawn class path
    actor_event_counts = Counter()

    # actors.parquet is authoritative: it carries class/archetype/location from
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
                # Spawn coordinates are Float32 on the wire and in the Parquet
                # column; widening them to Python floats would print the binary
                # artefact instead of the value the reference shows.
                #
                # A missing coordinate stays missing. Only static actors reach
                # here with no spawn data at all -- 27 opens on 02d4d478 --
                # because the parser now writes the wire's (0,0,0) default for
                # a dynamic actor whose location bit is clear rather than
                # dropping it (pipeline.rs, read_optional_quantized_vector).
                # Substituting {0,0,0} here would put those 27 back among the
                # 66 that really do spawn at the origin.
                has_loc = a_sx[i] is not None or a_sy[i] is not None or a_sz[i] is not None
                location = _vec3(
                    _f32_shortest(a_sx[i]) if a_sx[i] is not None else 0,
                    _f32_shortest(a_sy[i]) if a_sy[i] is not None else 0,
                    _f32_shortest(a_sz[i]) if a_sz[i] is not None else 0,
                ) if has_loc else None
                # Spawn rotation is independent of spawn location: a static
                # actor can omit both, while a dynamic projectile may carry a
                # direction even when its position is the origin.  Preserve
                # that distinction and never fabricate a zero rotation for a
                # row whose three nullable wire columns are all absent.
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
                    # First open wins: a GUID can be reused after a close, but
                    # the shot events that reference it belong to its first life.
                    guid_class.setdefault(a_guid[i], class_path)
                # A static actor has no archetype and no class, and the
                # reference emits null for both. Deriving "Default__" + the leaf
                # of an empty class path produced the literal string
                # "Default__" for all 27 of them -- a value that looks like an
                # archetype and identifies nothing.
                archetype = a_arch[i]
                # guid_class above keeps the full object path (weapon lookup
                # matches on it); the event carries the package path the
                # reference emits.
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
                # `actors.event` has THREE values -- open / close / dormant --
                # and this was an `else`, so every dormant row published as
                # `actor_closed`. Dormancy is the server stopping replication
                # of an actor that is STILL ALIVE, which for a settled smoke,
                # wall or trap is its normal steady state; valplay reads
                # `actor_closed` as a despawn (pipeline/metrics/
                # ability_detail.py pairs spawn/close into a lifetime), so a
                # settled ability read as destroyed with its lifetime
                # truncated to the moment it stopped moving.
                #
                # `tools/extract_active_effects.py` in this same directory
                # already gets this right and says why. This is that reasoning
                # applied one hop later, at the last seam before the data is
                # consumed.
                #
                # WHY a distinct type rather than a flag on `actor_closed`:
                # valplay filters events by exact `type` equality and has no
                # event-type allowlist, so a new type is ignored by consumers
                # that do not know it while `actor_closed` immediately stops
                # over-reporting despawns. A flag would leave every existing
                # consumer reading dormancy as destruction until valplay
                # changed in lockstep. See BUNDLE_SCHEMA_VERSION for why this
                # is not a version bump.
                raw_event = a_event[i]
                event_type = _ACTOR_EVENT_TYPES.get(raw_event)
                if event_type is None:
                    # A fourth value the parser learned and this adapter has
                    # not. Defaulting it to a despawn is the bug above; it is
                    # published under its own type carrying the raw value, and
                    # counted, so it is a visible unknown rather than a
                    # plausible close.
                    tally.bump("unknown_actor_lifecycle_events")
                    event_type = "actor_lifecycle_unknown"
                actor_event_counts[event_type] += 1
                event = {
                    "type": event_type,
                    "time_ms": a_time[i],
                    "actor_net_guid": a_guid[i],
                    "channel": a_chan[i],
                    # The wire's own value, forwarded so a consumer can audit
                    # the mapping above instead of trusting this adapter's
                    # choice of type name.
                    "actor_event": raw_event,
                }
                events.append((a_pid[i], a_time[i], event))

        if verbose:
            print(f"  {len(actors_table):,} actor lifecycle events from actors.parquet")
            for name, count in sorted(actor_event_counts.items()):
                print(f"    {name}: {count:,}")
    else:
        # Fallback: infer from first/last field appearance (legacy behavior)
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

        # This branch infers lifetimes from the first and last field row for
        # each actor because actors.parquet is absent, so there is no `event`
        # column and no dormancy information AT ALL -- the last field row is
        # the last time the actor was seen, whatever the reason. `actor_event`
        # is therefore null rather than "close": this path cannot tell a
        # despawn from a dormancy, and claiming either would be inventing the
        # distinction the actors.parquet branch above exists to preserve.
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

    A row the parser could not name carries a value this bundle has no key to
    put it under, so it is dropped -- 1,996 of them on the documented
    reference export. The group still emits an event, which is right (the
    other rows in it are real), but a group whose rows were ALL unnamed then
    emits an event with an empty payload that is indistinguishable from a
    genuine existence signal. `tally` is what tells the two apart.
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

        # Build nested payload from field paths.
        # WHY two passes: vrfkit emits BOTH a bare array-container blob (e.g.
        # field_name="Rounds", raw_bits=the whole serialized array) AND the
        # individually-decoded sub-fields (e.g. "Rounds[0].Reports[0]...").
        # If we naively set the bare blob first, it clobbers the list that
        # _set_nested needs. Solution: first pass collects names that have
        # indexed versions (contain '['), second pass skips bare blobs for
        # those names.
        indexed_names = set()
        for ri in row_indices:
            fn = col_fn[ri]
            if fn and '[' in fn:
                # Top-level array name = everything before first '['
                indexed_names.add(fn[:fn.index('[')])

        payload = {}
        # {RAW_BLOB_PREFERRED name: blob built?} for this event; created only
        # for an event that carries such a field.
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
                # The container row of a field whose consumer decodes the
                # blob: built from raw_bits whether or not the row is also
                # typed (see RAW_BLOB_PREFERRED). `_get_value` still ran above,
                # so its multi-typed counter keeps seeing this row.
                if blob_state is None:
                    blob_state = {}
                raw = col_raw[ri]
                if raw is not None:
                    blob = _raw_blob(raw, col_bits[ri])
                    blob["TypeName"] = type_name
                    payload[fn] = blob
                    blob_state[fn] = True
                else:
                    # No bits, no blob: counted once the event is complete.
                    # A typed value is published in the blob's place rather
                    # than dropped -- the consumer skips a non-blob, visibly --
                    # unless this event already built the blob.
                    blob_state.setdefault(fn, False)
                    if value is not None and not blob_state[fn]:
                        payload[fn] = value
                continue
            if value is None and not is_raw:
                continue

            # Combat report array leaves are labelled from the wire; the bundle
            # wants the reference's member names. Keyed on the handle because
            # the declared names are not unique within one element.
            fn = _combat_report_leaf_name(gp, fn, col_handle[ri])

            # Normalize boolean field names (strip 'b' prefix)
            is_bool = col_bool[ri] is not None
            fn = _normalize_prop_field_name(fn, is_bool)

            if fn in VECTOR_PROPERTIES and isinstance(value, str):
                parsed_vec = _parse_vector_or_none(value)
                if parsed_vec is not None:
                    value = parsed_vec
            elif fn in JSON_OBJECT_PROPERTIES and isinstance(value, str):
                # No try/except: the parser writes this column, so a value that
                # will not parse is a parser bug and must stop the run rather
                # than quietly ship the raw string downstream.
                value = json.loads(value)

            # Parse the field path and set in nested structure
            parts = _parse_field_path(fn, tally)
            if len(parts) == 1 and parts[0][1] is None:
                # Simple top-level field. Skip it when it is the container of
                # indexed sub-fields: the sub-fields carry the decoded data.
                #
                # Skipped whether raw OR typed. This used to test `is_raw`, and
                # stream.rs writes a flattened array's element rows first and
                # the container row after them, so a typed container reached
                # the assignment below -- which no conflict counter watched then
                # -- and replaced the decoded list with its own value. No such
                # typed container occurs on the 1,018-export corpus
                # (2026-09-28); typing one upstream must not change the bundle.
                bare_name = parts[0][0]
                if bare_name in indexed_names:
                    continue
                # The parser flattens struct members and static-array elements
                # under one name only `handle` tells apart, so a repeat here is
                # a different property, not a newer copy: 24,060 on 02d4d478.
                if bare_name in payload:
                    tally.bump("property_key_collisions")
                payload[bare_name] = value
            elif parts[0][0] in RAW_BLOB_PREFERRED:
                # A decoded member of a raw-blob field. The blob carries it, so
                # the member is not published -- which makes a member whose
                # event built no blob a loss, counted below.
                if blob_state is None:
                    blob_state = {}
                blob_state.setdefault(parts[0][0], False)
            else:
                _set_nested(payload, parts, value, tally)

        _count_unbuilt_blobs(blob_state, tally)
        _drop_padding_elements(payload)

        # Emit even if payload is empty (some events are just existence signals)
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
    both events, the shot first -- they tie on (packet_id, time_ms) and the
    stable sort preserves that order.

    Two losses are counted rather than fixed, because the export cannot tell
    us how to fix them:

    * A group is keyed (packet_id, actor, group_path, handle), so one actor
      invoking the same function TWICE in one packet produces one group. The
      second call's parameters overwrite the first's and one rpc_received
      comes out where two calls happened. Nothing in fields.parquet marks
      where one invocation ends and the next begins -- inventing a boundary
      (row contiguity, first repeated parameter) would split legitimate single
      invocations in two, which is the same silent corruption pointed the
      other way. So the collision is reported, not guessed at.
    * A row with no field name cannot name its parameter, and a group whose
      rows are ALL unnamed cannot even name its function, so it is dropped
      whole. The count is the only trace either leaves.
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

        # Determine RPC name from first field_name
        rpc_name = None
        payload = {}
        effect_blobs = {}  # shot array parameter -> _EffectBlob
        # {raw-sourced parameter: blob built?} for this invocation; created
        # only when it carries one. See `_RAW_SOURCED_RPC_PARAMS`.
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
                # The row is the function itself, not one of its parameters.
                # Usually that means a zero-parameter RPC and there is nothing
                # to carry -- but 608 rows on 02d4d478 arrive with the whole
                # parameter block as undecoded bits, because the descriptor
                # bound no property handles for that function. Dropping them
                # made "payload: null" mean two different things: no parameters
                # at all, and parameters we could not read.
                #
                # Keyed under the function's own name. The reference emits none
                # of these functions (they sit in its 241 unbound groups), so
                # there is no key to match -- this is a vrfkit-only convention.
                #
                # A typed row is carried as well. This tested `is_raw`, so a
                # function row the overlay typed was dropped with no counter;
                # none is typed on the 1,018-export corpus (2026-09-28).
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
            # A raw-sourced parameter keeps the preserved wire blob as its
            # source whether or not the overlay typed it: a value_str overlay
            # must neither replace the shot decoder's raw input nor change a
            # consumer's blob contract. `_get_value` still ran above, so its
            # multi-typed counter keeps seeing these rows.
            sourced = _RAW_SOURCED_RPC_PARAMS.get(name)
            if sourced is not None:
                if param in sourced:
                    if blob_state is None:
                        blob_state = {}
                    raw = col_raw[ri]
                    if raw is not None:
                        bits = col_bits[ri]
                        if name == "ReplayPlayContinuousEffectAtLocation":
                            # bit_count travels with the bytes. Recomputing it
                            # downstream as len(data) * 8 would feed the last
                            # byte's padding bits to the decoder as data.
                            effect_blobs[param] = _EffectBlob(bytes(raw), bits)
                        value = _raw_blob(raw, bits)
                        is_raw = True
                        blob_state[param] = True
                    else:
                        # Counted once the invocation is complete; whatever
                        # typed value the row has still goes out below.
                        blob_state.setdefault(param, False)
                elif "[" in param:
                    member_of = param.split("[", 1)[0]
                    if member_of in sourced:
                        # A decoded member (dropped by `_normalize_rpc_param`
                        # because the blob carries it): the blob is expected.
                        if blob_state is None:
                            blob_state = {}
                        blob_state.setdefault(member_of, False)
            if value is None and not is_raw:
                continue
            # Map parameter names to match C# parser output
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

        # Build valorant_shot_received for shot RPCs
        if rpc_name == "ReplayPlayContinuousEffectAtLocation":
            # No blob guard: 7 of 02d4d478's 2,647 invocations carry only
            # scalar params and an undecoded EffectContainer. They are still
            # effect events the reference emits, and requiring a blob dropped
            # them entirely rather than letting them reach the "unknown"
            # bucket built for exactly this case.
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
            # Still emit as rpc_received too (some downstream might need it)

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
    """Sort by (packet_id, time_ms) and write events.ndjson.

    Returns ``(events_written, time_ms_regressions)``.

    # The ordering contract this file owns

    `packet_id` is the replay's own global packet counter and is the only
    total order the wire actually provides: vrfkit stamps it from a
    monotonically increasing driver counter (`driver/mod.rs`, `total_packets`)
    and never sorts, so append order IS packet order. Sorting on it here is
    therefore a re-assertion, not a reordering, and the sort is stable, so
    events tying on `(packet_id, time_ms)` keep the order the phases appended
    them in -- actors, then properties, then RPCs, and inside each phase the
    insertion order of the grouping dicts, which is the row order of
    fields.parquet, which is wire order.

    `time_ms` is NOT part of that guarantee and must not be treated as one.
    It is derived per demo frame from a raw `read_f32` with no validation
    (`vrf-frame/src/lib.rs`), and a frame whose time is not finite yields
    `time_ms = 0`. A single such frame mid-replay makes `time_ms`
    non-monotonic in file order.

    That matters downstream: valplay orders same-millisecond events by
    `(time_ms, line_index)`, which is a sound total order only while `time_ms`
    is non-decreasing in file order. So the regression is COUNTED here rather
    than repaired -- repairing it would mean reordering away from packet
    order, i.e. away from the wire -- and the count is published in the bundle
    manifest so the consumer can see that its tie-break is standing on sand
    instead of assuming it is not.
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
    """Concatenate one block's line pieces row by row; return the text's bytes.

    `pieces` alternate literal fragments (scalars) and slot texts (arrays of
    one block's length). The joined lines sit back to back in the result's
    data buffer, so the block is written as that buffer's used span.

    A NULL line is refused, not written. `binary_join_element_wise` emits
    NULL for a row with any null input, and a null contributes no bytes to
    the data buffer: the line would vanish from the file while
    `movement_rows_written` still counted it. `_write_movement` refuses null
    input columns before it gets here, so this is the second line of defence.
    """
    lines = pc.binary_join_element_wise(*pieces, "")
    if lines.null_count:
        raise RuntimeError(
            f"{lines.null_count:,} movement lines came out NULL; writing the "
            "block would drop them while still counting them as written")
    return _string_bytes(lines)


def _string_bytes(arr) -> memoryview:
    """The values of a pa.string() array as one span of bytes, back to back.

    That is the data buffer between the array's first and last offsets --
    not the whole buffer, which for a slice starts before the array's first
    value, and which Arrow may allocate larger than it fills.
    """
    if arr.type != pa.string():
        raise RuntimeError(f"expected a string array, got {arr.type}")
    offsets = numpy.frombuffer(arr.buffers()[1], dtype=numpy.int32,
                               count=len(arr) + 1, offset=arr.offset * 4)
    return memoryview(arr.buffers()[2])[offsets[0]:offsets[-1]]


def _write_movement(movement_path: Path, output_dir: Path, verbose: bool) -> tuple:
    """Write movement.ndjson, keeping the last sub-move per (packet, character).

    Returns ``(rows_read, rows_written, non_finite_rows)``. The first two
    differ by the intra-packet sub-move collapse below, so only `rows_read`
    can be compared with what the export manifest declared; publishing
    `rows_written` against `quality.movement_rows` would report every healthy
    replay as lossy. `rows_read` is `None` when the table is absent, which is
    a different fact from an empty one.

    `non_finite_rows` counts WRITTEN rows with at least one non-finite float
    (position, velocity, yaw or pitch); a collapsed sub-move is not in the
    bundle and is not counted. Such a value is written the way the encoder
    spells a non-finite float everywhere else in this bundle -- `Infinity`,
    `-Infinity`, `NaN` (see `_json_scalar_column`) -- not as `null` and not as
    a number. Python's json reads those tokens; a strict parser rejects the
    line. valplay parses with orjson when it is installed, skips a line it
    cannot parse, and then recounts fewer movement rows than
    `adapter.movement_rows_written` declares, which stops the bundle from
    publishing -- a refusal, not a quietly shorter track. The count is what
    names the cause.
    """
    if not movement_path.exists():
        # Truncate rather than return. `convert` reuses an existing output
        # directory, so converting a replay with no movement table over an
        # earlier bundle left the EARLIER replay's movement.ndjson sitting
        # beside the new events -- a whole replay's positions attributed to
        # the wrong match, under a run that printed "Conversion complete".
        # An empty file says "this replay has no movement"; a missing one is
        # indistinguishable from a stale one.
        (output_dir / "movement.ndjson").write_text("", encoding='utf-8')
        if verbose:
            print("  movement.parquet not found, movement.ndjson written empty")
        return None, 0, 0

    t0 = time.time()
    if verbose:
        print("Converting movement.parquet...")
    mv_table = pq.read_table(movement_path, columns=list(_MOVEMENT_COLUMNS))
    n_mv = len(mv_table)

    # Every movement column is declared non-null, and that is load-bearing:
    # `to_numpy` turns a uint32 column with a null into float64 with NaN, and
    # every line of it would then carry `1000.0`-style times, or `nan`. It
    # used to be assumed. A null is now refused here, naming the column.
    for name in _MOVEMENT_COLUMNS:
        nulls = mv_table.column(name).null_count
        if nulls:
            raise ValueError(f"movement.parquet column {name!r} carries "
                             f"{nulls:,} nulls; the export declares it non-null")

    # Batch-extract every column to a numpy array. `to_numpy` hands over the
    # raw buffer; `to_pylist` boxes one Python float/int per row through
    # pyarrow's per-element type dispatch (~14x slower on these 1.84M-row
    # columns, the same win the fields path gets via _numeric_column_to_pylist).
    # With no nulls, zero_copy_only=False never widens ints to float64 --
    # uint32 stays uint32, float32 stays float32.
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

    # Keep only the last sub-move per (packet_id, character).
    #
    # Our decoder walks the marker-chained move sequence inside a
    # replication packet and emits every sub-move, so a packet can produce
    # several rows sharing one time_ms. That is genuinely more data --
    # 1,687 of the 2,387 extra rows on 02d4d478 carry distinct positions,
    # and none of the reference's rows are missing from ours -- and
    # movement.parquet keeps all of it.
    #
    # The bundle cannot. valplay's posture.py requires 0 < dt before adding
    # a distance step but updates last_sample unconditionally, so for two
    # sub-moves A then B at the same ms it adds |A-prev|, skips the A->B
    # leg, and continues from B. The result is a distance_m that is *lower*
    # than the reference for every player (3.1-5.2 m on 02d4d478) -- an
    # impossible direction for finer sampling, and simply wrong.
    #
    # The reference retains only the final move per packet, and dropping
    # exactly these rows reproduces its movement_detail on 60/60 values
    # with no rounding. So this is not an approximation: it is the shape
    # the consumer was written against.
    #
    # The key is the PACKET, not the millisecond. Both readings collapse the
    # same sub-moves -- vrfkit/src/sink/stream.rs, decode_movement_rpc,
    # hoists time_ms and packet_id out of the sink BEFORE walking the move
    # chain, so every sub-move of one packet carries both identically -- but
    # keying on the millisecond ALSO merges two different packets that land
    # in the same millisecond, and there the earlier packet's final move is a
    # distinct sample, not a sub-move. It was being dropped and counted as an
    # intra-packet collapse, so the message said the loss was the intended
    # one. Keying on the packet is what the rationale above always said.
    #
    # This makes packet_id a REQUIRED movement column. It has been one for as
    # long as the table has existed (vrf-export/src/tables/movement.rs), and
    # an export old enough to lack it raises here rather than converting. That
    # is the intended trade: falling back to the millisecond key would restore
    # the silent loss this fixes, and restore it invisibly.
    #
    # Vectorised: pack the two uint32 keys into one uint64, and the FIRST
    # index of each key in the reversed array is the LAST index in the
    # original -- exactly what the per-row dict overwrite computed. `np.sort`
    # restores row order. Both keys are uint32 so the shift cannot collide.
    key64 = (mv_pid.astype(numpy.uint64) << numpy.uint64(32)) | mv_char.astype(numpy.uint64)
    first_in_rev = numpy.unique(key64[::-1], return_index=True)[1]
    keep = numpy.sort((n_mv - 1) - first_in_rev)
    del key64, first_in_rev
    movement_collapsed = n_mv - len(keep)

    # Every position and velocity component is Float32 on the wire and in
    # Parquet; widening to a Python float prints the binary artefact
    # (349.989990234375 for what the reference shows as 349.99).
    # position_bbox is min/max of the raw values, so it surfaced there even
    # though the derived distances agreed either way.
    #
    # yaw and pitch are NOT shortened. The reference serializes those two
    # through a different path from position/velocity and writes the widened
    # float32 (253.289794921875), not its shortest round-trip. Measured over
    # 4,000 reference rows: yaw and pitch are exactly float32-representable
    # 4000/4000, position.x only 25/4000 -- the discriminating signal.
    # Applying _f32_shortest here was a regression introduced in 3d37c68
    # alongside the position fix, and it moved 1,821,648 yaw and 1,699,418
    # pitch rows away from the reference. It changed no metric, which is
    # exactly why it survived.
    #
    # Select the kept rows once per column via numpy fancy indexing (C-level)
    # and turn each kept float column into its JSON TEXT once per distinct
    # value (see _json_scalar_column). One float column at a time, in
    # `_MOVEMENT_LINE`'s slot order, so only one kept copy is alive at once;
    # holding all eight cost +35-83 MB of peak working set across 11 exports.
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
    # time_ms and character_net_guid are uint32 and nearly all distinct, so
    # they are not deduplicated: Arrow's integer-to-string cast writes the
    # plain decimal digits, the same text `int.__repr__` -- and so the JSON
    # encoder -- gives an int.
    int_slots = (pa.array(mv_time[keep]), pa.array(mv_char[keep]))
    # The table and its column views are no longer needed; what the write
    # loop reads is above. Dropping them now keeps them out of its peak.
    del (mv_table, mv_time, mv_pid, mv_char, mv_px, mv_py, mv_pz,
         mv_vx, mv_vy, mv_vz, mv_yaw, mv_pitch, keep, non_finite)

    # The lines are assembled in Arrow's C++, one block at a time: each slot's
    # texts are taken for the block's rows (`take` for the deduplicated
    # floats, a cast for the ints) and `binary_join_element_wise` interleaves
    # them with `_MOVEMENT_LINE`'s literal fragments, so no Python object is
    # made per row -- only per distinct value. The text of every slot is
    # what the per-row `_MOVEMENT_LINE % (...)` join put there, so the bytes
    # are too (`MovementLineAssemblyTests`; sha256-identical on 11 exports).
    #
    # The file is written in binary mode, so the line ending is spelled
    # here: it was opened in text mode, which turns '\n' into os.linesep --
    # CRLF on Windows -- and the fragments carry that same os.linesep so each
    # platform keeps the bytes it had.
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

    # ---- Load manifest ----
    #
    # An absent manifest is survivable but never harmless, and the bundle it
    # produces does not look damaged: `_write_manifest` fills every field with
    # a plausible default ("unknown", 0, "") and the gameplay-tag table comes
    # out empty, so every effect blob is keyed by its numeric tag index and
    # every shot reports null ammo, firing state, player and attack vectors.
    # None of that is visible in the output, so it is counted here and again
    # below, where we know whether any shot actually paid the price.
    manifest = {}
    if manifest_path.exists():
        manifest = json.loads(manifest_path.read_text(encoding='utf-8'))
    else:
        tally.bump("missing_manifest")
    # The bundle manifest is written LAST, once the counts it has to carry
    # exist. It used to be written here, before a single row had been read,
    # which is why it could only ever carry header scalars.
    upstream_quality = manifest.get("quality")
    if not isinstance(upstream_quality, dict):
        # An export with no quality accounting is not a lossy conversion --
        # older vrfkit builds emitted none and this run dropped nothing -- so
        # it is deliberately NOT counted in the tally. It is still visible: the
        # manifest publishes `"quality": null`, which says "nobody counted"
        # rather than the plausible zero that would say "nothing was lost".
        upstream_quality = None

    # ---- Load fields.parquet ----
    cols = _load_field_columns(fields_path, verbose)

    # ---- Classify rows: RPC vs replicated property ----
    # We need to group and emit events in packet_id order (time order).
    # Strategy: build a list of events keyed by packet_id, then sort and write.
    t0 = time.time()
    if verbose:
        print("Grouping rows into events...")
    (actor_first, actor_last, prop_groups, rpc_groups,
     unresolved_cnc_rows) = _group_rows(cols)
    if verbose:
        print(f"  {len(prop_groups):,} property events, {len(rpc_groups):,} RPC invocations")
        print(f"  Grouped in {time.time()-t0:.1f}s")

    # ---- Build events list ----
    t0 = time.time()
    if verbose:
        print("Building event records...")

    # Containment chain for weapon identity. Loaded before the actor pass so
    # guid_class can be filled from the same loop that emits actor_spawned.
    guid_outer, guid_path, net_guid_rows_read = _load_net_guids(export_dir)

    # 1 & 2. actor_spawned and actor_closed
    events, guid_class = _build_actor_events(
        export_dir, actor_first, actor_last, verbose, tally
    )

    # 3. export_group_received events (replicated properties)
    events += _build_property_events(cols, prop_groups, tally)

    # 4. rpc_received events, and valorant_shot_received for the effect RPCs
    def equippable_lookup(firing_state_guid):
        return _resolve_equippable(firing_state_guid, guid_outer, guid_path, guid_class)

    def fire_mode_lookup(firing_state_guid, source_id):
        return _resolve_fire_mode(firing_state_guid, source_id, guid_outer, guid_path)

    shot_ctx = _ShotContext(
        # Gameplay tag table for effect blob decoding.
        _build_tag_table(manifest), equippable_lookup, fire_mode_lookup,
    )
    rpc_events, shot_count, resolved_weapon_count = _build_rpc_events(
        cols, rpc_groups, shot_ctx, tally
    )
    events += rpc_events

    # 5. The server's own labelled Event-chunk timeline.  Its rows have no
    # packet id, so _build_server_timeline_events derives a private sort key
    # from the replication time index and publishes only group/time plus the
    # fixed-layout words Rust already validated.  Event id, metadata and raw
    # payload never cross the adapter seam.
    timeline_events, server_timeline_rows_read = _build_server_timeline_events(
        export_dir, cols, verbose
    )
    events += timeline_events

    # Only now is it known whether the empty tag table cost anything: a replay
    # with no shots loses nothing by having no tag names, and counting it
    # there would cry wolf.
    if shot_count and not shot_ctx.tag_table:
        tally.bump("empty_gameplay_tag_table")

    if verbose:
        print(f"  {len(events):,} total events built in {time.time()-t0:.1f}s")
        print(f"  {shot_count:,} valorant_shot_received events")
        pct = 100 * resolved_weapon_count / shot_count if shot_count else 0
        print(f"  {resolved_weapon_count:,} with a resolved weapon ({pct:.2f}%)")

    # ---- Sort by (packet_id, time_ms) and write ----
    events_written, time_ms_regressions = _write_events(events, output_dir, verbose)
    if time_ms_regressions:
        # Not a conversion loss -- every event is still written -- but it
        # invalidates the (time_ms, line index) tie-break the consumer uses,
        # so it is surfaced with the losses rather than buried in the manifest.
        tally.bump("events_time_ms_regressions", time_ms_regressions)

    # ---- Convert movement.parquet ----
    movement_rows_read, movement_written, non_finite_movement_rows = _write_movement(
        movement_path, output_dir, verbose
    )
    # Every row is still written, so this is not a loss; it is surfaced with
    # the losses because a strict parser rejects the line it is on.
    tally.bump("non_finite_movement_rows", non_finite_movement_rows)

    # ---- Cross-check what vrfkit declared against what was actually read ----
    #
    # Only exact table-height identities are judged. `quality.movement_rows`,
    # `quality.net_guid_rows` and `quality.event_rows` are the row counts of the
    # tables vrfkit wrote, so they must equal the heights this adapter reads
    # back. `quality.net.fields`
    # is NOT such an identity -- fields.parquet carries flattened array leaves,
    # struct sub-fields and preservation rows on top of the wire fields
    # (1,277,658 rows against 429,637 fields on the 02d4d478 reference) -- so it
    # is recorded, never compared. Asserting an equality that does not hold
    # would put a permanent false alarm in front of every run, and a false alarm
    # is how a real one stops being read.
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
        # The export's own manifest does not describe the tables sitting next
        # to it. Which of the two is wrong is not knowable here, so nothing is
        # repaired and nothing is refused: the disagreement is counted, and the
        # consumer decides what a bundle whose producer contradicts itself is
        # allowed to publish.
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
        # Every conversion loss this run recorded, printed below as well.
        # Written even when empty: `{}` says the counters ran and found
        # nothing, where an absent key would not distinguish that from a run
        # that never counted.
        "losses": dict(tally),
    }
    _write_manifest(manifest, output_dir, adapter_accounting)

    return {
        "events_written": events_written,
        "movement_written": movement_written,
        "tally": tally,
    }


def _print_summary(output_dir: Path, result: dict) -> None:
    """The console summary, printed once the bundle is published.

    "complete" is a claim, so it is only made when it is true. A run that
    dropped rows or invented values says how many and of what; the counters
    that did not fire are not printed, because a block of ten zeroes is a
    block readers learn to skip. `convert` calls this after the publish
    commit, with the final path: printed from the staging step, it named a
    directory the publish renamed away, and it claimed completion before a
    publish that could still fail.
    """
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
            # os.replace(staging, output_dir) is the commit point.  Cleanup is
            # best-effort after that: reporting conversion failure here would
            # be false (and callers might retry over the newly published
            # bundle).  Leave the uniquely named old bundle recoverable.
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
    # One replay builds ~680k event dicts and holds them all until they are
    # sorted and written, so the cyclic collector rescans an ever-growing set
    # of container objects it will never find a cycle in. Measured on a 53 MB
    # replay: 6.58s -> 5.43s wall, 17.5% faster, with peak RSS unchanged at
    # 2.2 GB -- no cycles were being reclaimed, only looked for. Reference
    # counting still frees everything acyclic, and the process is short-lived
    # and exits after one conversion.
    #
    # Deliberately here and not at import time: the tools test suite imports
    # this module, and a library import must not change the caller's GC.
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
        # Derive stem from source_file in manifest or directory name
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
