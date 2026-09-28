//! The overlay table's resolution order, and the hash index's agreement
//! with the binary search it replaced.

use std::collections::BTreeSet;

use crate::checksum_table::CHECKSUM_TYPES;
use crate::decode::{DecodeError, FieldType, decode_field};
use crate::overlay::{
    OverlayEntry, OverlayHandleEntry, OverlayStats, OverlayTable, apply_overlay,
    apply_overlay_with_handle, canonical_group, group_hash_state, lookup_checksum,
    resolve_field_type, resolve_field_type_with_checksum,
};
use crate::types::{RotatorQuantization, VectorQuantization};
use crate::{OVERLAY_HANDLE_TABLE, OVERLAY_TABLE};

const BOMB_GS: &str = "/Game/GameModes/Bomb/BombGameState.BombGameState_C";
const BOMB_PS: &str = "/Game/GameModes/Bomb/BombPlayerState.BombPlayerState_C";
const SWIFT_GS: &str = "/Game/GameModes/_Development/Swiftplay_EndOfRoundCredits\
/Swiftplay_EoRCredits_GameState.Swiftplay_EoRCredits_GameState_C";
const SWIFT_PS: &str = "/Game/GameModes/_Development/Swiftplay_EndOfRoundCredits\
/Swiftplay_EoRCredits_PlayerState.Swiftplay_EoRCredits_PlayerState_C";

/// One name, two properties: the byte-shaped `B` and the 32-bit `B`
/// (checksum 943211507, the second word of the player-state GUID -- see
/// `player_state_guid_parts_are_scoped_uint32`) resolve by checksum alone.
#[test]
fn scoped_types_require_the_exact_group_name_and_checksum() {
    let table = OverlayTable::new(&OVERLAY_TABLE);
    for group in [BOMB_PS, SWIFT_PS] {
        assert_eq!(
            resolve_field_type_with_checksum(&table, group, Some("B"), None, Some(379198054)),
            Some(FieldType::Byte)
        );
        assert_eq!(
            resolve_field_type_with_checksum(&table, group, Some("B"), None, Some(943211507)),
            Some(FieldType::UInt32)
        );
        for checksum in [None, Some(1)] {
            assert_eq!(
                resolve_field_type_with_checksum(&table, group, Some("B"), None, checksum),
                None
            );
        }
    }
    for (group, name) in [("/Unobserved", "B"), (BOMB_PS, "Unobserved")] {
        assert_eq!(
            resolve_field_type_with_checksum(&table, group, Some(name), None, Some(379198054)),
            None
        );
    }
}

#[test]
fn scoped_bytes_decode_exactly_and_reject_a_wider_payload() {
    let table = OverlayTable::new(&OVERLAY_TABLE);
    let mut stats = OverlayStats::default();
    let value = crate::apply_overlay_with_checksum(
        &table,
        BOMB_PS,
        group_hash_state(BOMB_PS),
        Some("B"),
        39,
        Some(379198054),
        Some(&[255]),
        8,
        &mut stats,
    )
    .expect("scoped byte is attempted");
    assert_eq!(value.value_i64, Some(255));
    let rejected = crate::apply_overlay_with_checksum(
        &table,
        BOMB_PS,
        group_hash_state(BOMB_PS),
        Some("B"),
        39,
        Some(379198054),
        Some(&[255, 0, 0, 0]),
        32,
        &mut stats,
    )
    .expect("known type reports a rejected width");
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

/// Every scoped identity must be reachable. The table (and its class
/// aliases) resolve a name before `SCOPED_TYPES` is consulted, so a scoped
/// entry whose group already has a table entry of the same name is never
/// read: it can disagree with the type that is, and no row or counter would
/// show it. Five force-module entries were exactly that once the table typed
/// the same `NetMulticastApplyForceModule` parameters by name.
///
/// Resolved without a checksum, so only the table, the aliases and the
/// engine-reference fallback can answer. The fallback answers for any group,
/// so a name it covers is recognised by also answering for a group no table
/// declares, and is not counted as shadowing.
#[test]
fn no_scoped_identity_is_shadowed_by_the_table() {
    let table = OverlayTable::new(&OVERLAY_TABLE);
    let by_name =
        |group, name| resolve_field_type_with_checksum(&table, group, Some(name), None, None);
    let shadowed: Vec<(&str, &str, u32)> = crate::scoped_types::SCOPED_TYPES
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
}

/// Upstream 8b7afcb's Raze fields are typed by exact identity only: the
/// checksum is part of the key, and another group or checksum resolves to
/// nothing rather than borrowing the type.
#[test]
fn raze_scoped_identities_require_their_exact_checksum() {
    let table = OverlayTable::new(&OVERLAY_TABLE);
    let resolve = |group, name, checksum| {
        resolve_field_type_with_checksum(&table, group, Some(name), None, checksum)
    };
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
    // Of the seven force-module identities 8b7afcb declares, `Source` and
    // `Duration` are still scoped here. `Module`, `ModuleType`, `Character`,
    // `NetTimestamp` and `RespawnNumber` are typed by name in the table
    // (apply_type_corrections.py), which resolves before any scoped entry, so
    // scoped copies of them would be unreachable and were removed.
    assert_eq!(
        resolve(FORCE_APPLY, "Duration", Some(1_815_021_954)),
        Some(FieldType::Float)
    );
    assert_eq!(
        resolve(FORCE_APPLY, "Source", Some(1_966_913_909)),
        Some(FieldType::ObjectNetGuid)
    );
    assert_eq!(resolve(FORCE_APPLY, "Duration", Some(1)), None);
    // The Remove RPC is another group: exact identity means the scoped
    // `Duration` checksum does not reach it. (Its `ModuleType` IS typed, but
    // through the table entry's checksum, which is not this mechanism.)
    assert_eq!(
        resolve(
            "/Script/ShooterGame.ForceModuleManagerComponent:NetMulticastRemoveForceModule",
            "Duration",
            Some(1_815_021_954)
        ),
        None
    );
    assert_eq!(
        resolve(
            "/Script/ShooterGame.ShooterCharacter:ClientResetRemoteMovementPrediction",
            "isPossess",
            Some(3_522_099_335)
        ),
        Some(FieldType::Bool)
    );
}

/// The Boom Bot replicates short rotation components and a location in
/// hundredths of a centimetre (two decimals), verified against its spawn
/// location. Raze's byte-rotation projectiles stay untyped: they were declined
/// while `RepMovement` read every location at scale 100 (docs/UPSTREAM_RAZE_WARDEN.md),
/// and now that the level is per class, typing one needs its own spawn-join
/// entry in `REP_MOVEMENT_LOCATION_EVIDENCE` first.
#[test]
fn boombot_movement_is_short_and_byte_rotation_projectiles_stay_raw() {
    let table = OverlayTable::new(&OVERLAY_TABLE);
    assert_eq!(
        resolve_field_type_with_checksum(
            &table,
            CLAY_BOOMBOT,
            Some("ReplicatedMovement"),
            None,
            Some(REPLICATED_MOVEMENT_CHECKSUM)
        ),
        Some(FieldType::RepMovement {
            rotation: crate::types::RotatorQuantization::ShortComponents,
            location: VectorQuantization::RoundTwoDecimals,
        })
    );
    for group in [CLAY_SATCHEL, CLAY_ROCKET] {
        assert_eq!(
            resolve_field_type_with_checksum(
                &table,
                group,
                Some("ReplicatedMovement"),
                None,
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
    fn decode(
        stats: &mut OverlayStats,
        group: &str,
        name: &str,
        handle: u32,
        checksum: u32,
        raw: &[u8],
        bits: u32,
    ) -> crate::overlay::OverlayResult {
        let table = OverlayTable::new(&OVERLAY_TABLE);
        crate::apply_overlay_with_checksum(
            &table,
            group,
            group_hash_state(group),
            Some(name),
            handle,
            Some(checksum),
            Some(raw),
            bits,
            stats,
        )
        .expect("a scoped identity is attempted")
    }
    let mut stats = OverlayStats::default();
    let seed = decode(
        &mut stats,
        CLAY_SATCHEL_ABILITY,
        "CosmeticRandomSeed",
        56,
        2_863_861_815,
        &[0xE1, 0xE9, 0x4B, 0x40],
        32,
    );
    assert_eq!(seed.value_i64, Some(1_078_716_897));
    let offset = decode(
        &mut stats,
        CLAY_SATCHEL,
        "LocationOffset",
        5,
        111_823_753,
        &[0xD3, 0x20, 0x67, 0xB7, 0xA8, 0x97, 0x48, 0x00],
        64,
    );
    assert_eq!(offset.value_str.as_deref(), Some("(-782.71,-1366.59,5.8)"));
    let rotation = decode(
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
    // `ModuleType` resolves through its table entry (EnumByte), not a scoped
    // identity, since the table typed the Apply parameters by name; upstream's
    // recorded 3-bit payload must still read 2.
    let module_type = decode(
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
    let truncated = decode(
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

/// The four 32-bit words `A`/`B`/`C`/`D` on the player state are one FGuid, whose
/// members Unreal declares `uint32`, so they are `UInt32` -- scoped per group
/// and checksum from `tools/fixtures/scoped_type_evidence.json`.
///
/// The scope is load-bearing. The handles drift by build (A is 219, 204 or 207)
/// and 207 is D's handle on 11.11-12.05, so no handle key could work; `B` also
/// names nine byte-shaped properties and `A` an 8-bit one (1036865991); and
/// scoped entries never follow the Swiftplay alias, so each group needs its own.
#[test]
fn player_state_guid_parts_are_scoped_uint32() {
    let table = OverlayTable::with_handles(&OVERLAY_TABLE, &OVERLAY_HANDLE_TABLE);
    let resolve = |group: &str, field: &str, checksum: Option<u32>| {
        resolve_field_type_with_checksum(&table, group, Some(field), None, checksum)
    };
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

/// The first overlay use of `UInt32`, proved end to end: scoped lookup, then
/// `decode_u32`, then `value_i64`, with a high-bit word staying positive.
///
/// 0xe28c69d7 is a real `D` value from the 2026-09-28 audit. Read as `Int32`
/// the same four bytes are -494114345 -- a plausible wrong number that a
/// width check alone would accept. A payload of any other width is a decode
/// error, not a truncated or padded value.
#[test]
fn player_state_guid_parts_decode_unsigned_and_exactly() {
    let table = OverlayTable::with_handles(&OVERLAY_TABLE, &OVERLAY_HANDLE_TABLE);
    let mut stats = OverlayStats::default();
    let mut apply = |group: &str, raw: &[u8], bits| {
        crate::apply_overlay_with_checksum(
            &table,
            group,
            group_hash_state(group),
            Some("D"),
            210,
            Some(1_032_080_829),
            Some(raw),
            bits,
            &mut stats,
        )
        .expect("a scoped GUID part is attempted")
        .value_i64
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

/// A Bomb class is already canonical and must not be rewritten.
#[test]
fn canonical_group_leaves_a_bomb_class_alone() {
    assert_eq!(canonical_group(BOMB_GS), BOMB_GS);
    assert_eq!(canonical_group(BOMB_PS), BOMB_PS);
    assert_eq!(
        canonical_group("/Game/Whatever.Whatever_C"),
        "/Game/Whatever.Whatever_C"
    );
}

#[test]
fn bomb_player_crosshair_fields_are_typed_without_the_colliding_b() {
    const GROUP: &str = "/Game/GameModes/Bomb/BombPlayerState.BombPlayerState_C";
    let table = OverlayTable::new(&OVERLAY_TABLE);
    for field in [
        "bHasOutline",
        "bDisplayCenterDot",
        "bShowLines",
        "bUseAdvancedOptions",
    ] {
        assert_eq!(table.lookup(GROUP, field), Some(FieldType::Bool), "{field}");
    }
    for field in ["OutlineThickness", "CenterDotSize", "LineLength", "Opacity"] {
        assert_eq!(
            table.lookup(GROUP, field),
            Some(FieldType::Float),
            "{field}"
        );
    }
    for field in ["G", "R"] {
        assert_eq!(table.lookup(GROUP, field), Some(FieldType::Byte), "{field}");
    }
    assert_eq!(table.lookup(GROUP, "ProfileName"), Some(FieldType::FString));
    assert_eq!(
        table.lookup(GROUP, "B"),
        None,
        "B has both 8- and 32-bit wire fields"
    );
}

#[test]
fn tidal_wave_rpc_parameters_are_typed() {
    const CHUNK: &str = "/Game/Characters/Mage/S0/Ability_X/GameObject_Mage_X_TidalWave_Chunk.GameObject_Mage_X_TidalWave_Chunk_C";
    let table = OverlayTable::new(&OVERLAY_TABLE);
    let initialize = format!("{CHUNK}:MulticastInitialize");
    for (field, field_type) in [
        ("ChunkIndex", FieldType::Int32),
        ("ChunkSpacing", FieldType::Float),
        ("Velocity In", FieldType::Double),
        ("PreviousChunk", FieldType::ObjectNetGuid),
    ] {
        assert_eq!(
            table.lookup(&initialize, field),
            Some(field_type),
            "{field}"
        );
    }
    assert_eq!(
        table.lookup(
            &format!("{CHUNK}:MulticastWallStartLinger"),
            "FinalEndpointReached"
        ),
        Some(FieldType::Bool)
    );
}

#[test]
fn canonical_group_maps_the_swiftplay_siblings() {
    assert_eq!(canonical_group(SWIFT_GS), BOMB_GS);
    assert_eq!(canonical_group(SWIFT_PS), BOMB_PS);
}

/// Suffixed forms are deliberately NOT aliased: the table holds no entries for
/// the Bomb spellings of `_ClassNetCache` or `<Class>:<Function>`, so aliasing
/// them would be an untested claim buying nothing. Pinned so a later "make it
/// consistent" edit has to argue with a test.
#[test]
fn canonical_group_does_not_alias_the_suffixed_forms() {
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
    let table = OverlayTable::with_handles(&OVERLAY_TABLE, &OVERLAY_HANDLE_TABLE);
    for field in ["ChosenCeremonyForRound", "RoundResults", "BombState"] {
        let bomb = resolve_field_type(&table, BOMB_GS, Some(field), None);
        let swift = resolve_field_type(&table, SWIFT_GS, Some(field), None);
        assert_eq!(swift, bomb, "{field} must resolve the same on both classes");
        assert!(bomb.is_some(), "{field} should be in the table at all");
    }
}

/// The alias must not invent types. A name in neither class stays unresolved.
#[test]
fn the_alias_does_not_invent_a_type() {
    let table = OverlayTable::with_handles(&OVERLAY_TABLE, &OVERLAY_HANDLE_TABLE);
    assert_eq!(
        resolve_field_type(&table, SWIFT_GS, Some("NoSuchFieldAnywhere"), None),
        None,
    );
    // and an unaliased group gains nothing
    assert_eq!(
        resolve_field_type(
            &table,
            "/Game/Nope.Nope_C",
            Some("ChosenCeremonyForRound"),
            None
        ),
        None,
    );
}

#[test]
fn table_is_sorted() {
    let table = &OVERLAY_TABLE;
    for window in table.windows(2) {
        let cmp = window[0]
            .group_path
            .cmp(window[1].group_path)
            .then_with(|| window[0].field_name.cmp(window[1].field_name));
        assert!(
            cmp.is_lt() || cmp.is_eq(),
            "table not sorted at {:?} vs {:?}",
            (window[0].group_path, window[0].field_name),
            (window[1].group_path, window[1].field_name)
        );
    }
}

#[test]
fn lookup_finds_known_field() {
    let table = OverlayTable::new(&OVERLAY_TABLE);
    let ft = table.lookup(
        "/Game/GameModes/Bomb/BombPlayerState.BombPlayerState_C",
        "CompetitiveTier",
    );
    assert_eq!(ft, Some(FieldType::Int32));
}

/// Live per-player economy is replicated under `MoneyManagementComponent` on
/// BOTH 13.01 and 13.02, but no C# descriptor declares the group, so without
/// these entries the fields ship untyped even though their raw bits decode to
/// real credits -- `Money` is 800 across all actors at pistol-round start and
/// runs 0..9000 in multiples of 50; `StartOfRoundMoney` is 800 active / 0
/// inactive; `TotalMoneyGranted` is cumulative 800..34200. `StartOfRoundMoney`'s
/// type is descriptor-corroborated (declared Int32 under OwnerExclusivePlayerInfo
/// at OwnerExclusivePlayerInfoDescriptor.cs:93, a separate end-of-round path).
/// See tools/apply_type_corrections.py ADDITIONS.
#[test]
fn money_management_economy_is_typed() {
    let table = OverlayTable::new(&OVERLAY_TABLE);
    let group = "/Script/ShooterGame.MoneyManagementComponent";
    assert_eq!(table.lookup(group, "Money"), Some(FieldType::Int32));
    assert_eq!(
        table.lookup(group, "StartOfRoundMoney"),
        Some(FieldType::Int32)
    );
    assert_eq!(
        table.lookup(group, "TotalMoneyGranted"),
        Some(FieldType::Int32)
    );
}

/// Concussion state is replicated under a SHARED component path
/// (`/Game/Characters/Components/Comp_Actor_Concussable.Comp_Actor_Concussable_C`)
/// that is attached to every player character -- it is NOT agent-specific.
/// On the 98605b1b Demos export the component appears on 9 distinct actors
/// spanning eight agents (Phoenix, Breach, Smonk, Clay, Guide, Wushu, Terra,
/// Pandemic, Deadeye) plus Guide's PossessableScout pawn, and the field
/// names and bit widths are identical on every one of them.
///
/// The widths are self-checking across all 375 rows: ConcussStartTime and
/// ConcussEndTime are 32 bits on all 39 rows each (Float), and ConcussLevel
/// is 64 bits on all 297 rows (Double). Read as Float, the start/end times
/// are game-seconds (389.5/392.0 ... 1916.7/1919.2 -- the ~2.5 s gap is the
/// concussion duration); read as Double, ConcussLevel runs the 0..1
/// intensity ramp. No descriptor declares this group, so the entries are
/// ADDITIONS in the same wire-evidence class as `Money` and `Ping`.
#[test]
fn concussion_fields_are_typed() {
    let table = OverlayTable::new(&OVERLAY_TABLE);
    let group = "/Game/Characters/Components/Comp_Actor_Concussable\
.Comp_Actor_Concussable_C";
    assert_eq!(
        table.lookup(group, "ConcussStartTime"),
        Some(FieldType::Float)
    );
    assert_eq!(
        table.lookup(group, "ConcussEndTime"),
        Some(FieldType::Float)
    );
    assert_eq!(table.lookup(group, "ConcussLevel"), Some(FieldType::Double));
}

/// `Comp_AbilityFuelSystem` is a generic component attached to a handful of
/// fuel-burning ability actors -- on 98605b1b that is Sage/Guide's heal
/// (`Ability_Guide_4_Heal`) and Viper/Pandemic's smoke screen. It is
/// per-ability, not per-player, but the component path is shared so a single
/// entry covers every actor that carries it.
///
/// CurrentFuel is 64 bits on all 5702 rows and reads as Double a smooth
/// 1.0 -> 0.0 drain (1.0, 0.9993, 0.9909, 0.9824, ...). IsFuelDraining is
/// 1 bit on all 60 rows, raw 0x00/0x01 -- an unambiguous Bool. The task note
/// guessed CurrentFuel as Float, but the wire is 64-bit; Double is what
/// decodes. No descriptor declares this group, so these are ADDITIONS.
#[test]
fn ability_fuel_fields_are_typed() {
    let table = OverlayTable::new(&OVERLAY_TABLE);
    let group = "/Game/Characters/Components/Comp_AbilityFuelSystem\
.Comp_AbilityFuelSystem_C";
    assert_eq!(table.lookup(group, "CurrentFuel"), Some(FieldType::Double));
    assert_eq!(table.lookup(group, "IsFuelDraining"), Some(FieldType::Bool));
}

/// `Ping` on BombPlayerState is a 16-bit LE unsigned integer that behaves
/// like latency in milliseconds (min ~6, p50 ~15, p90 ~19, max ~473 on
/// 02d4d478). No descriptor declares it, but the wire evidence is
/// overwhelming and the encoding was settled
/// (docs/archive/PROJECT_STATUS.md 18). Typed as `SerializedInt{65536}`
/// (16 bits LSB-first) -- the same wire-evidence ADDITION class as `Money`.
#[test]
fn ping_latency_is_typed() {
    let table = OverlayTable::new(&OVERLAY_TABLE);
    assert_eq!(
        table.lookup(
            "/Game/GameModes/Bomb/BombPlayerState.BombPlayerState_C",
            "Ping"
        ),
        Some(FieldType::SerializedInt { max: 65536 })
    );
}

#[test]
fn equippable_used_is_an_object_net_guid() {
    // The C# descriptor attaches a custom decoder
    // (DamageParameters.cs:51 -> ValorantPayloadDecoders.Equippable), which
    // extract_descriptors.py cannot see through, so it lands in table.rs as
    // Raw. That decoder is exactly archive.ReadIntPacked(), i.e. our
    // ObjectNetGuid. Leaving it Raw forces consumers to guess the encoding;
    // the adapter guessed a fixed 16-bit LE integer and produced values that
    // were never valid NetGUIDs. tools/apply_type_corrections.py restores
    // the real type.
    let table = OverlayTable::new(&OVERLAY_TABLE);
    for group in [
        "/Script/ShooterGame.DamageableComponent:MulticastNotifyDamage_Base",
        "/Script/ShooterGame.DamageableComponent:MulticastNotifyDamage_Point",
    ] {
        assert_eq!(
            table.lookup(group, "EquippableUsed"),
            Some(FieldType::ObjectNetGuid),
            "EquippableUsed must decode as a net GUID in {group}",
        );
    }
}

/// The two death-montage parameters of both damage RPCs are IntPacked GUIDs,
/// not the opaque payload the descriptor's `AddRaw` declares. Over the
/// 1,018-replay audit (959,445 rows each): 8-bit rows are all the null GUID,
/// and the rest resolve 100% -- `DeathMontageEffectOverride` through
/// `net_guids` to an `FXC_*_C` finisher effect class, and
/// `DeathMontageEffectOverrideContext` through `actors.parquet` to a `*_PC_C`
/// pawn open at the event. `Raw` table entries win before the checksum and
/// the scoped types, so this has to be a table correction, and the
/// exact-quote match must not reach `...IsQueued` (a Bool).
#[test]
fn the_death_montage_parameters_are_object_net_guids() {
    let table = OverlayTable::new(&OVERLAY_TABLE);
    for group in [
        "/Script/ShooterGame.DamageableComponent:MulticastNotifyDamage_Base",
        "/Script/ShooterGame.DamageableComponent:MulticastNotifyDamage_Point",
    ] {
        for field in [
            "DeathMontageEffectOverride",
            "DeathMontageEffectOverrideContext",
        ] {
            assert_eq!(
                table.lookup(group, field),
                Some(FieldType::ObjectNetGuid),
                "{field} in {group}"
            );
        }
        assert_eq!(
            table.lookup(group, "bDeathMontageEffectOverrideIsQueued"),
            Some(FieldType::Bool),
            "the Bool sibling is untouched in {group}"
        );
    }
    assert_eq!(lookup_checksum(1712763745), Some(FieldType::ObjectNetGuid));
    assert_eq!(lookup_checksum(2397897524), Some(FieldType::ObjectNetGuid));
}

/// `AresEquippableDataTracker.OriginalBuyerTeam` is an inline FName: 97 bits
/// is 1 (isHardcoded = 0) + 32 (length 4) + `Red\0` + 32 (number 0), and 105
/// bits the same around `Blue\0`. Those two payloads are the only ones in the
/// 1,018-replay audit (748,381 rows, main and checkpoint), and the descriptor's
/// EnumByte could read neither -- no row is 8 bits.
#[test]
fn original_buyer_team_is_an_fname() {
    let table = OverlayTable::new(&OVERLAY_TABLE);
    assert_eq!(
        table.lookup(
            "/Script/ShooterGame.AresEquippableDataTracker",
            "OriginalBuyerTeam"
        ),
        Some(FieldType::FName)
    );
    assert_eq!(lookup_checksum(255019476), Some(FieldType::FName));
}

#[test]
fn transition_context_is_an_object_net_guid() {
    let table = OverlayTable::new(&OVERLAY_TABLE);
    assert_eq!(
        table.lookup(
            "/Script/ShooterGame.EquippableStateMachineComponent",
            "TransitionContext"
        ),
        Some(FieldType::ObjectNetGuid)
    );
}

#[test]
fn hawk_flash_post_control_velocity_is_vector_double_only_on_its_exact_group() {
    let table = OverlayTable::new(&OVERLAY_TABLE);
    let group = "/Game/Characters/Guide/S0/Ability_E/Projectile_Guide_E_HawkFlash.Projectile_Guide_E_HawkFlash_C";
    assert_eq!(
        table.lookup(group, "PostControlVelocity"),
        Some(FieldType::VectorDouble)
    );
    assert_ne!(
        table.lookup(
            "/Script/ShooterGame.EquippableStateMachineComponent",
            "PostControlVelocity"
        ),
        Some(FieldType::VectorDouble)
    );
}

/// HawkFlash's `ReplicatedMovement` is read with byte rotator components and
/// its `Banking` as a double, on that exact group only. Over the 1,018-replay
/// audit every one of its 1,033,952 movement payloads (71-118 bits) is
/// consumed exactly by the byte reading and 54.6% overrun or leave residue
/// under the short one; `Banking` is 64 bits on all 801,700 rows, reading
/// -180..180. The location is whole units: the packed integer at each actor's
/// first update matches its spawn position (see REP_MOVEMENT_LOCATION_EVIDENCE),
/// so the entry states `RoundWholeNumber` and the export is world units.
#[test]
fn hawk_flash_movement_and_banking_are_typed_on_their_exact_group() {
    const HAWK: &str = "/Game/Characters/Guide/S0/Ability_E/Projectile_Guide_E_HawkFlash.Projectile_Guide_E_HawkFlash_C";
    let table = OverlayTable::new(&OVERLAY_TABLE);
    assert_eq!(
        table.lookup(HAWK, "ReplicatedMovement"),
        Some(FieldType::RepMovement {
            rotation: RotatorQuantization::ByteComponents,
            location: VectorQuantization::RoundWholeNumber,
        })
    );
    assert_eq!(table.lookup(HAWK, "Banking"), Some(FieldType::Double));
    assert_eq!(lookup_checksum(677106858), Some(FieldType::Double));
    // Still no name rule and no checksum for ReplicatedMovement as a whole:
    // byte and short donors both remain in the table (see
    // `only_the_seeker_nade_keeps_short_rotator_components`), so 2749104612
    // stays out of the checksum table.
    assert_eq!(lookup_checksum(2749104612), None);
}

/// Cypher's trapwire and cage classes were renamed in 13.01, and the five
/// descriptor-typed fields follow them: the same name, checksum and width on
/// both sides of the rename (see apply_type_corrections.py). The old paths
/// keep their entries -- 11.06-12.08 still carry them -- and `Deployed`'s
/// checksum is learned so that the next rename is caught without an entry.
/// `CreatedByCharacter` (2035145197) and `RelativeScale3D` (1992268157) are
/// deliberately not learned: every agent's ability classes carry them, and a
/// learned checksum would type all of those unmeasured.
#[test]
fn cypher_trap_fields_follow_the_13_01_rename() {
    const OLD_E: &str = "/Game/Characters/Gumshoe/S0/Ability_E/";
    const NEW_4: &str = "/Game/Characters/Gumshoe/S0/Ability_4/";
    let table = OverlayTable::new(&OVERLAY_TABLE);
    for wire in [
        "GameObject_Gumshoe_{}_TripWire.GameObject_Gumshoe_{}_TripWire_C",
        "GameObject_Gumshoe_{}_TripWire_SecondWire.GameObject_Gumshoe_{}_TripWire_SecondWire_C",
    ] {
        let old = format!("{OLD_E}{}", wire.replace("{}", "E"));
        let new = format!("{NEW_4}{}", wire.replace("{}", "4"));
        assert_eq!(
            table.lookup(&old, "Deployed"),
            Some(FieldType::Bool),
            "{old}"
        );
        assert_eq!(
            table.lookup(&new, "Deployed"),
            Some(FieldType::Bool),
            "{new}"
        );
    }
    for (old, new) in [
        (
            "/Game/Characters/Gumshoe/S0/Ability_E/Ability_Gumshoe_E_TripWire.Ability_Gumshoe_E_TripWire_C",
            "/Game/Characters/Gumshoe/S0/Ability_4/Ability_Gumshoe_4_TripWire.Ability_Gumshoe_4_TripWire_C",
        ),
        (
            "/Game/Characters/Gumshoe/S0/Ability_4/Ability_Gumshoe_4_CageTrap.Ability_Gumshoe_4_CageTrap_C",
            "/Game/Characters/Gumshoe/S0/Ability_Q/Ability_Gumshoe_Q_CageTrap.Ability_Gumshoe_Q_CageTrap_C",
        ),
    ] {
        for group in [old, new] {
            assert_eq!(
                table.lookup(group, "CreatedByCharacter"),
                Some(FieldType::ObjectNetGuid),
                "{group}"
            );
        }
    }
    for group in [
        "/Game/Characters/Gumshoe/S0/Ability_4/Ability_Gumshoe_4_CageTrap.Ability_Gumshoe_4_CageTrap_C",
        "/Game/Characters/Gumshoe/S0/Ability_Q/Ability_Gumshoe_Q_CageTrap.Ability_Gumshoe_Q_CageTrap_C",
    ] {
        assert_eq!(
            table.lookup(group, "RelativeScale3D"),
            Some(FieldType::VectorNetQuantize { scale: 100 }),
            "{group}"
        );
    }
    assert_eq!(lookup_checksum(3902815170), Some(FieldType::Bool));
    assert_eq!(lookup_checksum(2035145197), None);
    assert_eq!(lookup_checksum(1992268157), None);
    // A future path for the same wire resolves through the checksum alone.
    assert_eq!(
        resolve_field_type_with_checksum(
            &table,
            "/Game/Characters/Gumshoe/S0/Ability_C/GameObject_Gumshoe_C_TripWire.GameObject_Gumshoe_C_TripWire_C",
            Some("Deployed"),
            None,
            Some(3902815170)
        ),
        Some(FieldType::Bool)
    );
}

/// The five AGameObject smoke and zone classes read byte rotator components,
/// and the only table entry left with short ones is Gekko's Wingman, an
/// AShooterCharacter pawn.
///
/// The C# descriptors give the five the builder's ShortComponents default by
/// calling a bare `.ReplicatedMovement()`. None of them ever replicates a
/// rotation, so the wire cannot choose between the widths (13-J, 16-D); the
/// choice follows the game's own class data (13.06): all five derive
/// natively from AGameObject > AActor, no Blueprint default in their chains
/// writes `ReplicatedMovement`, and every AGameObject class whose rotation is
/// observable decodes at byte width only (AProjectile 38 of 38 byte,
/// AShooterCharacter 7 of 7 short). `apply_type_corrections.py`,
/// `retype_game_object_rotators`, has the evidence and the bound.
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
    let table = OverlayTable::new(&OVERLAY_TABLE);
    for group in GAME_OBJECTS {
        assert_eq!(
            table.lookup(group, "ReplicatedMovement"),
            Some(FieldType::RepMovement {
                rotation: RotatorQuantization::ByteComponents,
                location: VectorQuantization::RoundWholeNumber,
            }),
            "{group}"
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
    assert_eq!(lookup_checksum(REPLICATED_MOVEMENT_CHECKSUM), None);
}

#[test]
fn damage_geometry_fields_are_quantized_vectors() {
    // Same trap as EquippableUsed: DamageParameters attaches
    // ValorantPayloadDecoders.VectorNetQuantize* to these four, so
    // extract_descriptors.py cannot see the type and they land as Raw --
    // even though vrf-decode already implements the exact quantization.
    // Scales are the C# call sites: VectorNetQuantize = 1,
    // VectorNetQuantize100 = 100, VectorNetQuantizeNormal = unit vector.
    const BASE: &str = "/Script/ShooterGame.DamageableComponent:MulticastNotifyDamage_Base";
    const POINT: &str = "/Script/ShooterGame.DamageableComponent:MulticastNotifyDamage_Point";

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

    let table = OverlayTable::new(&OVERLAY_TABLE);
    for (group, field, want) in expected {
        assert_eq!(table.lookup(group, field), Some(want), "{field} in {group}");
    }
}

#[test]
fn overlay_falls_back_to_the_b_prefixed_boolean_name() {
    // The C# descriptors bind by handle and treat the name as a label, so
    // one spells a boolean `bDeathMontageEffectOverrideIsQueued` while the
    // replay declares it without the prefix. A name-keyed lookup misses,
    // and the field stayed raw on 581 rows.
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
    const GROUP: &str =
        "/Script/ShooterGame.ReplayEffectComponent:ReplayPlayContinuousEffectAtLocation";
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

/// A replay that declares a REAL, different name at a handle must not be typed
/// through the descriptor's stale mapping for that handle.
///
/// This is how a game patch moves a property: the descriptor still maps handle
/// 7 to `OldField: Int32`, the replay now declares `NewField` there and sends a
/// `Float`. Both name probes miss, and the handle fallback used to reuse the
/// stale entry -- so `1.0f32` came out as `value_i64 = 1065353216`, consuming
/// all 32 bits, with `decoded_ok` incremented and `Decode errors` still zero.
/// A confident wrong number, which is the one outcome this crate refuses.
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

/// The refusal must NOT fire on a bare decimal wire name. `"248"` is the
/// decimal spelling of a hardcoded Unreal FName index the replay never resolves
/// to text -- it declares nothing, so it cannot conflict, and the handle
/// fallback is the only thing that can type such a field.
///
/// A guard for the case pinned by
/// `overlay_falls_back_to_an_explicit_property_handle_when_the_wire_name_differs`.
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

/// The `b`-prefix probe resolves before the handle fallback is consulted, so a
/// `bFoo`/`Foo` spelling difference never reaches the new refusal. A guard:
/// this passed before the change and must keep passing.
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
fn lookup_returns_none_for_unknown() {
    let table = OverlayTable::new(&OVERLAY_TABLE);
    let ft = table.lookup("nonexistent", "field");
    assert_eq!(ft, None);
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

#[test]
fn apply_overlay_graceful_on_decode_failure() {
    let entries: &[OverlayEntry] = &[OverlayEntry {
        group_path: "/test",
        field_name: "Broken",
        field_type: FieldType::FString, // needs more than 1 bit
    }];
    let table = OverlayTable::new(entries);
    let mut stats = OverlayStats::default();
    let data = [0x01u8]; // only 1 bit -- FString needs at least 32 bits for length
    let result = apply_overlay(
        &table,
        "/test",
        group_hash_state("/test"),
        Some("Broken"),
        Some(&data),
        1,
        &mut stats,
    );
    // Should return Some but with all values None (decode failure)
    assert!(result.is_some());
    let r = result.unwrap();
    assert_eq!(r.value_i64, None);
    assert_eq!(r.value_str, None);
    assert_eq!(stats.decoded_err, 1);
}

/// The error report's `kind` is the only column that tells an operator WHY a
/// field failed -- `field_name` says which -- and the report is the permanent
/// schema-drift diagnostic. So each failure must print the label of its own
/// cause. Every `BitIo` error used to print `EOF`, including an invalid string
/// that consumed its payload exactly, and six value refusals printed
/// `Residual`, the label that means leftover bits.
///
/// Asserted on the printed label, because that is what reaches the operator.
///
/// Every `DecodeError` variant the overlay can meet has a case, and the test
/// checks that itself: `decode_field` is asked which variant each fixture
/// fails with, and the variants reached are compared with the list the
/// variant match below is generated from. That match has no wildcard, so a
/// new variant does not compile until it is listed, and a listed variant
/// with no case fails here. The cases used to be a hand-picked subset, and
/// four of the refusals (`UnsupportedTextHistory`, `NonFiniteComponent`,
/// `InvalidQuantizationScale`, `InvalidFNameNumber`) could go back to
/// `Residual` with this test still green.
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
    /// The payload of `fields`, each `(value, width)` written least
    /// significant bit first, the order `BitReader` reads them in.
    fn packed_bits(fields: &[(u64, u32)]) -> Payload {
        let mut bytes = Vec::new();
        let mut len = 0u32;
        for &(value, width) in fields {
            for bit in 0..width {
                if len % 8 == 0 {
                    bytes.push(0);
                }
                if (value >> bit) & 1 != 0 {
                    bytes[(len / 8) as usize] |= 1 << (len % 8);
                }
                len += 1;
            }
        }
        (bytes, len)
    }
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
    );
    static ENTRIES: [OverlayEntry; 13] = [
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
        entry(
            "ZeroQuantizeScale",
            FieldType::VectorNetQuantize { scale: 0 },
        ),
        entry("ZeroSerializedIntMax", FieldType::SerializedInt { max: 0 }),
    ];
    // An inline FName (hardcoded bit clear), "Source" with its null, then
    // instance number -1.
    let mut fname = vec![(0, 1), (7, 32)];
    fname.extend(b"Source\0".iter().map(|&byte| (u64::from(byte), 8)));
    fname.push((u64::from(-1i32 as u32), 32));
    let fname = packed_bits(&fname);
    // A 7-bit SerializedInt(128) header of 0 -- no component bits, no extra
    // info -- takes the raw-f32 fallback, and the first word is a NaN.
    let nan_vector = packed_bits(&[
        (0, 7),
        (0x7fc0_0000, 32),
        (u64::from(1.0f32.to_bits()), 32),
        (u64::from(2.0f32.to_bits()), 32),
    ]);
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
        // with one byte behind it. It used to print `EOF`, because the byte
        // loop ran into the end instead of the prefix being checked.
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
        // raising, which is why this variant was split from NotFullyConsumed.
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

/// Byte-sized properties nested inside replicated arrays are written with
/// only their significant bits, so the decoder must take its width from the
/// payload rather than assuming 8.
///
/// This is not hypothetical: `CombatReport` `AssistType` arrives as a 5-bit
/// payload, and a fixed 8-bit read left all 364 of its rows in a real replay
/// untyped while every neighbouring field decoded fine.
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
/// every key in the generated table and on keys that are not in it.
///
/// A wrong overlay type moves NO summary counter -- the row still emits and
/// the block still walks -- so nothing but an equivalence check like this
/// one would catch an index that quietly disagrees on a handful of entries.
#[test]
fn the_hash_index_answers_exactly_what_the_binary_search_answered() {
    let table = OverlayTable::with_handles(&OVERLAY_TABLE, &OVERLAY_HANDLE_TABLE);

    for entry in &OVERLAY_TABLE {
        // Every real key.
        assert_eq!(
            table.lookup(entry.group_path, entry.field_name),
            table.lookup_by_binary_search(entry.group_path, entry.field_name),
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
                table.lookup_b_prefixed(entry.group_path, probe),
                table.lookup_b_prefixed_by_binary_search(entry.group_path, probe),
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
                table.lookup(group, name),
                table.lookup_by_binary_search(group, name),
                "miss disagrees for {group}::{name}",
            );
            assert_eq!(
                table.lookup_b_prefixed(group, name),
                table.lookup_b_prefixed_by_binary_search(group, name),
                "b-prefixed miss disagrees for {group}::b{name}",
            );
        }
    }
}

/// Same equivalence for the 84-entry handle fallback table, including
/// handles that are not declared for a group that is.
#[test]
fn the_handle_index_answers_exactly_what_the_binary_search_answered() {
    let table = OverlayTable::with_handles(&OVERLAY_TABLE, &OVERLAY_HANDLE_TABLE);

    for entry in &OVERLAY_HANDLE_TABLE {
        for handle in [entry.handle, entry.handle.wrapping_add(1000), u32::MAX, 0] {
            assert_eq!(
                table.lookup_handle(entry.group_path, handle),
                table.lookup_handle_by_binary_search(entry.group_path, handle),
                "handle lookup disagrees for {}::{handle}",
                entry.group_path,
            );
        }
        assert_eq!(
            table.lookup_handle("/Game/NoSuchGroupPathAnywhere", entry.handle),
            None,
        );
    }
}

/// `BlindManagerComponent.LongestActiveBlindDuration` is a 32-bit float giving
/// the longest active flash-blind duration in seconds (0.0..2.1 on observed
/// data). Common to all player characters. Typed as Float.
#[test]
fn blind_duration_is_typed() {
    let table = OverlayTable::new(&OVERLAY_TABLE);
    assert_eq!(
        table.lookup(
            "/Script/ShooterGame.BlindManagerComponent",
            "LongestActiveBlindDuration"
        ),
        Some(FieldType::Float)
    );
}

/// `MulticastNotifyHeal` and `MulticastNotifyOverhealDecay` are RPC parameter
/// blocks whose group paths the wire registers as
/// `/Script/ShooterGame.DamageableComponent:MulticastNotifyHeal` and
/// `:MulticastNotifyOverhealDecay`. The DamageableComponent C# descriptor
/// (DamageableComponentClassNetCacheDescriptor.cs) only declares the two
/// `MulticastNotifyDamage_*` handles, so the heal/decay parameter groups ship
/// untyped even though their scalars decode cleanly. The RPC sink resolves
/// these under their colon-group path with the bare parameter name, the same
/// shape as the EquippableUsed correction.
///
/// On the 98605b1b Demos export `HealTaken` is 32 bits on all 1252 rows and
/// reads as Float a 0.05..400 heal magnitude -- the 0x3f800000 bit pattern
/// (1.0f, the IEEE-754 identity) recurs, which is a float signature no int
/// read produces. `DecayApplied` is 32 bits on all 699 rows and reads as Float
/// a 0.07..50 overheal-decay amount, clustering tightly around 0.195 (a
/// per-tick decay). No descriptor declares either field, so these are
/// ADDITIONS in the same wire-evidence class as `Money` and `Ping`.
#[test]
fn heal_and_overheal_decay_scalars_are_typed() {
    let table = OverlayTable::new(&OVERLAY_TABLE);
    assert_eq!(
        table.lookup(
            "/Script/ShooterGame.DamageableComponent:MulticastNotifyHeal",
            "HealTaken"
        ),
        Some(FieldType::Float)
    );
    assert_eq!(
        table.lookup(
            "/Script/ShooterGame.DamageableComponent:MulticastNotifyOverhealDecay",
            "DecayApplied"
        ),
        Some(FieldType::Float)
    );
}

/// `PlayerScoreComponent.Score` is the per-player combat score. No C#
/// descriptor declares the group. On 98605b1b it is 32 bits on all 430 rows.
/// Read as Float the bytes are denormal slop (~1e-44) -- the float read
/// rejects itself -- but read as Int32 the values run 21..5833 with 415
/// distinct values, exactly the shape of a cumulative combat score across a
/// full match. Typed as Int32. ADDITION, same wire-evidence class as `Money`.
#[test]
fn player_score_is_typed() {
    let table = OverlayTable::new(&OVERLAY_TABLE);
    assert_eq!(
        table.lookup("/Script/ShooterGame.PlayerScoreComponent", "Score"),
        Some(FieldType::Int32)
    );
}

/// The replay scoreboard publishes authoritative cumulative K/D/A counters
/// through this component. Each value is a 32-bit little-endian integer; if
/// these remain Raw, downstream consumers are forced to reconstruct the
/// scoreboard from lossy kill RPCs instead of using the server totals.
#[test]
fn basic_combat_stats_are_typed() {
    let table = OverlayTable::new(&OVERLAY_TABLE);
    let group = "/Script/ShooterGame.BasicCombatStatsComponent";
    for field in ["AggregateKills", "AggregateDeaths", "AggregateAssists"] {
        assert_eq!(
            table.lookup(group, field),
            Some(FieldType::Int32),
            "{field}"
        );
    }
}

/// `ZoomMultiplierComponent` drives the ADS/scope FOV transition. No C#
/// descriptor declares the group, so its properties ship raw even though the
/// values are textbook floats. On 98605b1b all five fields below are 32 bits
/// on every row with zero NaN:
///   - SourceFov/TargetFov: 20.6..103.0, and 103.0 is Valorant's documented
///     default hip-fire FOV (the mode the player is in when not ADS), so a
///     wrong type cannot produce it.
///   - SourceFov1P/TargetFov1P: 5.0..70.0, and 70.0 is the default 1P FOV.
///   - TotalTransitionTimeDuration: 0.0..0.25, the ADS transition time.
///
/// SourceZoomLevel/TargetZoomLevel are deliberately NOT typed: ~70% of their
/// rows are the 0xFFFFFFFF sentinel, which Float reads as NaN, and the rest
/// are 0.0, so the field is an enum-or-sentinel, not a clean float. These
/// five are ADDITIONS, same wire-evidence class as `Money`.
#[test]
fn zoom_multiplier_fov_fields_are_typed() {
    let table = OverlayTable::new(&OVERLAY_TABLE);
    let group = "/Script/ShooterGame.ZoomMultiplierComponent";
    assert_eq!(table.lookup(group, "SourceFov"), Some(FieldType::Float));
    assert_eq!(table.lookup(group, "TargetFov"), Some(FieldType::Float));
    assert_eq!(table.lookup(group, "SourceFov1P"), Some(FieldType::Float));
    assert_eq!(table.lookup(group, "TargetFov1P"), Some(FieldType::Float));
    assert_eq!(
        table.lookup(group, "TotalTransitionTimeDuration"),
        Some(FieldType::Float)
    );
}

/// `UsableComponent` drives every hold-to-interact object: spike plant/defuse,
/// ultimate-orb pickup, doors. No C# descriptor declares the group. On a bomb
/// replay `HighestProgress` is 32 bits on ~12k rows and reads as Float a clean
/// 0..1 ramp advancing 1/128 per tick (a u32 read is non-monotonic; only the
/// float read is linear), and `bIsActive` is a single 0x01 bit on ~150 rows --
/// the "someone is interacting" flag. ADDITIONS, same wire-evidence class as
/// `Money`.
#[test]
fn usable_component_interaction_is_typed() {
    let table = OverlayTable::new(&OVERLAY_TABLE);
    let group = "/Script/ShooterGame.UsableComponent";
    assert_eq!(
        table.lookup(group, "HighestProgress"),
        Some(FieldType::Float)
    );
    assert_eq!(table.lookup(group, "bIsActive"), Some(FieldType::Bool));
}

/// Ammo used to need a hand-written handle name and no longer does.
///
/// `MagazineAmmo` and `ReserveAmmo` are bare groups the replay never names --
/// every row is handle 2 with field_name None -- so `HANDLE_ADDITIONS` called
/// handle 2 `AmmoCount` and typed it Int32. The cooked game says both are
/// `AmmoComponent`, which the replay *does* declare, with handle 2 as
/// `AuthResourceAmount`. The leaf remap in `vrfkit`'s `sink/paths.rs` sends
/// them there, so the guess is gone and the real declaration does the work.
///
/// What is pinned here is the destination: the group the remap targets carries
/// the name and the type, so a regression in the table shows up as this test
/// rather than as silently unnamed handles.
#[test]
fn the_ammo_component_declares_the_handle_the_bare_groups_land_on() {
    let table = OverlayTable::with_handles(&OVERLAY_TABLE, &OVERLAY_HANDLE_TABLE);
    const GROUP: &str = "/Script/ShooterGame.AmmoComponent";
    assert_eq!(
        table.lookup(GROUP, "AuthResourceAmount"),
        Some(FieldType::Int32),
    );

    let mut stats = OverlayStats::default();
    let data = 12i32.to_le_bytes();
    let result = apply_overlay_with_handle(
        &table,
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

/// `FiniteSpeedMovementComponent` drives projectile travel. No C# descriptor
/// declares the group. `MaximumRange` is the projectile's max travel distance
/// in Unreal units: 32 bits on all 11699 rows on 98605b1b, reads as Float a
/// 397.6..49986.1 range with the mode at ~19993 UU (~500 m), which is the
/// right order of magnitude for a Valorant projectile. Typed as Float.
/// (bIsActive is deliberately NOT typed despite a clean 1-bit width: all 574
/// rows are 0x01, so the field carries no information a consumer can use, and
/// widening the table for a constant is not worth it.) ADDITION, same
/// wire-evidence class as `Money`.
#[test]
fn finite_speed_movement_max_range_is_typed() {
    let table = OverlayTable::new(&OVERLAY_TABLE);
    assert_eq!(
        table.lookup(
            "/Script/ShooterGame.FiniteSpeedMovementComponent",
            "MaximumRange"
        ),
        Some(FieldType::Float)
    );
}

/// `Owner`, `Instigator`, `AttachParent` and `Controller` are `AActor` /
/// `USceneComponent` object references Unreal replicates on every actor, always
/// as a NetGUID. The C# descriptors declare them only for the classes they
/// happen to cover, so on 02d4d478 they are typed on 129 group/field pairs
/// (4,601 rows) and untyped on 203 more (6,048 rows) -- same four names, same
/// encoding, no table entry. The type does not vary by class, so it resolves by
/// name once the table has missed on both the group and its alias.
#[test]
fn an_engine_object_ref_resolves_on_a_group_the_table_never_saw() {
    let table = OverlayTable::with_handles(&OVERLAY_TABLE, &OVERLAY_HANDLE_TABLE);
    const BOMB_EQUIPPABLE: &str = "/Game/Equippables/Bomb/BombEquippable.BombEquippable_C";
    assert_eq!(
        table.lookup(BOMB_EQUIPPABLE, "Owner"),
        None,
        "not in the table"
    );
    assert_eq!(
        resolve_field_type(&table, BOMB_EQUIPPABLE, Some("Owner"), None),
        Some(FieldType::ObjectNetGuid),
    );
}

#[test]
fn the_engine_fallback_covers_every_one_of_the_four_names() {
    let table = OverlayTable::with_handles(&OVERLAY_TABLE, &OVERLAY_HANDLE_TABLE);
    for name in ["Owner", "Instigator", "AttachParent", "Controller"] {
        assert_eq!(
            resolve_field_type(&table, "/Game/NeverSeen.NeverSeen_C", Some(name), None),
            Some(FieldType::ObjectNetGuid),
            "{name} should resolve by name",
        );
    }
}

/// The fallback is a fixed list, not "anything that looks like a reference".
#[test]
fn the_engine_fallback_does_not_invent_other_names() {
    let table = OverlayTable::with_handles(&OVERLAY_TABLE, &OVERLAY_HANDLE_TABLE);
    for name in ["OwnerId", "Owner2", "MyOwner", "Parent", "Target"] {
        assert_eq!(
            resolve_field_type(&table, "/Game/NeverSeen.NeverSeen_C", Some(name), None),
            None,
            "{name} must stay unresolved",
        );
    }
}

/// A declared entry still wins: the fallback only runs after the table misses,
/// so a class that really does spell one of these names differently keeps its
/// declared type.
#[test]
fn a_table_entry_outranks_the_engine_fallback() {
    let entries: &[OverlayEntry] = &[OverlayEntry {
        group_path: "/test",
        field_name: "Owner",
        field_type: FieldType::Raw,
    }];
    let table = OverlayTable::new(entries);
    assert_eq!(
        resolve_field_type(&table, "/test", Some("Owner"), None),
        Some(FieldType::Raw),
    );
}

/// The 192-bit RPC vectors. Unreal splits an `FTransform` parameter into three
/// separate double vectors on this wire, and no descriptor declares any of
/// them, so 54,859 rows on 02d4d478 arrived raw. Read as 3 x f64 they are
/// unambiguous -- `Scale3D` is exactly (1,1,1) on every row, which no other
/// split produces. ADDITIONS, same wire-evidence class as `Money`.
///
/// The replay's own `compatible_checksum` agrees with the grouping and was not
/// used to derive it: `248` is 598402184 wherever it appears, `249` is
/// 747197698, `Translation` 2235276067, `Scale3D` 2983776962.
///
/// `249` here is the FTransform's `Rotation`, an FQuat sent as X/Y/Z (W is
/// implied), not a rotator: 747197698 reproduces as `Transform: FTransform ->
/// Rotation: FQuat` (tools/tests/test_compatible_checksum_facts.py). The bits
/// read the same as any 3 x f64; only the meaning was wrong.
#[test]
fn the_rpc_transform_vectors_are_typed() {
    let table = OverlayTable::new(&OVERLAY_TABLE);
    for (group, field) in [
        (
            "/Script/ShooterGame.EffectManagerComponent:MulticastPlayContinuousEffect",
            "Scale3D",
        ),
        (
            "/Script/ShooterGame.EffectManagerComponent:MulticastPlayContinuousEffect",
            "Translation",
        ),
        (
            "/Script/ShooterGame.EffectManagerComponent:MulticastPlayContinuousEffect",
            "249",
        ),
        (
            "/Script/ShooterGame.LocationalEffectManagerComponent:ClientPlayOneShotEffectAtLocation",
            "248",
        ),
        (
            "/Game/GameModes/Components/Comp_BombEvents.Comp_BombEvents_C:BombPlantedRPC",
            "PlantLocation",
        ),
        (
            "/Game/GameModes/Bomb/BombDestination.BombDestination_C:MulticastActivateBombSiteEffects",
            "BombLocation",
        ),
    ] {
        assert_eq!(
            table.lookup(group, field),
            Some(FieldType::VectorDouble),
            "{group}:{field}",
        );
    }
}

/// The decode that makes the reading unambiguous: the bytes below are the
/// `Scale3D` payload every row carries, and only a 3 x f64 split reads them as
/// (1,1,1). Six f32s would give (0, 1.875, 0, 1.875, 0, 1.875).
#[test]
fn a_192_bit_rpc_vector_decodes_as_three_doubles() {
    const GROUP: &str = "/Script/ShooterGame.EffectManagerComponent:MulticastPlayContinuousEffect";
    let table = OverlayTable::new(&OVERLAY_TABLE);
    let mut stats = OverlayStats::default();
    let mut bits = Vec::new();
    for _ in 0..3 {
        bits.extend_from_slice(&1.0f64.to_le_bytes());
    }
    let result = apply_overlay(
        &table,
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

/// Checksum propagation: a parameter no descriptor declares takes the type of
/// a declared field sharing its `compatible_checksum`.
///
/// `PlayerID` on `ReplayPlayerController:ClientReplayReceiveInputEvent`
/// `ProcessingCapture` is undeclared, and `BombPlayerState_C.PlayerId` is
/// declared `Int32`. They share checksum 2396673102, and reading the
/// undeclared rows as `Int32` yields exactly the ten values the declared column
/// holds.
#[test]
fn a_checksum_types_a_field_the_table_never_declared() {
    const UNDECLARED: &str =
        "/Script/ShooterGame.ReplayPlayerController:ClientReplayReceiveInputEventProcessingCapture";
    let table = OverlayTable::with_handles(&OVERLAY_TABLE, &OVERLAY_HANDLE_TABLE);
    assert_eq!(table.lookup(UNDECLARED, "PlayerID"), None, "not declared");
    assert_eq!(
        resolve_field_type_with_checksum(
            &table,
            UNDECLARED,
            Some("PlayerID"),
            None,
            Some(2396673102)
        ),
        Some(FieldType::Int32),
    );
}

/// The checksum runs last, so anything the table declares still wins.
#[test]
fn a_declared_entry_outranks_the_checksum() {
    let entries: &[OverlayEntry] = &[OverlayEntry {
        group_path: "/test",
        field_name: "PlayerID",
        field_type: FieldType::Raw,
    }];
    let table = OverlayTable::new(entries);
    assert_eq!(
        resolve_field_type_with_checksum(&table, "/test", Some("PlayerID"), None, Some(2396673102)),
        Some(FieldType::Raw),
    );
}

/// A checksum nothing donated types nothing -- the map asserts only what it
/// learned.
#[test]
fn an_unlearned_checksum_resolves_nothing() {
    let table = OverlayTable::with_handles(&OVERLAY_TABLE, &OVERLAY_HANDLE_TABLE);
    assert_eq!(
        resolve_field_type_with_checksum(
            &table,
            "/Game/Nope.Nope_C",
            Some("Whatever"),
            None,
            Some(1)
        ),
        None,
    );
}

/// The safety property: a checksum whose donors disagree is not in the table at
/// all, so the mechanism declines the cases it cannot settle. `ReplicatedMovement`
/// is the one that matters -- `ByteComponents` on 25 groups and `ShortComponents`
/// on 1, which differ in width, so guessing would desync the block; and that
/// one group packs its location at two decimals where the rest pack whole
/// units, which a guess would read 100x off with no error at all.
///
/// `AllianceFilter` used to be the second entry here and is not any more: its
/// donors disagreed only in the table, never on the wire -- see
/// `alliance_filter_donors_agree_so_the_checksum_types_the_receivers`.
#[test]
fn checksums_whose_donors_disagree_are_omitted() {
    assert_eq!(
        lookup_checksum(2749104612),
        None,
        "ReplicatedMovement: Byte vs Short components"
    );
}

/// `AllianceFilter` is one property, checksum 2270825073, declared by three
/// effect RPCs and received by five more: the weapon `...FromClient` pair,
/// `ReplayPlayOneShotEffectAtLocation` and both `ReplayRecord*Effect`.
///
/// The descriptors typed the three donors two ways -- `EnumByte` on the two
/// `EffectManagerComponent` multicasts, `EnumRemainingBits` on
/// `ReplayPlayContinuousEffectAtLocation` -- so the checksum learner dropped
/// the checksum and the receivers shipped raw: 4,560,248 rows over the 1,018
/// replays audited at 259ed10, every one of the 16,030,813 rows under this
/// checksum 3 bits wide, where both readers return the same number. A
/// correction makes the donors agree. This pins both halves: the donors, and
/// the propagation that only a regenerated `checksum_table.rs` delivers -- a
/// corrected table with a stale checksum table would still leave the
/// receivers raw.
#[test]
fn alliance_filter_donors_agree_so_the_checksum_types_the_receivers() {
    let table = OverlayTable::with_handles(&OVERLAY_TABLE, &OVERLAY_HANDLE_TABLE);
    for group in [
        "/Script/ShooterGame.EffectManagerComponent:MulticastPlayContinuousEffect",
        "/Script/ShooterGame.EffectManagerComponent:MulticastPlayOneShotEffect",
        "/Script/ShooterGame.ReplayEffectComponent:ReplayPlayContinuousEffectAtLocation",
    ] {
        assert_eq!(
            table.lookup(group, "AllianceFilter"),
            Some(FieldType::EnumByte),
            "donor {group}"
        );
    }
    assert_eq!(lookup_checksum(2270825073), Some(FieldType::EnumByte));

    const RECEIVER: &str =
        "/Script/ShooterGame.AresEquippable:MulticastPlayContinuousEffectFromClient";
    assert_eq!(
        table.lookup(RECEIVER, "AllianceFilter"),
        None,
        "typed by checksum, not by name"
    );
    assert_eq!(
        resolve_field_type_with_checksum(
            &table,
            RECEIVER,
            Some("AllianceFilter"),
            None,
            Some(2270825073)
        ),
        Some(FieldType::EnumByte),
    );
}

/// The weapon effect RPCs name the `EffectManager` of the pawn holding the
/// weapon. Checksum 1051633025, declared only by these two functions: all
/// 3,112,054 rows in the 1,018-replay audit are IntPacked 16/24-bit GUIDs that
/// resolve in the same export's `net_guids` to `EffectManager`, whose outer is
/// the holder's `*_PC_C` (or Yoru's decoy). Both twins are pinned: typing only
/// one would be the "Viper typed, Phoenix not" shape this table has shipped
/// before.
#[test]
fn the_weapon_effect_rpcs_type_their_effect_manager_reference() {
    let table = OverlayTable::new(&OVERLAY_TABLE);
    for group in [
        "/Script/ShooterGame.AresEquippable:MulticastPlayContinuousEffectFromClient",
        "/Script/ShooterGame.AresEquippable:MulticastPlayOneShotEffectFromClient",
    ] {
        assert_eq!(
            table.lookup(group, "EffectManagerComponent"),
            Some(FieldType::ObjectNetGuid),
            "{group}"
        );
    }
    assert_eq!(lookup_checksum(1051633025), Some(FieldType::ObjectNetGuid));
}

/// Every group the overlay assigns a `RepMovement` type to -- a table entry or
/// an exact scoped type -- with the location level the wire was measured at
/// for that class.
///
/// Measured 2026-09-28 over the 1,018 replays audited at 259ed10 (21 of the
/// 24 builds carry these rows): each actor `open` in actors.parquet joined to
/// the actor's first `ReplicatedMovement` row at the same `time_ms` on the
/// same channel, spawn position at least 50 units from the origin, comparing
/// |packed location integer| with |spawn xyz|. The count is those joins, then
/// the builds they span; the ratio is the level's divisor on every build.
/// Checkpoint tables carry no `ReplicatedMovement` rows at all, so this is
/// main-stream evidence only. docs/DATA.md has the method in full.
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
        (
            "/Game/Characters/Clay/S0/Ability_E/Pawn_Clay_E_Boomba.Pawn_Clay_E_Boomba_C",
            RoundTwoDecimals,
        ),
        // Table entry (apply_type_corrections.py ADDITIONS). 8,265 joins, 15
        // builds, ratio 1.000 (p1-p99 0.9998-1.0001), every component within
        // 0.50 of spawn; re-measured at integration with an independent join.
        (
            "/Game/Characters/Guide/S0/Ability_E/Projectile_Guide_E_HawkFlash.Projectile_Guide_E_HawkFlash_C",
            RoundWholeNumber,
        ),
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

/// The location level of every `RepMovement` type the overlay can assign is
/// the measured one, and no group gets a `RepMovement` type this list does not
/// name.
///
/// The level is not on the wire, so the generator has to default it (whole
/// units, extract_descriptors.py REP_MOVEMENT_LOCATION), and a default is a
/// prior, not a measurement. Pinning each class keeps a changed default or a
/// dropped correction from moving a class silently; failing on an unlisted
/// group keeps a NEW class from taking the default without anyone checking
/// it -- add it here only with its spawn-position evidence.
///
/// Three routes can assign the type, and all three are held to the list: the
/// table and the exact scoped types by group, checksum propagation by
/// admitting no `RepMovement` at all. The scoped route matters because it is
/// generated from its own fixture, where a new `RepMovement` literal would
/// never pass through the table's default or its corrections.
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

/// `StopMovementTime` is the other half of a pair whose `StartMovementTime` is
/// already Float: same RPC family, 32 bits on all 13,316 rows, and the same
/// shape read as f32 -- a -1.0 sentinel on 5,371 of them and 0.76..136.98 on
/// the rest, against -1.0..1771.83 for the declared sibling.
///
/// `HandleNumber` identifies a force module so a later Remove/Cleanup RPC can
/// name it. Read as u32 its 3,741 rows hold 1..765 with every value present --
/// a dense sequential id, which no other reading of those bits produces.
///
/// One entry each: checksum propagation carries `StopMovementTime` to
/// `ReplayStopContinuousEffectAtLocation` (244888268) and `HandleNumber` to
/// `NetMulticastRemoveForceModule` (3336285386).
///
/// `HandleNumber` is unsigned: 3336285386 reproduces only as
/// `Handle: FForceModuleHandle -> HandleNumber: uint32`
/// (tools/tests/test_compatible_checksum_facts.py), so both the Apply entry and
/// the checksum that carries it to Remove say `UInt32`.
#[test]
fn the_movement_time_pair_and_force_module_handle_are_typed() {
    let table = OverlayTable::new(&OVERLAY_TABLE);
    assert_eq!(
        table.lookup(
            "/Script/ShooterGame.EffectManagerComponent:MulticastStopContinuousEffect",
            "StopMovementTime"
        ),
        Some(FieldType::Float),
    );
    assert_eq!(
        table.lookup(
            "/Script/ShooterGame.ForceModuleManagerComponent:NetMulticastApplyForceModule",
            "HandleNumber"
        ),
        Some(FieldType::UInt32),
    );
    assert_eq!(lookup_checksum(3336285386), Some(FieldType::UInt32));
}

/// `EffectID` is the `int64` member of `FEffectID`, not the `ulong` the C#
/// descriptors declare: each replay checksum below reproduces with `int64`
/// and not with `uint64` (tools/tests/test_compatible_checksum_facts.py has
/// the chains), and the 13.06 executable's reflection agrees.
///
/// The table entries are the donors; the checksum table carries the type on to
/// the RPCs that declare no entry of their own (`MulticastStopContinuousEffect`,
/// the weapons' `MulticastPlayContinuousEffectFromClient`,
/// `ReplayStopContinuousEffectAtLocation`). Both halves are pinned because the
/// CI checksum guard compares a one-group fixture and cannot see a checksum
/// table left behind by a retyped donor.
#[test]
fn effect_ids_are_signed_on_every_donor_and_in_the_checksum_table() {
    let table = OverlayTable::new(&OVERLAY_TABLE);
    for (group, checksum) in [
        ("/Script/ShooterGame.EffectManagerComponent", 1129645208),
        (
            "/Script/ShooterGame.EffectManagerComponent:MulticastPlayContinuousEffect",
            2340855891,
        ),
        (
            "/Script/ShooterGame.EffectManagerComponent:MulticastUpdateContinuousEffect",
            2340855891,
        ),
        (
            "/Script/ShooterGame.ReplayEffectComponent:ReplayPlayContinuousEffectAtLocation",
            2251343646,
        ),
    ] {
        assert_eq!(
            table.lookup(group, "EffectID"),
            Some(FieldType::Int64),
            "{group}"
        );
        assert_eq!(
            lookup_checksum(checksum),
            Some(FieldType::Int64),
            "{checksum}"
        );
    }
    assert_eq!(
        resolve_field_type_with_checksum(
            &table,
            "/Script/ShooterGame.EffectManagerComponent:MulticastStopContinuousEffect",
            Some("EffectID"),
            None,
            Some(2340855891)
        ),
        Some(FieldType::Int64),
    );
    assert!(
        !OVERLAY_TABLE
            .iter()
            .any(|e| e.field_type == FieldType::UInt64),
        "no entry should still read an int64 property as UInt64"
    );
}

/// The rest of `NetMulticastApplyForceModule`'s parameters, and the one
/// `NetMulticastRemoveForceModule` parameter that only the checksum reaches.
///
/// Measured over the 1,018-replay audit (665,519 Apply rows, all main
/// stream): `RespawnNumber` is 32 bits reading 0..38 and equals the same
/// character's typed `AresInventory.RespawnNumber` on 665,363 of 665,370
/// comparable rows; `NetTimestamp` is 32 bits of finite f32 on the 1/128 s
/// tick grid; `ModuleType` is 3 bits reading {0, 2}; `Module` and `Character`
/// are IntPacked GUIDs -- every `Module` resolves to a `ForceModule_*` class
/// and every `Character` equals the row's own actor GUID.
///
/// `ModuleType` is declared once, on Apply. Remove carries the same property
/// (checksum 3263282135) with no entry of its own, so its 2,743,504 rows are
/// typed only if the regenerated checksum table learned the Apply donor --
/// the same route `HandleNumber` takes above. Paired by (object, handle),
/// Remove and Apply agree on 647,381 of 647,381 rows.
#[test]
fn the_force_module_apply_parameters_are_typed_and_remove_follows_by_checksum() {
    const APPLY: &str =
        "/Script/ShooterGame.ForceModuleManagerComponent:NetMulticastApplyForceModule";
    const REMOVE: &str =
        "/Script/ShooterGame.ForceModuleManagerComponent:NetMulticastRemoveForceModule";
    let table = OverlayTable::with_handles(&OVERLAY_TABLE, &OVERLAY_HANDLE_TABLE);
    for (field, expected) in [
        ("RespawnNumber", FieldType::Int32),
        ("NetTimestamp", FieldType::Float),
        ("ModuleType", FieldType::EnumByte),
        ("Module", FieldType::ObjectNetGuid),
        ("Character", FieldType::ObjectNetGuid),
    ] {
        assert_eq!(table.lookup(APPLY, field), Some(expected), "Apply {field}");
    }
    assert_eq!(lookup_checksum(3263282135), Some(FieldType::EnumByte));
    assert_eq!(
        table.lookup(REMOVE, "ModuleType"),
        None,
        "typed by checksum, not by name"
    );
    assert_eq!(
        resolve_field_type_with_checksum(
            &table,
            REMOVE,
            Some("ModuleType"),
            None,
            Some(3263282135)
        ),
        Some(FieldType::EnumByte),
    );
    // The component's own `RespawnNumber` property (checksum 3044239005) is a
    // different property on a different group; the RPC entry does not reach it.
    assert_eq!(
        table.lookup(
            "/Script/ShooterGame.ForceModuleManagerComponent",
            "RespawnNumber"
        ),
        None
    );
}

/// `ReadyingStateComponent.AuthEquipSpeed` and the two inventory correction
/// counters. Measured over the 1,018-replay audit: `AuthEquipSpeed` is 3 bits
/// on all 1,015,515 rows (main {0,1,2}, checkpoints always 0), matching its
/// descriptor-typed sibling `AutoEquipTransitionContext.AutoEquipSpeed` in the
/// same packet; `CorrectionIndex` and `LastSeenClientCorrectionIndex` are 32
/// bits on all 1,222,930 / 1,129,599 rows, strictly increasing per actor, with
/// `LastSeen <= Correction - 1` on every paired row.
#[test]
fn readying_speed_and_inventory_correction_counters_are_typed() {
    let table = OverlayTable::new(&OVERLAY_TABLE);
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
        assert_eq!(table.lookup(group, field), Some(expected), "{field}");
    }
    for (checksum, expected) in [
        (3151779304u32, FieldType::EnumByte),
        (3198546915, FieldType::Int32),
        (1076231069, FieldType::Int32),
    ] {
        assert_eq!(lookup_checksum(checksum), Some(expected), "{checksum}");
    }
}

/// Which named area of the map a player is standing in -- "A Site", "Mid",
/// "Heaven", the callouts the game itself announces.
///
/// The group only became reachable when the `CalloutRegionTracker` leaf was
/// remapped to its native class. The field is an `ObjectNetGuid`: unpacking the
/// raw bits of all 1,957 non-zero rows and resolving them through
/// `net_guids.parquet` gives 1,957 of 1,957 a `CalloutRegion_*` path, over 22
/// distinct regions. Nothing else in the export says where a player is in map
/// terms rather than in centimetres.
#[test]
fn the_callout_region_is_typed() {
    let table = OverlayTable::new(&OVERLAY_TABLE);
    assert_eq!(
        table.lookup(
            "/Script/ShooterGame.CalloutRegionTrackingComponent",
            "CurrentRegion"
        ),
        Some(FieldType::ObjectNetGuid),
    );
}

/// The per-cast ability log -- who cast what, when, and where.
///
/// This repo twice concluded there is no exact cast count on the wire. There
/// is: `Comp_AbilityStatisticsReplicator` replicates one record per cast, and
/// vrfkit was already flattening it into `AbilityCastsThisRound[i].<member>`
/// rows. Every value arrived raw, so a survey that scans the typed columns
/// walked straight past a fully named array.
///
/// The member names carry Blueprint property GUIDs. Those are stable --
/// byte-identical on 13.01 and 13.02 -- which is what makes pinning them safe.
///
/// Each member is corroborated by something outside itself: `Player`'s 352
/// values are all UUIDs and all match a `manifest.players.subject`, `Round`
/// covers exactly 0..17 for an 18-round match, `CastLocation` reads as 3 x f64
/// inside the map bounds `movement.parquet` describes, and `Slot` takes four
/// values for three abilities plus an ultimate.
#[test]
fn the_ability_cast_log_is_typed() {
    const GROUP: &str = "/Game/Characters/_Core/Comp_AbilityStatisticsReplicator\
.Comp_AbilityStatisticsReplicator_C";
    let table = OverlayTable::new(&OVERLAY_TABLE);
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
        assert_eq!(table.lookup(GROUP, field), Some(expected), "{field}");
    }
}

/// Both smoke walls are typed, not just the one that was noticed.
///
/// `MulticastAddSmokeScreenPoint` is declared by two classes -- Viper's
/// `SmokeScreenManager` and Phoenix's `FlameWallManager` -- and only Viper's
/// was in the table. Phoenix's `Translation` and `Scale3D` came out null on
/// 2,791 rows across 31 replays with decode errors at 0, and because Viper's
/// side decoded fine the ability looked handled.
///
/// The checksum fallback could not rescue it and should not have: Unreal gives
/// the two classes' properties different checksums (2794273677 / 1639439377
/// against Viper's 2235276067 / 2983776962), so it refused rather than
/// guessing -- fail-closed working exactly as designed, which is why this
/// needed a name-level fix.
///
/// The bar for admitting them is the repo's usual one, read off the wire:
/// every row is 192 bits (3 x f64), `Translation` decodes to map coordinates
/// (7211.7, 1670.3, 96.0), and `Scale3D` is (1,1,1) on every row -- a value no
/// other reading produces.
#[test]
fn both_classes_declaring_the_smoke_point_rpc_are_typed() {
    const VIPER: &str = "/Game/Characters/Pandemic/S0/Ability_E/\
GameObject_Pandemic_E_SmokeScreenManager.GameObject_Pandemic_E_SmokeScreenManager_C\
:MulticastAddSmokeScreenPoint";
    const PHOENIX: &str = "/Game/Characters/Phoenix/S0/Ability_Q/Production/\
GameObject_Phoenix_Q_FlameWallManager_Production.\
GameObject_Phoenix_Q_FlameWallManager_Production_C:MulticastAddSmokeScreenPoint";
    let table = OverlayTable::new(&OVERLAY_TABLE);
    for group in [VIPER, PHOENIX] {
        for field in ["Translation", "Scale3D"] {
            assert_eq!(
                table.lookup(group, field),
                Some(FieldType::VectorDouble),
                "{group} {field}"
            );
        }
    }
}

/// The weapon classes declare the same property as everything else.
///
/// `"215"` and `"216"` are not handles -- they are field *names*, the decimal
/// spelling of a hardcoded Unreal FName index the replay never resolves to
/// text. 353 groups decode them; 17 weapon groups did not, because `table.rs`
/// pinned those to `Raw` and a name hit wins before the checksum fallback is
/// ever consulted. 48,010 rows over 20 replays.
///
/// The checksums settle it: `1710918439` and `4109980037` on every group,
/// decoding or not, at a uniform 3 bits, and where it decodes the value is
/// always 3 and 1. Same checksum means Unreal hashed the same property, so
/// `Raw` on a weapon was never a different type -- it was a guess. The comment
/// that introduced it said "Weapons use Raw (correct)" and nothing had checked.
#[test]
fn the_weapon_classes_type_215_and_216_like_everything_else() {
    const WEAPONS: [&str; 3] = [
        "/Game/Equippables/Guns/Rifles/AK/AssaultRifle_AK.AssaultRifle_AK_C",
        "/Game/Equippables/Guns/Sidearms/BasePistol/BasePistol.BasePistol_C",
        "/Game/Equippables/Melee/Ability_Melee_Base.Ability_Melee_Base_C",
    ];
    const ALREADY_TYPED: &str = "/Game/GameModes/Bomb/TimedBomb.TimedBomb_C";
    let table = OverlayTable::new(&OVERLAY_TABLE);
    for group in WEAPONS.iter().chain(std::iter::once(&ALREADY_TYPED)) {
        for field in ["215", "216"] {
            assert_eq!(
                table.lookup(group, field),
                Some(FieldType::EnumRemainingBits),
                "{group} {field}"
            );
        }
    }
}

/// The parameter after `248` is the rotation, on all five RPCs that send it.
///
/// `248` is already typed `VectorDouble` -- the placement location -- and `249`
/// follows it, unnamed, on 441,814 rows over 20 replays. Three things settle
/// it. The widths are 3, 19, 35 and 51 bits, which is exactly
/// `3 + 16 x (flags set)` for `RotationShort`'s three conditional components
/// and nothing else. Decoding all 441,814 that way consumes every payload
/// exactly, with no leftover on any of the four widths. And the same UFunction
/// declares this parameter by name on builds where the replay names it: the
/// table already carries `ReplayPlayContinuousEffectAtLocation.Rotation` as
/// `RotationShort`.
///
/// It is not the other `249`. That one is the FQuat X/Y/Z of an FTransform, a
/// `VectorDouble` under 747197698; this family shares 2526428638 -- a
/// top-level `Rotation: FRotator` -- and is 19 bits, not 192.
#[test]
fn the_effect_placement_rotation_is_typed_on_every_rpc_that_sends_it() {
    const GROUPS: [&str; 5] = [
        "/Script/ShooterGame.LocationalEffectManagerComponent:ClientPlayOneShotEffectAtLocation",
        "/Script/ShooterGame.ReplayEffectComponent:ReplayPlayContinuousEffectAtLocation",
        "/Script/ShooterGame.ReplayEffectComponent:ReplayPlayOneShotEffectAtLocation",
        "/Script/ShooterGame.EffectManagerComponent:ReplayRecordOneShotEffect",
        "/Script/ShooterGame.EffectManagerComponent:ReplayRecordContinuousEffect",
    ];
    let table = OverlayTable::new(&OVERLAY_TABLE);
    for group in GROUPS {
        assert_eq!(
            table.lookup(group, "249"),
            Some(FieldType::RotationShort),
            "{group}"
        );
    }
    // The location it pairs with, unchanged, on the four that send it numbered.
    // `ReplayPlayContinuousEffectAtLocation` is the exception and the reason
    // this typing is safe: that one names both parameters, and its `Rotation`
    // is already `RotationShort`. The numbered spelling now agrees with the
    // named one on the same function.
    for group in GROUPS {
        if group.ends_with("ReplayPlayContinuousEffectAtLocation") {
            assert_eq!(
                table.lookup(group, "Rotation"),
                Some(FieldType::RotationShort),
                "{group} names its rotation and must still agree"
            );
            continue;
        }
        assert_eq!(
            table.lookup(group, "248"),
            Some(FieldType::VectorDouble),
            "{group} 248"
        );
    }
}

/// The RNG component's seed is an Int32.
///
/// 120,853 rows, one group, one checksum, 32 bits on every row, and 120,852 of
/// the 120,853 values distinct across the full i32 range -- which is what a
/// seed looks like and what nothing else does. The sibling
/// `AuthInitialRandomSeed` on the same component matches in width and in that
/// near-total distinctness.
///
/// Int32 over UInt32 is not settled by the data -- the same 32 bits read either
/// way -- and follows Unreal's `FRandomStream`, whose seed is an `int32`.
#[test]
fn the_random_number_generator_seed_is_typed() {
    let table = OverlayTable::new(&OVERLAY_TABLE);
    assert_eq!(
        table.lookup(
            "/Script/ShooterGame.NetworkedRandomNumberGeneratorComponent",
            "AuthCurrentRandomSeed"
        ),
        Some(FieldType::Int32)
    );
}

#[test]
fn targeting_vectors_and_heal_causer_require_exact_scoped_checksums() {
    let table = OverlayTable::new(&OVERLAY_TABLE);
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
            "/Script/ShooterGame.DamageableComponent:MulticastNotifyHeal",
            "HealCauser",
            546618027,
            FieldType::ObjectNetGuid,
        ),
    ];
    for (group, field, checksum, expected) in cases {
        assert_eq!(
            resolve_field_type_with_checksum(&table, group, Some(field), None, Some(checksum)),
            Some(expected)
        );
        assert_eq!(
            resolve_field_type_with_checksum(&table, group, Some(field), None, Some(checksum ^ 1)),
            None
        );
        assert_eq!(
            resolve_field_type_with_checksum(&table, "/wrong", Some(field), None, Some(checksum)),
            None
        );
    }
}

const HEAL_PARAMS: &str = "/Script/ShooterGame.DamageableComponent:MulticastNotifyHeal";
const DECAY_PARAMS: &str = "/Script/ShooterGame.DamageableComponent:MulticastNotifyOverhealDecay";

/// The heal and overheal-decay references, each typed by its exact
/// group/name/checksum from `tools/fixtures/scoped_type_evidence.json`.
///
/// Scoped because the same parameter names carry other checksums on the damage
/// RPCs (`MulticastNotifyDamage_Base` / `_Point`), which the descriptor types by
/// name in their own groups -- a name rule would merge signatures the schema
/// keeps apart. The negatives pin that boundary: no checksum, a neighbouring
/// checksum, the exported `_ClassNetCache` spelling, or the sibling RPC types
/// nothing. Production resolves through `with_handles`, so this does too.
#[test]
fn heal_and_decay_references_require_exact_scoped_checksums() {
    const CNC: &str = "/Script/ShooterGame.DamageableComponent_ClassNetCache";
    let table = OverlayTable::with_handles(&OVERLAY_TABLE, &OVERLAY_HANDLE_TABLE);
    let resolve = |group: &str, field: &str, checksum: Option<u32>| {
        resolve_field_type_with_checksum(&table, group, Some(field), None, checksum)
    };
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
    let table = OverlayTable::with_handles(&OVERLAY_TABLE, &OVERLAY_HANDLE_TABLE);
    let mut stats = OverlayStats::default();
    let mut apply = |group: &str, field: &str, handle: u32, checksum: u32, raw: &[u8], bits| {
        crate::apply_overlay_with_checksum(
            &table,
            group,
            group_hash_state(group),
            Some(field),
            handle,
            Some(checksum),
            Some(raw),
            bits,
            &mut stats,
        )
        .expect("a scoped reference is attempted")
        .value_i64
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

/// The life-change array walks into its four members, on real wire bytes.
///
/// `docs/DATA.md`'s health section rests on these and nothing shipped could
/// read them -- the array arrived as one opaque blob, so every figure in that
/// section came from a script outside the repository. The fixtures here are
/// actual payloads from a 13.02 replay.
///
/// The local handles differ per RPC, which is the reason for three schemas.
/// `MulticastNotifyHeal` also names its parameter `LifeChangeBySection` rather
/// than `LifeChangeEvents`, so a dispatch keyed on the array's name alone
/// would miss two of the five functions entirely.
#[test]
#[cfg(feature = "array")]
fn the_life_change_array_walks_into_its_members() {
    use crate::{
        ArrayDecodeStats, LIFE_CHANGE_BY_SECTION_SCHEMA, LIFE_CHANGE_DAMAGE_SCHEMA,
        LIFE_CHANGE_SECTION_SCHEMA, decode_struct_array,
    };

    // MulticastNotifyDamage_Point.LifeChangeEvents, one element.
    let damage: Vec<u8> = vec![
        0x02, 0x02, 0x16, 0x20, 0xE5, 0x3E, 0x18, 0x40, 0x00, 0x80, 0x0E, 0x44, 0x1A, 0x40, 0x00,
        0x00, 0xF0, 0xC1, 0x1C, 0x02, 0x01, 0x00, 0x00,
    ];
    // MulticastNotifyHeal.LifeChangeBySection, one element.
    let heal: Vec<u8> = vec![
        0x02, 0x02, 0x06, 0x20, 0x29, 0x14, 0x08, 0x40, 0xFF, 0x0F, 0x08, 0x42, 0x0A, 0x40, 0x00,
        0x00, 0x80, 0x3F, 0x0C, 0x02, 0x01, 0x00, 0x00,
    ];

    for (label, raw, bits, schema, want) in [
        (
            "damage",
            &damage,
            177u32,
            &LIFE_CHANGE_DAMAGE_SCHEMA,
            [
                "ChangedComponent",
                "LifeResult",
                "DeltaLife",
                "bAliveAfterChange",
            ],
        ),
        (
            "heal",
            &heal,
            177,
            &LIFE_CHANGE_BY_SECTION_SCHEMA,
            [
                "ChangedComponent",
                "LifeResult",
                "DeltaLife",
                "bAliveAfterChange",
            ],
        ),
    ] {
        let mut stats = ArrayDecodeStats::default();
        let fields = decode_struct_array(raw, bits, Some(schema), &[], &mut stats);
        assert_eq!(stats.errors, 0, "{label} decoded with errors");
        assert_eq!(fields.len(), 4, "{label}: {fields:?}");
        for (field, name) in fields.iter().zip(want) {
            assert!(
                field.path.ends_with(name),
                "{label}: {} vs {name}",
                field.path
            );
        }
    }

    // The section schema numbers from 1, so it must not be interchangeable.
    let mut stats = ArrayDecodeStats::default();
    let wrong = decode_struct_array(
        &damage,
        177,
        Some(&LIFE_CHANGE_SECTION_SCHEMA),
        &[],
        &mut stats,
    );
    assert!(
        wrong.iter().all(|f| !f.path.ends_with("LifeResult")),
        "the wrong schema must not happen to name the members: {wrong:?}"
    );
}
