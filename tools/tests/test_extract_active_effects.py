"""`extract_active_effects.py` must not treat a dormancy close as a despawn.

`actors.parquet` gained a third `event` value when the sink stopped recording
every channel close as `"close"`. A dormant actor has NOT been destroyed -- it
merely stopped replicating, which for a settled smoke or wall is the normal
steady state -- so ending its lifetime there would make persistent effects
vanish early in any reproduction built on this table.

The failure this file guards is the quieter one: `elif ev == "close"` simply
does not match `"dormant"`, so the instance stays pending and falls out of the
loop as open-ended. No row is lost and nothing errors; the output just silently
changes meaning. Counting is what makes that visible.
"""
from __future__ import annotations

import sys
import unittest
from pathlib import Path

import pyarrow as pa
import pyarrow.parquet as pq

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import extract_active_effects as effects  # noqa: E402

CLASS = "/Game/Characters/Pandemic/S0/Ability_Q/GameObject_Pandemic_Q_Smoke.GameObject_X_C"


def _export(tmp: Path, rows: list[tuple[int, str, int]]) -> Path:
    """Write a minimal `actors.parquet` of `(guid, event, time_ms)` rows."""
    n = len(rows)
    table = pa.table({
        "actor_net_guid": pa.array([r[0] for r in rows], pa.int64()),
        "event": pa.array([r[1] for r in rows], pa.string()),
        "time_ms": pa.array([r[2] for r in rows], pa.int64()),
        "class_path": pa.array([CLASS] * n, pa.string()),
        "spawn_x": pa.array([1.0] * n, pa.float64()),
        "spawn_y": pa.array([2.0] * n, pa.float64()),
        "spawn_z": pa.array([3.0] * n, pa.float64()),
    })
    out = tmp / "export"
    out.mkdir(parents=True, exist_ok=True)
    pq.write_table(table, out / "actors.parquet")
    return out


class DormantCloseTests(unittest.TestCase):
    def setUp(self):
        import tempfile
        self._tmp = tempfile.TemporaryDirectory()
        self.tmp = Path(self._tmp.name)
        self.addCleanup(self._tmp.cleanup)

    def test_a_dormant_actor_does_not_end_its_effect_instance(self):
        """Dormancy is not destruction: the instance stays open-ended."""
        rows = effects.build_with_tally(_export(self.tmp, [(7, "open", 100), (7, "dormant", 500)]))[0]
        self.assertEqual(len(rows), 1)
        self.assertIsNone(rows[0]["close_ms"])
        self.assertIsNone(rows[0]["duration_ms"])

    def test_a_dormant_instance_is_counted_rather_than_quietly_open_ended(self):
        """The tally is the whole point -- an unexplained open-ended row and a
        dormancy-ended one are indistinguishable in the table itself."""
        out = _export(self.tmp, [(7, "open", 100), (7, "dormant", 500)])
        self.assertEqual(effects.build_with_tally(out)[1]["went_dormant"], 1)

    def test_a_real_close_still_ends_the_instance(self):
        rows = effects.build_with_tally(_export(self.tmp, [(7, "open", 100), (7, "close", 500)]))[0]
        self.assertEqual((rows[0]["close_ms"], rows[0]["duration_ms"]), (500, 400))
        self.assertEqual(
            effects.build_with_tally(_export(self.tmp, [(7, "open", 100), (7, "close", 500)]))[1]
            ["went_dormant"], 0)

    def test_an_actor_that_wakes_after_dormancy_keeps_one_instance(self):
        """A dormant actor that replicates again was never gone. Two instances
        here would be a false despawn/respawn pair in any reproduction."""
        out = _export(self.tmp, [(7, "open", 100), (7, "dormant", 300), (7, "close", 900)])
        rows, tally = effects.build_with_tally(out)
        self.assertEqual(len(rows), 1)
        self.assertEqual((rows[0]["open_ms"], rows[0]["close_ms"]), (100, 900))
        self.assertEqual(tally["went_dormant"], 1)


GIANTSLAYER = ("/Game/Characters/Deadeye/S0/Ability_X/Gun_Giantslayer/"
               "Gun_Deadeye_X_Giantslayer_Prototype_FIreRatePrototype."
               "Gun_Deadeye_X_Giantslayer_Prototype_FireRatePrototype_C")
BREACH_FLASH = ("/Game/Characters/Breach/S0/Ability_Q/Projectile_Breach_Q_ThroughWalls_Flash."
                "Projectile_Breach_Q_ThroughWalls_Flash_C")
PHOENIX_WALL = ("/Game/Characters/Phoenix/S0/Ability_Q/Production/"
                "Projectile_Phoenix_Q_FlameWall_ThroughWall."
                "Projectile_Phoenix_Q_FlameWall_ThroughWall_C")
ORBITAL_STRIKE = ("/Game/Characters/Sarge/S0/Ability_OrbitalStrike/"
                  "GameObject_Sarge_X_OrbitalStrike_Production."
                  "GameObject_Sarge_X_OrbitalStrike_Production_C")
ULT_ORB = "/Game/GameObjects/CollectibleOrbs/UltPointOrb.UltPointOrb_C"


class ClassifierTests(unittest.TestCase):
    """Substring keywords matched names they were not written for. Measured
    over the 1,018-export audit corpus (259ed10, 2026-09-28)."""

    def test_chambers_ult_gun_is_not_an_effect(self):
        """'fire' inside 'FIreRate': 17,304 of 32,714 damage_zone rows, with
        a median lifetime of ~100 s -- an equippable, not a zone."""
        self.assertFalse(effects.is_effect_class(GIANTSLAYER))

    def test_breachs_flash_through_walls_is_not_a_wall(self):
        """'wall' inside 'ThroughWalls': 1,416 rows filed as walls. A flash
        projectile is not a persistent effect, and no other flash projectile
        is in the table."""
        self.assertFalse(effects.is_effect_class(BREACH_FLASH))

    def test_classify_reads_the_name_the_filter_reads(self):
        """No class in the corpus needs this, but a 'ThroughWall' name with a
        real keyword must be classified by that keyword, the way the filter
        admits it -- not as the wall the filter just refused to see."""
        path = "/Game/Characters/X/S0/Ability_Q/Projectile_X_ThroughWall_Trap.Projectile_X_ThroughWall_Trap_C"
        self.assertTrue(effects.is_effect_class(path))
        self.assertEqual(effects.classify(path), "trap")

    def test_phoenixs_flame_wall_through_wall_is_still_a_wall(self):
        self.assertTrue(effects.is_effect_class(PHOENIX_WALL))
        self.assertEqual(effects.classify(PHOENIX_WALL), "wall")

    def test_brimstones_orbital_strike_is_a_damage_zone_not_an_orb(self):
        """'orb' inside 'OrbitalStrike': 174 rows, 4-9 s of area damage."""
        self.assertTrue(effects.is_effect_class(ORBITAL_STRIKE))
        self.assertEqual(effects.classify(ORBITAL_STRIKE), "damage_zone")

    def test_the_ult_orb_is_still_an_orb(self):
        self.assertTrue(effects.is_effect_class(ULT_ORB))
        self.assertEqual(effects.classify(ULT_ORB), "orb")


class ActorKindTests(unittest.TestCase):
    """The table keeps a projectile and the zone it places as two rows, by
    design; `actor_kind` lets a consumer count one of them."""

    def test_actor_kind_is_the_class_leaf_prefix(self):
        for path, kind in (
                ("/Game/Characters/Wraith/S0/Ability_4/Projectile_Wraith_4_Smoke."
                 "Projectile_Wraith_4_Smoke_C", "projectile"),
                ("/Game/Characters/Wraith/S0/Ability_4/Zone_Wraith_4_Smoke."
                 "Zone_Wraith_4_Smoke_C", "zone"),
                (CLASS, "game_object"),
                ("/Game/Characters/Phoenix/S0/Ability_4/Production/NewMolotov/"
                 "Patch_Phoenix_MolotovFire.Patch_Phoenix_MolotovFire_C", "patch"),
                ("/Game/Characters/Hunter/S0/Ability_E/Drone/Pawn_Hunter_E_Drone."
                 "Pawn_Hunter_E_Drone_C", "pawn"),
                (ULT_ORB, "other")):
            with self.subTest(path=path):
                self.assertEqual(effects.actor_kind(path), kind)

    def test_every_row_carries_its_actor_kind(self):
        import tempfile
        with tempfile.TemporaryDirectory() as temp:
            rows = effects.build_with_tally(_export(Path(temp), [(7, "open", 100), (7, "close", 500)]))[0]
        self.assertEqual(rows[0]["actor_kind"], "game_object")
        self.assertIn("actor_kind", effects.SCHEMA.names)


if __name__ == "__main__":
    unittest.main()
