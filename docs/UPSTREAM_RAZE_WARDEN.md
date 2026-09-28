# Upstream Warden and Raze descriptors, 2026-09-28

The latest common all-build audit is [BUILD_VERIFICATION.md](BUILD_VERIFICATION.md).
This report keeps its own date and scope.

This review covers the two ValorantReplayParser commits after
[`2b66c65`](UPSTREAM_REVEALS.md):
[`2103d92`](https://github.com/michel-giehl/ValorantReplayParser/commit/2103d924c59a629fb3eaecd4dedf283b0267a761)
("feat: Add warden") and
[`8b7afcb`](https://github.com/michel-giehl/ValorantReplayParser/commit/8b7afcbb98bc4f8d4c342aef8242b142568624ca)
("feat: add raze descriptors"). The local starting commit is `259ed10`; the
implementation is on the branch that introduces this report. The previous
surveys are [UPSTREAM_PARITY.md](UPSTREAM_PARITY.md) and
[UPSTREAM_REVEALS.md](UPSTREAM_REVEALS.md).

Evidence comes from all 1,018 unique replays of the local corpus (builds
11.06 through 13.06; 308 of them contain Raze), exported with checkpoints by
`259ed10` and again by this branch. Replay files and exports stay private.

## Disposition

| Upstream change | Disposition | Reason |
|---|---|---|
| `2103d92`: `ValorantEquippableResolver` names `BattleRifle_C` the Warden (rifle) | Adopted | The one file is re-vendored byte for byte; `equippable_table.py` regenerated. 837 Warden instances in 37 of the 38 13.06 replays. |
| `8b7afcb` `ClayActorDescriptor`, four ability classes: `bInPersistentData`, `CreatedByCharacter`, `CosmeticRandomSeed`, `AttachComponent`, `RelativeScale3D` | Adopted with evidence | 20 exact group/name/checksum identities. |
| `8b7afcb` satchel projectile attachment: `AttachComponent`, `RelativeScale3D`, `LocationOffset`, `RotationOffset` | Adopted with evidence | 4 exact identities. |
| `8b7afcb` Boom Bot pawn: `bAIControlled`, `ReplicatedMovement` (short rotation) | Adopted with evidence | 2 exact identities; the location scale was checked against spawn positions. |
| `8b7afcb` `ReplicatedMovement` (byte rotation) on the satchel, the three Paint Shells projectiles and the rocket | Declined | The rotation width is right, but `RepMovement` would export these locations 100 times too small. See [below](#declined-projectile-replicatedmovement). |
| `8b7afcb` `RazeForceParameters` (`ForceModuleManagerComponent:NetMulticastApplyForceModule`) | Adopted with evidence | 7 exact identities. `HandleNumber` and `SourceLocation` were already typed with the same types. |
| `8b7afcb` `ClientResetRemoteMovementPrediction(isPossess)` on Raze and the Boom Bot | Adopted with evidence, wider than upstream | The wire declares one native `ShooterCharacter` parameter group, so the identity covers every character and pawn that sends the RPC: Raze's character and Boom Bot account for 8,511 of the 291,346 rows, and the other 282,835 follow from the wire identity rather than from a choice. Constant `true` in this corpus. |
| `8b7afcb` roles `215`/`216`, `Owner`, `Instigator`, `AttachParent`, `Controller`, `PlayerState`, the Boom Bot's timestamp, gravity, movement mode and `bReplicateMovement`, the satchel explosion's fields, and every other `ClayAgentDescriptor` handle except the array | Not applicable | Already typed, with the same types, through the table, engine object references or checksum donors: 1,716,934 main and 179,976 checkpoint rows in the Raze property groups. |
| `8b7afcb` `ClayAgentDescriptor.FocusProjectiles` (`RepLayoutDynamicArray`) | Deferred | Mapped to `Raw` even by faithful vendoring. Needs a measured array route; see [below](#deferred-focusprojectiles). |
| `8b7afcb` parameterless RPCs (`MulticastOnItemMovedToPersistentData`, `MulticastOnBoombaNoLongerActivatable`, `MulticastStopProjectile`, `Multicast Event Triggered`) | Not applicable | No parameters to type. Every invocation is already exported as a 0-bit row (69,375 `Multicast Event Triggered` alone). |
| `8b7afcb` `AddSubobjectClassPath("ForceModuleManager", ...)` | Not applicable | `vrf-schema` already maps the bare `ForceModuleManager` instance to its `_ClassNetCache`. |
| `8b7afcb` `TidalWaveClassNetCacheDescriptors` registers the apply RPC | Not applicable | The file is not vendored; vrfkit resolves the function from the replay's own export table, and the parameters are typed above. |
| `8b7afcb` catalog registrations and C# tests | Not applicable | The generator does not read registrations. The tests' recorded payloads are reused as Python and Rust vectors. |

Every field and function `8b7afcb` declares occurs in the corpus, so nothing
here rests on the descriptor alone. Its handle numbers match the wire from
12.09 on; older builds carry the same names at other handles, which the
name-keyed identities do not depend on.

## Observed but not declared by `8b7afcb`

The survey also saw fields in the same groups that neither commit names. They
stay as they were (raw unless something else already typed them); they are
listed so the absence reads as a decision, not an oversight. Rows are main /
checkpoint over the 1,018 replays.

| Group | Field | Rows | Note |
|---|---|---:|---|
| Raze Paint Shells and their spawner under the 11.06-12.09 paths (`Projectile_Clay_4_ProjectilePrimary_C`, `..._ProjectileSecondary_C`, `..._SecondarySpawner_C`) | `ReplicatedMovement` | 8,385 / 23,336 / 392 | Upstream names only the 13.01+ paths. Measured the same way: byte rotation consumes all 32,113, and every one of the 2,352 actors lands within 1 m of its spawn only at 100 times the decoded location |
| `Clay_PC_C` | `NumResetsForRespawn`, `bAllowCorpseMovement`, `bHidden`, `bShouldUseMeshMaterialManager` | 0 / 5,499; 24 / 80; 32 / 175; 17 / 325 | Not in `ClayAgentDescriptor` |
| `Clay_PC_C` cache | `UpdateHalosEvent.CommActionEnum`, `MulticastItemPickedUp.*`, `MulticastApplyLastSeenMinimapSnapshot.*` | 10,076; 4,750; 30 | Not in upstream's agent cache |
| Boom Bot pawn and cache | `bShouldUseMeshMaterialManager`, `MultiCastSetMinimapPulse.Pulsate` | 144; 874 | Not declared |
| Satchel projectile and cache | `AttachSocket`, `HideSatchelStuckToLocalPawn.108` | 6; 5 | Not declared |
| Force-module apply RPC | `ContextActor` (handle 9) | 7,990 | Upstream declares handles 0-8 only |
| Force-module remove RPC | `ModuleType` | 2,743,504 | Declared `EnumRemainingBits` by upstream `99d9646`, not by these commits; shares the adopted checksum and agrees with it on every paired handle, but is not typed here |
| `ForceModuleManagerComponent` | `AuthActivePredictedForceModules`, `NetMulticastEnforceEndOfLifeCleanup.ModulesCleanedUpByServer` | 61,969 / 52,575; 40,765 | Arrays; not declared |
| `Comp_Projectile_CosmeticPlayEffectUntilTriggerMarker` / `...SoundLoop` | unresolved cache payloads | 2,016; 838 | Bare instance names with no class; upstream's cosmetic descriptor covers the base component only |

## Why no `8b7afcb` descriptor file is vendored

`tools/compare_descriptor_sources.py` cannot extract upstream at `8b7afcb` at
all: `extract_descriptors.py` rejects five declarations in
`ClayAbilityDescriptors.cs` -- a `ReplicatedMovement` quantization chosen by
`this is ClayBoomBotPawnDescriptor ? ... : ...`, and four `AddPropertyHandle`
calls whose lambdas cast (`x => ((ClayBoomBotPawnDescriptor)x).Controller`).
Beyond those, the descriptors switch fields on and off per class with
`if (IsMoving)`, `if (IsAbility)` and `this is` tests. The generator attributes
every declaration in a `Configure` body to the declaring class and passes it to
every subclass, so even with the rejected shapes taught, all eleven Clay
classes would receive the satchel's attachment fields and the projectiles'
movement handle.

Wholesale re-vendoring is no better. Against `2103d92`, the last revision the
generator can read, the vendored input has 1,194 named and 96 explicit-handle
entries and upstream 941 and 272: 441 removals, 188 additions, 11 type changes
and 176 handle additions, and 549 committed table entries would be lost and 41
overwritten. `2103d92` itself changes no parsed field and no handle; its only
source difference is the resolver file adopted above.

Vendoring single files does not work either. Upstream's
`AgentClassNetCacheDescriptors.cs` now builds each agent's cache with
`CreateFunctions(agent)` instead of a `[...]` list; with only that file
replaced, the generator dropped all 29 agent `_ClassNetCache` entries and still
reported success. `ClayDescriptors.cs` builds its caches through
`private static ClassNetCacheDescriptor` helpers, which the factory check only
looked for when public or internal. Both shapes now fail generation instead
(`tools/extract_descriptors.py`, with a test for each). `RazeForceParameters.cs`
does parse, but its table entries would become checksum donors, and the
checksum generator has no selective import: a write run over this corpus would
also import the 115 unrelated donors the previous update declined.

So the adopted types are exact group/name/checksum identities in
[`scoped_type_evidence.json`](../tools/fixtures/scoped_type_evidence.json), the
mechanism [UPSTREAM_PARITY.md](UPSTREAM_PARITY.md) used for upstream's smoke
descriptors. They apply to nothing else and are not checksum donors.
`generate_scoped_types.py` gained the five non-primitive shapes these
declarations need (`EnumRemainingBits`, `RotationShort`,
`VectorNetQuantize100`, `RepMovementByte`, `RepMovementShort`), each with an
independent decoder in `validate_type_evidence.py`. `table.rs` is unchanged.

## Adopted identities

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
| Satchel projectile | `LocationOffset` | VectorNetQuantize100 | 4,831 | 0 | 40 to 70 bits; upstream's 64-bit vector reads (-782.71,-1366.59,5.8) |
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

Object references were resolved against the same export's `actors.parquet`
(dynamic actors) and `net_guids.parquet` (static paths), main rows only. The
force-module pairing reads `NetMulticastRemoveForceModule.ModuleType`
independently from its raw bits; that parameter shares the checksum but is not
typed by this change. 2,096,123 remove calls name a handle whose apply is not
in the stream.

## Declined: projectile `ReplicatedMovement`

The Raze satchel, Paint Shells and rocket payloads are consumed exactly with
byte rotation, as upstream declares; short rotation fails 183,677 of the
351,709. (The Boom Bot is the reverse: short consumes all 2,296, byte fails
1,110.) They stay raw because `FieldType::RepMovement` reads every location at scale 100, and
these classes replicate whole centimetres. Exact consumption cannot catch
that: the scale changes no width.

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

This is a defect in shipped output, not only in these candidates: the
24 typed groups include `EquippablePickupProjectile_C` (288,644 actors),
Sova's and Fade's reveal projectiles and the smoke projectiles and game
objects. Upstream's C#
`ReplicatedMovementDecoder` has the same fixed scale. Fixing it rewrites typed
values on those groups and every export baseline, so it is recorded in
[FOLLOWUP.md](FOLLOWUP.md#remaining-work) rather than folded into this change.
[DATA.md](DATA.md) documents the metre reading until then. A Rust test pins
the decline: a Raze projectile's `ReplicatedMovement` resolves to no type.

## Deferred: `FocusProjectiles`

Upstream declares `Clay_PC_C.FocusProjectiles` as a RepLayout dynamic array of
one object reference. The generator maps any `RepLayoutDynamicArray` to `Raw`,
so vendoring would type nothing; typing it needs a measured array route in the
export sink. The evidence for one exists. Parsed as capacity, index, handle,
width and payload, 24,409 of the 25,197 main payloads are consumed exactly.
Every element carries the declared `FocusProjectiles` member (handle 46, or 48
before 12.05, checksum 831657485), and all 12,837 elements resolve to a Raze
actor: satchels 5,360, Paint Shells 4,534, Boom Bots 2,351 and rockets 592.
The other 788 payloads are empty arrays followed by one zero byte (`02 00 00`,
`04 00 00`), the trailer shape the ActiveBlinds and TrackedRewards routes had to
admit explicitly. The route, its checkpoint row ranges and that trailer are
left for a separate change.

## Warden

`BattleRifle.BattleRifle_C` appears only in 13.06: 837 weapon instances in 37
of the 38 13.06 replays. 841 `MulticastNotifyDamage_*.EquippableUsed`
references name one of those instances, and 215
`KillData[*].KillingEquippableClass` values resolve to `BattleRifle_C`. The
previous table named none of them; the regenerated one maps the class path, its
package path and `Default__BattleRifle_C` to Warden. `KillingEquippableClass`
resolves to a bare `BattleRifle_C` path in `net_guids`, which, as for every
other weapon, the table does not key directly.

## Replay validation

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
change; they were not imported, as in [UPSTREAM_REVEALS.md](UPSTREAM_REVEALS.md).
Scoped identities are not donors, so `checksum_table.rs` is unchanged.

**Public fixtures.** The three public 12.10, 12.11 and 13.00 replays CI
audits contain no Raze and no force-module apply RPC, so CI's real-bytes type
check cannot cover these identities; they rest on the local corpus run above.
The Python and Rust tests carry upstream's recorded payloads instead.

**A/B against `259ed10`.** `ab_compare.py` exported and validated each
replay with both binaries, checkpoints on: three per build folder plus
`02d4d478` and the three public fixtures (66 replays), then one Raze replay
from each of the 18 builds that have one plus the 13.06 fixture `abf07066`
(19 replays). In both runs every table other than `fields.parquet` and
`checkpoint_fields.parquet` is byte-identical, validation verdicts and oracle
lines are identical, and the only manifest keys to differ are the two overlay
counters. In those two tables the rows found only in the `259ed10` output are
exactly the newly typed rows, each matched by one row only in the branch
output: 252,996 main and 8,236 checkpoint rows in the first run, 76,253 and
6,276 in the second, on all 34 identities. Every branch-side Parquet file is
byte-identical to the full-corpus export of the same replay (858 of 858 and
247 of 247), so the committed binary's output is the audited one.

**Baselines.** The export and checkpoint baselines of `02d4d478` move by
exactly the typed rows it contains, 3,209 of them: 560 each of `Character`,
`Module`, `ModuleType`, `NetTimestamp` and `RespawnNumber`, 147 `Duration`,
29 `Source` and 233 `isPossess`. `overlay_decoded_ok` goes from 796,920 to
800,129, `overlay_not_in_table` from 163,534 to 160,325, and `fields.parquet`
from 16,455,178 to 16,460,477 bytes with a new hash; its rows and every other
file are unchanged. The replay has no Raze. The per-build framing baselines do
not move.

**Sweep.** The MSRV 1.86 development sweep with the regression guards
(`sweep.sh --guards`: formatting, clippy with warnings denied, workspace
tests, all-target and all-feature checks, rustdoc, the offset probe, Rust and
Python Parquet interop, the 27 advertised feature configurations, ASCII and
generator checks, table regeneration, baseline schemas, the Python suite, the
full documentation check, a release build, both `02d4d478` baselines and the
seven per-build framing baselines) passes on the final commit, with 717 Rust
and 925 Python tests.

## Reproduction

Inputs remain private. The scripts, their JSON outputs and both corpus exports
are under `$env:LOCALAPPDATA/vrfkit/auto-20260928` (`audit-main`, and
`scratch-upstream-raze` for this change).

```powershell
python -W error tools/compare_descriptor_sources.py --baseline third_party/vrp `
  --candidate "<upstream clone>::2103d924c59a629fb3eaecd4dedf283b0267a761" `
  --downstream-table crates/vrf-decode/src/table.rs --output audit.json
cargo +1.86.0 build --release -p vrfkit --locked
python -W error tools/verify_build_corpus.py --exe target/release/vrfkit.exe `
  --corpus "<corpus>" --work-dir "<new directory>" --output audit.json --jobs 12
python -W error tools/validate_type_evidence.py "<export root>" "<specification>.json" --compare-typed
python -W error tools/generate_scoped_types.py --check
python -W error tools/extract_equippables.py --check
```

The validator specification lists each adopted identity under its exported
spelling -- property fields as they are, RPC parameters under the
`_ClassNetCache` group as `Function.parameter`, and `isPossess` once per cache
group that sends it.
