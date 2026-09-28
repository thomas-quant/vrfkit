//! Additive decoders for two payload shapes the field stream hands over whole:
//! the parent row keeps its `raw_bits` and is emitted either way, these only
//! add rows, and a decoder that fails leaves the export as it would have been.
//!
//! - **Flattened arrays.** UE flattens a `TArray` element's members onto
//!   consecutive handles of the enclosing group, whose net field exports name
//!   them; `decode_struct_array` walks that, and this module types the leaves.
//! - **Struct blobs.** `RoundResults`, `TeamEconomy` and `RoundInfos`, opaque
//!   to the overlay, have dedicated decoders in `vrf-decode`.

use smallvec::SmallVec;
use vrf_bitio::BitReader;
use vrf_decode::{ABILITY_CASTS_SCHEMA, COMBAT_ROUNDS_SCHEMA, FieldType, structs};
use vrf_schema::NetGuidCache;

use super::intern::put;
use super::{ExportSink, FieldValues, MeasuredArrayRoute, TABLE};

/// The four typed columns a decoded value lands in. At most one is ever
/// populated; `vrf_export`'s crate doc ("sparse value columns vs. Union") says
/// why this is four nullable columns rather than a union.
type DecodedColumns = (Option<i64>, Option<f64>, Option<bool>, Option<String>);

/// What the replay declares at `handle`: one slot of
/// `ExportSink::declared_handle_names` or `declared_handle_checksums`.
fn declared_at<T: Copy>(slots: &[Option<T>], handle: u32) -> Option<T> {
    slots.get(handle as usize).copied().flatten()
}

#[derive(Clone, Copy)]
enum VerifiedArrayLeaf {
    Field(FieldType),
    KillWeaponTheme,
    TrackedRewardLocalizedText,
}

struct VerifiedNestedLeaf {
    path: String,
    handle: u32,
    bit_count: u32,
    raw_bits: Vec<u8>,
    value_i64: i64,
}

/// Identities measured with exact consumption on all 714 replays. They qualify
/// wire windows only, claiming no gameplay ownership or order for the references.
fn verified_nested_container(
    parent: &str,
    handle: u32,
    name: Option<&str>,
    checksum: Option<u32>,
    resolved: Option<FieldType>,
) -> bool {
    let expected = match parent {
        "SelectedV2" => (13, "EquippableAttachments", 3_137_596_882),
        "KillData" => (6, "AssistingPlayers", 1_689_463_717),
        _ => return false,
    };
    (handle, name, checksum) == (expected.0, Some(expected.1), Some(expected.2))
        && resolved.is_none()
}

fn verified_nested_member(
    parent: &str,
    handle: u32,
    name: Option<&str>,
    checksum: Option<u32>,
    resolved: Option<FieldType>,
) -> bool {
    let expected = match (parent, handle) {
        ("SelectedV2", 14) => ("SocketAsset", 3_666_994_016),
        ("SelectedV2", 15) => ("AttachmentAsset", 856_446_005),
        ("KillData", 7) => ("AssistingPlayers", 1_417_448_159),
        _ => return false,
    };
    name == Some(expected.0)
        && checksum == Some(expected.1)
        && (resolved.is_none() || resolved == Some(FieldType::ObjectNetGuid))
}

pub(super) fn strict_nested_array_preflight(raw: &[u8], bit_count: u32, allowed: &[u32]) -> bool {
    // The generic walker tolerates EOF terminators and skips zero-width members;
    // these routes demand explicit terminators and check every handle before a
    // zero-width member could vanish.
    if bit_count == 0 {
        return false;
    }
    let Ok(mut reader) = BitReader::with_bit_len(raw, u64::from(bit_count)) else {
        return false;
    };
    let Ok(capacity) = reader.read_int_packed() else {
        return false;
    };
    if capacity > vrf_decode::MAX_ELEMENTS {
        return false;
    }
    let mut elements = 0;
    loop {
        if reader.at_end() {
            return false;
        }
        let Ok(encoded_index) = reader.read_int_packed() else {
            return false;
        };
        if encoded_index == 0 {
            return reader.at_end();
        }
        if encoded_index > capacity || elements == vrf_decode::MAX_ELEMENTS {
            return false;
        }
        elements += 1;
        let mut fields = 0;
        loop {
            if reader.at_end() {
                return false;
            }
            let Ok(encoded_handle) = reader.read_int_packed() else {
                return false;
            };
            if encoded_handle == 0 {
                break;
            }
            if fields == vrf_decode::MAX_FIELDS_PER_ELEMENT {
                return false;
            }
            fields += 1;
            let handle = encoded_handle - 1;
            if !allowed.contains(&handle) {
                return false;
            }
            let Ok(payload_bits) = reader.read_int_packed() else {
                return false;
            };
            if payload_bits == 0 || reader.skip_bits(u64::from(payload_bits)).is_err() {
                return false;
            }
        }
    }
}

fn decode_verified_nested_array(
    parent: &str,
    container: &vrf_decode::FlattenedField,
    declared_names: &[Option<&str>],
    declared_checksums: &[Option<u32>],
    group_path: &str,
) -> (
    Option<Vec<VerifiedNestedLeaf>>,
    vrf_decode::ArrayDecodeStats,
    u64,
) {
    let declared_name = declared_at(declared_names, container.handle);
    let declared_checksum = declared_at(declared_checksums, container.handle);
    let resolved =
        vrf_decode::resolve_field_type(&TABLE, group_path, declared_name, Some(container.handle));
    if !verified_nested_container(
        parent,
        container.handle,
        declared_name,
        declared_checksum,
        resolved,
    ) {
        return (None, vrf_decode::ArrayDecodeStats::default(), 0);
    }

    let allowed: &[u32] = if parent == "SelectedV2" {
        &[14, 15]
    } else {
        &[7]
    };
    if !strict_nested_array_preflight(&container.raw_bits, container.bit_count, allowed) {
        let stats = vrf_decode::ArrayDecodeStats {
            errors: 1,
            ..Default::default()
        };
        return (None, stats, 0);
    }

    let mut stats = vrf_decode::ArrayDecodeStats::default();
    let flattened = vrf_decode::decode_struct_array_exact(
        &container.raw_bits,
        container.bit_count,
        declared_names,
        &mut stats,
    );
    if !stats.is_clean() {
        return (None, stats, 0);
    }

    let mut decoded = Vec::with_capacity(flattened.len());
    for leaf in flattened {
        let declared_name = declared_at(declared_names, leaf.handle);
        let declared_checksum = declared_at(declared_checksums, leaf.handle);
        let resolved =
            vrf_decode::resolve_field_type(&TABLE, group_path, declared_name, Some(leaf.handle));
        if !verified_nested_member(
            parent,
            leaf.handle,
            declared_name,
            declared_checksum,
            resolved,
        ) {
            return (None, stats, 1);
        }
        let mut failures = 0;
        let (value_i64, value_f64, value_bool, value_str) = decode_leaf_with_stats(
            FieldType::ObjectNetGuid,
            &leaf.raw_bits,
            leaf.bit_count,
            &mut failures,
        );
        let Some(value_i64) = value_i64 else {
            return (None, stats, failures.max(1));
        };
        if value_f64.is_some() || value_bool.is_some() || value_str.is_some() {
            return (None, stats, failures.max(1));
        }
        decoded.push(VerifiedNestedLeaf {
            path: leaf.path,
            handle: leaf.handle,
            bit_count: leaf.bit_count,
            raw_bits: leaf.raw_bits,
            value_i64,
        });
    }
    (Some(decoded), stats, 0)
}

/// New structural-array routes may type only leaf windows independently
/// validated across the corpus. Everything else remains an exact raw child.
fn verified_array_leaf_type(
    parent: &str,
    checksum: Option<u32>,
    handle: u32,
    resolved: Option<FieldType>,
    declared_name: Option<&str>,
) -> Option<FieldType> {
    let wanted = match (parent, checksum, handle) {
        ("AllPlayersObfuscatedPlayerInformation", Some(1_349_268_968), 49) => FieldType::Bool,
        ("AllPlayersObfuscatedPlayerInformation", Some(1_349_268_968), 50) => FieldType::EnumByte,
        ("ServerActiveEffects", Some(3_301_618_856), 5 | 6) => FieldType::Bool,
        ("ServerActiveEffects", Some(3_301_618_856), 7 | 8) => FieldType::ObjectNetGuid,
        ("ServerActiveEffects", Some(3_301_618_856), 30 | 31) => FieldType::VectorDouble,
        ("ServerActiveEffects", Some(3_301_618_856), 33) => FieldType::Float,
        ("ServerActiveEffects", Some(3_301_618_856), 34) => FieldType::EnumByte,
        _ => return None,
    };
    // No top-level overlay for these two; their exact 192-bit windows were
    // decoded independently on all measured builds. Scoped to this parent.
    let measured_vector = parent == "ServerActiveEffects"
        && checksum == Some(3_301_618_856)
        && matches!(
            (handle, declared_name),
            (30, Some("Translation")) | (31, Some("Scale3D"))
        );
    (resolved == Some(wanted) || (resolved.is_none() && measured_vector)).then_some(wanted)
}

/// ActiveBlinds members, which have no top-level overlay. The enclosing checksum
/// and every member declaration were observed unchanged in 13.02 and 13.05; a
/// changed name or checksum, or a conflicting overlay, refuses typing.
///
/// `EffectID` is signed: checksum 3321413110 reproduces only as
/// `AuthBlindManagerState: FBlindManagerState -> ActiveBlinds: TArray ->
/// FActiveBlind -> BlindEffectID: FEffectID -> EffectID: int64` (`uint64` gives
/// 2854897423), and the 13.06 executable's reflection has `FEffectID.EffectID`
/// as Int64 too; the chain is recomputed in
/// tools/tests/test_compatible_checksum_facts.py. Below 2^63 both readings
/// agree; UInt64 refused the rest.
fn verified_blind_leaf_type(
    handle: u32,
    name: Option<&str>,
    checksum: Option<u32>,
    resolved: Option<FieldType>,
) -> Option<FieldType> {
    let (wanted_name, wanted_checksum, wanted_type) = match handle {
        3 => ("BlindId", 2_836_858_544, FieldType::UInt32),
        4 => ("EffectID", 3_321_413_110, FieldType::Int64),
        5 => ("SourceID", 4_130_766_059, FieldType::FName),
        6 => ("bLocalEffect", 2_802_682_995, FieldType::Bool),
        7 => ("bTransient", 815_378_154, FieldType::Bool),
        8 => ("InitialDuration", 1_370_668_337, FieldType::Float),
        9 => ("StartNetMovementTime", 2_358_118_895, FieldType::Float),
        10 => ("BlindConfig", 4_121_438_116, FieldType::ObjectNetGuid),
        11 => ("CausingActor", 2_370_661_694, FieldType::ObjectNetGuid),
        _ => return None,
    };
    (name == Some(wanted_name)
        && checksum == Some(wanted_checksum)
        && (resolved.is_none() || resolved == Some(wanted_type)))
    .then_some(wanted_type)
}

fn blind_member_width_valid(handle: u32, width: u32) -> bool {
    match handle {
        3 => width == 32,
        4 => width == 64,
        5 => width == 297,
        6 | 7 => width == 1,
        8 | 9 => width == 32,
        10 => width == 16,
        // Object references use IntPacked. A null actor is the one-byte zero,
        // observed in 59 main/checkpoint windows across the 81-file audit.
        11 => matches!(width, 8 | 16 | 24),
        _ => false,
    }
}

/// The array bits minus the one extra zero IntPacked an empty ActiveBlinds delta
/// may carry after its index terminator (57 windows in 13.01/13.02/13.04/13.05);
/// the parent keeps its original bits, and each spared byte is counted
/// (`ExportStats::active_blinds_empty_trailers`). Populated arrays, nonzero
/// tails and other trailers keep the exact-window checks.
fn active_blind_array_bits(raw: &[u8], bit_count: u32) -> u32 {
    let without_empty_trailer = (|| {
        let mut reader = BitReader::with_bit_len(raw, u64::from(bit_count)).ok()?;
        let capacity = reader.read_int_packed().ok()?;
        if capacity > vrf_decode::MAX_ELEMENTS || reader.read_int_packed().ok()? != 0 {
            return None;
        }
        if reader.bits_remaining() != 8 || reader.read_int_packed().ok()? != 0 {
            return None;
        }
        Some(bit_count - 8)
    })();
    without_empty_trailer.unwrap_or(bit_count)
}

/// `TrackedRewards` leaf types have independent full-corpus evidence. The
/// enclosing exact-array route proves the framing; each typed leaf still needs
/// its own declared name, handle, checksum, and overlay type to agree.
fn verified_reward_leaf_type(
    handle: u32,
    declared_name: Option<&str>,
    declared_checksum: Option<u32>,
    resolved: Option<FieldType>,
) -> Option<FieldType> {
    let (name, checksum, wanted) = match handle {
        28 => ("RewardName", 1_337_472_711, FieldType::FName),
        30 => ("InstancesOfReward", 2_922_243_316, FieldType::Int32),
        31 => ("RewardGrantStrategy", 3_589_631_714, FieldType::EnumByte),
        32 => ("Source", 1_118_571_008, FieldType::EnumByte),
        _ => return None,
    };
    (declared_name == Some(name) && declared_checksum == Some(checksum) && resolved == Some(wanted))
        .then_some(wanted)
}

/// RequestedIgnoreActors is an exact array route, but its child reference has
/// no descriptor overlay. Admit only the measured declaration and only while
/// resolution is absent or agrees; explicit Raw, Skip, and conflicts stay raw.
fn verified_requested_ignore_actor_leaf(
    handle: u32,
    declared_name: Option<&str>,
    declared_checksum: Option<u32>,
    resolved: Option<FieldType>,
) -> Option<FieldType> {
    (handle == 5
        && declared_name == Some("RequestedIgnoreActors")
        && declared_checksum == Some(3_344_674_359)
        && matches!(resolved, None | Some(FieldType::ObjectNetGuid)))
    .then_some(FieldType::ObjectNetGuid)
}

/// This descriptor is deliberately Raw in the generated table. The full FText
/// reader is admitted only for this measured parent leaf; Skip, a conflict, or
/// any future declared type change remains raw.
fn verified_reward_localized_text(
    handle: u32,
    declared_name: Option<&str>,
    declared_checksum: Option<u32>,
    resolved: Option<FieldType>,
) -> bool {
    handle == 29
        && declared_name == Some("LocalizedRewardName")
        && declared_checksum == Some(483_770_233)
        && resolved == Some(FieldType::Raw)
}

fn decode_tracked_reward_localized_text(
    raw: &[u8],
    bit_count: u32,
    failures: &mut u64,
) -> DecodedColumns {
    match vrf_decode::decode_ftext_tree(raw, bit_count) {
        Ok(value) => (None, None, None, Some(value.to_json())),
        Err(_) => {
            *failures = failures.saturating_add(1);
            (None, None, None, None)
        }
    }
}

/// `SelectedV2`'s six observed, declaration-qualified IntPacked NetGUID leaves.
/// The overlay has no entry for them; one that later names a different type is
/// a refusal, not something this route silently overrides.
fn verified_selected_v2_leaf_type(
    handle: u32,
    declared_name: Option<&str>,
    declared_checksum: Option<u32>,
    resolved: Option<FieldType>,
) -> Option<FieldType> {
    let (name, checksum) = match handle {
        3 => ("EquippableDataAsset", 1_793_937_854),
        4 => ("EquippableSkinDataAsset", 3_765_038_216),
        5 => ("EquippableSkinLevelDataAsset", 603_923_741),
        6 => ("EquippableSkinChromaDataAsset", 3_166_589_204),
        7 => ("EquippableCharmDataAsset", 3_345_806_642),
        8 => ("EquippableCharmLevelDataAsset", 1_087_985_310),
        _ => return None,
    };
    (declared_name == Some(name)
        && declared_checksum == Some(checksum)
        && matches!(resolved, None | Some(FieldType::ObjectNetGuid)))
    .then_some(FieldType::ObjectNetGuid)
}

/// KillData primitive windows, measured over 714 replays: their wire types,
/// not the values' meaning or units. Scoped to the exact parent route and the
/// declared handle, name and checksum; any overlay disagreement, Raw or Skip
/// included, refuses the type.
fn verified_kill_data_leaf(
    handle: u32,
    declared_name: Option<&str>,
    declared_checksum: Option<u32>,
    resolved: Option<FieldType>,
) -> Option<VerifiedArrayLeaf> {
    let (name, checksum, kind) = match handle {
        3 => (
            "Victim",
            3_990_035_472,
            VerifiedArrayLeaf::Field(FieldType::ObjectNetGuid),
        ),
        4 => (
            "KillingEquippableClass",
            2_071_131_011,
            VerifiedArrayLeaf::Field(FieldType::ObjectNetGuid),
        ),
        5 => (
            "WeaponTheme",
            1_839_952_321,
            VerifiedArrayLeaf::KillWeaponTheme,
        ),
        9 => (
            "DamageType",
            2_992_423_760,
            VerifiedArrayLeaf::Field(FieldType::ObjectNetGuid),
        ),
        10 => (
            "DamageTaken",
            2_001_471_495,
            VerifiedArrayLeaf::Field(FieldType::Float),
        ),
        11 => (
            "DamageRegion",
            3_229_265_809,
            // Preserve the observed byte code; no enum-label meaning is claimed.
            VerifiedArrayLeaf::Field(FieldType::Byte),
        ),
        12 => (
            "GameTimeElapsed",
            3_684_431_363,
            VerifiedArrayLeaf::Field(FieldType::Float),
        ),
        13 => (
            "RoundTimestamp",
            2_328_473_242,
            VerifiedArrayLeaf::Field(FieldType::Float),
        ),
        14 => (
            "RoundNumber",
            843_024_485,
            VerifiedArrayLeaf::Field(FieldType::Int32),
        ),
        15 => (
            "bDidKillTriggerFinisher",
            2_795_684_046,
            VerifiedArrayLeaf::Field(FieldType::Bool),
        ),
        _ => return None,
    };
    if declared_name != Some(name) || declared_checksum != Some(checksum) {
        return None;
    }
    match kind {
        VerifiedArrayLeaf::Field(wanted) if resolved.is_none() || resolved == Some(wanted) => {
            Some(kind)
        }
        VerifiedArrayLeaf::KillWeaponTheme if resolved.is_none() => Some(kind),
        _ => None,
    }
}

fn decode_kill_weapon_theme(raw: &[u8], bit_count: u32, failures: &mut u64) -> DecodedColumns {
    let decoded = (|| {
        let mut reader = BitReader::with_bit_len(raw, u64::from(bit_count)).map_err(|_| ())?;
        if !reader.read_bit().map_err(|_| ())? {
            return Err(());
        }
        // The generic FString reader tolerates a missing null terminator; this
        // shape needs one for every nonzero length, checked (a valid character
        // in the terminator slot included) before decoding.
        let mut framing = reader.clone();
        let length = framing.read_i32().map_err(|_| ())?;
        let units = i64::from(length).unsigned_abs();
        let unit_bits = if length < 0 { 16 } else { 8 };
        if units * (unit_bits / 8) > 64 * 1024 {
            return Err(());
        }
        if units != 0 {
            framing.skip_bits((units - 1) * unit_bits).map_err(|_| ())?;
            if framing.read_bits(unit_bits as u32).map_err(|_| ())? != 0 {
                return Err(());
            }
        }
        if framing.bits_remaining() != 0 {
            return Err(());
        }
        let value = reader.read_fstring(64 * 1024).map_err(|_| ())?;
        if reader.bits_remaining() != 0 {
            return Err(());
        }
        Ok(value)
    })();
    match decoded {
        Ok(value) => (None, None, None, Some(value)),
        Err(()) => {
            *failures = failures.saturating_add(1);
            (None, None, None, None)
        }
    }
}

/// The measured route a flattened-array parent belongs to, from its exact
/// group, name and checksum. Whether the replay's branch admits that route is
/// a separate question, answered by `ExportSink::admits`.
fn measured_array_route(
    group: &str,
    parent: &str,
    checksum: Option<u32>,
) -> Option<MeasuredArrayRoute> {
    let route = match (group, parent, checksum) {
        (
            "/Script/ShooterGame.OwnerExclusivePlayerInfo",
            "AllPlayersObfuscatedPlayerInformation",
            Some(1_349_268_968),
        ) => MeasuredArrayRoute::AllPlayersObfuscatedPlayerInformation,
        ("/Script/ShooterGame.OwnerExclusivePlayerInfo", "TrackedRewards", Some(976_048_801)) => {
            MeasuredArrayRoute::TrackedRewards
        }
        ("/Script/ShooterGame.PersonalizationComponent", "SelectedV2", Some(4_218_721_055)) => {
            MeasuredArrayRoute::SelectedV2
        }
        ("/Script/ShooterGame.PlayerMatchStatsComponent", "KillData", Some(1_493_759_848)) => {
            MeasuredArrayRoute::KillData
        }
        (
            "/Script/ShooterGame.EffectManagerComponent",
            "ServerActiveEffects",
            Some(3_301_618_856),
        ) => MeasuredArrayRoute::ServerActiveEffects,
        (
            "/Script/ShooterGame.FiniteSpeedMovementComponent",
            "RequestedIgnoreActors",
            Some(1_063_739_204),
        ) => MeasuredArrayRoute::RequestedIgnoreActors,
        ("/Script/ShooterGame.BlindManagerComponent", "ActiveBlinds", Some(3_853_965_310)) => {
            MeasuredArrayRoute::ActiveBlinds
        }
        _ => return None,
    };
    Some(route)
}

/// The sole measured empty `TrackedRewards` variant, a 24-bit `02 00 00` window:
/// capacity one, the index-zero terminator, and an opaque zero byte. A literal
/// route, not a relaxation of `decode_struct_array_exact`: nothing else matches.
fn is_tracked_rewards_opaque_empty_variant(
    group: &str,
    parent: &str,
    checksum: Option<u32>,
    raw: &[u8],
    bit_count: u32,
) -> bool {
    matches!(
        (group, parent, checksum, bit_count, raw),
        (
            "/Script/ShooterGame.OwnerExclusivePlayerInfo",
            "TrackedRewards",
            Some(976_048_801),
            24,
            [0x02, 0x00, 0x00]
        )
    )
}

/// The struct-blob fields that have a dedicated decoder in `vrf-decode`.
#[derive(Clone, Copy)]
enum StructBlob {
    RoundResults,
    TeamEconomy,
    RoundInfos,
}

impl ExportSink<'_> {
    /// Every name the replay declares for `group_path`, by handle (empty for an
    /// unknown group), borrowed from `cache` alone so `&mut self.stats` stays free.
    fn declared_handle_names<'g>(
        cache: &'g NetGuidCache,
        group_path: &str,
    ) -> Vec<Option<&'g str>> {
        let Some(group) = cache.get_group_by_path(group_path) else {
            return Vec::new();
        };
        group
            .fields
            .iter()
            .map(|slot| slot.as_ref().map(|f| f.name.as_str()))
            .collect()
    }

    /// The declared checksums, by handle like the names, so leaf typing cannot
    /// infer a checksum from a child's position.
    fn declared_handle_checksums(cache: &NetGuidCache, group_path: &str) -> Vec<Option<u32>> {
        let Some(group) = cache.get_group_by_path(group_path) else {
            return Vec::new();
        };
        group
            .fields
            .iter()
            .map(|slot| slot.as_ref().map(|field| field.compatible_checksum))
            .collect()
    }

    /// Check if a field name is a known DynamicArray that should be flattened.
    pub(super) fn is_known_array_field(
        &self,
        field_name: Option<&str>,
        checksum: Option<u32>,
    ) -> bool {
        match (field_name, checksum) {
            (Some("Rounds"), _) => self.current_group_path.contains("CombatReportComponent"),
            // Ability-cast structs (a GUID FString at handle 3, ints, floats,
            // vectors): leaves are named from the replay's declarations or
            // `_h{N}`; `get_array_schema` adds only the nested `Effects` schema.
            (Some("AbilityCastsThisRound"), _) => self
                .current_group_path
                .contains("AbilityStatisticsReplicator"),
            // The measured routes: the same identity-to-route map that picks
            // the exact walker in `emit_flattened_array`, gated per branch.
            (Some(name), _) => measured_array_route(&self.current_group_path, name, checksum)
                .is_some_and(|route| self.admits(route)),
            (None, _) => false,
        }
    }

    /// Get the array schema for a known DynamicArray field.
    fn get_array_schema(
        &self,
        field_name: Option<&str>,
    ) -> Option<&'static vrf_decode::ArrayFieldSchema> {
        match field_name {
            Some("Rounds") if self.current_group_path.contains("CombatReportComponent") => {
                Some(&COMBAT_ROUNDS_SCHEMA)
            }
            // Each cast carries an `Effects` array of the statistics it produced,
            // naming the players each landed on -- nesting the walker sees only
            // through this schema, or the debuff log stays one opaque leaf.
            Some("AbilityCastsThisRound")
                if self
                    .current_group_path
                    .contains("Comp_AbilityStatisticsReplicator") =>
            {
                Some(&ABILITY_CASTS_SCHEMA)
            }
            _ => None,
        }
    }

    /// Flatten a known DynamicArray field and emit one row per leaf.
    pub(super) fn emit_flattened_array(
        &mut self,
        field_name: Option<&str>,
        checksum: Option<u32>,
        raw: &[u8],
        bit_count: u32,
    ) {
        let schema = self.get_array_schema(field_name);
        let declared = Self::declared_handle_names(self.cache, &self.current_group_path);
        let declared_checksums =
            Self::declared_handle_checksums(self.cache, &self.current_group_path);
        let parent_name = field_name.unwrap_or("_array");
        let measured = measured_array_route(&self.current_group_path, parent_name, checksum)
            .is_some_and(|route| self.admits(route));
        let array_bits = if measured && parent_name == "ActiveBlinds" {
            active_blind_array_bits(raw, bit_count)
        } else {
            bit_count
        };
        if array_bits != bit_count {
            self.stats.active_blinds_empty_trailers += 1;
        }
        if measured
            && parent_name == "ActiveBlinds"
            && !strict_nested_array_preflight(raw, array_bits, &[3, 4, 5, 6, 7, 8, 9, 10, 11])
        {
            self.stats.array.errors += 1;
            return;
        }
        if measured
            && is_tracked_rewards_opaque_empty_variant(
                &self.current_group_path,
                parent_name,
                checksum,
                raw,
                bit_count,
            )
        {
            self.stats.tracked_rewards_opaque_empty_variants += 1;
            return;
        }
        let mut isolated = vrf_decode::ArrayDecodeStats::default();
        let flattened = if measured {
            vrf_decode::decode_struct_array_exact(raw, array_bits, &declared, &mut isolated)
        } else {
            vrf_decode::decode_struct_array(
                raw,
                bit_count,
                schema,
                &declared,
                &mut self.stats.array,
            )
        };
        if measured {
            self.stats.array.merge_from(&isolated);
            if !isolated.is_clean() {
                return;
            }
            if parent_name == "ActiveBlinds"
                && flattened
                    .iter()
                    .any(|field| !blind_member_width_valid(field.handle, field.bit_count))
            {
                self.stats.array_leaf_decode_errors += 1;
                return;
            }
            if parent_name == "ActiveBlinds"
                && flattened.iter().any(|field| {
                    let name = declared_at(&declared, field.handle);
                    let checksum = declared_at(&declared_checksums, field.handle);
                    let resolved = vrf_decode::resolve_field_type(
                        &TABLE,
                        &self.current_group_path,
                        name,
                        Some(field.handle),
                    );
                    verified_blind_leaf_type(field.handle, name, checksum, resolved).is_none()
                })
            {
                self.stats.array_leaf_decode_errors += 1;
                return;
            }
        }

        // Resolve every leaf's type before touching `self.records`: the overlay
        // first, keyed on the name the replay declares for the handle (the
        // generated table types most flattened members), with
        // `decode_array_leaf`'s hardcoded map only for names the table lacks.
        // That map alone left `DeathLocation` (handle 104, `VectorDouble` in the
        // table) an all-null `_h104` on all 3,492 arrivals on 02d4d478.
        let leaf_types: Vec<Option<VerifiedArrayLeaf>> = flattened
            .iter()
            .map(|f| {
                // The full resolution order (name, b-prefixed name, handle ->
                // descriptor name), as an ordinary field gets, so a property
                // types the same inside an array as outside.
                let name = declared_at(&declared, f.handle);
                let declared_resolved = vrf_decode::resolve_field_type(
                    &TABLE,
                    &self.current_group_path,
                    name,
                    Some(f.handle),
                );
                let resolved =
                    declared_resolved.filter(|ft| !matches!(ft, FieldType::Raw | FieldType::Skip));
                let declared_checksum = declared_at(&declared_checksums, f.handle);
                if measured && parent_name == "TrackedRewards" {
                    if verified_reward_localized_text(
                        f.handle,
                        name,
                        declared_checksum,
                        declared_resolved,
                    ) {
                        Some(VerifiedArrayLeaf::TrackedRewardLocalizedText)
                    } else {
                        verified_reward_leaf_type(f.handle, name, declared_checksum, resolved)
                            .map(VerifiedArrayLeaf::Field)
                    }
                } else if measured && parent_name == "RequestedIgnoreActors" {
                    verified_requested_ignore_actor_leaf(
                        f.handle,
                        name,
                        declared_checksum,
                        declared_resolved,
                    )
                    .map(VerifiedArrayLeaf::Field)
                } else if measured && parent_name == "ActiveBlinds" {
                    verified_blind_leaf_type(f.handle, name, declared_checksum, declared_resolved)
                        .map(VerifiedArrayLeaf::Field)
                } else if measured && parent_name == "SelectedV2" {
                    verified_selected_v2_leaf_type(
                        f.handle,
                        name,
                        declared_checksum,
                        declared_resolved,
                    )
                    .map(VerifiedArrayLeaf::Field)
                } else if measured && parent_name == "KillData" {
                    verified_kill_data_leaf(f.handle, name, declared_checksum, declared_resolved)
                } else if measured {
                    verified_array_leaf_type(parent_name, checksum, f.handle, resolved, name)
                        .map(VerifiedArrayLeaf::Field)
                } else {
                    resolved.map(VerifiedArrayLeaf::Field)
                }
            })
            .collect();

        let nested_results: Vec<_> = flattened
            .iter()
            .map(|f| {
                if measured
                    && matches!(
                        (parent_name, f.handle),
                        ("SelectedV2", 13) | ("KillData", 6)
                    )
                {
                    decode_verified_nested_array(
                        parent_name,
                        f,
                        &declared,
                        &declared_checksums,
                        &self.current_group_path,
                    )
                } else {
                    (None, vrf_decode::ArrayDecodeStats::default(), 0)
                }
            })
            .collect();
        for (_, nested_stats, nested_failures) in &nested_results {
            self.stats.array.merge_from(nested_stats);
            self.stats.array_leaf_decode_errors = self
                .stats
                .array_leaf_decode_errors
                .saturating_add(*nested_failures);
        }

        for ((f, declared_type), (nested, _, _)) in
            flattened.iter().zip(leaf_types).zip(nested_results)
        {
            let columns = match declared_type {
                Some(VerifiedArrayLeaf::Field(ft)) => decode_leaf_with_stats(
                    ft,
                    &f.raw_bits,
                    f.bit_count,
                    &mut self.stats.array_leaf_decode_errors,
                ),
                Some(VerifiedArrayLeaf::KillWeaponTheme) => decode_kill_weapon_theme(
                    &f.raw_bits,
                    f.bit_count,
                    &mut self.stats.array_leaf_decode_errors,
                ),
                Some(VerifiedArrayLeaf::TrackedRewardLocalizedText) => {
                    decode_tracked_reward_localized_text(
                        &f.raw_bits,
                        f.bit_count,
                        &mut self.stats.array_leaf_decode_errors,
                    )
                }
                // The hardcoded map is CombatReport-only: handle 3 is Int32
                // there and an FString in AbilityCastsThisRound.
                None if parent_name == "Rounds" => decode_array_leaf(
                    f.handle,
                    &f.raw_bits,
                    f.bit_count,
                    &mut self.stats.array_leaf_decode_errors,
                ),
                None => (None, None, None, None),
            };
            // `f.path` carries its own leading separator: "Rounds[0].RoundNumber".
            self.push_child(
                f.handle,
                &[parent_name, &f.path],
                f.bit_count,
                &f.raw_bits,
                columns,
            );
            self.stats.fields_emitted += 1;

            // Nested rows follow their raw container row; the whole nested
            // window was validated first, so a bad member cannot leak a prefix.
            if let Some(nested) = nested {
                for leaf in nested {
                    self.push_child(
                        leaf.handle,
                        &[parent_name, &f.path, &leaf.path],
                        leaf.bit_count,
                        &leaf.raw_bits,
                        (Some(leaf.value_i64), None, None, None),
                    );
                    self.stats.fields_emitted += 1;
                }
            }
        }
    }

    /// Push one row decoded out of a parent payload, named by concatenating
    /// `name`. Addressed inside the payload, not by a declared handle, so its
    /// checksum is null. The caller counts it.
    pub(super) fn push_child(
        &mut self,
        handle: u32,
        name: &[&str],
        bit_count: u32,
        raw: &[u8],
        (value_i64, value_f64, value_bool, value_str): DecodedColumns,
    ) {
        let field_name = self
            .channel_state
            .names
            .intern_fmt(|out| name.iter().for_each(|part| out.push_str(part)));
        self.push_field(FieldValues {
            handle,
            field_name: Some(field_name),
            compatible_checksum: None,
            bit_count,
            raw_bits: Some(SmallVec::from_slice(raw)),
            value_i64,
            value_f64,
            value_bool,
            value_str,
        });
    }

    /// The block's group with game-mode siblings mapped to the class the blob
    /// gates key on: Swiftplay carries `RoundResults` and `TeamEconomy` on
    /// `Swiftplay_EoRCredits_GameState_C`, which a bare
    /// `contains("BombGameState")` misses -- a clean-looking export with no
    /// score (docs/archive/PROJECT_STATUS.md sections 26 and 33).
    /// `vrf_decode::canonical_group` is the overlay's own alias table, so the
    /// two agree on what a game state is.
    fn canonical_group(&self) -> &str {
        vrf_decode::canonical_group(&self.current_group_path)
    }

    /// Which dedicated decoder owns this field on this group, if any. One
    /// classifier for both the predicate and the dispatcher: two copies that
    /// disagreed would divert a blob and then decline it, losing its leaves
    /// with no counter moving.
    fn struct_blob_kind(&self, field_name: Option<&str>) -> Option<StructBlob> {
        match field_name? {
            "RoundResults" if self.canonical_group().contains("BombGameState") => {
                Some(StructBlob::RoundResults)
            }
            "TeamEconomy" if self.canonical_group().contains("BombGameState") => {
                Some(StructBlob::TeamEconomy)
            }
            "RoundInfos" if self.current_group_path.contains("OwnerExclusivePlayerInfo") => {
                Some(StructBlob::RoundInfos)
            }
            _ => None,
        }
    }

    /// Check if a field is a struct blob that has a dedicated decoder.
    pub(super) fn is_struct_blob_field(&self, field_name: Option<&str>) -> bool {
        self.struct_blob_kind(field_name).is_some()
    }

    /// Whether this is a `MultiItemSlot.MultiContents` blob; the parent stays
    /// `Raw` and the items become extra `MultiContents[i]` rows.
    pub(super) fn is_multi_contents_field(&self, field_name: Option<&str>) -> bool {
        matches!(field_name, Some("MultiContents"))
            && self.current_group_path.contains("MultiItemSlot")
    }

    /// Decode a `MultiContents` blob (`TArray<AAresItem*>`) into one
    /// `MultiContents[index]` row per item, the NetGUID in `value_i64` as for
    /// `ItemSlot.Contents`. [`vrf_decode::decode_object_ref_array_with_stats`]
    /// returns `(wire element index, NetGUID)` pairs, and the wire index labels
    /// the row: arrays are delta-replicated per element, so a re-send may carry
    /// only the changed slot, which arrival order would put in slot 0.
    pub(super) fn emit_multi_contents(&mut self, raw: &[u8], bit_count: u32) {
        let guids =
            vrf_decode::decode_object_ref_array_with_stats(raw, bit_count, &mut self.stats.array);
        for (index, guid) in &guids {
            self.emit_struct_sub_field(
                |out| put(out, format_args!("MultiContents[{index}]")),
                Some(i64::from(*guid)),
                None,
            );
            self.stats.multi_contents_items_emitted += 1;
        }
    }

    /// Decode a struct blob and emit flattened sub-field rows.
    /// Returns true if decoding succeeded and sub-fields were emitted.
    pub(super) fn decode_struct_blob(
        &mut self,
        field_name: &str,
        raw: &[u8],
        bit_count: u32,
    ) -> bool {
        let emitted = match self.struct_blob_kind(Some(field_name)) {
            Some(StructBlob::RoundResults) => self.decode_round_results_blob(raw, bit_count),
            Some(StructBlob::TeamEconomy) => self.decode_team_economy_blob(raw, bit_count),
            Some(StructBlob::RoundInfos) => self.decode_round_infos_blob(raw, bit_count),
            None => false,
        };
        if emitted {
            self.stats.struct_blobs_decoded += 1;
        }
        emitted
    }

    /// Count a struct-blob failure rather than drop it: it costs no rows (the
    /// decoders are additive), but it must not go unsaid.
    fn record_blob_failure(&mut self, err: &dyn std::fmt::Display) {
        self.stats.struct_blobs_failed += 1;
        if self.stats.struct_blob_first_error.is_none() {
            self.stats.struct_blob_first_error = Some(err.to_string());
        }
    }

    /// Run one struct-blob decoder over the blob's declared bit length and
    /// the group's declared names. Every failure, the reader's included, is
    /// recorded here, so the wording cannot drift between the decoders.
    fn decode_blob<T>(
        &mut self,
        raw: &[u8],
        bit_count: u32,
        decode: impl FnOnce(&mut BitReader<'_>, &[Option<&str>]) -> structs::Result<Vec<T>>,
    ) -> Option<Vec<T>> {
        let Ok(mut reader) = BitReader::with_bit_len(raw, u64::from(bit_count)) else {
            self.record_blob_failure(&"declared bit length exceeds buffer");
            return None;
        };
        // The decoded elements own their strings, so once `decode` returns
        // nothing borrows `self.cache` and a failure can be recorded.
        let declared = Self::declared_handle_names(self.cache, &self.current_group_path);
        match decode(&mut reader, &declared) {
            Ok(results) => Some(results),
            Err(err) => {
                self.record_blob_failure(&err);
                None
            }
        }
    }

    /// Decode RoundResults blob and emit sub-field rows.
    fn decode_round_results_blob(&mut self, raw: &[u8], bit_count: u32) -> bool {
        let Some(results) = self.decode_blob(raw, bit_count, structs::decode_round_results) else {
            return false;
        };

        for rr in &results {
            let index = rr.round_number;
            self.emit_struct_sub_field(
                |out| put(out, format_args!("RoundResults[{index}].RoundNumber")),
                Some(i64::from(rr.round_number)),
                None,
            );
            if let Some(ref team) = rr.winning_team {
                self.emit_struct_sub_field(
                    |out| put(out, format_args!("RoundResults[{index}].WinningTeam")),
                    None,
                    Some(team.clone()),
                );
            }
            for (member, text) in [
                ("WinningTeamRole", rr.winning_team_role.map(|r| r.as_str())),
                ("RoundResult", rr.round_result.map(|o| o.as_str())),
            ] {
                if let Some(text) = text {
                    self.emit_struct_sub_field(
                        |out| put(out, format_args!("RoundResults[{index}].{member}")),
                        None,
                        Some(text.to_owned()),
                    );
                }
            }
        }

        !results.is_empty()
    }

    /// Decode TeamEconomy blob and emit sub-field rows.
    fn decode_team_economy_blob(&mut self, raw: &[u8], bit_count: u32) -> bool {
        let Some(results) = self.decode_blob(raw, bit_count, structs::decode_team_economy_declared)
        else {
            return false;
        };

        for te in &results {
            let index = te.index;
            self.emit_struct_sub_field(
                |out| put(out, format_args!("TeamEconomy[{index}].Index")),
                Some(i64::from(te.index)),
                None,
            );
            // Widened to i64 here rather than in the loop body: the members
            // are a mix of u32 and i32 and the array has to be one type.
            for (member, value) in [
                ("ReplicationId", te.replication_id.map(i64::from)),
                ("LoadoutValue", te.loadout_value.map(i64::from)),
                (
                    "AverageLoadoutValue",
                    te.average_loadout_value.map(i64::from),
                ),
            ] {
                if let Some(v) = value {
                    self.emit_struct_sub_field(
                        |out| put(out, format_args!("TeamEconomy[{index}].{member}")),
                        Some(v),
                        None,
                    );
                }
            }
        }

        !results.is_empty()
    }

    /// Decode RoundInfos blob and emit sub-field rows.
    fn decode_round_infos_blob(&mut self, raw: &[u8], bit_count: u32) -> bool {
        let Some(results) = self.decode_blob(raw, bit_count, structs::decode_round_infos) else {
            return false;
        };

        for ri in &results {
            let index = ri.index;
            for (member, value) in [
                ("RoundNumber", ri.round_number),
                ("StartOfRoundMoney", ri.start_of_round_money),
                ("StartOfRoundLoadoutValue", ri.start_of_round_loadout_value),
                ("EndOfRoundMoney", ri.end_of_round_money),
                ("EndOfRoundLoadoutValue", ri.end_of_round_loadout_value),
            ] {
                if let Some(v) = value {
                    self.emit_struct_sub_field(
                        |out| put(out, format_args!("RoundInfos[{index}].{member}")),
                        Some(i64::from(v)),
                        None,
                    );
                }
            }
        }

        !results.is_empty()
    }

    /// Emit one struct-blob member row, its name built by `name` straight into
    /// the interner's scratch buffer (no allocation). Only i64 and str are
    /// parameters: no member decodes to a float or a bool.
    fn emit_struct_sub_field(
        &mut self,
        name: impl FnOnce(&mut String),
        value_i64: Option<i64>,
        value_str: Option<String>,
    ) {
        let field_name = self.channel_state.names.intern_fmt(name);
        self.push_field(FieldValues {
            handle: 0,
            field_name: Some(field_name),
            bit_count: 0,
            raw_bits: None,
            value_i64,
            value_str,
            ..FieldValues::default()
        });
        self.stats.fields_emitted += 1;
    }
}

/// Decode one array leaf with a type the caller already resolved: the one
/// decode-and-widen every typing path shares.
pub(super) fn decode_leaf_with_stats(
    field_type: FieldType,
    raw: &[u8],
    bit_count: u32,
    failures: &mut u64,
) -> DecodedColumns {
    use vrf_decode::{DecodedValue, decode_field};

    match decode_field(field_type, raw, bit_count) {
        Ok(DecodedValue::I64(v)) => (Some(v), None, None, None),
        Ok(DecodedValue::F64(v)) => (None, Some(v), None, None),
        Ok(DecodedValue::Bool(v)) => (None, None, Some(v), None),
        Ok(DecodedValue::Str(v)) => (None, None, None, Some(v)),
        Err(_) => {
            *failures = failures.saturating_add(1);
            (None, None, None, None)
        }
    }
}

/// A handle -> type map derived from the C# `CombatRoundReportsDecoder`: the
/// floor for leaves the overlay cannot name (the caller asks it first). All
/// `None` for an unknown handle or a failed decode.
fn decode_array_leaf(
    handle: u32,
    raw: &[u8],
    bit_count: u32,
    failures: &mut u64,
) -> DecodedColumns {
    let field_type = match handle {
        3 | 5 | 19 | 21 | 46 | 81 | 96 => FieldType::Int32,
        18 | 20 | 47 | 82 => FieldType::Float,
        22 | 25 | 48 | 49 | 83 | 84 | 103 => FieldType::Bool,
        23 | 45 | 80 => FieldType::EnumByte,
        13 | 24 | 50 | 85 | 98 => FieldType::ObjectNetGuid,
        11 => FieldType::FString,
        12 => FieldType::FName,
        _ => return (None, None, None, None),
    };

    decode_leaf_with_stats(field_type, raw, bit_count, failures)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sink::test_fixtures::{bits_from_bytes, bytes, packed};
    use crate::sink::{ChannelState, ExportStats, MeasuredArrayRoutes, RecordBuffers};
    use std::sync::Arc;
    use vrf_export::FieldRecord;
    use vrf_net::field::FieldSink;

    const OWNER: &str = "/Script/ShooterGame.OwnerExclusivePlayerInfo";
    const OWNER_PARENT: &str = "AllPlayersObfuscatedPlayerInformation";
    const OWNER_CHECKSUM: u32 = 1_349_268_968;
    const REWARDS_PARENT: &str = "TrackedRewards";
    const REWARDS_CHECKSUM: u32 = 976_048_801;
    const SELECTED_GROUP: &str = "/Script/ShooterGame.PersonalizationComponent";
    const SELECTED_PARENT: &str = "SelectedV2";
    const SELECTED_CHECKSUM: u32 = 4_218_721_055;
    const KILL_GROUP: &str = "/Script/ShooterGame.PlayerMatchStatsComponent";
    const KILL_PARENT: &str = "KillData";
    const KILL_CHECKSUM: u32 = 1_493_759_848;
    const MEASURED_BUILD: &str = "++Ares-Core+release-13.05";
    const BLINDS: (&str, &str, u32) = (
        "/Script/ShooterGame.BlindManagerComponent",
        "ActiveBlinds",
        3_853_965_310,
    );
    /// The nine measured ActiveBlinds member declarations.
    const BLIND_MEMBERS: [(u32, &str, u32); 9] = [
        (3, "BlindId", 2_836_858_544),
        (4, "EffectID", 3_321_413_110),
        (5, "SourceID", 4_130_766_059),
        (6, "bLocalEffect", 2_802_682_995),
        (7, "bTransient", 815_378_154),
        (8, "InitialDuration", 1_370_668_337),
        (9, "StartNetMovementTime", 2_358_118_895),
        (10, "BlindConfig", 4_121_438_116),
        (11, "CausingActor", 2_370_661_694),
    ];

    /// A row's four typed columns.
    fn values(row: &FieldRecord) -> (Option<i64>, Option<f64>, Option<bool>, Option<&str>) {
        (
            row.value_i64,
            row.value_f64,
            row.value_bool,
            row.value_str.as_deref(),
        )
    }

    fn one_leaf(handle: u32, payload: &[bool]) -> Vec<bool> {
        let mut bits = Vec::new();
        for value in [1, 1, handle + 1, payload.len() as u32] {
            packed(&mut bits, value);
        }
        bits.extend_from_slice(payload);
        packed(&mut bits, 0);
        packed(&mut bits, 0);
        bits
    }

    fn one_element(fields: &[(u32, Vec<bool>)]) -> Vec<bool> {
        let mut bits = Vec::new();
        packed(&mut bits, 1);
        packed(&mut bits, 1);
        for (handle, payload) in fields {
            packed(&mut bits, handle + 1);
            packed(&mut bits, payload.len() as u32);
            bits.extend_from_slice(payload);
        }
        packed(&mut bits, 0);
        packed(&mut bits, 0);
        bits
    }

    /// One ActiveBlinds element with every member at its measured width.
    fn blind_element(effect_id: i64) -> Vec<bool> {
        let mut source_id = vec![false];
        source_id.extend(bits_from_bytes(&(29i32).to_le_bytes()));
        source_id.extend(bits_from_bytes(b"DedicatedServerWorldSourceID\0"));
        source_id.extend(bits_from_bytes(&0i32.to_le_bytes()));
        assert_eq!(source_id.len(), 297);
        let mut blind_config = Vec::new();
        packed(&mut blind_config, 256);
        let mut causing_actor = Vec::new();
        packed(&mut causing_actor, 257);
        one_element(&[
            (3, bits_from_bytes(&7u32.to_le_bytes())),
            (4, bits_from_bytes(&effect_id.to_le_bytes())),
            (5, source_id),
            (6, vec![true]),
            (7, vec![false]),
            (8, bits_from_bytes(&1.5f32.to_le_bytes())),
            (9, bits_from_bytes(&10.0f32.to_le_bytes())),
            (10, blind_config),
            (11, causing_actor),
        ])
    }

    fn kill_weapon_theme_payload(value: &str, utf16: bool) -> Vec<bool> {
        let mut bits = vec![true];
        let (length, body) = if value.is_empty() && !utf16 {
            (0, Vec::new())
        } else if utf16 {
            let units: Vec<u16> = value.encode_utf16().chain([0]).collect();
            let body = units.iter().flat_map(|v| v.to_le_bytes()).collect();
            (-(units.len() as i32), body)
        } else {
            let mut body = value.as_bytes().to_vec();
            body.push(0);
            (body.len() as i32, body)
        };
        bits.extend(bits_from_bytes(&length.to_le_bytes()));
        bits.extend(bits_from_bytes(&body));
        bits
    }

    fn export_array(
        identity: (&str, &str, u32),
        leaf: (u32, &str),
        bits: &[bool],
        branch: Option<&str>,
    ) -> (RecordBuffers, ExportStats) {
        export_array_with_child_checksum(identity, (leaf.0, leaf.1, 0), bits, branch)
    }

    fn export_array_with_child_checksum(
        identity: (&str, &str, u32),
        leaf: (u32, &str, u32),
        bits: &[bool],
        branch: Option<&str>,
    ) -> (RecordBuffers, ExportStats) {
        export_array_with_declarations(identity, &[leaf], bits, branch)
    }

    fn export_array_with_declarations(
        identity: (&str, &str, u32),
        leaves: &[(u32, &str, u32)],
        bits: &[bool],
        branch: Option<&str>,
    ) -> (RecordBuffers, ExportStats) {
        let (group, parent, checksum) = identity;
        let mut cache = NetGuidCache::new();
        cache
            .add_export_group(vrf_schema::NetFieldExportGroup::new(group.into(), 7, 128))
            .unwrap();
        for (handle, name, compatible_checksum) in
            std::iter::once((0, parent, checksum)).chain(leaves.iter().copied())
        {
            assert!(cache.set_field_on_group(
                7,
                vrf_schema::NetFieldExport {
                    handle,
                    compatible_checksum,
                    name: name.into(),
                }
            ));
        }
        let mut channel_state = ChannelState::new();
        let mut records = RecordBuffers::default();
        let mut sink = ExportSink::new(&mut cache, &mut channel_state, &mut records);
        sink.set_current_group_path(Arc::from(group));
        if let Some(branch) = branch {
            sink.enable_measured_array_routes(branch);
        }
        let raw = bytes(bits);
        sink.on_field(
            0,
            bits.len() as u32,
            BitReader::with_bit_len(&raw, bits.len() as u64).unwrap(),
        );
        let stats = sink.stats;
        (records, stats)
    }

    #[test]
    fn measured_array_emits_typed_child_before_exact_raw_parent() {
        let bits = one_leaf(49, &[true]);
        let (records, stats) = export_array(
            (OWNER, OWNER_PARENT, OWNER_CHECKSUM),
            (49, "bIsAfk"),
            &bits,
            Some(MEASURED_BUILD),
        );
        assert_eq!(records.fields.len(), 2);
        let child = &records.fields[0];
        assert_eq!(
            child.field_name.as_deref(),
            Some("AllPlayersObfuscatedPlayerInformation[0].bIsAfk")
        );
        assert_eq!(child.value_bool, Some(true));
        assert_eq!(child.raw_bits.as_deref(), Some([1u8].as_slice()));
        assert_eq!(child.bit_count, 1);
        assert_eq!(child.compatible_checksum, None);
        let parent = &records.fields[1];
        assert_eq!(parent.field_name.as_deref(), Some(OWNER_PARENT));
        assert_eq!(parent.raw_bits.as_deref(), Some(bytes(&bits).as_slice()));
        assert_eq!(parent.bit_count, bits.len() as u32);
        assert_eq!(stats.array.fields_emitted, 1);
        assert_eq!(stats.fields_emitted, 2);
    }

    #[test]
    fn measured_array_rejects_unmeasured_build_and_wrong_identity() {
        let bits = one_leaf(49, &[true]);
        for (group, name, checksum, branch) in [
            (OWNER, OWNER_PARENT, OWNER_CHECKSUM, None),
            (
                OWNER,
                OWNER_PARENT,
                OWNER_CHECKSUM,
                Some("++Ares-Core+release-13.07"),
            ),
            (
                OWNER,
                OWNER_PARENT,
                OWNER_CHECKSUM + 1,
                Some(MEASURED_BUILD),
            ),
            (
                "/Script/ShooterGame.Other",
                OWNER_PARENT,
                OWNER_CHECKSUM,
                Some(MEASURED_BUILD),
            ),
            (
                OWNER,
                "DifferentArray",
                OWNER_CHECKSUM,
                Some(MEASURED_BUILD),
            ),
        ] {
            let (records, stats) =
                export_array((group, name, checksum), (49, "bIsAfk"), &bits, branch);
            assert_eq!(
                records.fields.len(),
                1,
                "{group}/{name}/{checksum}/{branch:?}"
            );
            assert_eq!(
                records.fields[0].raw_bits.as_deref(),
                Some(bytes(&bits).as_slice())
            );
            assert_eq!(stats.array.fields_emitted, 0);
        }
    }

    #[test]
    fn measured_array_rejects_suffix_and_missing_terminator_transactionally() {
        let valid = one_leaf(49, &[true]);
        let mut suffix = valid.clone();
        suffix.extend([false; 8]);
        let truncated = valid[..valid.len() - 8].to_vec();
        for bits in [suffix, truncated] {
            let (records, stats) = export_array(
                (OWNER, OWNER_PARENT, OWNER_CHECKSUM),
                (49, "bIsAfk"),
                &bits,
                Some(MEASURED_BUILD),
            );
            assert_eq!(records.fields.len(), 1, "no partially accepted children");
            assert_eq!(
                records.fields[0].raw_bits.as_deref(),
                Some(bytes(&bits).as_slice())
            );
            assert!(
                stats.array.unconsumed_root_bits > 0
                    || stats.array.implicit_terminations > 0
                    || stats.array.errors > 0
            );
        }
    }

    #[test]
    fn measured_array_unknown_leaf_stays_raw() {
        let bits = one_leaf(48, &[true, false, true]);
        let (records, _) = export_array(
            (OWNER, OWNER_PARENT, OWNER_CHECKSUM),
            (48, "SubjectUniqueId"),
            &bits,
            Some(MEASURED_BUILD),
        );
        assert_eq!(records.fields.len(), 2);
        let child = &records.fields[0];
        assert_eq!(child.raw_bits.as_deref(), Some([5u8].as_slice()));
        assert_eq!(values(child), (None, None, None, None));
    }

    #[test]
    fn tracked_rewards_unverified_children_stay_raw() {
        let bits = one_leaf(19, &[true]);
        let (records, stats) = export_array(
            (OWNER, REWARDS_PARENT, REWARDS_CHECKSUM),
            (19, "AdditionalRawReward"),
            &bits,
            Some(MEASURED_BUILD),
        );
        assert_eq!(records.fields.len(), 2);
        let child = &records.fields[0];
        assert_eq!(
            child.field_name.as_deref(),
            Some("TrackedRewards[0].AdditionalRawReward")
        );
        assert_eq!(child.raw_bits.as_deref(), Some([1u8].as_slice()));
        assert_eq!(values(child), (None, None, None, None));
        assert_eq!(stats.tracked_rewards_opaque_empty_variants, 0);
    }

    #[test]
    fn tracked_rewards_types_only_the_four_verified_leaf_identities() {
        let fname_zero = vec![true, false, false, false, false, false, false, false, false];
        for (handle, name, checksum, payload, want_i64, want_str) in [
            (28, "RewardName", 1_337_472_711, fname_zero, None, Some("0")),
            (
                30,
                "InstancesOfReward",
                2_922_243_316,
                vec![false; 32],
                Some(0),
                None,
            ),
            (
                31,
                "RewardGrantStrategy",
                3_589_631_714,
                vec![false; 2],
                Some(0),
                None,
            ),
            (
                32,
                "Source",
                1_118_571_008,
                vec![true, true, false],
                Some(3),
                None,
            ),
        ] {
            let bits = one_leaf(handle, &payload);
            let (records, _) = export_array_with_child_checksum(
                (OWNER, REWARDS_PARENT, REWARDS_CHECKSUM),
                (handle, name, checksum),
                &bits,
                Some(MEASURED_BUILD),
            );
            let child = &records.fields[0];
            assert_eq!(child.value_i64, want_i64, "{name}");
            assert_eq!(child.value_str.as_deref(), want_str, "{name}");
            assert_eq!(child.raw_bits.as_deref(), Some(bytes(&payload).as_slice()));
        }
    }

    #[test]
    fn tracked_rewards_refuses_wrong_child_identity_or_resolved_type() {
        let bits = one_leaf(30, &[false; 32]);
        for (name, checksum) in [("OtherName", 2_922_243_316), ("InstancesOfReward", 0)] {
            let (records, _) = export_array_with_child_checksum(
                (OWNER, REWARDS_PARENT, REWARDS_CHECKSUM),
                (30, name, checksum),
                &bits,
                Some(MEASURED_BUILD),
            );
            assert_eq!(records.fields[0].value_i64, None, "{name}/{checksum}");
        }
        assert_eq!(
            verified_reward_leaf_type(
                30,
                Some("InstancesOfReward"),
                Some(2_922_243_316),
                Some(FieldType::Float),
            ),
            None
        );
    }

    #[test]
    fn tracked_rewards_localized_text_requires_its_raw_declaration() {
        assert!(verified_reward_localized_text(
            29,
            Some("LocalizedRewardName"),
            Some(483_770_233),
            Some(FieldType::Raw),
        ));
        for (handle, name, checksum, resolved) in [
            (29, Some("Other"), Some(483_770_233), Some(FieldType::Raw)),
            (
                29,
                Some("LocalizedRewardName"),
                Some(0),
                Some(FieldType::Raw),
            ),
            (
                29,
                Some("LocalizedRewardName"),
                Some(483_770_233),
                Some(FieldType::Skip),
            ),
            (
                29,
                Some("LocalizedRewardName"),
                Some(483_770_233),
                Some(FieldType::FText),
            ),
            (
                28,
                Some("LocalizedRewardName"),
                Some(483_770_233),
                Some(FieldType::Raw),
            ),
        ] {
            assert!(!verified_reward_localized_text(
                handle, name, checksum, resolved
            ));
        }
    }

    #[test]
    fn tracked_rewards_bad_typed_width_keeps_raw_leaf_and_counts_error() {
        let bits = one_leaf(30, &[false; 8]);
        let (records, stats) = export_array_with_child_checksum(
            (OWNER, REWARDS_PARENT, REWARDS_CHECKSUM),
            (30, "InstancesOfReward", 2_922_243_316),
            &bits,
            Some(MEASURED_BUILD),
        );
        assert_eq!(records.fields.len(), 2);
        assert_eq!(records.fields[0].raw_bits.as_deref(), Some([0].as_slice()));
        assert_eq!(records.fields[0].value_i64, None);
        assert_eq!(stats.array_leaf_decode_errors, 1);
    }

    #[test]
    fn localized_reward_text_keeps_raw_on_success_and_failure() {
        let empty = bits_from_bytes(&[0, 0, 0, 0, 255, 0, 0, 0, 0]);
        for (payload, errors, expected) in [
            (
                empty.clone(),
                0,
                Some(r#"{"flags":0,"history":255,"kind":"empty"}"#),
            ),
            (empty[..empty.len() - 1].to_vec(), 1, None),
        ] {
            let bits = one_leaf(29, &payload);
            let (records, stats) = export_array_with_child_checksum(
                (OWNER, REWARDS_PARENT, REWARDS_CHECKSUM),
                (29, "LocalizedRewardName", 483_770_233),
                &bits,
                Some(MEASURED_BUILD),
            );
            assert_eq!(records.fields.len(), 2);
            assert_eq!(
                records.fields[0].raw_bits.as_deref(),
                Some(bytes(&payload).as_slice())
            );
            assert_eq!(values(&records.fields[0]), (None, None, None, expected));
            assert_eq!(stats.array_leaf_decode_errors, errors);
            assert_eq!(
                records.fields[1].raw_bits.as_deref(),
                Some(bytes(&bits).as_slice())
            );
        }
    }

    #[test]
    fn selected_v2_and_kill_data_are_exact_raw_child_routes() {
        for (group, parent, checksum) in [
            (SELECTED_GROUP, SELECTED_PARENT, SELECTED_CHECKSUM),
            (KILL_GROUP, KILL_PARENT, KILL_CHECKSUM),
        ] {
            let bits = one_leaf(19, &[true, false, true]);
            let (records, stats) = export_array(
                (group, parent, checksum),
                (19, "NestedRawMember"),
                &bits,
                Some(MEASURED_BUILD),
            );
            assert_eq!(records.fields.len(), 2, "{parent}");
            assert_eq!(records.fields[0].raw_bits.as_deref(), Some([5].as_slice()));
            assert_eq!(
                values(&records.fields[0]),
                (None, None, None, None),
                "{parent}"
            );
            assert_eq!(stats.array.fields_emitted, 1);
        }
    }

    #[test]
    fn selected_v2_types_only_the_six_qualified_object_net_guid_leaves() {
        let mut multi_byte = Vec::new();
        packed(&mut multi_byte, 128);
        let (records, _) = export_array_with_child_checksum(
            (SELECTED_GROUP, SELECTED_PARENT, SELECTED_CHECKSUM),
            (3, "EquippableDataAsset", 1_793_937_854),
            &one_leaf(3, &multi_byte),
            Some(MEASURED_BUILD),
        );
        assert_eq!(records.fields[0].value_i64, Some(128));
        assert_eq!(
            records.fields[0].raw_bits.as_deref(),
            Some([1, 2].as_slice())
        );

        let zero = vec![false; 8];
        let (records, _) = export_array_with_child_checksum(
            (SELECTED_GROUP, SELECTED_PARENT, SELECTED_CHECKSUM),
            (8, "EquippableCharmLevelDataAsset", 1_087_985_310),
            &one_leaf(8, &zero),
            Some(MEASURED_BUILD),
        );
        assert_eq!(records.fields[0].value_i64, Some(0));

        for (name, checksum) in [("Other", 1_793_937_854), ("EquippableDataAsset", 0)] {
            let (records, _) = export_array_with_child_checksum(
                (SELECTED_GROUP, SELECTED_PARENT, SELECTED_CHECKSUM),
                (3, name, checksum),
                &one_leaf(3, &multi_byte),
                Some(MEASURED_BUILD),
            );
            assert_eq!(records.fields[0].value_i64, None, "{name}/{checksum}");
        }
        for (handle, name, checksum) in [
            (9, "A", 3_055_317_389),
            (13, "EquippableAttachments", 3_137_596_882),
            (14, "SocketAsset", 3_666_994_016),
            (15, "AttachmentAsset", 856_446_005),
        ] {
            let (records, _) = export_array_with_child_checksum(
                (SELECTED_GROUP, SELECTED_PARENT, SELECTED_CHECKSUM),
                (handle, name, checksum),
                &one_leaf(handle, &multi_byte),
                Some(MEASURED_BUILD),
            );
            assert_eq!(records.fields[0].value_i64, None, "{name} remains raw");
        }
        assert_eq!(
            verified_selected_v2_leaf_type(
                3,
                Some("EquippableDataAsset"),
                Some(1_793_937_854),
                Some(FieldType::Float),
            ),
            None,
            "a contradictory overlay must leave the leaf raw"
        );
        for blocked in [FieldType::Raw, FieldType::Skip] {
            assert_eq!(
                verified_selected_v2_leaf_type(
                    3,
                    Some("EquippableDataAsset"),
                    Some(1_793_937_854),
                    Some(blocked),
                ),
                None,
                "an explicit raw/skip declaration is not an absent overlay"
            );
        }
    }

    #[test]
    fn selected_v2_bad_object_net_guid_windows_stay_raw_and_count_errors() {
        for payload in [bits_from_bytes(&[1]), bits_from_bytes(&[1, 1, 1, 1, 0x20])] {
            let (records, stats) = export_array_with_child_checksum(
                (SELECTED_GROUP, SELECTED_PARENT, SELECTED_CHECKSUM),
                (3, "EquippableDataAsset", 1_793_937_854),
                &one_leaf(3, &payload),
                Some(MEASURED_BUILD),
            );
            assert_eq!(records.fields[0].value_i64, None);
            assert_eq!(stats.array_leaf_decode_errors, 1);
            assert_eq!(
                records.fields[0].raw_bits.as_deref(),
                Some(bytes(&payload).as_slice())
            );
        }
    }

    #[test]
    fn kill_data_types_only_qualified_primitive_leaves() {
        let mut object = Vec::new();
        packed(&mut object, 128);
        let float = bits_from_bytes(&(-1.5f32).to_le_bytes());
        let int = bits_from_bytes(&(-2i32).to_le_bytes());
        for (handle, name, checksum, payload, vi, vf, vb) in [
            (
                3,
                "Victim",
                3_990_035_472,
                object.clone(),
                Some(128),
                None,
                None,
            ),
            (
                4,
                "KillingEquippableClass",
                2_071_131_011,
                vec![false; 8],
                Some(0),
                None,
                None,
            ),
            (
                9,
                "DamageType",
                2_992_423_760,
                object.clone(),
                Some(128),
                None,
                None,
            ),
            (
                10,
                "DamageTaken",
                2_001_471_495,
                float.clone(),
                None,
                Some(-1.5),
                None,
            ),
            (
                11,
                "DamageRegion",
                3_229_265_809,
                vec![true, false, true],
                Some(5),
                None,
                None,
            ),
            (
                12,
                "GameTimeElapsed",
                3_684_431_363,
                float.clone(),
                None,
                Some(-1.5),
                None,
            ),
            (
                13,
                "RoundTimestamp",
                2_328_473_242,
                float,
                None,
                Some(-1.5),
                None,
            ),
            (14, "RoundNumber", 843_024_485, int, Some(-2), None, None),
            (
                15,
                "bDidKillTriggerFinisher",
                2_795_684_046,
                vec![false],
                None,
                None,
                Some(false),
            ),
        ] {
            let (records, _) = export_array_with_child_checksum(
                (KILL_GROUP, KILL_PARENT, KILL_CHECKSUM),
                (handle, name, checksum),
                &one_leaf(handle, &payload),
                Some(MEASURED_BUILD),
            );
            let child = &records.fields[0];
            assert_eq!(child.value_i64, vi, "{name}");
            assert_eq!(child.value_f64, vf, "{name}");
            assert_eq!(child.value_bool, vb, "{name}");
            assert_eq!(child.raw_bits.as_deref(), Some(bytes(&payload).as_slice()));
        }
    }

    #[test]
    fn kill_data_weapon_theme_decodes_exact_prefixed_fstrings() {
        for (value, utf16) in [
            ("/Game/Themes/Standard", false),
            ("\u{d14c}\u{b9c8}", true),
            ("", false),
        ] {
            let payload = kill_weapon_theme_payload(value, utf16);
            let (records, stats) = export_array_with_child_checksum(
                (KILL_GROUP, KILL_PARENT, KILL_CHECKSUM),
                (5, "WeaponTheme", 1_839_952_321),
                &one_leaf(5, &payload),
                Some(MEASURED_BUILD),
            );
            assert_eq!(records.fields[0].value_str.as_deref(), Some(value));
            assert_eq!(
                records.fields[0].raw_bits.as_deref(),
                Some(bytes(&payload).as_slice())
            );
            assert_eq!(stats.array_leaf_decode_errors, 0);
        }
    }

    #[test]
    fn kill_data_weapon_theme_rejects_bad_flag_terminator_and_residual() {
        let mut bad_flag = kill_weapon_theme_payload("x", false);
        bad_flag[0] = false;
        let mut bad_terminator = kill_weapon_theme_payload("x", false);
        // Use a valid UTF-8 byte so rejection cannot come from UTF decoding.
        let last = bad_terminator.len() - 8;
        bad_terminator[last] = true;
        let mut bad_wide_terminator = kill_weapon_theme_payload("x", true);
        let last_wide = bad_wide_terminator.len() - 16;
        bad_wide_terminator[last_wide] = true;
        let mut residual = kill_weapon_theme_payload("x", false);
        residual.push(false);
        let mut invalid_utf8 = vec![true];
        invalid_utf8.extend(bits_from_bytes(&2i32.to_le_bytes()));
        invalid_utf8.extend(bits_from_bytes(&[0xff, 0]));
        for payload in [
            bad_flag,
            bad_terminator,
            bad_wide_terminator,
            residual,
            invalid_utf8,
        ] {
            let (records, stats) = export_array_with_child_checksum(
                (KILL_GROUP, KILL_PARENT, KILL_CHECKSUM),
                (5, "WeaponTheme", 1_839_952_321),
                &one_leaf(5, &payload),
                Some(MEASURED_BUILD),
            );
            assert_eq!(records.fields[0].value_str, None);
            assert_eq!(
                records.fields[0].raw_bits.as_deref(),
                Some(bytes(&payload).as_slice())
            );
            assert_eq!(stats.array_leaf_decode_errors, 1);
        }
    }

    #[test]
    fn kill_data_refuses_wrong_child_identity_and_any_overlay_disagreement() {
        assert!(
            verified_kill_data_leaf(10, Some("DamageTaken"), Some(2_001_471_495), None).is_some()
        );
        for (name, checksum) in [("Other", 2_001_471_495), ("DamageTaken", 0)] {
            assert!(verified_kill_data_leaf(10, Some(name), Some(checksum), None).is_none());
            let payload = bits_from_bytes(&1.5f32.to_le_bytes());
            let (records, _) = export_array_with_child_checksum(
                (KILL_GROUP, KILL_PARENT, KILL_CHECKSUM),
                (10, name, checksum),
                &one_leaf(10, &payload),
                Some(MEASURED_BUILD),
            );
            assert_eq!(records.fields[0].value_f64, None);
            assert_eq!(
                records.fields[0].raw_bits.as_deref(),
                Some(bytes(&payload).as_slice())
            );
        }
        let payload = kill_weapon_theme_payload("theme", false);
        let (records, _) = export_array_with_child_checksum(
            (KILL_GROUP, KILL_PARENT, KILL_CHECKSUM),
            (5, "Other", 1_839_952_321),
            &one_leaf(5, &payload),
            Some(MEASURED_BUILD),
        );
        assert_eq!(records.fields[0].value_str, None);
        for blocked in [FieldType::Int32, FieldType::Raw, FieldType::Skip] {
            assert!(
                verified_kill_data_leaf(
                    10,
                    Some("DamageTaken"),
                    Some(2_001_471_495),
                    Some(blocked)
                )
                .is_none()
            );
        }
        assert!(
            verified_kill_data_leaf(
                5,
                Some("WeaponTheme"),
                Some(1_839_952_321),
                Some(FieldType::FString)
            )
            .is_none()
        );
    }

    #[test]
    fn measured_nested_arrays_emit_after_their_preserved_raw_containers() {
        let mut first = Vec::new();
        packed(&mut first, 128);
        let mut second = Vec::new();
        packed(&mut second, 9);
        let selected_nested = one_element(&[(14, first.clone()), (15, second.clone())]);
        let (records, stats) = export_array_with_declarations(
            (SELECTED_GROUP, SELECTED_PARENT, SELECTED_CHECKSUM),
            &[
                (13, "EquippableAttachments", 3_137_596_882),
                (14, "SocketAsset", 3_666_994_016),
                (15, "AttachmentAsset", 856_446_005),
            ],
            &one_leaf(13, &selected_nested),
            Some(MEASURED_BUILD),
        );
        assert_eq!(records.fields.len(), 4);
        assert_eq!(
            records.fields[0].field_name.as_deref(),
            Some("SelectedV2[0].EquippableAttachments")
        );
        assert_eq!(
            records.fields[0].raw_bits.as_deref(),
            Some(bytes(&selected_nested).as_slice())
        );
        assert_eq!(
            records.fields[1].field_name.as_deref(),
            Some("SelectedV2[0].EquippableAttachments[0].SocketAsset")
        );
        assert_eq!(records.fields[1].value_i64, Some(128));
        assert_eq!(
            records.fields[1].raw_bits.as_deref(),
            Some(bytes(&first).as_slice())
        );
        assert_eq!(
            records.fields[2].field_name.as_deref(),
            Some("SelectedV2[0].EquippableAttachments[0].AttachmentAsset")
        );
        assert_eq!(records.fields[2].value_i64, Some(9));
        assert_eq!(
            records.fields[3].field_name.as_deref(),
            Some(SELECTED_PARENT)
        );
        assert_eq!(stats.array.fields_emitted, 3);

        let kill_nested = one_element(&[(7, first.clone())]);
        let (records, stats) = export_array_with_declarations(
            (KILL_GROUP, KILL_PARENT, KILL_CHECKSUM),
            &[
                (6, "AssistingPlayers", 1_689_463_717),
                (7, "AssistingPlayers", 1_417_448_159),
            ],
            &one_leaf(6, &kill_nested),
            Some(MEASURED_BUILD),
        );
        assert_eq!(records.fields.len(), 3);
        assert_eq!(
            records.fields[0].field_name.as_deref(),
            Some("KillData[0].AssistingPlayers")
        );
        assert_eq!(
            records.fields[1].field_name.as_deref(),
            Some("KillData[0].AssistingPlayers[0].AssistingPlayers")
        );
        assert_eq!(records.fields[1].value_i64, Some(128));
        assert_eq!(records.fields[2].field_name.as_deref(), Some(KILL_PARENT));
        assert_eq!(stats.array.fields_emitted, 2);
    }

    #[test]
    fn nested_array_preflight_requires_bounds_nonzero_windows_and_terminators() {
        let valid = one_element(&[(7, bits_from_bytes(&[2]))]);
        assert!(strict_nested_array_preflight(
            &bytes(&valid),
            valid.len() as u32,
            &[7]
        ));
        assert!(!strict_nested_array_preflight(&[], 0, &[7]));
        assert!(!strict_nested_array_preflight(
            &bytes(&valid[..valid.len() - 8]),
            (valid.len() - 8) as u32,
            &[7]
        ));
        let mut suffix = valid.clone();
        suffix.extend([false; 8]);
        assert!(!strict_nested_array_preflight(
            &bytes(&suffix),
            suffix.len() as u32,
            &[7]
        ));
        let zero_width = one_element(&[(7, Vec::new())]);
        assert!(!strict_nested_array_preflight(
            &bytes(&zero_width),
            zero_width.len() as u32,
            &[7]
        ));
        let unexpected = one_element(&[(8, bits_from_bytes(&[2]))]);
        assert!(!strict_nested_array_preflight(
            &bytes(&unexpected),
            unexpected.len() as u32,
            &[7]
        ));
        // The generic array walker skips zero-width members. The projectile
        // route must reject one even when all three expected members follow.
        let path_with_unknown_zero = one_element(&[
            (4, Vec::new()),
            (1, bits_from_bytes(&[0; 4])),
            (2, bits_from_bytes(&[0; 24])),
            (3, bits_from_bytes(&[0; 24])),
        ]);
        assert!(!strict_nested_array_preflight(
            &bytes(&path_with_unknown_zero),
            path_with_unknown_zero.len() as u32,
            &[1, 2, 3]
        ));
        let mut capacity_limit = Vec::new();
        packed(&mut capacity_limit, vrf_decode::MAX_ELEMENTS + 1);
        packed(&mut capacity_limit, 0);
        assert!(!strict_nested_array_preflight(
            &bytes(&capacity_limit),
            capacity_limit.len() as u32,
            &[7]
        ));
        let mut index_range = Vec::new();
        packed(&mut index_range, 1);
        packed(&mut index_range, 2);
        assert!(!strict_nested_array_preflight(
            &bytes(&index_range),
            index_range.len() as u32,
            &[7]
        ));
        let fields = (0..=vrf_decode::MAX_FIELDS_PER_ELEMENT)
            .map(|_| (7, bits_from_bytes(&[0])))
            .collect::<Vec<_>>();
        let field_limit = one_element(&fields);
        assert!(!strict_nested_array_preflight(
            &bytes(&field_limit),
            field_limit.len() as u32,
            &[7]
        ));
    }

    #[test]
    fn active_blinds_empty_delta_with_zero_trailer_is_complete() {
        let identity = BLINDS;
        // Captured 57 times across 13.01/13.02/13.04/13.05: capacity
        // one (56 cases) or two (one case), no changed elements, zero trailer.
        for capacity in [1, 2] {
            let mut bits = Vec::new();
            packed(&mut bits, capacity);
            packed(&mut bits, 0);
            let (_, control) =
                export_array_with_declarations(identity, &[], &bits, Some(MEASURED_BUILD));
            assert_eq!(control.array.errors, 0);
            assert_eq!(
                control.active_blinds_empty_trailers, 0,
                "no trailer to spare"
            );
            packed(&mut bits, 0);
            let (records, stats) =
                export_array_with_declarations(identity, &[], &bits, Some(MEASURED_BUILD));
            assert_eq!(stats.array.errors, 0, "capacity {capacity}");
            assert_eq!(
                stats.active_blinds_empty_trailers, 1,
                "the tolerance must be seen firing"
            );
            assert_eq!(stats.array.unconsumed_root_bits, 0);
            assert_eq!(stats.array_leaf_decode_errors, 0);
            assert_eq!(
                records.fields.len(),
                1,
                "unchanged elements add no children"
            );
            assert_eq!(
                records.fields[0].raw_bits.as_deref(),
                Some(bytes(&bits).as_slice())
            );
        }
    }

    #[test]
    fn active_blinds_null_causing_actor_is_a_decoded_reference() {
        let identity = BLINDS;
        // The 59 rejected value windows contain a one-byte IntPacked zero.
        // Keep the positive reference as a control through the same sink.
        for reference in [257, 0] {
            let mut payload = Vec::new();
            packed(&mut payload, reference);
            let bits = one_leaf(11, &payload);
            let (records, stats) = export_array_with_declarations(
                identity,
                &[(11, "CausingActor", 2_370_661_694)],
                &bits,
                Some(MEASURED_BUILD),
            );
            assert_eq!(stats.array.errors, 0);
            assert_eq!(stats.array_leaf_decode_errors, 0, "reference {reference}");
            assert_eq!(records.fields.len(), 2);
            assert_eq!(records.fields[0].value_i64, Some(i64::from(reference)));
            assert_eq!(
                records.fields[0].raw_bits.as_deref(),
                Some(bytes(&payload).as_slice())
            );
            assert_eq!(
                records.fields[1].raw_bits.as_deref(),
                Some(bytes(&bits).as_slice())
            );
        }
    }

    #[test]
    fn active_blinds_invalid_trailers_and_references_still_fail() {
        let identity = BLINDS;
        let mut empty = Vec::new();
        packed(&mut empty, 1);
        packed(&mut empty, 0);
        for trailer in [
            vec![true; 8],
            bits_from_bytes(&[2]),
            bits_from_bytes(&[0, 0]),
        ] {
            let mut bits = empty.clone();
            bits.extend(trailer);
            let (records, stats) =
                export_array_with_declarations(identity, &[], &bits, Some(MEASURED_BUILD));
            assert_eq!(stats.array.errors, 1);
            assert_eq!(stats.active_blinds_empty_trailers, 0, "refused, not spared");
            assert_eq!(records.fields.len(), 1);
            assert_eq!(
                records.fields[0].raw_bits.as_deref(),
                Some(bytes(&bits).as_slice())
            );
        }
        for payload in [
            bits_from_bytes(&[1]),
            bits_from_bytes(&[0, 0]),
            vec![false; 7],
        ] {
            let bits = one_leaf(11, &payload);
            let (records, stats) = export_array_with_declarations(
                identity,
                &[(11, "CausingActor", 2_370_661_694)],
                &bits,
                Some(MEASURED_BUILD),
            );
            assert_eq!(stats.array_leaf_decode_errors, 1);
            assert!(records.fields.iter().all(|row| row.value_i64.is_none()));
            assert_eq!(
                records.fields.last().unwrap().raw_bits.as_deref(),
                Some(bytes(&bits).as_slice())
            );
        }
        let mut populated = one_leaf(11, &bits_from_bytes(&[0]));
        packed(&mut populated, 0);
        let (_, stats) = export_array_with_declarations(
            identity,
            &[(11, "CausingActor", 2_370_661_694)],
            &populated,
            Some(MEASURED_BUILD),
        );
        assert_eq!(
            stats.array.errors, 1,
            "only empty deltas admit the zero trailer"
        );
        assert_eq!(stats.active_blinds_empty_trailers, 0);
        assert!(
            !strict_nested_array_preflight(&[2, 0, 0], 24, &[11]),
            "other array routes retain the exact-window contract"
        );
    }

    #[test]
    fn active_blinds_null_reference_obeys_build_and_parent_identity_guards() {
        let identity = BLINDS;
        let declaration = [(11, "CausingActor", 2_370_661_694)];
        let bits = one_leaf(11, &bits_from_bytes(&[0]));
        for branch in ["13.01", "13.02", "13.04", "13.05", "13.06"] {
            let branch = format!("++Ares-Core+release-{branch}");
            let (records, stats) =
                export_array_with_declarations(identity, &declaration, &bits, Some(&branch));
            assert_eq!(records.fields.len(), 2, "{branch}");
            assert_eq!(records.fields[0].value_i64, Some(0));
            assert_eq!(stats.array_leaf_decode_errors, 0);
        }
        for branch in [None, Some("++Ares-Core+release-12.10"), Some("unknown")] {
            let (records, _) =
                export_array_with_declarations(identity, &declaration, &bits, branch);
            assert_eq!(records.fields.len(), 1, "{branch:?}");
        }
        for changed in [
            ("/Script/ShooterGame.OtherComponent", identity.1, identity.2),
            (identity.0, "OtherArray", identity.2),
            (identity.0, identity.1, identity.2 + 1),
        ] {
            let (records, _) =
                export_array_with_declarations(changed, &declaration, &bits, Some(MEASURED_BUILD));
            assert_eq!(records.fields.len(), 1);
            assert_eq!(
                records.fields[0].raw_bits.as_deref(),
                Some(bytes(&bits).as_slice())
            );
        }
    }

    #[test]
    fn active_blinds_every_truncated_null_update_retains_only_raw_parent() {
        let identity = BLINDS;
        let bits = one_leaf(11, &bits_from_bytes(&[0]));
        for length in 1..bits.len() {
            let truncated = &bits[..length];
            let (records, stats) = export_array_with_declarations(
                identity,
                &[(11, "CausingActor", 2_370_661_694)],
                truncated,
                Some(MEASURED_BUILD),
            );
            assert_eq!(stats.array.errors, 1, "cut at {length}");
            assert_eq!(records.fields.len(), 1, "cut at {length}");
            assert_eq!(records.fields[0].bit_count, length as u32);
            assert_eq!(
                records.fields[0].raw_bits.as_deref(),
                Some(bytes(truncated).as_slice())
            );
        }
    }

    #[test]
    fn active_blinds_empty_delta_rejects_every_nonzero_trailer_byte() {
        for trailer in 1..=255u8 {
            let bits = bits_from_bytes(&[2, 0, trailer]);
            let (records, stats) =
                export_array_with_declarations(BLINDS, &[], &bits, Some(MEASURED_BUILD));
            assert_eq!(stats.array.errors, 1, "trailer {trailer}");
            assert_eq!(stats.active_blinds_empty_trailers, 0, "trailer {trailer}");
            assert_eq!(records.fields.len(), 1);
        }
    }

    #[test]
    fn active_blinds_sparse_updates_keep_indices_and_packed_reference_boundaries() {
        let identity = BLINDS;
        for reference in [0, 1, 127, 128, 16_383, 16_384, 2_097_151] {
            let mut bits = Vec::new();
            packed(&mut bits, 3);
            for index in [0, 2] {
                packed(&mut bits, index + 1);
                packed(&mut bits, 12);
                let mut payload = Vec::new();
                packed(&mut payload, reference);
                packed(&mut bits, payload.len() as u32);
                bits.extend(payload);
                packed(&mut bits, 0);
            }
            packed(&mut bits, 0);
            let (records, stats) = export_array_with_declarations(
                identity,
                &[(11, "CausingActor", 2_370_661_694)],
                &bits,
                Some(MEASURED_BUILD),
            );
            assert_eq!(stats.array.errors + stats.array_leaf_decode_errors, 0);
            assert_eq!(records.fields.len(), 3);
            for (row, index) in records.fields[..2].iter().zip([0, 2]) {
                assert_eq!(
                    row.field_name.as_deref(),
                    Some(format!("ActiveBlinds[{index}].CausingActor").as_str())
                );
                assert_eq!(row.value_i64, Some(i64::from(reference)));
            }
            assert_eq!(
                records.fields[2].raw_bits.as_deref(),
                Some(bytes(&bits).as_slice())
            );
        }
    }

    #[test]
    fn active_blinds_changed_member_declaration_retains_only_raw_parent() {
        let bits = blind_element(8);
        let (valid, clean) =
            export_array_with_declarations(BLINDS, &BLIND_MEMBERS, &bits, Some(MEASURED_BUILD));
        assert_eq!(valid.fields.len(), 10);
        assert_eq!(clean.array_leaf_decode_errors, 0);
        assert_eq!(valid.fields[0].value_i64, Some(7));
        assert_eq!(valid.fields[8].value_i64, Some(257));

        let mut changed = BLIND_MEMBERS;
        changed[0].2 += 1;
        let (refused, stats) =
            export_array_with_declarations(BLINDS, &changed, &bits, Some(MEASURED_BUILD));
        assert_eq!(refused.fields.len(), 1);
        assert_eq!(refused.fields[0].field_name.as_deref(), Some(BLINDS.1));
        assert_eq!(
            refused.fields[0].raw_bits.as_deref(),
            Some(bytes(&bits).as_slice())
        );
        assert_eq!(stats.array_leaf_decode_errors, 1);
    }

    /// `EffectID` is an `int64` (see `verified_blind_leaf_type`): bit 63 set is a
    /// negative ID, not an overflow a `UInt64` read would refuse.
    #[test]
    fn active_blinds_effect_id_is_signed() {
        let (valid, stats) = export_array_with_declarations(
            BLINDS,
            &BLIND_MEMBERS,
            &blind_element(-2),
            Some(MEASURED_BUILD),
        );
        assert_eq!(stats.array_leaf_decode_errors, 0);
        let effect_id = valid
            .fields
            .iter()
            .find(|f| f.field_name.as_deref() == Some("ActiveBlinds[0].EffectID"))
            .expect("the EffectID leaf is emitted");
        assert_eq!(effect_id.value_i64, Some(-2));
        assert_eq!(effect_id.bit_count, 64);
    }

    #[test]
    fn nested_array_identity_and_overlay_disagreements_are_refused() {
        assert!(verified_nested_container(
            "KillData",
            6,
            Some("AssistingPlayers"),
            Some(1_689_463_717),
            None
        ));
        assert!(!verified_nested_container(
            "KillData",
            6,
            Some("Other"),
            Some(1_689_463_717),
            None
        ));
        assert!(!verified_nested_container(
            "KillData",
            6,
            Some("AssistingPlayers"),
            Some(0),
            None
        ));
        for blocked in [FieldType::ObjectNetGuid, FieldType::Raw, FieldType::Skip] {
            assert!(!verified_nested_container(
                "KillData",
                6,
                Some("AssistingPlayers"),
                Some(1_689_463_717),
                Some(blocked)
            ));
        }
        for blocked in [FieldType::Float, FieldType::Raw, FieldType::Skip] {
            assert!(!verified_nested_member(
                "SelectedV2",
                14,
                Some("SocketAsset"),
                Some(3_666_994_016),
                Some(blocked)
            ));
        }
    }

    #[test]
    fn malformed_nested_value_is_transactional_and_keeps_outer_raw() {
        let malformed = one_element(&[(7, bits_from_bytes(&[2])), (7, bits_from_bytes(&[1]))]);
        let (records, stats) = export_array_with_declarations(
            (KILL_GROUP, KILL_PARENT, KILL_CHECKSUM),
            &[
                (6, "AssistingPlayers", 1_689_463_717),
                (7, "AssistingPlayers", 1_417_448_159),
            ],
            &one_leaf(6, &malformed),
            Some(MEASURED_BUILD),
        );
        assert_eq!(records.fields.len(), 2);
        assert_eq!(
            records.fields[0].raw_bits.as_deref(),
            Some(bytes(&malformed).as_slice())
        );
        assert_eq!(records.fields[1].field_name.as_deref(), Some(KILL_PARENT));
        assert_eq!(stats.array_leaf_decode_errors, 1);
        assert_eq!(
            stats.array.fields_emitted, 3,
            "walker count includes attempted leaves, while no row prefix leaks"
        );

        let valid = one_element(&[(14, bits_from_bytes(&[2])), (15, bits_from_bytes(&[4]))]);
        let (records, stats) = export_array_with_declarations(
            (SELECTED_GROUP, SELECTED_PARENT, SELECTED_CHECKSUM),
            &[
                (13, "EquippableAttachments", 3_137_596_882),
                (14, "SocketAsset", 3_666_994_016),
                (15, "AttachmentAsset", 0),
            ],
            &one_leaf(13, &valid),
            Some(MEASURED_BUILD),
        );
        assert_eq!(
            records.fields.len(),
            2,
            "wrong nested checksum emits no prefix"
        );
        assert_eq!(
            records.fields[0].raw_bits.as_deref(),
            Some(bytes(&valid).as_slice())
        );
        assert_eq!(stats.array_leaf_decode_errors, 1);
    }

    #[test]
    fn nested_fixture_is_not_enabled_outside_the_exact_parent_gate() {
        let nested = one_element(&[(7, bits_from_bytes(&[2]))]);
        for (group, checksum, branch) in [
            (
                "/Script/ShooterGame.Other",
                KILL_CHECKSUM,
                Some(MEASURED_BUILD),
            ),
            (KILL_GROUP, KILL_CHECKSUM + 1, Some(MEASURED_BUILD)),
            (KILL_GROUP, KILL_CHECKSUM, None),
        ] {
            let (records, stats) = export_array_with_declarations(
                (group, KILL_PARENT, checksum),
                &[
                    (6, "AssistingPlayers", 1_689_463_717),
                    (7, "AssistingPlayers", 1_417_448_159),
                ],
                &one_leaf(6, &nested),
                branch,
            );
            assert_eq!(records.fields.len(), 1, "{group}/{checksum}/{branch:?}");
            assert_eq!(records.fields[0].field_name.as_deref(), Some(KILL_PARENT));
            assert_eq!(stats.array.fields_emitted, 0);
        }
    }

    #[test]
    fn selected_v2_and_kill_data_refuse_wrong_identity_and_exact_residuals() {
        for (group, parent, checksum) in [
            (SELECTED_GROUP, SELECTED_PARENT, SELECTED_CHECKSUM),
            (KILL_GROUP, KILL_PARENT, KILL_CHECKSUM),
        ] {
            let valid = one_leaf(19, &[true]);
            for (actual_group, actual_checksum, branch, bits) in [
                (
                    "/Script/ShooterGame.Other",
                    checksum,
                    Some(MEASURED_BUILD),
                    valid.clone(),
                ),
                (group, checksum + 1, Some(MEASURED_BUILD), valid.clone()),
                (group, checksum, None, valid.clone()),
                (group, checksum, Some(MEASURED_BUILD), {
                    let mut suffix = valid.clone();
                    suffix.extend([false; 8]);
                    suffix
                }),
                (
                    group,
                    checksum,
                    Some(MEASURED_BUILD),
                    valid[..valid.len() - 8].to_vec(),
                ),
            ] {
                let (records, stats) = export_array(
                    (actual_group, parent, actual_checksum),
                    (19, "NestedRawMember"),
                    &bits,
                    branch,
                );
                assert_eq!(
                    records.fields.len(),
                    1,
                    "{parent}/{actual_group}/{actual_checksum}"
                );
                if actual_group == group && actual_checksum == checksum && branch.is_some() {
                    assert!(
                        stats.array.unconsumed_root_bits > 0
                            || stats.array.errors > 0
                            || stats.array.implicit_terminations > 0,
                        "exact residual lost its diagnostic: {stats:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn tracked_rewards_literal_opaque_empty_variant_keeps_only_parent_raw() {
        let bits = bits_from_bytes(&[0x02, 0x00, 0x00]);
        let (records, stats) = export_array(
            (OWNER, REWARDS_PARENT, REWARDS_CHECKSUM),
            (49, "Rewards"),
            &bits,
            Some(MEASURED_BUILD),
        );
        assert_eq!(records.fields.len(), 1);
        assert_eq!(
            records.fields[0].field_name.as_deref(),
            Some(REWARDS_PARENT)
        );
        assert_eq!(
            records.fields[0].raw_bits.as_deref(),
            Some([2, 0, 0].as_slice())
        );
        assert_eq!(stats.tracked_rewards_opaque_empty_variants, 1);
        assert_eq!(stats.array.fields_emitted, 0);
    }

    #[test]
    fn tracked_rewards_refuses_wrong_identity_and_any_other_trailer_shape() {
        let literal = bits_from_bytes(&[0x02, 0x00, 0x00]);
        for (group, parent, checksum, branch, bits) in [
            (
                OWNER,
                REWARDS_PARENT,
                REWARDS_CHECKSUM + 1,
                Some(MEASURED_BUILD),
                literal.clone(),
            ),
            (
                "/Script/ShooterGame.Other",
                REWARDS_PARENT,
                REWARDS_CHECKSUM,
                Some(MEASURED_BUILD),
                literal.clone(),
            ),
            (
                OWNER,
                "OtherRewards",
                REWARDS_CHECKSUM,
                Some(MEASURED_BUILD),
                literal.clone(),
            ),
            (
                OWNER,
                REWARDS_PARENT,
                REWARDS_CHECKSUM,
                None,
                literal.clone(),
            ),
        ] {
            let (records, stats) =
                export_array((group, parent, checksum), (49, "Rewards"), &bits, branch);
            assert_eq!(
                records.fields.len(),
                1,
                "{group}/{parent}/{checksum}/{branch:?}"
            );
            assert_eq!(stats.tracked_rewards_opaque_empty_variants, 0);
            assert_eq!(stats.array.fields_emitted, 0);
        }
    }

    /// A different trailing byte, a nonempty extension, or no zero index
    /// terminator: none becomes an accepted optional trailer, and each keeps
    /// the exact decoder's diagnostic.
    #[test]
    fn tracked_rewards_residual_variants_keep_exact_diagnostics() {
        for bits in [
            bits_from_bytes(&[0x02, 0x00, 0x01]),
            bits_from_bytes(&[0x02, 0x00, 0x00, 0x00]),
            bits_from_bytes(&[0x02]),
        ] {
            let (records, stats) = export_array(
                (OWNER, REWARDS_PARENT, REWARDS_CHECKSUM),
                (49, "Rewards"),
                &bits,
                Some(MEASURED_BUILD),
            );
            assert_eq!(records.fields.len(), 1);
            assert_eq!(stats.tracked_rewards_opaque_empty_variants, 0);
            assert_eq!(stats.array.fields_emitted, 0);
            assert!(
                stats.array.unconsumed_root_bits > 0
                    || stats.array.errors > 0
                    || stats.array.implicit_terminations > 0,
                "residual window lost its exact-decoder diagnostic: {stats:?}"
            );
        }
        // A complete nonempty array followed by a zero byte is not the measured
        // empty variant. Walking a child does not authorize emitting it when
        // the enclosing array retains unexplained bits.
        let mut bits = one_leaf(19, &[false; 32]);
        bits.extend(bits_from_bytes(&[0]));
        let (records, stats) = export_array(
            (OWNER, REWARDS_PARENT, REWARDS_CHECKSUM),
            (19, "Rewards"),
            &bits,
            Some(MEASURED_BUILD),
        );
        assert_eq!(records.fields.len(), 1);
        assert_eq!(stats.tracked_rewards_opaque_empty_variants, 0);
        assert_eq!(stats.array.fields_emitted, 1);
        assert_eq!(stats.array.unconsumed_root_bits, 8);
    }

    #[test]
    fn measured_effect_and_ignore_routes_emit_expected_leaf_values() {
        let payload: Vec<bool> = (0..32)
            .map(|bit| (-0.0f32).to_bits() & (1 << bit) != 0)
            .collect();
        let bits = one_leaf(33, &payload);
        let (records, _) = export_array(
            (
                "/Script/ShooterGame.EffectManagerComponent",
                "ServerActiveEffects",
                3_301_618_856,
            ),
            (33, "StartTimeStamp"),
            &bits,
            Some(MEASURED_BUILD),
        );
        assert_eq!(records.fields.len(), 2);
        assert_eq!(
            records.fields[0].value_f64.unwrap().to_bits(),
            (-0.0f64).to_bits()
        );

        let mut payload = Vec::new();
        packed(&mut payload, 700);
        let bits = one_leaf(5, &payload);
        let (records, _) = export_array_with_child_checksum(
            (
                "/Script/ShooterGame.FiniteSpeedMovementComponent",
                "RequestedIgnoreActors",
                1_063_739_204,
            ),
            (5, "RequestedIgnoreActors", 3_344_674_359),
            &bits,
            Some(MEASURED_BUILD),
        );
        assert_eq!(records.fields.len(), 2);
        let child = &records.fields[0];
        assert_eq!(child.raw_bits.as_deref(), Some(bytes(&payload).as_slice()));
        assert_eq!(values(child), (Some(700), None, None, None));
    }

    #[test]
    fn existing_ability_array_keeps_its_float_type_without_measured_route() {
        let payload: Vec<bool> = (0..32)
            .map(|bit| 12.5f32.to_bits() & (1 << bit) != 0)
            .collect();
        let bits = one_leaf(7, &payload);
        let (records, _) = export_array(
            (
                "/Game/Characters/_Core/Comp_AbilityStatisticsReplicator.Comp_AbilityStatisticsReplicator_C",
                "AbilityCastsThisRound",
                0,
            ),
            (7, "CastTime_4_5AE288704801A9B74D6D159DFC2BD147"),
            &bits,
            None,
        );
        assert_eq!(records.fields.len(), 2);
        assert_eq!(records.fields[0].value_f64, Some(12.5));
        assert_eq!(
            records.fields[0].field_name.as_deref(),
            Some("AbilityCastsThisRound[0].CastTime_4_5AE288704801A9B74D6D159DFC2BD147")
        );
    }

    #[test]
    fn measured_routes_require_the_full_qualified_identity() {
        assert_eq!(
            measured_array_route(
                "/Script/ShooterGame.FiniteSpeedMovementComponent",
                "RequestedIgnoreActors",
                Some(1_063_739_204)
            ),
            Some(MeasuredArrayRoute::RequestedIgnoreActors)
        );
        assert_eq!(
            measured_array_route(OWNER, REWARDS_PARENT, Some(REWARDS_CHECKSUM)),
            Some(MeasuredArrayRoute::TrackedRewards)
        );
        assert_eq!(
            measured_array_route(
                "/Script/ShooterGame.FiniteSpeedMovementComponent",
                "RequestedIgnoreActors",
                Some(1)
            ),
            None
        );
        assert_eq!(
            measured_array_route(
                "/Script/ShooterGame.Other",
                "RequestedIgnoreActors",
                Some(1_063_739_204)
            ),
            None
        );
    }

    /// The gate is per route, not per build: a typed child (a leaf the 13.05
    /// route types) appears exactly where the branch admits its route. Admission
    /// and the exact walker both take the route from `measured_array_route`, so
    /// this checks each parent reads its own route's bit: every flattened route
    /// has a case (no wildcard), every supported branch runs plus none, against
    /// `MeasuredArrayRoutes::for_branch`. Two pairs share their branches
    /// (PlayerInfo/TrackedRewards, KillData/RequestedIgnoreActors), so only the
    /// identity pin below sees a swap inside one.
    #[test]
    fn legacy_branches_expand_only_their_admitted_routes() {
        let mut reference = Vec::new();
        packed(&mut reference, 5);
        let float: Vec<bool> = (0..32)
            .map(|bit| 1.5f32.to_bits() & (1 << bit) != 0)
            .collect();
        // No wildcard: a new route does not compile until it has a case.
        let case_for = |route| {
            let case = match route {
                MeasuredArrayRoute::KillData => (
                    (KILL_GROUP, KILL_PARENT, KILL_CHECKSUM),
                    (3, "Victim", 3_990_035_472),
                    one_leaf(3, &reference),
                ),
                MeasuredArrayRoute::SelectedV2 => (
                    (SELECTED_GROUP, SELECTED_PARENT, SELECTED_CHECKSUM),
                    (3, "EquippableDataAsset", 1_793_937_854),
                    one_leaf(3, &reference),
                ),
                MeasuredArrayRoute::TrackedRewards => (
                    (OWNER, REWARDS_PARENT, REWARDS_CHECKSUM),
                    (30, "InstancesOfReward", 2_922_243_316),
                    one_leaf(30, &[false; 32]),
                ),
                MeasuredArrayRoute::ActiveBlinds => (
                    BLINDS,
                    (11, "CausingActor", 2_370_661_694),
                    one_leaf(11, &bits_from_bytes(&[0])),
                ),
                MeasuredArrayRoute::ServerActiveEffects => (
                    (
                        "/Script/ShooterGame.EffectManagerComponent",
                        "ServerActiveEffects",
                        3_301_618_856,
                    ),
                    (33, "StartTimeStamp", 0),
                    one_leaf(33, &float),
                ),
                MeasuredArrayRoute::RequestedIgnoreActors => (
                    (
                        "/Script/ShooterGame.FiniteSpeedMovementComponent",
                        "RequestedIgnoreActors",
                        1_063_739_204,
                    ),
                    (5, "RequestedIgnoreActors", 3_344_674_359),
                    one_leaf(5, &reference),
                ),
                MeasuredArrayRoute::AllPlayersObfuscatedPlayerInformation => (
                    (OWNER, OWNER_PARENT, OWNER_CHECKSUM),
                    (49, "bIsAfk", 0),
                    one_leaf(49, &[true]),
                ),
                // An RPC parameter, not a flattened property; its gate is
                // `projectile_path_rpc_expands_only_on_admitting_branches`.
                MeasuredArrayRoute::NetworkedProjectilePath => return None,
            };
            Some(case)
        };
        // The identity pin: each case must map to its own route.
        for route in MeasuredArrayRoute::ALL {
            if let Some(((group, parent, checksum), _, _)) = case_for(route) {
                assert_eq!(
                    measured_array_route(group, parent, Some(checksum)),
                    Some(route),
                    "{route:?}"
                );
            }
        }
        let branches = vrf_transform::ALL_VERSIONS
            .iter()
            .map(|version| Some(version.branch()))
            .chain([None]);
        let mut expanded = Vec::new();
        for branch in branches {
            let admitted =
                branch.map_or(MeasuredArrayRoutes::NONE, MeasuredArrayRoutes::for_branch);
            for route in MeasuredArrayRoute::ALL {
                let Some((identity, leaf, bits)) = case_for(route) else {
                    continue;
                };
                let (records, stats) =
                    export_array_with_declarations(identity, &[leaf], &bits, branch);
                let at = format!("{branch:?} {route:?}");
                let parent = records.fields.last().unwrap();
                assert_eq!(parent.field_name.as_deref(), Some(identity.1), "{at}");
                assert_eq!(parent.raw_bits.as_deref(), Some(bytes(&bits).as_slice()));
                assert_eq!(stats.array_leaf_decode_errors, 0, "{at}");
                if admitted.admits(route) {
                    if !expanded.contains(&route) {
                        expanded.push(route);
                    }
                    assert_eq!(records.fields.len(), 2, "{at}");
                    let child = &records.fields[0];
                    assert!(
                        child.value_i64.is_some()
                            || child.value_f64.is_some()
                            || child.value_bool.is_some(),
                        "{at}: the admitted child is typed"
                    );
                } else {
                    assert_eq!(records.fields.len(), 1, "{at}");
                    assert_eq!(stats.array.fields_emitted, 0, "{at}");
                }
            }
        }
        // Not vacuous: each of the seven flattened routes expanded on some
        // branch, and the run with no branch kept every one of them raw.
        assert_eq!(expanded.len(), 7, "{expanded:?}");
    }

    #[test]
    fn new_routes_type_only_the_verified_leaf_windows() {
        assert_eq!(
            verified_array_leaf_type(
                "ServerActiveEffects",
                Some(3_301_618_856),
                33,
                Some(FieldType::Float),
                None
            ),
            Some(FieldType::Float)
        );
        assert_eq!(
            verified_array_leaf_type(
                "RequestedIgnoreActors",
                Some(1_063_739_204),
                5,
                Some(FieldType::ObjectNetGuid),
                None
            ),
            None,
            "packed wire integers remain raw without an identity claim"
        );
        assert_eq!(
            verified_array_leaf_type(
                "ServerActiveEffects",
                Some(3_301_618_856),
                33,
                Some(FieldType::Double),
                None
            ),
            None
        );
    }

    #[test]
    fn requested_ignore_actor_requires_exact_child_identity_and_non_raw_resolution() {
        assert_eq!(
            verified_requested_ignore_actor_leaf(
                5,
                Some("RequestedIgnoreActors"),
                Some(3_344_674_359),
                None,
            ),
            Some(FieldType::ObjectNetGuid)
        );
        for (handle, name, checksum, resolved) in [
            (
                5,
                Some("RequestedIgnoreActors"),
                Some(3_344_674_359),
                Some(FieldType::Raw),
            ),
            (
                5,
                Some("RequestedIgnoreActors"),
                Some(3_344_674_359),
                Some(FieldType::Skip),
            ),
            (
                5,
                Some("RequestedIgnoreActors"),
                Some(3_344_674_359),
                Some(FieldType::Int32),
            ),
            (5, Some("Other"), Some(3_344_674_359), None),
            (5, Some("RequestedIgnoreActors"), Some(0), None),
            (4, Some("RequestedIgnoreActors"), Some(3_344_674_359), None),
        ] {
            assert_eq!(
                verified_requested_ignore_actor_leaf(handle, name, checksum, resolved),
                None
            );
        }
    }

    #[test]
    fn requested_ignore_actor_bad_packed_child_keeps_raw_rows_and_counts_error() {
        // 0xff is an unterminated IntPacked value: its continuation bit is
        // set, but the exact leaf window ends before another packed byte.
        let bits = one_leaf(5, &[true; 8]);
        let (records, stats) = export_array_with_child_checksum(
            (
                "/Script/ShooterGame.FiniteSpeedMovementComponent",
                "RequestedIgnoreActors",
                1_063_739_204,
            ),
            (5, "RequestedIgnoreActors", 3_344_674_359),
            &bits,
            Some(MEASURED_BUILD),
        );
        assert_eq!(records.fields.len(), 2);
        let child = &records.fields[0];
        assert_eq!(child.raw_bits.as_deref(), Some([0xff].as_slice()));
        assert_eq!(values(child), (None, None, None, None));
        let parent = &records.fields[1];
        assert_eq!(parent.raw_bits.as_deref(), Some(bytes(&bits).as_slice()));
        assert_eq!(stats.array_leaf_decode_errors, 1);
    }

    #[test]
    fn a_typed_array_leaf_failure_is_counted_while_its_raw_input_survives() {
        let raw = [0x7a];
        let mut failures = 0;

        let decoded = decode_leaf_with_stats(FieldType::Int32, &raw, 8, &mut failures);

        assert_eq!(decoded, (None, None, None, None));
        assert_eq!(failures, 1);
        assert_eq!(raw, [0x7a]);
    }

    #[test]
    fn measured_effect_vectors_are_typed_without_a_top_level_overlay() {
        let payload: Vec<bool> = [1.25f64, -0.0, -2.5]
            .iter()
            .flat_map(|value| (0..64).map(move |bit| value.to_bits() & (1 << bit) != 0))
            .collect();
        for (handle, name) in [(30, "Translation"), (31, "Scale3D")] {
            let bits = one_leaf(handle, &payload);
            let (records, _) = export_array(
                (
                    "/Script/ShooterGame.EffectManagerComponent",
                    "ServerActiveEffects",
                    3_301_618_856,
                ),
                (handle, name),
                &bits,
                Some(MEASURED_BUILD),
            );
            assert_eq!(records.fields.len(), 2);
            assert_eq!(
                records.fields[0].value_str.as_deref(),
                Some("(1.25,-0,-2.5)")
            );
        }
        assert_eq!(
            verified_array_leaf_type(
                "ServerActiveEffects",
                Some(3_301_618_856),
                30,
                None,
                Some("DifferentMember")
            ),
            None
        );
    }
}
