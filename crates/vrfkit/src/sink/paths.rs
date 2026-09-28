//! Content-block group-path resolution.
//!
//! Every content block is attributed to a declared export group before any
//! field in it can be named. The rules are the C# `ContentBlockPathResolver`'s
//! (`resolve_actor_group_path`: `ResolveCachedActorExportGroupPath` /
//! `ResolveCachedActorClassPath` in `ResolveActorPackageOrClassPath` order;
//! `resolve_subobject_group_path`: `ResolveSubobjectExportGroupPath` /
//! `ResolveSubobjectClassPath`; `create_combined_candidate`:
//! `TryCreateCombinedCandidate`; `NetGuidCache::unique_leaf_match`:
//! `UniqueLeafMatch`). The reference replay has 608,020 blocks, and
//! docs/archive/PROJECT_STATUS.md 5-P measured this resolution at 371 ms, the
//! export's largest slice once the Parquet writers left the packet loop.
//!
//! # The memo
//!
//! [`BlockPathMemo`] is what this module adds over the C#. Resolution is a pure
//! function of the header's `is_actor`, `has_rep_layout`, `class_net_guid` and
//! `object_net_guid` and the channel index and actor GUID -- the memo key --
//! and of three inputs with independent stamps, none covering another: the
//! declared group paths (`NetGuidCache::schema_generation`, which tracks no
//! field or GUID mutation); the GUID -> path and GUID -> outer maps
//! (`NetGuidCache::guid_generation`, the one stamp that also sees the
//! frame-level ExportData section, `vrf_frame::read_export_data`, which calls
//! `set_net_guid_path` directly rather than through this crate's
//! `register_path`); and the channel -> (actor, archetype) map
//! (`ChannelState::resolution_generation`, moved only by
//! `on_actor_open`/`on_actor_close`). Any stamp moving discards the whole memo,
//! so a hit is indistinguishable from a recomputation.
//!
//! The value is `(group path, function count)` because `resolve_function_count`
//! can *replace* the path (the bare-instance-name branch). `actor_net_guid`
//! grows over a replay, so an unbounded memo was the risk; in practice an input
//! moves every ~330 blocks and the table is discarded long before it grows. It
//! costs kilobytes and removes four fifths of the work (probe/hit/miss counts
//! on 02d4d478: docs/PERFORMANCE_NOTES.md#group-path-resolution-memo).

use std::sync::Arc;

use vrf_net::content::ContentBlockHeader;
use vrf_net::types::NetworkGuid;
use vrf_schema::{FxHashMap, NetFieldExportGroup, find_class_net_cache_key, find_replay_path_key};

use super::{ChannelState, ExportSink};

/// Stably named subobject leaves -> the class path they resolve to, tagged with
/// the block kind each applies to: the fallback when no `class_net_guid` names
/// the class.
///
/// The first four pairs are the C# `ContentBlockPathResolver`'s ClassNetCache
/// effect entries. The rest go beyond it: VALORANT replicates components under
/// bare instance names but declares their layouts under the class, so without
/// a remap every handle such a block carries stays unnamed (`CurrentEquippable`,
/// the spike carrier, included). The class is usually native; for four
/// Blueprint-class instances the replay declares, and the pair names, the
/// Blueprint class under `/Game/`. All of them are RepLayout-only on purpose:
/// the AbilitySystem `_ClassNetCache` group is declared with an incomplete
/// function table, so its RPC stream stays unresolved and is brute-forced
/// (fc=34), and a remapped component's RPC rows stay bare by design.
///
/// The component pairs are read from the shipped game, not inferred:
/// `tools/extract_component_classes` lists each component export (a
/// `<Name>_GEN_VARIABLE` export or a class-default-object subobject, its class
/// resolved through the IoStore global container), and on the 13.06 containers
/// reproduces every pair here a cooked asset can hold, `InventoryComponent` and
/// `AbilitiesAndBuffsComponent` (first argued from handle shapes) included.
/// Each 13.06 addition is a bare group of at least 9,000 rows in the
/// 1,018-replay corpus whose name has one class in every package, whose target
/// the replay declares wherever the leaf carries RepLayout rows, and whose
/// handles are a subset of the declared ones (widths agreeing where the target
/// has rows). docs/DATA.md ("Reading component classes out of the game") has
/// the procedure, numbers and rejects; `tools/check_component_remaps.py` watches
/// for the symptoms of a rename, which nothing here can detect -- re-derive then.
#[rustfmt::skip]
const KNOWN_SUBOBJECT_CLASS_PATHS: &[(&str, &str, GroupKind)] = &[
    ("ReplayEffect", "/Script/ShooterGame.ReplayEffectComponent", GroupKind::ClassNetCache),
    ("EffectManager", "/Script/ShooterGame.EffectManagerComponent", GroupKind::ClassNetCache),
    ("LocationalEffectManager", "/Script/ShooterGame.LocationalEffectManagerComponent", GroupKind::ClassNetCache),
    ("DamageHandlerComponent", "/Script/ShooterGame.DamageableComponent", GroupKind::ClassNetCache),
    ("InventoryComponent", "/Script/ShooterGame.AresInventory", GroupKind::RepLayout),
    ("AbilitiesAndBuffsComponent", "/Script/ShooterGame.AresAbilitySystemComponent", GroupKind::RepLayout),
    ("ZoomStateMachine", "/Script/ShooterGame.EquippableStateMachineComponent", GroupKind::RepLayout),
    ("SelectBounceStateMachine", "/Script/ShooterGame.EquippableStateMachineComponent", GroupKind::RepLayout),
    ("StateMachine_Priming", "/Script/ShooterGame.EquippableStateMachineComponent", GroupKind::RepLayout),
    ("TargetingToggle_StateMachine", "/Script/ShooterGame.EquippableStateMachineComponent", GroupKind::RepLayout),
    ("SuppressionRoundsStateMachine", "/Script/ShooterGame.EquippableStateMachineComponent", GroupKind::RepLayout),
    ("BoostStateMachine", "/Script/ShooterGame.EquippableStateMachineComponent", GroupKind::RepLayout),
    ("Gun_StateMachine", "/Script/ShooterGame.EquippableStateMachineComponent", GroupKind::RepLayout),
    ("RewindStateMachine", "/Script/ShooterGame.EquippableStateMachineComponent", GroupKind::RepLayout),
    ("UseAbilityStateMachine", "/Script/ShooterGame.EquippableStateMachineComponent", GroupKind::RepLayout),
    ("Resume_StateMachine", "/Script/ShooterGame.EquippableStateMachineComponent", GroupKind::RepLayout),
    ("Sprint_StateMachine", "/Script/ShooterGame.EquippableStateMachineComponent", GroupKind::RepLayout),
    ("Slide_StateMachine", "/Script/ShooterGame.EquippableStateMachineComponent", GroupKind::RepLayout),
    ("ProjectileStateMachine", "/Script/ShooterGame.EquippableStateMachineComponent", GroupKind::RepLayout),
    ("EquipStateMachine", "/Script/ShooterGame.EquippableStateMachineComponent", GroupKind::RepLayout),
    ("PrimaryTriggerActionStateMachine", "/Script/ShooterGame.EquippableStateMachineComponent", GroupKind::RepLayout),
    ("EquippableStateMachine_Activate", "/Script/ShooterGame.EquippableStateMachineComponent", GroupKind::RepLayout),
    ("LaserStateMachine", "/Script/ShooterGame.EquippableStateMachineComponent", GroupKind::RepLayout),
    ("SelfResStateMachine", "/Script/ShooterGame.EquippableStateMachineComponent", GroupKind::RepLayout),
    ("Ability State Machine (EquippableStateMachine)", "/Script/ShooterGame.EquippableStateMachineComponent", GroupKind::RepLayout),
    ("TimerStateMachine", "/Script/ShooterGame.EquippableStateMachineComponent", GroupKind::RepLayout),
    ("CloakStateMachine", "/Script/ShooterGame.EquippableStateMachineComponent", GroupKind::RepLayout),
    ("SpontaneousEquip_StateMachine", "/Script/ShooterGame.EquippableStateMachineComponent", GroupKind::RepLayout),
    ("EquippableStateMachine_Dart", "/Script/ShooterGame.EquippableStateMachineComponent", GroupKind::RepLayout),
    ("EquippableStateMachine_Attack", "/Script/ShooterGame.EquippableStateMachineComponent", GroupKind::RepLayout),
    ("EquippableStateMachine_PickUpOnCooldown", "/Script/ShooterGame.EquippableStateMachineComponent", GroupKind::RepLayout),
    ("SwapCameras_StateMachine", "/Script/ShooterGame.EquippableStateMachineComponent", GroupKind::RepLayout),
    // One native component instantiated twice; its handle 2 is `AuthResourceAmount`,
    // which superseded a hand-named `AmmoCount`. Magazine reads 0..100, reserve 0..200.
    ("MagazineAmmo", "/Script/ShooterGame.AmmoComponent", GroupKind::RepLayout),
    ("ReserveAmmo", "/Script/ShooterGame.AmmoComponent", GroupKind::RepLayout),
    ("CalloutRegionTracker", "/Script/ShooterGame.CalloutRegionTrackingComponent", GroupKind::RepLayout),
    ("VisionComponent", "/Script/ShooterGame.ShooterCharacterVisionComponent", GroupKind::RepLayout),
    ("HealthDamageSection", "/Script/ShooterGame.ChildDamageSectionComponent", GroupKind::RepLayout),
    ("ShieldDamageSection", "/Script/ShooterGame.ChildDamageSectionComponent", GroupKind::RepLayout),
    ("OverhealDamageSection", "/Script/ShooterGame.ChildDamageSectionComponent", GroupKind::RepLayout),
    ("PreventDeathDamageSection", "/Script/ShooterGame.AttachedDamageSectionComponent", GroupKind::RepLayout),
    ("UsableComponent_EquippableGroundPickup", "/Script/ShooterGame.UsableComponent_EquippableGroundPickups", GroupKind::RepLayout),
    ("Usable_PickUp", "/Script/ShooterGame.UsableComponent", GroupKind::RepLayout),
    ("StealthComp", "/Script/ShooterGame.SimpleVisualTimelineStealthComp", GroupKind::RepLayout),
    ("StealthV1AddedForAISight", "/Script/ShooterGame.StealthComponent", GroupKind::RepLayout),
    ("Collision Static Mesh", "/Script/Engine.StaticMeshComponent", GroupKind::RepLayout),
    ("PMAimToolingPointsTarget", "/Script/InputTooling.AimToolingPointsTargetComponent", GroupKind::RepLayout),
    // Handle 2 is `AttachParent`, a packed reference: 16 bits vs the target's 24
    // is the NetGUID's size, not a type disagreement. The other handles match.
    ("PMAimToolingTarget", "/Script/InputTooling.AimToolingSkeletalTargetComponent", GroupKind::RepLayout),
    // Blueprint component classes: the replay declares the Blueprint group, not
    // its native parent's.
    ("Comp_Ability_CooldownComponent1", "/Game/Characters/Components/Comp_Ability_CooldownComponent.Comp_Ability_CooldownComponent_C", GroupKind::RepLayout),
    ("DamageSection_Vampire_Q_BloodArmor", "/Game/Characters/Vampire/S0/Ability_Q/DamageSection_Vampire_Q_Heal_BloodArmor.DamageSection_Vampire_Q_Heal_BloodArmor_C", GroupKind::RepLayout),
    ("ChooseTeleportSpot_StateComponent", "/Game/Characters/States/ChooseMapLocationOnNavMesh_StateComponent.ChooseMapLocationOnNavMesh_StateComponent_C", GroupKind::RepLayout),
    // The armour section: the four 13.06 armour items add it as this subclass of
    // `AttachedDamageSectionComponent`. Declared in all 529 corpus replays and
    // 6,895 checkpoints whose leaf carries RepLayout rows, with every handle used
    // (2 `bAlive`, 5 `LastKnownDamageOwner`, checkpoint-only 3 `Life`). In the 486
    // replays carrying Phoenix's native `PreventDeathDamageSection`,
    // `unique_leaf_match` (the leaf plus `Component`) binds the native parent
    // first, which declares only `bAlive`, so handles 3 and 5 stay unnamed there.
    // docs/DATA.md has the numbers.
    ("AttachedDamageSection", "/Game/Gear/BasicArmorAttachedDamageSection.BasicArmorAttachedDamageSection_C", GroupKind::RepLayout),
    // GAS attribute sets: runtime subobjects in no cooked asset, so these rest on
    // the wire. `_2`: its 116 handles are a subset of the native group's 122, all
    // 32 bits like the named instance. `_1`, over all 536 corpus replays carrying
    // it (11.07-13.06): every main-stream handle (116 per replay) and every
    // checkpoint's (10,455 checkpoints) is declared by the native group, and all
    // 2,681,046 rows are 32 bits, the named instance's width on each handle
    // (124,280 per-replay comparisons, none different). The same set again, for
    // actors that are not player characters.
    ("AresAttributeSet_1", "/Script/ShooterGame.AresAttributeSet", GroupKind::RepLayout),
    ("AresAttributeSet_2", "/Script/ShooterGame.AresAttributeSet", GroupKind::RepLayout),
];

/// Everything a block's resolution reads that is not cache state. All six stay
/// in the key though each branch ignores some: dropping a field the resolution
/// does read is silent byte movement, while an over-precise key costs entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct BlockKey {
    channel_index: u32,
    actor_net_guid: u32,
    class_net_guid: u32,
    object_net_guid: u32,
    is_actor: bool,
    has_rep_layout: bool,
}

/// Memo for [`ExportSink::resolve_block`]. See the module docs for why it is
/// exactly equivalent to recomputing.
#[derive(Debug, Clone, Default)]
pub(super) struct BlockPathMemo {
    /// The three stamps `entries` was last valid for (see the module doc);
    /// `guid_generation` is the only one a GUID write, via `register_path` or
    /// frame-level ExportData, moves.
    schema_generation: u64,
    resolution_generation: u64,
    guid_generation: u64,
    entries: FxHashMap<BlockKey, (Arc<str>, u32, &'static str, &'static str)>,
}

impl BlockPathMemo {
    /// Drop everything if any stamp has moved, then report the current entry
    /// for `key`.
    fn get(
        &mut self,
        key: &BlockKey,
        schema: u64,
        resolution: u64,
        guid: u64,
    ) -> Option<(Arc<str>, u32, &'static str, &'static str)> {
        if self.schema_generation != schema
            || self.resolution_generation != resolution
            || self.guid_generation != guid
        {
            self.entries.clear();
            self.schema_generation = schema;
            self.resolution_generation = resolution;
            self.guid_generation = guid;
            return None;
        }
        self.entries
            .get(key)
            .map(|(path, count, group_source, count_source)| {
                (Arc::clone(path), *count, *group_source, *count_source)
            })
    }

    fn insert(
        &mut self,
        key: BlockKey,
        path: Arc<str>,
        count: u32,
        group_source: &'static str,
        count_source: &'static str,
    ) {
        self.entries
            .insert(key, (path, count, group_source, count_source));
    }
}

impl ExportSink<'_> {
    pub(super) fn current_block_resolution_evidence(
        &self,
        channel_index: u32,
        actor_guid: u32,
        header: &ContentBlockHeader,
    ) -> BlockResolutionEvidence {
        let (actor_archetype_outer_path, actor_archetype_path) = if header.is_actor {
            self.resolve_actor_package_and_archetype(channel_index, actor_guid)
        } else {
            (None, None)
        };
        let group_declared = self
            .cache
            .get_group_by_path(&self.current_group_path)
            .is_some();
        BlockResolutionEvidence {
            group_resolution_source: self.current_group_resolution_source,
            group_declared,
            function_count_source: self.current_function_count_source,
            resolution_memo_hit: self.current_resolution_memo_hit,
            actor_archetype_path,
            actor_archetype_outer_path,
            actor_guid_path: self.cache.get_path_by_guid(actor_guid).map(str::to_owned),
            class_guid_path: header
                .has_class_net_guid
                .then(|| self.cache.get_path_by_guid(header.class_net_guid.0))
                .flatten()
                .map(str::to_owned),
            object_guid_path: (!header.is_actor)
                .then(|| self.cache.get_path_by_guid(header.object_net_guid.0))
                .flatten()
                .map(str::to_owned),
            object_outer_path: (!header.is_actor)
                .then(|| self.cache.get_outer_path(header.object_net_guid.0))
                .flatten()
                .map(str::to_owned),
        }
    }

    /// Resolve one content block, memoised: set `current_group_path` and return
    /// the function count its RPC handles are read against (0 for a RepLayout
    /// block or an unresolved group).
    pub(super) fn resolve_block(
        &mut self,
        channel_index: u32,
        actor_net_guid: NetworkGuid,
        header: &ContentBlockHeader,
    ) -> u32 {
        let key = BlockKey {
            channel_index,
            actor_net_guid: actor_net_guid.0,
            class_net_guid: header.class_net_guid.0,
            object_net_guid: header.object_net_guid.0,
            is_actor: header.is_actor,
            has_rep_layout: header.has_rep_layout,
        };
        let schema = self.cache.schema_generation();
        let resolution = self.channel_state.resolution_generation;
        let guid = self.cache.guid_generation();
        if let Some((path, count, group_source, count_source)) = self
            .channel_state
            .block_paths
            .get(&key, schema, resolution, guid)
        {
            self.set_current_group_path(path);
            self.current_group_resolution_source = group_source;
            self.current_function_count_source = count_source;
            self.current_resolution_memo_hit = true;
            return count;
        }

        let (path, mut group_source) =
            self.resolve_group_path(channel_index, actor_net_guid.0, header);
        let interned = self.channel_state.names.intern(&path);
        self.set_current_group_path(interned);
        let (count, count_source) = if header.has_rep_layout {
            (0, "rep_layout_not_applicable")
        } else {
            // May replace `current_group_path`, which is why the memo stores
            // the pair and this line comes before the insert.
            self.resolve_function_count(header, channel_index, actor_net_guid.0)
        };
        if count_source == "class_net_cache_instance_name" {
            group_source = "class_net_cache_instance_name";
        }
        self.current_group_resolution_source = group_source;
        self.current_function_count_source = count_source;
        self.current_resolution_memo_hit = false;
        let resolved = Arc::clone(&self.current_group_path);
        self.channel_state
            .block_paths
            .insert(key, resolved, count, group_source, count_source);
        count
    }

    /// The export group path for a block and the rule that produced it; a
    /// ClassNetCache block binds only to a `*_ClassNetCache` group ([`GroupKind`]).
    fn resolve_group_path(
        &self,
        channel_index: u32,
        guid: u32,
        header: &ContentBlockHeader,
    ) -> (String, &'static str) {
        if header.is_actor {
            self.resolve_actor_group_path(channel_index, guid, header)
        } else {
            self.resolve_subobject_group_path(header)
        }
    }

    /// Actor blocks: the first group of the wanted kind matched by the combined
    /// archetype candidate, the archetype's outer (package) path, the archetype
    /// path unless it is a CDO, then the actor GUID's own path.
    fn resolve_actor_group_path(
        &self,
        channel_index: u32,
        actor_guid: u32,
        header: &ContentBlockHeader,
    ) -> (String, &'static str) {
        let (package_path, archetype_path) =
            self.resolve_actor_package_and_archetype(channel_index, actor_guid);
        let combined =
            self.create_combined_candidate(package_path.as_deref(), archetype_path.as_deref());

        // A ClassNetCache block accepts only a `_ClassNetCache` group: the key
        // `AggroBot_PC.AggroBot_PC_C` would otherwise bind the 14-field
        // RepLayout group, not the 4-field ClassNetCache one, and
        // ReadSerializedInt would consume the wrong number of bits.
        let want = GroupKind::for_block(header);

        if let Some(hit) = self.match_group(combined.as_deref(), want) {
            return (hit, "actor_archetype_combined");
        }
        if let Some(hit) = self.match_group(package_path.as_deref(), want) {
            return (hit, "actor_archetype_outer");
        }
        if let Some(arch) = archetype_path.as_deref() {
            if !is_class_default_object_path(arch) {
                if let Some(hit) = self.match_group(Some(arch), want) {
                    return (hit, "actor_archetype_path");
                }
            }
        }

        if let Some(actor_path) = self.cache.get_path_by_guid(actor_guid) {
            if let Some(hit) = self.match_group(Some(actor_path), want) {
                return (hit, "actor_guid_path");
            }
            // UniqueLeafMatch, as the class and subobject paths apply it: a
            // static actor arrives as a bare instance name (`AresWorldSettings`)
            // that no key above matches to `/Script/ShooterGame.AresWorldSettings`.
            // An ambiguous leaf binds nothing, so this determines, not guesses.
            // The `_ClassNetCache` guard is load-bearing here: the instance-name
            // resolver in `resolve_function_count` runs only while the path is
            // still bare, and a RepLayout group returned for a ClassNetCache
            // block would hand ReadSerializedInt the wrong capacity. (A leaf
            // match meets the guard only if the actor's own path ends in
            // `_ClassNetCache`, so on a ClassNetCache block this is inert.)
            if let Some(g) = self.cache.unique_leaf_match(actor_path) {
                if want.accepts(g) {
                    return (g.path.clone(), "actor_guid_unique_leaf");
                }
            }
            // The table: bare `InventoryComponent` never leaf-matches `AresInventory`.
            if let Some(known) = resolve_known_subobject_class_path(actor_path, want) {
                if let Some(hit) = self.match_group(Some(known), want) {
                    return (hit, "actor_guid_known_remap");
                }
            }
            return (actor_path.to_owned(), "actor_guid_unresolved_fallback");
        }

        // The best candidate even unmatched: the export needs a path, and the
        // raw bits still ship.
        if let Some(combined) = combined {
            (combined, "actor_archetype_combined_unresolved_fallback")
        } else if let Some(package_path) = package_path {
            (package_path, "actor_archetype_outer_unresolved_fallback")
        } else {
            (format!("<unknown:{actor_guid}>"), "actor_unknown")
        }
    }

    /// Subobject blocks: the `class_net_guid` path (as a group, a unique leaf,
    /// then the table), else the object's outer path as a group, then its own
    /// path the same three ways.
    fn resolve_subobject_group_path(&self, header: &ContentBlockHeader) -> (String, &'static str) {
        let want = GroupKind::for_block(header);

        if header.class_net_guid.0 != 0 {
            if let Some(class_path) = self.cache.get_path_by_guid(header.class_net_guid.0) {
                if let Some(hit) = self.match_group(Some(class_path), want) {
                    return (hit, "subobject_class_guid_path");
                }
                if let Some(g) = self.cache.unique_leaf_match(class_path) {
                    if want.accepts(g) {
                        return (g.path.clone(), "subobject_class_guid_unique_leaf");
                    }
                }
                if let Some(known) = resolve_known_subobject_class_path(class_path, want) {
                    if let Some(hit) = self.match_group(Some(known), want) {
                        return (hit, "subobject_class_guid_known_remap");
                    }
                }
                return (
                    class_path.to_owned(),
                    "subobject_class_guid_unresolved_fallback",
                );
            }
        }

        if header.object_net_guid.0 != 0 {
            if let Some(obj_path) = self.cache.get_path_by_guid(header.object_net_guid.0) {
                let outer = self.cache.get_outer_path(header.object_net_guid.0);
                if let Some(hit) = self.match_group(outer, want) {
                    return (hit, "subobject_object_outer_path");
                }
                if let Some(hit) = self.match_group(Some(obj_path), want) {
                    return (hit, "subobject_object_guid_path");
                }
                if let Some(g) = self.cache.unique_leaf_match(obj_path) {
                    if want.accepts(g) {
                        return (g.path.clone(), "subobject_object_guid_unique_leaf");
                    }
                }
                if let Some(known) = resolve_known_subobject_class_path(obj_path, want) {
                    if let Some(hit) = self.match_group(Some(known), want) {
                        return (hit, "subobject_object_guid_known_remap");
                    }
                }
                return (
                    obj_path.to_owned(),
                    "subobject_object_guid_unresolved_fallback",
                );
            }
        }

        let fallback_guid = if header.class_net_guid.0 != 0 {
            header.class_net_guid.0
        } else {
            header.object_net_guid.0
        };
        (format!("<unknown:{fallback_guid}>"), "subobject_unknown")
    }

    /// The canonical path of the first declared group of kind `want` that a
    /// lookup key of `candidate` finds. One function, so no caller can pick the
    /// wrong key generator or skip the `_ClassNetCache` guard.
    fn match_group(&self, candidate: Option<&str>, want: GroupKind) -> Option<String> {
        let candidate = candidate?;
        want.find(candidate, |key| {
            let group = self.cache.get_group_by_path(key)?;
            want.accepts(group).then(|| group.path.clone())
        })
    }

    /// `(package_or_class_path, archetype_path)` for an actor channel; either is
    /// `None` while the cache lacks the mapping. The two `to_owned()` calls stay:
    /// 5-P replaced them with borrows, measured no change (median 1.580 s vs
    /// 1.590 s, interleaved runs) and reverted. Not calling this -- the memo --
    /// is what moved the number.
    pub(super) fn resolve_actor_package_and_archetype(
        &self,
        channel_index: u32,
        actor_guid: u32,
    ) -> (Option<String>, Option<String>) {
        let archetype_guid =
            match channel_archetype(self.channel_state, channel_index, NetworkGuid(actor_guid)) {
                Some(g) if g.is_valid() => g,
                _ => return (None, None),
            };

        let archetype_path = self
            .cache
            .get_path_by_guid(archetype_guid.0)
            .map(|s| s.to_owned());

        let package_path = self
            .cache
            .get_outer_path(archetype_guid.0)
            .map(|s| s.to_owned());

        (package_path, archetype_path)
    }

    /// `package_path` joined with the class name of the archetype's leaf
    /// (`Default__` stripped), unless the package path already ends with it.
    pub(super) fn create_combined_candidate(
        &self,
        package_path: Option<&str>,
        archetype_path: Option<&str>,
    ) -> Option<String> {
        let pkg = package_path?;
        let class_name = extract_class_name_from_archetype(archetype_path?)?;

        if ends_with_class_name(pkg, class_name) {
            return Some(pkg.to_owned());
        }

        let mut combined = String::with_capacity(pkg.len() + 1 + class_name.len());
        combined.push_str(pkg);
        combined.push('.');
        combined.push_str(class_name);
        Some(combined)
    }

    /// A ClassNetCache block's function count: its group's declared slot count,
    /// as the C# `ReadSerializedInt(FunctionsByHandle.Length)` reads it
    /// (`FunctionsByHandle` sized to `replayGroup.NetFieldExportsLength`). 0 when
    /// no group resolves: the payload is then preserved raw and counted as a
    /// stream failure, never dropped. May replace `current_group_path` (the
    /// bare-instance-name branch), which is why the memo stores the pair.
    fn resolve_function_count(
        &mut self,
        header: &ContentBlockHeader,
        channel_index: u32,
        actor_guid: u32,
    ) -> (u32, &'static str) {
        if let Some(group) = self.cache.get_group_by_path(&self.current_group_path) {
            if is_class_net_cache(group) {
                return (group.len(), "current_resolved_group");
            }
        }

        if header.class_net_guid.0 != 0 {
            if let Some(class_path) = self.cache.get_path_by_guid(header.class_net_guid.0) {
                if let Some(len) = self.class_net_cache_len(class_path) {
                    return (len, "class_guid_path");
                }
            }
        }

        if header.is_actor {
            let (package_path, archetype_path) =
                self.resolve_actor_package_and_archetype(channel_index, actor_guid);
            if let Some(combined) =
                self.create_combined_candidate(package_path.as_deref(), archetype_path.as_deref())
            {
                if let Some(len) = self.class_net_cache_len(&combined) {
                    return (len, "actor_archetype_combined");
                }
            }
        }

        // A bare instance name -- static actors (`BombDestination_A`,
        // `WindowShieldA1`), stably named subobjects (`ForceModuleManager`,
        // `AudDeadeyeVOComponent`) -- has no archetype or class GUID on the wire.
        // Match it against the replay's own `_ClassNetCache` groups by Unreal
        // naming conventions and take the declared capacity, never a guess.
        // The C# reference fails these blocks.
        if is_bare_instance_name(&self.current_group_path) {
            if let Some(group) = self
                .cache
                .resolve_cnc_for_instance_name(&self.current_group_path)
            {
                let len = group.len();
                // Rebind the block to that group, or its RPCs are named by handle.
                let resolved = self.channel_state.names.intern(&group.path);
                self.set_current_group_path(resolved);
                return (len, "class_net_cache_instance_name");
            }
        }

        (0, "unresolved_class_net_cache")
    }

    /// Declared length of the `_ClassNetCache` group `candidate` resolves to.
    fn class_net_cache_len(&self, candidate: &str) -> Option<u32> {
        find_class_net_cache_key(candidate, |key| {
            let group = self.cache.get_group_by_path(key)?;
            is_class_net_cache(group).then(|| group.len())
        })
    }
}

pub(super) struct BlockResolutionEvidence {
    pub group_resolution_source: &'static str,
    pub group_declared: bool,
    pub function_count_source: &'static str,
    pub resolution_memo_hit: bool,
    pub actor_archetype_path: Option<String>,
    pub actor_archetype_outer_path: Option<String>,
    pub actor_guid_path: Option<String>,
    pub class_guid_path: Option<String>,
    pub object_guid_path: Option<String>,
    pub object_outer_path: Option<String>,
}

/// Which family of export group a block may bind to. It selects both the
/// lookup-key generator and the acceptance test, which must move together: a
/// ClassNetCache block bound to a RepLayout group reads the wrong handle width,
/// a decode failure, not a naming one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GroupKind {
    RepLayout,
    ClassNetCache,
}

impl GroupKind {
    fn for_block(header: &ContentBlockHeader) -> Self {
        if header.has_rep_layout {
            Self::RepLayout
        } else {
            Self::ClassNetCache
        }
    }

    /// Probe this kind's lookup keys in order until one is accepted. The
    /// generators are visitors, not `Vec<String>` builders, so a path with no
    /// alias -- the common case -- allocates nothing.
    fn find<T>(self, path: &str, probe: impl FnMut(&str) -> Option<T>) -> Option<T> {
        match self {
            Self::RepLayout => find_replay_path_key(path, probe),
            Self::ClassNetCache => find_class_net_cache_key(path, probe),
        }
    }

    fn accepts(self, group: &NetFieldExportGroup) -> bool {
        match self {
            Self::RepLayout => true,
            Self::ClassNetCache => is_class_net_cache(group),
        }
    }
}

fn is_class_net_cache(group: &NetFieldExportGroup) -> bool {
    group.path.ends_with(vrf_schema::CLASS_NET_CACHE_SUFFIX)
}

/// A path with no separators and no `<unknown:` marker -- the shape the
/// instance-name resolver is allowed to see.
fn is_bare_instance_name(path: &str) -> bool {
    !path.contains('/') && !path.contains('.') && !path.contains(':') && !path.starts_with('<')
}

/// The archetype path's leaf without a `Default__` prefix:
/// `/Game/Characters/AggroBot/AggroBot_PC.Default__AggroBot_PC_C` -> `AggroBot_PC_C`.
fn extract_class_name_from_archetype(archetype_path: &str) -> Option<&str> {
    if archetype_path.is_empty() {
        return None;
    }
    let leaf_start = archetype_path.rfind(['/', '.', ':']).map_or(0, |i| i + 1);
    let leaf = &archetype_path[leaf_start..];
    if leaf.is_empty() {
        return None;
    }
    Some(leaf.strip_prefix("Default__").unwrap_or(leaf))
}

/// Check whether `path` already ends with `.{class_name}` or `:{class_name}`.
fn ends_with_class_name(path: &str, class_name: &str) -> bool {
    let sep_index = path.len().wrapping_sub(class_name.len() + 1);
    if sep_index >= path.len() {
        return false;
    }
    let sep = path.as_bytes()[sep_index];
    (sep == b'.' || sep == b':') && path[sep_index + 1..] == *class_name
}

/// Whether the path's leaf is a class default object (`Default__...`), as the
/// C# `ReplayPath.IsClassDefaultObjectPath`.
fn is_class_default_object_path(path: &str) -> bool {
    let leaf_start = path.rfind(['/', '.', ':']).map_or(0, |i| i + 1);
    path[leaf_start..].starts_with("Default__")
}

/// The [`KNOWN_SUBOBJECT_CLASS_PATHS`] target for `object_path`'s leaf and kind.
fn resolve_known_subobject_class_path(object_path: &str, want: GroupKind) -> Option<&'static str> {
    let leaf_start = object_path.rfind(['/', '.', ':']).map_or(0, |i| i + 1);
    let leaf = &object_path[leaf_start..];
    KNOWN_SUBOBJECT_CLASS_PATHS
        .iter()
        .find(|(name, _, kind)| *name == leaf && *kind == want)
        .map(|(_, class_path, _)| *class_path)
}

/// An archetype GUID together with the actor it was read for.
///
/// Channel indices are recycled and a *static* actor carries no archetype to
/// displace its predecessor's, so a channel-keyed archetype decodes the new
/// actor under the old class -- and nothing fails: fields get names, values get
/// types, and rows ship under a class the actor never had. Destruction retires
/// the entry; dormancy keeps it (a wake-up need not repeat the archetype), and
/// the actor stamp keeps a surviving dormant entry safe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ChannelArchetype {
    actor: NetworkGuid,
    archetype: NetworkGuid,
}

/// Register an archetype for a channel and, if that changed anything, tell the
/// memo. Called from `on_actor_open`.
pub(super) fn set_channel_archetype(
    state: &mut ChannelState,
    channel_index: u32,
    actor: NetworkGuid,
    archetype: NetworkGuid,
) {
    let entry = ChannelArchetype { actor, archetype };
    if state.archetypes.get(&channel_index) == Some(&entry) {
        return;
    }
    state.archetypes.insert(channel_index, entry);
    state.note_resolution_input_changed();
}

/// The archetype GUID recorded for `channel_index`, but only if it was read for
/// `actor`. See [`ChannelArchetype`].
pub(super) fn channel_archetype(
    state: &ChannelState,
    channel_index: u32,
    actor: NetworkGuid,
) -> Option<NetworkGuid> {
    state
        .archetypes
        .get(&channel_index)
        .filter(|entry| entry.actor == actor)
        .map(|entry| entry.archetype)
}

/// Retire the archetype state owned by a destroyed channel index.
pub(super) fn retire_channel_archetype(state: &mut ChannelState, channel_index: u32) {
    if state.archetypes.remove(&channel_index).is_some() {
        state.note_resolution_input_changed();
    }
}

#[cfg(test)]
mod tests {
    #![allow(unused_must_use)]

    use super::*;
    use crate::sink::RecordBuffers;
    use crate::sink::test_fixtures::channel_open;
    use vrf_net::pipeline::ReplicationSink;
    use vrf_schema::NetGuidCache;

    /// Build a cache holding `groups` as declared export groups and mapping
    /// `guid` to `guid_path`, then run one actor content block through a sink
    /// and report the group path it resolved to.
    fn actor_group_path_for(
        groups: &[&str],
        guid: u32,
        guid_path: &str,
        has_rep_layout: bool,
    ) -> String {
        let mut cache = NetGuidCache::new();
        for (i, path) in groups.iter().enumerate() {
            cache.add_export_group(vrf_schema::NetFieldExportGroup::new(
                (*path).to_owned(),
                i as u32 + 1,
                4,
            ));
        }
        cache.set_net_guid_path(guid, guid_path.to_owned(), None);
        let mut channel_state = ChannelState::new();
        let mut records = RecordBuffers::default();
        let mut sink = ExportSink::new(&mut cache, &mut channel_state, &mut records);

        let header = ContentBlockHeader {
            has_rep_layout,
            is_actor: true,
            ..ContentBlockHeader::default()
        };
        // No archetype is registered, so only the actor-GUID path is left.
        sink.on_content_block(3, NetworkGuid(guid), &header);
        sink.current_group_path.to_string()
    }

    /// An actor whose own NetGUID path is a unique leaf of a declared group
    /// reaches that group, as the class and subobject paths do.
    #[test]
    fn an_actor_path_that_is_a_unique_leaf_reaches_its_declared_group() {
        assert_eq!(
            actor_group_path_for(
                &["/Script/ShooterGame.AresWorldSettings"],
                42,
                "AresWorldSettings",
                true,
            ),
            "/Script/ShooterGame.AresWorldSettings",
        );
    }

    /// A component whose bare name is not its group's leaf (`InventoryComponent`
    /// vs `AresInventory`) reaches the group through the table.
    #[test]
    fn a_blueprint_component_name_reaches_its_native_parent_group() {
        assert_eq!(
            actor_group_path_for(
                &["/Script/ShooterGame.AresInventory"],
                100,
                "InventoryComponent",
                true,
            ),
            "/Script/ShooterGame.AresInventory",
        );
    }

    /// The pairs read from the cooked game, in all their shapes (one class under
    /// many names, two instances of one class, another module, an engine class,
    /// `/Game/` Blueprint classes, names with spaces), every 13.06 addition
    /// listed. Each reaches its group from a RepLayout block and stays bare on a
    /// ClassNetCache block, which fails if the remap stops asking the block kind;
    /// the table check below fails on any ClassNetCache pair beyond the C#
    /// reference's four, which would bind that leaf's RPC stream to
    /// `<target>_ClassNetCache` -- the AbilitySystem mis-parse.
    #[test]
    fn component_names_read_from_the_game_reach_their_native_groups() {
        const ESM: &str = "/Script/ShooterGame.EquippableStateMachineComponent";
        for (leaf, native) in [
            ("ZoomStateMachine", ESM),
            ("Gun_StateMachine", ESM),
            ("MagazineAmmo", "/Script/ShooterGame.AmmoComponent"),
            ("ReserveAmmo", "/Script/ShooterGame.AmmoComponent"),
            (
                "CalloutRegionTracker",
                "/Script/ShooterGame.CalloutRegionTrackingComponent",
            ),
            (
                "VisionComponent",
                "/Script/ShooterGame.ShooterCharacterVisionComponent",
            ),
            (
                "PMAimToolingPointsTarget",
                "/Script/InputTooling.AimToolingPointsTargetComponent",
            ),
            // Added from the 13.06 containers by tools/extract_component_classes.
            ("Resume_StateMachine", ESM),
            ("Sprint_StateMachine", ESM),
            ("Slide_StateMachine", ESM),
            ("ProjectileStateMachine", ESM),
            ("EquipStateMachine", ESM),
            ("PrimaryTriggerActionStateMachine", ESM),
            ("EquippableStateMachine_Activate", ESM),
            ("LaserStateMachine", ESM),
            ("SelfResStateMachine", ESM),
            ("Ability State Machine (EquippableStateMachine)", ESM),
            ("TimerStateMachine", ESM),
            ("CloakStateMachine", ESM),
            ("SpontaneousEquip_StateMachine", ESM),
            ("EquippableStateMachine_Dart", ESM),
            ("EquippableStateMachine_Attack", ESM),
            ("EquippableStateMachine_PickUpOnCooldown", ESM),
            ("SwapCameras_StateMachine", ESM),
            (
                "ShieldDamageSection",
                "/Script/ShooterGame.ChildDamageSectionComponent",
            ),
            (
                "OverhealDamageSection",
                "/Script/ShooterGame.ChildDamageSectionComponent",
            ),
            (
                "PreventDeathDamageSection",
                "/Script/ShooterGame.AttachedDamageSectionComponent",
            ),
            ("Usable_PickUp", "/Script/ShooterGame.UsableComponent"),
            (
                "StealthComp",
                "/Script/ShooterGame.SimpleVisualTimelineStealthComp",
            ),
            (
                "StealthV1AddedForAISight",
                "/Script/ShooterGame.StealthComponent",
            ),
            (
                "Collision Static Mesh",
                "/Script/Engine.StaticMeshComponent",
            ),
            (
                "PMAimToolingTarget",
                "/Script/InputTooling.AimToolingSkeletalTargetComponent",
            ),
            (
                "Comp_Ability_CooldownComponent1",
                "/Game/Characters/Components/Comp_Ability_CooldownComponent.Comp_Ability_CooldownComponent_C",
            ),
            (
                "DamageSection_Vampire_Q_BloodArmor",
                "/Game/Characters/Vampire/S0/Ability_Q/DamageSection_Vampire_Q_Heal_BloodArmor.DamageSection_Vampire_Q_Heal_BloodArmor_C",
            ),
            (
                "ChooseTeleportSpot_StateComponent",
                "/Game/Characters/States/ChooseMapLocationOnNavMesh_StateComponent.ChooseMapLocationOnNavMesh_StateComponent_C",
            ),
            (
                "AttachedDamageSection",
                "/Game/Gear/BasicArmorAttachedDamageSection.BasicArmorAttachedDamageSection_C",
            ),
        ] {
            assert_eq!(
                actor_group_path_for(&[native], 100, leaf, true),
                native,
                "{leaf}",
            );
            assert_eq!(
                actor_group_path_for(&[native], 100, leaf, false),
                leaf,
                "{leaf}: a ClassNetCache block is not handed the RepLayout group",
            );
        }

        // The table itself, through the resolver's lookup, so it also fails if
        // that lookup stops filtering by kind.
        let class_net_cache: Vec<&str> = KNOWN_SUBOBJECT_CLASS_PATHS
            .iter()
            .filter(|(_, _, kind)| *kind == GroupKind::ClassNetCache)
            .map(|(leaf, _, _)| *leaf)
            .collect();
        assert_eq!(
            class_net_cache,
            [
                "ReplayEffect",
                "EffectManager",
                "LocationalEffectManager",
                "DamageHandlerComponent",
            ],
            "only the C# reference's pairs remap a ClassNetCache block",
        );
        for (leaf, _, kind) in KNOWN_SUBOBJECT_CLASS_PATHS {
            if *kind == GroupKind::RepLayout {
                assert_eq!(
                    resolve_known_subobject_class_path(leaf, GroupKind::ClassNetCache),
                    None,
                    "{leaf} is RepLayout-only",
                );
            }
        }
    }

    /// Both attribute-set instances reach the native group (wire evidence on
    /// the table entry).
    #[test]
    fn the_second_attribute_set_reaches_the_same_native_group() {
        for leaf in ["AresAttributeSet_1", "AresAttributeSet_2"] {
            assert_eq!(
                actor_group_path_for(&["/Script/ShooterGame.AresAttributeSet"], 100, leaf, true),
                "/Script/ShooterGame.AresAttributeSet",
                "{leaf}",
            );
        }
    }

    /// A component the table does not list keeps its bare path rather than
    /// joining whichever native group looks close.
    #[test]
    fn an_unlisted_component_name_is_not_remapped() {
        assert_eq!(
            actor_group_path_for(
                &["/Script/ShooterGame.AmmoComponent"],
                100,
                "SomeUnlistedComponent",
                true,
            ),
            "SomeUnlistedComponent",
        );
    }

    /// Two declared groups sharing a leaf mark it `AMBIGUOUS_LEAF`: the actor
    /// keeps its raw path rather than binding to whichever came first.
    #[test]
    fn an_ambiguous_actor_leaf_binds_to_nothing() {
        assert_eq!(
            actor_group_path_for(
                &[
                    "/Script/ShooterGame.AresWorldSettings",
                    "/Game/Maps/Ascent.AresWorldSettings",
                ],
                42,
                "AresWorldSettings",
                true,
            ),
            "AresWorldSettings",
        );
    }

    /// A ClassNetCache actor block is not captured by a RepLayout leaf; see the
    /// `_ClassNetCache` guard in `resolve_actor_group_path`.
    #[test]
    fn a_class_net_cache_actor_block_is_not_captured_by_a_rep_layout_leaf() {
        assert_eq!(
            actor_group_path_for(
                &["/Script/ShooterGame.AresWorldSettings"],
                42,
                "AresWorldSettings",
                false,
            ),
            "AresWorldSettings",
        );
    }

    /// A static actor on a recycled channel is not decoded under the previous
    /// actor's archetype; see [`ChannelArchetype`].
    #[test]
    fn a_reused_channel_does_not_inherit_the_previous_actors_archetype() {
        let mut cache = NetGuidCache::new();
        cache.add_export_group(vrf_schema::NetFieldExportGroup::new(
            "/Game/Effects/Smoke.Smoke_C".to_owned(),
            1,
            4,
        ));
        // GUID 8 is the dynamic actor's archetype; its outer names the class.
        cache.set_net_guid_path(
            8,
            "Default__Smoke_C".to_owned(),
            Some(vrf_schema::NetworkGuid(9)),
        );
        cache.set_net_guid_path(9, "/Game/Effects/Smoke".to_owned(), None);
        // GUID 77 is a static actor that opens later on the same channel and
        // brings no archetype with it.
        cache.set_net_guid_path(77, "SomeStaticProp".to_owned(), None);

        let mut channel_state = ChannelState::new();
        let mut records = RecordBuffers::default();
        let mut sink = ExportSink::new(&mut cache, &mut channel_state, &mut records);

        let header = ContentBlockHeader {
            has_rep_layout: true,
            is_actor: true,
            ..ContentBlockHeader::default()
        };

        // The dynamic actor opens on channel 5 and resolves to its class.
        sink.on_actor_open(&channel_open(5, 42, 8));
        sink.on_content_block(5, NetworkGuid(42), &header);
        assert_eq!(
            &*sink.current_group_path, "/Game/Effects/Smoke.Smoke_C",
            "the dynamic actor must reach its own class",
        );

        // It closes, and the channel number is handed to a static actor.
        sink.on_actor_close(5, NetworkGuid(42), false);
        sink.on_actor_open(&channel_open(5, 77, 0));
        sink.on_content_block(5, NetworkGuid(77), &header);
        assert_eq!(
            &*sink.current_group_path, "SomeStaticProp",
            "the static actor must not be decoded under the previous actor's class",
        );
    }

    /// ...while the *same* actor waking from dormancy without re-sending its
    /// archetype keeps its class.
    #[test]
    fn the_same_actor_reopening_without_an_archetype_keeps_its_class() {
        let mut cache = NetGuidCache::new();
        cache.add_export_group(vrf_schema::NetFieldExportGroup::new(
            "/Game/Effects/Smoke.Smoke_C".to_owned(),
            1,
            4,
        ));
        cache.set_net_guid_path(
            8,
            "Default__Smoke_C".to_owned(),
            Some(vrf_schema::NetworkGuid(9)),
        );
        cache.set_net_guid_path(9, "/Game/Effects/Smoke".to_owned(), None);

        let mut channel_state = ChannelState::new();
        let mut records = RecordBuffers::default();
        let mut sink = ExportSink::new(&mut cache, &mut channel_state, &mut records);

        let header = ContentBlockHeader {
            has_rep_layout: true,
            is_actor: true,
            ..ContentBlockHeader::default()
        };

        sink.on_actor_open(&channel_open(5, 42, 8));
        sink.on_actor_close(5, NetworkGuid(42), true);
        // Woken: same actor, same channel, no archetype on the wire.
        sink.on_actor_open(&channel_open(5, 42, 0));
        sink.on_content_block(5, NetworkGuid(42), &header);
        assert_eq!(
            &*sink.current_group_path, "/Game/Effects/Smoke.Smoke_C",
            "a dormancy wake must not lose the actor's class",
        );
    }

    /// The memo does not answer for a block whose inputs moved: a
    /// `register_path` between two resolutions of one key moves only
    /// `NetGuidCache::guid_generation`, and without that stamp in
    /// `BlockPathMemo::get` the second returns the first's answer.
    #[test]
    fn a_guid_path_registration_invalidates_the_memo() {
        use vrf_net::net_guid::GuidPathSink;

        let mut cache = NetGuidCache::new();
        cache.add_export_group(vrf_schema::NetFieldExportGroup::new(
            "/Script/ShooterGame.AresWorldSettings".to_owned(),
            1,
            4,
        ));
        let mut channel_state = ChannelState::new();
        let mut records = RecordBuffers::default();

        let header = ContentBlockHeader {
            has_rep_layout: true,
            is_actor: true,
            ..ContentBlockHeader::default()
        };

        // First pass: GUID 42 has no path at all, so the block falls through to
        // the unknown marker.
        {
            let mut sink = ExportSink::new(&mut cache, &mut channel_state, &mut records);
            sink.on_content_block(3, NetworkGuid(42), &header);
            assert_eq!(&*sink.current_group_path, "<unknown:42>");
        }
        // Second pass: the same block, after the wire declared the GUID's path.
        {
            let mut sink = ExportSink::new(&mut cache, &mut channel_state, &mut records);
            sink.register_path(42, "AresWorldSettings", NetworkGuid(0));
            sink.on_content_block(3, NetworkGuid(42), &header);
            assert_eq!(
                &*sink.current_group_path, "/Script/ShooterGame.AresWorldSettings",
                "the memo answered with a resolution its inputs had invalidated"
            );
        }
    }

    /// The same for a GUID write that bypasses `register_path`: frame-level
    /// ExportData writes the cache between one packet's sink and the next, and
    /// `guid_generation` is the only stamp that moves.
    #[test]
    fn a_frame_level_guid_registration_invalidates_the_memo() {
        let mut cache = NetGuidCache::new();
        cache.add_export_group(vrf_schema::NetFieldExportGroup::new(
            "/Script/ShooterGame.AresWorldSettings".to_owned(),
            1,
            4,
        ));
        let mut channel_state = ChannelState::new();
        let mut records = RecordBuffers::default();

        let header = ContentBlockHeader {
            has_rep_layout: true,
            is_actor: true,
            ..ContentBlockHeader::default()
        };

        {
            let mut sink = ExportSink::new(&mut cache, &mut channel_state, &mut records);
            sink.on_content_block(3, NetworkGuid(42), &header);
            assert_eq!(&*sink.current_group_path, "<unknown:42>");
        }
        // The frame-level write: straight into the cache, with no sink alive.
        cache.set_net_guid_path(42, "AresWorldSettings".to_owned(), None);
        {
            let mut sink = ExportSink::new(&mut cache, &mut channel_state, &mut records);
            sink.on_content_block(3, NetworkGuid(42), &header);
            assert_eq!(
                &*sink.current_group_path, "/Script/ShooterGame.AresWorldSettings",
                "the memo answered with a resolution a frame-level GUID write had invalidated"
            );
        }
    }

    /// A repeat registration that changes nothing leaves the memo standing (if
    /// every re-declaration moved a stamp, the hit rate would collapse). The
    /// asserts read the memo's hit flag, so they hold whichever stamp moves.
    #[test]
    fn a_redundant_registration_leaves_the_memo_alone() {
        use vrf_net::net_guid::GuidPathSink;

        let mut cache = NetGuidCache::new();
        let mut channel_state = ChannelState::new();
        let mut records = RecordBuffers::default();
        let mut sink = ExportSink::new(&mut cache, &mut channel_state, &mut records);

        let header = ContentBlockHeader {
            has_rep_layout: true,
            is_actor: true,
            ..ContentBlockHeader::default()
        };

        sink.register_path(42, "AresWorldSettings", NetworkGuid(7));
        sink.on_content_block(3, NetworkGuid(42), &header);
        assert!(!sink.current_resolution_memo_hit, "a first resolution hit");
        sink.register_path(42, "AresWorldSettings", NetworkGuid(7));
        sink.on_content_block(3, NetworkGuid(42), &header);
        assert!(
            sink.current_resolution_memo_hit,
            "an unchanged re-declaration invalidated the memo"
        );

        // A different outer is a real change (the `outer_net_guid` column and a
        // resolution input): an invalid outer removes the one the cache held.
        sink.register_path(42, "AresWorldSettings", NetworkGuid(0));
        sink.on_content_block(3, NetworkGuid(42), &header);
        assert!(
            !sink.current_resolution_memo_hit,
            "a changed outer left the memo standing"
        );
        assert_eq!(sink.cache.get_outer_guid(42), None);
    }
}
