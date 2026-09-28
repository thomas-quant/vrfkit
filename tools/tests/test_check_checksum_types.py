"""Guards for the compatible-checksum type check.

The check is only worth running if three things hold, and each one can be
broken without anything else in the repo noticing:

  * the formula reproduces checksums the game itself wrote -- the vectors
    below are declared checksums from real replays, not values this file
    computed and then asserted;
  * a different type string does NOT reproduce them, so a match is evidence
    of the type and not of the name alone;
  * `untestable` never reaches the match count, and every counter prints
    even at zero.
"""
import contextlib
import io
import json
import random
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import check_checksum_types as cct  # noqa: E402


# ---------------------------------------------------------------------------
# A second implementation of the formula, written from UE 5.3 FCrc rather than
# from zlib: a CRC-32 table built here, `StrCrc32` feeding every TCHAR as four
# bytes low byte first, `MemCrc32` over the little-endian static index.

def _table():
    table = []
    for i in range(256):
        c = i
        for _ in range(8):
            c = (c >> 1) ^ 0xEDB88320 if c & 1 else c >> 1
        table.append(c)
    return table


CRC_TABLE = _table()


def ue_str_crc32(text, crc):
    crc = ~crc & 0xFFFFFFFF
    for ch in text:
        code = ord(ch)
        for _ in range(4):
            crc = (crc >> 8) ^ CRC_TABLE[(crc ^ code) & 0xFF]
            code >>= 8
    return ~crc & 0xFFFFFFFF


def ue_mem_crc32_u32(value, crc):
    crc = ~crc & 0xFFFFFFFF
    for shift in (0, 8, 16, 24):
        crc = (crc >> 8) ^ CRC_TABLE[(crc ^ (value >> shift)) & 0xFF]
    return ~crc & 0xFFFFFFFF


def ue_checksum(name, cpp_type, static_index=0, parent=0):
    crc = ue_str_crc32(name.lower(), parent)
    crc = ue_str_crc32(cpp_type.lower(), crc)
    return ue_mem_crc32_u32(static_index, crc)


# ---------------------------------------------------------------------------
# Declared checksums from real replays (the 1,018-replay declaration corpus,
# 11.06-13.06, collected 2026-09-28), with the parent chain that reproduces
# them. The chains are PARENT_CHAINS entries; their provenance is there.
# (wire name, name hashed, C++ type, chain links, declared checksum)

TRANSFORM = (("Transform", "FTransform"),)
SPAWN_TRANSFORM = (("SpawnTransform", "FTransform"),)
HANDLE = (("Handle", "FForceModuleHandle"),)
CORRECT = (("AuthServerCorrectRepVariables", "FInventoryServerCorrectRepVariables"),)
BLIND = (("AuthBlindManagerState", "FBlindManagerState"), ("ActiveBlinds", "TArray"),
         ("ActiveBlinds", "FActiveBlind"))
BLIND_EFFECT = BLIND + (("BlindEffectID", "FEffectID"),)
FRAGMENT = (("FragmentInfo", "FGroundVolumeFragmentArray"), ("Items", "TArray"),
            ("Items", "FGroundVolumeFragment"))
GRID = FRAGMENT + (("GridPos", "FIntPoint"),)
ATTACH = (("AttachmentReplication", "FRepAttachment"),)
EFFECT_ID = (("EffectID", "FEffectID"),)

REPLAY_VECTORS = (
    # effect RPCs and TransformTransitionContext: `249` is FTransform.Rotation
    ("249", "Rotation", "FQuat", TRANSFORM, 747197698),
    ("Translation", "Translation", "FVector", TRANSFORM, 2235276067),
    ("Scale3D", "Scale3D", "FVector", TRANSFORM, 2983776962),
    ("249", "Rotation", "FQuat", SPAWN_TRANSFORM, 1874998526),
    ("HandleNumber", "HandleNumber", "uint32", HANDLE, 3336285386),
    ("CorrectionIndex", "CorrectionIndex", "int32", CORRECT, 3198546915),
    ("LastSeenClientCorrectionIndex", "LastSeenClientCorrectionIndex", "int32", CORRECT, 1076231069),
    ("StopMovementTime", "StopMovementTime", "float", (), 244888268),
    ("EffectManagerComponent", "EffectManagerComponent", "UEffectManagerComponent*", (), 1051633025),
    ("EffectID", "EffectID", "int64", EFFECT_ID, 2340855891),
    ("BlindId", "BlindId", "uint32", BLIND, 2836858544),
    ("InitialDuration", "InitialDuration", "float", BLIND, 1370668337),
    ("SourceID", "SourceID", "FName", BLIND_EFFECT, 4130766059),
    ("bLocalEffect", "bLocalEffect", "bool", BLIND_EFFECT, 2802682995),
    ("EffectID", "EffectID", "int64", BLIND_EFFECT, 3321413110),
    ("253", "ID", "int32", FRAGMENT, 1175316786),
    ("bIsActive", "bIsActive", "bool", FRAGMENT, 518428974),
    ("TravelDistance", "TravelDistance", "float", FRAGMENT, 956522941),
    ("X", "X", "int32", GRID, 2123226522),
    ("Y", "Y", "int32", GRID, 2134384775),
    ("ConvexHullPoints", "ConvexHullPoints", "TArray", FRAGMENT, 3039966384),
    # an array's element continues from the array's own declared checksum
    ("ConvexHullPoints", "ConvexHullPoints", "FVector", FRAGMENT + (("ConvexHullPoints", "TArray"),), 2749781999),
    ("AttachParent", "AttachParent", "AActor*", ATTACH, 3846863854),
    ("RelativeScale3D", "RelativeScale3D", "FVector_NetQuantize100", ATTACH, 1992268157),
    # top-level engine properties
    ("PlayerState", "PlayerState", "APlayerState*", (), 3964024390),
    ("SpawnLocation", "SpawnLocation", "FVector", (), 1139493118),
    ("248", "Location", "FVector", (), 598402184),
    # a bitfield bool (`uint8 bIsActive:1`) hashes as its storage type
    ("bIsActive", "bIsActive", "uint8", (), 2967469237),
    # Blueprint fields matched by the 13.06 pak reader (bp-properties track)
    ("BoundToGamePhase", "BoundToGamePhase", "bool", (), 520326154),
)


class FormulaTests(unittest.TestCase):
    def test_the_two_implementations_agree(self):
        """zlib over UTF-32LE against the per-TCHAR FCrc loop, 2,000 inputs."""
        rng = random.Random(20260928)
        alphabet = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_<>*: "
        for _ in range(2000):
            name = "".join(rng.choice(alphabet) for _ in range(rng.randint(1, 40)))
            cpp = "".join(rng.choice(alphabet) for _ in range(rng.randint(1, 30)))
            index, parent = rng.randrange(64), rng.randrange(2 ** 32)
            self.assertEqual(cct.compatible_checksum(name, cpp, index, parent),
                             ue_checksum(name, cpp, index, parent))

    def test_replay_checksums_reproduce_with_both_implementations(self):
        for wire, name, cpp, links, declared in REPLAY_VECTORS:
            with self.subTest(wire=wire, cpp=cpp):
                seed = cct.chain_checksum(links)
                self.assertEqual(cct.compatible_checksum(name, cpp, 0, seed), declared)
                ue_seed = 0
                for link_name, link_type in links:
                    ue_seed = ue_checksum(link_name, link_type, 0, ue_seed)
                self.assertEqual(ue_checksum(name, cpp, 0, ue_seed), declared)

    def test_a_different_type_string_does_not_reproduce(self):
        """A match must be evidence of the type, not of the name alone."""
        for wire, name, cpp, links, declared in REPLAY_VECTORS:
            seed = cct.chain_checksum(links)
            for other in cct.ALTERNATIVE_TYPES + ("UEffectManagerComponent*", "APlayerState*"):
                if other.lower() == cpp.lower():
                    continue
                with self.subTest(wire=wire, cpp=cpp, other=other):
                    self.assertNotEqual(cct.compatible_checksum(name, other, 0, seed), declared)

    def test_each_step_of_the_formula_is_load_bearing(self):
        """Dropping the lower-casing, the UTF-32 width, the index or the parent
        breaks every vector -- so the vectors pin the formula, not a lookalike."""
        import struct
        import zlib
        variants = {
            "no lower-casing": lambda n, t, i, p: zlib.crc32(
                struct.pack("<I", i), zlib.crc32(t.encode("utf-32-le"), zlib.crc32(n.encode("utf-32-le"), p))),
            "utf-16": lambda n, t, i, p: zlib.crc32(
                struct.pack("<I", i), zlib.crc32(t.lower().encode("utf-16-le"),
                                                 zlib.crc32(n.lower().encode("utf-16-le"), p))),
            "no index": lambda n, t, i, p: zlib.crc32(
                t.lower().encode("utf-32-le"), zlib.crc32(n.lower().encode("utf-32-le"), p)),
            "parent ignored": lambda n, t, i, p: cct.compatible_checksum(n, t, i, 0),
        }
        for label, variant in variants.items():
            reproduced = sum(variant(name, cpp, 0, cct.chain_checksum(links)) == declared
                             for _, name, cpp, links, declared in REPLAY_VECTORS)
            # Top-level vectors survive "parent ignored" by definition, and the
            # all-lower-case ones survive "no lower-casing"; nothing else may.
            if label == "parent ignored":
                self.assertEqual(reproduced, sum(1 for v in REPLAY_VECTORS if not v[3]), label)
            elif label == "no lower-casing":
                self.assertEqual(reproduced, sum(1 for v in REPLAY_VECTORS
                                                 if v[1] == v[1].lower() and v[2] == v[2].lower()
                                                 and all(a == a.lower() and b == b.lower()
                                                         for a, b in v[3])), label)
            else:
                self.assertEqual(reproduced, 0, label)

    def test_non_ascii_is_refused_rather_than_hashed(self):
        with self.assertRaises(ValueError):
            cct.compatible_checksum("Bé", "float")

    def test_every_parent_chain_fact_is_pinned_by_a_replay_vector(self):
        """A chain with a typo would silently reproduce nothing; each one must
        reproduce a declared checksum here, or be a prefix of one that does."""
        pinned = {links for *_, links, _ in REPLAY_VECTORS}
        prefixes = {links[:i] for links in pinned for i in range(1, len(links) + 1)}
        for chain in cct.PARENT_CHAINS:
            with self.subTest(chain=chain.links):
                self.assertTrue(chain.links in prefixes or self._reproduces_member(chain),
                                "no replay vector exercises this chain")

    @staticmethod
    def _reproduces_member(chain):
        # Chains whose members vrfkit types but no vector above names: pinned
        # by a member checksum from the same corpus.
        extra = {
            (("TimeStamp", "FNetworkedMovementTimestamp"),): ("NetTimestamp", "float", 259706372),
            (("CurrentEffectID", "FEffectID"),): ("EffectID", "int64", 2251343646),
            (("ServerActiveEffects", "TArray"), ("ServerActiveEffects", "FActiveEffectInfo")):
                ("StartTimeStamp", "float", 3801979459),
            (("ServerActiveEffects", "TArray"), ("ServerActiveEffects", "FActiveEffectInfo"),
             ("EffectID", "FEffectID")): ("EffectID", "int64", 1129645208),
            (("ServerActiveEffects", "TArray"), ("ServerActiveEffects", "FActiveEffectInfo"),
             ("Transform", "FTransform")): ("Translation", "FVector", 2319708401),
            (("AuthBlindManagerState", "FBlindManagerState"),):
                ("LongestActiveBlindDuration", "float", 3668710569),
        }.get(chain.links)
        if extra is None:
            return False
        name, cpp, declared = extra
        return cct.compatible_checksum(name, cpp, 0, cct.chain_checksum(chain.links)) == declared


class ParseTests(unittest.TestCase):
    def test_field_types_parse_with_their_parameters(self):
        self.assertEqual(cct.parse_field_type("FieldType::Float"), ("Float", {}))
        self.assertEqual(cct.parse_field_type("FieldType::SerializedInt { max: 65536 }"),
                         ("SerializedInt", {"max": 65536}))
        multi = ("FieldType::RepMovement {\n    rotation: RotatorQuantization::ByteComponents,\n"
                 "    location: VectorQuantization::RoundWholeNumber,\n}")
        self.assertEqual(cct.canonical_type(multi),
                         "RepMovement { rotation: ByteComponents, location: RoundWholeNumber }")

    def test_every_field_type_variant_is_classified(self):
        """A new FieldType must be given a C++ spelling (or a reason) before
        this check can be trusted with it; main() refuses to run otherwise."""
        variants = cct.parse_field_type_variants(cct.DECODE_RS.read_text(encoding="utf-8"))
        self.assertIn("ObjectNetGuid", variants)
        self.assertEqual(set(variants) - set(cct.CPP_TYPES) - set(cct.UNTYPED_VARIANTS), set())

    def test_a_field_type_enum_that_does_not_parse_is_refused(self):
        """An empty read would make "every variant is classified" vacuous."""
        with self.assertRaises(cct.TableError):
            cct.parse_field_type_variants("pub enum FieldType {\n}\n")
        with self.assertRaises(cct.TableError):
            cct.parse_field_type_variants("pub enum FieldKind {\n    Raw,\n    Skip,\n}\n")

    def test_the_committed_tables_parse_whole(self):
        resolver = cct.Resolver.from_repo()
        self.assertGreater(len(resolver.entries), 1000)
        self.assertGreater(len(resolver.checksums), 400)
        self.assertIn("Owner", resolver.engine_refs)
        # every alias read out of overlay.rs must lead somewhere the table can
        # type, or the Python resolver would be porting a different order
        self.assertTrue(resolver.aliases)
        tabled_groups = {group for group, _ in resolver.entries}
        for source, target in resolver.aliases.items():
            with self.subTest(alias=source):
                self.assertTrue(source.startswith("/") and "." in source)
                self.assertIn(target, tabled_groups)

    def test_an_entry_the_pattern_misses_is_refused(self):
        src = ('pub static OVERLAY_TABLE: [OverlayEntry; 2] = [\n'
               '    OverlayEntry {\n        group_path: "/G.A",\n        field_name: "X",\n'
               '        field_type: FieldType::Float,\n    },\n'
               '    OverlayEntry {\n        group_path: "/G.A",\n        field_name: "Y",\n'
               '        field_type: Float,\n    },\n];\n'
               'pub static OVERLAY_HANDLE_TABLE: [OverlayHandleEntry; 0] = [];\n')
        with self.assertRaises(cct.TableError):
            cct.parse_overlay_table(src)
        whole = src.replace("field_type: Float", "field_type: FieldType::Float")
        entries, handles = cct.parse_overlay_table(whole)
        self.assertEqual(len(entries), 2)

    def test_resolution_constants_are_read_from_overlay_rs(self):
        src = ('const GROUP_ALIASES: &[(&str, &str)] = &[\n    (\n        "/G/A\\\n/B.B_C",\n'
               '        "/G/C.C_C",\n    ),\n];\nconst ENGINE_OBJECT_REFS: [&str; 2] = ["Owner", "Controller"];\n')
        aliases, refs = cct.parse_resolution_constants(src)
        self.assertEqual(aliases, {"/G/A/B.B_C": "/G/C.C_C"})
        self.assertEqual(refs, ("Owner", "Controller"))


def resolver(entries=None, handles=None, scoped=None, checksums=None, aliases=None,
             refs=("Owner", "Instigator", "AttachParent", "Controller")):
    return cct.Resolver(entries or {}, handles or {}, scoped or {}, checksums or {},
                        aliases or {}, refs)


class ResolverTests(unittest.TestCase):
    """The order of `overlay::resolve_entry`, on synthetic tables."""

    def test_name_then_b_prefix_then_handle(self):
        r = resolver(entries={("/G.A", "X"): "Float", ("/G.A", "bFlag"): "Bool",
                              ("/G.A", "Loc"): "VectorDouble"},
                     handles={("/G.A", 5): "Loc"})
        self.assertEqual(r.resolve("/G.A", "X", 1, 9), ("Float", "name"))
        self.assertEqual(r.resolve("/G.A", "Flag", 1, 9), ("Bool", "b-prefix"))
        self.assertEqual(r.resolve("/G.A", "248", 5, 9), ("VectorDouble", "handle"))

    def test_a_conflicting_wire_name_refuses_the_handle(self):
        r = resolver(entries={("/G.A", "Loc"): "VectorDouble"}, handles={("/G.A", 5): "Loc"})
        self.assertEqual(r.resolve("/G.A", "Other", 5, 9), (None, None))

    def test_alias_scoped_engine_reference_then_checksum_table(self):
        r = resolver(entries={("/G.Bomb", "X"): "Int32"}, aliases={"/G.Swift": "/G.Bomb"},
                     scoped={("B", "/G.P", 7): "Byte"}, checksums={7: "UInt32", 8: "Float"})
        self.assertEqual(r.resolve("/G.Swift", "X", 1, 1), ("Int32", "alias name"))
        self.assertEqual(r.resolve("/G.P", "B", 1, 7), ("Byte", "scoped"))
        self.assertEqual(r.resolve("/G.Q", "B", 1, 7), ("UInt32", "checksum table"))
        self.assertEqual(r.resolve("/G.Q", "Owner", 1, 8), ("ObjectNetGuid", "engine reference"))
        self.assertEqual(r.resolve("/G.Q", "Y", 1, 99), (None, None))


def checker(classes=("Actor", "Pawn"), chains=cct.PARENT_CHAINS):
    return cct.Checker(cct.Seeds(chains), cct.object_spellings(classes))


class ClassifyTests(unittest.TestCase):
    def test_the_right_type_matches_and_a_changed_type_string_mismatches(self):
        """The mutation the check exists for: same declaration, vrfkit's type
        changed from Float to Double -- the checksum now names `float`."""
        declared = cct.compatible_checksum("Health", "float")
        self.assertEqual(checker().classify("Float", "Health", declared).verdict, "match")
        wrong = checker().classify("Double", "Health", declared)
        self.assertEqual((wrong.verdict, wrong.detail), ("mismatch", "float"))

    def test_a_sign_or_a_quaternion_is_a_mismatch(self):
        seed = cct.chain_checksum(HANDLE)
        v = checker().classify("Int32", "HandleNumber", cct.compatible_checksum("HandleNumber", "uint32", 0, seed))
        self.assertEqual((v.verdict, v.detail), ("mismatch", "uint32"))
        v = checker().classify("VectorDouble", "249", 747197698)
        self.assertEqual((v.verdict, v.detail, v.hit.seed), ("mismatch", "FQuat", "Transform:FTransform"))

    def test_a_nested_member_is_untestable_without_its_chain(self):
        declared = 3198546915  # AresInventory.CorrectionIndex
        self.assertEqual(checker().classify("Int32", "CorrectionIndex", declared).verdict, "match")
        v = checker(chains=()).classify("Int32", "CorrectionIndex", declared)
        self.assertEqual(v.verdict, "untestable")

    def test_an_enum_is_untestable_never_a_match(self):
        declared = cct.compatible_checksum("Mode", "TEnumAsByte<EMode>")
        for field_type in ("EnumByte", "EnumRemainingBits", "Byte", "SerializedInt { max: 16 }"):
            with self.subTest(field_type=field_type):
                v = checker().classify(field_type, "Mode", declared)
                self.assertEqual((v.verdict, v.detail), ("untestable", cct.ENUM_CAPABLE))

    def test_a_type_with_no_spelling_is_untestable_whatever_reproduces(self):
        """`ByteArray` names no C++ type, so a `TArray` reproduction cannot
        contradict it."""
        declared = cct.compatible_checksum("Blob", "TArray")
        v = checker().classify("ByteArray { max_bytes: 64 }", "Blob", declared)
        self.assertEqual((v.verdict, v.detail), ("untestable", "no single C++ spelling"))

    def test_an_object_needs_its_class_among_the_candidates(self):
        declared = cct.compatible_checksum("Owner", "AActor*")
        self.assertEqual(checker().classify("ObjectNetGuid", "Owner", declared).detail, "AActor*")
        v = checker(classes=("Pawn",)).classify("ObjectNetGuid", "Owner", declared)
        self.assertEqual(v.verdict, "untestable")

    def test_bool_as_uint8_is_a_match_with_its_own_rule(self):
        v = checker().classify("Bool", "bIsActive", 2967469237)
        self.assertEqual((v.verdict, v.detail), ("match", "uint8"))
        self.assertEqual(cct.match_rule("Bool", v.hit),
                         "Bool as uint8 (bitfield bool or byte; the width decides)")

    def test_fname_indices_checksum_zero_and_non_ascii_are_untestable(self):
        self.assertEqual(checker().classify("VectorDouble", "248", 598402184).verdict, "match")
        for name, checksum, reason in (
                ("108", 615243487, "bare FName index 108 not in HARDCODED_FNAMES"),
                # a group that declares no checksum (NetworkGameplayTagNodeIndex
                # declares `248` with 0) is not a failed reproduction
                ("Health", 0, "declared checksum is 0"),
                ("Hé", 5, "non-ASCII name")):
            with self.subTest(name=name):
                v = checker().classify("Float", name, checksum)
                self.assertEqual((v.verdict, v.detail), ("untestable", reason))

    def test_every_recomputation_is_counted(self):
        c = checker()
        c.classify("Float", "Health", cct.compatible_checksum("Health", "float"))
        self.assertGreater(c.trials, 0)


def identity(group, name, handle, checksum, build="13.06"):
    return cct.Identity(group, name, handle, checksum, {build}, 1)


class ReportTests(unittest.TestCase):
    def setUp(self):
        self.resolver = resolver(entries={("/G.A", "Health"): "Float", ("/G.A", "Mode"): "EnumByte",
                                          ("/G.A", "Raw"): "Raw"},
                                 checksums={cct.compatible_checksum("Count", "uint32"): "Int32",
                                            cct.compatible_checksum("Speed", "float"): "Float",
                                            123: "Float"})

    def test_untestable_is_never_counted_as_a_match(self):
        ids = {("/G.A", "Mode", 1, 7): identity("/G.A", "Mode", 1, 7)}
        report = cct.check_identities(ids, self.resolver, checker())
        self.assertEqual((report.by_verdict["match"], report.by_verdict["untestable"]), (0, 1))

    def test_verdicts_partition_the_typed_identities(self):
        ok = cct.compatible_checksum("Health", "float")
        ids = {
            ("/G.A", "Health", 1, ok): identity("/G.A", "Health", 1, ok),
            ("/G.A", "Mode", 2, 7): identity("/G.A", "Mode", 2, 7),
            ("/G.A", "Raw", 3, 8): identity("/G.A", "Raw", 3, 8),
            ("/G.A_ClassNetCache", "Fn", 1, 9): identity("/G.A_ClassNetCache", "Fn", 1, 9),
            ("/G.A", "Unknown", 4, 10): identity("/G.A", "Unknown", 4, 10),
        }
        report = cct.check_identities(ids, self.resolver, checker())
        self.assertEqual(report.identities, 2)
        self.assertEqual(sum(report.by_verdict.values()), 2)
        self.assertEqual(report.not_checked["not typed by vrfkit (Raw)"], 1)
        self.assertEqual(report.not_checked["function slots (ClassNetCache groups)"], 1)
        self.assertEqual(report.not_checked["not typed by vrfkit (no resolution)"], 1)

    def test_checksum_table_entries_are_checked_under_their_carriers(self):
        count = cct.compatible_checksum("Count", "uint32")
        speed = cct.compatible_checksum("Speed", "float")
        ids = {("/G.B", "Count", 1, count): identity("/G.B", "Count", 1, count),
               ("/G.B", "Speed", 2, speed): identity("/G.B", "Speed", 2, speed)}
        counts, bad = cct.check_checksum_table(ids, self.resolver, checker())
        self.assertEqual((counts["match"], counts["mismatch"], counts["no carrier in the input"]), (1, 1, 1))
        self.assertEqual([(c, n, s) for c, _, n, s, _ in bad], [(count, "Count", "uint32")])


def siblings_for(checker_=None):
    objects = checker_.objects if checker_ is not None else ()
    return cct.SiblingSeeds(cct.spelling_universe(objects))


class SiblingSeedTests(unittest.TestCase):
    """Tier 2: a parent's checksum recovered from the members that share it."""

    PARENT = 0x5EED1234

    def member(self, name, truth, parent=None):
        return cct.compatible_checksum(name, truth, 0, self.PARENT if parent is None else parent)

    def test_implied_parent_inverts_the_formula(self):
        rng = random.Random(7)
        for _ in range(300):
            name = "".join(rng.choice("abcXYZ_0123") for _ in range(rng.randint(1, 30)))
            cpp = rng.choice(cct.ALTERNATIVE_TYPES)
            index, parent = rng.randrange(8), rng.randrange(2 ** 32)
            declared = cct.compatible_checksum(name, cpp, index, parent)
            self.assertEqual(cct.implied_parent(declared, name, cpp, index), parent)

    def test_the_transform_seed_is_recovered_from_its_members(self):
        """Translation and Scale3D alone give back `Transform:FTransform`."""
        found = siblings_for().establish([("Translation", 2235276067, ("FVector",)),
                                          ("Scale3D", 2983776962, ("FVector",))])
        self.assertEqual(list(found.established), [cct.chain_checksum(TRANSFORM)])

    def test_right_types_with_different_name_lengths_establish_a_seed(self):
        s = siblings_for()
        found = s.establish([("Health", self.member("Health", "float"), ("float",)),
                             ("SourceID", self.member("SourceID", "FName"), ("FName",))])
        self.assertEqual(list(found.established), [self.PARENT])
        self.assertGreater(s.comparisons, 0)
        # top-level members agree on 0, which is known already, not recovered
        top = s.establish([("Health", cct.compatible_checksum("Health", "float"), ("float",)),
                           ("SourceID", cct.compatible_checksum("SourceID", "FName"), ("FName",))])
        self.assertEqual((top.established, top.refused), ({}, {}))

    def test_equal_name_lengths_under_one_wrong_spelling_are_refused(self):
        """The systematic false agreement: `A` and `B` are int32, both read as
        uint8 -- their implied parents agree, at a wrong seed."""
        members = [(n, self.member(n, "int32"), ("uint8",)) for n in ("A", "B")]
        s = siblings_for()
        found = s.establish(members)
        self.assertEqual(found.established, {})
        self.assertEqual(len(found.refused), 1)
        self.assertNotIn(self.PARENT, found.refused)  # it agreed on a WRONG parent
        self.assertEqual(s.stats["refused: same name length, same spelling"], 1)

    def test_rule_a_does_not_depend_on_the_truth_being_known(self):
        """The same shape with a truth no universe holds is refused too."""
        members = [(n, self.member(n, "Zzzzz"), ("uint8",)) for n in ("A", "B")]
        found = siblings_for().establish(members)
        self.assertEqual((found.established, len(found.refused)), ({}, 1))

    def test_different_name_lengths_under_one_wrong_spelling_do_not_agree(self):
        members = [(n, self.member(n, "int32"), ("uint8",)) for n in ("A", "Bb")]
        s = siblings_for()
        found = s.establish(members)
        self.assertEqual((found.established, found.refused), ({}, {}))
        self.assertEqual(s.stats["candidate agreements"], 0)

    def test_a_compensating_misplacement_is_refused(self):
        """`uint32` read as `uint64` beside `int32` read as `int64`, names one
        character apart: wrong in the same place and the same way."""
        members = [("Abc", self.member("Abc", "uint32"), ("uint64",)),
                   ("Defg", self.member("Defg", "int32"), ("int64",))]
        s = siblings_for()
        found = s.establish(members)
        self.assertEqual(found.established, {})
        self.assertEqual(s.stats["refused: an alternative pair deviates identically"], 1)

    def test_a_sibling_seed_decides_what_the_known_seeds_could_not(self):
        """No chains: 249 is untestable in tier 1, and a mismatch once its
        siblings give back the parent."""
        c = checker(chains=())
        s = siblings_for(c)
        ids = {("/S.T:Fn", n, h, ck): identity("/S.T:Fn", n, h, ck) for n, h, ck in (
            ("249", 0, 747197698), ("Translation", 1, 2235276067), ("Scale3D", 2, 2983776962))}
        r = resolver(entries={("/S.T:Fn", n): "VectorDouble" for n in ("249", "Translation", "Scale3D")})
        tier1 = cct.check_identities(ids, r, checker(chains=()))
        self.assertEqual(tier1.by_verdict["untestable"], 3)
        report = cct.check_identities(ids, r, c, s)
        self.assertEqual((report.by_verdict["match"], report.by_verdict["mismatch"]), (2, 1))
        (bad,) = report.mismatches
        self.assertEqual((bad["name"], bad["detail"]), ("249", "FQuat"))
        self.assertTrue(bad["seed"].startswith(f"sibling seed {cct.chain_checksum(TRANSFORM)} "))

    def test_a_sibling_seed_never_turns_untestable_into_a_match(self):
        c = checker(chains=())
        ids = {("/S.T", n, h, ck): identity("/S.T", n, h, ck) for n, h, ck in (
            ("Health", 1, self.member("Health", "float")),
            ("SourceID", 2, self.member("SourceID", "FName")),
            ("Mode", 3, self.member("Mode", "TEnumAsByte<EMode>")))}
        r = resolver(entries={("/S.T", "Health"): "Float", ("/S.T", "SourceID"): "FName",
                              ("/S.T", "Mode"): "EnumByte"})
        report = cct.check_identities(ids, r, c, siblings_for(c))
        self.assertEqual((report.by_verdict["match"], report.by_verdict["untestable"]), (2, 1))

    def test_the_chains_are_re_derived_and_a_disagreement_is_counted(self):
        """With the chains known, the sibling tier -- which never sees them --
        must find the same parent. Then plant a pair that makes a chain
        member imply ANOTHER established parent: that must be counted."""
        c = checker()
        ids = {("/S.T:Fn", n, h, ck): identity("/S.T:Fn", n, h, ck) for n, h, ck in (
            ("Translation", 1, 2235276067), ("Scale3D", 2, 2983776962))}
        r = resolver(entries={("/S.T:Fn", n): "VectorDouble" for n in ("Translation", "Scale3D")})
        report = cct.check_identities(ids, r, c, siblings_for(c))
        self.assertEqual(report.chain_rederived["re-derived"], 1)
        # the chain decided them first, and keeps them
        self.assertEqual({row["seed"] for row in report.rows}, {"Transform:FTransform"})

        # `bFlag` is a bool under the Transform chain; read as uint8 it implies
        # parent `other`, and two unrelated floats are made to agree on it.
        seed = cct.chain_checksum(TRANSFORM)
        flag = cct.compatible_checksum("bFlag", "bool", 0, seed)
        other = cct.implied_parent(flag, "bFlag", "uint8")
        ids = {("/S.U", n, h, ck): identity("/S.U", n, h, ck) for n, h, ck in (
            ("bFlag", 1, flag),
            ("Health", 2, cct.compatible_checksum("Health", "float", 0, other)),
            ("SourceID", 3, cct.compatible_checksum("SourceID", "float", 0, other)))}
        r = resolver(entries={("/S.U", "bFlag"): "Bool", ("/S.U", "Health"): "Float",
                              ("/S.U", "SourceID"): "Float"})
        report = cct.check_identities(ids, r, c, siblings_for(c))
        self.assertEqual(report.chain_rederived["DISAGREE: a sibling seed other than the chain's"], 1)

    def test_one_property_declared_twice_is_one_witness(self):
        """`bFlag` as a bool in one build and a uint8 bitfield in another,
        under one parent: both declarations imply it, but they are one
        property, not two members, so they establish nothing."""
        members = [("bFlag", self.member("bFlag", "bool"), ("bool", "uint8")),
                   ("bFlag", self.member("bFlag", "uint8"), ("bool", "uint8"))]
        found = siblings_for().establish(members)
        self.assertEqual((found.established, found.refused), ({}, {}))

        # Beside a second member, its two declarations still never pair with
        # each other: here every two-name pair is refused, so nothing is left.
        class RefuseAcrossNames(cct.SiblingSeeds):
            def ambiguous(self, first, second):
                return "refused for the test" if first[0] != second[0] else None

        members.append(("Health", self.member("Health", "float"), ("float",)))
        found = RefuseAcrossNames(()).establish(members)
        self.assertEqual(found.established, {})
        self.assertEqual(list(found.refused), [self.PARENT])

    def test_a_disagreement_between_the_tiers_fails_the_run(self):
        report = cct.Report(identities=1)
        self.assertEqual(cct.exit_status(report, [])[0], 0)
        report.chain_rederived[cct.DISAGREE] = 1
        code, message = cct.exit_status(report, [])
        self.assertEqual(code, 1)
        self.assertIn("disagree", message)
        self.assertEqual(cct.exit_status(cct.Report(), [])[0], 1)

    def test_the_ambiguity_universe_holds_every_spelling_the_tool_can_claim(self):
        """An agreement is weighed against every spelling the mismatch search
        itself could report; a smaller universe would let an agreement stand
        that one of the tool's own alternatives explains."""
        objects = cct.object_spellings(("Actor", "Foo_C"))
        universe = set(cct.spelling_universe(objects))
        claimable = set(cct.ALTERNATIVE_TYPES) | set(objects)
        for spec in cct.CPP_TYPES.values():
            claimable |= set(spec.expected)
        claimable |= set(cct.QUANTIZE_SPELLINGS.values()) | set(cct.SERIALIZED_INT_SPELLINGS.values())
        self.assertEqual(claimable - universe, set())
        s = cct.SiblingSeeds(cct.spelling_universe(objects))
        self.assertIn("afoo_c*", s.universe)

    def test_deviation_names_where_and_how(self):
        self.assertEqual(cct.deviation(3, "uint64", "uint32"), (7, (ord("6") ^ ord("3"), ord("4") ^ ord("2"))))
        self.assertEqual(cct.deviation(4, "int64", "int32"), (7, (ord("6") ^ ord("3"), ord("4") ^ ord("2"))))
        self.assertIsNone(cct.deviation(3, "int32", "uint32"))
        self.assertIsNone(cct.deviation(3, "FVector", "fvector"))


def write_export(root: Path, name: str, groups, build="++Ares-Core+release-13.06"):
    d = root / name
    d.mkdir()
    (d / "manifest.json").write_text(json.dumps({
        "replay_build": build,
        "net_field_export_groups": [
            {"path": path, "fields": [{"handle": h, "name": n, "compatible_checksum": c} for h, n, c in fields]}
            for path, fields in groups.items()]}), encoding="utf-8")
    return d


def run_main(*argv):
    out, err = io.StringIO(), io.StringIO()
    with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
        code = cct.main(list(argv))
    return code, out.getvalue(), err.getvalue()


# Identities typed by the committed tables in ways no pending change touches:
# `AresInventory.CorrectionIndex` (Int32, by name) and the engine reference
# `Owner` (ObjectNetGuid).
INVENTORY = "/Script/ShooterGame.AresInventory"
CORRECTION_INDEX = 3198546915
OWNER = cct.compatible_checksum("Owner", "AActor*")


class MainTests(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.root = Path(self._tmp.name)

    def tearDown(self):
        self._tmp.cleanup()

    def test_a_clean_export_exits_0_and_prints_its_zeros(self):
        d = write_export(self.root, "e", {INVENTORY: [(30, "CorrectionIndex", CORRECTION_INDEX),
                                                      (2, "Owner", OWNER)]})
        code, out, _ = run_main("--export", str(d))
        self.assertEqual(code, 0, out)
        self.assertIn("      2  match", out)
        self.assertIn("      0  mismatch", out)
        self.assertIn("      0  untestable", out)
        self.assertIn("      0  static-array element (index > 0)", out)
        self.assertIn(f"      0  {cct.ENUM_CAPABLE}", out)
        # the identity partition and the checksum_table.rs partition both
        # print their zero mismatch line
        self.assertEqual(out.count("      0  mismatch"), 2, out)
        self.assertIn("mismatches: 0 identities, 0 checksum_table.rs carriers", out)

    def test_a_type_the_checksum_contradicts_exits_1(self):
        seed = cct.chain_checksum(CORRECT)
        wrong = cct.compatible_checksum("CorrectionIndex", "uint32", 0, seed)
        d = write_export(self.root, "e", {INVENTORY: [(30, "CorrectionIndex", wrong)]})
        code, out, err = run_main("--export", str(d))
        self.assertEqual(code, 1)
        self.assertIn("MISMATCH /Script/ShooterGame.AresInventory | CorrectionIndex", out)
        self.assertIn("reproduces uint32 under AuthServerCorrectRepVariables", out)
        self.assertIn("FAILED", err)

    def test_nothing_typed_is_a_failure_not_a_pass(self):
        d = write_export(self.root, "e", {"/Game/Nothing.Nothing_C": [(1, "NoSuchField", 5)]})
        code, _, err = run_main("--export", str(d))
        self.assertEqual(code, 1)
        self.assertIn("nothing checked", err)

    def test_unreadable_input_exits_2(self):
        empty = self.root / "empty"
        empty.mkdir()
        self.assertEqual(run_main("--export", str(empty))[0], 2)
        self.assertEqual(run_main()[0], 2)

    def test_corpus_children_and_generated_siblings(self):
        write_export(self.root, "a", {INVENTORY: [(30, "CorrectionIndex", CORRECTION_INDEX)]})
        write_export(self.root, ".a.vrfkit-staging-1-2", {INVENTORY: [(30, "CorrectionIndex", 5)]})
        code, out, _ = run_main("--corpus", str(self.root))
        self.assertEqual(code, 0, out)
        self.assertIn("inputs: 1 export(s)", out)
        self.assertIn("1 generated sibling dir(s) skipped", out)

    def test_checkpoint_declarations_are_read_and_a_duplicate_group_key_refused(self):
        import pyarrow as pa
        import pyarrow.parquet as pq
        d = write_export(self.root, "e", {})
        groups = pa.table({"checkpoint_index": pa.array([0], pa.uint32()),
                           "ordinal": pa.array([0], pa.uint32()),
                           "group_path": [INVENTORY]})
        fields = pa.table({"checkpoint_index": pa.array([0, 0], pa.uint32()),
                           "group_ordinal": pa.array([0, 1], pa.uint32()),
                           "handle": pa.array([30, 31], pa.uint32()),
                           "compatible_checksum": pa.array([CORRECTION_INDEX, 1076231069], pa.uint32()),
                           "rendered_name": ["CorrectionIndex", "LastSeenClientCorrectionIndex"]})
        pq.write_table(groups, d / "checkpoint_export_groups.parquet")
        pq.write_table(fields, d / "checkpoint_export_fields.parquet")
        code, out, _ = run_main("--export", str(d))
        self.assertEqual(code, 0, out)
        self.assertIn("2 checkpoint declarations (1 exports carry them, 1 without a group)", out)
        self.assertIn("      1  match", out)
        pq.write_table(pa.concat_tables([groups, groups]), d / "checkpoint_export_groups.parquet")
        self.assertEqual(run_main("--export", str(d))[0], 2)

    def test_json_lists_every_classified_identity(self):
        d = write_export(self.root, "e", {INVENTORY: [(30, "CorrectionIndex", CORRECTION_INDEX)]})
        out_json = self.root / "report.json"
        self.assertEqual(run_main("--export", str(d), "--json", str(out_json))[0], 0)
        rows = json.loads(out_json.read_text(encoding="utf-8"))["identities"]
        self.assertEqual([(r["name"], r["verdict"], r["seed"]) for r in rows],
                         [("CorrectionIndex", "match",
                           "AuthServerCorrectRepVariables:FInventoryServerCorrectRepVariables")])


if __name__ == "__main__":
    unittest.main()
