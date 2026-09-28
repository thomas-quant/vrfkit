# The replay's checksum as type evidence

`tools/check_checksum_types.py` checks the overlay's types against something no
descriptor, table or decoder in this repo contributed: the `compatible_checksum`
the replay declares for every field. Unreal computes it from the property's name
and its C++ type, so recomputing it from the type vrfkit decodes either
reproduces the declared value or it does not.

Every figure below was measured on 2026-09-28 against the tables at `9f92756`
unless it says otherwise.

## The formula

UE 5.3 `RepLayout.cpp`, `GetRepLayoutCmdCompatibleChecksum`:

```text
crc = StrCrc32(lower(name), parent)     every TCHAR fed as 4 bytes, low byte first
crc = StrCrc32(lower(cpp_type), crc)    "int32", "fvector", "aactor*", "tarray", ...
crc = MemCrc32(u32 LE static index, crc)
```

`FCrc::StrCrc32` and `MemCrc32` are the standard reflected CRC-32, so this is
`zlib.crc32` over the UTF-32LE bytes of the lower-cased strings, then the index.
`parent` is 0 for a class property or an RPC parameter. A member of a flattened
(non-NetSerialize) struct continues from its struct property's checksum, and a
`TArray`'s element from the array's -- the element has the array's name.

**Provenance.** The game-file analysis of 2026-09-28 measured the formula on the
installed 13.06 build: reading the cooked Blueprint classes, 194 Blueprint
fields declared by eight 13.06 replays reproduce by name and checksum, and
several native identities reproduce once their struct nesting is known. This
tool re-implements it; it does not import that code. Two cross-checks:

- **Against the analysis's implementation** (`gamemodel.py`, outside the repo):
  every checksum it computes while flattening the 13.06 classes and RPCs --
  13,046 of them -- was recomputed by `compatible_checksum` on the same inputs.
  0 differ. Both reproduce 194 of the 194 Blueprint fields and 92 of the 115
  Blueprint RPC parameters of those eight replays; the other 23 are enums and
  `FTransform` members, which that reader does not flatten.
- **Against a second implementation in the tests**: a CRC-32 table built in
  `tools/tests/test_check_checksum_types.py`, feeding each character as four
  bytes the way `FCrc::StrCrc32` does. It and the tool agree on 2,000 random
  inputs and on 29 checksums declared by real replays (top-level fields,
  `FTransform` members, struct members two to four levels deep, an array and
  its element, a bitfield bool). Each step -- the lower-casing, the UTF-32
  width, the index, the parent -- breaks every vector it touches when dropped,
  and no alternative type string reproduces any of them.

**Across builds.** The formula was measured on 13.06 declarations. On the
1,018-replay corpus it reproduces declared checksums on every one of the 24
builds, 11.06 through 13.06 (per-build counts are printed on every run), and a
property keeps its checksum across all of them -- `249` in the effect RPCs is
747197698 on each build that declares it.

## What the tool does

1. Reads every declared identity -- `(group, field name, handle, checksum)` --
   from each export's `manifest.json` and, when present, its checkpoint
   declaration tables.
2. Resolves the `FieldType` vrfkit gives it by parsing `table.rs`,
   `scoped_types.rs`, `checksum_table.rs` and the resolution constants in
   `overlay.rs`, in `overlay::resolve_entry`'s order.
3. **Tier 1.** Recomputes the checksum from the C++ spellings that `FieldType`
   can stand for (`CPP_TYPES`), under every known parent seed: 0, and the
   struct chains in `PARENT_CHAINS`.
4. **Tier 2.** Recovers further parent seeds from each group's own members
   (below) and re-tests what tier 1 left untestable under them.

| verdict | meaning |
|---|---|
| match | vrfkit's type reproduces the declared checksum |
| mismatch | vrfkit's type does not, but another spelling (`ALTERNATIVE_TYPES`, or an object pointer) does under the same seeds -- the checksum names a different type |
| untestable | nothing reproduces it; the checksum says nothing here, and it is never counted as a match |

Every recomputation is a 1-in-2^32 chance of an accidental reproduction, so the
tool counts them and prints `trials / 2^32`, the number of chance reproductions
to expect. On the corpus that is 0.0032 (13,685,166 recomputations).

It checks `checksum_table.rs` too. The table maps a checksum to a type with no
name, so each entry is recomputed under every name that declares its checksum.

### The resolver is vrfkit's

The resolution is a Python port, so it was checked against the Rust:

- **Against `resolve_field_type_with_checksum` itself**, through a scratch
  binary linked to `vrf-decode`: all 12,937 distinct identities the corpus
  declares resolve to the same `FieldType`, parameters included.
- **Against real exports**: on 7 exports (11.06, 12.05, 13.01, two 13.05, two
  13.06), main and checkpoint streams, every row that carries a checksum was
  mapped to its identity (an RPC parameter row to its parameter group, the way
  `sink/rpc.rs` finds it). All 15,114 (identity, export, stream) cells the
  resolver calls typed carry a `value_*` on every row; all 9,869 it calls
  untyped carry none. The only other values on checksum-carrying rows are 148
  `FloatValues` / `ObjectValues` / `VectorValues` cells, which the effect-blob
  decoder fills after the overlay declines them. None of the 1,127,553
  ClassNetCache function rows carries a checksum or a value, which is why the
  tool counts those groups' fields as function slots rather than properties.

## Parent chains (tier 1)

A member of a flattened struct cannot be recomputed without its parent's
checksum. `PARENT_CHAINS` names the struct properties above the members vrfkit
types -- a property name and a C++ type each, nothing more -- with where the
fact came from. Every chain is pinned by a declared checksum in the unit tests,
and the tool prints how many typed identities each one decided.

| chain | source |
|---|---|
| `Transform: FTransform` | UE 5.3 `FTransform`; the parameter name from the 13.06 executable's reflection data |
| `SpawnTransform: FTransform` | 13.06 reflection: `AAresGameStateBase::MulticastResetForRespawn` |
| `AttachmentReplication: FRepAttachment` | UE 5.3 `AActor` |
| `Handle: FForceModuleHandle`, `TimeStamp: FNetworkedMovementTimestamp` | 13.06 reflection: `UForceModuleManagerComponent` RPCs |
| `AuthServerCorrectRepVariables: FInventoryServerCorrectRepVariables` | 13.06 reflection: `UAresInventory` |
| `EffectID` / `CurrentEffectID: FEffectID` | 13.06 reflection: the effect RPCs |
| `ServerActiveEffects: TArray > FActiveEffectInfo` (and its `EffectID`, `Transform`) | 13.06 reflection: `UEffectManagerComponent` |
| `AuthBlindManagerState: FBlindManagerState > ActiveBlinds ...` | 13.06 reflection: `UBlindManagerComponent` |
| `FragmentInfo: FGroundVolumeFragmentArray > Items ...` | 13.06 reflection: `UGroundVolumeComponent` |

`HARDCODED_FNAMES` maps the bare FName indices the replay writes (`249` is
`Rotation`) the same way: UE 5.3 `UnrealNames.inl`, indices read back from the
13.06 executable.

## Sibling seeds (tier 2)

CRC-32 runs backwards: given the hashed suffix, the state it started from is
unique. So each member's (name, checksum) and a type for it imply exactly one
parent checksum (`implied_parent`). Members of one struct, each typed right,
imply the same parent; two differently named members implying the same one by
accident is a 1-in-2^32 event. That recovers a parent without knowing its name
or type, from the replay alone.

**The systematic false agreement.** A wrong type of the right length shifts the
implied parent by an amount fixed by where in the hashed string it is wrong and
how -- not by the name, not by the true parent. Two members wrong in the same
place and the same way still agree, at a wrong parent. The corpus has the
textbook case: `BombPlayerState`'s four GUID words `A`, `B`, `C`, `D` imply one
common parent under `int32`, `uint8`, `float` and `FName` alike -- every
five-character spelling -- because the names are all one character long. So
`SiblingSeeds` lets an agreement establish a parent only through a pair of
members that is

- **not** two names of one length given one spelling (refused whatever the
  truth is, since any truth of that length would agree the same way), and
- **not** wrong in the same place and the same way under any two spellings
  the tool knows -- every alternative, every `FieldType` spelling and every
  object pointer candidate (`deviation` keys may not intersect). This catches
  the second shape, `uint32` read as `uint64` beside `int32` read as `int64`
  with names one character apart.

A spelling of a different length cannot agree systematically at all: its shift
depends on the name's own CRC. Object references never establish a parent --
thousands of candidate spellings each would make an agreement a lottery -- but
are tested at the parents others establish. The residual blind spot is two
truths outside the tool's spelling universe that deviate identically from
their hypotheses at aligned offsets.

Tier 2 never sees `PARENT_CHAINS`, so it is also a cross-check of them. On the
corpus it re-derives 17 of the 247 chain seeds in use (one per group); 229 have
fewer than two members that agree -- `AttachmentReplication`'s members are
mostly `AttachParent` alone, an object reference -- and 1 group has fewer than
two hashable members. **0 disagree**: no member a chain decided implies a
different parent that siblings established. A disagreement fails the run,
because one of the two tiers would then be wrong about that member.

## Corpus result (1,018 replays, 24 builds)

`python tools/check_checksum_types.py --corpus <declaration corpus>`, where each
child holds one export's `manifest.json` and checkpoint declaration tables:

| | identities |
|---|---:|
| declared (group, name, handle, checksum) | 12,937 |
| ClassNetCache function slots | 1,084 |
| not typed by vrfkit (no resolution / Raw / Skip) | 6,631 / 172 / 4 |
| **typed (group, name, checksum, FieldType)** | **4,242** |
| match (top level 1,658, parent chain 282, sibling seed 388) | 2,328 |
| mismatch (all under a parent chain) | 17 |
| untestable | 1,897 |

Matches by rule: 742 exact spellings, 106 `Bool` as `uint8`, 1,480 object
references with their class. Untestable by reason: 1,597 enum-capable types,
236 object references whose class is not among the candidates (or nested
without a seed), 64 with no known parent seed. Tier 2 examined 792 groups:
124 candidate agreements, 124 established, 0 refused, from 47,604 implied-parent
comparisons (0.0000 chance agreements expected). `checksum_table.rs`: 458
checksums -- 344 match, 6 mismatch, 108 untestable, 0 without a carrier.

**The mismatches** -- each on every build that declares it:

| identity | vrfkit | the checksum says |
|---|---|---|
| `249` in the effect RPCs, `TransformTransitionContext`, `TransitionContext_Sequoia_X_TeleportInfo_C`, `StateContext_ActorTrailTargetingResult_C` (747197698), `MulticastResetForRespawn` (1874998526) | `VectorDouble` | `FQuat`: the X, Y, Z of `FTransform.Rotation`, not a vector or a rotator |
| `EffectID` in the effect RPCs and `EffectManagerComponent` (2340855891, 2251343646, 1129645208) | `UInt64` | `int64` |
| `HandleNumber` in `NetMulticast{Apply,Remove}ForceModule` (3336285386) | `Int32` | `uint32` |

The same three show up as the 6 `checksum_table.rs` entries (747197698,
1874998526, 1129645208, 2251343646, 2340855891, 3336285386). None changes a
decoded bit today: the quaternion's components are the three doubles already
decoded, and the integers are in range either way. What is wrong is the label a
consumer reads them by. That is exactly what this check is for, so **the tool
exits 1 on the corpus**, and it is deliberately not in the required sweep: an
allowlist that let it pass would be a check that cannot fail.

## What it cannot tell

- **Enums.** No spelling of a C++ enum (`TEnumAsByte<E>`, `E`, `E::Type`)
  reproduces, so `EnumByte`, `EnumRemainingBits`, a `Byte` that is an enum
  and a `SerializedInt` with an enum's bound are untestable. A plain `uint8`
  still matches, and a different type that reproduces is still a mismatch.
- **Object classes.** An object reference hashes as `A<Class>*` / `U<Class>*`.
  The candidates are the classes the input declares groups for plus
  `ENGINE_CLASSES`; a reference to anything else (a data asset) is untestable.
- **Nested members without a seed.** No chain, and fewer than two siblings
  that agree unambiguously: the parent is unknown. `A`/`B`/`C`/`D` above are
  this case -- under vrfkit's `UInt32` they imply four different parents,
  which rules `uint32` out for the four together, but the five-character
  truth they share cannot be told apart, so they stay untestable rather
  than becoming a mismatch with no named alternative.
- **`Bool` as `uint8`.** A bitfield bool hashes as its storage type, the same
  string as a byte; only the wire width separates them.
- **Format, scale and meaning.** A match says the declaration hashed this name
  and C++ type. It does not check how a NetSerialize type packs its bits, a
  quantization the type does not name, or what a value means.
- **Other typing paths.** Struct-blob decoders (`sink/blobs.rs`), array
  schemas and the effect-blob decoder are not overlay tables and are not
  checked. Array leaves are typed by the walker without their checksum; the
  tool resolves every declared identity with it, as the property and RPC
  paths do.
- **ClassNetCache checksums** are a different hash (functions, not
  properties) and are not recomputed.
