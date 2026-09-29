# Descriptor adoption validation, 2026-09-23 to 2026-09-28 [ARCHIVED -- HISTORICAL]

Measurements recorded when candidate descriptor declarations were adopted into
vrfkit or declined, in three batches: build 13.06 support and two ability
arrays (2026-09-23), reveal projectiles and player effect targets
(2026-09-24), and Raze and the Warden (2026-09-28). Each section keeps its own
date, starting commit, sample and binary, and every figure is as first
recorded; none is a current result. Replay files, exports and run artifacts
stayed private and outside the repository. The current all-build audit is
[`../BUILD_VERIFICATION.md`](../BUILD_VERIFICATION.md).

## 2026-09-23: build 13.06 and two ability arrays

Starting commit `06807e718e052d0c820f6044439d66ccf233b5ba`; implementation
commit `4533ff77ad331a0899cfe3c8661bbc631e019a7a`. The update added:

- the 13.06 payload transform, with eleven golden vectors; all 88 vectors
  across eight builds pass, and the three S-box tables remain byte-identical;
- exact `MulticastSetPath.NetworkedProjectilePath` array children for
  Phoenix's flash path: elapsed seconds, location and velocity;
- declaration-qualified `BlindManagerComponent.ActiveBlinds` children,
  including IDs, references, flags and durations;
- nine exact group/name/checksum smoke scalar identities.

A Breach `ExitResults` candidate was deferred: no Breach group or
`ExitResults` payload occurs in 16,165,103 surveyed main/checkpoint field
rows, and exact-consumption validation needs a real payload. The update
exports measured wire values; it does not promote temporal or spatial
associations to guaranteed flash, nearsight, cast or hit events.

The historical 714-replay totals were not remeasured. The sample is twelve
preserved replays, plus an existing 13.01 export used for raw evidence only;
values were checked against golden vectors and independent Python decoders.
The later full audit found and fixed two ActiveBlinds cases missing from this
sample: an extra zero trailer on an empty delta, and a one-byte null
`CausingActor` reference. See its
[resolved findings](../BUILD_VERIFICATION.md#resolved-findings).

### 13.06 replay validation

The six preserved older fixtures cover 12.10, 12.11, 13.00, 13.02, 13.04 and
13.05, one replay each. All six were validated and exported both normally and
with `--checkpoints`. The six available 13.06 replays were validated and
exported with `--checkpoints`. Every invocation exited successfully.

The 13.06 framing baseline is committed in
[`tools/baselines/build_1306.json`](../../tools/baselines/build_1306.json):
3,369,208 content blocks, 2,476,146 property fields, 1,869,797 RPCs,
zero malformed blocks, and a 100.000000% framing oracle rate on every replay.
The baseline also retains the nonzero skipped-bit counters; framing success
does not mean that every payload has a known meaning.

Across all twelve replays, the checkpoint-enabled decode guard reports:

| Measurement | Result |
|---|---:|
| Main overlay values decoded | 7,481,715 |
| Main overlay errors | 0 |
| Main struct blobs decoded / failed | 1,709 / 0 |
| Checkpoint overlay values decoded / errors | 590,387 / 0 |
| Checkpoint struct blobs decoded / failed | 1,701 / 0 |
| Checkpoint array / truncated RPC / movement failures | 0 / 0 / 0 |

There are 75 existing opaque empty reward variants in the main stream; they
remain explicit. Non-null values and preservation were checked separately
from these counters.

Before/after comparison preserved all 14,516,354 prior main/checkpoint field
rows, including identical raw columns and every one of the 10,557,328 prior
non-null typed values. It adds 716,450 array-child rows and types 1,893
previously null scalar rows. Of 132 non-field Parquet files, 126 are
byte-identical. The six changed files are the 13.06 `checkpoint_blocks`
tables: only `field_row_start` and `field_row_count` change to describe added
children. All other columns and block counts are identical, ranges remain
contiguous, and their totals equal the new checkpoint field-row counts.
The six older normal exports also preserve every prior row/value and every
non-field Parquet byte; they add 3,681 array children and type 295 existing
rows. These are the same six replays, so they are not added to the
twelve-replay totals above.

The older baseline binary is `06807e7`. Because that version cannot decode
13.06, the new-build baseline is an isolated checkout of the same commit with
only the 13.06 transform added. This separates transform support from added
typing and measured-array routes. The candidate enables the previously
validated array routes on 13.06 as well as the two new routes. Their
additional 13.06 rows must not all be attributed to the two new abilities.

### Independent typed-value evidence

The two ability routes require exact parent identities and observed build
scope. ActiveBlinds additionally checks each member declaration's handle,
name and checksum; path members use the pinned PathPoint descriptor, not
unrelated sibling parameter declarations. Unexpected widths, unknown
handles, malformed or missing terminators and unconsumed bits prevent
promotion. Parent raw payloads remain available on success and refusal.
Regression tests cover zero-width unknown members and counted declaration
refusal with raw-parent retention, plus missing path members and non-finite
elapsed time through the actual RPC sink.

Across six 13.06 candidate exports, the independent Python decoder matched:

| Route | Parents | Elements | Typed children matched |
|---|---:|---:|---:|
| ActiveBlinds | 335 | 169 | 1,521 |
| NetworkedProjectilePath | 50 | 800 | 2,400 |

The first 13.06 replay contains neither route; the validator reports that
absence. On the 13.05 fixture it also matched 630 blind children and 960 path
children; on 13.02 it matched 603 blind children and 1,488 path children.
Comparisons cover child path/context, raw bytes and every typed
value, including FName strings, packed references, floating values and vectors.
The tool checks main fields; it does not claim checkpoint samples of an
ability merely because checkpoint decoding succeeded.

Nine smoke scalar identities add five `CreatedByCharacter: ObjectNetGuid`
and four `bInPersistentData: Bool` entries. All 1,893 matching rows in the
twelve candidate replays changed from null to a typed value equal to an
independent exact decode, with no missing identity, decode failure or
mismatch. The broader raw audit found 2,105 matching rows including 212 in the
existing 13.01 export. Those 13.01 rows were not freshly re-exported: no
permitted source replay was available for this run. That existing export also
contains neither of the two new ability-array routes. Detailed per-identity
evidence is in
[`scoped_type_evidence.json`](../../tools/fixtures/scoped_type_evidence.json).

### 13.06 reproduction

Pass local paths for the private inputs.

```powershell
cargo +1.86.0 build --release -p vrfkit --locked
$exe = "<candidate-vrfkit.exe>"
$corpus = "<preserved-corpus-directory>"
$env:VRFKIT_JOBS = "2"
python -W error tools/validate_corpus.py $exe $corpus --recursive
python -W error tools/check_decode_errors_corpus.py $exe $corpus --recursive --jobs 2 --checkpoints
python -W error tools/check_corpus_baseline.py --baseline tools/baselines/build_1306.json --exe $exe --corpus "$corpus/build_1306" --require-input
& $exe validate "<replay.vrf>"
& $exe export "<replay.vrf>" --out "<export-directory>" --checkpoints
python -W error tools/validate_ability_array_evidence.py "<export-directory>" --compare-typed --require-routes
python -W error tools/validate_type_evidence.py "<export-root>" "<nine-new-scalar-specifications.json>" --compare-typed
```

The scalar specification is the nine new entries selected from the scoped
evidence fixture, passed as a JSON list. Export-root validation must select
only one export per replay to avoid counting normal/checkpoint duplicates.
Run the ability validator over several directories together if a single
replay does not contain both routes.

A private checkpoint export baseline for 13.06 sample
`a4b7406f-e3f2-4831-9111-1456e62871e1` was created with
`check_export_baseline.py --update`, then checked again without `--update`.
All thirteen Parquet hashes and fourteen counter/file identities matched.

Eleven 13.06 golden vectors are included in the 88-vector transform test.
After the last failure-accounting fix, the final binary again passed the
twelve-replay checkpoint decode guard and the private 13.06 export hash
baseline.

## 2026-09-24: reveal projectiles and player effect targets

Starting commit `7ae8efb9f8dfe328f9773e76b040cab7e6ffc518`. The update added
five exact reveal descriptor paths (nine named and twelve explicit-handle
overlay entries) and typed Fade's reveal projectile `ReplicatedMovement` with
byte-component rotation; Sova's projectile already used that decoder.
`extract_player_effects.py` now admits a player target for blind updates and
continuous-effect observations (including nearsight) only through a
`SpawnedCharacter` identity -- the manifest's value, or an earlier value of
the same field that a reconnect replaced -- and not through possession or
ownership. Device observations remain available as source evidence but do not
increment the player totals.

This is an observation view. There is no cast correlation, unique-hit
deduplication, inferred explosion time or effect-interval join. Reveal
descriptors do not identify revealed enemies or reveal duration. Original raw
fields remain intact.

### Reveal replay evidence

Twelve preserved replays were freshly exported with `--checkpoints`: six on
13.06 and one each on 12.10, 12.11, 13.00, 13.02, 13.04 and 13.05. Comparisons
use the matching 2026-09-23 candidate exports from
`4533ff77ad331a0899cfe3c8661bbc631e019a7a`. The existing 13.01 export is
excluded from these fresh-validation totals.

| Output change | 13.02 | 13.06 sample 03 | 13.06 sample 06 | Total |
|---|---:|---:|---:|---:|
| Fade movement rows: raw-only to typed JSON | 649 | 1,146 | 167 | 1,962 |

All 1,962 new values match an independent Python bit decoder, including full
payload consumption, location, rotation, velocities, flags and optional frame
fields. Choosing short-component rotation instead fails exact consumption on
832 of those payloads. All 1,559 existing Sova movement values also match that
independent decoder; all 1,090 Owner/Instigator values across the five exact
reveal group paths match independent packed-GUID decoding. No matching reveal
payloads occur in the checkpoint field tables in this sample. These values
predate the per-class `ReplicatedMovement` location level of 2026-09-28.

Only the three `fields.parquet` files above change. Every row, raw byte and
column except the new Fade `value_str` values is identical. All other 153
Parquet files, including every checkpoint table, are byte-identical. The
checksum generator was run on all twelve fresh manifests: existing mappings
agree and the conflicting movement checksum remains excluded. Its 91
additional donors from unrelated fields are outside this change and were not
imported.

The effect tool observes 306 blind updates: 289 on confirmed player bodies
and 17 on other actors. Those 17 remain in the output. Their spawn classes
include Sova drones, Yoru decoys, Fade prowlers and other ability pawns.
These are replicated update counts, not unique flash hits. Effect paths are
reported without guessing whether every continuous effect is a debuff.

The framing guard passes 12/12 replays with 100% oracle rates and zero
malformed fields. The checkpoint decode guard reports zero overlay, struct,
array, truncated-RPC and movement failures: 7,483,677 decoded main rows and
590,387 decoded checkpoint fields. Existing partial/skipped data remains;
the framing totals still include 87,038,567 skipped bits.

### Reveal reproduction

The private before/after audit selects the five exact descriptor paths and
only Owner, Instigator and ReplicatedMovement, checks main and checkpoint
tables, and compares all Parquet columns before accepting a typed-only
difference.

```powershell
cargo +1.86.0 build --release -p vrfkit --locked
& $exe export $replay --out $export --checkpoints
python -W error tools/extract_player_effects.py --export $export --out player-effects.json
python -W error tools/validate_corpus.py $exe $corpus --recursive
python -W error tools/check_decode_errors_corpus.py $exe $corpus --recursive --jobs 2 --checkpoints
python -W error tools/check_export_baseline.py --baseline $baseline --replay $replay --out $guardExport --checkpoints --require-input
```

The 13.06 sample-03 export baseline changes only `overlay_decoded_ok`
(+1,146), `overlay_not_in_table` (-1,146), and the `fields.parquet` byte size
(+29,984) and hash, all explained by the Fade values. Its updated private
baseline is checked again without `--update`.

## 2026-09-28: Raze and the Warden

Starting commit `259ed10`. Evidence comes from all 1,018 unique replays of the
local corpus (builds 11.06 through 13.06; 308 of them contain Raze), exported
with checkpoints by `259ed10` and again by the candidate branch.

The candidate declarations covered five fields on four Raze ability classes,
four satchel attachment fields, the Boom Bot pawn's `bAIControlled` and
`ReplicatedMovement`, byte-rotation `ReplicatedMovement` on five Raze
projectiles, the force-module apply RPC's parameters,
`ClientResetRemoteMovementPrediction(isPossess)`, `Clay_PC_C.FocusProjectiles`
and the Warden's weapon class. Every declared field and function occurs in the
corpus. The declared handle numbers match the wire from 12.09 on; older builds
carry the same names at other handles, which the name-keyed identities do not
depend on. Declared fields that were already typed with the same types --
through the table, engine object references or checksum donors -- account for
1,716,934 main and 179,976 checkpoint rows in the Raze property groups.
Declared parameterless RPCs need no typing: every invocation is already
exported as a 0-bit row (69,375 `Multicast Event Triggered` alone).

The adopted types are exact group/name/checksum identities in
[`scoped_type_evidence.json`](../../tools/fixtures/scoped_type_evidence.json).
They apply to nothing else and are not checksum donors.
`generate_scoped_types.py` gained the five non-primitive shapes they need
(`EnumRemainingBits`, `RotationShort`, `VectorNetQuantize100`,
`RepMovementByte`, `RepMovementShort`), each with an independent decoder in
`validate_type_evidence.py`. `table.rs` was unchanged by this batch.

### Adopted Raze identities

Rows are all observed rows in the 1,018-replay corpus. Every payload was
consumed exactly by the independent decoder under the adopted type. Evidence
strings in the fixture carry each identity's widths, ranges and observed
builds.

| Group | Field | Type | Main rows | Checkpoint rows | Key evidence |
|---|---|---|---:|---:|---|
| 4 Raze ability classes (each) | `bInPersistentData` | Bool | 10,935 | 5,482 | 1 bit; true 5,468, false 5,467 per class |
| 4 Raze ability classes (each) | `CreatedByCharacter` | ObjectNetGuid | 687 | 5,136 | 681 resolve to a `Clay_PC_C` actor, 6 null, none unresolved |
| 4 Raze ability classes (each) | `CosmeticRandomSeed` | Int32 | 719 | 5,482 | 32 bits; 719 of 719 distinct within their replays |
| 4 Raze ability classes (each) | `AttachComponent` | ObjectNetGuid | 730 | 5,482 | 724 resolve to the character's `CollisionCylinder`, 6 null |
| 4 Raze ability classes (each) | `RelativeScale3D` | VectorNetQuantize100 | 719 | 5,482 | 31 bits, (1,1,1) on every row |
| Satchel projectile | `AttachComponent` | ObjectNetGuid | 4,833 | 0 | world geometry components, 7 character capsules, 2 null |
| Satchel projectile | `RelativeScale3D` | VectorNetQuantize100 | 4,831 | 0 | non-unit only on scaled world geometry (`Cube` 150 of 150) |
| Satchel projectile | `LocationOffset` | VectorNetQuantize100 | 4,831 | 0 | 40 to 70 bits |
| Satchel projectile | `RotationOffset` | RotationShort | 4,760 | 0 | exactly 3/19/35/51 bits |
| Boom Bot pawn | `bAIControlled` | Bool | 2,296 | 0 | true on every row |
| Boom Bot pawn | `ReplicatedMovement` | RepMovementShort | 2,296 | 0 | first update a median 0.05 cm from spawn |
| Force-module apply RPC | `ModuleType` | EnumRemainingBits | 665,519 | 0 | 3 bits; equals the remove RPC's value on 647,381 of 647,381 paired handles |
| Force-module apply RPC | `Module` | ObjectNetGuid | 665,519 | 0 | all resolve to 39 `ForceModule_*` classes |
| Force-module apply RPC | `Character` | ObjectNetGuid | 665,519 | 0 | all resolve to 43 character or pawn classes |
| Force-module apply RPC | `Source` | ObjectNetGuid | 46,400 | 0 | 36,260 resolve to classes (satchel explosion 7,322), 8,215 to actors with no class path, 1,925 unresolved |
| Force-module apply RPC | `Duration` | Float | 170,158 | 0 | 0.15 to 10.75 |
| Force-module apply RPC | `NetTimestamp` | Float | 665,519 | 0 | 0 to 252.7 |
| Force-module apply RPC | `RespawnNumber` | Int32 | 665,519 | 0 | 0 to 38 |
| `ShooterCharacter` possession reset | `isPossess` | Bool | 291,346 | 0 | true on every row, 48 cache groups |

`isPossess` is wider than the candidate declaration. The wire declares one
native `ShooterCharacter` parameter group, so the identity covers every
character and pawn that sends the RPC: Raze's character and the Boom Bot
account for 8,511 of the 291,346 rows, and the other 282,835 follow from the
wire identity rather than from a choice. The apply RPC's `HandleNumber` and
`SourceLocation` were already typed with the declared types (`HandleNumber`
has since been retyped `UInt32`: its checksum reproduces only as `uint32`; see
`tools/apply_type_corrections.py`).

Integration note (2026-09-28, `auto/integration-20260928`): the rows above
were measured under this review's scoped identities. The integrated tree
types `Module`, `ModuleType`, `Character`, `NetTimestamp` and `RespawnNumber`
by name in the overlay table (`tools/apply_type_corrections.py`), with the
same types except `ModuleType`, which the table reads as `EnumByte` rather
than `EnumRemainingBits` -- the same value on every payload measured here,
all of them 3 bits. The table resolves first, so the five scoped entries
could never be read and were removed from
`tools/fixtures/scoped_type_evidence.json`; a test now fails if any scoped
identity is shadowed that way. `Source` and `Duration` stay scoped.

Object references were resolved against the same export's `actors.parquet`
(dynamic actors) and `net_guids.parquet` (static paths), main rows only. The
force-module pairing reads `NetMulticastRemoveForceModule.ModuleType`
independently from its raw bits; that parameter shares the checksum but is not
typed by this change. 2,096,123 remove calls name a handle whose apply is not
in the stream.

### Declined: projectile `ReplicatedMovement`

The Raze satchel, Paint Shells and rocket payloads are consumed exactly with
byte rotation, as declared; short rotation fails 183,677 of the 351,709. (The
Boom Bot is the reverse: short consumes all 2,296, byte fails 1,110.) They
stayed raw because `FieldType::RepMovement` then read every location at scale
100, and these classes replicate whole centimetres. Exact consumption cannot
catch that: the scale changes no width.

Method: for every actor, its first `ReplicatedMovement` against its
`actors.parquet` spawn location, which the field decoder never touches, over
all 1,018 replays.

| Groups | Actors | Median distance as decoded | Median distance times 100 | Within 1 m |
|---|---:|---:|---:|---|
| Raze projectile candidates (5) | 29,972 | 6,505-7,051 cm | 0.49-0.51 cm | all, times 100 |
| Raze Boom Bot | 2,296 | 0.05 cm | 6,626 m | all, as decoded |
| Already typed, non-pawn (24) | 425,146 | 5,816-7,601 cm | 0.41-0.51 cm | all, times 100 |
| Already typed, pawn (`Pawn_Aggrobot_SeekerNade_C`) | 932 | 0.05 cm | -- | all, as decoded |

The separation is complete: every actor of every non-pawn group lands within
1 m of its spawn only after multiplying by 100, and no actor of the two pawns
does. On two replays, the speed reported by Raze's satchel, grenade and rocket
and by Sova's Recon Bolt matches the speed implied by consecutive centimetre
positions (median ratio 0.98 to 1.02). The discriminator is the class, not the
rotation width: five of the 24 affected groups use short rotation.

The Paint Shells and their spawner under the 11.06-12.09 paths
(`Projectile_Clay_4_ProjectilePrimary_C`, `..._ProjectileSecondary_C`,
`..._SecondarySpawner_C`), which no candidate names, measure the same way:
byte rotation consumes all 32,113 `ReplicatedMovement` rows (8,385 / 23,336 /
392), and every one of the 2,352 actors lands within 1 m of its spawn only at
100 times the decoded location.

The fixed scale was a defect in shipped output too, on the already-typed
groups above. Integration note (2026-09-28): the per-class level has landed.
`FieldType::RepMovement` now carries each class's measured location level, so
the reason for this decline no longer holds; the five projectiles stay raw
only because nothing types them yet, and typing one needs its spawn-join line
in `REP_MOVEMENT_LOCATION_EVIDENCE`. The Boom Bot's scoped entry states two
decimals, re-measured at integration (2,296 joins, 18 builds, ratio
99.9986-100.0014). DATA.md's metre reading is superseded by
[its per-class section](../DATA.md#replicatedmovementlocation-is-world-units-at-a-per-class-level).

### Deferred: `FocusProjectiles`

A candidate declaration gives `Clay_PC_C.FocusProjectiles` as a RepLayout
dynamic array of one object reference. The descriptor generator of the time
mapped any RepLayout dynamic array to `Raw`, so adopting the declaration would
have typed nothing; typing it needs a measured array route in the export
sink. The evidence for one exists. Parsed as capacity, index, handle, width
and payload, 24,409 of the 25,197 main payloads are consumed exactly. Every
element carries the declared `FocusProjectiles` member (handle 46, or 48
before 12.05, checksum 831657485), and all 12,837 elements resolve to a Raze
actor: satchels 5,360, Paint Shells 4,534, Boom Bots 2,351 and rockets 592.
The other 788 payloads are empty arrays followed by one zero byte
(`02 00 00`, `04 00 00`), the trailer shape the ActiveBlinds and
TrackedRewards routes had to admit explicitly. The route, its checkpoint row
ranges and that trailer are left for a separate change; it is listed under
[remaining work](../FOLLOWUP.md#remaining-work).

### Warden

`BattleRifle.BattleRifle_C` appears only in 13.06: 837 weapon instances in 37
of the 38 13.06 replays. 841 `MulticastNotifyDamage_*.EquippableUsed`
references name one of those instances, and 215
`KillData[*].KillingEquippableClass` values resolve to `BattleRifle_C`. The
previous equippable table named none of them; the updated one maps the class
path, its package path and `Default__BattleRifle_C` to Warden.
`KillingEquippableClass` resolves to a bare `BattleRifle_C` path in
`net_guids`, which, as for every other weapon, the table does not key
directly.

### Raze replay validation

Both corpus runs used `tools/verify_build_corpus.py`: validation, checkpoint
export and the strict counter gate, on all 1,018 unique replays. The branch
run (binary built from this change's sources) passes 1,018 of 1,018, like the
`259ed10` run.

**Before and after, whole corpus.** Every export was compared file by file.
`fields.parquet` (1,416,263,689 rows) and `checkpoint_fields.parquet`
(389,606,257 rows) keep the same rows in the same order with every non-value
column equal. Value columns change only from null to a value, and only on the
adopted identities: 3,914,506 main and 108,256 checkpoint rows, 4,022,762 in
all, the same count per identity as the raw audit found. All 11,198 other
Parquet files are byte-identical. In every manifest the only counters to move
are `overlay_decoded_ok` and `overlay_not_in_table`, by that same count
(checkpoint counters move in the 308 replays with Raze).

**Independent values.** `validate_type_evidence.py --compare-typed` over the
branch exports decodes all 4,022,762 rows of the adopted identities, main and
checkpoint, with no failure, and every exported value equals its independent
decode. The five declined projectile identities remain untyped on all 351,709
of their rows.

**Checksum step.** `extract_checksum_types.py --check` on 18 fresh Raze
exports, one per build that has Raze, and the tool's own `learn`/`reconcile`
over all 1,018 fresh manifests: 0 disagreeing, 0 ruled out, 0 ambiguous. The
115 checksums a write run would add are the same unrelated set as before this
change; they were not imported, as unrelated donors were not on
[2026-09-24](#reveal-replay-evidence). Scoped identities are not donors, so
`checksum_table.rs` is unchanged.

**Public fixtures.** The three public 12.10, 12.11 and 13.00 replays CI
audits contain no Raze and no force-module apply RPC, so CI's real-bytes type
check cannot cover these identities; they rest on the local corpus run above.
The Python and Rust tests carry recorded payload vectors instead.

**A/B against `259ed10`.** A private A/B script, not in this repository,
exported and validated each replay with both binaries, checkpoints on: three
per build folder plus `02d4d478` and the three public fixtures (66 replays),
then one Raze replay from each of the 18 builds that have one plus the 13.06
fixture `abf07066` (19 replays). In both runs every table other than
`fields.parquet` and `checkpoint_fields.parquet` is byte-identical, validation
verdicts and oracle lines are identical, and the only manifest keys to differ
are the two overlay counters. In those two tables the rows found only in the
`259ed10` output are exactly the newly typed rows, each matched by one row
only in the branch output: 252,996 main and 8,236 checkpoint rows in the first
run, 76,253 and 6,276 in the second, on all 34 identities. Every branch-side
Parquet file is byte-identical to the full-corpus export of the same replay
(858 of 858 and 247 of 247), so the committed binary's output is the audited
one.

**Baselines.** The export and checkpoint baselines of `02d4d478` move by
exactly the typed rows it contains, 3,209 of them: 560 each of `Character`,
`Module`, `ModuleType`, `NetTimestamp` and `RespawnNumber`, 147 `Duration`,
29 `Source` and 233 `isPossess`. `overlay_decoded_ok` goes from 796,920 to
800,129, `overlay_not_in_table` from 163,534 to 160,325, and `fields.parquet`
from 16,455,178 to 16,460,477 bytes with a new hash; its rows and every other
file are unchanged. The replay has no Raze. The per-build framing baselines do
not move.

### Raze reproduction

Inputs remain private. The scripts and their JSON outputs are kept outside the
repository. The two corpus exports they were computed from (the 1,018-replay
export at `259ed10` and the candidate re-export) were not kept; exporting the
same replays with `--checkpoints` regenerates them.

```powershell
cargo +1.86.0 build --release -p vrfkit --locked
python -W error tools/verify_build_corpus.py --exe target/release/vrfkit.exe `
  --corpus "<corpus>" --work-dir "<new directory>" --output audit.json --jobs 12
python -W error tools/validate_type_evidence.py "<export root>" "<specification>.json" --compare-typed
python -W error tools/generate_scoped_types.py --check
```

The validator specification lists each adopted identity under its exported
spelling -- property fields as they are, RPC parameters under the
`_ClassNetCache` group as `Function.parameter`, and `isPossess` once per cache
group that sends it.
