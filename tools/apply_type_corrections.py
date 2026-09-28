"""Apply the measured type corrections and ADDITIONS to the generated table.rs.

Two kinds of entry. A correction retypes an entry the C# descriptors declare
and the wire contradicts; an ADDITION types a field the descriptors are silent
on, under the bar written above `ADDITIONS`. Each entry's evidence sits at it.

Run after extract_descriptors.py and before cargo fmt. Rules key on each
entry's own group, field and type, so they rewrite the one-line layout and the
rustfmt'd one alike. The verdict is the END STATE of every EXPECTED row, never
the operation count, which is 0 both on a corrected table and on a dead rule.
`--check` verifies the FILE: a correction missing even from the corrected copy
is a DEAD PATTERN; one the corrected copy has and the file lacks means the file
was NEVER CORRECTED.

Usage:
    python tools/apply_type_corrections.py            # apply, then verify
    python tools/apply_type_corrections.py --check    # verify the file, no write
"""
import argparse
import re
import sys
from collections import Counter
from collections.abc import Callable
from pathlib import Path
from typing import NamedTuple

if __package__:
    from .atomic_io import atomic_write_text
else:  # direct script execution
    from atomic_io import atomic_write_text

TABLE_RS = Path(__file__).parent.parent / "crates" / "vrf-decode" / "src" / "table.rs"

#: Gekko's Wingman: the one class whose `ReplicatedMovement` location is packed
#: at two decimals. See its rule in `RETYPES`.
SEEKER_NADE_GROUP = (
    "/Game/Characters/AggroBot/S0/Ability_Q/Pawn_Aggrobot_SeekerNade."
    "Pawn_Aggrobot_SeekerNade_C"
)

#: The five AGameObject-derived classes (CoveAbility, DarkCoverAbility and the
#: three Smonk descriptors) whose `ReplicatedMovement` the C# declares with a
#: bare `.ReplicatedMovement()` -- the builder's ShortComponents default -- and
#: which their rule in `RETYPES` reads with byte rotator components. Exact
#: group paths, compared with `==`.
#:
#: None of the five ever replicates a rotation, so both widths read the same 3
#: flag bits and the wire cannot choose. The game's class data does (13.06
#: cooked classes and executable reflection, read-only): all five derive
#: natively from AGameObject > AActor, like
#: GameObject_Terra_C_TimeSlowGrenade_Explosion_C, which the table reads at
#: byte width on wire evidence; no Blueprint default in their chains writes
#: ReplicatedMovement (build-wide only PlaceholderPlayerController_C does), so
#: the quantization is the native class's; and per native class every
#: observable rotation decodes at one width only -- AShooterCharacter 7/7
#: Short, AProjectile 38/38 Byte, AGameObject_NoMesh 2/2 Byte, AGameObject 4/4
#: Byte. That is a prior, not a measurement of these five. The bound: all 903
#: replays whose main stream declares ReplicatedMovement on one of them
#: (2026-09-28 survey of the 1,018-replay corpus, 21 builds, 11.06-13.06)
#: export byte-identical Parquet and manifest.json, checkpoints and overlay
#: counters included, before and after this rule. If one is ever seen
#: replicating a rotation, exact consumption decides and overrules the prior.
GAME_OBJECT_BYTE_ROTATOR_GROUPS = (
    "/Game/Characters/Mage/S0/Ability_E/GameObject_Mage_E_WorldSmoke."
    "GameObject_Mage_E_WorldSmoke_C",
    "/Game/Characters/Smonk/S0/Ability_E/MapTargetSmoke/GameObject_Smonk_NewSmoke."
    "GameObject_Smonk_NewSmoke_C",
    "/Game/Characters/Smonk/S0/Ability_E/MapTargetSmoke/GameObject_Smonk_NewSmoke_PDS."
    "GameObject_Smonk_NewSmoke_PDS_C",
    "/Game/Characters/Smonk/S0/Ability_Q/DebuffKnife/DecayLauncher/"
    "GameObject_Smonk_Q_DecayExplosion.GameObject_Smonk_Q_DecayExplosion_C",
    "/Game/Characters/Wraith/S0/Ability_4/Zone_Wraith_4_Smoke.Zone_Wraith_4_Smoke_C",
)

#: The four `EffectID` entries the C# descriptors declare `UInt64`, retyped
#: `Int64`: (group, the replay's compatible_checksum, the chain it reproduces).
#:
#: Each is `FEffectID.EffectID`, and each checksum reproduces with it typed
#: `int64`, not `uint64` (tools/tests/test_compatible_checksum_facts.py
#: recomputes all four); the 13.06 executable's reflection agrees. `decode_u64`
#: would also refuse a legitimate negative int64 (bit 63 set). No exported value
#: changes: over all 1,018 replays (r3 exports, main and checkpoint tables,
#: 2026-09-28) 27,672,549 rows under 2340855891, 3,269,732 under 2251343646 and
#: 26,813 `ActiveBlinds[].EffectID` leaves are all 64 bits wide, values
#: 2..40,853, bit 63 never set, equal to an independent little-endian i64 read
#: on every row. 2340855891 also reaches MulticastStopContinuousEffect, the
#: weapons' MulticastPlayContinuousEffectFromClient and
#: ReplayStopContinuousEffectAtLocation through checksum_table.rs, which learns
#: the new type with `extract_checksum_types.py --retype`;
#: `ActiveBlinds[].BlindEffectID.EffectID` (3321413110) is typed in
#: crates/vrfkit/src/sink/blobs.rs.
EFFECT_ID_INT64 = (
    ("/Script/ShooterGame.EffectManagerComponent", 1129645208,
     "ServerActiveEffects: TArray<FActiveEffectInfo> -> EffectID: FEffectID "
     "-> EffectID: int64"),
    ("/Script/ShooterGame.EffectManagerComponent:MulticastPlayContinuousEffect",
     2340855891, "parameter EffectID: FEffectID -> EffectID: int64"),
    ("/Script/ShooterGame.EffectManagerComponent:MulticastUpdateContinuousEffect",
     2340855891, "parameter EffectID: FEffectID -> EffectID: int64"),
    ("/Script/ShooterGame.ReplayEffectComponent:ReplayPlayContinuousEffectAtLocation",
     2251343646, "parameter CurrentEffectID: FEffectID -> EffectID: int64"),
)

#: (group_path substring, field_name, the WHOLE required `FieldType::...`), one
#: per correction, checked against the file after writing; a miss is a hard
#: failure. `verify` compares types with `==`: a substring test would pass
#: `Int32` for `UInt32` and `Byte` for `EnumByte`.
EXPECTED = [
    ("TimedBomb.TimedBomb_C", "TimeRemainingToExplode", "FieldType::Double"),
    ("TimedBomb.TimedBomb_C", "DefuseProgress", "FieldType::Double"),
    ("Comp_Ability_CooldownComponent_C", "StartTimeStamp", "FieldType::Double"),
    ("Comp_Ability_CooldownComponent_C", "CooldownSeconds", "FieldType::Double"),
]
#: The non-weapon groups whose "215"/"216" the descriptors declare Int32. Their
#: rule in `RETYPES` matches a group path ENDING with one of these.
NON_WEAPON_215_216 = (
    "TimedBomb.TimedBomb_C",
    "EquippablePickupProjectile.EquippablePickupProjectile_C",
    "EquippableGroundPickup.EquippableGroundPickup_C",
    "OwnerExclusivePlayerInfo",
    "Projectile_Phoenix_Q_FlameWall_ThroughWall.Projectile_Phoenix_Q_FlameWall_ThroughWall_C",
)
for _group in NON_WEAPON_215_216:
    for _field in ("215", "216"):
        EXPECTED.append((_group, _field, "FieldType::EnumRemainingBits"))
EXPECTED += [
    ("/Game/Characters/", "ReplayLastTransformUpdateTimeStamp",
     "FieldType::Float"),
    ("SmokeScreen", "ReplicatedMovement",
     "FieldType::RepMovement { rotation: RotatorQuantization::ByteComponents, "
     "location: VectorQuantization::RoundWholeNumber }"),
    (SEEKER_NADE_GROUP, "ReplicatedMovement",
     "FieldType::RepMovement { rotation: RotatorQuantization::ShortComponents, "
     "location: VectorQuantization::RoundTwoDecimals }"),
    *[(_group, "ReplicatedMovement",
       "FieldType::RepMovement { rotation: RotatorQuantization::ByteComponents, "
       "location: VectorQuantization::RoundWholeNumber }")
      for _group in GAME_OBJECT_BYTE_ROTATOR_GROUPS],
    *[(_group, "EffectID", "FieldType::Int64") for _group, _c, _chain in EFFECT_ID_INT64],
    ("AresEquippableDataTracker", "OriginalBuyerTeam", "FieldType::FName"),
    # The next eight are typed by extract_descriptors.py's PAYLOAD_DECODER_TYPES
    # (`.Decode(ValorantPayloadDecoders.X)`, DamageParameters.cs:50-51 and
    # MulticastNotifyDamagePointParameters.cs:40-46) and only verified here.
    # EquippableUsed on 02d4d478: all 632 values 8/16/24 bits and even, as
    # dynamic NetGUIDs are (116 distinct), and 114 of 115 resolve to a weapon
    # class path in actors.parquet. The reference bundle agrees on the scales:
    # DamageImpactLocation integral, DamageOrigin two decimals,
    # DamageDirection / DamageImpactNormal unit vectors.
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

#: Entries the C# descriptors are SILENT on, typed anyway. A correction says
#: "the descriptor declares X and the wire disagrees"; an ADDITION has nothing
#: to disagree with, so the bar is higher: complete, self-checking wire
#: evidence -- one bit width on every row and a value distribution a wrong type
#: cannot produce -- or a type the reference declares for the same property
#: under another group. Never widen this list by eye; the evidence for each
#: entry is written at that entry.
#:
#: Keys are group + name, not checksum: decode_byte fails only at 0 or >8 bits,
#: so a future build that reused a name for a different enum of <=8 bits would
#: decode silently wrong. Swiftplay's game state falls back to the Bomb class
#: through GROUP_ALIASES (vrf-decode overlay.rs), so no Bomb entry is repeated
#: for it.
#:
#: DELIBERATELY NOT ADDED -- each failed the bar and keeps its raw bits:
#: * BaseTeamState Wins, Points, InitialRole, TeamRole, TeamPlayerStates,
#:   TeamComponent, TeamExclusiveTeamInfo: the 13.02 group declares them, the
#:   reference declares none of them under any group, so no type has a source.
#: * Comp_AbilityStatisticsReplicator.AbilityCastsThisRound as a scalar (on all
#:   ten player characters): 16 to 6712 bits over 955 rows, and the bytes after
#:   the 16-bit `0000` empty case are ASCII GUID strings -- a variable-width
#:   struct array (its members are typed below), not an Int32.
#: * Comp_AbilityFuelSystem FuelFull/FuelEmpty: 0-bit payloads.
#: * BlindManagerComponent.ActiveBlinds: a variable-width array.
#: * DamageableComponent LifeChangeBySection (beside the heal/decay
#:   parameters): 177 bits, a variable-width struct array.
#: * ZoomMultiplierComponent SourceZoomLevel/TargetZoomLevel: ~70% of rows are
#:   the 0xFFFFFFFF sentinel, NaN as Float -- an enum or sentinel, not a clean
#:   float; CooldownOption/TransitionState: 2-bit, where the width alone cannot
#:   tell an enum from a SerializedInt.
#: * FiniteSpeedMovementComponent.bIsActive: 1 bit, but all 574 rows are 0x01;
#:   NumCollisions: 32 bits, but the i32 values are only 0/1, indistinguishable
#:   from a Bool the property block widened to 32 bits; RequestedIgnoreActors:
#:   a variable-width array.
ADDITIONS = [
    # Crosshair settings: each name decoded independently, with exact
    # consumption, on retained 13.01/13.02/13.04/13.05 payloads. B is absent:
    # its color handles are 8-bit, but the name also carries a 32-bit property
    # (checksum 943211507, handle 220/205/208 by build) that a name key cannot
    # split, so both are typed by exact identity in
    # tools/fixtures/scoped_type_evidence.json -- the byte-shaped B's as Byte,
    # the 32-bit B (second player-state GUID word, with A, C, D) as UInt32.
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
    # Tidal Wave: types from upstream 99d9646 (pinned at b51d674), each
    # confirmed against retained payload widths and ranges. AliveChunks is a
    # variable-width collection and stays raw.
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
    # ChosenCeremonyForRound, on wire evidence alone (no descriptor names it):
    # 246 rows over 6 replays of both 13.01 and 13.02. 126 are 16 bits and, read
    # as IntPacked, resolve 126/126 to an actors.parquet actor whose class ends
    # in `Ceremony_C` (Default, Clutch, Closer, Flawless, Ace, TeamAce); 120 are
    # 8 bits of `00`, the null GUID written at round start and replaced at round
    # end; 0 are odd, as a dynamic NetGUID must be even.
    ("/Game/GameModes/Bomb/BombGameState.BombGameState_C",
     "ChosenCeremonyForRound", "FieldType::ObjectNetGuid"),
    # Phoenix's wall, the other class declaring MulticastAddSmokeScreenPoint:
    # 2,791 rows over 31 replays read null at 0 decode errors while Viper's
    # SmokeScreenManager was typed. The checksum fallback rightly refused to
    # carry Viper's types over: different properties (2794273677 / 1639439377
    # against 2235276067 / 2983776962). Every row is 192 bits (3 x f64);
    # Translation reads as map coordinates ((7211.7, 1670.3, 96.0) on an Ascent
    # replay); Scale3D is (1,1,1) on every row, which no other reading gives.
    # The parameter's third member, named `249` (the hardcoded FName index of
    # Rotation, not a handle), is its FQuat X/Y/Z like the typed `249`s below
    # -- 177696787 reproduces as `ValveSetTransform: FTransform -> Rotation:
    # FQuat` (tools/tests/test_compatible_checksum_facts.py), and all 58,598
    # rows under it in the 1,018-replay corpus (Phoenix's and Viper's walls,
    # Astra's MulticastAddAnchor) are 192 bits with |xyz| <= 1 -- but stays raw;
    # typing it is a separate change.
    ("/Game/Characters/Phoenix/S0/Ability_Q/Production/"
     "GameObject_Phoenix_Q_FlameWallManager_Production."
     "GameObject_Phoenix_Q_FlameWallManager_Production_C:MulticastAddSmokeScreenPoint",
     "Translation", "FieldType::VectorDouble"),
    ("/Game/Characters/Phoenix/S0/Ability_Q/Production/"
     "GameObject_Phoenix_Q_FlameWallManager_Production."
     "GameObject_Phoenix_Q_FlameWallManager_Production_C:MulticastAddSmokeScreenPoint",
     "Scale3D", "FieldType::VectorDouble"),
    # `249` on the effect-placement RPCs: the rotation that pairs with the `248`
    # location, unnamed, on 441,814 rows over 20 replays. Its widths are 3, 19
    # (most rows), 35 and 51 bits, exactly `3 + 16 x (flags set)`, which only
    # RotationShort produces; decoding all 441,814 that way leaves no bit over;
    # and the table's named twin, ReplayPlayContinuousEffectAtLocation.Rotation,
    # is already RotationShort. Yaw is set on 92.5% of rows, pitch 14.5%, roll
    # 0.1%, all on a 0.0055-degree lattice. The family's 2526428638 reproduces
    # as a top-level `Rotation: FRotator` (test_compatible_checksum_facts.py);
    # the other `249`, an FTransform's FQuat (747197698 and siblings, see the
    # RPC vectors below), is a different property under the same number.
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
    # The RNG component's seed: 120,853 rows, one group, one checksum, 32 bits
    # each, 120,852 distinct over the full i32 range -- a seed, not a counter,
    # time or GUID; sibling AuthInitialRandomSeed matches in width and
    # distinctness. The data cannot settle the sign: Int32 follows Unreal's
    # FRandomStream (an int32 seed) and changes only half the values' sign.
    ("/Script/ShooterGame.NetworkedRandomNumberGeneratorComponent",
     "AuthCurrentRandomSeed", "FieldType::Int32"),
    # BaseTeamState, new in 13.02, which deleted BombGameState.TeamEconomy and
    # moved team economy to its own actor under the same property names: a
    # relocated property whose type the pinned reference declares, not one
    # guessed from values (GameState/AresTeamEconomy.cs:11-12, `int?
    # LoadoutValue, int? AverageLoadoutValue`; OwnerExclusivePlayerInfo's
    # {Start,End}OfRoundLoadoutValue are Int32 too). The wire corroborates: all
    # 44+44 payloads are 32 bits, LE i32 4300/4150 at round 1 up to 34300, and
    # AverageLoadoutValue is exactly LoadoutValue/5 on every row (five players).
    ("/Script/ShooterGame.BaseTeamState", "AverageLoadoutValue", "FieldType::Int32"),
    ("/Script/ShooterGame.BaseTeamState", "LoadoutValue", "FieldType::Int32"),
    # The heal/decay RPC parameters, carried on the wire by name;
    # DamageableComponentClassNetCacheDescriptor.cs declares only the two
    # MulticastNotifyDamage_* functions. On the 98605b1b Demos export: HealTaken
    # 32 bits on all 1,252 rows, Float 0.05..400, with 0x3f800000 (1.0f), a
    # float-only pattern, recurring; DecayApplied 32 bits on all 699 rows, Float
    # 0.07..50, clustering at 0.195. The *Instigator/*Causer references are
    # typed by exact identity in tools/fixtures/scoped_type_evidence.json
    # instead: the damage RPCs reuse those names under other checksums.
    ("/Script/ShooterGame.DamageableComponent:MulticastNotifyHeal",
     "HealTaken", "FieldType::Float"),
    ("/Script/ShooterGame.DamageableComponent:MulticastNotifyOverhealDecay",
     "DecayApplied", "FieldType::Float"),
    # MaximumRange, a projectile's travel limit in Unreal units; no descriptor
    # declares the group. On the 98605b1b Demos export: 32 bits on all 11,699
    # rows, Float 397.6..49986.1, mode ~19,993.
    ("/Script/ShooterGame.FiniteSpeedMovementComponent",
     "MaximumRange", "FieldType::Float"),
    # ServerMovementTime, a movement clock in seconds, over all 714
    # release-13.01..13.05 exports: 32 bits on all 4,571,175 rows, in exactly
    # these four groups; an independent LE Float read is finite everywhere and
    # monotonic for all 337,754 actors. Values start near 1/128 s and fit an
    # actor-relative clock, but the epoch (actor spawn or not) is not
    # established. Residuals follow the 128 Hz grid and replication delay;
    # Phoenix FlareCurve actors can open their channels ~0.65 s after it starts.
    ("/Script/ShooterGame.FiniteSpeedMovementComponent",
     "ServerMovementTime", "FieldType::Float"),
    ("/Script/ShooterGame.SplineMovementComponent",
     "ServerMovementTime", "FieldType::Float"),
    ("/Script/ShooterGame.PrecalculatedProjectileMovementComponent",
     "ServerMovementTime", "FieldType::Float"),
    ("/Game/Characters/Components/Comp_Projectile_FloatCurveMovement."
     "Comp_Projectile_FloatCurveMovement_C",
     "ServerMovementTime", "FieldType::Float"),
    # ReplayLastTransformUpdateTimeStamp on the nine descriptor-silent pawn
    # groups; RETYPES corrects the 33 the descriptor emits as Skip. Over the same
    # 714 exports: 32 bits on all 32,978,229 rows in 42 character/pawn groups, a
    # finite, actor-monotonic Float in seconds. It behaves as server world time
    # with a per-file offset from replay time -- ~10 s on 672 of 714 files,
    # 10.8..110.1 s on the other 42 -- so estimate the offset, never subtract 10.
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
    # MoneyManagementComponent, the live per-player credits, on 13.01 and 13.02
    # (02d4d478 and 13.02 Demos files). No descriptor declares the group: Money
    # and TotalMoneyGranted appear under no group in the reference, and
    # StartOfRoundMoney only under OwnerExclusivePlayerInfo
    # (OwnerExclusivePlayerInfoDescriptor.cs:93, an end-of-round snapshot),
    # which sources its Int32. All three 32 bits on every row, LE i32: Money is
    # 800 on all ten actors at pistol-round start (t=8 ms) and 0..9000 in steps
    # of 50; StartOfRoundMoney 800 for active players, else 0;
    # TotalMoneyGranted cumulative 800..34200. The group has no other wire field.
    ("/Script/ShooterGame.MoneyManagementComponent", "Money", "FieldType::Int32"),
    ("/Script/ShooterGame.MoneyManagementComponent", "StartOfRoundMoney", "FieldType::Int32"),
    ("/Script/ShooterGame.MoneyManagementComponent", "TotalMoneyGranted", "FieldType::Int32"),
    # Ping, which no descriptor declares, was the largest untyped wire-declared
    # field: 20,193 rows on 02d4d478 (222,855 over the 11-replay bundle), always
    # 16 bits, a LE u16 behaving as latency in ms (02d4d478: min 6, p5 10, p50
    # 15, p90 19, p99 25, max 473, 57 distinct).
    # SerializedInt{65536} reads exactly those 16 bits, LSB-first, and passes
    # decode_field's full-consumption guard.
    # Typed despite PROJECT_STATUS 18-D: it passed Money's gate; per-player latency is now wanted.
    ("/Game/GameModes/Bomb/BombPlayerState.BombPlayerState_C",
     "Ping", "FieldType::SerializedInt { max: 65536 }"),
    # BasicCombatStatsComponent, the cumulative scoreboard K/D/A: on
    # release-13.02 all 407 updates are 32 bits, and the final LE Int32
    # counters match the in-game scoreboard for all ten players, including the
    # two post-round bomb deaths the kill RPCs report and the scoreboard omits.
    ("/Script/ShooterGame.BasicCombatStatsComponent",
     "AggregateKills", "FieldType::Int32"),
    ("/Script/ShooterGame.BasicCombatStatsComponent",
     "AggregateDeaths", "FieldType::Int32"),
    ("/Script/ShooterGame.BasicCombatStatsComponent",
     "AggregateAssists", "FieldType::Int32"),
    # PlayerScoreComponent.Score, the per-player combat score; no descriptor
    # declares the group. On the 98605b1b Demos export: 32 bits on all 430 rows;
    # as Float a denormal (~1e-44), as Int32 21..5833 with 415 distinct values,
    # a cumulative match score.
    ("/Script/ShooterGame.PlayerScoreComponent", "Score", "FieldType::Int32"),
    # Comp_Actor_Concussable, a generic component under
    # /Game/Characters/Components/ that no descriptor declares. On the 98605b1b
    # Demos export: 9 actors spanning eight agents (Phoenix, Breach, Smonk,
    # Clay, Guide, Wushu, Terra, Pandemic, Deadeye) plus Guide's
    # PossessableScout pawn, with the same names and widths on each. All 375
    # rows: Start/EndTime 32 bits on 39 rows each, Float game-seconds ~2.5 s
    # apart (389.5/392.0 .. 1916.7/1919.2); Level 64 bits on 297 rows, a Double
    # 0..1 intensity ramp.
    ("/Game/Characters/Components/Comp_Actor_Concussable.Comp_Actor_Concussable_C",
     "ConcussStartTime", "FieldType::Float"),
    ("/Game/Characters/Components/Comp_Actor_Concussable.Comp_Actor_Concussable_C",
     "ConcussEndTime", "FieldType::Float"),
    ("/Game/Characters/Components/Comp_Actor_Concussable.Comp_Actor_Concussable_C",
     "ConcussLevel", "FieldType::Double"),
    # Comp_AbilityFuelSystem, a generic per-ability component no descriptor
    # declares (on 98605b1b: Sage's heal Ability_Guide_4_Heal and Viper's
    # smoke). CurrentFuel is 64 bits on all 5,702 rows, a Double draining
    # smoothly 1.0 -> 0.0 (1.0, 0.9993, 0.9909, 0.9824, ...); IsFuelDraining is
    # 1 bit on all 60 rows, raw 0x00/0x01.
    ("/Game/Characters/Components/Comp_AbilityFuelSystem.Comp_AbilityFuelSystem_C",
     "CurrentFuel", "FieldType::Double"),
    ("/Game/Characters/Components/Comp_AbilityFuelSystem.Comp_AbilityFuelSystem_C",
     "IsFuelDraining", "FieldType::Bool"),
    # LongestActiveBlindDuration: Float 0.0..2.1 s, agent-common
    # (blind_duration_is_typed in crates/vrf-decode/src/tests/overlay.rs).
    ("/Script/ShooterGame.BlindManagerComponent",
     "LongestActiveBlindDuration", "FieldType::Float"),
    # ZoomMultiplierComponent, the ADS/scope FOV transition; no descriptor
    # declares the group. On the 98605b1b Demos export all five are 32 bits on
    # every row with no NaN: Source/TargetFov 20.6..103.0 (103.0 is Valorant's
    # documented default hip-fire FOV, which a wrong type cannot yield),
    # Source/TargetFov1P 5.0..70.0 (70.0 the default 1P FOV),
    # TotalTransitionTimeDuration 0.0..0.25 s.
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
    # ReadyingStateComponent.AuthEquipSpeed, the equip-speed state of a weapon
    # being readied; no descriptor declares it. 2026-09-28, the 1,018 replays
    # audited at 259ed10, rows by name or by checksum 3151779304 in any group
    # (only this identity carries either): 866,096 main + 149,419 checkpoint
    # rows, exactly 3 bits with zero padding on every build that has it (none
    # on 12.10, 12.11, 13.00). Main values are {0, 1, 2} on every build
    # (432,879 / 347,664 / 85,553; 11.06 alone 1,400 / 1,297 / 104); on every
    # 25th export the transitions are only 0 <-> 1 and 0 <-> 2, and 2 holds a
    # median 203 ms. More than a width: the same-width sibling
    # AutoEquipTransitionContext.AutoEquipSpeed (EnumByte in the vendored
    # AdditionalComponentDescriptors.cs, 1dee99f -- the same reader, not an
    # independent authority) holds {0, 1, 2} on every build, equals an
    # independent decode on 58,384 of 58,384 sampled rows, and equals
    # AuthEquipSpeed in the same packet on the same actor on 1,986 of 1,999
    # rows; at a uniform 3 bits only SerializedInt max 8 fits, which gives the
    # same integers, so the CooldownOption/TransitionState refusal does not
    # apply. All 149,419 checkpoint rows read 0 (the bits are 000) while
    # AutoEquipSpeed and EquipSpeedOverride are non-zero in the same
    # checkpoints and 0.2% of carriers are mid-readying: unexplained, so a
    # checkpoint AuthEquipSpeed is not readying state. Enum names are unknown;
    # only the integer is typed. Not checksum-scoped: generate_scoped_types.py
    # has no EnumByte.
    ("/Script/ShooterGame.ReadyingStateComponent", "AuthEquipSpeed",
     "FieldType::EnumByte"),
    # AresInventory's server/client correction counters, declared nowhere in
    # AresInventoryDescriptor.cs. 2026-09-28, the 1,018 audit replays;
    # checksums 3198546915 / 1076231069, each carried by this field only.
    # * CorrectionIndex: 1,041,822 main + 181,108 checkpoint rows, 32 bits on
    #   all 24 builds; LE i32 1..2011, never 0 or negative, strictly increasing
    #   per (actor, object) on all 1,001,823 consecutive main pairs; each
    #   checkpoint value equals the last main value for the same (actor,
    #   object) at or before it. As f32 every value is a denormal (the Score
    #   shape), and the max rules out a widened Bool.
    # * LastSeenClientCorrectionIndex: 948,502 main + 181,097 checkpoint rows,
    #   32 bits, 1..2010, strictly increasing per actor on all 921,268 pairs,
    #   never >= CorrectionIndex at the same (time, actor, object): L == C-1 on
    #   814,054 main rows, L < C-1 on 134,448, L >= C on 0 (checkpoints 179,990
    #   / 1,107 / 0).
    # The handle moves from 29/30 (11.06-12.03) to 30/31 (12.04 on); the name
    # key does not. Int32 follows the same descriptor's Int32 RespawnNumber
    # (1..2011 cannot settle the sign); the meaning rests on the names and the
    # counter shape only.
    ("/Script/ShooterGame.AresInventory", "CorrectionIndex", "FieldType::Int32"),
    ("/Script/ShooterGame.AresInventory", "LastSeenClientCorrectionIndex",
     "FieldType::Int32"),
    # The 192-bit RPC vectors. An FTransform parameter reaches this wire as
    # three double vectors -- rotation, translation, scale -- none declared, so
    # 54,859 rows arrived raw; the table's MulticastPlayContinuousEffect:
    # Transform entry (320-bit FieldType::Transform) is dead here, as the
    # replay's schema has no `Transform` parameter. As 3 x f64 Scale3D is
    # exactly (1.0, 1.0, 1.0) on every row -- 6 x f32 would give (0, 1.875, 0,
    # 1.875, 0, 1.875) -- and the `248` locations are map coordinates in Unreal
    # units with plausible floor heights.
    # `249` is the transform's Rotation FQuat, X/Y/Z only (doubles under UE5
    # large world coordinates), named by hardcoded FName index 249: 747197698
    # reproduces as `Transform: FTransform -> Rotation: FQuat`, not FRotator
    # (3753301026) or FVector (1853556327), with Translation/Scale3D under the
    # same parent (tools/tests/test_compatible_checksum_facts.py), and the 13.06
    # reflection has Transform.Rotation as a quaternion. W is not sent: per the
    # public engine source, NOT verified in this binary, FQuat::NetSerialize
    # normalizes, flips all four signs when W < 0 and writes X, Y, Z, so W =
    # sqrt(max(0, 1 - |xyz|^2)). The data fit: over all 1,018 replays (r3
    # exports, 2026-09-28) all 12,698,371 rows under 747197698 are 192 bits with
    # |xyz| <= 1 (max 1.0, none above 1 + 1e-6), and 12,594,391 hold a -0.0,
    # what the sign flip leaves on a zero component. value_str is the
    # quaternion's "(x,y,z)", not Euler angles or a direction; no W is
    # exported. Cross-check: BombPlantedRPC.PlantLocation and the unrelated
    # MulticastActivateBombSiteEffects.BombLocation report byte-identical
    # coordinates, 9 rows each against 9 `spikePlanted` events. The replay's
    # checksums agree with the grouping without having derived it: every `248`
    # 598402184, `249` 747197698, Translation 2235276067, Scale3D 2983776962.
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
    # EffectManagerComponent on the weapons' two effect RPCs: the EffectManager
    # of the pawn holding the weapon; no descriptor declares it. Checksum
    # 1051633025 is declared at parameter handle 0 of exactly these two
    # functions in all 1,018 manifests audited at 259ed10 (the OneShot function
    # exists in 993), always under this name; the rows' `handle` is the
    # function slot instead (1/2 Continuous, 2/3 OneShot; see ForceModule
    # below). 2026-09-28, those exports (fields + checkpoint_fields, checksum ==
    # 1051633025): 3,112,054 rows (Continuous 2,906,169 under 24 weapon
    # _ClassNetCache paths, OneShot 205,885 under 17 -- DMR arrives under two
    # folder spellings), all main stream, as RPC parameters always are. 16 bits
    # (3,103,449) or 24 (8,605), every row consumed exactly as IntPacked, 0 zero,
    # 0 odd, every value resolving in the same export's net_guids to one path,
    # `EffectManager`, whose outer is a *_PC_C player character on 3,110,231
    # rows and Yoru's decoy (Pawn_Stealth_4_Decoy_V2_C) on 1,823. The outer
    # equals the weapon's latest typed Instigator at or before the RPC's time_ms
    # on 3,111,803 rows; the other 251 match an earlier Instigator of that
    # weapon, 250 of them with the current one 0 (just dropped); matched by
    # packet_id instead the rate is lower (95.85% on OneShot). Both twins, as
    # for the vectors above: typing one leaves the other raw at 0 decode errors.
    # The RPC's other references (EffectContainer, WaitOnReplicationActor,
    # ClientControllerThatTriggered) were already ObjectNetGuid via the checksum
    # table.
    ("/Script/ShooterGame.AresEquippable:MulticastPlayContinuousEffectFromClient",
     "EffectManagerComponent", "FieldType::ObjectNetGuid"),
    ("/Script/ShooterGame.AresEquippable:MulticastPlayOneShotEffectFromClient",
     "EffectManagerComponent", "FieldType::ObjectNetGuid"),
    # The same FQuat X/Y/Z under another parent: `SpawnTransform: FTransform
    # -> Rotation: FQuat` reproduces its checksum 1874998526 (Translation
    # 131504838, Scale3D 3357891630 follow the same parent). 183,577 rows over
    # the 1,018 replays, all 192 bits, |xyz| <= 1.
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
    # StopMovementTime, the other half of the already-Float StartMovementTime:
    # same RPC family, 32 bits on all 13,316 rows, and as f32 the same shape, a
    # -1.0 sentinel on 5,371 and 0.76..136.98 on the rest (the declared sibling:
    # -1.0..1771.83). One entry per checksum is enough; 244888268 carries it to
    # ReplayStopContinuousEffectAtLocation too.
    ("/Script/ShooterGame.EffectManagerComponent:MulticastStopContinuousEffect",
     "StopMovementTime", "FieldType::Float"),
    # HandleNumber names a force module for the later Remove/Cleanup RPC. As
    # u32 the 3,741 rows hold 1..765 with every value present, a dense
    # sequential id no other reading gives. UInt32, because the checksum
    # decides what the values cannot: 3336285386 (shared with
    # NetMulticastRemoveForceModule) reproduces only as the parameter `Handle:
    # FForceModuleHandle` with member `HandleNumber: uint32` -- not as int32
    # (4073821633), nor at the top level -- and
    # NetMulticastEnforceEndOfLifeCleanup's 1457545067 is the same member inside
    # `ModulesCleanedUpByServer: TArray<FForceModuleHandle>`
    # (tools/tests/test_compatible_checksum_facts.py pins the chains); the 13.06
    # reflection agrees. No exported number changes: over all 1,018 replays
    # (2026-09-28) 665,519 Apply and 2,743,504 Remove rows are 32 bits,
    # 1..4,518, bit 31 never set.
    ("/Script/ShooterGame.ForceModuleManagerComponent:NetMulticastApplyForceModule",
     "HandleNumber", "FieldType::UInt32"),
    # The other five NetMulticastApplyForceModule parameters, none declared.
    # 2026-09-28, the 1,018 replays audited at 259ed10: the RPC occurs in 1,015
    # (not the single 12.10, 12.11 and 13.00 files), 665,519 calls, every row
    # main stream (checkpoint_export_fields declares the RPC, checkpoint_fields
    # holds no row of it). Each checksum below is declared by this parameter
    # and no other property, at one width on every build with rows. Handles
    # quoted here are the manifest's PARAMETER handles; an exported row's
    # `handle` is the ClassNetCache function slot, 1 for Apply.
    #
    # RespawnNumber (3960441757): 32 bits, LE i32 0..38, none negative; equals
    # the character's latest typed AresInventory.RespawnNumber (paired through
    # both RPCs' Character) on 665,363 of 665,370 comparable rows, 4 of the
    # other 7 after the export's last inventory row and 3 before its first.
    # Int32 as the same-named AresInventory and DamageParameters siblings (0..38
    # cannot settle the sign). NOT the component's own
    # ForceModuleManagerComponent.RespawnNumber (3044239005, declared in 554
    # manifests, never carrying a row), a key this entry cannot reach: do not
    # merge them.
    ("/Script/ShooterGame.ForceModuleManagerComponent:NetMulticastApplyForceModule",
     "RespawnNumber", "FieldType::Int32"),
    # NetTimestamp (259706372): 32 bits of finite f32, 0.0..252.703125; 8,285
    # exact zeros are real 0x00000000 payloads; 8,622 of the 657,234 non-zero
    # values are off the 1/128 s grid. Two typed clocks agree: the character's
    # AresInventory.NetTimestamp (under 0.1 s on 645,350 of 648,553 comparable
    # rows) and MulticastNotifyDamage_{Point,Base}.NetTimestamp on the same
    # actor (under 0.05 s on 5,500 of 5,548, a 7-export sample). The epoch is
    # per actor / per life, NOT replay time: do not subtract it from time_ms.
    ("/Script/ShooterGame.ForceModuleManagerComponent:NetMulticastApplyForceModule",
     "NetTimestamp", "FieldType::Float"),
    # ModuleType (3263282135): 3 bits on every row, {0, 2} (656,897 / 8,622),
    # read LSB-first as decode_byte does. Settled outside the field, which the
    # CooldownOption/TransitionState refusal lacked: each of the 39 module
    # class names maps to one type on unambiguously paired rows (2 is exactly
    # the six displacement modules -- Clay knockbacks, Breach X knock-up, the
    # two repel-other-character modules), and NetMulticastRemoveForceModule's
    # ModuleType agrees with the paired Apply on 647,381 of 647,381 rows. Remove
    # is typed by this donor through checksum_table.rs; its value 1 (2,081,004
    # rows, never on Apply, never paired) has no established meaning. Upstream's
    # delta declares Remove.ModuleType EnumRemainingBits: vendored, it would make
    # the donors disagree and drop the checksum, and Remove would go raw unless
    # it got its own entry. Enum names are unknown; only the integer is typed.
    ("/Script/ShooterGame.ForceModuleManagerComponent:NetMulticastApplyForceModule",
     "ModuleType", "FieldType::EnumByte"),
    # Module (739992589): 16 bits, IntPacked, every row odd (a static GUID) and
    # resolving in the same export's net_guids to a ForceModule_* /
    # DeathForceModule_* / FM_* class (39 short names, 43 class/package pairs;
    # ForceModule_Tag_Heavy_C 425,735, DeathForceModule_C 142,089, ...): a
    # TSubclassOf reference no other type could name.
    ("/Script/ShooterGame.ForceModuleManagerComponent:NetMulticastApplyForceModule",
     "Module", "FieldType::ObjectNetGuid"),
    # Character (1346692128): 16/24 bits, IntPacked, every row even (dynamic).
    # net_guids resolves none (dynamic actors are not registered there);
    # actors.parquet opens resolve all 665,519, to 43 character/pawn classes,
    # and the value equals the row's own actor_net_guid (from the channel
    # header, independent of the payload) on every row -- redundant, so
    # self-checking. Join it through actors.parquet or actor_net_guid.
    ("/Script/ShooterGame.ForceModuleManagerComponent:NetMulticastApplyForceModule",
     "Character", "FieldType::ObjectNetGuid"),
    # The callout region a player stands in, reachable once the
    # CalloutRegionTracker leaf was remapped: all 1,957 non-zero rows unpack and
    # resolve in net_guids to a `CalloutRegion_*` path, 22 distinct; nothing
    # else in the export places a player in map terms. That is the region
    # ACTOR's object name (outer: the map's `<Map>_Callout_Volumes` level), not
    # the callout shown or announced: that label is the actor's RegionName,
    # string-table text the replay does not carry, and it differs from the
    # object name's letters for at least 9 of 160 letter-style regions (Ascent's
    # CalloutRegion_A_Link shows "Tree"; installed 13.06 string tables,
    # read-only, 2026-09-28). An identifier, not a label.
    ("/Script/ShooterGame.CalloutRegionTrackingComponent",
     "CurrentRegion", "FieldType::ObjectNetGuid"),
    # The per-cast ability log (who cast what, when, where), which vrfkit
    # flattens into `AbilityCastsThisRound[i].<member>` rows under member names
    # carrying Blueprint property GUIDs, byte-identical on 13.01 and 13.02. Each
    # member checks out against something outside itself: Player's 352 values
    # are 36-char UUIDs, all matching a manifest.players.subject; Round covers
    # exactly 0..17, the replay's 18 rounds; CastLocation is 3 x f64 inside the
    # map bounds movement.parquet describes; Slot takes 3, 4, 5, 9 (three
    # abilities and an ultimate); CastTime is seconds within the round.
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
    # Each cast's `Effects[]`: what it did and to whom, the authoritative debuff
    # log (EnemiesSuppressed, EnemiesSlowed, EnemiesVulnerabled and 28 more)
    # that the cosmetic-effect channel only proxies. LifeChangeEvents' members
    # are typed by life_change_member_type in crates/vrfkit/src/sink/rpc.rs;
    # their MulticastNotifyDamage_* entries in table.rs are unreachable
    # (CLAUDE.md, "Traps").

    # LocalizedStat, an FText whose string-table key is the statistic's name
    # (EnemiesBlinded, DamageDealt, ...): 29 distinct values over 4,341 rows,
    # each 1:1 with a `Statistic` value and without a collision; all 4,341
    # decode with zero residual bits on decode_ftext's layout (an earlier
    # check: 225 of 225 rows, to EnemiesBlocked, HealingDone and 17 more).
    # `Statistic` itself decodes to a bare integer, so this column is the only
    # machine-readable map from those integers to names.
    ("/Game/Characters/_Core/Comp_AbilityStatisticsReplicator"
     ".Comp_AbilityStatisticsReplicator_C",
     "LocalizedStat_14_C3A26F5E46CDAD94571AE6B0EDEA058B", "FieldType::FText"),

    # AffectedPlayer is the check: all 224 values resolve to a
    # manifest.players.actor_net_guid, over exactly 10 distinct players.
    # Statistic is a small enum whose observed values line up with the named
    # statistics (0 EnemiesBlinded, 7 EnemiesBlocked, 8 EnemiesNearsighted, ...),
    # and Time reads as seconds within the round like its sibling CastTime.
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
    # HawkFlash's ReplicatedMovement and Banking. third_party/vrp has no
    # HawkFlash class, so these are ADDITIONS on the exact group; DATA.md's "no
    # name rule for ReplicatedMovement" stands and checksum 2749104612 stays
    # dropped (byte donors still sit beside Gekko's short, two-decimal
    # Wingman). 2026-09-28, the 1,018 replays audited at 259ed10: the group is
    # on 15 builds (11.06-13.06), every row main stream -- no class's
    # ReplicatedMovement reaches a checkpoint table, so that side is untested.
    #
    # ReplicatedMovement (handle 10 on every build): 1,033,952 rows, 71-118
    # bits. ByteComponents consumes all of them exactly on every build;
    # ShortComponents overruns or leaves residue on 564,158 (54.6%); and the
    # rotation is on the wire (pitch/yaw flags on 99.9% of rows), so the width
    # is measured, not bounded. All four flags 0 on every row; |velocity| median
    # 1799.93 (p99 1800.56), the magnitude of the separately typed
    # PostControlVelocity (a different property, whose direction differs); byte
    # yaw against atan2(vy, vx) median error 0.36 deg; the packed location's
    # displacement over ~50 ms windows / dt matches the velocity, median ratio
    # 1.00. An independent reader that matched Rust on 8,249,671 typed Byte rows
    # reproduced all of it. Location in whole units: every header says
    # "scaled", and the packed integer at each of the 8,265 actors' first
    # update is within 0.87 cm of the actors.parquet spawn (2,919 to 15,023
    # units off at /100); on this change's own exports (31 replays, 15 builds,
    # 327 actors, first update at open time) median 0.50 cm, max 0.87, against
    # 8,043 cm at /100. REP_MOVEMENT_LOCATION_EVIDENCE in
    # crates/vrf-decode/src/tests/overlay.rs pins it. Roll is never
    # replicated: its 0.0 is the absent-flag default, not a measurement.
    ("/Game/Characters/Guide/S0/Ability_E/Projectile_Guide_E_HawkFlash."
     "Projectile_Guide_E_HawkFlash_C",
     "ReplicatedMovement",
     "FieldType::RepMovement { rotation: RotatorQuantization::ByteComponents, "
     "location: VectorQuantization::RoundWholeNumber }"),
    # Banking (677106858; handle 17 on 11.06-11.09 and 18 after, where
    # PostControlVelocity took 17, so a handle rule would mistype one of them):
    # 801,700 rows, 64 bits on every row of every build. As LE f64 every value
    # is finite and in [-180, 180] (p1/p99 -74.6/73.5) with the low 29 mantissa
    # bits zero -- a widened f32 -- and the per-actor step wraps at +-180 like
    # an angle; two f32 halves or an Int64 are nonsense (the low word takes 7
    # values; median |i64| 4.6e18). In the proposer's measurement, not
    # re-derived by the verifier, its sign follows the velocity heading's turn
    # direction on 86% of clearly turning rows (r = 0.56). Only a double angle
    # in degrees is claimed; 150 actors start at exactly 0.0.
    ("/Game/Characters/Guide/S0/Ability_E/Projectile_Guide_E_HawkFlash."
     "Projectile_Guide_E_HawkFlash_C",
     "Banking", "FieldType::Double"),
    # Cypher's trapwire and cage, renamed in 13.01. Through 12.08 (and in the
    # one 13.00 fixture) the classes sit at the paths the C# descriptors name,
    # where the table types them:
    # Ability_E/{Ability,GameObject}_Gumshoe_E_TripWire(_SecondWire)_C and
    # Ability_4/{Ability,Projectile}_Gumshoe_4_CageTrap_C. From 13.01 on they
    # are Ability_4/{Ability,GameObject}_Gumshoe_4_TripWire(_SecondWire)_C and
    # Ability_Q/{Ability,Projectile}_Gumshoe_Q_CageTrap_C, and nothing typed the
    # fields below: null on every row at 0 decode errors. Relocated properties
    # with descriptor-sourced types, the BaseTeamState kind. Across the
    # manifests of all 1,018 replays the old paths are declared only on
    # 11.06-13.00 and the new only on 13.01-13.06, never both in one replay,
    # with identical field sets, handles and checksums; the trapwire's
    # SetEnemyInTrap.PairedWire points at its own class, and its checksum moves
    # from 3671888355 to 3454621121 -- exactly `AGameObject_Gumshoe_E_TripWire_C*`
    # -> `AGameObject_Gumshoe_4_TripWire_C*` (test_compatible_checksum_facts.py,
    # with the chains). Figures: 2026-09-28, exports of all 1,018 replays
    # (fields + checkpoint_fields), independent readers from
    # validate_type_evidence.py. The old entries stay (11.06-12.08 carry the
    # old paths). Only Deployed's checksum is donated to checksum_table.rs: the
    # four TripWire groups are its only carriers, so it types nothing new and
    # catches the next rename. 2035145197 and 1992268157 are NOT donated: every
    # agent's ability classes carry them, unmeasured.
    #
    # Deployed (3902815170 = `Deployed: bool`): 8,908 + 8,889 main rows on the
    # two wires in 268 replays of 13.01-13.06, 1 bit, every one true, none in
    # checkpoints. False is the class default and never sent: the one row per
    # wire (17,797 wire actors) arrives 0-266 ms after the channel opens
    # (median 106) on the first wire and 466-1,071 ms (median 743) on the
    # second, the timing of the old paths (0-260 / 512-1,049 ms over their 345 +
    # 345 rows).
    ("/Game/Characters/Gumshoe/S0/Ability_4/GameObject_Gumshoe_4_TripWire."
     "GameObject_Gumshoe_4_TripWire_C", "Deployed", "FieldType::Bool"),
    ("/Game/Characters/Gumshoe/S0/Ability_4/GameObject_Gumshoe_4_TripWire_SecondWire."
     "GameObject_Gumshoe_4_TripWire_SecondWire_C", "Deployed", "FieldType::Bool"),
    # CreatedByCharacter (2035145197 = `CreatedByCharacter: AShooterCharacter*`):
    # 687 main + 5,135 checkpoint rows on each ability class in 270 replays,
    # 16-bit IntPacked or the 8-bit null; every non-null value is the export's
    # Gumshoe_PC_C actor (actors.parquet), as on the old paths.
    ("/Game/Characters/Gumshoe/S0/Ability_4/Ability_Gumshoe_4_TripWire."
     "Ability_Gumshoe_4_TripWire_C", "CreatedByCharacter", "FieldType::ObjectNetGuid"),
    ("/Game/Characters/Gumshoe/S0/Ability_Q/Ability_Gumshoe_Q_CageTrap."
     "Ability_Gumshoe_Q_CageTrap_C", "CreatedByCharacter", "FieldType::ObjectNetGuid"),
    # RelativeScale3D (1992268157): 684 main + 5,135 checkpoint rows on the cage
    # ability, 31 bits, (1, 1, 1) on every row, the old cage's
    # VectorNetQuantize100. The old trapwire ability had no entry, so neither
    # does the new one.
    ("/Game/Characters/Gumshoe/S0/Ability_Q/Ability_Gumshoe_Q_CageTrap."
     "Ability_Gumshoe_Q_CageTrap_C", "RelativeScale3D",
     "FieldType::VectorNetQuantize { scale: 100 }"),
]
EXPECTED += [(g, f, t) for g, f, t in ADDITIONS]

#: (group_path, handle, field_name) names for handles the replay leaves
#: unnamed, each paired with an ADDITION of the same (group_path, field_name) so
#: the overlay can type it. Empty: the mechanism is kept for the next bare
#: handle without a native group to borrow from (docs/DATA.md).
HANDLE_ADDITIONS = [

]

GROUP_RE = re.compile(r'group_path: "([^"]+)"')
FIELD_RE = re.compile(r'field_name: "([^"]+)"')
TYPE_MARKER = "field_type:"


def normalize_type(field_type: str) -> str:
    """One spelling for a field type, whichever layout it was written in.

    rustfmt breaks a braced type over lines with a trailing comma
    (`RepMovement { rotation: X, }`), and `verify` compares with `==`, so that
    collapses to the one-line `RepMovement { rotation: X }` first.
    """
    collapsed = " ".join(field_type.rstrip().rstrip(",").split())
    return re.sub(r",\s*\}", " }", collapsed)


def _field_type_of(block: str) -> str | None:
    """The `field_type` value of one `OverlayEntry { ... }` block.

    Brace-counted, not a regex: field types contain braces
    (`VectorNetQuantize { scale: 100 }`), and the table exists in TWO layouts
    -- one entry per line as extract_descriptors.py emits it, and the rustfmt'd
    multi-line form that is committed -- so a newline-anchored pattern would
    silently match nothing on one of them. The block's opening
    `OverlayEntry {` is already consumed, so the first unmatched `}` closes it.
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
    """Yield (group_path, field_name, field_type) for every OverlayEntry, each
    looked up within its own `OverlayEntry {` block."""
    for block in content.split("    OverlayEntry {")[1:]:
        group = GROUP_RE.search(block)
        field = FIELD_RE.search(block)
        ftype = _field_type_of(block)
        if group and field and ftype:
            yield group.group(1), field.group(1), ftype


def _own_entry(block: str) -> tuple[str, str, str | None]:
    """`(group, field, type)` of the entry that opens one split block, or
    `("", "", None)` when it names none.

    The LAST block also holds all of OVERLAY_HANDLE_TABLE, whose entries repeat
    group paths and field names, so a rule keys on the block's first
    `group_path`/`field_name` and its own `field_type`, never on other text.
    """
    group, field = GROUP_RE.search(block), FIELD_RE.search(block)
    if not (group and field):
        return "", "", None
    return group.group(1), field.group(1), _field_type_of(block)


def _insert_sorted(content: str, marker: str, rows, key_of, render, label,
                   slice_name: str, find_close) -> tuple[str, int]:
    """Insert every row whose key (`row[:2]`) is not already in the slice.

    Both slices are sorted by that key (`tests::overlay::table_is_sorted`), so
    a row goes in front of the first entry that sorts after it. A new tail
    goes before the slice's closing `];`, which `find_close` locates in the
    final block: that block holds the last OverlayEntry, `];` and all of
    OVERLAY_HANDLE_TABLE, so OVERLAY_TABLE's close is the first `\\n];` there
    and OVERLAY_HANDLE_TABLE's, the file's last slice, the last.
    """
    added = 0
    for row in rows:
        blocks = content.split(marker)
        keys = [key_of(block) for block in blocks[1:]]
        if row[:2] in keys:
            continue
        target = next((i for i, k in enumerate(keys) if k > row[:2]), None)
        if target is None:
            last = blocks[-1]
            close = find_close(last, "\n];")
            if close == -1:
                raise SystemExit(
                    f"{TABLE_RS}: {label(*row)} would append but the "
                    f"{slice_name} closing '];' could not be located."
                )
            content = (marker.join(blocks[:-1]) + marker + last[:close]
                       + render(*row) + last[close:])
        else:
            # blocks[target + 1] is the block for `keys[target]`.
            content = (marker.join(blocks[: target + 1]) + render(*row)
                       + marker + marker.join(blocks[target + 1:]))
        added += 1
    return content, added


def _entry_key(block: str) -> tuple[str, str]:
    g, f = GROUP_RE.search(block), FIELD_RE.search(block)
    return (g.group(1), f.group(1)) if g and f else ("", "")


def apply_additions(content: str) -> tuple[str, int]:
    """Insert every entry in `ADDITIONS` that is not already present."""
    return _insert_sorted(
        content, "    OverlayEntry {", ADDITIONS, _entry_key,
        lambda group, field, ftype: (
            "    OverlayEntry {\n"
            f'        group_path: "{group}",\n'
            f'        field_name: "{field}",\n'
            f"        field_type: {ftype},\n"
            "    },\n"
        ),
        lambda group, field, _ftype: f"{group}/{field}", "OVERLAY_TABLE", str.find)


def _type_token_swap(old: str, new: str) -> tuple[str, str]:
    """The one token to swap in an entry's type so that `old` becomes `new`.

    A bare `old` is one token. Two braced types must differ in exactly one
    word, and only that word is swapped, because rustfmt breaks a braced type
    over lines; anything else is refused rather than guessed.
    """
    old_n, new_n = normalize_type(old), normalize_type(new)
    if "{" not in old_n:
        return old_n, new_n
    old_words, new_words = old_n.split(), new_n.split()
    differ = [(a.rstrip(","), b.rstrip(","))
              for a, b in zip(old_words, new_words) if a != b]
    if len(old_words) != len(new_words) or len(differ) != 1:
        raise SystemExit(
            f"{TABLE_RS}: {old} -> {new} is not a one-word change, so it "
            f"cannot be rewritten in place in both layouts."
        )
    return differ[0]


class Retype(NamedTuple):
    """One correction: `old` -> `new` on the entries it keys.

    `groups` is a predicate on an entry's OWN group path. `expected` is how
    many entries a freshly generated table must change, and `label` names the
    rule when another count is refused; `None` makes any count acceptable.
    """
    groups: Callable[[str], bool]
    field: str
    old: str
    new: str
    expected: int | None = None
    label: str = ""


def exact_retype(group: str, field: str, old: str, new: str,
                 expected: int = 1) -> Retype:
    """A rule for the one entry keyed EXACTLY `(group, field)`."""
    return Retype(lambda own_group: own_group == group, field, old, new,
                  expected, f"{group}/{field} {old} -> {new}")


def retype(content: str, rule: Retype) -> tuple[str, int]:
    """Apply one `Retype` to every entry it keys.

    The key is each block's OWN entry (`_own_entry`): its group satisfies
    `rule.groups`, its field is `rule.field` and its whole type is `rule.old`.
    Only the differing token is swapped (`_type_token_swap`), inside that
    entry's `field_type`, so both layouts are rewritten and a corrected table
    changes nothing (count 0). An entry counts only once its type reads `new`,
    or the run fails: a count that moves without the change is worse than
    none. Any count but 0 or `rule.expected` means the key matched something
    it was not written for, which fails too.
    """
    token, replacement = _type_token_swap(rule.old, rule.new)
    blocks = content.split("    OverlayEntry {")
    changed = 0
    for i, block in enumerate(blocks[1:], 1):
        own_group, own_field, own_type = _own_entry(block)
        if own_field != rule.field or not rule.groups(own_group):
            continue
        if own_type != normalize_type(rule.old):
            continue
        at = block.find(TYPE_MARKER)
        rewritten = block[:at] + block[at:].replace(token, replacement, 1)
        if _field_type_of(rewritten) != normalize_type(rule.new):
            raise SystemExit(
                f"{TABLE_RS}: {own_group}/{rule.field} {rule.old} -> {rule.new} "
                f"did not rewrite the entry's own type."
            )
        blocks[i] = rewritten
        changed += 1
    if rule.expected is not None and changed not in (0, rule.expected):
        raise SystemExit(
            f"{TABLE_RS}: {rule.label} changed {changed} entries, expected "
            f"{rule.expected} (or 0 on a corrected table)."
        )
    return "    OverlayEntry {".join(blocks), changed


#: The weapon half of the "215"/"216" correction, which EXPECTED cannot list:
#: its rule discovers the targets in the table (every `/Game/Equippables/`
#: group carrying one of these fields, 18 today), so the expectation is
#: derived from the same table, and finding no weapon group at all is a
#: failure, not "nothing to do".
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

    EVERY hit must carry the required type: groups match by substring
    (`"SmokeScreen"` reaches `SmokeScreenManager` too), so an `any()` would pass
    a wrong-typed entry beside a right-typed sibling.
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

#: The slices declare their length, so an addition must resync it or the file
#: does not build.
TABLE_LEN_RE = re.compile(r"(pub static OVERLAY_TABLE: \[OverlayEntry; )(\d+)(\])")


def _resync_len(content: str, len_re: re.Pattern, n: int, slice_name: str) -> str:
    """Rewrite one slice's declared length to `n`, the entries present."""
    new_content, hits = len_re.subn(
        lambda m: f"{m.group(1)}{n}{m.group(3)}", content, count=1
    )
    if hits != 1:
        raise SystemExit(
            f"{TABLE_RS}: expected exactly one {slice_name} length declaration, "
            f"found {hits}."
        )
    return new_content


def resync_table_len(content: str) -> str:
    """Rewrite the declared `OVERLAY_TABLE` length to the entries present."""
    return _resync_len(content, TABLE_LEN_RE, sum(1 for _ in parse_entries(content)),
                       "OVERLAY_TABLE")


HANDLE_NUM_RE = re.compile(r"handle: (\d+)")


def parse_handle_entries(content: str):
    """Yield (group_path, handle, field_name) for every OverlayHandleEntry."""
    for block in content.split("    OverlayHandleEntry {")[1:]:
        g = GROUP_RE.search(block)
        h = HANDLE_NUM_RE.search(block)
        f = FIELD_RE.search(block)
        if g and h and f:
            yield g.group(1), int(h.group(1)), f.group(1)


def _handle_key(block: str) -> tuple[str, int]:
    g, h = GROUP_RE.search(block), HANDLE_NUM_RE.search(block)
    return (g.group(1), int(h.group(1))) if g and h else ("", -1)


def apply_handle_additions(content: str) -> tuple[str, int]:
    """`apply_additions` for OVERLAY_HANDLE_TABLE, keyed on `(group_path,
    handle)`: insert every HANDLE_ADDITIONS entry not already present."""
    return _insert_sorted(
        content, "    OverlayHandleEntry {", HANDLE_ADDITIONS, _handle_key,
        lambda group, handle, field: (
            "    OverlayHandleEntry {\n"
            f'        group_path: "{group}",\n'
            f"        handle: {handle},\n"
            f'        field_name: "{field}",\n'
            "    },\n"
        ),
        lambda group, handle, _field: f"{group}/handle {handle}",
        "OVERLAY_HANDLE_TABLE", str.rfind)


HANDLE_TABLE_LEN_RE = re.compile(
    r"(pub static OVERLAY_HANDLE_TABLE: \[OverlayHandleEntry; )(\d+)(\])"
)


def resync_handle_table_len(content: str) -> str:
    """Rewrite the declared `OVERLAY_HANDLE_TABLE` length to the entries present."""
    return _resync_len(content, HANDLE_TABLE_LEN_RE,
                       sum(1 for _ in parse_handle_entries(content)),
                       "OVERLAY_HANDLE_TABLE")


def rewrite_header(content: str) -> tuple[str, tuple[str, ...]]:
    """Recount the table and rewrite both generated header lines.

    extract_descriptors.py counts the descriptors it read; the corrections
    and ADDITIONS then change those counts. Counted from the parsed entries,
    not by substring, so `FieldType::RawPayload` cannot count as `Raw`.
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


def _ends_with(suffixes: str | tuple[str, ...]) -> Callable[[str], bool]:
    """A `Retype.groups` predicate: the group path ends with `suffixes`."""
    return lambda group: group.endswith(suffixes)


def _rep_movement(rotation: str, location: str) -> str:
    return (f"FieldType::RepMovement {{ rotation: RotatorQuantization::{rotation}, "
            f"location: VectorQuantization::{location} }}")


GAME_OBJECT_ROTATOR_RETYPE = Retype(
    lambda group: group in GAME_OBJECT_BYTE_ROTATOR_GROUPS, "ReplicatedMovement",
    _rep_movement("ShortComponents", "RoundWholeNumber"),
    _rep_movement("ByteComponents", "RoundWholeNumber"),
    len(GAME_OBJECT_BYTE_ROTATOR_GROUPS), "the AGameObject rotator pass")

#: Every correction `main` applies, in order. A suffix key matches a group path
#: ending with it; only the rules with an `expected` count check theirs.
RETYPES = [
    # Float -> Double: these time fields are 64 bits on the wire.
    *[Retype(_ends_with(suffix), field, "FieldType::Float", "FieldType::Double")
      for suffix, field in (
          ("TimedBomb.TimedBomb_C", "TimeRemainingToExplode"),
          ("TimedBomb.TimedBomb_C", "DefuseProgress"),
          ("Comp_Ability_CooldownComponent_C", "StartTimeStamp"),
          ("Comp_Ability_CooldownComponent_C", "CooldownSeconds"),
      )],

    # Skip -> Float: every payload of this timestamp is a 32-bit Float (the
    # figures are at its pawn ADDITIONS).
    Retype(lambda _group: True, "ReplayLastTransformUpdateTimeStamp",
           "FieldType::Skip", "FieldType::Float"),

    # "215"/"216" -> EnumRemainingBits in every group, weapons included. They
    # are field NAMES, not handles: the decimal spelling of hardcoded FName
    # indices the replay never resolves to text (read_fname in vrf-schema), on
    # 370 groups. "215" carries 1710918439 and "216" 4109980037 on every group
    # that sends them, decoding or not, at a uniform 3 bits, and decode to 3
    # and 1 without exception: one checksum is one property, so the weapons'
    # Raw was an unverified guess. It cost 48,010 rows over 20 replays with no
    # error, because a name hit wins in resolve_in_group before the checksum
    # fallback. Two rules: the descriptors spell the non-weapon groups Int32
    # and the weapon groups Raw, and a rule keyed on the wrong old type
    # rewrites nothing and moves no counter.
    *[Retype(lambda group: group.startswith(WEAPON_GROUP_MARKER), field,
             "FieldType::Raw", ACTOR_BOOKKEEPING_TYPE)
      for field in ACTOR_BOOKKEEPING_FIELDS],
    *[Retype(_ends_with(NON_WEAPON_215_216), field, "FieldType::Int32",
             ACTOR_BOOKKEEPING_TYPE)
      for field in ACTOR_BOOKKEEPING_FIELDS],

    # Byte rotator components for the Astra smoke-screen projectiles.
    # ProjectileSmokeScreenDescriptor.cs calls a bare `.ReplicatedMovement()`
    # (ShortComponents, 16 bits per axis) where every other projectile
    # descriptor -- FlameWall, MageWall, NeonTunnel, EquippablePickup -- passes
    # ByteComponents. On release-13.01 these payloads are 113-124 bits and a
    # Short read runs off the end: 137 EOF failures on one replay, all from
    # this group.
    Retype(lambda group: "SmokeScreen" in group, "ReplicatedMovement",
           _rep_movement("ShortComponents", "RoundWholeNumber"),
           _rep_movement("ByteComponents", "RoundWholeNumber")),

    # Two-decimal location for Gekko's Wingman pawn; the generator gives every
    # RepMovement entry whole units, the level of the other 24 classes
    # (extract_descriptors.py REP_MOVEMENT_LOCATION). Joined to its
    # actors.parquet spawn (the first ReplicatedMovement row at the `open`
    # time_ms, same channel), the packed integer is 100x the coordinate on all
    # 932 actors in 1,018 replays over 15 builds (2026-09-28, 259ed10): median
    # |packed| / |spawn| 100.000, p1..p99 99.998 to 100.001, every component
    # within 0.0502 of spawn after /100; components are 17-22 bits where a
    # whole-unit class on the same maps needs 10-15. The C# reader's fixed
    # VectorNetQuantize100 is right for this class. Every other measured Pawn
    # class also packs two decimals, but none is in the table: a pattern, not
    # evidence for an unmeasured class.
    Retype(lambda group: group == SEEKER_NADE_GROUP, "ReplicatedMovement",
           _rep_movement("ShortComponents", "RoundWholeNumber"),
           _rep_movement("ShortComponents", "RoundTwoDecimals")),

    # Byte rotator components for five AGameObject classes; the evidence is on
    # GAME_OBJECT_BYTE_ROTATOR_GROUPS.
    GAME_OBJECT_ROTATOR_RETYPE,

    # No DamagedBone rule: decode_fname honours the isHardcoded bit, so the
    # declared FName decodes, 9-bit hardcoded names included (177 of 581
    # payloads). A former FName -> Raw rule here shipped mojibake for all 581
    # payloads (22 bone names: Head 69, Spine4 51, L_Shoulder 46, ...).

    # EnumByte -> FName for AresEquippableDataTracker.OriginalBuyerTeam.
    # AdditionalComponentDescriptors.cs declares EnumByte ("a small team
    # enum") and says to fall back if that fails, and it does: no row is 8
    # bits. The 97- and 105-bit payloads (248 on 02d4d478) are an inline FName:
    #
    #   97 bits  = 1 isHardcoded (0) + i32 length 4 + "Red\0"  + i32 number 0
    #   105 bits = 1 isHardcoded (0) + i32 length 5 + "Blue\0" + i32 number 0
    #
    # 2026-09-28, every row of the group over the 1,018 replays at 259ed10
    # (fields + checkpoint_fields): 748,381 rows (636,009 main, 112,372
    # checkpoint) in every replay; checksum 255019476 is this field's alone,
    # and the group declares nothing else. Exactly two payloads exist -- 97 bits
    # 08000000a4cac8000000000000 and 105 bits 0a00000084d8eaca000000000000 --
    # and an independent FName reader consumes all 748,381 rows exactly, to
    # "Red" (376,779) and "Blue" (371,602), isHardcoded 0 and number 0 on
    # every row; decode_fname already reads the byte-identical CombatReport
    # ParticipantTeamName payloads (3.98M rows) as "Red"/"Blue", and no
    # byte-aligned FString fits either width. It is the team NAME as sent,
    # mapped to no side or player; 12.10, 12.11 and 13.00 carry one main row
    # each, all "Blue".
    exact_retype("/Script/ShooterGame.AresEquippableDataTracker", "OriginalBuyerTeam",
                 "FieldType::EnumByte", "FieldType::FName"),

    # Raw -> ObjectNetGuid for TransitionContext: the descriptor's
    # RawPayload("UTransitionContext") states no wire reader. The corpus shows
    # exact IntPacked consumption; resolved non-null IDs name
    # transition-context object groups, and null stays zero. Not every ID is
    # claimed to resolve, and a group/name key cannot impose the measured
    # build, handle or checksum gates.
    Retype(lambda group: group == "/Script/ShooterGame.EquippableStateMachineComponent",
           "TransitionContext", "FieldType::Raw", "FieldType::ObjectNetGuid"),

    # EnumRemainingBits -> EnumByte for AllianceFilter on
    # ReplayEffectComponent:ReplayPlayContinuousEffectAtLocation: table
    # consistency, not a wire mismatch -- the wire agrees with both
    # declarations. AllianceFilter (EAresAlliance) is ONE property, checksum
    # 2270825073, declared `byte AllianceFilter` (EnumByte) by
    # EffectManagerComponent:MulticastPlay{Continuous,OneShot}Effect and
    # `.EnumRemainingBits()` at ReplayPlayContinuousEffectAtLocationParameters.cs:43.
    # extract_checksum_types.py drops a checksum whose donors disagree, so the
    # five RPCs carrying it undeclared stayed untyped:
    # AresEquippable:MulticastPlay{Continuous,OneShot}EffectFromClient under
    # every weapon's _ClassNetCache, ReplayPlayOneShotEffectAtLocation, and both
    # EffectManagerComponent:ReplayRecord*Effect. 2026-09-28, all 1,018 replays
    # at 259ed10 (24 builds, 11.06-13.06), fields and checkpoint_fields rows
    # with that checksum: 16,030,813 rows, every one exactly 3 bits on every
    # build, 0 in checkpoints; the manifests declare it under those eight groups
    # only, always named AllianceFilter; 11,470,565 rows typed through the
    # donors, 4,560,248 raw. Read LSB-first the donors hold {1, 3} and every raw
    # row 3 (AllianceAny), all inside EAresAlliance 0..5; an independent Python
    # decode matched all 11,470,565 typed rows. EnumByte is the stricter reader
    # (decode_byte refuses 0-bit and >8-bit payloads; EnumRemainingBits answers
    # 0 for no bits and reads up to 32), and at 1..8 bits both give the same
    # integer, so this RPC's 3,094,607 existing rows keep their values (none is
    # 0 bits wide). The vendored descriptor stays verbatim. checksum_table.rs
    # learns 2270825073 -> EnumByte only when extract_checksum_types.py runs
    # after this, and
    # `alliance_filter_donors_agree_so_the_checksum_types_the_receivers` in
    # crates/vrf-decode/src/tests/overlay.rs fails until both have run.
    exact_retype(
        "/Script/ShooterGame.ReplayEffectComponent:ReplayPlayContinuousEffectAtLocation",
        "AllianceFilter", "FieldType::EnumRemainingBits", "FieldType::EnumByte"),

    # Raw -> ObjectNetGuid for DeathMontageEffectOverride and
    # DeathMontageEffectOverrideContext on both MulticastNotifyDamage_* RPCs.
    # The descriptors declare both with AddRaw
    # (MulticastNotifyDamagePointParameters.cs:55-56, ...BaseParameters.cs:29-30),
    # an opaque payload like TransitionContext's, and a Raw table entry wins in
    # resolve_entry before a scoped type or the checksum is consulted.
    # 2026-09-28, the 1,018 replays at 259ed10, both fields, both RPCs: 959,445
    # rows each (Point 632,906, Base 326,539), main stream only; the 3 replays
    # without them (12.10, 12.11, 13.00) never declare the fields. One checksum
    # per name corpus-wide (1712763745 / 2397897524), carried by no other
    # property. The handle drifts by build (Point 40-43 / 41-44, Base 32-35 /
    # 33-36): the name keys this, so do not "fix" OVERLAY_HANDLE_TABLE from it.
    # Every row is consumed exactly as IntPacked, and in every build every
    # 8-bit row is 0x00, the null reference, as IntPacked requires.
    # * Override: 8 bits 947,364, 16 bits 12,081. The 12,081 non-zero values
    #   are all odd (static GUIDs), 2,642 distinct, each resolving in the same
    #   export's net_guids to one of 190 `FXC_*_C` finisher kill-effect classes
    #   (FXC_Finisher_Afterglow_Victim_C, ..._Demonstone_..., ...); non-zero only
    #   on events with bDamageKilledTarget and a non-zero Context.
    # * Context: 8 / 16 / 24 bits (897,283 / 62,075 / 87). The 62,162 non-zero
    #   values are all even (dynamic): net_guids resolves none, as for the typed
    #   EventInstigatorPawn on the same events, while actors.parquet resolves
    #   62,162 of 62,162 to a `*_PC_C` player pawn open at the event's time_ms
    #   (EquippableUsed's standard). Non-zero only on killing events; it equals
    #   EventInstigatorPawn on 22,224 of them and Character on 1,537, so it is a
    #   pawn reference and nothing more specific: not "the killer", not "the
    #   victim".
    # Second implementation: validate_type_evidence.py (ObjectNetGuid,
    # checksum-scoped) over the whole corpus for the Override, exit 0 with 0
    # failures; over 5 exports (11.06-13.06) for the Context, exit 0.
    *[exact_retype(f"/Script/ShooterGame.DamageableComponent:{rpc}", field,
                   "FieldType::Raw", "FieldType::ObjectNetGuid")
      for field in ("DeathMontageEffectOverride", "DeathMontageEffectOverrideContext")
      for rpc in ("MulticastNotifyDamage_Base", "MulticastNotifyDamage_Point")],

    # UInt64 -> Int64 for the four EffectID entries; the evidence is on
    # EFFECT_ID_INT64.
    *[exact_retype(group, "EffectID", "FieldType::UInt64", "FieldType::Int64")
      for group, _checksum, _chain in EFFECT_ID_INT64],
]


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="verify without writing")
    return parser.parse_args(argv)


def main():
    check_only = parse_args().check
    content = TABLE_RS.read_text(encoding="utf-8")
    # The FILE, kept for `--check`; `content` becomes the corrected copy.
    on_disk = content
    count = 0

    for rule in RETYPES:
        content, n = retype(content, rule)
        count += n

    # Additions last, so the bucket recount below sees them.
    content, n_added = apply_additions(content)
    count += n_added
    content = resync_table_len(content)

    content, n_handle_added = apply_handle_additions(content)
    count += n_handle_added
    content = resync_handle_table_len(content)

    content, header_lines = rewrite_header(content)

    # The verdict is the end state, not `count` (module docstring). A table can
    # be both DEAD and NEVER CORRECTED, so both sections print.
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
            print("A missing correction means extract_descriptors.py no longer "
                  "emits that entry, emits it at a type no RETYPES rule "
                  "rewrites, or now declares an ADDITION's key at another "
                  "type: update RETYPES, ADDITIONS or EXPECTED. The run order "
                  "cannot cause it; every rule rewrites both layouts.",
                  file=sys.stderr)
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
