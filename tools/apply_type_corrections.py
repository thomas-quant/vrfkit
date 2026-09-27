"""Apply known type corrections to the generated overlay table.

These corrections represent empirically-verified differences between the C#
descriptor declarations and the actual wire format in build 13.01:
  - Time-related Float fields are actually Double (64 bits on wire)
  - Actor bookkeeping fields "215"/"216" in non-weapon groups are variable-width
    (3 bits in 13.01), not Int32

Run after extract_descriptors.py regenerates table.rs, and BEFORE cargo fmt.

That ordering is load-bearing and used to be silent. Two of the passes below
match one-line literals, which is the shape extract_descriptors.py emits but
not the shape rustfmt leaves behind, so running the corrector on an already
formatted table applies nothing. The old script printed "Applied 0" and
exited 0 for that case -- indistinguishable from "everything was already
correct".

So the script no longer trusts its own operation count. After writing, it
verifies the END STATE of every correction against the parsed table and fails
loudly if any is missing. That check is format-independent: if the
application patterns ever rot again, the verification still catches it.

`--check` verifies the FILE, not the corrected copy. It used to apply every
correction in memory and verify that, suppressing only the write, so it could
not tell "already corrected" from "correctable and nobody corrected it" -- and
CI runs `--check`, which is how a regenerated table.rs could go green while the
Rust build used the uncorrected one. The two now report separately: a
correction missing from the corrected copy is a DEAD PATTERN, and one present
there but absent from the file means the file was NEVER CORRECTED.

Usage:
    python tools/apply_type_corrections.py            # apply, then verify
    python tools/apply_type_corrections.py --check    # verify the file, no write
"""
import argparse
import re
import sys
from collections import Counter
from pathlib import Path

if __package__:
    from .atomic_io import atomic_write_text
else:  # direct script execution
    from atomic_io import atomic_write_text

TABLE_RS = Path(__file__).parent.parent / "crates" / "vrf-decode" / "src" / "table.rs"

#: (group_path substring, field_name, required FieldType -- IN FULL).
#: One entry per correction the passes below make. Checked against the file
#: after writing; a miss is a hard failure.
#:
#: The type is the WHOLE `FieldType::...` expression and `verify` compares it
#: with `==`, because a substring test cannot see the two mistakes a type table
#: is most likely to make. `"Int32" in "FieldType::UInt32"` is True and
#: `"Byte" in "FieldType::EnumByte"` is True, so under the old check an entry
#: with the wrong signedness or the wrong byte variant verified clean -- the
#: check passed on exactly the errors it existed to catch.
EXPECTED = [
    ("TimedBomb.TimedBomb_C", "TimeRemainingToExplode", "FieldType::Double"),
    ("TimedBomb.TimedBomb_C", "DefuseProgress", "FieldType::Double"),
    ("Comp_Ability_CooldownComponent_C", "StartTimeStamp", "FieldType::Double"),
    ("Comp_Ability_CooldownComponent_C", "CooldownSeconds", "FieldType::Double"),
]
for _group in (
    "TimedBomb.TimedBomb_C",
    "EquippablePickupProjectile.EquippablePickupProjectile_C",
    "EquippableGroundPickup.EquippableGroundPickup_C",
    "OwnerExclusivePlayerInfo",
    "Projectile_Phoenix_Q_FlameWall_ThroughWall.Projectile_Phoenix_Q_FlameWall_ThroughWall_C",
):
    for _field in ("215", "216"):
        EXPECTED.append((_group, _field, "FieldType::EnumRemainingBits"))
EXPECTED += [
    ("/Game/Characters/", "ReplayLastTransformUpdateTimeStamp",
     "FieldType::Float"),
    ("SmokeScreen", "ReplicatedMovement",
     "FieldType::RepMovement { rotation: RotatorQuantization::ByteComponents }"),
    ("AresEquippableDataTracker", "OriginalBuyerTeam", "FieldType::Raw"),
    ("MulticastNotifyDamage_Base", "EquippableUsed", "FieldType::ObjectNetGuid"),
    ("MulticastNotifyDamage_Point", "EquippableUsed", "FieldType::ObjectNetGuid"),
    ("MulticastNotifyDamage_Base", "DamageOrigin",
     "FieldType::VectorNetQuantize { scale: 100 }"),
    ("MulticastNotifyDamage_Point", "DamageOrigin",
     "FieldType::VectorNetQuantize { scale: 100 }"),
    ("MulticastNotifyDamage_Point", "DamageImpactLocation",
     "FieldType::VectorNetQuantize { scale: 1 }"),
    ("MulticastNotifyDamage_Point", "DamageImpactBoneRelativeLocation",
     "FieldType::VectorNetQuantize { scale: 1 }"),
    ("MulticastNotifyDamage_Point", "DamageDirection", "FieldType::VectorNetQuantizeNormal"),
    ("MulticastNotifyDamage_Point", "DamageImpactNormal", "FieldType::VectorNetQuantizeNormal"),
    ("/Script/ShooterGame.EquippableStateMachineComponent", "TransitionContext",
     "FieldType::ObjectNetGuid"),
    ("ReplayEffectComponent:ReplayPlayContinuousEffectAtLocation", "AllianceFilter",
     "FieldType::EnumByte"),
    ("MulticastNotifyDamage_Base", "DeathMontageEffectOverride", "FieldType::ObjectNetGuid"),
    ("MulticastNotifyDamage_Point", "DeathMontageEffectOverride", "FieldType::ObjectNetGuid"),
    ("MulticastNotifyDamage_Base", "DeathMontageEffectOverrideContext",
     "FieldType::ObjectNetGuid"),
    ("MulticastNotifyDamage_Point", "DeathMontageEffectOverrideContext",
     "FieldType::ObjectNetGuid"),
]

#: Entries the WIRE carries that the C# descriptors cannot declare, because the
#: group did not exist when the reference was pinned.
#:
#: This is a different kind of claim from every correction above. Those say
#: "the descriptor declares X and the wire disagrees". This says "the descriptor
#: is SILENT and here is the type anyway", so the bar is higher and only one
#: case clears it today.
#:
#: `/Script/ShooterGame.BaseTeamState` is new in build 13.02, which deleted
#: `BombGameState.TeamEconomy` and moved team economy into a separately
#: replicated actor. The property NAMES did not change, and the reference
#: declares their types at the pinned commit --
#: `GameState/AresTeamEconomy.cs:11-12`:
#:
#:     public sealed record AresTeamEconomyUpdate(
#:         int Index, uint? ReplicationId,
#:         int? LoadoutValue,            <- Int32
#:         int? AverageLoadoutValue);    <- Int32
#:
#: so this is a descriptor-sourced type for a relocated property, not a type
#: guessed from values. `OwnerExclusivePlayerInfo.{Start,End}OfRoundLoadoutValue`
#: are Int32 in the same reference, which corroborates the family.
#:
#: Corroborated on the wire, not decided by it: all 44+44 payloads are 32 bits,
#: read as little-endian i32 they run 4300/4150 at round 1 to 34300, and
#: AverageLoadoutValue is EXACTLY LoadoutValue/5 on every row (a five-player
#: team).
#:
#: DELIBERATELY NOT ADDED: `Wins`, `Points`, `InitialRole`, `TeamRole`,
#: `TeamPlayerStates`, `TeamComponent`, `TeamExclusiveTeamInfo`. The same
#: 13.02 group declares all of them and the reference declares NONE of them
#: under any group, so there is no source for their types. They keep their raw
#: bits and stay untyped. That line is what makes this addition defensible;
#: widening it by eye would undo the reason it is allowed at all.
#: `ChosenCeremonyForRound` is the second entry of this kind, and it rests on
#: wire evidence alone -- no descriptor names it, in either build. It is added
#: because that evidence is unusually complete and completely self-checking:
#:
#:   246 occurrences across 6 replays and BOTH builds (13.01 and 13.02)
#:   126 are 16 bits and read as IntPacked resolve, 126 of 126, to an actor
#:       whose class path ends in `Ceremony_C` -- Default, Clutch, Closer,
#:       Flawless, Ace, TeamAce
#:   120 are 8 bits of `00`, i.e. GUID 0, the null reference, written at round
#:       start and replaced by a real ceremony at round end
#:     0 odd GUIDs, and a dynamic NetGUID must be even
#:
#: An ObjectNetGuid reads an IntPacked, so it covers both widths without a
#: special case. A wrong type here cannot hide: every non-zero value has to
#: name a ceremony actor that the same export already lists in actors.parquet.
#:
#: This is a WEAKER justification than the BaseTeamState pair above, where the
#: reference declares the property's type and only its group moved. Recorded as
#: such deliberately -- see docs/archive/PROJECT_STATUS.md 31-C and 32.
#: Swiftplay's copy of this field is NOT listed here. It was, briefly, and the
#: entry was removed when `GROUP_ALIASES` landed in `vrf-decode/src/overlay.rs`
#: -- Swiftplay's game state now falls back to the Bomb class for every field,
#: so a second entry would state the same fact twice and drift from it.
#:
#: `MoneyManagementComponent.{Money,StartOfRoundMoney,TotalMoneyGranted}` are
#: the live per-player credits, replicated under this group on BOTH 13.01 and
#: 13.02 (verified on 02d4d478 and 13.02 Demos files). No descriptor declares
#: the group: `Money`/`TotalMoneyGranted` appear under no group in the C#
#: reference, and `StartOfRoundMoney` is declared only under
#: `OwnerExclusivePlayerInfo` (OwnerExclusivePlayerInfoDescriptor.cs:93), a
#: separate end-of-round snapshot path. So the extractor emits nothing and the
#: fields ship untyped.
#:
#: All three are 32-bit on every row. Read as little-endian i32 they are
#: unambiguous credits: `Money` is 800 across all ten actors at pistol-round
#: start (t=8ms) and runs 0..9000 in multiples of 50; `StartOfRoundMoney` is
#: 800 for active players and 0 otherwise; `TotalMoneyGranted` is cumulative,
#: 800..34200. `StartOfRoundMoney`'s type is descriptor-sourced (Int32), the
#: same strength as the BaseTeamState pair above; `Money` and
#: `TotalMoneyGranted` rest on wire evidence alone, like ChosenCeremonyForRound.
#: No other MoneyManagementComponent field exists on the wire, so this does not
#: widen by eye -- the DELIBERATELY NOT ADDED line above stays intact.
#:
#: `BombPlayerState.Ping` is the largest untyped wire-declared field (20193 rows
#: on 02d4d478, 222855 across the 11-replay bundle). No descriptor declares it.
#: The encoding was settled in docs/archive/PROJECT_STATUS.md 18: bit_count is
#: always 16, and the value is a 16-bit little-endian unsigned integer that
#: behaves like latency in
#: milliseconds -- on 02d4d478: min 6, p5 10, p50 15, p90 19, p99 25, max 473,
#: 57 distinct values. Typed as `SerializedInt{65536}` (16 bits LSB-first), which
#: read_serialized_int reads as exactly 16 bits and satisfies decode_field's
#: full-consumption guard. This is the same wire-evidence ADDITION class as Money
#: -- 18-D's "no descriptor declares it" objection is exactly the gate Money
#: cleared, and the per-player latency series is now wanted.
#:
#: Status-effect components. `Comp_Actor_Concussable` and
#: `Comp_AbilityFuelSystem` are GENERIC Blueprint components shipped under
#: `/Game/Characters/Components/`, not agent-specific classes -- no C#
#: descriptor declares either group, so without these entries the fields
#: ship as raw bits even though they decode cleanly to real values.
#:
#: Concussion was surveyed for agent-commonality on the 98605b1b Demos
#: export: the component is on 9 distinct actors spanning eight agents
#: (Phoenix, Breach, Smonk, Clay, Guide, Wushu, Terra, Pandemic, Deadeye)
#: plus Guide's PossessableScout pawn, with identical field names and bit
#: widths on every actor -- typing the group once covers every agent. The
#: widths are self-checking across all 375 rows: ConcussStartTime and
#: ConcussEndTime are 32 bits on all 39 rows each (Float), ConcussLevel is
#: 64 bits on all 297 rows (Double). Read as Float the start/end times are
#: game-seconds (389.5/392.0 ... 1916.7/1919.2 -- the ~2.5 s gap is the
#: concussion duration); read as Double ConcussLevel runs the 0..1
#: intensity ramp.
#:
#: AbilityFuel is per-ability rather than per-player (on 98605b1b: Sage/
#: Guide's heal `Ability_Guide_4_Heal` and Viper/Pandemic's smoke), but the
#: component path is shared so one entry covers every actor that carries
#: it. CurrentFuel is 64 bits on all 5702 rows and reads as Double a smooth
#: 1.0 -> 0.0 drain (1.0, 0.9993, 0.9909, 0.9824, ...); IsFuelDraining is
#: 1 bit on all 60 rows, raw 0x00/0x01 -- an unambiguous Bool. CurrentFuel
#: was guessed Float in the task brief, but the wire is 64-bit; Double is
#: what decodes.
#:
#: DELIBERATELY NOT ADDED: `Comp_AbilityStatisticsReplicator.AbilityCasts
#: ThisRound`. It is on all ten player characters (agent-common, like
#: Concussion), but the brief's "Int32" guess is wrong: the payload runs 16
#: to 6712 bits across 955 rows and the bytes after the 16-bit `0000` empty
#: case decode to ASCII GUID strings -- it is a variable-width array of
#: struct entries, not a 32-bit integer. Forcing Int32 would decode only
#: the smallest rows and mislabel every other row, so it stays raw until
#: the array element type is worked out. Also deliberately not added:
#: `FuelFull`/`FuelEmpty` (0-bit payloads, no value to decode) and
#: `BlindManagerComponent.ActiveBlinds` (a variable-width array).
#: `LongestActiveBlindDuration` was outside the brief and not yet
#: corroborated when this note was written; it is now typed below, in the
#: ADDITIONS list, once its Float read was confirmed (0.0..2.1 s, agent-
#: common) -- see `blind_duration_is_typed` in
#: crates/vrf-decode/src/tests/overlay.rs.
#: Final-sweep ADDITIONS. Four descriptor-silent groups whose raw bits decode
#: cleanly to real game values on the 98605b1b Demos export. Each is held to
#: the same wire-evidence bar as Money/Ping: a single consistent bit width on
#: every row, and a value distribution that a wrong type cannot reproduce.
#:
#: `DamageableComponent:MulticastNotifyHeal.HealTaken` and
#: `:MulticastNotifyOverhealDecay.DecayApplied`. The DamageableComponent C#
#: descriptor (DamageableComponentClassNetCacheDescriptor.cs) declares only the
#: two MulticastNotifyDamage_* handles, so the heal/decay RPC parameter groups
#: -- which the wire still carries by name -- have no declared type. The RPC
#: sink resolves these under their colon-group paths
#: (`/Script/ShooterGame.DamageableComponent:MulticastNotifyHeal` etc.) with
#: the bare parameter name, the same shape as the EquippableUsed correction.
#: HealTaken is 32 bits on all 1252 rows and reads as Float a 0.05..400 heal
#: magnitude; the 0x3f800000 pattern (1.0f) recurs and is a float signature no
#: int produces. DecayApplied is 32 bits on all 699 rows and reads as Float a
#: 0.07..50 overheal-decay amount clustering at 0.195. The sibling
#: LifeChangeBySection (177 bits, a struct array) and the *Instigator/*Causer
#: actor refs are deliberately NOT added: the first is variable-width and the
#: second are metadata refs this project does not type.
#:
#: `PlayerScoreComponent.Score` is the per-player combat score. No descriptor
#: declares the group. 32 bits on all 430 rows. Read as Float the bytes are
#: denormal slop (~1e-44) -- the float read rules itself out -- but read as
#: Int32 the values run 21..5833 with 415 distinct values, the exact shape of
#: a cumulative combat score across a full match.
#:
#: `BasicCombatStatsComponent` carries the authoritative cumulative scoreboard
#: K/D/A. On release-13.02 all 407 observed updates are exactly 32 bits. Read
#: little-endian as Int32, the final counters for all ten players match the
#: in-game scoreboard exactly, including two post-round objective-bomb deaths
#: that the kill RPC stream exposes but the scoreboard deliberately excludes.
#:
#: `ZoomMultiplierComponent` drives the ADS/scope FOV transition. No descriptor
#: declares the group. The five fields below are 32 bits on every row with zero
#: NaN: SourceFov/TargetFov run 20.6..103.0 and 103.0 is Valorant's documented
#: default hip-fire FOV; SourceFov1P/TargetFov1P run 5.0..70.0 and 70.0 is the
#: default 1P FOV; TotalTransitionTimeDuration runs 0.0..0.25 (the ADS
#: transition seconds). A wrong type cannot yield 103.0. Deliberately NOT added:
#: SourceZoomLevel/TargetZoomLevel (~70% of rows are the 0xFFFFFFFF sentinel,
#: which Float reads as NaN -- an enum-or-sentinel, not a clean float) and
#: CooldownOption/TransitionState (2-bit enums the wire count alone cannot
#: disambiguate from a SerializedInt).
#:
#: `FiniteSpeedMovementComponent.MaximumRange` is a projectile's max travel
#: distance in Unreal units. No descriptor declares the group. 32 bits on all
#: 11699 rows, reads as Float 397.6..49986.1 with the mode at ~19993 UU
#: (~500 m), the right order of magnitude for a Valorant projectile.
#:
#: Two time fields were verified across all 714 release-13.01--13.05 exports.
#: `ServerMovementTime` is 32 bits on all 4,571,175 rows in exactly the four
#: movement-component groups listed below. An independent little-endian Float
#: read is finite everywhere and monotonic for all 337,754 actors. It is an
#: movement clock in seconds: values begin near 1/128 second and are consistent
#: with an actor-relative clock, but the exact epoch (including whether it is
#: actor spawn) is not established. Small residuals reflect the 128 Hz value
#: grid and replication delay; Phoenix FlareCurve actors can open their channels
#: about 0.65 seconds after the movement clock starts.
#:
#: `ReplayLastTransformUpdateTimeStamp` is 32 bits on all 32,978,229 rows in 42
#: character/pawn groups and reads as a finite, actor-monotonic Float in seconds.
#: The descriptor emitted 33 of these as Skip; the correction pass below changes
#: those entries, and the nine descriptor-silent pawn groups are added here.
#: The value behaves as server world time with a file-specific offset from
#: replay time: 672/714 files have an offset around 10 seconds, while 42 range
#: from 10.8 to 110.1 seconds. A consumer must estimate the per-file offset
#: rather than subtracting 10.
#:
#: Deliberately NOT added: FiniteSpeedMovementComponent.bIsActive (1 bit but all
#: 574 rows are 0x01 -- a constant that carries no consumer information),
#: NumCollisions (32 bits but the i32 values are all 0/1, indistinguishable from
#: a Bool widened to 32 bits by the property block), and RequestedIgnoreActors
#: (a variable-width array).
ADDITIONS = [
    # Crosshair settings: every adopted name was independently decoded on
    # retained 13.01/13.02/13.04/13.05 payloads with exact consumption. B is
    # deliberately absent: its color handles are 8-bit, but handle 208 uses
    # the same name for a 32-bit field, which a name-keyed overlay cannot split.
    *[("/Game/GameModes/Bomb/BombPlayerState.BombPlayerState_C", field,
       "FieldType::Bool") for field in (
        "bHasOutline", "bDisplayCenterDot", "bFadeCrosshairWithFiringError",
        "bShowSpectatedPlayerCrosshair", "bFixMinErrorAcrossWeapons",
        "bAllowVertScaling", "bShowMovementError", "bShowShootingError",
        "bShowMinError", "bShowLines", "bUsePrimaryCrosshairForADS",
        "bUseCustomCrosshairOnAllPrimary", "bUseAdvancedOptions",
    )],
    *[("/Game/GameModes/Bomb/BombPlayerState.BombPlayerState_C", field,
       "FieldType::Float") for field in (
        "OutlineThickness", "OutlineOpacity", "CenterDotSize",
        "CenterDotOpacity", "LineThickness", "LineLength",
        "LineLengthVertical", "LineOffset", "Opacity", "FiringErrorScale",
        "MovementErrorScale",
    )],
    ("/Game/GameModes/Bomb/BombPlayerState.BombPlayerState_C", "G", "FieldType::Byte"),
    ("/Game/GameModes/Bomb/BombPlayerState.BombPlayerState_C", "R", "FieldType::Byte"),
    ("/Game/GameModes/Bomb/BombPlayerState.BombPlayerState_C",
     "ProfileName", "FieldType::FString"),
    # Tidal Wave types come from upstream 99d9646 (pinned at b51d674) and each
    # was independently confirmed against retained payload widths and ranges.
    # AliveChunks is a variable-width collection and remains raw.
    *[("/Game/Characters/Mage/S0/Ability_X/"
       "GameObject_Mage_X_TidalWave_Chunk.GameObject_Mage_X_TidalWave_Chunk_C:"
       "MulticastInitialize", field, field_type) for field, field_type in (
        ("ChunkIndex", "FieldType::Int32"),
        ("Generation", "FieldType::Int32"),
        ("Num Chunks", "FieldType::Int32"),
        ("ChunkSpacing", "FieldType::Float"),
        # The replay exports a space; upstream labels handle 4 `VelocityIn`.
        ("Velocity In", "FieldType::Double"),
        ("Anchor Spacing In", "FieldType::Double"),
        ("Num Crossfade Anchors In", "FieldType::Int32"),
        ("PreviousChunk", "FieldType::ObjectNetGuid"),
    )],
    ("/Game/Characters/Mage/S0/Ability_X/"
     "GameObject_Mage_X_TidalWave_Chunk.GameObject_Mage_X_TidalWave_Chunk_C:"
     "MulticastWallStartLinger", "LingerWallStopPosition", "FieldType::Double"),
    ("/Game/Characters/Mage/S0/Ability_X/"
     "GameObject_Mage_X_TidalWave_Chunk.GameObject_Mage_X_TidalWave_Chunk_C:"
     "MulticastWallStartLinger", "FinalEndpointReached", "FieldType::Bool"),
    ("/Game/Characters/Mage/S0/Ability_X/"
     "GameObject_Mage_X_TidalWave.GameObject_Mage_X_TidalWave_C:MulticastStopWave",
     "FinalEndpointReached", "FieldType::Bool"),
    ("/Game/GameModes/Bomb/BombGameState.BombGameState_C",
     "ChosenCeremonyForRound", "FieldType::ObjectNetGuid"),
    # Phoenix's wall, the other class declaring `MulticastAddSmokeScreenPoint`.
    # Viper's `SmokeScreenManager` was typed and this one was not, so 2,791 rows
    # over 31 replays came out null while decode errors stayed at 0 -- and
    # because Viper's side worked, the ability looked handled.
    #
    # The checksum fallback could not carry the type across and was right not
    # to: Unreal hashes the property, and these are different properties
    # (2794273677 / 1639439377 against Viper's 2235276067 / 2983776962). It
    # refused instead of guessing, which is the behaviour that made this a
    # missing name rather than a wrong type.
    #
    # Admitted on wire evidence, same bar as the rest: every row is 192 bits
    # (3 x f64); `Translation` reads as map coordinates (7211.7, 1670.3, 96.0)
    # on an Ascent replay; `Scale3D` is (1,1,1) on every row, which no other
    # reading of those bits produces. The third parameter of the same RPC
    # arrives as handle `249` with no name from the replay and is left raw --
    # naming a handle is what HANDLE_ADDITIONS is for, and its bar is higher.
    ("/Game/Characters/Phoenix/S0/Ability_Q/Production/"
     "GameObject_Phoenix_Q_FlameWallManager_Production."
     "GameObject_Phoenix_Q_FlameWallManager_Production_C:MulticastAddSmokeScreenPoint",
     "Translation", "FieldType::VectorDouble"),
    ("/Game/Characters/Phoenix/S0/Ability_Q/Production/"
     "GameObject_Phoenix_Q_FlameWallManager_Production."
     "GameObject_Phoenix_Q_FlameWallManager_Production_C:MulticastAddSmokeScreenPoint",
     "Scale3D", "FieldType::VectorDouble"),
    # `249` on the effect-placement RPCs: the rotation that pairs with `248`.
    #
    # `248` is already here as the placement location. `249` follows it,
    # unnamed, on 441,814 rows over 20 replays, and three things settle it as a
    # `RotationShort`. The widths are 3, 19, 35 and 51 bits, which is exactly
    # `3 + 16 x (flags set)` for that type's three conditional components and
    # is not a shape any other type produces. Decoding all 441,814 that way
    # consumes every payload exactly, no leftover at any width. And the table
    # already carries `ReplayPlayContinuousEffectAtLocation.Rotation` as
    # `RotationShort` -- the same UFunction parameter, from a replay that named
    # it instead of sending the number.
    #
    # Decoded, yaw is set on 92.5% of rows, pitch on 14.5% and roll on 0.1%,
    # all on a 0.0055-degree lattice: a ground-placed effect facing somewhere.
    #
    # Not to be confused with the other `249` above, which is a `VectorDouble`.
    # That one is 192 bits under a different checksum; this family shares
    # 2526428638 and is 19 bits on most rows. Same number, different property --
    # which is the whole reason the checksum is the thing to check.
    ("/Script/ShooterGame.LocationalEffectManagerComponent:ClientPlayOneShotEffectAtLocation",
     "249", "FieldType::RotationShort"),
    ("/Script/ShooterGame.ReplayEffectComponent:ReplayPlayContinuousEffectAtLocation",
     "249", "FieldType::RotationShort"),
    ("/Script/ShooterGame.ReplayEffectComponent:ReplayPlayOneShotEffectAtLocation",
     "249", "FieldType::RotationShort"),
    ("/Script/ShooterGame.EffectManagerComponent:ReplayRecordOneShotEffect",
     "249", "FieldType::RotationShort"),
    ("/Script/ShooterGame.EffectManagerComponent:ReplayRecordContinuousEffect",
     "249", "FieldType::RotationShort"),
    # The RNG component's seed. 120,853 rows, one group, one checksum, 32 bits
    # on every row, and 120,852 of the values distinct across the full i32
    # range -- which is what a seed looks like and what a counter, a time or a
    # GUID does not. The sibling `AuthInitialRandomSeed` matches in width and
    # in that near-total distinctness.
    #
    # Int32 rather than UInt32 is not settled by the data: the same 32 bits
    # read either way. It follows Unreal's `FRandomStream`, whose seed is an
    # `int32`, and the choice only changes the sign of half the values.
    ("/Script/ShooterGame.NetworkedRandomNumberGeneratorComponent",
     "AuthCurrentRandomSeed", "FieldType::Int32"),
    ("/Script/ShooterGame.BaseTeamState", "AverageLoadoutValue", "FieldType::Int32"),
    ("/Script/ShooterGame.BaseTeamState", "LoadoutValue", "FieldType::Int32"),
    ("/Script/ShooterGame.DamageableComponent:MulticastNotifyHeal",
     "HealTaken", "FieldType::Float"),
    ("/Script/ShooterGame.DamageableComponent:MulticastNotifyOverhealDecay",
     "DecayApplied", "FieldType::Float"),
    ("/Script/ShooterGame.FiniteSpeedMovementComponent",
     "MaximumRange", "FieldType::Float"),
    ("/Script/ShooterGame.FiniteSpeedMovementComponent",
     "ServerMovementTime", "FieldType::Float"),
    ("/Script/ShooterGame.SplineMovementComponent",
     "ServerMovementTime", "FieldType::Float"),
    ("/Script/ShooterGame.PrecalculatedProjectileMovementComponent",
     "ServerMovementTime", "FieldType::Float"),
    ("/Game/Characters/Components/Comp_Projectile_FloatCurveMovement."
     "Comp_Projectile_FloatCurveMovement_C",
     "ServerMovementTime", "FieldType::Float"),
    ("/Game/Characters/Cashew/S0/Ability_E/"
     "AIPawn_Cashew_E_SeekingTargetMissile."
     "AIPawn_Cashew_E_SeekingTargetMissile_C",
     "ReplayLastTransformUpdateTimeStamp", "FieldType::Float"),
    ("/Game/Characters/Clay/S0/Ability_E/Pawn_Clay_E_Boomba."
     "Pawn_Clay_E_Boomba_C",
     "ReplayLastTransformUpdateTimeStamp", "FieldType::Float"),
    ("/Game/Characters/Guide/S0/Ability_Q/Pawn_Guide_Q_PossessableScout."
     "Pawn_Guide_Q_PossessableScout_C",
     "ReplayLastTransformUpdateTimeStamp", "FieldType::Float"),
    ("/Game/Characters/Gumshoe/S0/Ability_E/"
     "Pawn_Gumshoe_E_PossessableCamera.Pawn_Gumshoe_E_PossessableCamera_C",
     "ReplayLastTransformUpdateTimeStamp", "FieldType::Float"),
    ("/Game/Characters/Killjoy/S0/Ability_E/Pawn_Killjoy_E_Turret."
     "Pawn_Killjoy_E_Turret_C",
     "ReplayLastTransformUpdateTimeStamp", "FieldType::Float"),
    ("/Game/Characters/Killjoy/S0/Ability_Q/"
     "Pawn_Killjoy_Q_StealthAlarmbot.Pawn_Killjoy_Q_StealthAlarmbot_C",
     "ReplayLastTransformUpdateTimeStamp", "FieldType::Float"),
    ("/Game/Characters/Pine/S0/Ability_E/Pawn_Pine_E_RadEater."
     "Pawn_Pine_E_RadEater_C",
     "ReplayLastTransformUpdateTimeStamp", "FieldType::Float"),
    ("/Game/Characters/Rift/S0/Ability_X/WorldTargeting/"
     "Rift_TargetingForm_PC.Rift_TargetingForm_PC_C",
     "ReplayLastTransformUpdateTimeStamp", "FieldType::Float"),
    ("/Game/Characters/Stealth/S0/Ability_4/Pawn_Stealth_4_Decoy_V2."
     "Pawn_Stealth_4_Decoy_V2_C",
     "ReplayLastTransformUpdateTimeStamp", "FieldType::Float"),
    ("/Script/ShooterGame.MoneyManagementComponent", "Money", "FieldType::Int32"),
    ("/Script/ShooterGame.MoneyManagementComponent", "StartOfRoundMoney", "FieldType::Int32"),
    ("/Script/ShooterGame.MoneyManagementComponent", "TotalMoneyGranted", "FieldType::Int32"),
    ("/Game/GameModes/Bomb/BombPlayerState.BombPlayerState_C",
     "Ping", "FieldType::SerializedInt { max: 65536 }"),
    ("/Script/ShooterGame.BasicCombatStatsComponent",
     "AggregateKills", "FieldType::Int32"),
    ("/Script/ShooterGame.BasicCombatStatsComponent",
     "AggregateDeaths", "FieldType::Int32"),
    ("/Script/ShooterGame.BasicCombatStatsComponent",
     "AggregateAssists", "FieldType::Int32"),
    ("/Script/ShooterGame.PlayerScoreComponent", "Score", "FieldType::Int32"),
    ("/Game/Characters/Components/Comp_Actor_Concussable.Comp_Actor_Concussable_C",
     "ConcussStartTime", "FieldType::Float"),
    ("/Game/Characters/Components/Comp_Actor_Concussable.Comp_Actor_Concussable_C",
     "ConcussEndTime", "FieldType::Float"),
    ("/Game/Characters/Components/Comp_Actor_Concussable.Comp_Actor_Concussable_C",
     "ConcussLevel", "FieldType::Double"),
    ("/Game/Characters/Components/Comp_AbilityFuelSystem.Comp_AbilityFuelSystem_C",
     "CurrentFuel", "FieldType::Double"),
    ("/Game/Characters/Components/Comp_AbilityFuelSystem.Comp_AbilityFuelSystem_C",
     "IsFuelDraining", "FieldType::Bool"),
    ("/Script/ShooterGame.BlindManagerComponent",
     "LongestActiveBlindDuration", "FieldType::Float"),
    ("/Script/ShooterGame.ZoomMultiplierComponent",
     "SourceFov", "FieldType::Float"),
    ("/Script/ShooterGame.ZoomMultiplierComponent",
     "SourceFov1P", "FieldType::Float"),
    ("/Script/ShooterGame.ZoomMultiplierComponent",
     "TargetFov", "FieldType::Float"),
    ("/Script/ShooterGame.ZoomMultiplierComponent",
     "TargetFov1P", "FieldType::Float"),
    ("/Script/ShooterGame.ZoomMultiplierComponent",
     "TotalTransitionTimeDuration", "FieldType::Float"),
    # UsableComponent drives every hold-to-interact object: spike plant/defuse,
    # ultimate-orb pickup, doors. HighestProgress is a 0..1 float that advances
    # 1/128 per tick (a u32 read is non-monotonic; only f32 ramps linearly);
    # bIsActive is the 1-bit "someone is interacting" flag. No C# descriptor --
    # typed from wire evidence, same bar as Ping/Money.
    ("/Script/ShooterGame.UsableComponent", "HighestProgress", "FieldType::Float"),
    ("/Script/ShooterGame.UsableComponent", "bIsActive", "FieldType::Bool"),
    # `ReadyingStateComponent.AuthEquipSpeed`: the equip-speed state of a
    # weapon being readied. No descriptor declares it. Measured 2026-09-28
    # over the 1,018 replays audited at 259ed10, rows selected by name OR by
    # checksum 3151779304 in any group (only this identity carries either):
    # 866,096 main + 149,419 checkpoint rows, exactly 3 bits on every row of
    # every build that has it (none on 12.10, 12.11, 13.00), zero padding.
    # Main-stream values are {0, 1, 2} on every build (432,879 / 347,664 /
    # 85,553; 11.06 alone 1,400 / 1,297 / 104). On every 25th export the
    # transitions are only 0 <-> 1 and 0 <-> 2, and 2 holds for a median 203 ms.
    #
    # What makes it more than a width: the same-width sibling
    # `AutoEquipTransitionContext.AutoEquipSpeed` (typed EnumByte by this
    # repo's own vendored AdditionalComponentDescriptors.cs, 1dee99f -- so a
    # sibling that decodes cleanly under the same reader, not an independent
    # authority) is 3 bits with {0, 1, 2} on every build, its Rust value equals
    # an independent decode on 58,384 of 58,384 sampled rows, and in the same
    # packet on the same actor it equals AuthEquipSpeed on 1,986 of 1,999
    # rows. At a uniform 3 bits the only SerializedInt reading that fits is
    # max 8, which gives the same integers, so the ZoomMultiplier "wire count
    # alone" refusal above does not apply.
    #
    # CHECKPOINT CAVEAT: all 149,419 checkpoint rows read 0 -- while the
    # sibling AutoEquipSpeed and EquipSpeedOverride are non-zero in the same
    # checkpoints and 0.2% of carriers are mid-readying at those instants. The
    # bits really are 000; the meaning of this field at a checkpoint is
    # unexplained, so checkpoint AuthEquipSpeed is not readying state. Enum
    # member names are not established; only the integer is typed.
    #
    # Known limitation, shared by every ADDITIONS entry: the key is group +
    # name, not checksum. decode_byte fails loudly only at 0 or >8 bits, so a
    # future build that reused the name for a different enum of <=8 bits would
    # decode silently as the wrong value. generate_scoped_types.py has no
    # EnumByte, which is why this is not a checksum-scoped type.
    ("/Script/ShooterGame.ReadyingStateComponent", "AuthEquipSpeed",
     "FieldType::EnumByte"),
    # `AresInventory.CorrectionIndex` / `LastSeenClientCorrectionIndex`: the
    # inventory's server/client correction counters. AresInventoryDescriptor.cs
    # declares neither, so these are wire-evidence ADDITIONS like Money.
    # Measured 2026-09-28 over the 1,018 audit replays (checksums 3198546915 /
    # 1076231069, each carried by this one field only):
    #
    #   CorrectionIndex: 1,041,822 main + 181,108 checkpoint rows, 32 bits on
    #   every row of all 24 builds. As little-endian i32: 1..2011, never 0 or
    #   negative, strictly increasing per (actor, object) on all 1,001,823
    #   consecutive main-stream pairs. Every one of the 181,108 checkpoint
    #   values equals the last main-stream value for the same (actor, object)
    #   at or before the checkpoint -- two streams, one counter. Read as f32
    #   every value is a denormal (the PlayerScoreComponent.Score shape), and
    #   the max rules out a widened Bool.
    #   LastSeenClientCorrectionIndex: 948,502 main + 181,097 checkpoint rows,
    #   32 bits everywhere, 1..2010, strictly increasing per actor on all
    #   921,268 pairs, and never >= CorrectionIndex at the same (time, actor,
    #   object): L == C-1 on 814,054 main rows, L < C-1 on 134,448, L >= C on
    #   0 (checkpoints 179,990 / 1,107 / 0).
    #
    # The wire handle steps from 29/30 (11.06-12.03) to 30/31 (12.04 on);
    # name keying is immune to that. Int32 over UInt32 follows the Int32
    # RespawnNumber of the same descriptor; 1..2011 cannot settle the sign.
    # The meaning is inferred from the names and the counter shape only.
    ("/Script/ShooterGame.AresInventory", "CorrectionIndex", "FieldType::Int32"),
    ("/Script/ShooterGame.AresInventory", "LastSeenClientCorrectionIndex",
     "FieldType::Int32"),
    # The 192-bit RPC vectors. Unreal serialises an FTransform parameter as
    # three separate double vectors on this wire -- rotation, translation,
    # scale -- and no descriptor declares any of them, so 54,859 rows arrived
    # raw. The table's `MulticastPlayContinuousEffect:Transform` entry
    # (FieldType::Transform, 320 bits) is dead against this stream: no
    # `Transform` parameter exists in the replay's own schema.
    #
    # Read as 3 x f64 they are unambiguous. `Scale3D` is exactly
    # (1.0, 1.0, 1.0) on every row, which no other reading produces -- 6 x f32
    # gives (0, 1.875, 0, 1.875, 0, 1.875). The `248` locations are map
    # coordinates in Unreal units with plausible floor heights, and `249` is a
    # rotator carrying negative zero, which a wrong split would not produce.
    #
    # Independently cross-checked: `BombPlantedRPC.PlantLocation` and
    # `MulticastActivateBombSiteEffects.BombLocation` are two unrelated RPCs
    # that report byte-identical coordinates, 9 rows each against 9
    # `spikePlanted` events.
    #
    # The replay's own `compatible_checksum` agrees with the grouping and was
    # not used to derive it: every `248` is 598402184, every `249` is
    # 747197698, every `Translation` 2235276067, every `Scale3D` 2983776962,
    # across all the groups below.
    ("/Script/ShooterGame.LocationalEffectManagerComponent:ClientPlayOneShotEffectAtLocation",
     "248", "FieldType::VectorDouble"),
    ("/Script/ShooterGame.ReplayEffectComponent:ReplayPlayOneShotEffectAtLocation",
     "248", "FieldType::VectorDouble"),
    ("/Script/ShooterGame.EffectManagerComponent:ReplayRecordOneShotEffect",
     "248", "FieldType::VectorDouble"),
    ("/Script/ShooterGame.EffectManagerComponent:ReplayRecordContinuousEffect",
     "248", "FieldType::VectorDouble"),
    ("/Script/ShooterGame.EffectManagerComponent:MulticastPlayContinuousEffect",
     "249", "FieldType::VectorDouble"),
    ("/Script/ShooterGame.EffectManagerComponent:MulticastPlayContinuousEffect",
     "Translation", "FieldType::VectorDouble"),
    ("/Script/ShooterGame.EffectManagerComponent:MulticastPlayContinuousEffect",
     "Scale3D", "FieldType::VectorDouble"),
    ("/Script/ShooterGame.AresEquippable:MulticastPlayContinuousEffectFromClient",
     "249", "FieldType::VectorDouble"),
    ("/Script/ShooterGame.AresEquippable:MulticastPlayContinuousEffectFromClient",
     "Translation", "FieldType::VectorDouble"),
    ("/Script/ShooterGame.AresEquippable:MulticastPlayContinuousEffectFromClient",
     "Scale3D", "FieldType::VectorDouble"),
    ("/Script/ShooterGame.EffectManagerComponent:MulticastPlayOneShotEffect",
     "249", "FieldType::VectorDouble"),
    ("/Script/ShooterGame.EffectManagerComponent:MulticastPlayOneShotEffect",
     "Translation", "FieldType::VectorDouble"),
    ("/Script/ShooterGame.EffectManagerComponent:MulticastPlayOneShotEffect",
     "Scale3D", "FieldType::VectorDouble"),
    ("/Script/ShooterGame.AresEquippable:MulticastPlayOneShotEffectFromClient",
     "249", "FieldType::VectorDouble"),
    ("/Script/ShooterGame.AresEquippable:MulticastPlayOneShotEffectFromClient",
     "Translation", "FieldType::VectorDouble"),
    ("/Script/ShooterGame.AresEquippable:MulticastPlayOneShotEffectFromClient",
     "Scale3D", "FieldType::VectorDouble"),
    # `EffectManagerComponent` on the two weapon effect RPCs: the EffectManager
    # of the pawn holding the weapon. Descriptor-silent; wire evidence only.
    #
    # One property: compatible_checksum 1051633025 is declared at parameter
    # handle 0 of exactly these two functions in all 1,018 manifests audited at
    # 259ed10 (the OneShot function exists in 993), always with this name. That
    # is the manifest's parameter handle -- the `handle` column of the exported
    # rows is the ClassNetCache function slot (1/2 Continuous, 2/3 OneShot).
    #
    # Measured 2026-09-28 over those 1,018 exports (fields + checkpoint_fields,
    # checksum == 1051633025): 3,112,054 rows (Continuous 2,906,169 under 24
    # weapon `_ClassNetCache` group paths, OneShot 205,885 under 17 -- DMR
    # arrives under two folder spellings), all main stream: RPC parameters
    # never reach checkpoints. Widths 16 bits
    # (3,103,449) and 24 bits (8,605): read as IntPacked every row is consumed
    # exactly, and every value resolves in the same export's net_guids.parquet,
    # to one path only: `EffectManager`. 0 zero, 0 odd (all dynamic GUIDs). The
    # resolved object's outer is a `*_PC_C` player character on 3,110,231 rows
    # and Yoru's decoy (Pawn_Stealth_4_Decoy_V2_C) on 1,823. Cross-check against
    # a separately typed field: the outer equals the weapon actor's latest
    # typed `Instigator` at or before the RPC's time_ms on 3,111,803 rows; all
    # 251 others match an earlier Instigator of that weapon, 250 of them with
    # the current Instigator 0 (just dropped). Matching by packet_id instead of
    # time_ms gives a lower rate (95.85% on OneShot); the figure is the time_ms
    # one.
    #
    # Both twins, as for 249/Translation/Scale3D above: typing one would leave
    # the other raw with decode errors at 0. The heal-block note above ("metadata
    # refs this project does not type") is about DamageableComponent's heal
    # Instigator/Causer and was itself superseded by the HealCauser scoped type
    # (scoped_types.rs, fc50bfe); the siblings in this very RPC --
    # EffectContainer, WaitOnReplicationActor, ClientControllerThatTriggered --
    # were already ObjectNetGuid through the checksum table.
    ("/Script/ShooterGame.AresEquippable:MulticastPlayContinuousEffectFromClient",
     "EffectManagerComponent", "FieldType::ObjectNetGuid"),
    ("/Script/ShooterGame.AresEquippable:MulticastPlayOneShotEffectFromClient",
     "EffectManagerComponent", "FieldType::ObjectNetGuid"),
    ("/Script/ShooterGame.AresGameStateBase:MulticastResetForRespawn",
     "249", "FieldType::VectorDouble"),
    ("/Script/ShooterGame.ForceModuleManagerComponent:NetMulticastApplyForceModule",
     "SourceLocation", "FieldType::VectorDouble"),
    ("/Game/Abilities/GrenadeExplodeIndicator.GrenadeExplodeIndicator_C:MulticastTriggerExplodeIndicator",
     "IndicatorLocation", "FieldType::VectorDouble"),
    ("/Game/GameModes/Bomb/BombDestination.BombDestination_C:MulticastActivateBombSiteEffects",
     "BombLocation", "FieldType::VectorDouble"),
    ("/Game/GameModes/Components/Comp_BombEvents.Comp_BombEvents_C:BombPlantedRPC",
     "PlantLocation", "FieldType::VectorDouble"),
    ("/Game/Equippables/Finishers/Rogue/Desturctible/FXC_Rogue_Finisher_Destructible.FXC_Rogue_Finisher_Destructible_C:Set skeletal Collision",
     "Collision Static Mesh Scale", "FieldType::VectorDouble"),
    # `StopMovementTime` is the other half of the pair whose `StartMovementTime`
    # is already Float: same RPC family, same 32-bit width on all 13,316 rows,
    # and the same shape when read as f32 -- a -1.0 sentinel on 5,371 of them
    # and 0.76..136.98 on the rest, against -1.0..1771.83 for the declared
    # sibling. One entry per checksum is enough; 244888268 carries it to
    # `ReplayStopContinuousEffectAtLocation` as well.
    ("/Script/ShooterGame.EffectManagerComponent:MulticastStopContinuousEffect",
     "StopMovementTime", "FieldType::Float"),
    # `HandleNumber` identifies a force module for the later Remove/Cleanup RPC
    # to name. Read as u32 the 3,741 rows hold 1..765 with every value in that
    # range present -- a dense sequential id, which no other reading of these
    # bits produces. Checksum 3336285386 shares it with `NetMulticastRemove`
    # `ForceModule`.
    ("/Script/ShooterGame.ForceModuleManagerComponent:NetMulticastApplyForceModule",
     "HandleNumber", "FieldType::Int32"),
    # The other five `NetMulticastApplyForceModule` parameters. Descriptor-silent;
    # measured 2026-09-28 over the 1,018 replays audited at 259ed10. The RPC
    # occurs in 1,015 of them (not in the single 12.10, 12.11 and 13.00 files),
    # 665,519 calls, every row in the main stream: checkpoint_fields carries no
    # row of this RPC although checkpoint_export_fields declares it. Each
    # checksum below is declared by this one parameter and no other property,
    # and one width holds on every build that has rows. Handles quoted in the
    # evidence are the manifest's PARAMETER handles; every exported row carries
    # the ClassNetCache function slot, 1 for Apply.
    #
    # `RespawnNumber` (3960441757): 32 bits, little-endian i32 0..38, none
    # negative. Cross-check against a typed field: it equals the same
    # character's latest typed `AresInventory.RespawnNumber` (paired through
    # this RPC's `Character` and the inventory's `Character`) on 665,363 of
    # 665,370 comparable rows; 4 of the 7 others come after the export's last
    # inventory row, 3 before its first. Int32 rather than UInt32 follows the
    # Int32 declarations of the same-named AresInventory and DamageParameters
    # siblings; 0..38 cannot settle the sign. NOT the component's own
    # `ForceModuleManagerComponent.RespawnNumber` property (checksum 3044239005,
    # declared in 554 manifests, never carrying a row), which is a different
    # group key this entry cannot reach -- do not merge them.
    #
    # `NetTimestamp` (259706372): 32 bits of finite f32, 0.0..252.703125, 8,285
    # exact zeros that are real 0x00000000 payloads; 8,622 of the 657,234
    # non-zero values are off the 1/128 s tick grid. Two separately typed
    # clocks agree with it: the character's AresInventory.NetTimestamp (residual
    # under 0.1 s on 645,350 of 648,553 comparable rows) and the
    # MulticastNotifyDamage_{Point,Base}.NetTimestamp on the same actor (under
    # 0.05 s on 5,500 of 5,548 in a 7-export sample). The epoch is per actor /
    # per life, NOT replay time: do not subtract it from time_ms.
    #
    # `ModuleType` (3263282135): 3 bits on every row, values {0, 2} (656,897 /
    # 8,622); read LSB-first over the payload width as decode_byte does. What
    # settles it is outside the field: every one of the 39 module class names
    # maps to one type on unambiguously paired rows (2 is exactly the six
    # displacement modules -- Clay knockbacks, Breach X knock-up, the two
    # repel-other-character modules), and the same property on
    # NetMulticastRemoveForceModule agrees with the paired Apply on 647,381 of
    # 647,381 rows. That is what the ZoomMultiplier "wire count alone" refusal
    # above lacked. Remove has no entry of its own and is typed by this donor
    # through checksum_table.rs; its value 1 (2,081,004 rows, never on Apply,
    # never paired) has no established meaning. Upstream's descriptor delta
    # names Remove.ModuleType EnumRemainingBits: if that is ever vendored, the
    # donors disagree, the checksum is dropped, and Remove goes raw again unless
    # it gets its own entry. Enum names are unknown; only the integer is typed.
    #
    # `Module` (739992589): 16 bits, IntPacked, every row odd (a static GUID)
    # and every one resolving in the same export's net_guids to a
    # `ForceModule_*` / `DeathForceModule_*` / `FM_*` class (39 short names, 43
    # class/package pairs; ForceModule_Tag_Heavy_C 425,735,
    # DeathForceModule_C 142,089, ...) -- a TSubclassOf reference no other type
    # could name.
    #
    # `Character` (1346692128): 16/24 bits, IntPacked, every row even (dynamic).
    # net_guids resolves 0 of them -- dynamic actors are not registered there --
    # but actors.parquet opens resolve all 665,519, to 43 character/pawn
    # classes, and the value equals the row's own actor_net_guid (read from the
    # channel header, independent of the payload) on every row. Redundant, and
    # therefore self-checking. Join it through actors.parquet or
    # actor_net_guid, not net_guids.
    ("/Script/ShooterGame.ForceModuleManagerComponent:NetMulticastApplyForceModule",
     "RespawnNumber", "FieldType::Int32"),
    ("/Script/ShooterGame.ForceModuleManagerComponent:NetMulticastApplyForceModule",
     "NetTimestamp", "FieldType::Float"),
    ("/Script/ShooterGame.ForceModuleManagerComponent:NetMulticastApplyForceModule",
     "ModuleType", "FieldType::EnumByte"),
    ("/Script/ShooterGame.ForceModuleManagerComponent:NetMulticastApplyForceModule",
     "Module", "FieldType::ObjectNetGuid"),
    ("/Script/ShooterGame.ForceModuleManagerComponent:NetMulticastApplyForceModule",
     "Character", "FieldType::ObjectNetGuid"),
    # Which named area of the map a player is standing in -- "A Site", "Mid",
    # "Heaven" and so on, the same callouts the game announces. The group only
    # became reachable when the `CalloutRegionTracker` leaf was remapped, and
    # the field is an ObjectNetGuid: unpacking the raw bits of all 1,957
    # non-zero rows and looking them up in net_guids resolves 1,957 of 1,957 to
    # a `CalloutRegion_*` path, 22 distinct regions. Nothing else in the export
    # names where a player is in map terms.
    ("/Script/ShooterGame.CalloutRegionTrackingComponent",
     "CurrentRegion", "FieldType::ObjectNetGuid"),
    # The per-cast ability log: who cast what, when, and where. vrfkit already
    # flattens the array into `AbilityCastsThisRound[i].<member>` rows and the
    # replay declares every member name -- but every value arrived raw, so a
    # survey that scans typed columns walks straight past it. That is how this
    # repo concluded twice that no exact cast count exists on the wire.
    #
    # The names carry Blueprint property GUIDs, which are stable: byte-identical
    # on 13.01 and 13.02.
    #
    # Each member checks out against something outside itself. `Player` is an
    # FString whose 352 values are all 36-char UUIDs, and all 352 match a
    # `manifest.players.subject`. `Round` covers exactly 0..17, the replay's 18
    # rounds. `CastLocation` reads as 3 x f64 inside the map bounds that
    # `movement.parquet` describes. `Slot` takes four values (3, 4, 5, 9) --
    # three abilities and an ultimate. `CastTime` is seconds within the round.
    ("/Game/Characters/_Core/Comp_AbilityStatisticsReplicator.Comp_AbilityStatisticsReplicator_C",
     "Player_11_0963330440D68BDF1A8E34B035420342", "FieldType::FString"),
    ("/Game/Characters/_Core/Comp_AbilityStatisticsReplicator.Comp_AbilityStatisticsReplicator_C",
     "Slot_12_22D571914FAFD5F0EBD400B7E2F28B36", "FieldType::Byte"),
    ("/Game/Characters/_Core/Comp_AbilityStatisticsReplicator.Comp_AbilityStatisticsReplicator_C",
     "Round_22_905E6CC0448D2C6270A94C9690101E49", "FieldType::Int32"),
    ("/Game/Characters/_Core/Comp_AbilityStatisticsReplicator.Comp_AbilityStatisticsReplicator_C",
     "RoundPhase_25_84478C0047988409FEEC9E95C15DFB02", "FieldType::Byte"),
    ("/Game/Characters/_Core/Comp_AbilityStatisticsReplicator.Comp_AbilityStatisticsReplicator_C",
     "CastTime_4_5AE288704801A9B74D6D159DFC2BD147", "FieldType::Float"),
    ("/Game/Characters/_Core/Comp_AbilityStatisticsReplicator.Comp_AbilityStatisticsReplicator_C",
     "CastLocation_21_61F4B6BC47A10FE8CD34D29141FC9B88", "FieldType::VectorDouble"),
    ("/Game/Characters/_Core/Comp_AbilityStatisticsReplicator.Comp_AbilityStatisticsReplicator_C",
     "DestroyedCount_36_5936AB33418F8A2AB3A52DBF4492CF7F", "FieldType::Int32"),
    # Inside each cast, `Effects[]` records what the cast did and to whom. This
    # is the authoritative debuff log -- `EnemiesSuppressed`, `EnemiesSlowed`,
    # `EnemiesVulnerabled` and 28 more -- where the cosmetic-effect channel is
    # only a proxy for it.
    #
    # NOT the way `LifeChangeEvents`' members get their types.
    #
    # `table.rs` carries `ChangedComponent`, `LifeResult`, `DeltaLife` and
    # `bAliveAfterChange` under the `MulticastNotifyDamage_*` groups, and those
    # entries are unreachable: the names are members inside the array payload
    # and never arrive as top-level RPC parameters, so the name lookup never
    # asks for them. Changing `LifeResult` there from `Raw` to `Float` compiles,
    # passes every test, and moves no row.
    #
    # The members are typed in `crates/vrfkit/src/sink/rpc.rs`, by
    # `life_change_member_type`, on the path the array walker gives each leaf.
    # That is where to look, and where to change it.

    # `LocalizedStat`: an FText, and now typed as one.
    #
    # It was `FString` once and returned null on 3,011 of 3,011 rows, which is
    # why it was removed. The wire is an `FText` carrying a string-table entry,
    # and the key it carries is the statistic's name -- `EnemiesBlinded`,
    # `DamageDealt`, 29 distinct values across 4,341 rows, each mapping 1:1 to
    # a `Statistic` enum value.
    #
    # The note that removed it said this was not worth doing because
    # `Statistic` already carried the same fact. That was wrong in the way that
    # matters: `Statistic` decodes to a bare integer and this repository ships
    # no table mapping those integers to names -- they exist in a comment
    # below and in README prose, and nowhere a consumer can reach. This column
    # is the only machine-readable source of them.
    #
    # Evidence: 4,341 of 4,341 rows decode with zero residual bits, on the
    # layout in `decode_ftext`. The keys agree with `Statistic` without a
    # single collision.
    ("/Game/Characters/_Core/Comp_AbilityStatisticsReplicator"
     ".Comp_AbilityStatisticsReplicator_C",
     "LocalizedStat_14_C3A26F5E46CDAD94571AE6B0EDEA058B", "FieldType::FText"),

    # `AffectedPlayer` is the check: all 224 values resolve to a
    # `manifest.players.actor_net_guid`, across exactly 10 distinct players.
    # `Statistic` is a small enum whose observed values line up with the named
    # statistics (0 EnemiesBlinded, 7 EnemiesBlocked, 8 EnemiesNearsighted, ...),
    # and `Time` reads as seconds within the round like its sibling `CastTime`.
    ("/Game/Characters/_Core/Comp_AbilityStatisticsReplicator.Comp_AbilityStatisticsReplicator_C",
     "Statistic_2_0868666A4F6501815AB301BB615B2B5C", "FieldType::EnumByte"),
    ("/Game/Characters/_Core/Comp_AbilityStatisticsReplicator.Comp_AbilityStatisticsReplicator_C",
     "Value_7_E46F38AE4D245059AF7BB09E301C3C65", "FieldType::Float"),
    ("/Game/Characters/_Core/Comp_AbilityStatisticsReplicator.Comp_AbilityStatisticsReplicator_C",
     "Time_8_6CD58DD7441CBD0407DD1F89FDD05167", "FieldType::Float"),
    ("/Game/Characters/_Core/Comp_AbilityStatisticsReplicator.Comp_AbilityStatisticsReplicator_C",
     "AffectedPlayer_2_BAF988E34EAAE6B7A1D4758455186559", "FieldType::ObjectNetGuid"),
    ("/Game/Characters/_Core/Comp_AbilityStatisticsReplicator.Comp_AbilityStatisticsReplicator_C",
     "Value_5_203891704B7EF064EDB5528BFECC4807", "FieldType::Float"),
    # HawkFlash PostControlVelocity is exactly three finite LE f64 values in
    # every measured 192-bit window. The exact group prevents this from
    # generalizing to other velocity-named fields or gameplay behavior.
    ("/Game/Characters/Guide/S0/Ability_E/Projectile_Guide_E_HawkFlash."
     "Projectile_Guide_E_HawkFlash_C",
     "PostControlVelocity", "FieldType::VectorDouble"),
]
EXPECTED += [(g, f, t) for g, f, t in ADDITIONS]

#: Handle -> field_name additions for groups the replay never names. Each pairs
#: with an ADDITION of the same (group_path, field_name) so the overlay can type
#: the newly-named handle. Keyed on (group_path, handle) and inserted at the
#: sorted position the binary search over OVERLAY_HANDLE_TABLE requires.
HANDLE_ADDITIONS = [

]

GROUP_RE = re.compile(r'group_path: "([^"]+)"')
FIELD_RE = re.compile(r'field_name: "([^"]+)"')
TYPE_MARKER = "field_type:"


def normalize_type(field_type: str) -> str:
    """One spelling for a field type, whichever layout it was written in.

    `verify` compares types with `==`, so the two spellings rustfmt can produce
    have to collapse to one first. A struct-like type written on a single line
    is `RepMovement { rotation: X }`; broken across lines rustfmt adds a
    trailing comma before the brace, so the same type parses as
    `RepMovement { rotation: X, }`. Without this the exact comparison would
    report every braced type as wrong on a formatted table and right on a
    freshly generated one -- a check that depends on formatting is not a check.
    """
    collapsed = " ".join(field_type.rstrip().rstrip(",").split())
    return re.sub(r",\s*\}", " }", collapsed)


def _field_type_of(block: str) -> str | None:
    """The `field_type` value of one `OverlayEntry { ... }` block.

    Brace-counted rather than pattern-matched. Two things defeat a regex here,
    and both are live:

    * several field types contain their own braces
      (`VectorNetQuantize { scale: 100 }`), so a non-greedy match truncates;
    * the table exists in TWO layouts -- one entry per line as
      extract_descriptors.py emits it, and the rustfmt'd multi-line form that
      gets committed. A pattern anchored on a newline works on one and
      silently matches nothing on the other.

    That second case is not hypothetical: the first version of this function
    required a newline, so it reported all 25 corrections missing on a
    freshly generated table -- exactly when this script is supposed to run.

    The block has already had its opening `OverlayEntry {` consumed, so the
    first unmatched `}` is the one that closes the entry.
    """
    start = block.find(TYPE_MARKER)
    if start == -1:
        return None
    start += len(TYPE_MARKER)
    depth = 0
    for i, ch in enumerate(block[start:], start):
        if ch == "{":
            depth += 1
        elif ch == "}":
            if depth == 0:
                return normalize_type(block[start:i])
            depth -= 1
    return None


def parse_entries(content: str):
    """Yield (group_path, field_name, field_type) for every OverlayEntry.

    Split per `OverlayEntry {` block first so each lookup is scoped to one
    entry and cannot bleed into its neighbour.
    """
    for block in content.split("    OverlayEntry {")[1:]:
        group = GROUP_RE.search(block)
        field = FIELD_RE.search(block)
        ftype = _field_type_of(block)
        if group and field and ftype:
            yield group.group(1), field.group(1), ftype


def apply_additions(content: str) -> tuple[str, int]:
    """Insert every entry in `ADDITIONS` that is not already present.

    Insertion, not replacement, so it cannot reuse the `.replace()` shape the
    corrections above use -- there is nothing to replace.

    The slice is sorted by `(group_path, field_name)` and
    `tests::overlay::table_is_sorted` enforces it, so a new entry goes at its
    sorted position rather than at the end. We find the first existing entry
    that sorts AFTER ours and splice in before it; if no such entry exists we
    fail loudly rather than append, because appending past the final block
    would write into the `];` that closes the slice.
    """
    added = 0
    for group, field, ftype in ADDITIONS:
        blocks = content.split("    OverlayEntry {")
        keys = []
        for block in blocks[1:]:
            g, f = GROUP_RE.search(block), FIELD_RE.search(block)
            keys.append((g.group(1), f.group(1)) if g and f else ("", ""))
        if (group, field) in keys:
            continue

        target = next((i for i, k in enumerate(keys) if k > (group, field)), None)
        entry = (
            "    OverlayEntry {\n"
            f'        group_path: "{group}",\n'
            f'        field_name: "{field}",\n'
            f"        field_type: {ftype},\n"
            "    },\n"
        )
        if target is None:
            # The new entry is the new tail of OVERLAY_TABLE. Splice it in
            # before the `];` that closes that slice. The split puts the final
            # block (blocks[-1]) as the last OverlayEntry's text followed by
            # `];` and the OVERLAY_HANDLE_TABLE that comes after, so the first
            # `\n];` in that block is the OVERLAY_TABLE close. Refusing here
            # used to be the safe choice, but a sorted table whose last group
            # is new (e.g. ZoomMultiplierComponent) cannot gain entries without
            # it, so the append is handled rather than rejected.
            last = blocks[-1]
            close = last.find("\n];")
            if close == -1:
                raise SystemExit(
                    f"{TABLE_RS}: {group}/{field} would append but the "
                    f"OVERLAY_TABLE closing '];' could not be located."
                )
            head = (
                "    OverlayEntry {".join(blocks[:-1])
                + "    OverlayEntry {"
                + last[:close]
            )
            content = head + entry + last[close:]
        else:
            # blocks[target + 1] is the block for `keys[target]`; put the new
            # entry in front of the marker that introduces it.
            head = "    OverlayEntry {".join(blocks[: target + 1])
            tail = (
                "    OverlayEntry {"
                + "    OverlayEntry {".join(blocks[target + 1:])
            )
            content = head + entry + tail
        added += 1
    return content, added


def retype_exact(content: str, group: str, field: str, old: str, new: str,
                 expected: int) -> tuple[str, int]:
    """Rewrite `old` -> `new` on the entries keyed EXACTLY `(group, field)`.

    The older passes below match a substring of the group and the field line
    anywhere in a split block. That is only safe by luck: splitting on
    `    OverlayEntry {` leaves the whole OVERLAY_HANDLE_TABLE inside the LAST
    block, and that table repeats group paths and field names -- including
    `ReplayPlayContinuousEffectAtLocation` / `AllianceFilter` (handle 28) and
    `MulticastNotifyDamage_Point` / `DeathMontageEffectOverride` (handle 43).
    A substring pass on either would reach into the tail block and hold only
    because the last OverlayEntry happens not to carry the old type.

    Here the key is each block's OWN entry: the first `group_path` and
    `field_name` in the block, compared with `==`, and its own `field_type`
    compared in full. The type is then replaced once, and its first
    occurrence in the block is the entry's own `field_type`.

    `expected` is how many entries a freshly generated table must change. On an
    already corrected table the answer is 0; any other count means the key
    matched something it was not written for, and that is a hard failure
    rather than a quiet extra rewrite.
    """
    blocks = content.split("    OverlayEntry {")
    changed = 0
    for i, block in enumerate(blocks[1:], 1):
        g, f = GROUP_RE.search(block), FIELD_RE.search(block)
        if not (g and f and g.group(1) == group and f.group(1) == field):
            continue
        if _field_type_of(block) != normalize_type(old):
            continue
        blocks[i] = block.replace(old, new, 1)
        changed += 1
    if changed not in (0, expected):
        raise SystemExit(
            f"{TABLE_RS}: {group}/{field} {old} -> {new} changed {changed} "
            f"entries, expected {expected} (or 0 on a corrected table)."
        )
    return "    OverlayEntry {".join(blocks), changed


#: The weapon half of the "215"/"216" correction, which EXPECTED cannot list.
#:
#: The pass discovers its targets from the table itself -- every group under
#: `/Game/Equippables/` that carries one of these fields, 18 of them today --
#: so a hardcoded list would be a second, drifting copy of that discovery.
#: EXPECTED's "215"/"216" rows name the five HARDCODED non-weapon groups and
#: nothing else, so before this the weapon pass had no check at all: it could
#: match nothing, print "Applied 0" and exit 0, which is indistinguishable from
#: having had nothing to do. The expectation is therefore derived from the same
#: table the pass ran over.
WEAPON_GROUP_MARKER = "/Game/Equippables/"
ACTOR_BOOKKEEPING_FIELDS = ("215", "216")
ACTOR_BOOKKEEPING_TYPE = "FieldType::EnumRemainingBits"


def weapon_expectations(content: str) -> list[tuple[str, str, str]]:
    """Every `/Game/Equippables/` "215"/"216" entry, with the type it has."""
    return sorted(
        (g, f, t)
        for g, f, t in parse_entries(content)
        if WEAPON_GROUP_MARKER in g and f in ACTOR_BOOKKEEPING_FIELDS
    )


def expectation_count(content: str) -> int:
    """How many corrections `verify` actually checks against `content`."""
    return len(EXPECTED) + len(weapon_expectations(content))


def verify(content: str) -> list[str]:
    """Return one line per correction that is NOT present in `content`.

    EVERY hit has to carry the required type, not merely one of them. The group
    is matched by substring, so `"SmokeScreen"` reaches both `SmokeScreen` and
    `SmokeScreenManager`; under an `any()` a wrong-typed entry verified clean
    whenever a sibling group happened to be right -- the same shape of hole as
    the substring type test this function used to do.
    """
    entries = list(parse_entries(content))
    problems = []
    for group_part, field, required in EXPECTED:
        hits = [ft for g, f, ft in entries if group_part in g and f == field]
        if not hits:
            problems.append(f"{field} in *{group_part}*: entry not found at all")
        elif any(ft != required for ft in hits):
            problems.append(
                f"{field} in *{group_part}*: expected {required}, found {hits}"
            )

    weapons = weapon_expectations(content)
    if not weapons:
        problems.append(
            f"no {WEAPON_GROUP_MARKER} entry named "
            f"{' or '.join(ACTOR_BOOKKEEPING_FIELDS)} exists at all: the weapon "
            f"pass had nothing to convert, which is exactly what its discovery "
            f"going dead looks like"
        )
    for group, field, ftype in weapons:
        if ftype != ACTOR_BOOKKEEPING_TYPE:
            problems.append(
                f"{field} in {group}: expected {ACTOR_BOOKKEEPING_TYPE}, "
                f"found {ftype}"
            )
    return problems


#: The two generated header lines this script has to keep true, in file order.
#: `rewrite_header` recounts both from the parsed entries and `--check` fails
#: when either disagrees.
HEADER_RES = (
    re.compile(r"^// \d+ entries from \d+ groups\.$", re.MULTILINE),
    re.compile(r"^// Raw/Custom: \d+, Skip: \d+, Typed: \d+\.$", re.MULTILINE),
)

#: The slice is declared with an explicit length, so an addition that does not
#: update it does not compile. That is the good failure -- it is caught by
#: `cargo build` rather than by anyone noticing -- but the script should not
#: hand over a file that cannot build.
TABLE_LEN_RE = re.compile(r"(pub static OVERLAY_TABLE: \[OverlayEntry; )(\d+)(\])")


def resync_table_len(content: str) -> str:
    """Rewrite the declared `OVERLAY_TABLE` length to the entries present."""
    n = sum(1 for _ in parse_entries(content))
    new_content, hits = TABLE_LEN_RE.subn(
        lambda m: f"{m.group(1)}{n}{m.group(3)}", content, count=1
    )
    if hits != 1:
        raise SystemExit(
            f"{TABLE_RS}: expected exactly one OVERLAY_TABLE length declaration, "
            f"found {hits}."
        )
    return new_content


HANDLE_GROUP_RE = re.compile(r'group_path: "([^"]+)"')
HANDLE_NUM_RE = re.compile(r"handle: (\d+)")


def parse_handle_entries(content: str):
    """Yield (group_path, handle, field_name) for every OverlayHandleEntry."""
    for block in content.split("    OverlayHandleEntry {")[1:]:
        g = HANDLE_GROUP_RE.search(block)
        h = HANDLE_NUM_RE.search(block)
        f = FIELD_RE.search(block)
        if g and h and f:
            yield g.group(1), int(h.group(1)), f.group(1)


def apply_handle_additions(content: str) -> tuple[str, int]:
    """Insert every OverlayHandleEntry in HANDLE_ADDITIONS not already present.

    Mirrors `apply_additions` but keys on `(group_path, handle)` and writes into
    the OVERLAY_HANDLE_TABLE slice. Some groups (e.g. `MagazineAmmo`) are never
    given field names by the replay or the C# descriptors, so the handle table
    is the only place that can name them -- and without a name the overlay
    cannot type the handle.
    """
    added = 0
    for group, handle, field in HANDLE_ADDITIONS:
        blocks = content.split("    OverlayHandleEntry {")
        keys = []
        for block in blocks[1:]:
            g, h = HANDLE_GROUP_RE.search(block), HANDLE_NUM_RE.search(block)
            keys.append((g.group(1), int(h.group(1))) if g and h else ("", -1))
        if (group, handle) in keys:
            continue

        target = next((i for i, k in enumerate(keys) if k > (group, handle)), None)
        entry = (
            "    OverlayHandleEntry {\n"
            f'        group_path: "{group}",\n'
            f"        handle: {handle},\n"
            f'        field_name: "{field}",\n'
            "    },\n"
        )
        if target is None:
            # OVERLAY_HANDLE_TABLE is the last slice in the file; splice the new
            # entry in before its closing `];`.
            last = blocks[-1]
            close = last.rfind("\n];")
            if close == -1:
                raise SystemExit(
                    f"{TABLE_RS}: {group}/handle {handle} would append but the "
                    f"OVERLAY_HANDLE_TABLE closing '];' could not be located."
                )
            head = (
                "    OverlayHandleEntry {".join(blocks[:-1])
                + "    OverlayHandleEntry {"
                + last[:close]
            )
            content = head + entry + last[close:]
        else:
            head = "    OverlayHandleEntry {".join(blocks[: target + 1])
            tail = (
                "    OverlayHandleEntry {"
                + "    OverlayHandleEntry {".join(blocks[target + 1:])
            )
            content = head + entry + tail
        added += 1
    return content, added


HANDLE_TABLE_LEN_RE = re.compile(
    r"(pub static OVERLAY_HANDLE_TABLE: \[OverlayHandleEntry; )(\d+)(\])"
)


def resync_handle_table_len(content: str) -> str:
    """Rewrite the declared `OVERLAY_HANDLE_TABLE` length to the entries present."""
    n = sum(1 for _ in parse_handle_entries(content))
    new_content, hits = HANDLE_TABLE_LEN_RE.subn(
        lambda m: f"{m.group(1)}{n}{m.group(3)}", content, count=1
    )
    if hits != 1:
        raise SystemExit(
            f"{TABLE_RS}: expected exactly one OVERLAY_HANDLE_TABLE length "
            f"declaration, found {hits}."
        )
    return new_content


def rewrite_header(content: str) -> tuple[str, tuple[str, ...]]:
    """Recount the table and rewrite both generated header lines.

    extract_descriptors.py writes them from the descriptors it read, and then
    this script changes some of those types and ADDS entries the descriptors
    never declared -- so between the two the header is a statement about a table
    that no longer exists.

    The bucket line said "Raw/Custom: 164 ... Typed: 864" while the file held
    157 and 871: the seven corrections that turn a Raw into a real type. The
    shape line above it said "1185 entries from 171 groups" while the file held
    1188 from 172 -- and, worse, while the bucket line one row below it summed
    to 1188. Three consecutive lines, two of them recounted here and the third
    left to rot, contradicting each other in the same paragraph.

    Nothing reads the header, which is exactly why it went unnoticed and why it
    is worth fixing -- a comment on a generated file that quietly disagrees with
    the file is how a reader learns not to trust the comments.

    Counted from the parsed entries rather than by substring, so a type whose
    name contains another's (`FieldType::RawPayload` would contain `Raw`)
    cannot miscount.
    """
    buckets = Counter()
    groups = set()
    for group, _field, ftype in parse_entries(content):
        groups.add(group)
        if ftype == "FieldType::Raw":
            buckets["raw"] += 1
        elif ftype == "FieldType::Skip":
            buckets["skip"] += 1
        else:
            buckets["typed"] += 1

    lines = (
        f"// {sum(buckets.values())} entries from {len(groups)} groups.",
        f"// Raw/Custom: {buckets['raw']}, Skip: {buckets['skip']}, "
        f"Typed: {buckets['typed']}.",
    )
    for pattern, line in zip(HEADER_RES, lines):
        content, n = pattern.subn(lambda _m, _l=line: _l, content, count=1)
        if n != 1:
            raise SystemExit(
                f"{TABLE_RS}: expected exactly one generated header line "
                f"matching {pattern.pattern!r}, found {n}. Regenerate with "
                f"extract_descriptors.py first."
            )
    return content, lines


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="verify without writing")
    return parser.parse_args(argv)


def main():
    check_only = parse_args().check
    content = TABLE_RS.read_text(encoding="utf-8")
    # The file exactly as committed. Every pass below rewrites `content`, so by
    # the end it is the CORRECTED COPY -- and verifying that copy is what made
    # `--check` unable to tell an already-corrected table from a correctable one.
    on_disk = content
    count = 0

    # Fix: Float -> Double for time-related fields (verified: 64-bit on wire)
    float_to_double = [
        ('TimedBomb.TimedBomb_C", field_name: "TimeRemainingToExplode", field_type: FieldType::Float',
         'TimedBomb.TimedBomb_C", field_name: "TimeRemainingToExplode", field_type: FieldType::Double'),
        ('TimedBomb.TimedBomb_C", field_name: "DefuseProgress", field_type: FieldType::Float',
         'TimedBomb.TimedBomb_C", field_name: "DefuseProgress", field_type: FieldType::Double'),
        ('Comp_Ability_CooldownComponent_C", field_name: "StartTimeStamp", field_type: FieldType::Float',
         'Comp_Ability_CooldownComponent_C", field_name: "StartTimeStamp", field_type: FieldType::Double'),
        ('Comp_Ability_CooldownComponent_C", field_name: "CooldownSeconds", field_type: FieldType::Float',
         'Comp_Ability_CooldownComponent_C", field_name: "CooldownSeconds", field_type: FieldType::Double'),
    ]
    for old, new in float_to_double:
        if old in content:
            content = content.replace(old, new)
            count += 1

    # Fix: the descriptor marks the replay transform timestamp as Skip even
    # though every observed payload is a 32-bit Float. Work per entry block so
    # the correction remains effective after rustfmt splits generated entries.
    blocks = content.split("    OverlayEntry {")
    for i, block in enumerate(blocks):
        if i == 0:
            continue
        if 'field_name: "ReplayLastTransformUpdateTimeStamp"' not in block:
            continue
        if "FieldType::Skip" in block:
            blocks[i] = block.replace("FieldType::Skip", "FieldType::Float")
            count += 1
    content = "    OverlayEntry {".join(blocks)

    # Fix: every group's "215"/"216" is EnumRemainingBits, weapons included.
    #
    # These are not handles. They are field *names* -- the decimal spelling of
    # a hardcoded Unreal FName index the replay never resolves to text (see
    # `read_fname` in vrf-schema) -- and they appear on 370 groups.
    #
    # The block below used to say "Weapons use Raw (correct)" and nothing had
    # checked. The wire says otherwise, and the checksum is what settles it:
    # `"215"` carries 1710918439 and `"216"` carries 4109980037 on every group
    # that sends them, decoding or not, at a uniform 3 bits. Unreal hashes the
    # property's type into that number, so one checksum is one property; `Raw`
    # on a weapon was never a second type, only an unverified guess. Where it
    # does decode the value is 3 and 1 without exception.
    #
    # It cost 48,010 rows over 20 replays, and it was invisible: a name hit
    # wins in `resolve_in_group` before the checksum fallback is consulted, so
    # `Raw` blocked the very mechanism that would have caught it, and the rows
    # counted as "raw/skip" rather than as an error.
    #
    # Two passes because the two sets arrive spelled differently -- the
    # non-weapon groups are declared Int32 by the C# descriptor, the weapon
    # groups Raw. A single Int32-matching pass silently does nothing to the
    # weapons: the string it looks for is not there, so no substitution is made
    # and no counter moves.
    weapon_groups_215_216 = [
        line.split('group_path: "')[1].split('"')[0]
        for line in content.splitlines()
        if 'group_path: "/Game/Equippables/' in line
    ]
    for g in sorted(set(weapon_groups_215_216)):
        for field in ["215", "216"]:
            old = f'{g}", field_name: "{field}", field_type: FieldType::Raw'
            new = f'{g}", field_name: "{field}", field_type: FieldType::EnumRemainingBits'
            if old in content:
                content = content.replace(old, new)
                count += 1

    groups_215_216 = [
        "TimedBomb.TimedBomb_C",
        "EquippablePickupProjectile.EquippablePickupProjectile_C",
        "EquippableGroundPickup.EquippableGroundPickup_C",
        "OwnerExclusivePlayerInfo",
        "Projectile_Phoenix_Q_FlameWall_ThroughWall.Projectile_Phoenix_Q_FlameWall_ThroughWall_C",
    ]
    for g in groups_215_216:
        for field in ["215", "216"]:
            old = f'{g}", field_name: "{field}", field_type: FieldType::Int32'
            new = f'{g}", field_name: "{field}", field_type: FieldType::EnumRemainingBits'
            if old in content:
                content = content.replace(old, new)
                count += 1

    # Fix: rotation quantization for the Astra smoke-screen projectiles.
    #
    # `ProjectileSmokeScreenDescriptor.cs` calls `.ReplicatedMovement()` with no
    # argument, which defaults to ShortComponents (16 bits per rotation axis).
    # Every other projectile descriptor in the same codebase passes
    # ByteComponents explicitly -- FlameWall, MageWall, NeonTunnel and
    # EquippablePickup -- so the bare call reads as an oversight rather than a
    # deliberate difference.
    #
    # The wire agrees with the other projectiles: on release-13.01 these payloads
    # arrive at 113-124 bits and a ShortComponents read runs off the end (137 EOF
    # failures on one replay, every one of them from this single group).
    #
    # Entries span several lines, so rewrite per `OverlayEntry { .. }` block
    # rather than per line: a line-wise match cannot see the group path and the
    # field type at the same time.
    blocks = content.split("    OverlayEntry {")
    for i, block in enumerate(blocks):
        if i == 0:
            continue
        if "SmokeScreen" not in block or 'field_name: "ReplicatedMovement"' not in block:
            continue
        if "RotatorQuantization::ShortComponents" in block:
            blocks[i] = block.replace(
                "RotatorQuantization::ShortComponents",
                "RotatorQuantization::ByteComponents",
            )
            count += 1
    content = "    OverlayEntry {".join(blocks)

    # REMOVED: FName -> Raw for DamagedBone in MulticastNotifyDamage_Point.
    #
    # This pass existed because 177 of 581 payloads arrive at 9 bits, which
    # the FName decoder could not read. Its comment claimed the value "is
    # always bone index 0" -- generalised from those 177 and wrong: the
    # reference has 22 distinct bone names (Head 69, Spine4 51, L_Shoulder
    # 46, ...) and forcing Raw made us ship mojibake for all 581.
    #
    # The real cause was decode_fname ignoring the isHardcoded bit. Fixed in
    # vrf-decode, so the field decodes as the FName the C# descriptor
    # declares and needs no correction here.

    # Fix: EnumByte -> Raw for AresEquippableDataTracker.OriginalBuyerTeam.
    # C# declares this as EnumByte (single byte), but on wire it arrives as
    # 97-105 bits consistently (248 occurrences in 02d4d478). This is likely
    # a serialized FastArray entry or struct, not a bare enum. Mark Raw.
    blocks = content.split("    OverlayEntry {")
    for i, block in enumerate(blocks):
        if i == 0:
            continue
        if "AresEquippableDataTracker" not in block:
            continue
        if 'field_name: "OriginalBuyerTeam"' not in block:
            continue
        if "FieldType::EnumByte" in block:
            blocks[i] = block.replace("FieldType::EnumByte", "FieldType::Raw")
            count += 1
    content = "    OverlayEntry {".join(blocks)

    # Fix: Raw -> ObjectNetGuid for TransitionContext. The pinned C#
    # descriptor declares RawPayload("UTransitionContext"), so it does not
    # establish the wire reader. Corpus evidence establishes exact IntPacked
    # consumption; resolved non-null IDs name transition-context object groups
    # and null remains zero. This does not claim every ID resolves, nor does
    # this group/name overlay impose the measured build, handle, or checksum
    # gates that the scalar overlay cannot represent.
    blocks = content.split("    OverlayEntry {")
    for i, block in enumerate(blocks):
        if i == 0:
            continue
        if ('group_path: "/Script/ShooterGame.EquippableStateMachineComponent"'
                not in block):
            continue
        if 'field_name: "TransitionContext"' not in block:
            continue
        if "FieldType::Raw" in block:
            blocks[i] = block.replace("FieldType::Raw", "FieldType::ObjectNetGuid")
            count += 1
    content = "    OverlayEntry {".join(blocks)

    # Fix: Raw -> ObjectNetGuid for EquippableUsed on both damage RPCs.
    #
    # Not a wire/declaration mismatch like the others above -- the declaration
    # is simply invisible to the extractor. DamageParameters.cs:51 attaches a
    # custom decoder:
    #
    #   AddPropertyHandle(7, x => x.EquippableUsed, ...)
    #       .Decode(ValorantPayloadDecoders.Equippable)
    #
    # and that decoder (ValorantPayloadDecoders.cs:158) is exactly
    #
    #   var netGuid = archive.ReadIntPacked();
    #
    # which is what FieldType::ObjectNetGuid already implements. Because
    # extract_descriptors.py cannot see through .Decode(...), the field lands
    # here as Raw, and every consumer has to guess the encoding.
    #
    # Verified on 02d4d478 across all 632 occurrences: read as IntPacked the
    # values are 116 distinct and 100% even -- the engine requires dynamic
    # NetGUIDs to be even (IsDynamic => (Value & 1) == 0) -- and 114 of 115
    # resolve to a weapon class path in actors.parquet. The bits are 8, 16 or
    # 24 wide depending on the value, so any fixed-width read is wrong by
    # construction.
    blocks = content.split("    OverlayEntry {")
    for i, block in enumerate(blocks):
        if i == 0:
            continue
        if "DamageableComponent:MulticastNotifyDamage_" not in block:
            continue
        if 'field_name: "EquippableUsed"' not in block:
            continue
        if "FieldType::Raw" in block:
            blocks[i] = block.replace("FieldType::Raw", "FieldType::ObjectNetGuid")
            count += 1
    content = "    OverlayEntry {".join(blocks)

    # Fix: Raw -> quantized vectors for the damage geometry fields.
    #
    # Same invisibility problem as EquippableUsed above: these are attached
    # with .Decode(ValorantPayloadDecoders.VectorNetQuantize*(...)), so the
    # extractor sees a custom decoder and emits Raw, even though vrf-decode
    # already implements the exact quantization.
    #
    # Scales come from the C# call sites, not from guesswork:
    #   DamageParameters.cs:50                    VectorNetQuantize100
    #   MulticastNotifyDamagePointParameters.cs:40 VectorNetQuantizeNormal
    #   MulticastNotifyDamagePointParameters.cs:42 VectorNetQuantize
    #   MulticastNotifyDamagePointParameters.cs:44 VectorNetQuantizeNormal
    #   MulticastNotifyDamagePointParameters.cs:46 VectorNetQuantize
    #
    # Confirmed by the reference bundle's own output: DamageImpactLocation is
    # integral (scale 1), DamageOrigin carries two decimals (scale 100), and
    # DamageDirection / DamageImpactNormal are unit vectors.
    damage_vectors = {
        "DamageOrigin": "FieldType::VectorNetQuantize { scale: 100 }",
        "DamageImpactLocation": "FieldType::VectorNetQuantize { scale: 1 }",
        "DamageImpactBoneRelativeLocation": "FieldType::VectorNetQuantize { scale: 1 }",
        "DamageDirection": "FieldType::VectorNetQuantizeNormal",
        "DamageImpactNormal": "FieldType::VectorNetQuantizeNormal",
    }
    blocks = content.split("    OverlayEntry {")
    for i, block in enumerate(blocks):
        if i == 0:
            continue
        if "DamageableComponent:MulticastNotifyDamage_" not in block:
            continue
        for field, new_type in damage_vectors.items():
            if f'field_name: "{field}"' not in block:
                continue
            if "FieldType::Raw" in block:
                blocks[i] = block.replace("FieldType::Raw", new_type)
                count += 1
            break
    content = "    OverlayEntry {".join(blocks)

    # Fix: EnumRemainingBits -> EnumByte for AllianceFilter on
    # `ReplayEffectComponent:ReplayPlayContinuousEffectAtLocation`.
    #
    # A table-consistency correction, not a wire/declaration mismatch: the wire
    # agrees with both declarations. `AllianceFilter` (EAresAlliance) is ONE
    # property, compatible_checksum 2270825073, and three RPCs declare it --
    # `EffectManagerComponent:MulticastPlayContinuousEffect` and
    # `:MulticastPlayOneShotEffect` as `byte AllianceFilter` (EnumByte), and
    # ReplayPlayContinuousEffectAtLocationParameters.cs:43 as
    # `.EnumRemainingBits()`. Two donor types for one checksum is exactly what
    # extract_checksum_types.py drops, so the five RPCs that carry the same
    # parameter with no declaration of their own never got a type:
    # `AresEquippable:MulticastPlay{Continuous,OneShot}EffectFromClient` under
    # every weapon's `_ClassNetCache`, `ReplayPlayOneShotEffectAtLocation`, and
    # both `EffectManagerComponent:ReplayRecord*Effect`.
    #
    # Measured 2026-09-28 over all 1,018 replays audited at 259ed10 (24 builds,
    # 11.06-13.06), fields.parquet and checkpoint_fields.parquet, rows selected
    # by compatible_checksum == 2270825073: 16,030,813 rows, every one exactly
    # 3 bits wide on every build; 0 in checkpoints (RPC parameters never reach
    # them). The manifests declare the checksum under those eight groups only,
    # always named AllianceFilter. 11,470,565 rows were typed through the
    # donors and 4,560,248 were raw. Read LSB-first the donors hold {1, 3} and
    # every raw row holds 3 (AllianceAny), all inside EAresAlliance 0..5; an
    # independent Python decode matched the exported value on 11,470,565 of
    # 11,470,565 typed rows.
    #
    # EnumByte rather than EnumRemainingBits on the other two, because it is
    # the stricter reader: decode_byte refuses 0-bit and >8-bit payloads, while
    # EnumRemainingBits answers 0 for no bits and reads up to 32. At 1..8 bits
    # both return the same integer, so the 3,094,607 existing rows of this RPC
    # keep their values exactly -- and 0 of them are 0 bits wide, the one width
    # where `apply_overlay_inner`'s zero-bit special case used to answer here.
    #
    # The vendored descriptor stays verbatim. `checksum_table.rs` only learns
    # 2270825073 -> EnumByte when extract_checksum_types.py runs AFTER this, and
    # `alliance_filter_donors_agree_so_the_checksum_types_the_receivers` in
    # crates/vrf-decode/src/tests/overlay.rs fails until both have happened.
    content, n = retype_exact(
        content,
        "/Script/ShooterGame.ReplayEffectComponent:ReplayPlayContinuousEffectAtLocation",
        "AllianceFilter", "FieldType::EnumRemainingBits", "FieldType::EnumByte",
        expected=1)
    count += n

    # Fix: Raw -> ObjectNetGuid for DeathMontageEffectOverride and
    # DeathMontageEffectOverrideContext on both MulticastNotifyDamage_* RPCs.
    #
    # The descriptors declare both with AddRaw
    # (MulticastNotifyDamagePointParameters.cs:55-56, ...BaseParameters.cs:29-30):
    # an opaque payload, not a stated wire type -- the TransitionContext case
    # above. A scoped type or the checksum cannot reach them, because a Raw
    # table entry wins in resolve_entry before either is consulted.
    #
    # Measured 2026-09-28 over the 1,018 replays audited at 259ed10, both
    # fields, both RPCs: 959,445 rows each (Point 632,906, Base 326,539), all
    # main stream; the 3 replays without them (12.10, 12.11, 13.00) never
    # declare the fields. Each name carries one checksum corpus-wide
    # (1712763745 / 2397897524) and no other property carries either. The
    # handle drifts by build (Point 40-43 / 41-44, Base 32-35 / 33-36); the
    # name lookup is what keys this, so do not "fix" OVERLAY_HANDLE_TABLE from
    # it. Read as IntPacked every row is consumed exactly, and in every build
    # every 8-bit row is 0x00 -- the null reference -- so the widths carry the
    # value's size, as IntPacked requires:
    #
    #   DeathMontageEffectOverride: 8 bits 947,364, 16 bits 12,081. The
    #   12,081 non-zero values are all odd (static GUIDs), 2,642 distinct, and
    #   every one resolves in the same export's net_guids to one of 190
    #   `FXC_*_C` effect classes -- finisher kill effects
    #   (FXC_Finisher_Afterglow_Victim_C, ..._Demonstone_..., ...). Non-zero
    #   only on events with bDamageKilledTarget, and only with a non-zero
    #   Context.
    #   DeathMontageEffectOverrideContext: 8 / 16 / 24 bits (897,283 / 62,075
    #   / 87). The 62,162 non-zero values are all even (dynamic): net_guids
    #   resolves 0 of them, as it does for the already-typed
    #   EventInstigatorPawn on the same events, while actors.parquet resolves
    #   62,162 of 62,162 to a `*_PC_C` player pawn open at the event's time_ms
    #   -- the EquippableUsed standard above. Non-zero only on killing events.
    #   It equals EventInstigatorPawn on 22,224 of them and Character on
    #   1,537, so it is a pawn reference and nothing more specific: not "the
    #   killer", not "the victim".
    #
    # Second implementation: validate_type_evidence.py (ObjectNetGuid,
    # checksum-scoped) over the whole corpus for the Override, exit 0 with
    # 0 failures; over 5 exports (11.06-13.06) for the Context, exit 0.
    for field in ("DeathMontageEffectOverride", "DeathMontageEffectOverrideContext"):
        for rpc in ("MulticastNotifyDamage_Base", "MulticastNotifyDamage_Point"):
            content, n = retype_exact(
                content, f"/Script/ShooterGame.DamageableComponent:{rpc}", field,
                "FieldType::Raw", "FieldType::ObjectNetGuid", expected=1)
            count += n

    # Additions last, so the bucket recount below sees them.
    content, n_added = apply_additions(content)
    count += n_added
    content = resync_table_len(content)

    content, n_handle_added = apply_handle_additions(content)
    count += n_handle_added
    content = resync_handle_table_len(content)

    content, header_lines = rewrite_header(content)

    # The operation count is a diagnostic, not the verdict. 0 is correct when
    # the table was already corrected and wrong when the patterns are dead;
    # only the end state distinguishes them.
    #
    # TWO end states, and they answer different questions:
    #
    #   dead        `content` is the corrected copy, so a correction missing
    #               HERE could not be applied at all -- the pattern it matches
    #               on is gone. That is the failure this script was rewritten
    #               to catch and its message is unchanged.
    #   uncorrected present in the corrected copy and absent from the FILE, so
    #               the passes can fix it and nobody ran them. `--check` used
    #               to verify only the copy, so this was silently clean --
    #               and CI runs `--check`, which is how a regenerated table.rs
    #               went green while the Rust build used the uncorrected one.
    #
    # A regenerated table trips both at once (the one-line passes are dead in
    # the rustfmt'd layout while the block-based ones apply fine in memory), so
    # both sections print rather than one hiding the other.
    dead = verify(content)
    uncorrected = (
        [p for p in verify(on_disk) if p not in dead]
        if check_only else []
    )
    checked = expectation_count(content)
    if dead or uncorrected:
        if dead:
            print(f"FAILED: {len(dead)} of {checked} corrections are "
                  f"missing from {TABLE_RS}", file=sys.stderr)
            for line in dead:
                print(f"  {line}", file=sys.stderr)
            print("If table.rs was regenerated, run extract_descriptors.py, "
                  "then THIS script, and only then cargo fmt.", file=sys.stderr)
        if uncorrected:
            print(f"FAILED: {len(uncorrected)} of {checked} corrections are "
                  f"absent from {TABLE_RS} but ARE applied by this script -- "
                  f"the file was never corrected.", file=sys.stderr)
            for line in uncorrected:
                print(f"  {line}", file=sys.stderr)
            print("Run this script WITHOUT --check, then cargo fmt, and commit "
                  "the result.", file=sys.stderr)
        return 1

    if check_only:
        for pattern, line in zip(HEADER_RES, header_lines):
            stale = pattern.search(on_disk)
            if stale and stale.group(0) != line:
                print(f"FAILED: the generated header disagrees with the table.\n"
                      f"  file says {stale.group(0)}\n"
                      f"  counted   {line}", file=sys.stderr)
                return 1
    else:
        atomic_write_text(TABLE_RS, content)

    verb = "verified" if check_only else "applied"
    summary = "; ".join(line.lstrip("/ ").rstrip(".") for line in header_lines)
    print(f"{verb}: {count} replacement(s) made, "
          f"all {checked} corrections present in {TABLE_RS}; {summary}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
