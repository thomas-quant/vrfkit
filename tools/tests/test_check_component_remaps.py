"""Guards for the component-remap check.

The remap table in `sink/paths.rs` was read out of a shipped build, and a later
build can rename a component without anything in the repo noticing: the replay
never named it either, so there is no test that can fail. The export baseline
pins `overlay_no_field_name` and would catch it -- but only on the one replay
that has a baseline.

What the checker does instead works on any export: for each remap pair, ask
whether the native group it targets carries rows. If it does not, and the bare
leaf still does, the remap stopped matching.
"""
import collections
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import check_component_remaps as guard  # noqa: E402

SCRIPT = Path(__file__).resolve().parents[1] / "check_component_remaps.py"

PAIRS = [("ZoomStateMachine", "/Script/ShooterGame.EquippableStateMachineComponent")]


class VerdictTests(unittest.TestCase):
    def test_rows_on_the_native_group_mean_the_remap_works(self):
        v = guard.verdicts(PAIRS, {"/Script/ShooterGame.EquippableStateMachineComponent": 8112})
        self.assertEqual([x.state for x in v], ["ok"])

    def test_a_dead_remap_with_the_leaf_still_present_fails(self):
        """The rename signature: nothing on the target, everything still bare."""
        v = guard.verdicts(PAIRS, {"ZoomStateMachine": 8112})
        self.assertEqual([x.state for x in v], ["broken"])
        self.assertIn("8112", v[0].detail)

    def test_neither_present_is_absent_not_broken(self):
        """A replay without that component is not evidence of anything."""
        v = guard.verdicts(PAIRS, {"/Script/Other.Thing": 5})
        self.assertEqual([x.state for x in v], ["absent"])

    def test_a_leaf_lingering_beside_a_working_target_is_still_ok(self):
        """The ratio path, which only a pair with no RepLayout kind still takes.

        On the reference replay `ZoomStateMachine` drops from 8,112 rows to 70
        rather than to zero -- 0.12% of 60,101. Those 70 turned out to be
        ClassNetCache rows, which `bare_counts` now drops, so a RepLayout pair
        no longer needs this tolerance; see `StrictRepLayoutTests`.
        """
        v = guard.verdicts(PAIRS, {
            "/Script/ShooterGame.EquippableStateMachineComponent": 60101,
            "ZoomStateMachine": 70,
        })
        self.assertEqual([x.state for x in v], ["ok"])

    def test_a_dead_leaf_sharing_a_live_target_is_caught(self):
        """The case that a target-only check misses.

        Many leaves map to `EquippableStateMachineComponent` (nine when this
        was written, 26 now). If one stops resolving, the others keep the target
        busy, so "does the target have rows" says everything is fine while that
        component's blocks are all bare again. Measured by simulating the
        rename: the leaf goes to 15.58% of the target's rows, against 0.12% when
        it is healthy -- which the ratio catches; see `StrictRepLayoutTests`
        for the smaller shares it did not.
        """
        v = guard.verdicts(PAIRS, {
            "/Script/ShooterGame.EquippableStateMachineComponent": 52059,
            "ZoomStateMachine": 8112,
        })
        self.assertEqual([x.state for x in v], ["broken"])

    def test_class_net_cache_rows_do_not_count_as_bare(self):
        """The RepLayout-only remaps leave their RPC stream bare on purpose.

        `AbilitiesAndBuffsComponent` is mapped for RepLayout blocks and
        deliberately not for ClassNetCache ones -- the AbilitySystem `_cnc`
        group declares an incomplete function table, so remapping the RPC stream
        would mis-parse it. On the reference replay that leaves 9,366 bare rows
        against 652 on the native group, every one of them a `_cnc_h*` or
        unresolved-payload row. Counting those would report the healthy case as
        broken, which it did before this was split out.
        """
        counts = guard.bare_counts({
            "AbilitiesAndBuffsComponent": collections.Counter({
                "_cnc_h1": 4683,
                "__vrfkit_unresolved_class_net_cache_payload__": 4683,
            }),
            "ZoomStateMachine": collections.Counter({"CurrentState": 70}),
        })
        self.assertEqual(counts["AbilitiesAndBuffsComponent"], 0)
        self.assertEqual(counts["ZoomStateMachine"], 70)

    def test_exit_code_is_nonzero_only_when_something_is_broken(self):
        ok = guard.verdicts(PAIRS, {"/Script/ShooterGame.EquippableStateMachineComponent": 1})
        broken = guard.verdicts(PAIRS, {"ZoomStateMachine": 1})
        absent = guard.verdicts(PAIRS, {})
        self.assertEqual(guard.exit_code(ok), 0)
        self.assertEqual(guard.exit_code(absent), 0)
        self.assertEqual(guard.exit_code(broken), 1)


class StrictRepLayoutTests(unittest.TestCase):
    """A RepLayout pair is broken by any RepLayout row left bare.

    Healthy is exactly zero -- 92 exports, 48 RepLayout pairs, not one row --
    and the ratio could not see a dead leaf behind a busy shared target.
    """

    NATIVE = "/Script/ShooterGame.EquippableStateMachineComponent"
    KINDS = {"ZoomStateMachine": "RepLayout"}

    def test_nothing_bare_is_ok(self):
        v = guard.verdicts(PAIRS, {self.NATIVE: 60101}, self.KINDS)
        self.assertEqual([x.state for x in v], ["ok"])

    def test_one_bare_row_is_broken(self):
        v = guard.verdicts(PAIRS, {self.NATIVE: 60101, "ZoomStateMachine": 1}, self.KINDS)
        self.assertEqual([x.state for x in v], ["broken"])

    def test_a_dead_leaf_behind_a_busy_shared_target_is_caught(self):
        """The case the ratio missed: 2,495 bare rows against 62,000 is 4%.

        Measured shape, from a 13.06 export made before `Resume_StateMachine`
        was mapped: every one of its RepLayout rows was bare, and the ratio
        called it `ok` because 25 other leaves keep the target busy.
        """
        rows = {self.NATIVE: 62000, "ZoomStateMachine": 2495}
        self.assertEqual([x.state for x in guard.verdicts(PAIRS, rows)], ["ok"])
        self.assertEqual(
            [x.state for x in guard.verdicts(PAIRS, rows, self.KINDS)], ["broken"])

    def test_a_class_net_cache_pair_is_not_judged_on_rep_layout_rows(self):
        """The shape the ratio used to call `ok`: 70 bare RepLayout rows beside
        a busy RepLayout target. Neither is what a ClassNetCache pair routes,
        so with no ClassNetCache row either side there is nothing to judge."""
        rows = {self.NATIVE: 60101, "ZoomStateMachine": 70}
        v = guard.verdicts(PAIRS, rows, {"ZoomStateMachine": "ClassNetCache"})
        self.assertEqual([x.state for x in v], ["absent"])
        self.assertIn("70 RepLayout rows under the leaf", v[0].detail)

    def test_every_real_pair_has_a_kind(self):
        kinds = guard.remap_kinds()
        self.assertEqual(set(kinds), {leaf for leaf, _ in guard.remap_pairs()})
        self.assertEqual(set(kinds.values()), {"RepLayout", "ClassNetCache"})
        self.assertEqual(kinds["ZoomStateMachine"], "RepLayout")
        self.assertEqual(kinds["EffectManager"], "ClassNetCache")


class ClassNetCachePairTests(unittest.TestCase):
    """A ClassNetCache pair is judged on the rows it routes, strictly.

    The four C# reference pairs kept the 5% ratio over RepLayout rows -- the
    leaf's against the class's RepLayout group -- and neither is what they
    remap. Over the 1,018 exports of the 2026-09-28 audit that read
    `DamageHandlerComponent` `broken` on 10 healthy exports (one or two stray
    RepLayout rows, 0 on `DamageableComponent`) and `absent` on the other
    1,008, although `DamageableComponent_ClassNetCache` held rows on all
    1,018 (72.2M in all); and a scratch build with the pair's target renamed
    printed byte-identical output.
    The row counts below are those exports'.
    """

    PAIR = [("DamageHandlerComponent", "/Script/ShooterGame.DamageableComponent")]
    KINDS = {"DamageHandlerComponent": "ClassNetCache"}
    ROUTED = "/Script/ShooterGame.DamageableComponent_ClassNetCache"

    def verdict(self, rows, cnc_bare=None):
        return guard.verdicts(self.PAIR, rows, self.KINDS, cnc_bare)[0]

    def test_routed_rows_and_nothing_bare_is_ok(self):
        v = self.verdict({self.ROUTED: 89843})
        self.assertEqual(v.state, "ok")
        self.assertIn("89843 rows on the _ClassNetCache group", v.detail)

    def test_a_stray_rep_layout_row_under_the_leaf_is_reported_not_broken(self):
        """The healthy 13.05 export the ratio failed: one 1-bit RepLayout row
        under the leaf, 0 on the RepLayout group, 89,843 routed."""
        rows = {"DamageHandlerComponent": 1, self.ROUTED: 89843}
        v = self.verdict(rows)
        self.assertEqual(v.state, "ok")
        self.assertIn("1 RepLayout rows under the leaf", v.detail)
        self.assertEqual(guard.exit_code([v]), 0)

    def test_a_remap_that_did_not_fire_is_broken(self):
        """The same replay exported by the scratch build: 0 routed, the one
        RepLayout row unchanged, 4,563 whole payloads bare under the leaf."""
        v = self.verdict({"DamageHandlerComponent": 1},
                         {"DamageHandlerComponent": 4563})
        self.assertEqual(v.state, "broken")
        self.assertIn("4563 ClassNetCache rows still bare", v.detail)

    def test_rows_on_the_target_do_not_excuse_bare_ones(self):
        """The scratch build's 13.01 export: 78 rows still reached the class
        group without the table, beside 5,247 payloads left bare. Bare is
        checked first, so the 78 cannot read as the remap working."""
        v = self.verdict({self.ROUTED: 78}, {"DamageHandlerComponent": 5247})
        self.assertEqual(v.state, "broken")

    def test_one_bare_payload_beside_a_busy_target_is_broken(self):
        """Strict, as for a RepLayout pair: healthy is exactly zero -- 0 bare
        ClassNetCache rows in all 4,072 (pair, export) cases of the audit --
        so no share of a busy target may hide one."""
        v = self.verdict({self.ROUTED: 89843}, {"DamageHandlerComponent": 1})
        self.assertEqual(v.state, "broken")

    def test_the_healthy_and_the_broken_export_now_read_differently(self):
        """What the old verdict could not do. It saw only RepLayout rows, which
        are the same on both exports, so it returned one state for both."""
        healthy_rows = {"DamageHandlerComponent": 1, self.ROUTED: 89843}
        broken_rows = {"DamageHandlerComponent": 1}
        old = [guard.verdicts(self.PAIR, rows)[0].state
               for rows in (healthy_rows, broken_rows)]
        self.assertEqual(old, ["broken", "broken"])
        new = [self.verdict(healthy_rows).state,
               self.verdict(broken_rows, {"DamageHandlerComponent": 4563}).state]
        self.assertEqual(new, ["ok", "broken"])

    def test_the_rep_layout_target_group_says_nothing(self):
        """`EffectManager` read `ok` on every export because its RepLayout group
        happens to carry rows -- not rows this pair routes."""
        v = self.verdict({"/Script/ShooterGame.DamageableComponent": 5000})
        self.assertEqual(v.state, "absent")

    def test_neither_routed_nor_bare_is_absent(self):
        v = self.verdict({"DamageHandlerComponent": 2})
        self.assertEqual(v.state, "absent")
        self.assertIn("2 RepLayout rows under the leaf", v.detail)

    def test_the_two_bare_counts_split_every_row_of_a_bare_group(self):
        names = {"DamageHandlerComponent": collections.Counter({
            "__vrfkit_unresolved_class_net_cache_payload__": 4563, None: 1}),
            "AbilitiesAndBuffsComponent": collections.Counter({
                "_cnc_h1": 4683, "Status": 3})}
        rep_layout = guard.bare_counts(names)
        cnc = guard.cnc_bare_counts(names)
        self.assertEqual(rep_layout, {"DamageHandlerComponent": 1,
                                      "AbilitiesAndBuffsComponent": 3})
        self.assertEqual(cnc, {"DamageHandlerComponent": 4563,
                               "AbilitiesAndBuffsComponent": 4683})

    def test_the_rep_layout_tally_counts_only_class_net_cache_leaves(self):
        rows = {"DamageHandlerComponent": 2, "EffectManager": 3,
                "ZoomStateMachine": 70}
        kinds = {"DamageHandlerComponent": "ClassNetCache",
                 "EffectManager": "ClassNetCache", "ZoomStateMachine": "RepLayout"}
        self.assertEqual(guard.class_net_cache_leaf_rep_layout_rows(rows, kinds), 5)


class RenameSignalTests(unittest.TestCase):
    """What the ratio verdicts cannot see, and where it does show up.

    The FAILED text told the reader "the likely cause is a game build renaming
    the component". A rename cannot produce that verdict. When a build renames
    `ZoomStateMachine` to something else the replay stops declaring the old
    leaf at all, so `bare_rows` is 0 -- under the strict rule and the ratio
    alike -- and the pair reads `ok` whenever another leaf keeps the target
    group busy, or `absent` when none does. Never `broken`.

    The renamed component does not vanish from the export, though. It arrives
    as a bare group under its NEW name, which no pair in the table claims. That
    is the signal, and nothing was looking at it.
    """

    def test_a_renamed_leaf_is_not_broken_by_the_ratio_check(self):
        """States the gap the suspects list exists to cover."""
        v = guard.verdicts(PAIRS, {
            "/Script/ShooterGame.EquippableStateMachineComponent": 52059,
            "ZoomStateMachineV2": 8112,
        })
        self.assertEqual([x.state for x in v], ["ok"])
        self.assertEqual(guard.exit_code(v), 0)

    def test_the_renamed_leaf_surfaces_as_an_unclaimed_bare_group(self):
        suspects = guard.unmapped_bare_groups({
            "/Script/ShooterGame.EquippableStateMachineComponent": 52059,
            "ZoomStateMachineV2": 8112,
        }, PAIRS)
        self.assertEqual(suspects, [("ZoomStateMachineV2", 8112)])

    def test_a_mapped_leaf_is_never_a_suspect(self):
        """A leaf the table already claims is not an unexplained group."""
        self.assertEqual(guard.unmapped_bare_groups({"ZoomStateMachine": 70}, PAIRS), [])

    def test_a_native_group_is_never_a_suspect(self):
        """Only bare Blueprint leaves can be renamed out from under the table."""
        self.assertEqual(
            guard.unmapped_bare_groups(
                {"/Script/ShooterGame.EquippableStateMachineComponent": 52059}, PAIRS),
            [])

    def test_a_class_net_cache_only_group_is_not_a_suspect(self):
        """`row_counts` already zeroes those; a zero must not read as a rename.

        `AbilitiesAndBuffsComponent` is RepLayout-only by design, so its whole
        RPC stream stays bare on a healthy export. It reaches this function
        with a 0 because `bare_counts` dropped the `_cnc_h*` rows, and a
        rename suspect list that reported it would be pure noise.
        """
        self.assertEqual(
            guard.unmapped_bare_groups({"AbilitiesAndBuffsComponent": 0}, PAIRS), [])

    def test_suspects_come_back_worst_first(self):
        suspects = guard.unmapped_bare_groups(
            {"Small": 3, "Large": 900, "Middle": 40}, PAIRS)
        self.assertEqual([g for g, _ in suspects], ["Large", "Middle", "Small"])


class NothingCheckedTests(unittest.TestCase):
    """A run in which no pair appeared verified nothing, and must not say OK."""

    def test_every_pair_absent_means_nothing_was_checked(self):
        self.assertTrue(guard.nothing_checked(guard.verdicts(PAIRS, {})))

    def test_one_working_pair_is_enough_to_have_checked_something(self):
        v = guard.verdicts(PAIRS, {
            "/Script/ShooterGame.EquippableStateMachineComponent": 1})
        self.assertFalse(guard.nothing_checked(v))

    def test_a_broken_pair_also_counts_as_having_checked_something(self):
        self.assertFalse(guard.nothing_checked(
            guard.verdicts(PAIRS, {"ZoomStateMachine": 1})))


class MainTests(unittest.TestCase):
    """The vacuity lives in `main`, so it is exercised there."""

    def _export(self, directory, rows):
        import pyarrow as pa
        import pyarrow.parquet as pq
        table = pa.table({
            "group_path": [g for g, _ in rows],
            "field_name": [f for _, f in rows],
        })
        pq.write_table(table, Path(directory) / "fields.parquet")
        return Path(directory)

    def _run(self, directory):
        result = subprocess.run(
            [sys.executable, str(SCRIPT), "--export", str(directory)],
            capture_output=True, text=True, check=False)
        return result

    def test_an_export_with_no_remap_at_all_does_not_report_OK(self):
        """Not the same as having no export: this one ran and learned nothing."""
        with tempfile.TemporaryDirectory() as directory:
            self._export(directory, [("/Script/Other.Thing", "X")] * 5)
            result = self._run(directory)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertNotIn("OK:", result.stdout)

    def test_a_healthy_export_still_passes_and_says_so(self):
        with tempfile.TemporaryDirectory() as directory:
            native = "/Script/ShooterGame.EquippableStateMachineComponent"
            self._export(directory, [(native, "CurrentState")] * 100)
            result = self._run(directory)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("OK:", result.stdout)
        # Printed with its zero: a line that appears only when nonzero could
        # not tell "none" from "never counted".
        self.assertIn("0 RepLayout row(s) under the leaves of the ClassNetCache "
                      "pairs", result.stdout)

    def test_main_reads_a_stray_rep_layout_row_under_a_class_net_cache_leaf_as_ok(self):
        """The 10 healthy audit exports the old ratio failed, in miniature."""
        with tempfile.TemporaryDirectory() as directory:
            routed = "/Script/ShooterGame.DamageableComponent_ClassNetCache"
            self._export(directory,
                         [(routed, "MulticastNotifyHeal.HealTaken")] * 100
                         + [("DamageHandlerComponent", None)])
            result = self._run(directory)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("1 RepLayout row(s) under the leaves of the ClassNetCache "
                      "pairs", result.stdout)

    def test_main_fails_a_class_net_cache_remap_that_did_not_fire(self):
        """`main` must hand the leaf's ClassNetCache rows to `verdicts`: the
        scratch-build shape, whole payloads bare and nothing routed."""
        with tempfile.TemporaryDirectory() as directory:
            native = "/Script/ShooterGame.EquippableStateMachineComponent"
            self._export(directory,
                         [(native, "CurrentState")] * 100
                         + [("DamageHandlerComponent",
                             "__vrfkit_unresolved_class_net_cache_payload__")] * 50
                         + [("DamageHandlerComponent", None)])
            result = self._run(directory)
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("broken  DamageHandlerComponent", result.stdout)
        self.assertIn("50 ClassNetCache rows still bare", result.stdout)

    def test_main_judges_a_rep_layout_pair_strictly(self):
        """`main` must hand the table's kinds to `verdicts`.

        One bare RepLayout row against 100 on the target is 1% -- `ok` by the
        ratio, `broken` by the rule a RepLayout pair is held to.
        """
        with tempfile.TemporaryDirectory() as directory:
            native = "/Script/ShooterGame.EquippableStateMachineComponent"
            self._export(directory, [(native, "CurrentState")] * 100
                         + [("ZoomStateMachine", None)])
            result = self._run(directory)
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("broken  ZoomStateMachine", result.stdout)

    def test_the_failure_text_no_longer_blames_a_rename(self):
        """A `broken` verdict cannot be produced by a rename; see the class above."""
        with tempfile.TemporaryDirectory() as directory:
            self._export(directory, [("ZoomStateMachine", "CurrentState")] * 100)
            result = self._run(directory)
        self.assertEqual(result.returncode, 1)
        self.assertNotIn("renaming the component", result.stderr)


class PairParsingTests(unittest.TestCase):
    def test_the_real_table_parses(self):
        """Reads sink/paths.rs, so a reformat that breaks it shows up here."""
        pairs = guard.remap_pairs()
        self.assertGreater(len(pairs), 15)
        self.assertIn(
            ("ZoomStateMachine", "/Script/ShooterGame.EquippableStateMachineComponent"),
            pairs,
        )
        self.assertIn(
            ("InventoryComponent", "/Script/ShooterGame.AresInventory"), pairs
        )

    def test_every_target_is_an_absolute_object_path(self):
        """Native `/Script/` classes and, for four pairs, Blueprint classes."""
        for leaf, target in guard.remap_pairs():
            self.assertTrue(target.startswith(("/Script/", "/Game/")), f"{leaf} -> {target}")
            self.assertIn(".", target, f"{leaf} -> {target}")

    def test_a_blueprint_class_target_is_read(self):
        """The shape the `/Script/`-only pattern silently skipped."""
        table = """const KNOWN_SUBOBJECT_CLASS_PATHS: &[(&str, &str, GroupKind)] = &[
    (
        "ZoomStateMachine",
        "/Script/ShooterGame.EquippableStateMachineComponent",
        GroupKind::RepLayout,
    ),
    (
        "Comp_Ability_CooldownComponent1",
        "/Game/Characters/Components/Comp_Ability_CooldownComponent.Comp_Ability_CooldownComponent_C",
        GroupKind::RepLayout,
    ),"""
        pairs = guard.remap_pairs(table)
        self.assertEqual(
            [leaf for leaf, _ in pairs],
            ["ZoomStateMachine", "Comp_Ability_CooldownComponent1"])
        self.assertEqual(guard.unparsed_entries(table, pairs), 0)

    def test_an_entry_the_pattern_cannot_read_is_counted(self):
        """A pair nothing checks must not pass as a table with one fewer pair."""
        table = """    (
        "ZoomStateMachine",
        "/Script/ShooterGame.EquippableStateMachineComponent",
        GroupKind::RepLayout,
    ),
    (
        "RelativeTarget",
        "Script/NoLeadingSlash.Thing",
        GroupKind::RepLayout,
    ),"""
        pairs = guard.remap_pairs(table)
        self.assertEqual(len(pairs), 1)
        self.assertEqual(guard.unparsed_entries(table, pairs), 1)

    def test_the_real_table_parses_completely(self):
        table = guard.table_source()
        self.assertEqual(guard.unparsed_entries(table, guard.remap_pairs(table)), 0)


if __name__ == "__main__":
    unittest.main()
