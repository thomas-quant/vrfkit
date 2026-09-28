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
import generate_scoped_types as gst  # noqa: E402


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
    # An FTransform reaches the wire as three members. `249` (the hardcoded
    # FName index of `Rotation`) is its FQuat, sent as X/Y/Z -- not a rotator.
    (747197698, [("Transform", "FTransform"), ("Rotation", "FQuat")],
     ["FRotator", "FVector"], "FieldType::VectorDouble",
     "MulticastPlay*Effect(FromClient).249, TransformTransitionContext.249"),
    (2235276067, [("Transform", "FTransform"), ("Translation", "FVector")],
     ["FQuat"], "FieldType::VectorDouble",
     "MulticastPlay*Effect(FromClient).Translation"),
    (2983776962, [("Transform", "FTransform"), ("Scale3D", "FVector")],
     ["FQuat"], "FieldType::VectorDouble",
     "MulticastPlay*Effect(FromClient).Scale3D"),
    (1874998526, [("SpawnTransform", "FTransform"), ("Rotation", "FQuat")],
     ["FRotator", "FVector"], "FieldType::VectorDouble",
     "AresGameStateBase:MulticastResetForRespawn.249"),
    (177696787, [("ValveSetTransform", "FTransform"), ("Rotation", "FQuat")],
     ["FRotator", "FVector"], None,
     "MulticastAddSmokeScreenPoint.249 / MulticastAddAnchor.249 (raw)"),
    # The effect-placement RPCs' `249` is a different property: a top-level
    # FRotator, typed RotationShort by name (no checksum-table entry).
    (2526428638, [("Rotation", "FRotator")], ["FQuat", "FVector"], None,
     "ClientPlayOneShotEffectAtLocation / ReplayPlay*AtLocation / ReplayRecord*.249"),
    (598402184, [("Location", "FVector")], ["FVector_NetQuantize"], "FieldType::VectorDouble",
     "the effect-placement RPCs' 248"),
]

#: Blueprint properties typed by exact group, name and checksum
#: (tools/fixtures/scoped_type_evidence.json -> scoped_types.rs):
#: (replay checksum, property name, C++ type, rival types that must NOT
#:  reproduce it, the FieldType every scoped entry carrying the checksum must
#:  have, where).
#:
#: Each name and C++ type is a name-level fact from the 13.06 Blueprint class
#: definitions (read-only, 2026-09-28): the property's FProperty class, and
#: for an object reference its class, which fixes the `A`/`U` prefix. A
#: top-level Blueprint property has no parent, so its chain is one step.
SCOPED_FACTS = [
    (3110715024, "TrailPosition", "FVector", ["FVector3f", "FRotator"],
     "FieldType::VectorDouble", "Projectile_Hunter_{Q_RevealBolt,4_ExplosiveBolt}_C"),
    (1066899736, "IsPossessed", "bool", ["uint8"], "FieldType::Bool",
     "(Rift_)PossessableActorComponent_C"),
    (2181339745, "Possessed", "bool", ["uint8"], "FieldType::Bool",
     "Pawn_Gumshoe_E_PossessableCamera_C"),
    (2029268412, "IsDeployed", "bool", ["uint8"], "FieldType::Bool",
     "Pawn_Gumshoe_E_PossessableCamera_C"),
    (2740089937, "DeployedActor", "AActor*", ["UActor*", "APawn*"],
     "FieldType::ObjectNetGuid", "Ability_Killjoy_{E_Turret,Q_Alarmbot}_C"),
    (1908355023, "CurrentCharge", "double", ["float"], "FieldType::Double",
     "Comp_Equippable_Charged_C"),
    (1863385026, "CurrentLossStreak", "int32", ["uint32", "float"], "FieldType::Int32",
     "BombGameState_C"),
    (22256526, "LossStreakTeam", "UBaseTeamComponent*", ["ABaseTeamComponent*"],
     "FieldType::ObjectNetGuid", "BombGameState_C, Swiftplay_EoRCredits_GameState_C"),
    (2889152318, "ShouldOverrideMatchTimer", "bool", ["uint8"], "FieldType::Bool",
     "BombGameState_C, Swiftplay_EoRCredits_GameState_C"),
    # A TextProperty: typed with the full-tree reader, because the legacy
    # FText one keeps only string-table keys and refuses both forms it sends.
    (4004484071, "OverrideMatchTimerText", "FText", ["FString", "FName"],
     "FieldType::FTextTree", "BombGameState_C, Swiftplay_EoRCredits_GameState_C"),
    # The other Blueprint identities of 2026-09-28 (the ceremonies, the kill
    # effect classes, map interactables, ability items and effect objects):
    # a class reference is `UClass*`, not `TSubclassOf<>`, and an object
    # reference's A/U prefix is its class's.
    (1807371052, "ActiveSlowTimeEffects", "bool", ["uint8"],
     "FieldType::Bool", "BombGameState_C, Swiftplay_EoRCredits_GameState_C"),
    (2379229353, "AssignedPlayspace", "UClass*", ["TSubclassOf<UPlayspace>", "TSubclassOf<APlayspace>"],
     "FieldType::ObjectNetGuid", "Actor_Sequoia_X_StandardArena_C"),
    (2511444522, "BlueTeamStartingAvgInventoryValue", "int32", ["uint32", "float"],
     "FieldType::Int32", "ThriftyCeremony_C"),
    (945042264, "Bubble_Destroy_Audio", "UAkAudioEvent*", ["AAkAudioEvent*"],
     "FieldType::ObjectNetGuid", "FXC_Finisher_Destructible_C, FXC_WaterBlaster_Finisher_Destructible_C"),
    (3040835906, "CharacterLocation", "FVector", ["FVector3f", "FRotator"],
     "FieldType::VectorDouble", "GameObject_Breach_E_SweetSpotFissure_C, GameObject_SoundSensor_SweetSpotFissure_C"),
    (4180852283, "Charge", "double", ["float"],
     "FieldType::Double", "GameObject_Breach_4_FusionBlast_C, Projectile_Breach_4_FusionBlast_C"),
    (261706910, "ChargeDistance", "double", ["float"],
     "FieldType::Double", "Projectile_Breach_4_FusionBlast_C"),
    (951985780, "CloserPlayer", "AShooterPlayerState*", ["UShooterPlayerState*"],
     "FieldType::ObjectNetGuid", "CloserCeremony_C"),
    (829915424, "ClutchPlayer", "AShooterPlayerState*", ["UShooterPlayerState*"],
     "FieldType::ObjectNetGuid", "ClutchCeremony_C"),
    (4217858995, "Collision Offset Position", "FVector", ["FVector3f", "FRotator"],
     "FieldType::VectorDouble", "FXC_Finisher_Destructible_C, FXC_WaterBlaster_Finisher_Destructible_C"),
    (3409090095, "DefaultToTargetViewModeTargeting", "bool", ["uint8"],
     "FieldType::Bool", "Ability_Mage_E_WorldSmoke_C, Ability_Wraith_4_Smoke_C"),
    (1997410783, "Destroyed", "bool", ["uint8"],
     "FieldType::Bool", "BP_Breakable_Simple_Rockman_C, BP_Breakable_Simple_Saltman_Helmet_C"),
    (3404560428, "Enable Rotate around Axis", "bool", ["uint8"],
     "FieldType::Bool", "FXC_Finisher_Destructible_C"),
    (1641873608, "EndingStrikeLocation", "FVector", ["FVector3f", "FRotator"],
     "FieldType::VectorDouble", "AIPawn_Cashew_E_SeekingTargetMissile_C"),
    (3355379499, "FlawlessTeam", "UBaseTeamComponent*", ["ABaseTeamComponent*"],
     "FieldType::ObjectNetGuid", "FlawlessCeremony_C"),
    (3878297219, "Float", "double", ["float"],
     "FieldType::Double", "FloatTransitionContext_C"),
    (422586278, "GameplayStartTime", "double", ["float"],
     "FieldType::Double", "Switch_BlackMarket_2_C"),
    (1717661931, "Has Succesfully Hit", "bool", ["uint8"],
     "FieldType::Bool", "Patch_Aggrobot_C_ExplodeyPatch_C, Patch_Cable_4_NetToss_C, Patch_Deadeye_E_Slow_Large_C, Patch_Pandemic_AcidMolotov_NewMolotov_C, Patch_Phoenix_MolotovFire_C, Patch_Sarge_Q_Molotov_Production_C"),
    (1537044997, "HasPlayed", "bool", ["uint8"],
     "FieldType::Bool", "Switch_BlackMarket_2_C"),
    (6924581, "Hidden in Game", "bool", ["uint8"],
     "FieldType::Bool", "FXC_Finisher_Destructible_C, FXC_Rogue_Finisher_Destructible_C, FXC_WaterBlaster_Finisher_Destructible_C"),
    (501730740, "Is State Concuss", "bool", ["uint8"],
     "FieldType::Bool", "Ability_Iris_Thumper_C"),
    (1939830999, "IsActivated", "bool", ["uint8"],
     "FieldType::Bool", "ActivatableActorComponent_C, ActivatableActor_Selectable_Component_C"),
    (445799890, "IsAlive_0", "bool", ["uint8"],
     "FieldType::Bool", "GameObject_Thorne_E_Wall_Segment_Fortifying_C"),
    (3166326863, "IsArmed", "bool", ["uint8"],
     "FieldType::Bool", "Pawn_Killjoy_Q_StealthAlarmbot_C"),
    (2671071791, "IsBurrowed", "bool", ["uint8"],
     "FieldType::Bool", "Pawn_Killjoy_Q_StealthAlarmbot_C"),
    (1136330846, "IsCurrentlyDoingInitialFlyOut", "bool", ["uint8"],
     "FieldType::Bool", "AIPawn_Cashew_E_SeekingTargetMissile_C"),
    (2206903017, "IsDisabled", "bool", ["uint8"],
     "FieldType::Bool", "Switch_BlackMarket_2_C"),
    (1443783453, "LastUsedTime", "double", ["float"],
     "FieldType::Double", "Switch_BlackMarket_2_C"),
    (3871083543, "LeverDown", "bool", ["uint8"],
     "FieldType::Bool", "Switch_BlackMarket_2_C"),
    (2321996340, "Projectile", "AActor*", ["UActor*"],
     "FieldType::ObjectNetGuid", "Ability_Sequoia_Q_FragileMissilePrototypeEquipped_C, Ability_Wraith_Q_NearsightMissile_C"),
    (1159482238, "RedTeamStartingAvgInventoryValue", "int32", ["uint32", "float"],
     "FieldType::Int32", "ThriftyCeremony_C"),
    (244340520, "RocketIndex", "int32", ["uint32", "float"],
     "FieldType::Int32", "AIPawn_Cashew_E_SeekingTargetMissile_C"),
    (2055764079, "Rotate Axis Location", "FVector", ["FVector3f", "FRotator"],
     "FieldType::VectorDouble", "FXC_Finisher_Destructible_C"),
    (3642914479, "Rotate Offset", "double", ["float"],
     "FieldType::Double", "FXC_Finisher_Destructible_C"),
    (6804471, "Rotate Speed", "double", ["float"],
     "FieldType::Double", "FXC_Finisher_Destructible_C"),
    (936511401, "SalvagingTeam", "UBaseTeamComponent*", ["ABaseTeamComponent*"],
     "FieldType::ObjectNetGuid", "ThriftyCeremony_C"),
    (1905883563, "Seeking Missile Actor", "AAIPawn_Cashew_E_SeekingTargetMissile_C*", ["UAIPawn_Cashew_E_SeekingTargetMissile_C*"],
     "FieldType::ObjectNetGuid", "GameObject_Cashew_E_MapMissileMarker_C, GameObject_Cashew_E_MapMissileMarker_SecondRocket_C"),
    (2924225553, "Target", "AActor*", ["UActor*"],
     "FieldType::ObjectNetGuid", "GameObject_Cable_4_RemovableNet_C, GameObject_Hunter_E_Drone_RevealDart_C, GameObject_RemovableObject_GumshoeTrackingDart_C"),
    (4028634209, "TargetEquippable", "AAresEquippable*", ["UAresEquippable*"],
     "FieldType::ObjectNetGuid", "EquipRequestTransitionContext_C"),
    (1655581842, "TeamAcingTeam", "UBaseTeamComponent*", ["ABaseTeamComponent*"],
     "FieldType::ObjectNetGuid", "TeamAceCeremony_C"),
    (470315408, "VFX Offset Position", "FVector", ["FVector3f", "FRotator"],
     "FieldType::VectorDouble", "FXC_Finisher_Destructible_C, FXC_WaterBlaster_Finisher_Destructible_C"),
    (1424672044, "VFX_Destroy", "UParticleSystem*", ["AParticleSystem*"],
     "FieldType::ObjectNetGuid", "Finisher_Ninja_Destructible_C"),
    (2949030953, "VFX_Duration_Ground", "UParticleSystem*", ["AParticleSystem*"],
     "FieldType::ObjectNetGuid", "Finisher_Ninja_Destructible_C"),
    (2234578776, "VFX_Duration_Water", "UParticleSystem*", ["AParticleSystem*"],
     "FieldType::ObjectNetGuid", "Finisher_Ninja_Destructible_C"),
    (2642729003, "VFX_Ground", "UParticleSystem*", ["AParticleSystem*"],
     "FieldType::ObjectNetGuid", "Finisher_Ninja_Destructible_C"),
    (1816143426, "VFX_Spawn", "UParticleSystem*", ["AParticleSystem*"],
     "FieldType::ObjectNetGuid", "Finisher_Ninja_Destructible_C"),
    (3224851082, "Victim FXC", "UClass*", ["TSubclassOf<UEffectContainer>", "TSubclassOf<AEffectContainer>"],
     "FieldType::ObjectNetGuid", "OnKillEffect_Base_C"),
    (3421571079, "VictimFXC_Planted", "UClass*", ["TSubclassOf<UEffectContainer>", "TSubclassOf<AEffectContainer>"],
     "FieldType::ObjectNetGuid", "OnKillEffect_Base_C"),
    (475466788, "WarningActor", "AGameObject_Global_LineMissile_TrajectoryWarning_C*", ["UGameObject_Global_LineMissile_TrajectoryWarning_C*"],
     "FieldType::ObjectNetGuid", "Ability_Sequoia_Q_FragileMissilePrototypeEquipped_C, Ability_Wraith_Q_NearsightMissile_C"),
    (2636440541, "bShouldDisplayCeremony", "bool", ["uint8"],
     "FieldType::Bool", "AceCeremony_C, CloserCeremony_C, ClutchCeremony_C, DefaultCeremony_C, FlawlessCeremony_C, TeamAceCeremony_C, ThriftyCeremony_C"),
]

#: The C++ leaf type each checksum-table FieldType above stands for.
FIELD_TYPE_OF = {
    "int64": "FieldType::Int64",
    "uint32": "FieldType::UInt32",
    "int32": "FieldType::Int32",
    "double": "FieldType::Double",
    "FText": "FieldType::FTextTree",
    "bool": "FieldType::Bool",
    # Three doubles on this wire (UE5 large world coordinates); FQuat sends
    # X/Y/Z only, W implied.
    "FQuat": "FieldType::VectorDouble",
    "FVector": "FieldType::VectorDouble",
}


def field_type_of(cpp_type: str) -> str:
    """An object reference of any class is a NetGUID on the wire."""
    if cpp_type.endswith("*"):
        return "FieldType::ObjectNetGuid"
    return FIELD_TYPE_OF[cpp_type]


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


class ScopedFactTests(unittest.TestCase):
    """The scoped Blueprint entries against the type their checksum proves."""

    def test_every_blueprint_property_reproduces_its_replay_checksum(self):
        for checksum, name, cpp_type, _rivals, _field_type, where in SCOPED_FACTS:
            with self.subTest(where=where, name=name):
                self.assertEqual(compatible_checksum([(name, cpp_type)]), checksum)

    def test_no_rival_type_reproduces_it(self):
        for checksum, name, _cpp_type, rivals, _field_type, where in SCOPED_FACTS:
            for rival in rivals:
                with self.subTest(where=where, name=name, rival=rival):
                    self.assertNotEqual(compatible_checksum([(name, rival)]), checksum)

    def test_every_scoped_entry_with_the_checksum_has_the_proven_type(self):
        """Each fact must be used, by entries of its own name and its type.

        Read from the reviewed fixture; `generate_scoped_types.py --check`
        (in the sweep) holds scoped_types.rs to the same entries.
        """
        entries = gst.load(gst.EVIDENCE)
        for checksum, name, cpp_type, _rivals, field_type, where in SCOPED_FACTS:
            with self.subTest(where=where, name=name):
                self.assertEqual(field_type_of(cpp_type), field_type)
                carriers = [e for e in entries if e["checksum"] == checksum]
                self.assertTrue(carriers, "no scoped entry uses this fact")
                for entry in carriers:
                    self.assertEqual(entry["field"], name)
                    self.assertEqual(gst.TYPES[entry["type"]], (field_type,))


if __name__ == "__main__":
    unittest.main()
