//! Blueprint properties typed by exact group, name and checksum.
//!
//! Every identity here is an entry of `tools/fixtures/scoped_type_evidence.json`
//! (generated into `scoped_types.rs`): a Blueprint class the C# descriptors
//! never described, whose property type three independent sources agree on --
//! the 13.06 class definition, the replay's own `compatible_checksum`
//! (recomputed in `tools/tests/test_compatible_checksum_facts.py`), and exact
//! decoding of every corpus row by an independent reader. The fixture carries
//! the measurements; these tests pin that the overlay reaches each type only
//! through its full identity, and that a real payload decodes to the value the
//! independent reader gave it.

use crate::decode::FieldType;
use crate::overlay::{OverlayStats, OverlayTable, group_hash_state};
use crate::{OVERLAY_HANDLE_TABLE, OVERLAY_TABLE};

const REVEAL_BOLT: &str = "/Game/Characters/Hunter/S0/Ability_Q/\
Projectile_Hunter_Q_RevealBolt.Projectile_Hunter_Q_RevealBolt_C";
const SHOCK_BOLT: &str = "/Game/Characters/Hunter/S0/Ability_4/\
Projectile_Hunter_4_ExplosiveBolt.Projectile_Hunter_4_ExplosiveBolt_C";
const POSSESSABLE: &str = "/Game/Characters/States/PossessableActorComponent.\
PossessableActorComponent_C";
const RIFT_POSSESSABLE: &str = "/Game/Characters/Rift/S0/Ability_X/WorldTargeting/\
Rift_PossessableActorComponent.Rift_PossessableActorComponent_C";
const CYPHER_CAMERA: &str = "/Game/Characters/Gumshoe/S0/Ability_E/\
Pawn_Gumshoe_E_PossessableCamera.Pawn_Gumshoe_E_PossessableCamera_C";
const KJ_TURRET: &str = "/Game/Characters/Killjoy/S0/Ability_E/\
Ability_Killjoy_E_Turret.Ability_Killjoy_E_Turret_C";
const KJ_ALARMBOT: &str = "/Game/Characters/Killjoy/S0/Ability_Q/\
Ability_Killjoy_Q_Alarmbot.Ability_Killjoy_Q_Alarmbot_C";
const CHARGED: &str = "/Game/Characters/Global/ChargedProjectileTargeting/\
Comp_Equippable_Charged.Comp_Equippable_Charged_C";
const BOMB_GS: &str = "/Game/GameModes/Bomb/BombGameState.BombGameState_C";
const SWIFT_GS: &str = "/Game/GameModes/_Development/Swiftplay_EndOfRoundCredits\
/Swiftplay_EoRCredits_GameState.Swiftplay_EoRCredits_GameState_C";

/// The 2D identities: (group, field, checksum, type).
const TWO_D: [(&str, &str, u32, FieldType); 14] = [
    (
        REVEAL_BOLT,
        "TrailPosition",
        3_110_715_024,
        FieldType::VectorDouble,
    ),
    (
        SHOCK_BOLT,
        "TrailPosition",
        3_110_715_024,
        FieldType::VectorDouble,
    ),
    (POSSESSABLE, "IsPossessed", 1_066_899_736, FieldType::Bool),
    (
        RIFT_POSSESSABLE,
        "IsPossessed",
        1_066_899_736,
        FieldType::Bool,
    ),
    (CYPHER_CAMERA, "Possessed", 2_181_339_745, FieldType::Bool),
    (CYPHER_CAMERA, "IsDeployed", 2_029_268_412, FieldType::Bool),
    (
        KJ_TURRET,
        "DeployedActor",
        2_740_089_937,
        FieldType::ObjectNetGuid,
    ),
    (
        KJ_ALARMBOT,
        "DeployedActor",
        2_740_089_937,
        FieldType::ObjectNetGuid,
    ),
    (CHARGED, "CurrentCharge", 1_908_355_023, FieldType::Double),
    (
        BOMB_GS,
        "CurrentLossStreak",
        1_863_385_026,
        FieldType::Int32,
    ),
    (
        BOMB_GS,
        "LossStreakTeam",
        22_256_526,
        FieldType::ObjectNetGuid,
    ),
    (
        BOMB_GS,
        "ShouldOverrideMatchTimer",
        2_889_152_318,
        FieldType::Bool,
    ),
    (
        SWIFT_GS,
        "LossStreakTeam",
        22_256_526,
        FieldType::ObjectNetGuid,
    ),
    (
        SWIFT_GS,
        "ShouldOverrideMatchTimer",
        2_889_152_318,
        FieldType::Bool,
    ),
];

fn resolve(group: &str, field: &str, checksum: Option<u32>) -> Option<FieldType> {
    let table = OverlayTable::with_handles(&OVERLAY_TABLE, &OVERLAY_HANDLE_TABLE);
    crate::resolve_field_type_with_checksum(&table, group, Some(field), None, checksum)
}

/// The name alone, a neighbouring checksum or another class resolves to
/// nothing: a Blueprint renames, retypes or moves a property between builds,
/// and each of those must leave the field raw, not borrow this type.
#[test]
fn two_d_blueprint_fields_resolve_only_at_their_exact_identity() {
    for (group, field, checksum, field_type) in TWO_D {
        assert_eq!(
            resolve(group, field, Some(checksum)),
            Some(field_type),
            "{group} {field}"
        );
        for other in [None, Some(checksum ^ 1)] {
            assert_eq!(resolve(group, field, other), None, "{field} {other:?}");
        }
        assert_eq!(resolve("/Unobserved", field, Some(checksum)), None);
    }
}

/// Decode one real payload through the whole overlay path, as the export
/// does, returning its four value columns.
fn decode(
    stats: &mut OverlayStats,
    group: &str,
    field: &str,
    checksum: u32,
    raw: &[u8],
    bits: u32,
) -> (Option<i64>, Option<f64>, Option<bool>, Option<String>) {
    let table = OverlayTable::with_handles(&OVERLAY_TABLE, &OVERLAY_HANDLE_TABLE);
    let result = crate::apply_overlay_with_checksum(
        &table,
        group,
        group_hash_state(group),
        Some(field),
        15,
        Some(checksum),
        Some(raw),
        bits,
        stats,
    )
    .expect("a scoped identity is attempted");
    (
        result.value_i64,
        result.value_f64,
        result.value_bool,
        result.value_str,
    )
}

/// Real 13.06 payloads, and the values an independent reader
/// (`struct.unpack` / an IntPacked loop) gave them in the corpus audit.
#[test]
fn two_d_blueprint_payloads_decode_to_the_independent_values() {
    let mut stats = OverlayStats::default();
    // Sova's recon bolt: three little-endian doubles, a world position.
    let trail = [
        0x00, 0x00, 0x00, 0x60, 0x6c, 0xc8, 0xba, 0x40, 0x00, 0x00, 0x00, 0xe0, 0x07, 0x9c, 0xbe,
        0xc0, 0x00, 0x00, 0x00, 0xc0, 0x81, 0xc2, 0x7c, 0x40,
    ];
    assert_eq!(
        decode(
            &mut stats,
            REVEAL_BOLT,
            "TrailPosition",
            3_110_715_024,
            &trail,
            192
        )
        .3,
        Some("(6856.42333984375,-7836.03076171875,460.15667724609375)".to_owned())
    );
    assert_eq!(
        decode(
            &mut stats,
            POSSESSABLE,
            "IsPossessed",
            1_066_899_736,
            &[1],
            1
        )
        .2,
        Some(true)
    );
    assert_eq!(
        decode(
            &mut stats,
            CYPHER_CAMERA,
            "Possessed",
            2_181_339_745,
            &[0],
            1
        )
        .2,
        Some(false)
    );
    // IntPacked: 0x7d continues with 62, 0x3a stops with 29 -> 62 + 29 * 128.
    assert_eq!(
        decode(
            &mut stats,
            KJ_TURRET,
            "DeployedActor",
            2_740_089_937,
            &[0x7d, 0x3a],
            16
        )
        .0,
        Some(3_774)
    );
    // The 8-bit null reference is GUID 0, a value, not a failure.
    assert_eq!(
        decode(
            &mut stats,
            KJ_ALARMBOT,
            "DeployedActor",
            2_740_089_937,
            &[0],
            8
        )
        .0,
        Some(0)
    );
    assert_eq!(
        decode(
            &mut stats,
            CHARGED,
            "CurrentCharge",
            1_908_355_023,
            &[0x9a, 0x99, 0x99, 0x99, 0x99, 0x99, 0xb9, 0x3f],
            64
        )
        .1,
        Some(0.1)
    );
    assert_eq!(
        decode(
            &mut stats,
            BOMB_GS,
            "CurrentLossStreak",
            1_863_385_026,
            &[1, 0, 0, 0],
            32
        )
        .0,
        Some(1)
    );
    assert_eq!(
        decode(
            &mut stats,
            SWIFT_GS,
            "LossStreakTeam",
            22_256_526,
            &[0x75, 0x06],
            16
        )
        .0,
        Some(58 + 3 * 128)
    );
    assert_eq!((stats.decoded_ok, stats.decoded_err), (8, 0));
}

/// A payload of another width is a counted decode error with no value: the
/// type is the full claim, and a 32-bit float or a 2-bit flag in these slots
/// would be a different property, not a shorter reading of this one.
#[test]
fn two_d_blueprint_fields_refuse_another_width() {
    let mut stats = OverlayStats::default();
    assert_eq!(
        decode(
            &mut stats,
            SHOCK_BOLT,
            "TrailPosition",
            3_110_715_024,
            &[0; 12],
            96
        ),
        (None, None, None, None)
    );
    assert_eq!(
        decode(
            &mut stats,
            POSSESSABLE,
            "IsPossessed",
            1_066_899_736,
            &[1],
            2
        ),
        (None, None, None, None)
    );
    assert_eq!(
        decode(
            &mut stats,
            CHARGED,
            "CurrentCharge",
            1_908_355_023,
            &[0; 4],
            32
        ),
        (None, None, None, None)
    );
    assert_eq!((stats.decoded_ok, stats.decoded_err), (0, 3));
}
