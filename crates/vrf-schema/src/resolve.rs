//! Bare-name resolution: turning the short names the wire carries into the
//! fully-qualified export groups the replay declared.
//!
//! # Why this exists at all
//!
//! A content block frequently identifies its actor or subobject by a bare
//! instance name -- `AresWorldSettings`, `BombDestination_A`,
//! `EquippableStateMachine` -- while the export group that describes its fields
//! is declared under a qualified path such as
//! `/Script/ShooterGame.AresWorldSettings`. Nothing on the wire connects the
//! two. These resolvers close that gap using only the replay's own declared
//! schema, never a hardcoded table of actor or map names.
//!
//! # The ambiguity rule is the whole contract
//!
//! Both resolvers bind a name only when exactly **one** group can claim it. A
//! name claimed by two groups resolves to nothing, deliberately: guessing which
//! one is meant hands `ReadSerializedInt` the wrong field capacity, and it then
//! consumes the wrong number of bits and produces plausible garbage rather than
//! an error. The [`AMBIGUOUS_LEAF`](NetGuidCache::AMBIGUOUS_LEAF) sentinel in
//! `by_leaf` is how that is recorded at registration time.
//!
//! Each resolver tries several candidate leaves in priority order, and the
//! first candidate claimed at all decides. An ambiguous one ends the search:
//! the unique candidate after it names a different class, so falling through
//! to it would be the same guess.

use crate::cache::NetGuidCache;
use crate::export::NetFieldExportGroup;
use crate::hash::FxHashMap;

/// Whether a name is a qualified path rather than a bare leaf.
///
/// Both resolvers reject qualified paths up front. Written as one pass over the
/// bytes because it runs on the export's hot path -- `unique_leaf_match` alone
/// is entered 174,485 times for the reference replay -- and the three separate
/// `contains` calls it replaces walked the string three times.
///
/// Byte-wise rather than char-wise is safe here: `/`, `.` and `:` are ASCII, and
/// UTF-8 never encodes an ASCII byte inside a multi-byte sequence.
#[inline]
pub(crate) fn has_path_separator(name: &str) -> bool {
    name.bytes().any(|b| matches!(b, b'/' | b'.' | b':'))
}

/// Longest `stem + suffix` candidate built without touching the heap.
///
/// Every observed leaf is far shorter: the longest in the reference replay's
/// schema is 61 bytes, and the longest suffix appended below is 23.
const JOIN_STACK_CAP: usize = 128;

/// Call `f` with `a` and `b` concatenated, without allocating when the result
/// fits in [`JOIN_STACK_CAP`] bytes.
///
/// The resolvers probe `by_leaf` with a name plus one of a few fixed suffixes.
/// Building that with a `String` cost one malloc per probe, which the reference
/// replay pays 149,035 times in `unique_leaf_match` (the count of calls that
/// miss the exact leaf and go on to the suffixed forms) plus 17,318 times
/// across `resolve_cnc_for_instance_name`'s stem attempts. The key is only
/// needed for the duration of a `HashMap::get`, so it never has to own storage
/// that outlives the call.
#[inline]
fn with_joined<R>(a: &str, b: &str, f: impl FnOnce(&str) -> R) -> R {
    let total = a.len() + b.len();
    if total <= JOIN_STACK_CAP {
        let mut buf = [0u8; JOIN_STACK_CAP];
        buf[..a.len()].copy_from_slice(a.as_bytes());
        buf[a.len()..total].copy_from_slice(b.as_bytes());
        // Concatenating two `&str` always yields valid UTF-8, so this branch is
        // the one that runs. Falling through to the owned path on `Err` rather
        // than unwrapping keeps the function total -- there is no input that can
        // make it panic.
        if let Ok(joined) = core::str::from_utf8(&buf[..total]) {
            return f(joined);
        }
    }
    let mut owned = String::with_capacity(total);
    owned.push_str(a);
    owned.push_str(b);
    f(&owned)
}

/// Register the leaf component of a group path in the `by_leaf` index.
///
/// Extracts the trailing class name after the last `.` in the path. If another
/// group already claimed this leaf, marks it as ambiguous.
pub(crate) fn register_leaf(by_leaf: &mut FxHashMap<String, usize>, path: &str, idx: usize) {
    // Extract the leaf: the part after the last '.' in the path.
    let leaf = match path.rfind('.') {
        Some(dot_pos) => &path[dot_pos + 1..],
        None => return, // No dot separator -> not a qualified path, skip.
    };
    if leaf.is_empty() {
        return;
    }
    by_leaf
        .entry(leaf.to_owned())
        .and_modify(|existing| {
            if *existing != idx {
                *existing = NetGuidCache::AMBIGUOUS_LEAF;
            }
        })
        .or_insert(idx);
}

impl NetGuidCache {
    /// Resolve a bare class name to its export group using leaf-suffix matching.
    ///
    /// Mirrors the C# `ContentBlockPathResolver.UniqueLeafMatch`: when a path
    /// has no separators (is a bare name like `AresAttributeSet`), look for a
    /// registered group whose path ends with `.{name}`. Returns the canonical
    /// group only if exactly one such group exists (ambiguous leaves return None).
    ///
    /// Beyond C#'s exact-match logic, also tries common Unreal suffixes:
    /// - `name + "Component"` -- subobject GUIDs often omit the `Component`
    ///   suffix that their export group path includes (e.g. GUID path
    ///   `EquippableStateMachine` -> group `.EquippableStateMachineComponent`).
    /// - `name + "_C"` -- Blueprint-class GUIDs (especially `Comp_*` prefixed)
    ///   map to groups with a `_C` suffix on the leaf.
    ///
    /// This is the bridge between NetGUID paths (often bare class names) and
    /// fully-qualified export group paths.
    #[must_use]
    pub fn unique_leaf_match(&self, bare_name: &str) -> Option<&NetFieldExportGroup> {
        // Only apply to bare names (no path separators).
        if has_path_separator(bare_name) {
            return None;
        }
        // Exact leaf, then the two suffixed forms above. The first one claimed
        // at all decides, so an ambiguous exact leaf yields None rather than a
        // suffixed group of another class.
        self.leaf_claim(bare_name)
            .or_else(|| with_joined(bare_name, "Component", |k| self.leaf_claim(k)))
            .or_else(|| with_joined(bare_name, "_C", |k| self.leaf_claim(k)))
            .flatten()
    }

    /// Resolve a bare instance name to a `_ClassNetCache` export group.
    ///
    /// This bridges the gap between actor/subobject instance names (e.g.
    /// `BombDestination_A`, `ForceModuleManager`, `AudDeadeyeVOComponent`) and
    /// their ClassNetCache groups in the replay schema. The replay declares
    /// groups like `BombDestination_C_ClassNetCache` or
    /// `ForceModuleManagerComponent_ClassNetCache` but the wire only gives us
    /// the bare instance name.
    ///
    /// # Strategy
    ///
    /// For each candidate stem (starting with the full name, then progressively
    /// stripping the last `_SEGMENT`), try these leaf lookups in `by_leaf`:
    ///
    /// 1. `stem_ClassNetCache` (exact class, e.g. `AresAbilitySystem` matches
    ///    `AresAbilitySystemComponent_ClassNetCache` via step 2)
    /// 2. `stemComponent_ClassNetCache` (Unreal components often drop the suffix
    ///    in instance names)
    /// 3. `stem_C_ClassNetCache` (Blueprint classes use `_C` to denote the
    ///    compiled class)
    ///
    /// The capacity comes from the matched group's declared
    /// `NetFieldExportsLength` (never guessed). Only unambiguous matches (one
    /// group per leaf) are accepted, and an ambiguous candidate ends the search
    /// instead of yielding to a later suffix or a shorter stem.
    ///
    /// # Why instance-suffix stripping is needed
    ///
    /// Unreal appends instance identifiers to actor names: `BombDestination_A`,
    /// `WindowShieldA1`, `RespawningWallPlate_2`, `AmbientAudio_Ascent_*`. The
    /// class name is the stem before the instance suffix. Stripping one
    /// underscore segment at a time from the right correctly recovers the class
    /// for all observed patterns without hardcoding any actor or map name.
    #[must_use]
    pub fn resolve_cnc_for_instance_name(&self, bare_name: &str) -> Option<&NetFieldExportGroup> {
        // Only bare names (no path separators).
        if has_path_separator(bare_name) {
            return None;
        }

        // Try with the full name first, then progressively shorter stems.
        let mut stem = bare_name;
        loop {
            if let Some(claim) = self.try_cnc_leaf_candidates(stem) {
                return claim;
            }

            // Strip the last underscore-delimited segment to get a shorter stem.
            // e.g. "BombDestination_A" -> "BombDestination"
            //      "AmbientAudio_Ascent_Defender_SoundA_003" -> ...
            match stem.rfind('_') {
                Some(pos) if pos > 0 => {
                    stem = &bare_name[..pos];
                }
                _ => break,
            }
        }

        // Final fallback: strip trailing digits (handles WindowShieldA1 ->
        // WindowShield, MeleeAttackState1 -> MeleeAttackState). Only attempt
        // this if the name does NOT end with an underscore-separated segment
        // (those were already tried above).
        let trimmed = bare_name.trim_end_matches(|c: char| c.is_ascii_digit());
        if trimmed.len() < bare_name.len() && !trimmed.is_empty() {
            // Longest stem first, matching the loop above: try the
            // digit-only trim before the more aggressive one below.
            if let Some(claim) = self.try_cnc_leaf_candidates(trimmed) {
                return claim;
            }
            // Also strip a trailing uppercase letter -- one, not the whole
            // run -- that acts as a site/variant marker (e.g. WindowShieldA1
            // -> WindowShieldA -> WindowShield).
            if trimmed
                .chars()
                .next_back()
                .is_some_and(|c| c.is_ascii_uppercase())
            {
                let trimmed2 = &trimmed[..trimmed.len() - 1];
                if !trimmed2.is_empty() {
                    if let Some(claim) = self.try_cnc_leaf_candidates(trimmed2) {
                        return claim;
                    }
                }
            }
        }

        None
    }

    /// Look up one `by_leaf` key: `None` when no group claims the leaf,
    /// `Some(None)` when two or more do, `Some(Some(group))` for exactly one.
    ///
    /// `usize::MAX` in `by_leaf` means two or more groups share this leaf; the
    /// C# `UniqueLeafMatch` contract is that such a name binds to nothing rather
    /// than to whichever group happened to register first. Keeping "ambiguous"
    /// apart from "absent" is what lets the resolvers stop at it instead of
    /// trying their next candidate.
    #[inline]
    fn leaf_claim(&self, leaf: &str) -> Option<Option<&NetFieldExportGroup>> {
        match *self.leaf_index().get(leaf)? {
            Self::AMBIGUOUS_LEAF => Some(None),
            idx => self.groups().get(idx).map(Some),
        }
    }

    /// Try ClassNetCache leaf candidates for a given stem.
    ///
    /// Checks `stem_ClassNetCache`, `stemComponent_ClassNetCache`, and
    /// `stem_C_ClassNetCache` in the leaf index and returns the first claim,
    /// `Some(None)` when that candidate is ambiguous; `None` when none is
    /// claimed.
    fn try_cnc_leaf_candidates(&self, stem: &str) -> Option<Option<&NetFieldExportGroup>> {
        // The three suffixes are tried in this order and the first claimed
        // candidate decides, so a stem that could match under more than one
        // convention resolves the same way it always has, and an ambiguous
        // one resolves to nothing.
        for suffix in [
            "_ClassNetCache",
            "Component_ClassNetCache",
            "_C_ClassNetCache",
        ] {
            if let Some(claim) = with_joined(stem, suffix, |k| self.lookup_cnc_leaf(k)) {
                return Some(claim);
            }
        }
        None
    }

    /// Look up a CNC leaf in `by_leaf`, with [`Self::leaf_claim`]'s three
    /// outcomes.
    ///
    /// The `_ClassNetCache` suffix test below is a belt-and-braces assertion,
    /// not an active filter: every caller reaches this with a leaf that was
    /// itself derived from a `_ClassNetCache` path, so it does not fail on any
    /// input the callers can produce. It is kept because the invariant is a
    /// property of the CALLERS, and a future caller that does not hold it
    /// should get no claim rather than a wrongly-typed group. An ambiguous
    /// claim passes it: every group sharing a leaf that ends in the suffix
    /// ends in it too.
    fn lookup_cnc_leaf(&self, leaf: &str) -> Option<Option<&NetFieldExportGroup>> {
        self.leaf_claim(leaf).filter(|claim| {
            claim.is_none_or(|group| group.path.ends_with(crate::path::CLASS_NET_CACHE_SUFFIX))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export::NetFieldExportGroup;

    /// `(registered paths, bare name, the path it must resolve to)`.
    type Row<'a> = (&'a [&'a str], &'a str, Option<&'a str>);

    /// A cache holding one group per path, at indices 0, 1, 2, ... A failed
    /// registration fails the test rather than leaving a row that resolves to
    /// nothing for the wrong reason.
    fn cache_of(paths: &[&str]) -> NetGuidCache {
        let mut cache = NetGuidCache::new();
        for (index, path) in paths.iter().enumerate() {
            cache
                .add_export_group(NetFieldExportGroup::new((*path).into(), index as u32, 1))
                .unwrap();
        }
        cache
    }

    #[test]
    fn unique_leaf_match_resolves_each_row() {
        const FLOAT_CURVE: &str = "/Game/Characters/Components/Comp_Projectile_FloatCurveMovement.Comp_Projectile_FloatCurveMovement_C";
        let ambiguous_then_component = ["/Script/A.Foo", "/Script/B.Foo", "/Script/C.FooComponent"];
        let ambiguous_then_c = [
            "/Script/A.FooComponent",
            "/Script/B.FooComponent",
            "/Game/C.Foo_C",
        ];
        let separators = ["/Script/X.Game:Test", "/Script/X.Game/Test"];
        let rows: &[Row<'_>] = &[
            (
                &["/Script/ShooterGame.AresAttributeSet"],
                "AresAttributeSet",
                Some("/Script/ShooterGame.AresAttributeSet"),
            ),
            // Subobject GUIDs often omit the `Component` their group has.
            (
                &["/Script/ShooterGame.EquippableStateMachineComponent"],
                "EquippableStateMachine",
                Some("/Script/ShooterGame.EquippableStateMachineComponent"),
            ),
            // Blueprint-class GUIDs map to a leaf with a `_C` suffix.
            (
                &[FLOAT_CURVE],
                "Comp_Projectile_FloatCurveMovement",
                Some(FLOAT_CURVE),
            ),
            (
                &["/Script/ShooterGame.SomethingElse"],
                "NonexistentThing",
                None,
            ),
            // Two groups share the leaf, so neither name binds, the second
            // through `+Component` reaching the same ambiguous leaf.
            (
                &["/Script/A.TestComponent", "/Script/B.TestComponent"],
                "TestComponent",
                None,
            ),
            (
                &["/Script/A.TestComponent", "/Script/B.TestComponent"],
                "Test",
                None,
            ),
            // The first candidate claimed at all decides: an ambiguous exact
            // leaf does not fall through to `+Component`, nor an ambiguous
            // `+Component` to `+_C`. The later group of another class is
            // reachable by its own leaf.
            (&ambiguous_then_component, "Foo", None),
            (
                &ambiguous_then_component,
                "FooComponent",
                Some("/Script/C.FooComponent"),
            ),
            (&ambiguous_then_c, "Foo", None),
            (&ambiguous_then_c, "Foo_C", Some("/Game/C.Foo_C")),
            // A qualified name is never leaf-matched, even when a leaf is
            // spelled exactly like it.
            (&separators, "Game:Test", None),
            (&separators, "Game/Test", None),
        ];
        for &(paths, name, expected) in rows {
            assert_eq!(
                cache_of(paths)
                    .unique_leaf_match(name)
                    .map(|g| g.path.as_str()),
                expected,
                "{name} over {paths:?}"
            );
        }
    }

    #[test]
    fn resolve_cnc_for_instance_name_resolves_each_row() {
        const DEADEYE_VO_C: &str =
            "/Game/Audio/VOComponent/AudDeadeyeVoComponent.AudDeadeyeVOComponent_C_ClassNetCache";
        const DEADEYE: &str = "/Game/Audio/VOComponent/AudDeadeye.AudDeadeye_ClassNetCache";
        const DEADEYE_VO: &str =
            "/Game/Audio/VOComponent/AudDeadeyeVoComponent.AudDeadeyeVOComponent_ClassNetCache";
        const BOMB: &str = "/Game/GameModes/Bomb/BombDestination.BombDestination_C_ClassNetCache";
        const GRENADE: &str =
            "/Game/Abilities/GrenadeExplodeIndicator.GrenadeExplodeIndicator_C_ClassNetCache";
        const SWITCH: &str = "/Game/Maps/Switch_BlackMarket_2.Switch_BlackMarket_2_C_ClassNetCache";
        const X_C: &str = "/Game/X/X.X_C_ClassNetCache";
        const X_COMPONENT: &str = "/Script/ShooterGame.XComponent_ClassNetCache";
        const X_PLAIN: &str = "/Script/ShooterGame.X_ClassNetCache";
        let next_suffix = [
            "/Script/A.Foo_ClassNetCache",
            "/Script/B.Foo_ClassNetCache",
            "/Script/C.FooComponent_ClassNetCache",
        ];
        let shorter_stem = [
            "/Game/P1/Foo_Bar.Foo_Bar_C_ClassNetCache",
            "/Game/P2/Foo_Bar.Foo_Bar_C_ClassNetCache",
            "/Game/P3/Foo.Foo_C_ClassNetCache",
        ];
        let uppercase_trim = [
            "/Game/A/FooA.FooA_C_ClassNetCache",
            "/Game/B/FooA.FooA_C_ClassNetCache",
            "/Game/C/Foo.Foo_C_ClassNetCache",
        ];
        let separators = [
            "/Script/X.Foo_ClassNetCache",
            "/Script/X.Game:Foo_ClassNetCache",
        ];
        let rows: &[Row<'_>] = &[
            // The three suffixes: `_ClassNetCache`, `Component_ClassNetCache`,
            // `_C_ClassNetCache`.
            (
                &["/Script/ShooterGame.AresWorldSettings_ClassNetCache"],
                "AresWorldSettings",
                Some("/Script/ShooterGame.AresWorldSettings_ClassNetCache"),
            ),
            (
                &["/Script/ShooterGame.ForceModuleManagerComponent_ClassNetCache"],
                "ForceModuleManager",
                Some("/Script/ShooterGame.ForceModuleManagerComponent_ClassNetCache"),
            ),
            (&[DEADEYE_VO_C], "AudDeadeyeVOComponent", Some(DEADEYE_VO_C)),
            // A stem declared under more than one convention resolves by that
            // suffix order. The groups are registered in the reverse order, so
            // registration order cannot produce the same answers.
            (&[X_C, X_COMPONENT, X_PLAIN], "X", Some(X_PLAIN)),
            (&[X_C, X_COMPONENT], "X", Some(X_COMPONENT)),
            // Instance suffixes are stripped one `_` segment at a time.
            (&[BOMB], "BombDestination_A", Some(BOMB)),
            (&[BOMB], "BombDestination_B", Some(BOMB)),
            (
                &["/Game/Audio/Core/AmbientAudio.AmbientAudio_C_ClassNetCache"],
                "AmbientAudio_Ascent_Defender_SoundA_003",
                Some("/Game/Audio/Core/AmbientAudio.AmbientAudio_C_ClassNetCache"),
            ),
            (
                &["/Script/ShooterGame.MeleeAttackStateComponent_ClassNetCache"],
                "MeleeAttackState_Alt",
                Some("/Script/ShooterGame.MeleeAttackStateComponent_ClassNetCache"),
            ),
            (&[GRENADE], "GrenadeExplodeIndicator_Bounce", Some(GRENADE)),
            // The full name is tried first and matches without stripping.
            (&[SWITCH], "Switch_BlackMarket_2", Some(SWITCH)),
            // Trailing digits, then one trailing uppercase letter.
            (
                &["/Game/Interactable/WindowShield.WindowShield_C_ClassNetCache"],
                "WindowShieldA1",
                Some("/Game/Interactable/WindowShield.WindowShield_C_ClassNetCache"),
            ),
            // Both are real archive names. The digit-only trim is tried before
            // the uppercase trim (shorter-first matched the unrelated
            // `AudDeadeye` group), and the uppercase trim removes one letter:
            // `AudDeadeyeVOB` loses the `B`, not the whole `VOB` run.
            (&[DEADEYE, DEADEYE_VO], "AudDeadeyeVO2", Some(DEADEYE_VO)),
            (&[DEADEYE, DEADEYE_VO], "AudDeadeyeVOB1", Some(DEADEYE_VO)),
            (
                &["/Script/ShooterGame.SomethingUnrelated_ClassNetCache"],
                "AbilitiesAndBuffsComponent",
                None,
            ),
            (
                &[
                    "/Script/A.SharedName_ClassNetCache",
                    "/Script/B.SharedName_ClassNetCache",
                ],
                "SharedName",
                None,
            ),
            // An ambiguous candidate ends the search wherever the resolver
            // would move on: at the next suffix, the next shorter stem, and the
            // uppercase trim after the digit trim. Each later group is
            // reachable when nothing ambiguous comes first.
            (&next_suffix, "Foo", None),
            (&next_suffix, "FooComponent", Some(next_suffix[2])),
            (&shorter_stem, "Foo_Bar", None),
            (&shorter_stem, "Foo_Baz", Some(shorter_stem[2])),
            (&uppercase_trim, "FooA1", None),
            (&uppercase_trim, "Foo1", Some(uppercase_trim[2])),
            // A qualified name is never resolved, even where stripping a `_`
            // segment or matching a leaf spelled like it would succeed.
            (&separators, "Foo_Bar.Baz", None),
            (&separators, "Foo_Bar/Baz", None),
            (&separators, "Game:Foo", None),
        ];
        for &(paths, name, expected) in rows {
            assert_eq!(
                cache_of(paths)
                    .resolve_cnc_for_instance_name(name)
                    .map(|g| g.path.as_str()),
                expected,
                "{name} over {paths:?}"
            );
        }
    }
}
