//! The overlay table's resolution order, and the hash index's agreement
//! with the binary search it replaced.

use std::collections::BTreeSet;

use crate::checksum_table::CHECKSUM_TYPES;
use crate::decode::{DecodeError, FieldType, decode_field};
use crate::overlay::{
    OverlayEntry, OverlayHandleEntry, OverlayResult, OverlayStats, OverlayTable, apply_overlay,
    apply_overlay_with_handle, canonical_group, group_hash_state, lookup_checksum,
    resolve_field_type, resolve_field_type_with_checksum,
};
use crate::test_bits::BitWriter;
use crate::types::{RotatorQuantization, VectorQuantization};
use crate::{OVERLAY_HANDLE_TABLE, OVERLAY_TABLE};

/// The table production resolves through: every entry and the explicit-handle
/// fallback. One static, so its hash index is built once for the suite.
pub(super) static TABLE: OverlayTable =
    OverlayTable::with_handles(&OVERLAY_TABLE, &OVERLAY_HANDLE_TABLE);

/// The table entry for `(group, field)`, reported at the caller's line.
#[track_caller]
fn assert_typed(group: &str, field: &str, want: Option<FieldType>) {
    assert_eq!(TABLE.lookup(group, field), want, "{group} {field}");
}

/// The checksum table's answer for `checksum`, reported at the caller's line.
#[track_caller]
fn assert_checksum(checksum: u32, want: Option<FieldType>) {
    assert_eq!(lookup_checksum(checksum), want, "{checksum}");
}

/// What [`TABLE`] resolves `field` of `group` to, with no handle.
pub(super) fn resolve(group: &str, field: &str, checksum: Option<u32>) -> Option<FieldType> {
    resolve_field_type_with_checksum(&TABLE, group, Some(field), None, checksum)
}

/// One field through [`TABLE`] as the export path applies it: name, handle,
/// checksum and payload. The overlay declining the field fails the test.
#[track_caller]
pub(super) fn apply_scoped(
    stats: &mut OverlayStats,
    group: &str,
    field: &str,
    handle: u32,
    checksum: u32,
    raw: &[u8],
    bits: u32,
) -> OverlayResult {
    let Some(applied) = crate::apply_overlay_with_checksum(
        &TABLE,
        group,
        group_hash_state(group),
        Some(field),
        handle,
        Some(checksum),
        Some(raw),
        bits,
        stats,
    ) else {
        // In the body, not a closure, so #[track_caller] names the test's line.
        panic!("{group} {field}: the overlay declined it");
    };
    applied
}

const BOMB_GS: &str = "/Game/GameModes/Bomb/BombGameState.BombGameState_C";
const BOMB_PS: &str = "/Game/GameModes/Bomb/BombPlayerState.BombPlayerState_C";
const SWIFT_GS: &str = "/Game/GameModes/_Development/Swiftplay_EndOfRoundCredits\
/Swiftplay_EoRCredits_GameState.Swiftplay_EoRCredits_GameState_C";
const SWIFT_PS: &str = "/Game/GameModes/_Development/Swiftplay_EndOfRoundCredits\
/Swiftplay_EoRCredits_PlayerState.Swiftplay_EoRCredits_PlayerState_C";
const PLAY_CONTINUOUS: &str =
    "/Script/ShooterGame.EffectManagerComponent:MulticastPlayContinuousEffect";
const STOP_CONTINUOUS: &str =
    "/Script/ShooterGame.EffectManagerComponent:MulticastStopContinuousEffect";
const FROM_CLIENT: &str =
    "/Script/ShooterGame.AresEquippable:MulticastPlayContinuousEffectFromClient";
const REPLAY_AT_LOCATION: &str =
    "/Script/ShooterGame.ReplayEffectComponent:ReplayPlayContinuousEffectAtLocation";
const ONE_SHOT_AT_LOCATION: &str =
    "/Script/ShooterGame.LocationalEffectManagerComponent:ClientPlayOneShotEffectAtLocation";
const DAMAGE_BASE: &str = "/Script/ShooterGame.DamageableComponent:MulticastNotifyDamage_Base";
const DAMAGE_POINT: &str = "/Script/ShooterGame.DamageableComponent:MulticastNotifyDamage_Point";
const FORCE_REMOVE: &str =
    "/Script/ShooterGame.ForceModuleManagerComponent:NetMulticastRemoveForceModule";
const HAWK: &str = "/Game/Characters/Guide/S0/Ability_E/\
Projectile_Guide_E_HawkFlash.Projectile_Guide_E_HawkFlash_C";
const CAGE_TRAP_Q: &str =
    "/Game/Characters/Gumshoe/S0/Ability_Q/Ability_Gumshoe_Q_CageTrap.Ability_Gumshoe_Q_CageTrap_C";
const CAGE_TRAP_4: &str =
    "/Game/Characters/Gumshoe/S0/Ability_4/Ability_Gumshoe_4_CageTrap.Ability_Gumshoe_4_CageTrap_C";

/// One name, two properties: the byte-shaped `B` and the 32-bit `B`
/// (checksum 943211507, the second word of the player-state GUID -- see
/// `player_state_guid_parts_are_scoped_uint32`) resolve by checksum alone.
#[test]
fn scoped_types_require_the_exact_group_name_and_checksum() {
    for group in [BOMB_PS, SWIFT_PS] {
        assert_eq!(resolve(group, "B", Some(379198054)), Some(FieldType::Byte));
        assert_eq!(
            resolve(group, "B", Some(943211507)),
            Some(FieldType::UInt32)
        );
        for checksum in [None, Some(1)] {
            assert_eq!(resolve(group, "B", checksum), None);
        }
    }
    for (group, name) in [("/Unobserved", "B"), (BOMB_PS, "Unobserved")] {
        assert_eq!(resolve(group, name, Some(379198054)), None);
    }
}

#[test]
fn scoped_bytes_decode_exactly_and_reject_a_wider_payload() {
    let mut stats = OverlayStats::default();
    let value = apply_scoped(&mut stats, BOMB_PS, "B", 39, 379198054, &[255], 8);
    assert_eq!(value.value_i64, Some(255));
    let rejected = apply_scoped(&mut stats, BOMB_PS, "B", 39, 379198054, &[255, 0, 0, 0], 32);
    assert_eq!(rejected.value_i64, None);
    assert_eq!(stats.decoded_ok, 1);
    assert_eq!(stats.decoded_err, 1);
}

const CLAY_SATCHEL_ABILITY: &str =
    "/Game/Characters/Clay/S0/Ability_Q/Ability_Clay_Q_Satchel.Ability_Clay_Q_Satchel_C";
const CLAY_SATCHEL: &str = "/Game/Characters/Clay/S0/Ability_Q/\
Projectile_Clay_Q_Satchel_Arming.Projectile_Clay_Q_Satchel_Arming_C";
const CLAY_BOOMBOT: &str =
    "/Game/Characters/Clay/S0/Ability_E/Pawn_Clay_E_Boomba.Pawn_Clay_E_Boomba_C";
const CLAY_ROCKET: &str =
    "/Game/Characters/Clay/S0/Ability_X/Projectile_Clay_X_Rocket.Projectile_Clay_X_Rocket_C";
const FORCE_APPLY: &str =
    "/Script/ShooterGame.ForceModuleManagerComponent:NetMulticastApplyForceModule";
const REPLICATED_MOVEMENT_CHECKSUM: u32 = 2_749_104_612;

/// Every scoped identity must be reachable. The table and its aliases resolve
/// a name before `SCOPED_TYPES`, so a scoped entry whose group has a same-name
/// table entry is never read, silently (five force-module entries were, once
/// the table typed those `NetMulticastApplyForceModule` parameters by name).
/// Resolved without a checksum, a name the engine-reference fallback covers is
/// told apart by also resolving on a group no table declares. The scoped lookup
/// is a binary search on `(name, group, checksum)`, so the order is asserted too
/// and every entry must resolve to its own type at its own identity.
#[test]
fn no_scoped_identity_is_shadowed_by_the_table() {
    let by_name = |group, name| resolve(group, name, None);
    let scoped = &crate::scoped_types::SCOPED_TYPES;
    let shadowed: Vec<(&str, &str, u32)> = scoped
        .iter()
        .filter(|(name, group, _, _)| {
            by_name(group, name).is_some() && by_name("/Unobserved", name).is_none()
        })
        .map(|(name, group, checksum, _)| (*group, *name, *checksum))
        .collect();
    assert!(
        shadowed.is_empty(),
        "scoped entries the table resolves first, so never read: {shadowed:?}"
    );

    for pair in scoped.windows(2) {
        let (a, b) = (
            (pair[0].0, pair[0].1, pair[0].2),
            (pair[1].0, pair[1].1, pair[1].2),
        );
        assert!(a < b, "SCOPED_TYPES not strictly sorted: {a:?} then {b:?}");
    }
    for &(name, group, checksum, field_type) in scoped {
        assert_eq!(
            resolve(group, name, Some(checksum)),
            Some(field_type),
            "unreachable scoped identity {name} / {group} / {checksum}"
        );
    }
}

/// Upstream 8b7afcb's Raze fields are typed by exact identity only: the
/// checksum is part of the key, and another group or checksum resolves to
/// nothing rather than borrowing the type.
#[test]
fn raze_scoped_identities_require_their_exact_checksum() {
    assert_eq!(
        resolve(
            CLAY_SATCHEL_ABILITY,
            "CosmeticRandomSeed",
            Some(2_863_861_815)
        ),
        Some(FieldType::Int32)
    );
    assert_eq!(
        resolve(CLAY_SATCHEL_ABILITY, "CosmeticRandomSeed", Some(1)),
        None
    );
    assert_eq!(
        resolve(CLAY_SATCHEL_ABILITY, "CosmeticRandomSeed", None),
        None
    );
    assert_eq!(
        resolve("/Unobserved", "CosmeticRandomSeed", Some(2_863_861_815)),
        None
    );
    // Of 8b7afcb's seven force-module identities only `Source` and `Duration`
    // stay scoped: `Module`, `ModuleType`, `Character`, `NetTimestamp` and
    // `RespawnNumber` are typed by name in the table, which resolves first.
    assert_eq!(
        resolve(FORCE_APPLY, "Duration", Some(1_815_021_954)),
        Some(FieldType::Float)
    );
    assert_eq!(
        resolve(FORCE_APPLY, "Source", Some(1_966_913_909)),
        Some(FieldType::ObjectNetGuid)
    );
    assert_eq!(resolve(FORCE_APPLY, "Duration", Some(1)), None);
    // The Remove RPC is another group, which the scoped `Duration` does not
    // reach (its `ModuleType` is typed through the table entry's checksum).
    assert_eq!(resolve(FORCE_REMOVE, "Duration", Some(1_815_021_954)), None);
    assert_eq!(
        resolve(
            "/Script/ShooterGame.ShooterCharacter:ClientResetRemoteMovementPrediction",
            "isPossess",
            Some(3_522_099_335)
        ),
        Some(FieldType::Bool)
    );
}

/// The Boom Bot replicates short rotation components and a two-decimal
/// location (spawn-verified). Raze's byte-rotation projectiles stay untyped
/// until each has a spawn-join entry in `REP_MOVEMENT_LOCATION_EVIDENCE`
/// (docs/UPSTREAM_RAZE_WARDEN.md).
#[test]
fn boombot_movement_is_short_and_byte_rotation_projectiles_stay_raw() {
    assert_eq!(
        resolve(
            CLAY_BOOMBOT,
            "ReplicatedMovement",
            Some(REPLICATED_MOVEMENT_CHECKSUM)
        ),
        Some(FieldType::RepMovement {
            rotation: crate::types::RotatorQuantization::ShortComponents,
            location: VectorQuantization::RoundTwoDecimals,
        })
    );
    for group in [CLAY_SATCHEL, CLAY_ROCKET] {
        assert_eq!(
            resolve(
                group,
                "ReplicatedMovement",
                Some(REPLICATED_MOVEMENT_CHECKSUM)
            ),
            None,
            "{group}"
        );
    }
}

/// Upstream's own recorded payloads from replay 42e03082 (not in the local
/// corpus), decoded through the scoped identities: exact widths, exact values.
#[test]
fn upstream_recorded_raze_payloads_decode_through_their_scoped_identities() {
    let mut stats = OverlayStats::default();
    let seed = apply_scoped(
        &mut stats,
        CLAY_SATCHEL_ABILITY,
        "CosmeticRandomSeed",
        56,
        2_863_861_815,
        &[0xE1, 0xE9, 0x4B, 0x40],
        32,
    );
    assert_eq!(seed.value_i64, Some(1_078_716_897));
    let offset = apply_scoped(
        &mut stats,
        CLAY_SATCHEL,
        "LocationOffset",
        5,
        111_823_753,
        &[0xD3, 0x20, 0x67, 0xB7, 0xA8, 0x97, 0x48, 0x00],
        64,
    );
    assert_eq!(offset.value_str.as_deref(), Some("(-782.71,-1366.59,5.8)"));
    let rotation = apply_scoped(
        &mut stats,
        CLAY_SATCHEL,
        "RotationOffset",
        7,
        1_473_289_183,
        &[0x01, 0x80, 0xEE, 0x27, 0xF7, 0xFF, 0x07],
        51,
    );
    assert_eq!(
        rotation.value_str.as_deref(),
        Some("rot(90,284.03503,359.989)")
    );
    // `ModuleType` resolves through its table entry (EnumByte); upstream's
    // recorded 3-bit payload must still read 2.
    let module_type = apply_scoped(
        &mut stats,
        FORCE_APPLY,
        "ModuleType",
        1,
        3_263_282_135,
        &[0x02],
        3,
    );
    assert_eq!(module_type.value_i64, Some(2));
    assert_eq!(stats.decoded_ok, 4);
    // Upstream's truncation case: one byte cannot carry the rotator its
    // presence bits announce. Rejected and counted, not truncated.
    let truncated = apply_scoped(
        &mut stats,
        CLAY_SATCHEL,
        "RotationOffset",
        7,
        1_473_289_183,
        &[0x01],
        8,
    );
    assert_eq!(truncated.value_str, None);
    assert_eq!(stats.decoded_err, 1);
}

/// The four 32-bit words `A`/`B`/`C`/`D` on the player state are one FGuid of
/// `uint32` members, so `UInt32`, scoped per group and checksum from
/// `tools/fixtures/scoped_type_evidence.json`. The scope is load-bearing: the
/// handles drift by build (A is 219, 204 or 207, and 207 is D's on
/// 11.11-12.05), `B` also names nine byte-shaped properties and `A` an 8-bit
/// one (1036865991), and scoped entries never follow the Swiftplay alias.
#[test]
fn player_state_guid_parts_are_scoped_uint32() {
    let parts = [
        ("A", 988_169_428),
        ("B", 943_211_507),
        ("C", 965_590_766),
        ("D", 1_032_080_829),
    ];
    for group in [BOMB_PS, SWIFT_PS] {
        for (field, checksum) in parts {
            assert_eq!(
                resolve(group, field, Some(checksum)),
                Some(FieldType::UInt32),
                "{group} {field}"
            );
            for other in [None, Some(checksum ^ 1)] {
                assert_eq!(resolve(group, field, other), None, "{field} {other:?}");
            }
        }
    }
    for (field, checksum) in parts {
        assert_eq!(resolve("/Unobserved", field, Some(checksum)), None);
        assert_eq!(resolve(BOMB_GS, field, Some(checksum)), None);
    }
    assert_eq!(resolve(BOMB_PS, "A", Some(1_036_865_991)), None);
}

/// The first overlay use of `UInt32`, end to end (scoped lookup, `decode_u32`,
/// `value_i64`), with a high-bit word staying positive: 0xe28c69d7 is a real
/// `D` value from the 2026-09-28 audit, which `Int32` would read as -494114345,
/// a plausible wrong number a width check alone accepts. Any other width is a
/// decode error, not a truncated or padded value.
#[test]
fn player_state_guid_parts_decode_unsigned_and_exactly() {
    let mut stats = OverlayStats::default();
    let mut apply = |group: &str, raw: &[u8], bits| {
        apply_scoped(&mut stats, group, "D", 210, 1_032_080_829, raw, bits).value_i64
    };
    for group in [BOMB_PS, SWIFT_PS] {
        assert_eq!(
            apply(group, &[0xd7, 0x69, 0x8c, 0xe2], 32),
            Some(3_800_852_951)
        );
        assert_eq!(apply(group, &[0xd7, 0x69, 0x8c], 24), None, "{group}");
        assert_eq!(
            apply(group, &[0xd7, 0x69, 0x8c, 0xe2, 0x00], 40),
            None,
            "{group}"
        );
    }
    assert_eq!((stats.decoded_ok, stats.decoded_err), (2, 4));
}

#[test]
fn bomb_player_crosshair_fields_are_typed_without_the_colliding_b() {
    for field in [
        "bHasOutline",
        "bDisplayCenterDot",
        "bShowLines",
        "bUseAdvancedOptions",
    ] {
        assert_typed(BOMB_PS, field, Some(FieldType::Bool));
    }
    for field in ["OutlineThickness", "CenterDotSize", "LineLength", "Opacity"] {
        assert_typed(BOMB_PS, field, Some(FieldType::Float));
    }
    for field in ["G", "R"] {
        assert_typed(BOMB_PS, field, Some(FieldType::Byte));
    }
    assert_typed(BOMB_PS, "ProfileName", Some(FieldType::FString));
    assert_typed(BOMB_PS, "CompetitiveTier", Some(FieldType::Int32));
    assert_eq!(
        TABLE.lookup(BOMB_PS, "B"),
        None,
        "B has both 8- and 32-bit wire fields"
    );
}

#[test]
fn tidal_wave_rpc_parameters_are_typed() {
    const CHUNK: &str = "/Game/Characters/Mage/S0/Ability_X/GameObject_Mage_X_TidalWave_Chunk.GameObject_Mage_X_TidalWave_Chunk_C";
    let initialize = format!("{CHUNK}:MulticastInitialize");
    for (field, field_type) in [
        ("ChunkIndex", FieldType::Int32),
        ("ChunkSpacing", FieldType::Float),
        ("Velocity In", FieldType::Double),
        ("PreviousChunk", FieldType::ObjectNetGuid),
    ] {
        assert_typed(&initialize, field, Some(field_type));
    }
    assert_typed(
        &format!("{CHUNK}:MulticastWallStartLinger"),
        "FinalEndpointReached",
        Some(FieldType::Bool),
    );
}

/// Only the two Swiftplay siblings map to their Bomb twins; a Bomb class and
/// the suffixed forms stay as they are (see `GROUP_ALIASES`). Pinned so a later
/// "make it consistent" edit has to argue with a test.
#[test]
fn canonical_group_maps_only_the_swiftplay_siblings() {
    assert_eq!(canonical_group(SWIFT_GS), BOMB_GS);
    assert_eq!(canonical_group(SWIFT_PS), BOMB_PS);
    for path in [BOMB_GS, BOMB_PS, "/Game/Whatever.Whatever_C"] {
        assert_eq!(canonical_group(path), path);
    }
    for suffix in ["_ClassNetCache", ":SomeFunction"] {
        let path = format!("{SWIFT_GS}{suffix}");
        assert_eq!(canonical_group(&path), path, "{suffix} should not alias");
    }
}

/// The point of the alias: a Swiftplay field resolves to the type its Bomb
/// twin has. `ChosenCeremonyForRound` is the live case -- it is in the table
/// exactly once, under the Bomb game state.
#[test]
fn a_swiftplay_field_resolves_through_its_bomb_twin() {
    for field in ["ChosenCeremonyForRound", "RoundResults", "BombState"] {
        let bomb = resolve_field_type(&TABLE, BOMB_GS, Some(field), None);
        let swift = resolve_field_type(&TABLE, SWIFT_GS, Some(field), None);
        assert_eq!(swift, bomb, "{field} must resolve the same on both classes");
        assert!(bomb.is_some(), "{field} should be in the table at all");
    }
}

/// The alias must not invent types. A name in neither class stays unresolved.
#[test]
fn the_alias_does_not_invent_a_type() {
    assert_eq!(
        resolve_field_type(&TABLE, SWIFT_GS, Some("NoSuchFieldAnywhere"), None),
        None,
    );
    // and an unaliased group gains nothing
    assert_eq!(
        resolve_field_type(
            &TABLE,
            "/Game/Nope.Nope_C",
            Some("ChosenCeremonyForRound"),
            None
        ),
        None,
    );
}

/// The generators' contract: both tables strictly sorted by their key, which
/// the reference binary searches need, and no key twice (a duplicate could
/// resolve differently in the hash index and in the search).
#[test]
fn table_is_sorted() {
    let unsorted = OVERLAY_TABLE
        .windows(2)
        .position(|w| (w[0].group_path, w[0].field_name) >= (w[1].group_path, w[1].field_name));
    assert_eq!(
        unsorted, None,
        "OVERLAY_TABLE not strictly sorted at that index"
    );
    let unsorted = OVERLAY_HANDLE_TABLE
        .windows(2)
        .position(|w| (w[0].group_path, w[0].handle) >= (w[1].group_path, w[1].handle));
    assert_eq!(
        unsorted, None,
        "OVERLAY_HANDLE_TABLE not strictly sorted at that index"
    );
}

/// Live per-player credits under `MoneyManagementComponent`, a group no C#
/// descriptor declares, read as Int32. Evidence: the MoneyManagementComponent
/// entries in tools/apply_type_corrections.py.
#[test]
fn money_management_economy_is_typed() {
    let group = "/Script/ShooterGame.MoneyManagementComponent";
    assert_typed(group, "Money", Some(FieldType::Int32));
    assert_typed(group, "StartOfRoundMoney", Some(FieldType::Int32));
    assert_typed(group, "TotalMoneyGranted", Some(FieldType::Int32));
}

/// Concussion state lives on one shared component on every player character,
/// so one group types it for every agent: Start/EndTime Float, Level Double.
/// Evidence: the Comp_Actor_Concussable entries in
/// tools/apply_type_corrections.py.
#[test]
fn concussion_fields_are_typed() {
    let group = "/Game/Characters/Components/Comp_Actor_Concussable\
.Comp_Actor_Concussable_C";
    assert_typed(group, "ConcussStartTime", Some(FieldType::Float));
    assert_typed(group, "ConcussEndTime", Some(FieldType::Float));
    assert_typed(group, "ConcussLevel", Some(FieldType::Double));
}

/// `Comp_AbilityFuelSystem`, one shared per-ability component: CurrentFuel is a
/// Double, IsFuelDraining a Bool. Evidence: the Comp_AbilityFuelSystem entries
/// in tools/apply_type_corrections.py.
#[test]
fn ability_fuel_fields_are_typed() {
    let group = "/Game/Characters/Components/Comp_AbilityFuelSystem\
.Comp_AbilityFuelSystem_C";
    assert_typed(group, "CurrentFuel", Some(FieldType::Double));
    assert_typed(group, "IsFuelDraining", Some(FieldType::Bool));
}

/// `Ping` on BombPlayerState is a 16-bit LE unsigned integer that behaves like
/// latency in milliseconds (min ~6, p50 ~15, p90 ~19, max ~473 on 02d4d478),
/// typed `SerializedInt{65536}` (exactly 16 bits LSB-first). No descriptor
/// declares it; the encoding is docs/archive/PROJECT_STATUS.md 18-A/18-B, and
/// why it is typed despite that section's "not typed" is the
/// `BombPlayerState.Ping` ADDITIONS note in tools/apply_type_corrections.py.
#[test]
fn ping_latency_is_typed() {
    assert_typed(
        BOMB_PS,
        "Ping",
        Some(FieldType::SerializedInt { max: 65536 }),
    );
}

#[test]
fn equippable_used_is_an_object_net_guid() {
    // The C# descriptor attaches a custom decoder (DamageParameters.cs:51 ->
    // ValorantPayloadDecoders.Equippable) that is exactly
    // archive.ReadIntPacked(), our ObjectNetGuid. tools/extract_descriptors.py
    // types it from the decoder's name (PAYLOAD_DECODER_TYPES), and
    // tools/apply_type_corrections.py EXPECTED only verifies it. Left Raw, the
    // adapter guessed a fixed 16-bit LE integer that was never a valid NetGUID.
    for group in [DAMAGE_BASE, DAMAGE_POINT] {
        assert_eq!(
            TABLE.lookup(group, "EquippableUsed"),
            Some(FieldType::ObjectNetGuid),
            "EquippableUsed must decode as a net GUID in {group}",
        );
    }
}

/// Both damage RPCs' death-montage parameters are IntPacked GUIDs, not the
/// opaque payload the descriptor's `AddRaw` declares. A `Raw` entry wins before
/// the checksum and the scoped types, so this is a table correction, and it
/// must not reach `...IsQueued` (a Bool). Evidence: the
/// DeathMontageEffectOverride(Context) rules in tools/apply_type_corrections.py.
#[test]
fn the_death_montage_parameters_are_object_net_guids() {
    for group in [DAMAGE_BASE, DAMAGE_POINT] {
        for field in [
            "DeathMontageEffectOverride",
            "DeathMontageEffectOverrideContext",
        ] {
            assert_typed(group, field, Some(FieldType::ObjectNetGuid));
        }
        assert_eq!(
            TABLE.lookup(group, "bDeathMontageEffectOverrideIsQueued"),
            Some(FieldType::Bool),
            "the Bool sibling is untouched in {group}"
        );
    }
    assert_checksum(1712763745, Some(FieldType::ObjectNetGuid));
    assert_checksum(2397897524, Some(FieldType::ObjectNetGuid));
}

/// `AresEquippableDataTracker.OriginalBuyerTeam` is an inline FName (`Red` or
/// `Blue`), which the descriptor's EnumByte cannot read. Evidence: the
/// OriginalBuyerTeam rule in tools/apply_type_corrections.py.
#[test]
fn original_buyer_team_is_an_fname() {
    assert_typed(
        "/Script/ShooterGame.AresEquippableDataTracker",
        "OriginalBuyerTeam",
        Some(FieldType::FName),
    );
    assert_checksum(255019476, Some(FieldType::FName));
}

#[test]
fn transition_context_is_an_object_net_guid() {
    assert_typed(
        "/Script/ShooterGame.EquippableStateMachineComponent",
        "TransitionContext",
        Some(FieldType::ObjectNetGuid),
    );
}

#[test]
fn hawk_flash_post_control_velocity_is_vector_double_only_on_its_exact_group() {
    let group = HAWK;
    assert_typed(group, "PostControlVelocity", Some(FieldType::VectorDouble));
    assert_ne!(
        TABLE.lookup(
            "/Script/ShooterGame.EquippableStateMachineComponent",
            "PostControlVelocity"
        ),
        Some(FieldType::VectorDouble)
    );
}

/// HawkFlash's `ReplicatedMovement` reads byte rotator components and its
/// `Banking` a Double, on that exact group only; the location is whole units
/// (REP_MOVEMENT_LOCATION_EVIDENCE). Evidence: the HawkFlash ReplicatedMovement
/// and Banking entries in tools/apply_type_corrections.py.
#[test]
fn hawk_flash_movement_and_banking_are_typed_on_their_exact_group() {
    assert_typed(
        HAWK,
        "ReplicatedMovement",
        Some(FieldType::RepMovement {
            rotation: RotatorQuantization::ByteComponents,
            location: VectorQuantization::RoundWholeNumber,
        }),
    );
    assert_typed(HAWK, "Banking", Some(FieldType::Double));
    assert_checksum(677106858, Some(FieldType::Double));
}

/// Cypher's trapwire and cage classes were renamed in 13.01: the five
/// descriptor-typed fields follow on the new paths, the old paths keep their
/// entries, and only `Deployed`'s checksum is learned, so a future path still
/// resolves. Evidence: the Gumshoe TripWire/CageTrap entries in
/// tools/apply_type_corrections.py.
#[test]
fn cypher_trap_fields_follow_the_13_01_rename() {
    const OLD_E: &str = "/Game/Characters/Gumshoe/S0/Ability_E/";
    const NEW_4: &str = "/Game/Characters/Gumshoe/S0/Ability_4/";
    for wire in [
        "GameObject_Gumshoe_{}_TripWire.GameObject_Gumshoe_{}_TripWire_C",
        "GameObject_Gumshoe_{}_TripWire_SecondWire.GameObject_Gumshoe_{}_TripWire_SecondWire_C",
    ] {
        let old = format!("{OLD_E}{}", wire.replace("{}", "E"));
        let new = format!("{NEW_4}{}", wire.replace("{}", "4"));
        assert_typed(&old, "Deployed", Some(FieldType::Bool));
        assert_typed(&new, "Deployed", Some(FieldType::Bool));
    }
    for (old, new) in [
        (
            "/Game/Characters/Gumshoe/S0/Ability_E/Ability_Gumshoe_E_TripWire.Ability_Gumshoe_E_TripWire_C",
            "/Game/Characters/Gumshoe/S0/Ability_4/Ability_Gumshoe_4_TripWire.Ability_Gumshoe_4_TripWire_C",
        ),
        (CAGE_TRAP_4, CAGE_TRAP_Q),
    ] {
        for group in [old, new] {
            assert_typed(group, "CreatedByCharacter", Some(FieldType::ObjectNetGuid));
        }
    }
    for group in [CAGE_TRAP_4, CAGE_TRAP_Q] {
        assert_typed(
            group,
            "RelativeScale3D",
            Some(FieldType::VectorNetQuantize { scale: 100 }),
        );
    }
    assert_checksum(3902815170, Some(FieldType::Bool));
    assert_checksum(2035145197, None);
    assert_checksum(1992268157, None);
    // A future path for the same wire resolves through the checksum alone.
    assert_eq!(
        resolve(
            "/Game/Characters/Gumshoe/S0/Ability_C/GameObject_Gumshoe_C_TripWire.GameObject_Gumshoe_C_TripWire_C",
            "Deployed",
            Some(3902815170)
        ),
        Some(FieldType::Bool)
    );
}

/// The five AGameObject smoke and zone classes read byte rotator components;
/// the only table entry left with short ones is Gekko's Wingman, an
/// AShooterCharacter pawn. None of the five ever replicates a rotation, so the
/// wire cannot choose the width (13-J, 16-D); the game's 13.06 class data does.
/// Evidence and bound: `GAME_OBJECT_BYTE_ROTATOR_GROUPS` in
/// `apply_type_corrections.py`.
#[test]
fn only_the_seeker_nade_keeps_short_rotator_components() {
    const GAME_OBJECTS: [&str; 5] = [
        "/Game/Characters/Mage/S0/Ability_E/GameObject_Mage_E_WorldSmoke.GameObject_Mage_E_WorldSmoke_C",
        "/Game/Characters/Smonk/S0/Ability_E/MapTargetSmoke/GameObject_Smonk_NewSmoke.GameObject_Smonk_NewSmoke_C",
        "/Game/Characters/Smonk/S0/Ability_E/MapTargetSmoke/GameObject_Smonk_NewSmoke_PDS.GameObject_Smonk_NewSmoke_PDS_C",
        "/Game/Characters/Smonk/S0/Ability_Q/DebuffKnife/DecayLauncher/GameObject_Smonk_Q_DecayExplosion.GameObject_Smonk_Q_DecayExplosion_C",
        "/Game/Characters/Wraith/S0/Ability_4/Zone_Wraith_4_Smoke.Zone_Wraith_4_Smoke_C",
    ];
    const SEEKER_NADE: &str = "/Game/Characters/AggroBot/S0/Ability_Q/Pawn_Aggrobot_SeekerNade.Pawn_Aggrobot_SeekerNade_C";
    for group in GAME_OBJECTS {
        assert_typed(
            group,
            "ReplicatedMovement",
            Some(FieldType::RepMovement {
                rotation: RotatorQuantization::ByteComponents,
                location: VectorQuantization::RoundWholeNumber,
            }),
        );
    }
    let short: Vec<&str> = OVERLAY_TABLE
        .iter()
        .filter(|e| {
            matches!(
                e.field_type,
                FieldType::RepMovement {
                    rotation: RotatorQuantization::ShortComponents,
                    ..
                }
            )
        })
        .map(|e| e.group_path)
        .collect();
    assert_eq!(short, [SEEKER_NADE]);
    // SeekerNade's short, two-decimal donor still disagrees with the byte,
    // whole-unit ones, so the checksum stays dropped.
    assert_checksum(REPLICATED_MOVEMENT_CHECKSUM, None);
}

#[test]
fn damage_geometry_fields_are_quantized_vectors() {
    // Typed from the C# decoders' names, like EquippableUsed. Scales are the
    // C# call sites: VectorNetQuantize = 1, VectorNetQuantize100 = 100,
    // VectorNetQuantizeNormal = unit vector. Evidence: the damage-vector
    // EXPECTED rows in tools/apply_type_corrections.py.
    const BASE: &str = DAMAGE_BASE;
    const POINT: &str = DAMAGE_POINT;

    // DamageOrigin is on the shared base; the impact geometry only exists
    // for point damage, which is why the two groups differ here.
    let expected = [
        (
            BASE,
            "DamageOrigin",
            FieldType::VectorNetQuantize { scale: 100 },
        ),
        (
            POINT,
            "DamageOrigin",
            FieldType::VectorNetQuantize { scale: 100 },
        ),
        (
            POINT,
            "DamageImpactLocation",
            FieldType::VectorNetQuantize { scale: 1 },
        ),
        (
            POINT,
            "DamageImpactBoneRelativeLocation",
            FieldType::VectorNetQuantize { scale: 1 },
        ),
        (POINT, "DamageDirection", FieldType::VectorNetQuantizeNormal),
        (
            POINT,
            "DamageImpactNormal",
            FieldType::VectorNetQuantizeNormal,
        ),
    ];

    for (group, field, want) in expected {
        assert_typed(group, field, Some(want));
    }
}

#[test]
fn overlay_falls_back_to_the_b_prefixed_boolean_name() {
    // The descriptors bind by handle and spell some bools with a `b` the
    // replay omits (`bDeathMontageEffectOverrideIsQueued`): on 02d4d478, 632
    // rows resolve only this way, 581 on `_Point` and 51 on `_Base`
    // (docs/OVERLAY_RESOLUTION.md "The b-prefix fallback").
    let entries: &[OverlayEntry] = &[OverlayEntry {
        group_path: "/test",
        field_name: "bIsQueued",
        field_type: FieldType::Bool,
    }];
    let table = OverlayTable::new(entries);
    let mut stats = OverlayStats::default();
    let data = [0x01u8];
    let result = apply_overlay(
        &table,
        "/test",
        group_hash_state("/test"),
        Some("IsQueued"),
        Some(&data),
        1,
        &mut stats,
    );
    assert_eq!(
        result.and_then(|r| r.value_bool),
        Some(true),
        "the unprefixed wire name must resolve to the b-prefixed entry",
    );
}

#[test]
fn overlay_falls_back_to_an_explicit_property_handle_when_the_wire_name_differs() {
    const GROUP: &str = REPLAY_AT_LOCATION;
    let entries: &[OverlayEntry] = &[OverlayEntry {
        group_path: GROUP,
        field_name: "Location",
        field_type: FieldType::VectorDouble,
    }];
    let handle_entries: &[OverlayHandleEntry] = &[OverlayHandleEntry {
        group_path: GROUP,
        handle: 26,
        field_name: "Location",
    }];
    let table = OverlayTable::with_handles(entries, handle_entries);
    let mut data = Vec::new();
    data.extend_from_slice(&1.25f64.to_le_bytes());
    data.extend_from_slice(&(-2.5f64).to_le_bytes());
    data.extend_from_slice(&3.75f64.to_le_bytes());
    let mut stats = OverlayStats::default();

    let result = apply_overlay_with_handle(
        &table,
        GROUP,
        group_hash_state(GROUP),
        Some("248"),
        26,
        Some(&data),
        192,
        &mut stats,
    );

    assert_eq!(
        result.and_then(|value| value.value_str),
        Some("(1.25,-2.5,3.75)".to_owned()),
    );
    assert_eq!(stats.decoded_ok, 1);
    assert_eq!(stats.not_in_table, 0);
}

#[test]
fn overlay_uses_an_explicit_property_handle_when_the_wire_name_is_missing() {
    let entries: &[OverlayEntry] = &[OverlayEntry {
        group_path: "/test",
        field_name: "Health",
        field_type: FieldType::Int32,
    }];
    let handle_entries: &[OverlayHandleEntry] = &[OverlayHandleEntry {
        group_path: "/test",
        handle: 9,
        field_name: "Health",
    }];
    let table = OverlayTable::with_handles(entries, handle_entries);
    let mut stats = OverlayStats::default();
    let data = 100i32.to_le_bytes();

    let result = apply_overlay_with_handle(
        &table,
        "/test",
        group_hash_state("/test"),
        None,
        9,
        Some(&data),
        32,
        &mut stats,
    );

    assert_eq!(result.and_then(|value| value.value_i64), Some(100));
    assert_eq!(stats.decoded_ok, 1);
    assert_eq!(stats.no_field_name, 0);
    assert_eq!(stats.not_in_table, 0);
}

#[test]
fn overlay_keeps_direct_name_lookup_ahead_of_the_handle_fallback() {
    let entries: &[OverlayEntry] = &[
        OverlayEntry {
            group_path: "/test",
            field_name: "DeclaredName",
            field_type: FieldType::Int32,
        },
        OverlayEntry {
            group_path: "/test",
            field_name: "RuntimeName",
            field_type: FieldType::Bool,
        },
    ];
    let handle_entries: &[OverlayHandleEntry] = &[OverlayHandleEntry {
        group_path: "/test",
        handle: 9,
        field_name: "DeclaredName",
    }];
    let table = OverlayTable::with_handles(entries, handle_entries);
    let mut stats = OverlayStats::default();

    let result = apply_overlay_with_handle(
        &table,
        "/test",
        group_hash_state("/test"),
        Some("RuntimeName"),
        9,
        Some(&[1]),
        1,
        &mut stats,
    );

    assert_eq!(result.and_then(|value| value.value_bool), Some(true));
    assert_eq!(stats.decoded_ok, 1);
}

/// A REAL, different name declared at a handle is not typed through the
/// descriptor's stale mapping: a patch moving `OldField: Int32` to `NewField`
/// (a `Float`) would read `1.0f32` as 1065353216 with `Decode errors` at zero
/// (docs/OVERLAY_RESOLUTION.md "Fail-closed on a handle conflict").
#[test]
fn a_conflicting_declared_name_refuses_the_stale_handle_mapping() {
    let entries: &[OverlayEntry] = &[OverlayEntry {
        group_path: "/test",
        field_name: "OldField",
        field_type: FieldType::Int32,
    }];
    let handle_entries: &[OverlayHandleEntry] = &[OverlayHandleEntry {
        group_path: "/test",
        handle: 7,
        field_name: "OldField",
    }];
    let table = OverlayTable::with_handles(entries, handle_entries);
    let mut stats = OverlayStats::default();
    let data = 1.0f32.to_le_bytes();

    let result = apply_overlay_with_handle(
        &table,
        "/test",
        group_hash_state("/test"),
        Some("NewField"),
        7,
        Some(&data),
        32,
        &mut stats,
    );

    assert!(result.is_none(), "must not type a conflicting declaration");
    assert_eq!(stats.decoded_ok, 0, "1065353216 must not be reported");
    assert_eq!(stats.handle_conflicts_refused, 1, "{stats:?}");
    assert_eq!(stats.not_in_table, 1, "the field is untyped, not failed");
}

/// The refusal must NOT fire on a bare decimal wire name: `"248"`, an
/// unresolved hardcoded FName index, declares nothing, and the handle fallback
/// is the only thing that can type such a field.
#[test]
fn a_bare_fname_index_still_reaches_the_handle_fallback() {
    let entries: &[OverlayEntry] = &[OverlayEntry {
        group_path: "/test",
        field_name: "Health",
        field_type: FieldType::Int32,
    }];
    let handle_entries: &[OverlayHandleEntry] = &[OverlayHandleEntry {
        group_path: "/test",
        handle: 7,
        field_name: "Health",
    }];
    let table = OverlayTable::with_handles(entries, handle_entries);
    let data = 100i32.to_le_bytes();

    for wire_name in ["248", "0"] {
        let mut stats = OverlayStats::default();
        let result = apply_overlay_with_handle(
            &table,
            "/test",
            group_hash_state("/test"),
            Some(wire_name),
            7,
            Some(&data),
            32,
            &mut stats,
        );
        assert_eq!(
            result.and_then(|v| v.value_i64),
            Some(100),
            "wire name {wire_name}"
        );
        assert_eq!(stats.handle_conflicts_refused, 0, "wire name {wire_name}");
    }
}

/// The `b`-prefix probe resolves before the handle fallback, so a `bFoo`/`Foo`
/// spelling difference never reaches the refusal.
#[test]
fn a_b_prefixed_spelling_difference_is_not_treated_as_a_conflict() {
    let entries: &[OverlayEntry] = &[OverlayEntry {
        group_path: "/test",
        field_name: "bIsQueued",
        field_type: FieldType::Bool,
    }];
    let handle_entries: &[OverlayHandleEntry] = &[OverlayHandleEntry {
        group_path: "/test",
        handle: 7,
        field_name: "bIsQueued",
    }];
    let table = OverlayTable::with_handles(entries, handle_entries);
    let mut stats = OverlayStats::default();

    let result = apply_overlay_with_handle(
        &table,
        "/test",
        group_hash_state("/test"),
        Some("IsQueued"),
        7,
        Some(&[1]),
        1,
        &mut stats,
    );

    assert_eq!(result.and_then(|v| v.value_bool), Some(true));
    assert_eq!(stats.handle_conflicts_refused, 0, "{stats:?}");
}

#[test]
fn apply_overlay_decodes_int32() {
    let entries: &[OverlayEntry] = &[OverlayEntry {
        group_path: "/test",
        field_name: "Health",
        field_type: FieldType::Int32,
    }];
    let table = OverlayTable::new(entries);
    let mut stats = OverlayStats::default();
    let data = 100i32.to_le_bytes();
    let result = apply_overlay(
        &table,
        "/test",
        group_hash_state("/test"),
        Some("Health"),
        Some(&data),
        32,
        &mut stats,
    );
    assert!(result.is_some());
    let r = result.unwrap();
    assert_eq!(r.value_i64, Some(100));
    assert_eq!(stats.decoded_ok, 1);
}

#[test]
fn apply_overlay_returns_none_for_no_field_name() {
    let entries: &[OverlayEntry] = &[OverlayEntry {
        group_path: "/test",
        field_name: "Health",
        field_type: FieldType::Int32,
    }];
    let table = OverlayTable::new(entries);
    let mut stats = OverlayStats::default();
    let result = apply_overlay(
        &table,
        "/test",
        group_hash_state("/test"),
        None,
        Some(&[0; 4]),
        32,
        &mut stats,
    );
    assert!(result.is_none());
    assert_eq!(stats.no_field_name, 1);
}

/// A zero-bit payload is the value 0 for `EnumRemainingBits` (see
/// `scalar::decode_enum_remaining_bits`) and a `ZeroBits` failure for every
/// other type; a field with no payload takes the same arm, and is `ZeroBits`
/// for every type once it claims a nonzero width.
#[test]
fn a_zero_bit_payload_is_zero_only_for_enum_remaining_bits() {
    static ENTRIES: [OverlayEntry; 2] = [
        OverlayEntry {
            group_path: "/test",
            field_name: "Empty",
            field_type: FieldType::Int32,
        },
        OverlayEntry {
            group_path: "/test",
            field_name: "Zero",
            field_type: FieldType::EnumRemainingBits,
        },
    ];
    let table = OverlayTable::new(&ENTRIES);
    let empty: &[u8] = &[];
    for (field, raw_bits, bit_count, want) in [
        ("Zero", Some(empty), 0, Some(0)),
        ("Zero", None, 0, Some(0)),
        ("Zero", None, 8, None),
        ("Empty", Some(empty), 0, None),
        ("Empty", None, 8, None),
    ] {
        let case = format!("{field} {raw_bits:?} {bit_count}");
        let mut stats = OverlayStats::default();
        let r = apply_overlay(
            &table,
            "/test",
            group_hash_state("/test"),
            Some(field),
            raw_bits,
            bit_count,
            &mut stats,
        )
        .expect("a typed field is attempted");
        let columns = (r.value_i64, r.value_f64, r.value_bool, r.value_str);
        assert_eq!(columns, (want, None, None, None), "{case}");
        let kinds: Vec<String> = stats
            .error_report
            .top_n(usize::MAX)
            .iter()
            .map(|row| row.error_kind.to_string())
            .collect();
        if want.is_some() {
            assert_eq!((stats.decoded_ok, stats.decoded_err), (1, 0), "{case}");
            assert!(kinds.is_empty(), "{case}: {kinds:?}");
        } else {
            assert_eq!((stats.decoded_ok, stats.decoded_err), (0, 1), "{case}");
            assert_eq!(kinds, ["ZeroBits"], "{case}");
        }
    }
}

/// The error report's `kind` is the only column that says WHY a field failed,
/// so each failure must print the label of its own cause, asserted on the
/// printed label an operator reads. Every `DecodeError` variant the overlay can
/// meet has a case, checked by the test itself: `decode_field` names the
/// variant each fixture fails with, and the variants reached must equal the
/// list the wildcard-free match below is generated from.
#[test]
fn the_error_report_names_the_cause_of_each_failure() {
    const fn entry(field_name: &'static str, field_type: FieldType) -> OverlayEntry {
        OverlayEntry {
            group_path: "/test",
            field_name,
            field_type,
        }
    }
    /// A field payload: its bytes and its bit count.
    type Payload = (Vec<u8>, u32);
    // Generated from one list, so the list and the match cannot disagree.
    macro_rules! variants {
        ($($variant:ident),+ $(,)?) => {
            (
                [$(stringify!($variant)),+],
                |err: &DecodeError| -> &'static str {
                    match err {
                        $(DecodeError::$variant { .. } => stringify!($variant),)+
                    }
                },
            )
        };
    }
    let (every_variant, variant_of) = variants!(
        BitIo,
        NotFullyConsumed,
        RawOrSkip,
        UnsignedOverflow,
        NonFiniteComponent,
        InvalidQuantizationScale,
        InvalidFNameNumber,
        UnsupportedTextHistory,
        ByteArrayLengthCapExceeded,
        FTextTree,
    );
    static ENTRIES: [OverlayEntry; 15] = [
        entry("BadTextBool", FieldType::FTextTree),
        entry("BadUtf8", FieldType::FString),
        entry("ByteArrayOverCap", FieldType::ByteArray { max_bytes: 1 }),
        entry(
            "ByteArrayOverlongPrefix",
            FieldType::ByteArray { max_bytes: 8 },
        ),
        entry("LongInt", FieldType::Int32),
        entry("MistypedFText", FieldType::FText),
        entry("NaNVector", FieldType::VectorNetQuantize { scale: 100 }),
        entry("NegativeFNameNumber", FieldType::FName),
        entry("OverlongPrefix", FieldType::FString),
        entry("RunawayIntPacked", FieldType::ObjectNetGuid),
        entry("ShortInt", FieldType::Int32),
        entry("U64PastI64", FieldType::UInt64),
        entry("UnseenTextHistory", FieldType::FTextTree),
        entry(
            "ZeroQuantizeScale",
            FieldType::VectorNetQuantize { scale: 0 },
        ),
        entry("ZeroSerializedIntMax", FieldType::SerializedInt { max: 0 }),
    ];
    // An inline FName (hardcoded bit clear), "Source" with its null, then
    // instance number -1.
    let mut fname = BitWriter::new();
    fname.bits(0, 1).i32(7);
    for byte in b"Source\0" {
        fname.bits(u64::from(*byte), 8);
    }
    let fname = fname.i32(-1).finish();
    // A 7-bit SerializedInt(128) header of 0 -- no component bits, no extra
    // info -- takes the raw-f32 fallback, and the first word is a NaN.
    let nan_vector = BitWriter::new()
        .bits(0, 7)
        .bits(0x7fc0_0000, 32)
        .bits(u64::from(1.0f32.to_bits()), 32)
        .bits(u64::from(2.0f32.to_bits()), 32)
        .finish();
    let bytes = |data: &[u8]| (data.to_vec(), data.len() as u32 * 8);
    // (field, payload, variant decode_field fails with, printed label)
    let cases: Vec<(&str, Payload, &str, &str)> = vec![
        // The payload is shorter than the type: a real EOF.
        ("ShortInt", bytes(&[0x01, 0x00]), "BitIo", "EOF"),
        // The type finished with bits to spare.
        (
            "LongInt",
            bytes(&[0x01, 0, 0, 0, 0]),
            "NotFullyConsumed",
            "Residual",
        ),
        // Length 3, then three bytes that are not UTF-8: consumed exactly,
        // nothing ran out.
        (
            "BadUtf8",
            bytes(&[0x03, 0, 0, 0, 0xff, 0xfe, 0x00]),
            "BitIo",
            "Malformed",
        ),
        // A length prefix of 100 with one byte behind it.
        (
            "OverlongPrefix",
            bytes(&[0x64, 0, 0, 0, 0x41]),
            "BitIo",
            "Malformed",
        ),
        // The same cause in a byte array: a count of 4, within the cap of 8,
        // with one byte behind it.
        (
            "ByteArrayOverlongPrefix",
            bytes(&[0x08, 0xaa]),
            "BitIo",
            "Malformed",
        ),
        // Three declared where the table allows one, and no byte behind
        // them: the cap is checked first, so the table constant is what the
        // report names.
        (
            "ByteArrayOverCap",
            bytes(&[0x06]),
            "ByteArrayLengthCapExceeded",
            "Rejected",
        ),
        // Five IntPacked bytes that never clear the continuation bit.
        ("RunawayIntPacked", bytes(&[0xff; 5]), "BitIo", "Malformed"),
        // Two bytes declared where the table allows one: the constant needs
        // raising, not a layout.
        (
            "ByteArrayOverCap",
            bytes(&[0x04, 0xaa, 0xbb]),
            "ByteArrayLengthCapExceeded",
            "Rejected",
        ),
        // Reads fine; the value has no i64 spelling.
        (
            "U64PastI64",
            bytes(&[0, 0, 0, 0, 0, 0, 0, 0x80]),
            "UnsignedOverflow",
            "Rejected",
        ),
        // A table parameter no value can be read against. No bit is wrong.
        ("ZeroSerializedIntMax", bytes(&[0x00]), "BitIo", "Rejected"),
        // The same for a quantized vector's divisor.
        (
            "ZeroQuantizeScale",
            bytes(&[0x00]),
            "InvalidQuantizationScale",
            "Rejected",
        ),
        // The 8 bits after the first 33 are not the selector this reader
        // knows (5). A mistyped FText -- this repo's costliest bug shape --
        // lands here, so it must not read as leftover bits.
        (
            "MistypedFText",
            (vec![0; 6], 41),
            "UnsupportedTextHistory",
            "Rejected",
        ),
        // The full-tree reader's own causes, sorted the same way: a history
        // it has never seen laid out is refused ...
        (
            "UnseenTextHistory",
            bytes(&[0, 0, 0, 0, 5]),
            "FTextTree",
            "Rejected",
        ),
        // ... and an archive bool that is neither 0 nor 1 breaks the framing.
        (
            "BadTextBool",
            bytes(&[1, 0, 0, 0, 4, 3, 0, 0, 0, 0, 0, 0, 0xf0, 0x3f, 2, 0, 0, 0]),
            "FTextTree",
            "Malformed",
        ),
        // Read in full; the value has no JSON spelling.
        ("NaNVector", nan_vector, "NonFiniteComponent", "Rejected"),
        // Read in full; a negative instance number has no display spelling.
        (
            "NegativeFNameNumber",
            fname,
            "InvalidFNameNumber",
            "Rejected",
        ),
    ];
    let table = OverlayTable::new(&ENTRIES);
    let mut printed = Vec::new();
    for (field, (data, bits), variant, _) in &cases {
        let field_type = ENTRIES
            .iter()
            .find(|entry| entry.field_name == *field)
            .map(|entry| entry.field_type)
            .unwrap_or_else(|| panic!("{field}: no entry"));
        let err = decode_field(field_type, data, *bits)
            .expect_err(&format!("{field}: the fixture must not decode"));
        assert_eq!(variant_of(&err), *variant, "{field}: {err:?}");

        let mut stats = OverlayStats::default();
        let result = apply_overlay(
            &table,
            "/test",
            group_hash_state("/test"),
            Some(field),
            Some(data),
            *bits,
            &mut stats,
        );
        assert!(
            result.is_some_and(|r| r.value_i64.is_none()
                && r.value_f64.is_none()
                && r.value_bool.is_none()
                && r.value_str.is_none()),
            "{field}: must fail to decode"
        );
        assert_eq!(stats.decoded_err, 1, "{field}");
        let rows = stats.error_report.top_n(2);
        assert_eq!(rows.len(), 1, "{field}: one bucket");
        printed.push((*field, rows[0].error_kind.to_string()));
    }
    let wanted: Vec<(&str, String)> = cases
        .iter()
        .map(|(field, _, _, want)| (*field, (*want).to_owned()))
        .collect();
    assert_eq!(printed, wanted);

    // `Raw` and `Skip` return before `decode_field` is reached, so its
    // `RawOrSkip` arm cannot fire from the overlay and has no case.
    let reached: BTreeSet<&str> = cases.iter().map(|(_, _, variant, _)| *variant).collect();
    let expected: BTreeSet<&str> = every_variant
        .into_iter()
        .filter(|variant| *variant != "RawOrSkip")
        .collect();
    assert_eq!(
        reached, expected,
        "every DecodeError the overlay can meet needs a case"
    );
}

/// A byte takes its width from the payload, not a fixed 8 (see `decode_byte`).
#[test]
fn byte_takes_its_width_from_the_payload() {
    use crate::decode::{DecodedValue, FieldType, decode_field};

    // 5 significant bits holding 9 (0b01001), padded to one byte.
    let data = [0b0000_1001u8];
    for width in [1u32, 3, 5, 8] {
        let v = decode_field(FieldType::EnumByte, &data, width)
            .unwrap_or_else(|e| panic!("width {width} should decode: {e:?}"));
        let mask = ((1u16 << width) - 1) as u8;
        let expected = i64::from(0b0000_1001u8 & mask);
        assert_eq!(v, DecodedValue::I64(expected), "width {width}");
    }
}

/// A payload wider than a byte is not a byte field. Truncating to the low 8
/// bits would emit a plausible wrong number, so it is reported instead.
#[test]
fn byte_rejects_payloads_wider_than_eight_bits() {
    use crate::decode::{FieldType, decode_field};

    let data = [0xFFu8, 0xFF];
    // 12 bits declared: the nominal 8-bit read leaves 4 unconsumed, which
    // decode_field turns into an error rather than a truncated value.
    assert!(decode_field(FieldType::Byte, &data, 12).is_err());
}

/// The hash index must answer exactly what the binary search answered, on
/// every key in the generated table and on keys that are not in it. A wrong
/// overlay type moves NO counter, so only this equivalence would catch an
/// index that quietly disagrees on a handful of entries.
#[test]
fn the_hash_index_answers_exactly_what_the_binary_search_answered() {
    for entry in &OVERLAY_TABLE {
        assert_eq!(
            TABLE.lookup(entry.group_path, entry.field_name),
            TABLE.lookup_by_binary_search(entry.group_path, entry.field_name),
            "direct lookup disagrees for {}::{}",
            entry.group_path,
            entry.field_name,
        );

        // The `b`-prefix fallback, asked the way the overlay asks it.
        for probe in [
            entry.field_name,
            entry
                .field_name
                .strip_prefix('b')
                .unwrap_or(entry.field_name),
        ] {
            assert_eq!(
                TABLE.lookup_b_prefixed(entry.group_path, probe),
                TABLE.lookup_b_prefixed_by_binary_search(entry.group_path, probe),
                "b-prefixed lookup disagrees for {}::b{}",
                entry.group_path,
                probe,
            );
        }

        // Keys that must miss: a real group with a name nothing declares,
        // and a real name under a group that does not carry it.
        for (group, name) in [
            (entry.group_path, "NoSuchFieldNameAnywhere"),
            ("/Game/NoSuchGroupPathAnywhere", entry.field_name),
            (entry.group_path, ""),
        ] {
            assert_eq!(
                TABLE.lookup(group, name),
                TABLE.lookup_by_binary_search(group, name),
                "miss disagrees for {group}::{name}",
            );
            assert_eq!(
                TABLE.lookup_b_prefixed(group, name),
                TABLE.lookup_b_prefixed_by_binary_search(group, name),
                "b-prefixed miss disagrees for {group}::b{name}",
            );
        }
    }
}

/// Same equivalence for the handle fallback table, including handles that are
/// not declared for a group that is.
#[test]
fn the_handle_index_answers_exactly_what_the_binary_search_answered() {
    for entry in &OVERLAY_HANDLE_TABLE {
        for handle in [entry.handle, entry.handle.wrapping_add(1000), u32::MAX, 0] {
            assert_eq!(
                TABLE.lookup_handle(entry.group_path, handle),
                TABLE.lookup_handle_by_binary_search(entry.group_path, handle),
                "handle lookup disagrees for {}::{handle}",
                entry.group_path,
            );
        }
        assert_eq!(
            TABLE.lookup_handle("/Game/NoSuchGroupPathAnywhere", entry.handle),
            None,
        );
    }
}

/// `BlindManagerComponent.LongestActiveBlindDuration` is a 32-bit float giving
/// the longest active flash-blind duration in seconds (0.0..2.1 on observed
/// data). Common to all player characters. Typed as Float.
#[test]
fn blind_duration_is_typed() {
    assert_typed(
        "/Script/ShooterGame.BlindManagerComponent",
        "LongestActiveBlindDuration",
        Some(FieldType::Float),
    );
}

/// The `MulticastNotifyHeal` / `MulticastNotifyOverhealDecay` parameters
/// resolve under their colon-group paths by bare name (the EquippableUsed
/// shape): `HealTaken` a Float heal magnitude, `DecayApplied` a Float overheal
/// decay per tick. Evidence: the HealTaken and DecayApplied entries in
/// tools/apply_type_corrections.py.
#[test]
fn heal_and_overheal_decay_scalars_are_typed() {
    assert_typed(HEAL_PARAMS, "HealTaken", Some(FieldType::Float));
    assert_typed(DECAY_PARAMS, "DecayApplied", Some(FieldType::Float));
}

/// `PlayerScoreComponent.Score`, the per-player cumulative combat score, reads
/// as Int32. Evidence: the PlayerScoreComponent/Score entry in
/// tools/apply_type_corrections.py.
#[test]
fn player_score_is_typed() {
    assert_typed(
        "/Script/ShooterGame.PlayerScoreComponent",
        "Score",
        Some(FieldType::Int32),
    );
}

/// The scoreboard's authoritative cumulative K/D/A counters, Int32; untyped,
/// consumers would rebuild them from lossy kill RPCs. Evidence: the
/// BasicCombatStatsComponent entries in tools/apply_type_corrections.py.
#[test]
fn basic_combat_stats_are_typed() {
    let group = "/Script/ShooterGame.BasicCombatStatsComponent";
    for field in ["AggregateKills", "AggregateDeaths", "AggregateAssists"] {
        assert_typed(group, field, Some(FieldType::Int32));
    }
}

/// `ZoomMultiplierComponent`'s five FOV-transition fields read as Float.
/// `SourceZoomLevel`/`TargetZoomLevel` stay untyped: the rows that are not the
/// 0xFFFFFFFF sentinel are 0.0. Evidence: the ZoomMultiplierComponent entries
/// and the DELIBERATELY NOT ADDED list in tools/apply_type_corrections.py.
#[test]
fn zoom_multiplier_fov_fields_are_typed() {
    let group = "/Script/ShooterGame.ZoomMultiplierComponent";
    assert_typed(group, "SourceFov", Some(FieldType::Float));
    assert_typed(group, "TargetFov", Some(FieldType::Float));
    assert_typed(group, "SourceFov1P", Some(FieldType::Float));
    assert_typed(group, "TargetFov1P", Some(FieldType::Float));
    assert_typed(group, "TotalTransitionTimeDuration", Some(FieldType::Float));
}

/// `UsableComponent` (spike plant/defuse, ultimate orbs, doors): on a bomb
/// replay `HighestProgress` is 32 bits on ~12k rows and `bIsActive` one 0x01
/// bit on ~150 rows. Evidence for the Float ramp and the Bool flag: the
/// UsableComponent entries in tools/apply_type_corrections.py.
#[test]
fn usable_component_interaction_is_typed() {
    let group = "/Script/ShooterGame.UsableComponent";
    assert_typed(group, "HighestProgress", Some(FieldType::Float));
    assert_typed(group, "bIsActive", Some(FieldType::Bool));
}

/// The bare `MagazineAmmo` and `ReserveAmmo` groups (every row handle 2, no
/// name) are remapped to `AmmoComponent` by the leaf remap in vrfkit's
/// `sink/paths.rs`. This pins the destination: that group declares handle 2 as
/// `AuthResourceAmount`, an Int32.
#[test]
fn the_ammo_component_declares_the_handle_the_bare_groups_land_on() {
    const GROUP: &str = "/Script/ShooterGame.AmmoComponent";
    assert_typed(GROUP, "AuthResourceAmount", Some(FieldType::Int32));

    let mut stats = OverlayStats::default();
    let data = 12i32.to_le_bytes();
    let result = apply_overlay_with_handle(
        &TABLE,
        GROUP,
        group_hash_state(GROUP),
        Some("AuthResourceAmount"),
        2,
        Some(&data),
        32,
        &mut stats,
    );
    assert_eq!(result.and_then(|v| v.value_i64), Some(12));
    assert_eq!(stats.decoded_ok, 1);
    assert_eq!(stats.not_in_table, 0);
}

/// `FiniteSpeedMovementComponent.MaximumRange`, a projectile's travel limit in
/// Unreal units, reads as Float; `bIsActive` stays untyped. Evidence: the
/// MaximumRange entry and the DELIBERATELY NOT ADDED list in
/// tools/apply_type_corrections.py.
#[test]
fn finite_speed_movement_max_range_is_typed() {
    assert_typed(
        "/Script/ShooterGame.FiniteSpeedMovementComponent",
        "MaximumRange",
        Some(FieldType::Float),
    );
}

/// The four engine object references resolve by name once the table has
/// missed on the group and its alias (see `ENGINE_OBJECT_REFS`).
#[test]
fn the_engine_fallback_covers_every_one_of_the_four_names() {
    const BOMB_EQUIPPABLE: &str = "/Game/Equippables/Bomb/BombEquippable.BombEquippable_C";
    assert_eq!(
        TABLE.lookup(BOMB_EQUIPPABLE, "Owner"),
        None,
        "not in the table"
    );
    for group in ["/Game/NeverSeen.NeverSeen_C", BOMB_EQUIPPABLE] {
        for name in ["Owner", "Instigator", "AttachParent", "Controller"] {
            assert_eq!(
                resolve_field_type(&TABLE, group, Some(name), None),
                Some(FieldType::ObjectNetGuid),
                "{name} should resolve by name on {group}",
            );
        }
    }
}

/// The fallback is a fixed list, not "anything that looks like a reference".
#[test]
fn the_engine_fallback_does_not_invent_other_names() {
    for name in ["OwnerId", "Owner2", "MyOwner", "Parent", "Target"] {
        assert_eq!(
            resolve_field_type(&TABLE, "/Game/NeverSeen.NeverSeen_C", Some(name), None),
            None,
            "{name} must stay unresolved",
        );
    }
}

/// The 192-bit RPC vectors: Unreal splits an `FTransform` parameter into three
/// double vectors, which no descriptor declares (54,859 raw rows on 02d4d478).
/// `249` here is the transform's `Rotation` FQuat, not a rotator. Evidence: the
/// RPC vector entries in tools/apply_type_corrections.py.
#[test]
fn the_rpc_transform_vectors_are_typed() {
    for (group, field) in [
        (PLAY_CONTINUOUS, "Scale3D"),
        (PLAY_CONTINUOUS, "Translation"),
        (PLAY_CONTINUOUS, "249"),
        (ONE_SHOT_AT_LOCATION, "248"),
        (
            "/Game/GameModes/Components/Comp_BombEvents.Comp_BombEvents_C:BombPlantedRPC",
            "PlantLocation",
        ),
        (
            "/Game/GameModes/Bomb/BombDestination.BombDestination_C:MulticastActivateBombSiteEffects",
            "BombLocation",
        ),
    ] {
        assert_typed(group, field, Some(FieldType::VectorDouble));
    }
}

/// The decode that makes the reading unambiguous: the bytes below are the
/// `Scale3D` payload every row carries, and only a 3 x f64 split reads them as
/// (1,1,1). Six f32s would give (0, 1.875, 0, 1.875, 0, 1.875).
#[test]
fn a_192_bit_rpc_vector_decodes_as_three_doubles() {
    const GROUP: &str = PLAY_CONTINUOUS;
    let mut stats = OverlayStats::default();
    let mut bits = Vec::new();
    for _ in 0..3 {
        bits.extend_from_slice(&1.0f64.to_le_bytes());
    }
    let result = apply_overlay(
        &TABLE,
        GROUP,
        group_hash_state(GROUP),
        Some("Scale3D"),
        Some(&bits),
        192,
        &mut stats,
    );
    assert_eq!(result.and_then(|v| v.value_str).as_deref(), Some("(1,1,1)"));
    assert_eq!(stats.decoded_ok, 1);
    assert_eq!(stats.decoded_err, 0);
}

/// Checksum propagation: the undeclared `PlayerID` of
/// `ReplayPlayerController:ClientReplayReceiveInputEventProcessingCapture`
/// shares checksum 2396673102 with the declared Int32
/// `BombPlayerState_C.PlayerId`, and read as Int32 its rows hold exactly the
/// declared column's ten values.
#[test]
fn a_checksum_types_a_field_the_table_never_declared() {
    const UNDECLARED: &str =
        "/Script/ShooterGame.ReplayPlayerController:ClientReplayReceiveInputEventProcessingCapture";
    assert_eq!(TABLE.lookup(UNDECLARED, "PlayerID"), None, "not declared");
    assert_eq!(
        resolve(UNDECLARED, "PlayerID", Some(2396673102)),
        Some(FieldType::Int32),
    );
}

/// A declared entry outranks the engine references and the checksum, which run
/// only after the table misses, so a declared `Owner` or `PlayerID` keeps its
/// type. Declared `Raw` or `Skip` is a decision not to decode, reported only by
/// `raw_or_skip`, so that counter must move once per field.
#[test]
fn a_declared_entry_outranks_the_engine_and_checksum_fallbacks() {
    const fn declared(field_type: FieldType) -> [OverlayEntry; 2] {
        [
            OverlayEntry {
                group_path: "/test",
                field_name: "Owner",
                field_type,
            },
            OverlayEntry {
                group_path: "/test",
                field_name: "PlayerID",
                field_type,
            },
        ]
    }
    static RAW: [OverlayEntry; 2] = declared(FieldType::Raw);
    static SKIP: [OverlayEntry; 2] = declared(FieldType::Skip);

    let mut stats = OverlayStats::default();
    for (entries, field_type, raw_or_skip) in
        [(&RAW, FieldType::Raw, 1), (&SKIP, FieldType::Skip, 2)]
    {
        let table = OverlayTable::new(entries);
        assert_eq!(
            resolve_field_type(&table, "/test", Some("Owner"), None),
            Some(field_type)
        );
        // This checksum types an undeclared `PlayerID` as `Int32` elsewhere.
        let donated = Some(2396673102);
        assert_eq!(
            resolve_field_type_with_checksum(&table, "/test", Some("PlayerID"), None, donated),
            Some(field_type),
        );
        let result = apply_overlay(
            &table,
            "/test",
            group_hash_state("/test"),
            Some("Owner"),
            Some(&[1]),
            1,
            &mut stats,
        );
        assert!(result.is_none(), "{field_type:?} must not be decoded");
        assert_eq!(stats.raw_or_skip, raw_or_skip, "{stats:?}");
    }
    let others = (
        stats.decoded_ok,
        stats.decoded_err,
        stats.not_in_table,
        stats.no_field_name,
        stats.handle_conflicts_refused,
    );
    assert_eq!(others, (0, 0, 0, 0, 0), "{stats:?}");
}

/// A checksum nothing donated types nothing -- the map asserts only what it
/// learned.
#[test]
fn an_unlearned_checksum_resolves_nothing() {
    assert_eq!(resolve("/Game/Nope.Nope_C", "Whatever", Some(1)), None,);
}

/// `AllianceFilter` (checksum 2270825073) is declared by three effect RPCs and
/// received by five more; its donors were typed two ways, so the learner
/// dropped the checksum. A correction makes the donors agree; both the donors
/// and the regenerated `checksum_table.rs` are pinned. Evidence: the
/// AllianceFilter rule in tools/apply_type_corrections.py.
#[test]
fn alliance_filter_donors_agree_so_the_checksum_types_the_receivers() {
    for group in [
        PLAY_CONTINUOUS,
        "/Script/ShooterGame.EffectManagerComponent:MulticastPlayOneShotEffect",
        REPLAY_AT_LOCATION,
    ] {
        assert_eq!(
            TABLE.lookup(group, "AllianceFilter"),
            Some(FieldType::EnumByte),
            "donor {group}"
        );
    }
    assert_checksum(2270825073, Some(FieldType::EnumByte));

    const RECEIVER: &str = FROM_CLIENT;
    assert_eq!(
        TABLE.lookup(RECEIVER, "AllianceFilter"),
        None,
        "typed by checksum, not by name"
    );
    assert_eq!(
        resolve(RECEIVER, "AllianceFilter", Some(2270825073)),
        Some(FieldType::EnumByte),
    );
}

/// The weapon effect RPCs' `EffectManagerComponent` is an IntPacked reference
/// to the holder's `EffectManager`. Both twins are pinned, against the "Viper
/// typed, Phoenix not" shape. Evidence: the EffectManagerComponent entries in
/// tools/apply_type_corrections.py.
#[test]
fn the_weapon_effect_rpcs_type_their_effect_manager_reference() {
    for group in [
        FROM_CLIENT,
        "/Script/ShooterGame.AresEquippable:MulticastPlayOneShotEffectFromClient",
    ] {
        assert_typed(
            group,
            "EffectManagerComponent",
            Some(FieldType::ObjectNetGuid),
        );
    }
    assert_checksum(1051633025, Some(FieldType::ObjectNetGuid));
}

/// Every group the overlay assigns a `RepMovement` type (table or exact scoped
/// entry), with the location level measured for that class: 2026-09-28 over
/// the 1,018 replays audited at 259ed10 (21 of the 24 builds carry these rows),
/// each actor's spawn position against its first packed location. Per entry:
/// the joins, the builds they span and the ratio (the level's divisor on every
/// build). Main stream only: checkpoints carry no `ReplicatedMovement` rows.
/// docs/DATA.md has the method in full.
const REP_MOVEMENT_LOCATION_EVIDENCE: [(&str, VectorQuantization); 27] = {
    use VectorQuantization::{RoundTwoDecimals, RoundWholeNumber};
    [
        // 647 joins, 15 builds, ratio 1.000
        (
            "/Game/Characters/AggroBot/S0/Ability_4/Projectile_Aggrobot_C_ExplodeyPatch.Projectile_Aggrobot_C_ExplodeyPatch_C",
            RoundWholeNumber,
        ),
        // 1,007 joins, 15 builds, ratio 1.000
        (
            "/Game/Characters/AggroBot/S0/Ability_E/Projectile_Aggrobot_Zamboni_Rocket.Projectile_Aggrobot_Zamboni_Rocket_C",
            RoundWholeNumber,
        ),
        // 1,819 joins, 15 builds, ratio 1.000
        (
            "/Game/Characters/AggroBot/S0/Ability_E/Projectile_E_Aggrobot_DiscTurret_PowerWave.Projectile_E_Aggrobot_DiscTurret_PowerWave_C",
            RoundWholeNumber,
        ),
        // 1,812 joins, 15 builds, ratio 1.000
        (
            "/Game/Characters/AggroBot/S0/Ability_E/Projectile_E_Aggrobot_OrbSpawner.Projectile_E_Aggrobot_OrbSpawner_C",
            RoundWholeNumber,
        ),
        // 932 joins, 15 builds, ratio 100.000 -- the one two-decimal class
        (
            "/Game/Characters/AggroBot/S0/Ability_Q/Pawn_Aggrobot_SeekerNade.Pawn_Aggrobot_SeekerNade_C",
            RoundTwoDecimals,
        ),
        // 5,715 joins, 14 builds, ratio 1.000
        (
            "/Game/Characters/BountyHunter/S0/Ability_E/Projectile_E_BountyHunter_Divebomb.Projectile_E_BountyHunter_Divebomb_C",
            RoundWholeNumber,
        ),
        // Scoped entry (tools/fixtures/scoped_type_evidence.json), a pawn.
        // 2,296 joins, 18 builds, ratio 100.000 (p1-p99 99.9986-100.0014),
        // every component within 0.0504 of spawn; re-measured at integration.
        (CLAY_BOOMBOT, RoundTwoDecimals),
        // Table entry (apply_type_corrections.py ADDITIONS). 8,265 joins, 15
        // builds, ratio 1.000 (p1-p99 0.9998-1.0001), every component within
        // 0.50 of spawn; re-measured at integration with an independent join.
        (HAWK, RoundWholeNumber),
        // 5,280 joins, 5 builds, ratio 1.000
        (
            "/Game/Characters/Hunter/S0/Ability_4/Projectile_Hunter_4_ExplosiveBolt.Projectile_Hunter_4_ExplosiveBolt_C",
            RoundWholeNumber,
        ),
        // 12,032 joins, 14 builds, ratio 1.000
        (
            "/Game/Characters/Hunter/S0/Ability_Q/Projectile_Hunter_Q_RevealBolt.Projectile_Hunter_Q_RevealBolt_C",
            RoundWholeNumber,
        ),
        // 1,046 joins, 5 builds, ratio 1.000
        (
            "/Game/Characters/Mage/S0/Ability_E/GameObject_Mage_E_WorldSmoke.GameObject_Mage_E_WorldSmoke_C",
            RoundWholeNumber,
        ),
        // 596 joins, 5 builds, ratio 1.000
        (
            "/Game/Characters/Mage/S0/Ability_Q/Projectile_Mage_Q_Wall.Projectile_Mage_Q_Wall_C",
            RoundWholeNumber,
        ),
        // 703 joins, 8 builds, ratio 1.000
        (
            "/Game/Characters/Pandemic/S0/Ability_E/Projectile_Pandemic_E_SmokeScreen_NoCollision.Projectile_Pandemic_E_SmokeScreen_NoCollision_C",
            RoundWholeNumber,
        ),
        // 2,765 joins, 18 builds, ratio 1.000
        (
            "/Game/Characters/Phoenix/S0/Ability_Q/Production/Projectile_Phoenix_Q_FlameWall_ThroughWall.Projectile_Phoenix_Q_FlameWall_ThroughWall_C",
            RoundWholeNumber,
        ),
        // 27,667 joins, 21 builds, ratio 1.000
        (
            "/Game/Characters/Smonk/S0/Ability_E/MapTargetSmoke/GameObject_Smonk_NewSmoke.GameObject_Smonk_NewSmoke_C",
            RoundWholeNumber,
        ),
        // 3,320 joins, 21 builds, ratio 1.000
        (
            "/Game/Characters/Smonk/S0/Ability_E/MapTargetSmoke/GameObject_Smonk_NewSmoke_PDS.GameObject_Smonk_NewSmoke_PDS_C",
            RoundWholeNumber,
        ),
        // 3,490 joins, 21 builds, ratio 1.000
        (
            "/Game/Characters/Smonk/S0/Ability_Q/DebuffKnife/DecayLauncher/GameObject_Smonk_Q_DecayExplosion.GameObject_Smonk_Q_DecayExplosion_C",
            RoundWholeNumber,
        ),
        // 3,505 joins, 21 builds, ratio 1.000
        (
            "/Game/Characters/Smonk/S0/Ability_Q/DebuffKnife/DecayLauncher/Projectile_Smonk_DecayNade.Projectile_Smonk_DecayNade_C",
            RoundWholeNumber,
        ),
        // 3,269 joins, 12 builds, ratio 1.000
        (
            "/Game/Characters/Sprinter/S0/Ability_4/Projectile_Neon_C_Tunnel.Projectile_Neon_C_Tunnel_C",
            RoundWholeNumber,
        ),
        // 1,029 joins, 11 builds, ratio 1.000
        (
            "/Game/Characters/Terra/S0/Ability_4/GameObject_Terra_C_TimeSlowGrenade_Explosion.GameObject_Terra_C_TimeSlowGrenade_Explosion_C",
            RoundWholeNumber,
        ),
        // 1,033 joins, 11 builds, ratio 1.000
        (
            "/Game/Characters/Terra/S0/Ability_4/Projectile_Terra_C_TimeSlowGrenade.Projectile_Terra_C_TimeSlowGrenade_C",
            RoundWholeNumber,
        ),
        // 13,892 joins, 21 builds, ratio 1.000
        (
            "/Game/Characters/Vampire/S0/Ability_4/Projectile_Vampire_4_NearsightAoE.Projectile_Vampire_4_NearsightAoE_C",
            RoundWholeNumber,
        ),
        // 14,935 joins, 17 builds, ratio 1.000
        (
            "/Game/Characters/Wraith/S0/Ability_4/Projectile_Wraith_4_Smoke.Projectile_Wraith_4_Smoke_C",
            RoundWholeNumber,
        ),
        // 14,902 joins, 17 builds, ratio 1.000
        (
            "/Game/Characters/Wraith/S0/Ability_4/Zone_Wraith_4_Smoke.Zone_Wraith_4_Smoke_C",
            RoundWholeNumber,
        ),
        // 4,062 joins, 17 builds, ratio 1.000
        (
            "/Game/Characters/Wraith/S0/Ability_Q/Projectile_Wraith_Q_NearsightMissile.Projectile_Wraith_Q_NearsightMissile_C",
            RoundWholeNumber,
        ),
        // 11,976 joins, 20 builds, ratio 1.000
        (
            "/Game/Characters/Wushu/S0/Ability_4/Projectile_Wushu_4_Smoke.Projectile_Wushu_4_Smoke_C",
            RoundWholeNumber,
        ),
        // 288,644 joins, 21 builds, ratio 1.000
        (
            "/Game/Weapons/WeaponPickups/EquippablePickupProjectile.EquippablePickupProjectile_C",
            RoundWholeNumber,
        ),
    ]
};

/// Every `RepMovement` type the overlay can assign carries the measured level,
/// and no unlisted group gets one. The level is not on the wire, so the
/// generator defaults it (extract_descriptors.py REP_MOVEMENT_LOCATION): a
/// prior, not a measurement. A new class needs its spawn evidence here first.
/// All three routes are held to the list: the table and the scoped types by
/// group, checksum propagation by admitting no `RepMovement` at all.
#[test]
fn every_rep_movement_entry_carries_its_measured_location_level() {
    use std::collections::BTreeMap;

    let table = OVERLAY_TABLE.iter().map(|e| (e.group_path, e.field_type));
    let scoped = crate::scoped_types::SCOPED_TYPES
        .iter()
        .map(|&(_, group, _, field_type)| (group, field_type));
    let mut declared: BTreeMap<&str, VectorQuantization> = BTreeMap::new();
    for (group, field_type) in table.chain(scoped) {
        if let FieldType::RepMovement { location, .. } = field_type {
            let previous = declared.insert(group, location);
            assert!(
                previous.is_none() || previous == Some(location),
                "{group}: RepMovement declared at two location levels"
            );
        }
    }
    let measured: BTreeMap<&str, VectorQuantization> =
        REP_MOVEMENT_LOCATION_EVIDENCE.into_iter().collect();
    assert_eq!(
        measured.len(),
        REP_MOVEMENT_LOCATION_EVIDENCE.len(),
        "the evidence list names a group twice"
    );
    for (group, level) in &declared {
        assert_eq!(
            measured.get(group),
            Some(level),
            "{group}: declared {level:?}; the measured level differs or was never recorded"
        );
    }
    for group in measured.keys() {
        assert!(
            declared.contains_key(group),
            "{group}: measured but no route assigns it a RepMovement type"
        );
    }
    // The name rule the checksum map would apply is closed for this field
    // (donors disagree), so the table is the only route to a RepMovement type.
    assert!(
        CHECKSUM_TYPES
            .iter()
            .all(|(_, t)| !matches!(t, FieldType::RepMovement { .. })),
        "a checksum-propagated RepMovement type would bypass the per-class evidence"
    );
}

/// The map is only useful if it holds something; a silently empty generated
/// table would make every test above pass for the wrong reason.
#[test]
fn the_checksum_table_is_populated_and_sorted() {
    assert!(CHECKSUM_TYPES.len() > 300, "{}", CHECKSUM_TYPES.len());
    assert!(CHECKSUM_TYPES.windows(2).all(|w| w[0].0 < w[1].0));
}

/// `StopMovementTime` (Float, like its sibling `StartMovementTime`) and the
/// force module `HandleNumber` (UInt32: its checksum 3336285386 reproduces only
/// as `HandleNumber: uint32`) are typed, and the checksums carry them to the
/// Stop/Remove RPCs. Evidence: the StopMovementTime and HandleNumber entries in
/// tools/apply_type_corrections.py.
#[test]
fn the_movement_time_pair_and_force_module_handle_are_typed() {
    assert_typed(STOP_CONTINUOUS, "StopMovementTime", Some(FieldType::Float));
    assert_typed(FORCE_APPLY, "HandleNumber", Some(FieldType::UInt32));
    assert_checksum(3336285386, Some(FieldType::UInt32));
}

/// `EffectID` is `FEffectID`'s `int64`, not the descriptors' `ulong`. The table
/// entries donate it through the checksum table; both halves are pinned because
/// CI's one-group checksum guard cannot see a stale checksum table. Evidence:
/// `EFFECT_ID_INT64` in tools/apply_type_corrections.py.
#[test]
fn effect_ids_are_signed_on_every_donor_and_in_the_checksum_table() {
    for (group, checksum) in [
        ("/Script/ShooterGame.EffectManagerComponent", 1129645208),
        (PLAY_CONTINUOUS, 2340855891),
        (
            "/Script/ShooterGame.EffectManagerComponent:MulticastUpdateContinuousEffect",
            2340855891,
        ),
        (REPLAY_AT_LOCATION, 2251343646),
    ] {
        assert_typed(group, "EffectID", Some(FieldType::Int64));
        assert_checksum(checksum, Some(FieldType::Int64));
    }
    assert_eq!(
        resolve(STOP_CONTINUOUS, "EffectID", Some(2340855891)),
        Some(FieldType::Int64),
    );
    assert!(
        !OVERLAY_TABLE
            .iter()
            .any(|e| e.field_type == FieldType::UInt64),
        "no entry should still read an int64 property as UInt64"
    );
}

/// The rest of `NetMulticastApplyForceModule`'s parameters are typed on Apply.
/// Remove's `ModuleType` (2,743,504 rows) is typed only through the checksum
/// table learning the Apply donor; paired by (object, handle), Remove and Apply
/// agree on 647,381 of 647,381 rows. Evidence: the NetMulticastApplyForceModule
/// entries in tools/apply_type_corrections.py.
#[test]
fn the_force_module_apply_parameters_are_typed_and_remove_follows_by_checksum() {
    const APPLY: &str = FORCE_APPLY;
    const REMOVE: &str = FORCE_REMOVE;
    for (field, expected) in [
        ("RespawnNumber", FieldType::Int32),
        ("NetTimestamp", FieldType::Float),
        ("ModuleType", FieldType::EnumByte),
        ("Module", FieldType::ObjectNetGuid),
        ("Character", FieldType::ObjectNetGuid),
    ] {
        assert_typed(APPLY, field, Some(expected));
    }
    assert_checksum(3263282135, Some(FieldType::EnumByte));
    assert_eq!(
        TABLE.lookup(REMOVE, "ModuleType"),
        None,
        "typed by checksum, not by name"
    );
    assert_eq!(
        resolve(REMOVE, "ModuleType", Some(3263282135)),
        Some(FieldType::EnumByte),
    );
    // The component's own `RespawnNumber` property (checksum 3044239005) is a
    // different property on a different group; the RPC entry does not reach it.
    assert_typed(
        "/Script/ShooterGame.ForceModuleManagerComponent",
        "RespawnNumber",
        None,
    );
}

/// `ReadyingStateComponent.AuthEquipSpeed` (EnumByte) and the inventory's
/// `CorrectionIndex` / `LastSeenClientCorrectionIndex` (Int32) are typed, by
/// name and by checksum. Evidence: the AuthEquipSpeed and AresInventory entries
/// in tools/apply_type_corrections.py.
#[test]
fn readying_speed_and_inventory_correction_counters_are_typed() {
    for (group, field, expected) in [
        (
            "/Script/ShooterGame.ReadyingStateComponent",
            "AuthEquipSpeed",
            FieldType::EnumByte,
        ),
        (
            "/Script/ShooterGame.AresInventory",
            "CorrectionIndex",
            FieldType::Int32,
        ),
        (
            "/Script/ShooterGame.AresInventory",
            "LastSeenClientCorrectionIndex",
            FieldType::Int32,
        ),
    ] {
        assert_typed(group, field, Some(expected));
    }
    for (checksum, expected) in [
        (3151779304u32, FieldType::EnumByte),
        (3198546915, FieldType::Int32),
        (1076231069, FieldType::Int32),
    ] {
        assert_checksum(checksum, Some(expected));
    }
}

/// The callout region a player stands in, reachable since the
/// `CalloutRegionTracker` leaf was remapped: an `ObjectNetGuid` naming the
/// region actor. Evidence: the CurrentRegion entry in
/// tools/apply_type_corrections.py.
#[test]
fn the_callout_region_is_typed() {
    assert_typed(
        "/Script/ShooterGame.CalloutRegionTrackingComponent",
        "CurrentRegion",
        Some(FieldType::ObjectNetGuid),
    );
}

/// The per-cast ability log's members, flattened into
/// `AbilityCastsThisRound[i].<member>` rows. Their names carry Blueprint
/// property GUIDs, byte-identical on 13.01 and 13.02, which makes pinning them
/// safe. Evidence: the Comp_AbilityStatisticsReplicator entries in
/// tools/apply_type_corrections.py.
#[test]
fn the_ability_cast_log_is_typed() {
    const GROUP: &str = "/Game/Characters/_Core/Comp_AbilityStatisticsReplicator\
.Comp_AbilityStatisticsReplicator_C";
    for (field, expected) in [
        (
            "Player_11_0963330440D68BDF1A8E34B035420342",
            FieldType::FString,
        ),
        ("Slot_12_22D571914FAFD5F0EBD400B7E2F28B36", FieldType::Byte),
        (
            "Round_22_905E6CC0448D2C6270A94C9690101E49",
            FieldType::Int32,
        ),
        (
            "CastTime_4_5AE288704801A9B74D6D159DFC2BD147",
            FieldType::Float,
        ),
        (
            "CastLocation_21_61F4B6BC47A10FE8CD34D29141FC9B88",
            FieldType::VectorDouble,
        ),
    ] {
        assert_typed(GROUP, field, Some(expected));
    }
}

/// Both classes declaring `MulticastAddSmokeScreenPoint` are typed: Viper's
/// `SmokeScreenManager` and Phoenix's `FlameWallManager`, which the checksum
/// fallback rightly refused to type from Viper's, so it takes a name-level
/// entry. Evidence: the FlameWallManager Translation/Scale3D entries in
/// tools/apply_type_corrections.py.
#[test]
fn both_classes_declaring_the_smoke_point_rpc_are_typed() {
    const VIPER: &str = "/Game/Characters/Pandemic/S0/Ability_E/\
GameObject_Pandemic_E_SmokeScreenManager.GameObject_Pandemic_E_SmokeScreenManager_C\
:MulticastAddSmokeScreenPoint";
    const PHOENIX: &str = "/Game/Characters/Phoenix/S0/Ability_Q/Production/\
GameObject_Phoenix_Q_FlameWallManager_Production.\
GameObject_Phoenix_Q_FlameWallManager_Production_C:MulticastAddSmokeScreenPoint";
    for group in [VIPER, PHOENIX] {
        for field in ["Translation", "Scale3D"] {
            assert_typed(group, field, Some(FieldType::VectorDouble));
        }
    }
}

/// `"215"` and `"216"` are field names, decoded on 353 groups but pinned `Raw`
/// on 17 weapon groups, where a name hit wins before the checksum. Evidence
/// that every group carries the same property: the "215"/"216" rules in
/// tools/apply_type_corrections.py.
#[test]
fn the_weapon_classes_type_215_and_216_like_everything_else() {
    const WEAPONS: [&str; 3] = [
        "/Game/Equippables/Guns/Rifles/AK/AssaultRifle_AK.AssaultRifle_AK_C",
        "/Game/Equippables/Guns/Sidearms/BasePistol/BasePistol.BasePistol_C",
        "/Game/Equippables/Melee/Ability_Melee_Base.Ability_Melee_Base_C",
    ];
    const ALREADY_TYPED: &str = "/Game/GameModes/Bomb/TimedBomb.TimedBomb_C";
    for group in WEAPONS.iter().chain(std::iter::once(&ALREADY_TYPED)) {
        for field in ["215", "216"] {
            assert_typed(group, field, Some(FieldType::EnumRemainingBits));
        }
    }
}

/// `249`, after the `VectorDouble` placement location `248`, is the
/// `RotationShort` rotation on all five RPCs that send it, and not the FQuat
/// `249` of an FTransform. Evidence: the `249` RotationShort entries in
/// tools/apply_type_corrections.py.
#[test]
fn the_effect_placement_rotation_is_typed_on_every_rpc_that_sends_it() {
    const GROUPS: [&str; 5] = [
        ONE_SHOT_AT_LOCATION,
        REPLAY_AT_LOCATION,
        "/Script/ShooterGame.ReplayEffectComponent:ReplayPlayOneShotEffectAtLocation",
        "/Script/ShooterGame.EffectManagerComponent:ReplayRecordOneShotEffect",
        "/Script/ShooterGame.EffectManagerComponent:ReplayRecordContinuousEffect",
    ];
    for group in GROUPS {
        assert_typed(group, "249", Some(FieldType::RotationShort));
    }
    // The location it pairs with, on the four that send it numbered;
    // `ReplayPlayContinuousEffectAtLocation` names both, and its `Rotation`
    // must still agree.
    for group in GROUPS {
        if group.ends_with("ReplayPlayContinuousEffectAtLocation") {
            assert_eq!(
                TABLE.lookup(group, "Rotation"),
                Some(FieldType::RotationShort),
                "{group} names its rotation and must still agree"
            );
            continue;
        }
        assert_typed(group, "248", Some(FieldType::VectorDouble));
    }
}

/// The RNG component's seed reads as Int32, following Unreal's `FRandomStream`.
/// Evidence: the AuthCurrentRandomSeed entry in tools/apply_type_corrections.py.
#[test]
fn the_random_number_generator_seed_is_typed() {
    assert_typed(
        "/Script/ShooterGame.NetworkedRandomNumberGeneratorComponent",
        "AuthCurrentRandomSeed",
        Some(FieldType::Int32),
    );
}

#[test]
fn targeting_vectors_and_heal_causer_require_exact_scoped_checksums() {
    let cases = [
        (
            "/Script/ShooterGame.MapTargetingStateComponent",
            "CursorWorldLocation",
            3280594315,
            FieldType::VectorDouble,
        ),
        (
            "/Script/ShooterGame.MapTargetingStateComponent:MulticastRespondToValidSingleMapClick",
            "ClickedLocation",
            975869058,
            FieldType::VectorDouble,
        ),
        (
            HEAL_PARAMS,
            "HealCauser",
            546618027,
            FieldType::ObjectNetGuid,
        ),
    ];
    for (group, field, checksum, expected) in cases {
        assert_eq!(resolve(group, field, Some(checksum)), Some(expected));
        assert_eq!(resolve(group, field, Some(checksum ^ 1)), None);
        assert_eq!(resolve("/wrong", field, Some(checksum)), None);
    }
}

const HEAL_PARAMS: &str = "/Script/ShooterGame.DamageableComponent:MulticastNotifyHeal";
const DECAY_PARAMS: &str = "/Script/ShooterGame.DamageableComponent:MulticastNotifyOverhealDecay";

/// The heal and overheal-decay references, typed by exact group/name/checksum
/// (`tools/fixtures/scoped_type_evidence.json`): the same names carry other
/// checksums on the damage RPCs. No checksum, a neighbouring checksum, the
/// exported `_ClassNetCache` spelling or the sibling RPC types nothing.
#[test]
fn heal_and_decay_references_require_exact_scoped_checksums() {
    const CNC: &str = "/Script/ShooterGame.DamageableComponent_ClassNetCache";
    let cases = [
        (HEAL_PARAMS, "EventInstigator", 3_087_885_251),
        (HEAL_PARAMS, "EventInstigatorPawn", 3_901_949_544),
        (DECAY_PARAMS, "EventInstigator", 3_087_885_251),
        (DECAY_PARAMS, "EventInstigatorPawn", 3_901_949_544),
        (DECAY_PARAMS, "DecayCauser", 3_648_603_088),
    ];
    for (group, field, checksum) in cases {
        assert_eq!(
            resolve(group, field, Some(checksum)),
            Some(FieldType::ObjectNetGuid),
            "{group} {field}"
        );
        for other in [None, Some(checksum ^ 1)] {
            assert_eq!(
                resolve(group, field, other),
                None,
                "{group} {field} {other:?}"
            );
        }
        let (_, function) = group.split_once(':').expect("a parameter group");
        let exported = format!("{function}.{field}");
        assert_eq!(resolve(CNC, &exported, Some(checksum)), None, "{exported}");
    }
    // Each causer is declared on one RPC only.
    assert_eq!(
        resolve(HEAL_PARAMS, "DecayCauser", Some(3_648_603_088)),
        None
    );
    assert_eq!(resolve(DECAY_PARAMS, "HealCauser", Some(546_618_027)), None);
}

/// The scoped references decode through the ordinary packed-NetGUID reader,
/// with the payload consumed exactly.
///
/// The 24-bit window is the widest the 2026-09-28 audit saw on these
/// parameters (50,360 = 56 + 9*128 + 3*16384, low-bit continuation). The
/// single zero byte is the null reference: 39 `DecayCauser` rows in that audit
/// are exactly `0x00`, and they decode to 0 -- Unreal's null NetGUID, the same
/// value the damage-side references already export -- not to an actor.
#[test]
fn heal_and_decay_references_decode_packed_guids_exactly() {
    let mut stats = OverlayStats::default();
    let mut apply = |group: &str, field: &str, handle: u32, checksum: u32, raw: &[u8], bits| {
        apply_scoped(&mut stats, group, field, handle, checksum, raw, bits).value_i64
    };
    let pawn = apply(
        HEAL_PARAMS,
        "EventInstigatorPawn",
        8,
        3_901_949_544,
        &[0x71, 0x13, 0x06],
        24,
    );
    assert_eq!(pawn, Some(50_360));
    let null = apply(DECAY_PARAMS, "DecayCauser", 9, 3_648_603_088, &[0x00], 8);
    assert_eq!(null, Some(0));
    // A byte the packed value never claims is a residual, not a value.
    let residual = apply(
        HEAL_PARAMS,
        "EventInstigator",
        7,
        3_087_885_251,
        &[0x71, 0x13, 0x06, 0x00],
        32,
    );
    assert_eq!(residual, None);
    assert_eq!((stats.decoded_ok, stats.decoded_err), (2, 1));
}
