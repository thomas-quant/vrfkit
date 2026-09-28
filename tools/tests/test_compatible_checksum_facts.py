"""Name-level facts that pin a property's C++ type, recomputed from its checksum.

Every replay field declaration carries a `compatible_checksum`, and Unreal
derives it from the property's lowercase name, its lowercase C++ type and its
static array index -- chained, for a struct member or an array element, from
the checksum of the property that contains it. So a claim like "`EffectID` is
an `int64`" can be checked against the number the replay itself sends: the
type is right if the chain reproduces the checksum and a rival type does not.

The formula is the one the 2026-09-28 game-file analysis measured on 194
Blueprint fields of the 13.06 build (CRC32 over the UTF-32LE lowercase name,
then the lowercase C++ type, then the static index as little-endian u32). The
chains below are small facts about the game's types -- a parent's name and
C++ type -- taken from the 13.06 executable's reflection, read-only. They are
what the comments beside the overlay entries cite; this file is where those
citations can fail.

Each fact also names the `FieldType` the repository gives that checksum in
`crates/vrf-decode/src/checksum_table.rs`, when it gives one there, so a
retyped donor whose checksum table was never regenerated shows up here too.
"""
import struct
import sys
import unittest
import zlib
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import extract_checksum_types as ect  # noqa: E402


def _crc(text: str, seed: int) -> int:
    return zlib.crc32(text.lower().encode("utf-32-le"), seed)


def compatible_checksum(chain) -> int:
    """The checksum of the last `(name, cpp_type)` step, chained from the first.

    `cpp_type` is the spelling `GetCPPType` gives, compared lowercase: `int64`,
    `uint32`, `bool`, `TArray`, `F<Struct>`, `A<Class>*`. An array element is
    its own step, carrying the array's name and the element type.
    """
    checksum = 0
    for name, cpp_type in chain:
        checksum = _crc(name, checksum)
        checksum = _crc(cpp_type, checksum)
        checksum = zlib.crc32(struct.pack("<I", 0), checksum)
    return checksum


#: (replay checksum, chain, rival leaf types that must NOT reproduce it,
#:  the FieldType checksum_table.rs must give it or None when it has no entry,
#:  where the fact is used)
FACTS = [
    (3336285386,
     [("Handle", "FForceModuleHandle"), ("HandleNumber", "uint32")],
     ["int32"], "FieldType::UInt32",
     "NetMulticast{Apply,Remove}ForceModule.HandleNumber"),
    (1457545067,
     [("ModulesCleanedUpByServer", "TArray"),
      ("ModulesCleanedUpByServer", "FForceModuleHandle"),
      ("HandleNumber", "uint32")],
     ["int32"], None,
     "NetMulticastEnforceEndOfLifeCleanup.HandleNumber"),
    (2340855891,
     [("EffectID", "FEffectID"), ("EffectID", "int64")],
     ["uint64"], "FieldType::Int64",
     "EffectManagerComponent:Multicast{Play,Update,Stop}ContinuousEffect.EffectID"),
    (2251343646,
     [("CurrentEffectID", "FEffectID"), ("EffectID", "int64")],
     ["uint64"], "FieldType::Int64",
     "ReplayEffectComponent:ReplayPlayContinuousEffectAtLocation.EffectID"),
    (1129645208,
     [("ServerActiveEffects", "TArray"), ("ServerActiveEffects", "FActiveEffectInfo"),
      ("EffectID", "FEffectID"), ("EffectID", "int64")],
     ["uint64"], "FieldType::Int64",
     "EffectManagerComponent.EffectID"),
    (3321413110,
     [("AuthBlindManagerState", "FBlindManagerState"), ("ActiveBlinds", "TArray"),
      ("ActiveBlinds", "FActiveBlind"), ("BlindEffectID", "FEffectID"),
      ("EffectID", "int64")],
     ["uint64"], None,
     "BlindManagerComponent ActiveBlinds[].EffectID (sink/blobs.rs)"),
    # Cypher's trapwire, before and after the 13.01 rename: the wire's
    # `PairedWire` parameter names its own class, so its checksum moves with
    # the class name -- the rename proven in the type system itself.
    (3902815170, [("Deployed", "bool")], ["uint8"], "FieldType::Bool",
     "GameObject_Gumshoe_{E,4}_TripWire(_SecondWire)_C.Deployed"),
    (3671888355, [("PairedWire", "AGameObject_Gumshoe_E_TripWire_C*")],
     ["AGameObject_Gumshoe_4_TripWire_C*"], None,
     "GameObject_Gumshoe_E_TripWire_C:SetEnemyInTrap.PairedWire (11.06-12.08)"),
    (3454621121, [("PairedWire", "AGameObject_Gumshoe_4_TripWire_C*")],
     ["AGameObject_Gumshoe_E_TripWire_C*"], None,
     "GameObject_Gumshoe_4_TripWire_C:SetEnemyInTrap.PairedWire (13.01-13.06)"),
    (2035145197, [("CreatedByCharacter", "AShooterCharacter*")], ["UShooterCharacter*"], None,
     "Ability_Gumshoe_{E_TripWire,4_TripWire,4_CageTrap,Q_CageTrap}_C.CreatedByCharacter"),
]

#: The C++ leaf type each checksum-table FieldType above stands for.
FIELD_TYPE_OF = {
    "int64": "FieldType::Int64",
    "uint32": "FieldType::UInt32",
    "bool": "FieldType::Bool",
}


class FormulaTests(unittest.TestCase):
    def test_a_known_blueprint_field_reproduces(self):
        """A fact with no struct nesting, as an anchor for the formula itself."""
        self.assertEqual(compatible_checksum([("Deployed", "bool")]), 3902815170)

    def test_every_leaf_depends_on_its_parents(self):
        leaf_only = compatible_checksum([("EffectID", "int64")])
        self.assertNotEqual(leaf_only, 2340855891)


class FactTests(unittest.TestCase):
    def test_every_chain_reproduces_its_replay_checksum(self):
        for checksum, chain, _rivals, _table, where in FACTS:
            with self.subTest(where=where):
                self.assertEqual(compatible_checksum(chain), checksum)

    def test_no_rival_leaf_type_reproduces_it(self):
        for checksum, chain, rivals, _table, where in FACTS:
            for rival in rivals:
                with self.subTest(where=where, rival=rival):
                    self.assertNotEqual(
                        compatible_checksum(chain[:-1] + [(chain[-1][0], rival)]),
                        checksum)

    def test_the_checksum_table_agrees_with_the_proven_type(self):
        committed = ect.load_committed()
        for checksum, chain, _rivals, table_type, where in FACTS:
            with self.subTest(where=where):
                if table_type is None:
                    continue
                self.assertEqual(FIELD_TYPE_OF[chain[-1][1]], table_type)
                self.assertEqual(committed.get(checksum), table_type)


if __name__ == "__main__":
    unittest.main()
