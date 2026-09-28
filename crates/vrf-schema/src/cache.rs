//! The replay-wide accumulator for all dynamically-received schema state.
//!
//! [`NetGuidCache`] is the central authority for:
//!
//! - **path -> group**: find an export group by its full path string.
//! - **path_name_index -> group**: find an export group by the numeric index the
//!   engine assigns once and reuses for the rest of the replay.
//! - **NetGUID -> object path**: map the 32-bit runtime object ID to its
//!   human-readable path (populated from export-GUID bunches).
//! - **NetGUID -> outer NetGUID**: track the containment hierarchy so callers can
//!   walk from a component to its owning actor.
//! - **GameplayTag index -> name**: the `NetworkGameplayTagNodeIndex` group's
//!   fields double as a tag name table.
//!
//! This state accumulates over the entire replay and is never reset.
//!
//! The bare-name resolvers that sit on top of the leaf index
//! ([`NetGuidCache::unique_leaf_match`] and
//! [`NetGuidCache::resolve_cnc_for_instance_name`]) live in
//! [`crate::resolve`]; this module is the storage and the direct lookups.
//!
//! # Hashing
//!
//! Every map here uses [`FxHashMap`] rather than the standard hasher. See
//! [`crate::hash`] for the measurement and the security trade that motivates it.

use crate::error::{Result, SchemaError};
use crate::export::{NetFieldExport, NetFieldExportGroup};
use crate::guid::{NetGuidEntry, NetworkGuid};
use crate::hash::FxHashMap;
use crate::path::for_each_replay_path_key;
use crate::resolve::register_leaf;

/// The path used for the gameplay-tag name table group.
const GAMEPLAY_TAG_GROUP_PATH: &str = "NetworkGameplayTagNodeIndex";

/// Replay-wide schema accumulator.
///
/// All lookups are O(1) via `HashMap`. Field access within a group is O(1) via
/// direct `Vec` indexing (see [`NetFieldExportGroup::get_field`]).
pub struct NetGuidCache {
    /// path (String, ordinal) -> group index into `groups`.
    by_path: FxHashMap<String, usize>,
    /// path_name_index (u32) -> group index into `groups`.
    by_index: FxHashMap<u32, usize>,
    /// leaf name -> group index. Used by
    /// [`unique_leaf_match`](NetGuidCache::unique_leaf_match) to resolve bare
    /// class names (e.g. `AresAttributeSet`) to their full export group path
    /// (e.g. `/Script/ShooterGame.AresAttributeSet`).
    ///
    /// Mirrors the C# `ContentBlockPathResolver.UniqueLeafMatch` logic: a bare
    /// name is only resolved if exactly ONE group has a path ending with
    /// `.{name}`. Ambiguous names (multiple groups sharing the same leaf) are
    /// stored as `usize::MAX` to signal rejection.
    by_leaf: FxHashMap<String, usize>,
    /// Central storage for all groups.
    groups: Vec<NetFieldExportGroup>,
    /// NetGUID value -> object path string.
    guid_to_path: FxHashMap<u32, String>,
    /// NetGUID value -> outer NetGUID value (containment hierarchy).
    guid_to_outer: FxHashMap<u32, NetworkGuid>,
    /// Bumped whenever the set of group paths changes. See
    /// [`Self::schema_generation`].
    schema_generation: u64,
    /// Bumped whenever `guid_to_path` or `guid_to_outer` changes. See
    /// [`Self::guid_generation`].
    guid_generation: u64,
    /// Net-field exports [`Self::set_field_on_group`] could not place --
    /// either the group was not found or, the only way that can happen from
    /// [`crate::reader::read_net_field_exports`] (which validates the group
    /// exists before this is called), the handle exceeded the group's
    /// declared length. The C# reference logs a warning and moves on; this
    /// crate had no equivalent, so a field dropped this way vanished with no
    /// counter. See [`Self::dropped_field_exports`].
    ///
    /// Scoped to whichever `NetGuidCache` this is: the checkpoint pass builds
    /// a fresh one per chunk (`driver::checkpoints::process_chunk`) and drops
    /// it after, so a drop there never reaches the ReplayData-pass cache this
    /// counter is read from in the manifest.
    dropped_field_exports: u64,
}

impl NetGuidCache {
    /// Sentinel value in `by_leaf` indicating an ambiguous leaf (multiple
    /// groups share the same trailing class name).
    pub(crate) const AMBIGUOUS_LEAF: usize = usize::MAX;

    /// Create an empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self {
            by_path: FxHashMap::default(),
            by_index: FxHashMap::default(),
            by_leaf: FxHashMap::default(),
            groups: Vec::new(),
            guid_to_path: FxHashMap::default(),
            guid_to_outer: FxHashMap::default(),
            schema_generation: 0,
            guid_generation: 0,
            dropped_field_exports: 0,
        }
    }

    /// The leaf index, for the resolvers in [`crate::resolve`].
    pub(crate) fn leaf_index(&self) -> &FxHashMap<String, usize> {
        &self.by_leaf
    }

    /// A counter that changes whenever the set of group paths changes.
    ///
    /// Callers that memoise a *pure function of the group paths* -- "which group
    /// does this path resolve to", "is this function name unique across all
    /// groups" -- can stamp the memo with this value and discard it when the
    /// value moves. It is bumped by every [`Self::add_export_group`] and by
    /// [`Self::clear`], which are the only operations that add a path, add a
    /// path alias, or remove one.
    ///
    /// It deliberately does NOT track field mutations
    /// ([`Self::set_field_on_group`]): a memo of field contents would be unsound
    /// to key on this. Only path-set queries may use it. A successful
    /// `add_export_group` always bumps the generation because a supported merge
    /// can replace canonical path metadata and its aliases.
    #[must_use]
    pub fn schema_generation(&self) -> u64 {
        self.schema_generation
    }

    /// Register a new export group or merge it with an existing one.
    ///
    /// If exactly one coordinate already exists, that canonical group adopts
    /// both incoming coordinates and merges the fields. If the path and index
    /// identify different groups, the update fails without mutating the cache.
    ///
    /// Returns the index of the canonical group.
    pub fn add_export_group(&mut self, group: NetFieldExportGroup) -> Result<usize> {
        let existing_by_path = self.by_path.get(&group.path).copied();
        let existing_by_index = self.by_index.get(&group.path_name_index).copied();

        if let (Some(path_idx), Some(index_idx)) = (existing_by_path, existing_by_index) {
            if path_idx != index_idx {
                return Err(SchemaError::CrossedExportGroupIdentity {
                    path: group.path,
                    path_name_index: group.path_name_index,
                    path_group: self.groups[path_idx].path.clone(),
                    index_group: self.groups[index_idx].path.clone(),
                });
            }
        }

        let idx = if let Some(idx) = existing_by_path {
            // Same path: this is the same class re-declaring (or extending)
            // its own export group, so its previously-set handle slots are
            // still valid and are preserved.
            self.groups[idx].merge_from(&group);
            self.groups[idx].path = group.path;
            self.groups[idx].path_name_index = group.path_name_index;
            // The path can have arrived as another spelling and the index can
            // be new; either retires keys, so every lookup is rebuilt.
            self.rebuild_group_indexes();
            idx
        } else if let Some(idx) = existing_by_index {
            // Index matched, path did not: `path_name_index` was reused for a
            // path this cache has never seen, which the module doc says the
            // engine does over a replay's lifetime as old FNames are freed and
            // reassigned. The group at `idx` belongs to whatever class held
            // that index before -- merging would let its handle slots (field
            // names, `compatible_checksum`) survive into the new class, so a
            // content block resolved against the new path would read the OLD
            // class's field name/type at a handle the NEW class never
            // declared. Replace the group outright instead of merging into
            // it.
            self.groups[idx] = group;
            // The old class's spellings and leaf claim go with it.
            self.rebuild_group_indexes();
            idx
        } else {
            // Neither coordinate is known: a new group. It retires no key, so
            // it is registered on its own rather than by rebuilding every
            // group; `index_group` says why both leave the same lookups.
            let idx = self.groups.len();
            self.groups.push(group);
            self.index_group(idx);
            idx
        };

        self.schema_generation = self.schema_generation.wrapping_add(1);
        Ok(idx)
    }

    /// Rebuild every group-derived lookup after a canonical identity changes.
    ///
    /// A merge or a replacement can retire keys: a group's old spellings, its
    /// old index, its claim on a leaf. None of that can be taken back one key
    /// at a time -- `AMBIGUOUS_LEAF` does not record which groups claimed the
    /// leaf, and a spelling given up by one group may belong to another that
    /// `by_path` no longer names -- so those two arms rebuild from `groups`.
    /// A new group retires nothing; see [`Self::index_group`].
    fn rebuild_group_indexes(&mut self) {
        self.by_path.clear();
        self.by_index.clear();
        self.by_leaf.clear();
        for idx in 0..self.groups.len() {
            self.index_group(idx);
        }
    }

    /// Register group `idx` in the lookups: every spelling of its path, its
    /// `path_name_index`, and its leaf. A later registration wins a spelling
    /// or an index; a second claimant turns a leaf ambiguous.
    ///
    /// [`Self::rebuild_group_indexes`] is this, called for every group in
    /// storage order. Calling it once, for a group just pushed, leaves the
    /// maps that rebuild would, because:
    ///
    /// - the maps already equal a rebuild of the older groups. Every successful
    ///   `add_export_group` ends in one of the two, an error returns before it
    ///   touches anything, `clear` empties both sides, and nothing else writes
    ///   these maps or a group's path or index; and
    /// - a rebuild registers the new group last, and each write here is final
    ///   for the last group either way: a spelling or index it shares with an
    ///   older group ends up pointing at it, and a leaf it shares ends up
    ///   ambiguous.
    ///
    /// The cache tests hold every arm of `add_export_group` to a full rebuild
    /// after every call. The rebuild this spares a new group re-registers every
    /// existing group, and the checkpoint pass -- a fresh cache per checkpoint,
    /// every group new -- paid it once per group; the measured cost is in
    /// docs/PERFORMANCE_NOTES.md#registering-a-new-export-group.
    fn index_group(&mut self, idx: usize) {
        let group = &self.groups[idx];
        for_each_replay_path_key(&group.path, |key| {
            self.by_path.insert(key.to_owned(), idx);
        });
        self.by_index.insert(group.path_name_index, idx);
        register_leaf(&mut self.by_leaf, &group.path, idx);
    }

    /// Look up a group by its `path_name_index`.
    #[must_use]
    pub fn get_group_by_index(&self, path_name_index: u32) -> Option<&NetFieldExportGroup> {
        self.by_index
            .get(&path_name_index)
            .map(|&i| &self.groups[i])
    }

    /// Get a mutable reference to a group by its `path_name_index`.
    ///
    /// For its field slots only. The lookups are keyed on the group's `path`
    /// and `path_name_index`; changing either through this reference would
    /// leave them naming the old values.
    #[must_use]
    pub fn get_group_by_index_mut(
        &mut self,
        path_name_index: u32,
    ) -> Option<&mut NetFieldExportGroup> {
        self.by_index
            .get(&path_name_index)
            .copied()
            .map(move |i| &mut self.groups[i])
    }

    /// Look up a group by its full path (ordinal, case-sensitive).
    ///
    /// The hottest lookup in the crate: the sink probes it 2,989,695 times over
    /// the reference replay's export, once per candidate key per content block.
    #[must_use]
    pub fn get_group_by_path(&self, path: &str) -> Option<&NetFieldExportGroup> {
        self.by_path.get(path).map(|&i| &self.groups[i])
    }

    /// A counter that changes whenever a NetGUID -> path or NetGUID -> outer
    /// mapping changes.
    ///
    /// `set_net_guid_path` is called both through
    /// [`crate::reader::read_export_guids`] (frame-level ExportData, run once
    /// per frame ahead of that frame's packet loop) and through per-block
    /// export-GUID bunches during packet processing; neither caller routes
    /// through the same object that owns a group-path resolution memo, so a
    /// memo built from those resolutions has no other way to see this map
    /// change. Callers that memoise a function of `guid_to_path` /
    /// `guid_to_outer` must stamp with this and discard on a mismatch, the
    /// same pattern as [`Self::schema_generation`].
    #[must_use]
    pub fn guid_generation(&self) -> u64 {
        self.guid_generation
    }

    /// Register a NetGUID -> path mapping (from export GUID bunches).
    ///
    /// A no-op write -- the same path and outer this GUID already has -- does
    /// not bump [`Self::guid_generation`]. This isn't only about the redundant
    /// hashmap writes: [`crate::read_checkpoint_tables`] reads a fresh `NetGuidCache` per
    /// checkpoint, but the frame-level ExportData section
    /// ([`crate::read_export_guids`]) calls this once per exported
    /// GUID on *every* frame that re-declares one, with no pre-check of its
    /// own (unlike `vrfkit`'s `register_path`, which skips the call entirely
    /// when nothing changed, for its own reason -- an allocation, not this
    /// one). Without the check here, a replay that keeps re-sending a GUID's
    /// path bumps `guid_generation` every such frame, which -- now that a
    /// group-path resolution memo keys on this generation -- would collapse
    /// the memo's hit rate to near zero on exactly that traffic.
    pub fn set_net_guid_path(&mut self, net_guid: u32, path: String, outer: Option<NetworkGuid>) {
        let outer = outer.filter(|g| g.is_valid());
        if self.guid_to_path.get(&net_guid).map(String::as_str) == Some(path.as_str())
            && self.guid_to_outer.get(&net_guid).copied() == outer
        {
            return;
        }
        self.guid_to_path.insert(net_guid, path);
        match outer {
            Some(g) => {
                self.guid_to_outer.insert(net_guid, g);
            }
            None => {
                self.guid_to_outer.remove(&net_guid);
            }
        }
        self.guid_generation = self.guid_generation.wrapping_add(1);
    }

    /// Resolve a NetGUID to its object path.
    #[must_use]
    pub fn get_path_by_guid(&self, net_guid: u32) -> Option<&str> {
        self.guid_to_path.get(&net_guid).map(String::as_str)
    }

    /// Get the outer (containing) NetGUID for a given NetGUID.
    #[must_use]
    pub fn get_outer_guid(&self, net_guid: u32) -> Option<NetworkGuid> {
        self.guid_to_outer.get(&net_guid).copied()
    }

    /// Every registered NetGUID with its path and outer GUID.
    ///
    /// Order is unspecified (backed by a `HashMap`); sort if determinism
    /// matters.
    #[must_use]
    pub fn net_guid_entries(&self) -> Vec<NetGuidEntry<'_>> {
        self.guid_to_path
            .iter()
            .map(|(&net_guid, path)| NetGuidEntry {
                net_guid,
                path: path.as_str(),
                outer_net_guid: self.guid_to_outer.get(&net_guid).map(|o| o.0),
            })
            .collect()
    }

    /// Walk the outer chain to resolve the outer object's path.
    #[must_use]
    pub fn get_outer_path(&self, net_guid: u32) -> Option<&str> {
        let outer = self.get_outer_guid(net_guid)?;
        self.get_path_by_guid(outer.0)
    }

    /// Look up a gameplay-tag name by its network index.
    ///
    /// Tags are stored in the `NetworkGameplayTagNodeIndex` export group, where
    /// each field's handle is the tag index and its name is the tag string.
    #[must_use]
    pub fn get_gameplay_tag_name(&self, tag_index: u32) -> Option<&str> {
        let group = self.get_group_by_path(GAMEPLAY_TAG_GROUP_PATH)?;
        group.get_field(tag_index).map(|f| f.name.as_str())
    }

    /// Set a field directly on the group identified by `path_name_index`.
    ///
    /// Returns `true` if the group was found and the field handle was in range.
    pub fn set_field_on_group(&mut self, path_name_index: u32, field: NetFieldExport) -> bool {
        let placed = if let Some(group) = self.get_group_by_index_mut(path_name_index) {
            group.set_field(field)
        } else {
            false
        };
        if !placed {
            self.dropped_field_exports += 1;
        }
        placed
    }

    /// Net-field exports dropped by [`Self::set_field_on_group`]. See its doc.
    #[must_use]
    pub fn dropped_field_exports(&self) -> u64 {
        self.dropped_field_exports
    }

    /// Remove all state. Intended for tests or replay-boundary resets.
    pub fn clear(&mut self) {
        self.schema_generation = self.schema_generation.wrapping_add(1);
        self.guid_generation = self.guid_generation.wrapping_add(1);
        self.by_path.clear();
        self.by_index.clear();
        self.by_leaf.clear();
        self.groups.clear();
        self.guid_to_path.clear();
        self.guid_to_outer.clear();
    }

    /// Number of registered groups.
    #[must_use]
    pub fn group_count(&self) -> usize {
        self.groups.len()
    }

    /// Read-only access to all registered groups.
    ///
    /// Insertion-ordered, so it is stable across runs regardless of how the
    /// maps above hash their keys.
    #[must_use]
    pub fn groups(&self) -> &[NetFieldExportGroup] {
        &self.groups
    }
}

impl Default for NetGuidCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export::NetFieldExport;
    // -- NetGuidCache unit tests (ported from NetGuidCacheTests.cs) -----------

    #[test]
    fn property_exports_never_shadow_the_class_net_cache_group() {
        const CLASS: &str = "/Script/ShooterGame.DamageableComponent";
        const RPC: &str = "/Script/ShooterGame.DamageableComponent_ClassNetCache";

        for properties_first in [false, true] {
            let mut cache = NetGuidCache::new();
            let mut properties = NetFieldExportGroup::new(CLASS.into(), 1, 4);
            properties.set_field(NetFieldExport {
                handle: 0,
                compatible_checksum: 11,
                name: "Property".into(),
            });
            let mut functions = NetFieldExportGroup::new(RPC.into(), 2, 9);
            functions.set_field(NetFieldExport {
                handle: 0,
                compatible_checksum: 22,
                name: "MulticastNotifyDamage_Base".into(),
            });

            if properties_first {
                cache.add_export_group(properties).unwrap();
                cache.add_export_group(functions).unwrap();
            } else {
                cache.add_export_group(functions).unwrap();
                cache.add_export_group(properties).unwrap();
            }

            // A late property re-declaration must not replace the function
            // handle capacity or field names, regardless of insertion order.
            cache
                .add_export_group(NetFieldExportGroup::new(CLASS.into(), 1, 4))
                .unwrap();
            let property_group = cache.get_group_by_path(CLASS).unwrap();
            let rpc_group = cache.get_group_by_path(RPC).unwrap();
            assert_eq!(property_group.path_name_index, 1);
            assert_eq!(property_group.len(), 4);
            assert_eq!(property_group.get_field(0).unwrap().name, "Property");
            assert_eq!(rpc_group.path_name_index, 2);
            assert_eq!(rpc_group.len(), 9);
            assert_eq!(
                rpc_group.get_field(0).unwrap().name,
                "MulticastNotifyDamage_Base"
            );
            assert_eq!(cache.get_group_by_index(2).unwrap().path, RPC);
            assert_eq!(cache.group_count(), 2);
        }
    }

    #[test]
    fn cache_merge_expands_and_preserves() {
        let mut cache = NetGuidCache::new();
        let mut group = NetFieldExportGroup::new("/Game/Test.Test_C".into(), 7, 2);
        group.set_field(NetFieldExport {
            handle: 1,
            compatible_checksum: 17,
            name: "ExistingField".into(),
        });
        cache.add_export_group(group).unwrap();

        // Re-add with larger capacity.
        let expanded = NetFieldExportGroup::new("/Game/Test.Test_C".into(), 7, 4);
        cache.add_export_group(expanded).unwrap();

        let result = cache.get_group_by_index(7).unwrap();
        assert_eq!(result.len(), 4);
        assert_eq!(result.get_field(1).unwrap().name, "ExistingField");
    }

    /// A `path_name_index` reused for a genuinely different path must not
    /// hand the new class the old class's handle table. Before the fix, the
    /// index-only match merged into the existing group (preserving whatever
    /// slots the old class had set), so a content block resolved against the
    /// new path could read the old class's field name/type at a handle the
    /// new class never declared.
    #[test]
    fn same_index_with_new_path_does_not_inherit_old_fields() {
        let mut cache = NetGuidCache::new();
        cache
            .add_export_group(NetFieldExportGroup::new("/Script/G.Old".into(), 7, 2))
            .unwrap();
        cache.set_field_on_group(
            7,
            NetFieldExport {
                handle: 0,
                compatible_checksum: 111,
                name: "OldFieldZero".into(),
            },
        );

        cache
            .add_export_group(NetFieldExportGroup::new("/Script/G.New".into(), 7, 2))
            .unwrap();

        let group = cache.get_group_by_index(7).unwrap();
        assert_eq!(group.path, "/Script/G.New");
        assert!(
            group.get_field(0).is_none(),
            "handle 0 must not carry the old class's field after the index was reused: {:?}",
            group.get_field(0)
        );
    }

    /// `set_field_on_group` against a `path_name_index` no group holds must
    /// refuse the field and count the drop. `read_net_field_exports` checks
    /// that the group exists before it gets here, so no reader test reaches
    /// this branch; only a direct call does.
    #[test]
    fn set_field_on_unregistered_group_returns_false_and_counts_the_drop() {
        let mut cache = NetGuidCache::new();
        cache
            .add_export_group(NetFieldExportGroup::new("/Script/G.A".into(), 7, 1))
            .unwrap();
        assert_eq!(cache.dropped_field_exports(), 0);

        let placed = cache.set_field_on_group(
            8,
            NetFieldExport {
                handle: 0,
                compatible_checksum: 0,
                name: "Orphan".into(),
            },
        );

        assert!(!placed);
        assert_eq!(cache.dropped_field_exports(), 1);
        // The group registered at another index must not have received it.
        let group = cache.get_group_by_index(7).unwrap();
        assert_eq!(group.populated_fields().count(), 0);
        assert_eq!(cache.group_count(), 1);
    }

    #[test]
    fn cache_set_net_guid_path_stores_and_resolves() {
        let mut cache = NetGuidCache::new();
        cache.set_net_guid_path(17, "/Game/Test.Test_C".into(), None);

        assert_eq!(cache.get_path_by_guid(17).unwrap(), "/Game/Test.Test_C");
    }

    /// A redundant `set_net_guid_path` call -- same path, same outer -- must
    /// not bump `guid_generation`. Frame-level ExportData re-declares a GUID's
    /// path on every frame that re-exports it, with no pre-check of its own
    /// (unlike `vrfkit::sink::register_path`'s deliberate one); without this,
    /// a memo keyed on `guid_generation` would be invalidated on every such
    /// frame regardless of whether the mapping actually changed.
    #[test]
    fn a_redundant_set_net_guid_path_call_does_not_bump_guid_generation() {
        let mut cache = NetGuidCache::new();
        cache.set_net_guid_path(17, "/Game/Test.Test_C".into(), None);
        let after_first = cache.guid_generation();

        cache.set_net_guid_path(17, "/Game/Test.Test_C".into(), None);
        assert_eq!(cache.guid_generation(), after_first, "no change, no bump");

        cache.set_net_guid_path(17, "/Game/Test.Other_C".into(), None);
        assert_ne!(
            cache.guid_generation(),
            after_first,
            "a real change must still bump"
        );
    }

    #[test]
    fn cache_outer_guid_chain() {
        let mut cache = NetGuidCache::new();
        let outer = NetworkGuid(11);
        cache.set_net_guid_path(17, "Default__Test_C".into(), Some(outer));
        cache.set_net_guid_path(11, "/Game/Test.Test_C".into(), None);

        assert_eq!(cache.get_outer_guid(17).unwrap(), outer);
        assert_eq!(cache.get_outer_path(17).unwrap(), "/Game/Test.Test_C");
    }

    #[test]
    fn cache_net_guid_entries_yields_guid_path_and_outer() {
        let mut cache = NetGuidCache::new();
        cache.set_net_guid_path(11, "/Game/Test.Test_C".into(), None);
        cache.set_net_guid_path(17, "FiringState".into(), Some(NetworkGuid(11)));

        let mut entries = cache.net_guid_entries();
        entries.sort_by_key(|e| e.net_guid);

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].net_guid, 11);
        assert_eq!(entries[0].path, "/Game/Test.Test_C");
        assert_eq!(entries[0].outer_net_guid, None);
        assert_eq!(entries[1].net_guid, 17);
        assert_eq!(entries[1].path, "FiringState");
        assert_eq!(entries[1].outer_net_guid, Some(11));
    }

    #[test]
    fn cache_clear_removes_all() {
        let mut cache = NetGuidCache::new();
        cache
            .add_export_group(NetFieldExportGroup::new("/Game/Test.Test_C".into(), 7, 2))
            .unwrap();
        cache.set_net_guid_path(17, "/Game/Test.Test_C".into(), None);

        cache.clear();

        assert!(cache.get_group_by_path("/Game/Test.Test_C").is_none());
        assert!(cache.get_group_by_index(7).is_none());
        assert!(cache.get_path_by_guid(17).is_none());
        assert_eq!(cache.group_count(), 0);
    }

    #[test]
    fn cache_gameplay_tag_lookup() {
        let mut cache = NetGuidCache::new();
        let mut group = NetFieldExportGroup::new("NetworkGameplayTagNodeIndex".into(), 99, 5);
        group.set_field(NetFieldExport {
            handle: 2,
            compatible_checksum: 0,
            name: "Ability.Active".into(),
        });
        cache.add_export_group(group).unwrap();

        assert_eq!(cache.get_gameplay_tag_name(2).unwrap(), "Ability.Active");
        assert!(cache.get_gameplay_tag_name(4).is_none()); // unpopulated slot
        assert!(cache.get_gameplay_tag_name(99).is_none()); // out of range
    }

    // -- The lookups after any sequence of registrations ------------------------

    use std::collections::BTreeMap;

    /// `by_path`, `by_index` and `by_leaf`, sorted so a mismatch prints as a
    /// readable diff.
    type Lookups = (
        BTreeMap<String, usize>,
        BTreeMap<u32, usize>,
        BTreeMap<String, usize>,
    );

    fn lookups(cache: &NetGuidCache) -> Lookups {
        (
            cache.by_path.iter().map(|(k, &v)| (k.clone(), v)).collect(),
            cache.by_index.iter().map(|(&k, &v)| (k, v)).collect(),
            cache.by_leaf.iter().map(|(k, &v)| (k.clone(), v)).collect(),
        )
    }

    /// `(path, path_name_index, declared slots)` for every group, in storage
    /// order.
    fn identities(cache: &NetGuidCache) -> Vec<(String, u32, usize)> {
        cache
            .groups
            .iter()
            .map(|g| (g.path.clone(), g.path_name_index, g.fields.len()))
            .collect()
    }

    fn spellings(path: &str) -> Vec<String> {
        let mut out = Vec::new();
        for_each_replay_path_key(path, |key| out.push(key.to_owned()));
        out
    }

    /// The leaf `register_leaf` files a path under, restated: the text after
    /// the last `.`, when there is a dot and that text is not empty.
    fn leaf_of(path: &str) -> Option<&str> {
        let dot = path.rfind('.')?;
        Some(&path[dot + 1..]).filter(|leaf| !leaf.is_empty())
    }

    /// `add_export_group`'s contract restated without an index of any kind.
    ///
    /// Every lookup is a scan over the groups, answered from the definition: a
    /// path spelling belongs to the LAST group that has it (a later
    /// registration overwrites an earlier one), an index to the last group
    /// holding it, and a leaf to its group only when exactly one group ends in
    /// it -- two or more claimants give `AMBIGUOUS_LEAF`. Nothing here depends
    /// on the order in which the cache wrote its maps, so the cache agrees with
    /// this model only if its maps hold what a full rebuild would put there.
    #[derive(Default)]
    struct Model {
        groups: Vec<(String, u32, usize)>,
    }

    impl Model {
        fn owner_of_spelling(&self, key: &str) -> Option<usize> {
            self.groups
                .iter()
                .rposition(|(path, _, _)| spellings(path).iter().any(|s| s == key))
        }

        fn owner_of_index(&self, index: u32) -> Option<usize> {
            self.groups.iter().rposition(|&(_, i, _)| i == index)
        }

        fn lookups(&self) -> Lookups {
            let mut by_path = BTreeMap::new();
            let mut by_index = BTreeMap::new();
            let mut by_leaf = BTreeMap::new();
            for (path, index, _) in &self.groups {
                for key in spellings(path) {
                    let owner = self.owner_of_spelling(&key);
                    by_path.insert(key, owner.expect("a group has its own spellings"));
                }
                let owner = self.owner_of_index(*index);
                by_index.insert(*index, owner.expect("a group has its own index"));
                if let Some(leaf) = leaf_of(path) {
                    let claimants: Vec<usize> = (0..self.groups.len())
                        .filter(|&i| leaf_of(&self.groups[i].0) == Some(leaf))
                        .collect();
                    let entry = match claimants[..] {
                        [only] => only,
                        _ => NetGuidCache::AMBIGUOUS_LEAF,
                    };
                    by_leaf.insert(leaf.to_owned(), entry);
                }
            }
            (by_path, by_index, by_leaf)
        }
    }

    /// Which arm of `add_export_group` a call takes, decided by the model.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
    enum Arm {
        /// Neither the path nor the index is known: a new group.
        Fresh,
        /// The same path at the same index: a pure re-declaration.
        Redeclared,
        /// The same path at a new index.
        NewIndex,
        /// Matched through another spelling: the canonical path changes.
        NewSpelling,
        /// The index is reused for an unknown path: the group is replaced.
        Replaced,
        /// The path and the index name different groups: rejected.
        Crossed,
    }

    /// What one call did, for the coverage tallies.
    struct Step {
        arm: Arm,
        /// A new group that registers an alias spelling as well as its path.
        fresh_alias: bool,
        /// A new group with a spelling an older group already had, so the
        /// later-wins rule decided who keeps it.
        took_older_spelling: bool,
        leaves_turned_ambiguous: usize,
        leaves_disambiguated: usize,
    }

    /// Apply one `add_export_group` to the cache and to the model, and hold the
    /// cache to the model on everything a caller can observe: the returned
    /// index or the exact error, the groups, the generation (one bump per
    /// success, none for an error), and the lookups -- which must equal both
    /// the model's and what `rebuild_group_indexes` makes of the cache's own
    /// groups. An error must also leave the groups and lookups untouched.
    fn apply(
        cache: &mut NetGuidCache,
        model: &mut Model,
        path: &str,
        index: u32,
        slots: u32,
        what: &str,
    ) -> Step {
        let by_spelling = model.owner_of_spelling(path);
        let by_index = model.owner_of_index(index);
        let arm = match (by_spelling, by_index) {
            (Some(a), Some(b)) if a != b => Arm::Crossed,
            (Some(a), _) if model.groups[a].0 != path => Arm::NewSpelling,
            (Some(a), _) if model.groups[a].1 != index => Arm::NewIndex,
            (Some(_), _) => Arm::Redeclared,
            (None, Some(_)) => Arm::Replaced,
            (None, None) => Arm::Fresh,
        };
        let before = model.lookups();
        let lookups_before = lookups(cache);
        let groups_before = identities(cache);
        let generation = cache.schema_generation();

        let incoming = (path.to_owned(), index, slots as usize);
        let expected: Result<usize> = match (arm, by_spelling, by_index) {
            (Arm::Crossed, Some(a), Some(b)) => Err(SchemaError::CrossedExportGroupIdentity {
                path: path.to_owned(),
                path_name_index: index,
                path_group: model.groups[a].0.clone(),
                index_group: model.groups[b].0.clone(),
            }),
            (Arm::Fresh, _, _) => {
                model.groups.push(incoming);
                Ok(model.groups.len() - 1)
            }
            (Arm::Replaced, _, Some(idx)) => {
                model.groups[idx] = incoming;
                Ok(idx)
            }
            (_, Some(idx), _) => {
                let kept = model.groups[idx].2.max(incoming.2);
                model.groups[idx] = (incoming.0, index, kept);
                Ok(idx)
            }
            _ => unreachable!("{what}: every arm names the group it acts on"),
        };

        let result = cache.add_export_group(NetFieldExportGroup::new(path.into(), index, slots));
        assert_eq!(result, expected, "{what}: wrong outcome");
        let bumps = u64::from(expected.is_ok());
        assert_eq!(
            cache.schema_generation(),
            generation.wrapping_add(bumps),
            "{what}: the generation must move exactly once per success"
        );
        if expected.is_err() {
            assert_eq!(
                identities(cache),
                groups_before,
                "{what}: an error moved a group"
            );
            assert_eq!(
                lookups(cache),
                lookups_before,
                "{what}: an error moved a lookup"
            );
        }
        assert_eq!(identities(cache), model.groups, "{what}: groups diverged");

        let after = model.lookups();
        let held = lookups(cache);
        assert_eq!(held, after, "{what}: the lookups diverged from the model");
        cache.rebuild_group_indexes();
        assert_eq!(
            held,
            lookups(cache),
            "{what}: the lookups differ from a full rebuild of the same groups"
        );

        let ambiguous = |map: &BTreeMap<String, usize>, leaf: &String| {
            map.get(leaf) == Some(&NetGuidCache::AMBIGUOUS_LEAF)
        };
        Step {
            arm,
            fresh_alias: arm == Arm::Fresh && spellings(path).len() > 1,
            took_older_spelling: arm == Arm::Fresh
                && spellings(path).iter().any(|s| before.0.contains_key(s)),
            leaves_turned_ambiguous: after
                .2
                .keys()
                .filter(|leaf| ambiguous(&after.2, leaf) && !ambiguous(&before.2, leaf))
                .count(),
            leaves_disambiguated: before
                .2
                .keys()
                .filter(|leaf| ambiguous(&before.2, leaf) && !ambiguous(&after.2, leaf))
                .count(),
        }
    }

    /// Every arm of `add_export_group`, with the effect each call is there for
    /// written above it. The arm and the effect columns are checked against the
    /// model, so a step that stops reaching the case its comment names fails
    /// here rather than quietly testing something easier.
    #[test]
    fn every_registration_arm_leaves_the_lookups_a_full_rebuild_would_build() {
        use Arm::*;
        // One class under three spellings. The first two are each other's
        // `/_Core/` alias; the third's alias is the first.
        const JETT: &str = "/Game/Characters/Jett/Jett.Jett_C";
        const JETT_CORE: &str = "/Game/Characters/_Core/Jett/Jett.Jett_C";
        const JETT_ODD: &str = "/Game/_Core/Characters/Jett/Jett.Jett_C";
        // (path, index, slots, arm, took an older spelling, leaves turned
        // ambiguous, leaves disambiguated)
        let script: &[(&str, u32, u32, Arm, bool, usize, usize)] = &[
            // A new path.
            ("/Script/A.Shared", 1, 1, Fresh, false, 0, 0),
            // A second claimant of the leaf `Shared`.
            ("/Script/B.Shared", 2, 1, Fresh, false, 1, 0),
            // The same path again, with more slots.
            ("/Script/A.Shared", 1, 3, Redeclared, false, 0, 0),
            // The same path at a new index: index 1 must go.
            ("/Script/A.Shared", 3, 3, NewIndex, false, 0, 0),
            // Index 2 reused: `B.Shared` goes, and `Shared` is unique again.
            ("/Script/C.Other", 2, 2, Replaced, false, 0, 1),
            // A bare leaf: its `Default__` spelling is registered too.
            ("Foo", 4, 1, Fresh, false, 0, 0),
            // Reached through that alias: the canonical path changes.
            ("Default__Foo", 4, 1, NewSpelling, false, 0, 0),
            // Its alias is the previous group's own path; the later one wins.
            ("Default__Default__Foo", 5, 1, Fresh, true, 0, 0),
            // Its `/_Core/` spelling is registered too.
            (JETT, 6, 1, Fresh, false, 0, 0),
            // Its alias is the previous group's path, and it shares `Jett_C`.
            (JETT_ODD, 7, 1, Fresh, true, 1, 0),
            // Reached through the `/_Core/` alias, at a new index.
            (JETT_CORE, 8, 2, NewSpelling, false, 0, 0),
            // A third claimant of `Jett_C`.
            ("/Script/B.Jett_C", 9, 1, Fresh, false, 0, 0),
            // One of three `Jett_C` claimants goes: still ambiguous. `Other`
            // gains its second claimant.
            ("/Script/D.Other", 7, 1, Replaced, false, 1, 0),
            // The path and the index name different groups: nothing changes.
            ("/Script/A.Shared", 2, 1, Crossed, false, 0, 0),
            // One of two `Other` claimants goes: unique again.
            ("/Script/E.Third", 7, 1, Replaced, false, 0, 1),
            // An empty leaf is not filed at all.
            ("/Game/Empty.", 10, 1, Fresh, false, 0, 0),
        ];

        let mut cache = NetGuidCache::new();
        let mut model = Model::default();
        for (n, &(path, index, slots, arm, older, turned, cleared)) in script.iter().enumerate() {
            let what = format!("step {n}: {path:?} at index {index}");
            let step = apply(&mut cache, &mut model, path, index, slots, &what);
            let effect = (
                step.arm,
                step.took_older_spelling,
                step.leaves_turned_ambiguous,
                step.leaves_disambiguated,
            );
            assert_eq!(
                effect,
                (arm, older, turned, cleared),
                "{what}: the script no longer reaches its case"
            );
        }

        cache.clear();
        let mut model = Model::default();
        assert_eq!(lookups(&cache), model.lookups(), "clear left a lookup");
        let step = apply(&mut cache, &mut model, "Foo", 1, 1, "after clear");
        assert_eq!(step.arm, Fresh);
    }

    /// SplitMix64. A fixed seed walks the same sequences on every run.
    struct SplitMix64(u64);

    impl SplitMix64 {
        fn below(&mut self, n: usize) -> usize {
            self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^= z >> 31;
            (z % n as u64) as usize
        }
    }

    /// Random registration sequences over a small universe, so paths, indexes
    /// and leaves collide constantly. The universe holds three claimants of one
    /// leaf, both `Default__` directions, a doubled prefix whose alias is
    /// another member's path, three spellings of one `/_Core/` class, a second
    /// class on the same leaf, an empty leaf and a `:` path.
    ///
    /// Every arm and every effect is tallied, and each tally must be non-zero:
    /// the equality `apply` checks is only evidence for a case the sequences
    /// actually reached.
    #[test]
    fn random_registration_sequences_leave_the_lookups_a_full_rebuild_would_build() {
        const PATHS: [&str; 14] = [
            "/Script/A.Shared",
            "/Script/B.Shared",
            "/Script/C.Shared",
            "/Script/A.Other",
            "/Script/A.Shared_ClassNetCache",
            "Foo",
            "Default__Foo",
            "Default__Default__Foo",
            "/Game/Characters/Jett/Jett.Jett_C",
            "/Game/Characters/_Core/Jett/Jett.Jett_C",
            "/Game/_Core/Characters/Jett/Jett.Jett_C",
            "/Game/Characters/Sage/Sage.Jett_C",
            "/Game/Empty.",
            "/Script/X.Y:Z",
        ];
        const SEQUENCES: usize = 300;
        const CALLS: usize = 40;
        const INDEXES: usize = 8;

        let mut rng = SplitMix64(0x7672_666b_6974_0001);
        let mut arms: BTreeMap<Arm, usize> = BTreeMap::new();
        let (mut fresh_alias, mut took_older, mut turned, mut cleared, mut clears) =
            (0, 0, 0, 0, 0);
        for sequence in 0..SEQUENCES {
            let mut cache = NetGuidCache::new();
            let mut model = Model::default();
            for call in 0..CALLS {
                if rng.below(50) == 0 {
                    cache.clear();
                    model = Model::default();
                    assert_eq!(lookups(&cache), model.lookups(), "clear left a lookup");
                    clears += 1;
                    continue;
                }
                let path = PATHS[rng.below(PATHS.len())];
                let index = rng.below(INDEXES) as u32;
                let slots = rng.below(4) as u32;
                let what = format!("sequence {sequence}, call {call}: {path:?} at {index}");
                let step = apply(&mut cache, &mut model, path, index, slots, &what);
                *arms.entry(step.arm).or_default() += 1;
                fresh_alias += usize::from(step.fresh_alias);
                took_older += usize::from(step.took_older_spelling);
                turned += step.leaves_turned_ambiguous;
                cleared += step.leaves_disambiguated;
            }
        }

        let tallies = format!(
            "arms {arms:?}, fresh with alias {fresh_alias}, took an older spelling \
             {took_older}, leaves turned ambiguous {turned}, disambiguated {cleared}, \
             clears {clears}"
        );
        for arm in [
            Arm::Fresh,
            Arm::Redeclared,
            Arm::NewIndex,
            Arm::NewSpelling,
            Arm::Replaced,
            Arm::Crossed,
        ] {
            assert!(
                arms.get(&arm).is_some_and(|&n| n > 0),
                "{arm:?} never taken: {tallies}"
            );
        }
        assert!(fresh_alias > 0, "no new group had an alias: {tallies}");
        assert!(
            took_older > 0,
            "no new group took an older spelling: {tallies}"
        );
        assert!(turned > 0, "no leaf turned ambiguous: {tallies}");
        assert!(cleared > 0, "no ambiguous leaf became unique: {tallies}");
        assert!(clears > 0, "clear was never exercised: {tallies}");
    }
}
