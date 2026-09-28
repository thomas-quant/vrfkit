# Extractable data

What you can get out of a VALORANT replay with vrfkit. With checkpoints the
export is thirteen Parquet tables plus `manifest.json`. Unknown property payloads and unresolved
whole RPCs retain raw bytes, but successfully decoded movement and synthesized
child rows may not duplicate their input bytes. "Untyped" is not synonymous
with "lost"; check stream-loss counters separately.

Legend: ✅ typed (value decoded) · ◐ raw or derivable · ❌ unavailable in the
stated observation scope. Absence in a sample is not proof of format-wide absence.

Current validation is [BUILD_VERIFICATION.md](BUILD_VERIFICATION.md): one
acceptance rule on 1,018 replays of 24 builds, with the
[resolved findings](BUILD_VERIFICATION.md#resolved-findings) (among them the
2026-09-25 ActiveBlinds fix). Structured-array children are admitted per build
and per route: every measured route on 13.01--13.06, only the routes that
matched and decoded cleanly on 11.06--13.00, where the other parents stay raw
([legacy route table](LEGACY_BUILD_SUPPORT.md#measured-array-routes-2026-09-28)).
The dated batches that added types and context are the phase reports --
[targeting and heal values](TARGETING_AND_HEAL_VALUES.md) (with the 2026-09-09
714-replay physical coverage), [schema expansion](SCHEMA_EXPANSION.md),
[partial-header correction](PARTIAL_HEADER_CORRECTION.md),
[semantic context](SEMANTIC_CONTEXT_EXPANSION.md) and
[checkpoint path resolution](CHECKPOINT_PATH_RESOLUTION.md) -- and the KillData
[observations](KILL_OBSERVATIONS.md) and [ledger](KILL_LEDGER.md). Physical
row ratios are not semantic completeness.

---

## Player identity

| Data | Source | Status |
|---|---|---|
| Account UUID (subject) | `manifest.players.subject` / `BombPlayerState.Subject` | ✅ |
| Character NetGUID | `manifest.players.character_net_guid` / `SpawnedCharacter` | ✅ joins movement 10/10 on 71 of 71 replays |
| Agent (characterId) | `manifest` game_specific_data.playerLoadouts | ✅ |
| Agent GUID, replicated | `BombPlayerState` / Swiftplay player state `A`, `B`, `C`, `D` (checksums 988169428, 943211507, 965590766, 1032080829) | ✅ four `UInt32` words of one FGuid, non-negative in `value_i64`, main and checkpoint. Formatted `%08x-%04x-%04x-%04x-%04x%08x` from (A, B>>16, B&0xffff, C>>16, C&0xffff, D), they equalled the header `characterId` on all 4,112 comparable sets in a 30-replay check (0 mismatches; the sampled 11.06-11.09 replay headers carry no loadouts). Names stay `A`..`D` on the wire; the byte-shaped `B` fields are separate properties |
| Two players on the same agent | disambiguated by `subject` (characterId alone can't) | ✅ |
| Display name / Riot ID | — | ❌ not established by the available evidence |
| `ProfileName` | PlayerState replicated FString | ✅ exact string decoded; its purpose is not established as a display name or Riot ID |

`SpawnedCharacter` is replicated again as 0 when a player disconnects, so the
rule is **last non-zero write wins** -- 0 is not a NetGUID. Last-write-wins lost
9 players across 5 replays with nothing looking wrong (64 of 69 replays joined
all ten, the worst 7); with the rule, 71 of 71 do.

**The manifest keeps one pawn per player, and a reconnect gives a player two.**
On 39c2bb2c (13.05) PlayerState 256 writes `SpawnedCharacter` 1510 at t=66, 0
at 1,851,838 and 45530 at 1,948,245; the manifest keeps 45530, and pawn 1510 --
open from t=66 to 1,851,642 -- is in no manifest field. A join on the manifest
alone labelled its 1,854 effect records `unconfirmed_actor`, in a run that
exited 0, and its spike custody `unknown`. The analysis tools therefore take player
bodies from every non-zero value of the field, through
`tools/player_identity.py`. Measured on the 1,018-export audit corpus (parser
259ed10, 2026-09-28; main `fields.parquet`, top-level `SpawnedCharacter` rows
on `BombPlayerState_C` and its Swiftplay alias): 10,426 rows, all typed, naming
10,250 pawns, each by exactly one PlayerState; 97 of them in 86 exports are
earlier values the manifest dropped; the manifest's value is the last non-zero
history value for every player. A pawn's own `PlayerState` is not a body
criterion: 1,236 pawns carry a manifest player's PlayerState without ever being
a `SpawnedCharacter` value, and every one is Astra's `Rift_TargetingForm_PC_C`.
Every named pawn's own top-level `PlayerState` names the PlayerState that
spawned it (10,250 of 10,250), which is the independent check on the join.

## Economy

| Data | Source | Status |
|---|---|---|
| Current credits | `fields` `MoneyManagementComponent.Money` | ✅ Int32 |
| Start-of-round credits | `StartOfRoundMoney` | ✅ |
| Total granted | `TotalMoneyGranted` | ✅ |
| Team loadout value | `BaseTeamState.LoadoutValue` / `AverageLoadoutValue` | ✅ |
| Per-round balance change | `StartOfRoundMoney` − `EndOfRoundMoney` (OwnerExclusivePlayerInfo) | ◐ net balance change; grants, rewards, refunds and purchases must be separated before calling it spend |
| K / D / A | `BasicCombatStatsComponent.Aggregate*` | ✅ Int32 cumulative counters |
| ACS (combat score) | `PlayerScoreComponent.Score / rounds` | ✅ Int32 cumulative score |

## Purchase and inventory observations

| Data | Source | Status |
|---|---|---|
| Reported item | `PurchasedItemComponent.Purchaseable` → `net_guids.path` / `equippable_table.py` | ◐ replicated item state; repeated rows do not establish separate purchases |
| Who bought it | `PurchasedItemComponent.PurchasingPlayerState` → `manifest.players.subject` | ✅ |
| Observation time | `fields.time_ms` of the state update | ✅ replication time, not a guaranteed transaction timestamp |
| Which round | `time_ms` vs `events.roundStarted` | ◐ derivable |
| Cost | Temporally matched credit changes and item evidence | ◐ ambiguous grants/refunds and unmatched candidates must remain explicit |
| Source (buy/ability/etc.) | `PurchasableTransactionSource` | ◐ partial (some rows) |
| Inventory slot → item | `ItemSlot.Contents`, `AresInventory.ItemSlots` | ✅ / ◐ (MultiItemSlot raw) |
| Charges purchasable this round | `EquipmentChargeComponent.TotalChargesAllowedToPurchaseThisRound` | ✅ |
| Inventory correction counters | `AresInventory.CorrectionIndex` / `LastSeenClientCorrectionIndex` | ✅ Int32; strictly increasing per inventory, `LastSeen` always below `Correction`. What they count is inferred from the names only |

Join `Purchaseable` to `net_guids`, `PurchasingPlayerState` to player identity,
and update time to the latest preceding round boundary. Fields arrive as
separate updates: assemble component state and distinguish initialization or
round-start re-emission from a new item transition. A complete purchase ledger
requires corroborating credit changes; state rows alone are not that ledger.
Not every credit decrease is a spend: the side switch at half time and in
overtime resets credits, and that reset replicates as an ordinary `Money`
write 7-10 ms after `switchTeams`, before any buy phase opens.
`extract_match_observations.py` keeps those decreases out of its purchase
evidence (measured in [FOLLOWUP.md](FOLLOWUP.md#typing-and-data-dictionaries)).

## Combat — kills & deaths

| Data | Source | Status |
|---|---|---|
| K / D / A | `fields` CombatReport nested array | ✅ multiset-identical to the C# parser (on 13.01 -- see below) |
| Kill log (killer/killed NetGUID) | `events.characterDeath` word0/word1 + `MulticastNotifyKilledEnemy` RPC | ✅ 132/132 on 13.01; 9,677/9,677 over 71 replays on 13.02 by the same two-source join; Event structural overlay exact on 109,126/109,126 chunks across 527 replays |
| Multikill level | `MulticastNotifyKilledEnemy.MultikillLevel` | ✅ single/double/triple/quad |
| Kill timeline | `events.characterDeath` time_ms | ✅ (recovers the +13 the C# parser lost) |

## Combat — damage

| Data | Source | Status |
|---|---|---|
| Damage dealt / received | CombatReport `DamageDealt` / `DamageReceived` | ✅ |
| Regional damage (head/body/leg) | `Interactions[].Regions[].Hits/Damage` | ✅ multiset-identical (on 13.01) |
| Wallbang | `bIsWallPen` | ✅ |
| Damage source (weapon, location, bone) | `MulticastNotifyDamage` (EquippableUsed, ImpactLocation, ImpactBone) | ✅ |
| Finisher effect on a kill | `MulticastNotifyDamage_{Point,Base}.DeathMontageEffectOverride` → `net_guids.path` | ✅ ObjectNetGuid; 0 (the null reference) except on some kills, where it names an `FXC_*_C` finisher effect class |
| Death-montage context | `MulticastNotifyDamage_{Point,Base}.DeathMontageEffectOverrideContext` → `actors.parquet` (a dynamic actor: `net_guids` has no path for it) | ✅ ObjectNetGuid; 0 (the null reference) except on kills, where it is a player-character pawn open at the event. Not established as the killer or the victim |
| ADR | derived from CombatReport | ◐ +0.1–0.2 vs trackers (wire damage is fractional; not a bug) |
| Health / armour / overheal, absolute | `DamageableComponent` RPCs → `LifeChangeEvents[]` / `LifeChangeBySection[]` | ✅ typed section updates; actor/section timelines require joins, see below |
| Heal / overheal-decay source references | `MulticastNotifyHeal` `HealCauser`, `EventInstigator`, `EventInstigatorPawn`; `MulticastNotifyOverhealDecay` `DecayCauser`, `EventInstigator`, `EventInstigatorPawn` | ✅ `ObjectNetGuid` by exact group/name/checksum. `EventInstigator` is the instigator's PlayerController: it never joins to `actors.parquet` (join through the pawn's `Controller`/`Owner`), and that is expected, not a decode fault. `DecayCauser` = 0 means no causer. No heal credit is implied; see [TARGETING_AND_HEAL_VALUES.md](TARGETING_AND_HEAL_VALUES.md) |

**The historical "vs C#" figures here were measured on build 13.01 or earlier.**
They describe the preserved comparison fixtures, not current upstream parser
compatibility. A result from that fixture does not establish agreement on a
newer build; a fresh comparison needs its own replay and implementation evidence.

### Health is absolute, not a subtraction

The `DamageableComponent` RPCs carry an array whose elements hold
`ChangedComponent` (which damage section), `LifeResult` (**the absolute value
after the change**), `DeltaLife`, and `bAliveAfterChange`. Nothing has to be
accumulated to read a reported section result. The parent array remains in
`raw_bits`, and its decoded members are emitted as typed child rows. The array
uses ordinary RepLayout dynamic-array framing; its inner handles come from
the corresponding parameter-group declaration.

**The four members are now typed, so this section is checkable.** `vrfkit
export` emits one row per member beside the parent blob row, named
`<Function>.LifeChangeEvents[i].LifeResult` and so on. The figures below were
originally taken with an ad-hoc walker that is not in the repo, so they carry
their own provenance -- but the same joins now run against the exported
columns. The historical one-replay report recorded scalar agreement on 3,389
of 3,389 observations and resolved `ChangedComponent` references on 6,232 of
6,232. Its original wording omitted the route-dependent sign: damage compares
the negative delta sum, while healing and decay compare the positive sum.
Neither count establishes independently identified gameplay calls.

Two things to know before joining on them. The local handles differ per RPC --
the same four members sit at 10-13, 1-4 or 2-5 depending on which function
carries them -- and `MulticastNotifyHeal` and `MulticastNotifyOverhealDecay`
name their array `LifeChangeBySection`, not `LifeChangeEvents`. A filter on the
array's name alone silently drops more than half the calls.

Four tools build on these rows, each keeping raw evidence and unresolved
references visible: `tools/extract_healing_observations.py` (serialized heal
amounts, not effective HP; [HEALING_OBSERVATIONS.md](HEALING_OBSERVATIONS.md)),
`tools/extract_section_observations.py` (all five routes, parentless amounts
and checkpoint rows; [SECTION_OBSERVATIONS.md](SECTION_OBSERVATIONS.md)),
`tools/extract_section_timeline.py` (exact predecessors and ordering/lifetime
gaps; [SECTION_TIMELINE.md](SECTION_TIMELINE.md)) and
`tools/extract_section_packet_timeline.py` (packet-ordered comparisons; ties in
one packet still prevent links; [SECTION_PACKET_TIMELINE.md](SECTION_PACKET_TIMELINE.md)).
None establishes a game life, player credit or a continuous health timeline,
and the historical measurements below do not replace their raw validation.

Verified over 69 replays on build 13.02: 377,487 elements, zero parse errors,
zero residual bits, and every observed element carrying exactly four members
in that measurement. Corroborated against separate decode
paths -- the historical report recorded scalar agreement on 230,855 of 230,855
and alive-flag agreement on 61,045 of 61,045. The scalar relation requires the
damage sign correction above. It also requires an explicit accumulation rule:
f64 accumulation rounded once and iterative f32 addition can disagree. These
historical totals are not a fresh whole-corpus transition verification.

Three observations from that historical 69-replay comparison:

- **The alive flag needs separate death-event corroboration.** Deduplicated
  per `(victim, RespawnNumber)`, a false flag matched `events.characterDeath`
  9,362/9,362 across all 69 replays; a zero-value rule missed on two, because a
  character really can sit at exactly 0 health and be alive (65 cases, all
  KAY-O). The flag is also re-reported after death, hence the RespawnNumber
  dedup. This historical join does not authorize substituting RPC
  `RespawnNumber` for `VictimRespawnNumber`; the two differ in current data.
- **Armour is `AttachedDamageSection`, not `ShieldDamageSection`.** The latter
  is an empty shell -- 67,316 elements, every `LifeResult` 0. The real armour
  section's outer is a `HeavyArmorItem_C` / `LightArmorItem_C` /
  `PlasmaArmorItem_C`, and its maximum reads 50.00 / 25.00 / 25.00, which is the
  game's own numbers and an outside confirmation that the f32 decode is right.
  Armour absorbs 2:1 against health on 12,747 of 12,747 hits where it survived.
  The section's own replicated properties (`bAlive`, `LastKnownDamageOwner`,
  and `Life` in checkpoints) are exported under its Blueprint class
  `BasicArmorAttachedDamageSection_C` -- or, in replays that also carry
  Phoenix's `PreventDeathDamageSection`, under the native
  `AttachedDamageSectionComponent`; see "The armour section" below.
- **`MulticastNotifyOverhealDecay` sends `DeltaLife` positive while life goes
  down.** Its magnitude matches `DecayApplied` 33,181/33,181, and the running
  chain only closes if the sign is flipped. `life += DeltaLife` runs overheal
  backwards.

The historical round-start comparison found `LifeResult - DeltaLife == 100` on the first health
event of 10,981 of 10,996 lives. The 15 exceptions all read 200 and are all
Phoenix -- Run It Back, not a decode fault. On the reset broadcast
(`MulticastSectionLifeChange`) the `LifeResult` is trustworthy and the
`DeltaLife` is not an established edge delta. Preserve both reported values,
but do not use reset DeltaLife as an accumulated change or fill unknown starts
with 100.

## Abilities

| Data | Source | Status |
|---|---|---|
| Ultimate cast corroboration | `events.characterUltimateUsed` (word0 resolves to a character on 15,699/15,768 rows) | ◐ do not count Event rows as casts: they outnumber `UltimateActive` False→True transitions by 51.5%; use the transition as authority and Event only as a ±100 ms cross-check |
| Cooldown / start time | `Comp_Ability_CooldownComponent` | ✅ Double |
| Ability cast observations | `Comp_AbilityStatisticsReplicator.AbilityCastsThisRound[]` — `Player` (subject UUID), `Slot`, `Round`, `RoundPhase`, `CastTime`, `CastLocation` | ✅ cast records repeated in array snapshots; deduplicate by cast identity before counting. In the reference sample, `Player` matches a manifest subject 352/352. |
| Ability state stream | `AbilitiesAndBuffsComponent` (`_cnc_h1`); bodies recovered from RepLayout tails: `/Script/ShooterGame.AresAbilitySystemComponent` (`__vrfkit_chained_cnc_h1__`) | ◐ FastArray numeric structure: replication keys, deleted/changed item IDs and raw field boundaries. Measured 2026-09-28 on 1,018 exports (parser `259ed10`): all 3,999,493 `_cnc_h1` main windows (22 builds) and all 250,053 main and 181,108 checkpoint chained windows (24 builds) were consumed exactly. The first measurement (2026-09-09) covered 2,882,152 `_cnc_h1` windows in 714 exports of 13.01, 13.02, 13.04 and 13.05. Available through `extract_fastarray_observations.py` on those routes, streams and builds. 12.10 and 12.11 have only chained rows; `_cnc_h1` checkpoint rows were never observed and remain unvalidated. Ability/effect names and field meanings remain unverified. |
| GAS owner / avatar / attribute sets | `AresAbilitySystemComponent` (OwnerActor, AvatarActor, SpawnedAttributes, CachedAttributeSet) | ◐ Component remap exposes these names. OwnerActor/AvatarActor remain raw: all 629,578 measured packed values equal the enclosing actor GUID, not a separately established player owner. See [the reference audit](GAS_AND_PATCHVOLUME_INVESTIGATION.md). |
| Status effects on a player (nearsight / slow / detain / ...) | `EffectManagerComponent:MulticastPlayContinuousEffect` + `MulticastStopContinuousEffect`, on the **affected** player's actor | ✅ named, with start and end — see below |
| Active gameplay effects (GAS array) | `AresAbilitySystemComponent.ActiveGameplayEffects` | ◐ No named rows in the ordinary property group in the 714-file audit (2026-09-08, checkpoints included). Entries with that name and child handles occur under `AresAbilitySystemComponent_ClassNetCache` (49,076 rows). CNC framing can carry custom-delta properties as well as RPCs, so this location does not prove a function or an absent replicated array. This named array's item decoding remains unverified. |
| GAS attribute values | `AresAttributeSet.{BaseValue,CurrentValue}` per handle | ◐ **checkpoints only.** The live stream sends each attribute once when the channel opens and never updates it; `CurrentValue` does move (Reyna's ultimate puts handles at 1.1/0.9) but only checkpoint snapshots show it, and those are written at round transitions, so transient debuffs are gone by then |
| Persistent effect position (smoke/wall/molly/slow/trap) | `actors.parquet` class_path + spawn xyz | ✅ every spawned effect actor |
| Ground-area volume footprint (molotov, slow, net and wire patches; one circular `X` patch) | GroundVolumeComponent `FragmentInfo` cells, raw in `fields.parquet` (`PatchVolume` and the declared class) | ◐ decoded by `extract_ground_volumes.py`, not typed in Parquet: 36,661 of 36,661 windows exact in 1,018 exports, 346,186 cell updates with world polygon, floor, ceiling and grid cell, checked against a second reader and the owner's spawn. `Status` and `bIsActive` meanings unestablished. See [GROUND_VOLUMES.md](GROUND_VOLUMES.md). |
| Persistent effect lifetime | `actors.time_ms` paired across `event` `open`/`close` (non-fuel; a `dormant` event does not end the instance); `CurrentFuelLevel`+`WallActivated` (Viper) | ✅ |
| Smoke live position | `ReplicatedMovement.location` (world units, [per-class level](#replicatedmovementlocation-is-world-units-at-a-per-class-level)) / `MulticastAddSmokeScreenPoint.Translation` | ✅ |
| Raze ability items (owner, persistence, attachment, seed) | `Ability_Clay_{4,E,Q,X}_*`: `CreatedByCharacter`, `bInPersistentData`, `AttachComponent`, `RelativeScale3D`, `CosmeticRandomSeed` | ✅ exact group/name/checksum; `CreatedByCharacter` resolves to the `Clay_PC_C` actor on every non-null main row |
| Raze satchel attachment | `Projectile_Clay_Q_Satchel_Arming`: `AttachComponent`, `LocationOffset`, `RotationOffset`, `RelativeScale3D` | ✅ exact identity; offsets are relative to the attach component, which resolves to world geometry or a character capsule |
| Raze Boom Bot position | `Pawn_Clay_E_Boomba.ReplicatedMovement` (short rotation) / `bAIControlled` | ✅ location in centimetres, checked against spawn; `bAIControlled` is true on every observed row |
| Cypher trapwire armed, trap owner | `GameObject_Gumshoe_{E,4}_TripWire(_SecondWire)_C.Deployed`; `CreatedByCharacter` on the trapwire and cage ability items; the cage item's `RelativeScale3D` | ✅ on both sides of the 13.01 rename (`Ability_E` trapwire -> `Ability_4`, `Ability_4` cage -> `Ability_Q`; typed at the new paths since 2026-09-28). `Deployed` is sent once per wire, always true: false is the class default and is never replicated, so the row's `time_ms` is when that wire went live. `CreatedByCharacter` resolves to the `Gumshoe_PC_C` actor. The cage projectile's `HasStopped` has no 13.01 successor field |
| Sova (Hunter) bolt trail point | `Projectile_Hunter_{Q_RevealBolt,4_ExplosiveBolt}_C.TrailPosition`, and on 11.06-12.05 `Projectile_Hunter_4_ExplosiveBolt_PrototypeBalance_C.TrailPosition` | ✅ VectorDouble (world units), exact group/name/checksum since 2026-09-28. One to three values per bolt, the first in the packet that opens the bolt's channel; every value lies on that bolt's own path (its spawn point and `ReplicatedMovement` locations), 94-101 units away at most and 22-24 at the median, against a median 1,129-2,146 for another bolt's path. What the game draws from it is not established |
| Possession state (cameras, drones and other possessable pawns) | `PossessableActorComponent_C.IsPossessed` and `Rift_PossessableActorComponent_C.IsPossessed` (component rows, on the pawn's channel); Cypher's camera `Pawn_Gumshoe_E_PossessableCamera_C.Possessed` / `IsDeployed` (under `Ability_Q/Pawn_Gumshoe_Q_PossessableCamera_C` until 12.08) | ✅ Bool, exact identity. `IsPossessed` is carried by `Pawn_Gumshoe_E_PossessableCamera_C`, `Smonk_PostDeath_PC_C`, `Pawn_Hunter_E_Drone_C`, `Pawn_Guide_Q_PossessableScout_C`, `Pawn_Cashew_4_Spider_LockOn_C`, `Pawn_Gumshoe_Q_PossessableCamera_C`, `Pawn_Aggrobot_RollyPolly_C` and (the Rift component) `Rift_TargetingForm_PC_C`. The camera's `IsDeployed` is one row per camera, always true |
| Killjoy turret and alarmbot on the ability item | `Ability_Killjoy_{E_Turret,Q_Alarmbot}_C.DeployedActor` | ✅ ObjectNetGuid, exact identity: GUID 0 (null) or the `Pawn_Killjoy_E_Turret_C` / `Pawn_Killjoy_Q_StealthAlarmbot_C` actor of the same export, on every row |
| Charge of a charged ability | `Comp_Equippable_Charged_C.CurrentCharge` | ✅ Double 0.015..1.0, exact identity; carried by `Ability_Wraith_4_Smoke_C` and `Ability_Mage_E_WorldSmoke_C` channels. The unit is not established |
| Raze satchel, Paint Shells and rocket position | `ReplicatedMovement` on those projectiles | ◐ raw: declined while the reader read every location at /100, and not typed since -- each class needs its own spawn-join level evidence ([per-class level](#replicatedmovementlocation-is-world-units-at-a-per-class-level)); `actors.parquet` spawn xyz for placement |
| Guide (Gekko) E projectile flight | `Projectile_Guide_E_HawkFlash_C`: `ReplicatedMovement` (ByteComponents), `Banking` (Double), `PostControlVelocity` | ✅ typed, main stream only (11.06-13.06). `location` is world units (whole units, measured against spawn -- [per-class level](#replicatedmovementlocation-is-world-units-at-a-per-class-level)). Velocity is in world units; roll is never replicated and reads 0. `Banking` is an angle in degrees, -180..180; what it banks is not established |
| Interaction progress (plant/defuse/orb pickup) | `UsableComponent.HighestProgress` (Float 0..1) / `bIsActive` | ✅ |

### `CastTime` is not measured from `roundStarted`

Its zero is the barrier drop -- the *end* of the buy phase -- while
`events.roundStarted` fires when the round begins, at the buy phase's start.
Joining a cast on `roundStarted + CastTime` therefore lands 30 seconds **early**,
or 45 on the first round of each half.

Measured on 10,460 casts over 20 replays, the residual
`(cast row's time_ms - roundStarted) / 1000 - CastTime` is, for the first
fourteen rounds:

| round | n | median residual |
|---|---|---|
| 1 | 394 | **44.99 s** |
| 2-12 | 6,247 | **29.88-29.91 s** |
| 13 | 347 | **44.89 s** |
| 14 | 364 | **29.89 s** |

Those are the buy-phase lengths the game uses -- 45 s on the first round of each
half, 30 s otherwise -- so this is confirmed against a constant the replay does
not carry, not fitted to the data. The correct absolute time is

    roundStarted + buyPhaseLength(round) + CastTime

Overtime follows the same rule: on the full 71 replays round 25 -- the first of
overtime -- reads 44.88 s, so the 45 s buy phase applies there too.

The median is the statistic to use here, not the mean. `AbilityCastsThisRound`
is a replicated array that accumulates over the round, so a cast is re-sent on
every later replication and its `time_ms` drifts upward; the residual is exact
only on the first send. The share landing within 29-31 s therefore depends on
how much re-replication the sample carries -- 60.1% on the 20-replay set above,
57.4% over all 71 -- and the rest of the mass is that tail, not disagreement
about the epoch.

### Status effects, and where they actually live

For an executable view of these observations, use
`tools/extract_player_effects.py` (see [USAGE.md](USAGE.md#analysis-helpers)).
Effect replication can also target cameras, drones and decoys. The tool keeps
those observations but admits a player target only through a
`SpawnedCharacter` value -- the manifest's, or an earlier one the manifest
dropped when the player reconnected (see [Player identity](#player-identity));
possession alone is not a player body.

A debuff shows up as a continuous effect played **on the affected player's own
actor**, not on the caster's. `EffectManagerComponent`'s
`MulticastPlayContinuousEffect` carries an `EffectContainer` NetGUID that
resolves through `net_guids.parquet` to a named effect, and
`MulticastStopContinuousEffect` closes it by `EffectID`. On the reference replay
every one of the 55 nearsight applications has a matching stop, so start, end
and victim are all recovered.

The names say what the effect is and the measured durations match the game:

| effect | applications | median duration |
|---|---|---|
| `FXC_Wraith_Q_NearsightMissile_Nearsight_C` (Paranoia) | 13 | 2.37 s |
| `FXC_Vampire_4_NearsightAOE_Nearsight_C` (Leer) | 15 | 0.31 s |
| `FXC_Wushu_4_SmokeNearsight_C` | 15 | 0.95 s |
| `FXC_Deadeye_4_Trap_Slowed_C` (trap slow) | 2 | 2.01 s |
| `FXC_Aggrobot_X_DetainDebuff_C` (detain) | 1 | 3.15 s |

An AoE applies to several victims in the same tick, each as its own row, so
"who was affected" comes out per player rather than per cast.

Over 71 demo replays the Play/Stop pairs give a much larger duration sample.
These are not corrections to the table above -- that one is honest about being a
single replay -- but they are the numbers to quote. **Durations only:** this
pass paired by `EffectID` to measure length and did not account for every
unpaired Play, so it says nothing about the termination rate above.

| effect container | n | median |
|---|---|---|
| `FXC_Vampire_4_NearsightAOE_Nearsight_C` (Leer) | 823 | 0.43 s |
| `FXC_Wushu_4_SmokeNearsight_C` | 645 | 0.84 s |
| `FXC_Grenadier_Player_Suppressed_C` (suppress) | 369 | 8.00 s |
| `FXC_Deadeye_4_Trap_Slowed_C` (trap slow) | 299 | 2.23 s |
| `FXC_Global_ConcussedWavy_Prototype_C` | 273 | 2.60 s |
| `FXC_Wraith_Q_NearsightMissile_Nearsight_C` (Paranoia) | 216 | 2.32 s |
| `FXC_Thorne_4_PlayerMovingInSlowField_Production_C` | 170 | 4.89 s |

Suppress lands on 8.00 s, which is the figure the 215-replay pass below reached
by a different route -- two independent measurements agreeing is what makes the
rest of the column trustworthy.

**Exclude the caster-side containers.** `_Equip_C` and `_Cast_C` play on the
*caster's* actor and are the cast animation, not an application: counting them
inflates Leer by 1,365 and Paranoia by 390 over these replays, and would make a
one-sided cast look like a debuff on the caster. The three seen are
`FXC_Vampire_4_NearsightAOE_Equip_C`, `FXC_Wraith_Q_NearsightMissile_Equip_C`
and `FXC_Sequoia_Q_FragileMissile_Cast_C`. The `_Equip_C`/`_Cast_C` suffix is
the observed convention on those three, not a guarantee -- like the container
names above it is cosmetic-asset naming, so a caster-side effect under some
other suffix would not be caught by a suffix filter alone. Confirm against the
actor the effect plays on when it matters.

Two caveats. The container names are cosmetic-effect assets, so the same
gameplay state can arrive under more than one name and a pure-audio variant sits
beside the real one (`..._DetainDebuff_Audio_C`). And this is a *visual* effect
channel: strong evidence the state was applied, but not an authoritative flag --
no component carries an `IsVulnerable`-style boolean anywhere. A status is
always reconstructed as an interval from a start/stop pair.

Measured across all 215 replays:

| status | signal | coverage |
|---|---|---|
| suppressed | `FXC_Grenadier_Player_Suppressed_C` | 1,021 windows over 83 replays, zero unterminated; median 8.0 s |
| vulnerable | `..._Fragile*` effects (the internal name is **Fragile**, not Vulnerable) | 4.0 s duration; 82 of 215 replays show it by one signal or another |

**Vulnerable doubles damage, exactly.** `DamageDealt / FalloffMultiplier`
recovers each weapon's base damage, and over 123,008 gun and melee hits that
ratio is 1.0 on 122,284 and exactly 2.0 on 171. Inside a Fragile window the 2x
rate is 99/99; outside it is 72 in 122,356. A separate pass over the 71 demo
replays -- a different corpus, so a different denominator -- put the in-window
rate at 34 of 61 against 21 of 41,378 outside, a gap of the same three orders
of magnitude. Read the direction as settled and the in-window share as
depending on how completely the Fragile windows were paired.

The multiplier sits outside falloff,
so a Vandal headshot reads 320 = 40 x 4 x 2. Use `DamageDealt`, not
`DamageTaken` -- the latter is clamped to remaining life.

### Callout regions: the export names the actor, not the callout

`CalloutRegionTrackingComponent.CurrentRegion` resolves through `net_guids` to
the region actor a player stands in, which gives a position as a map callout
rather than as centimetres. What the export carries is correct: the actor's
object name, `CalloutRegion_A_Short`, whose outer chain ends at the map's
callout level, `/Game/Maps/Triad/Triad_Callout_Volumes`. That name is a level
designer's label, not the callout. The game's name for the region is the
actor's `RegionName`, a string-table text stored in that level; the replay
does not carry it, and nothing in this repository reads it.

Actor names come in two forms. The split was first measured over 64 demo
replays covering 12 maps; the 13.06 game files confirm it for all 13, Bind
included (160 region actors on the first seven maps, 140 on the other six):

| letters left after the prefix | numbered |
|---|---|
| Ascent, Bonsai (Split), Duality (Bind), Infinity (Abyss), Port (Icebox), Rook (Corrode), Triad (Haven) | Canyon (Fracture), Foxtrot (Breeze), Jam (Lotus), Juliett (Sunset), Pitt (Pearl), Plummet (Summit) |

The prefix varies independently (`BP_`, `InfinityCallout`, bare), so the
form is whether letters survive after stripping it. A numbered name
(`BP_CalloutRegion10`, `BP_CalloutRegion_C_0`) has nothing behind it. A
lettered name is not a label either: on 9 of the 160 lettered actors the
game's `RegionName` is another word.

| map | actor | `RegionName` |
|---|---|---|
| Ascent | `CalloutRegion_A_Link` | Tree |
| Ascent | `CalloutRegion_Back_B` | Boat House |
| Split | `BP_CalloutRegion_B_Main` | Garage |
| Haven | `CalloutRegion_A_Short` | Sewer |
| Haven | `CalloutRegion_C_Short` | Garage |
| Icebox | `BP_Callout_B_Angled_Container` | Yellow |
| Bind | `CalloutRegion_B_Lobby` | Fountain |
| Ascent, Haven | `CalloutRegion_Mid` | Courtyard |

Another word is not necessarily another place -- some may be synonyms -- but a
label read off the actor name is not the game's. Take names from `RegionName`
on every map; the two forms only say where the actor name is legible.
valorant-api.com lists the same names -- its callouts matched the game's
`RegionName` and super-region on all 300 regions of the 13 maps in 13.06 --
but keyed by a point location, not by the actor.

Method and provenance: the callout levels of the installed 13.06 game were
read statically on 2026-09-28 and each region actor's `RegionName` resolved
through the map's string table; no game file or extract is in this
repository. Lettered means letters remain after stripping `BP_CalloutRegion`,
`CalloutRegion`, `BP_Callout`, `InfinityCallout` or `BP_`; another word means
neither name contains the other once the site letter and non-letters are
dropped (`AttackerSpawn` against "Spawn" counts as the same).

Spike sites have the same trap: a `BombDestination` actor's name does not
give its site. In Icebox's bomb-mode level, `BombDestination_A_0` has
`BombSite` B and `BombDestination_B2` has A. Use `TimedBomb.PlantedAtSite`,
which agreed with the game's plant volumes on all 50 plants of nine 13.06
replays.

The regions are still usable without names -- the id is stable within a map
and the spatial extent can be recovered by pooling player positions per
region.

### Slows are visible in the movement data itself

Independently of the effect channel above, a slow is legible from speed alone,
because VALORANT's horizontal speeds sit on a multiplicative lattice off
675 cm/s:

```
675.00 run   607.50 (0.90)   573.75 (0.85)   540.00 (0.80, rifle ADS)
513.00 (0.76)   506.25 (0.75)   405.00 (0.60)   324.00 walk
```

Inside a slow zone every one of those values appears **halved** -- 337.50,
303.75, 286.88, 270.00, 256.50, 253.13, 161.90. Measured across 70 replays and
128,324,174 movement rows, the multiplier is 0.500 to within 0.04%. It is a
multiplier and not a cap: values already below 337.5 are halved too.

Matching a speed against the halved lattice to within ~1% classifies a slow with
under 1% false positives on four negative-control zones (smokes, cages, molly,
heal pool). A plain threshold does not work -- walking is 324 and slowed running
is 337.5, 13.5 cm/s apart.

Effective radius is roughly 500-600 cm and the estimate is genuinely unstable
below that, so treat it as a range. The slow lingers about 0.3-0.5 s after
leaving. Sage's orb and Chamber's trap slow; Fade's Seize and Terra's time-slow
grenade showed no movement-speed effect at their actor's position.

**Crouch is not in `movement_state`.** That column is 0 on all
1,034,035,170 exported movement rows in the historical 2026-08-31 527-replay corpus. Crouch
is `bCrouchHeld` on the character actor, or a ~19 cm drop in `pos_z`, and
crouch speed is ~190 cm/s.

## Movement & position

| Data | Source | Status |
|---|---|---|
| Position (cm) | `movement.parquet` pos_x/y/z | ✅ ≤0.0005 vs C# (on 13.01) |
| Rotation (yaw/pitch) | movement | ✅ exact |
| Velocity | movement vel_x/y/z | ✅ exact |
| Time (128 Hz tick, resets per round) / global | movement `timestamp` / `time_ms` | ✅ — `timestamp` is a **tick counter**, not milliseconds |
| Posture (crouch) | `fields.bCrouchHeld` (not movement_state) | ✅ |
| Trajectory | movement time series per character | ✅ |
| Force modules on a character (tagging, knockback, movement modifiers) | `ForceModuleManagerComponent` RPCs: `NetMulticastApplyForceModule` -- `Module` (→ `net_guids`, a `ForceModule_*` class), `ModuleType`, `Character` (equals the row's actor), `RespawnNumber`, `NetTimestamp`, `HandleNumber`, `SourceLocation`, `Source`, `Duration`; `NetMulticastRemoveForceModule` -- `HandleNumber`, `ModuleType` | ✅ typed: `Source` and `Duration` by exact group/name/checksum, Remove's `ModuleType` through Apply's checksum, the rest by name. `ModuleType` names are unknown: 0 is most modules and 2 the six displacement ones on Apply; Remove's 1 has no established meaning. `NetTimestamp` is a per-actor/per-life clock, not replay time |
| Ability projectile, smoke, pawn and dropped-weapon position | `fields.ReplicatedMovement` `location` / `linear_velocity` (JSON in `value_str`) | ✅ world units on the 26 classes the table types and the Boom Bot's scoped entry -- [see below](#replicatedmovementlocation-is-world-units-at-a-per-class-level) |

### The tick is 128 Hz by a 3:13 pattern, not by alternating

Consecutive per-character steps are 7 ms on 18.72% and 8 ms on 81.28% -- that is
3:13, or 3/16 and 13/16. The mean is 7.8128 ms, **127.995 Hz**. It is not a 1:1
alternation, so do not assume 7,8,7,8 when reconstructing a clock; count ticks
and multiply by 125/16. Measured over 37,775,664 steps on 20 replays.

3x7 + 13x8 = 125 ms is arithmetic on that ratio, not a measurement, and the two
part company at the tail: of 7,836,799 sixteen-tick windows, 95.93% come to
exactly 125 ms, and 0.45% of single-tick steps are neither 7 nor 8 ms (up to
234 ms). Use 125/16 as the nominal rate; do not assume any given window hits
it.

`timestamp` increments by 1 per tick (Δ=1 on 37,938,231 steps, against 34 steps
of 3 or 4) and resets each round, which is why the round cut is found at the
point it drops rather than from a timer.

`time_ms` is the global scrub axis and is non-decreasing on all 71 replays --
though not strictly increasing, since many rows share a `time_ms`.
`duration_ms - max(time_ms)` lands within -1..8 ms.

### Minimap projection

The transform is not in the replay. It comes from **valorant-api.com**, which
publishes `xMultiplier`, `yMultiplier`, `xScalarToAdd` and `yScalarToAdd` per
map; join on `manifest.level_names_and_times[0].name`, which is that API's
`mapUrl`. Those constants are an external source and are not reproduced here.

**The axes cross.** What works is

    u = pos_y * xMultiplier + xScalarToAdd
    v = pos_x * yMultiplier + yScalarToAdd

`pos_y` drives the horizontal axis and `pos_x` the vertical. Of the four
sign/order variants only this one holds up: it puts 100.0000% of live positions
inside [0,1]² on eleven of twelve maps, while feeding `pos_x` to `u` collapses
to 0.9% on Haven and 3.1% on Fracture. Containment alone would not prove it --
a small enough scale contains everything -- so note also that the bounding
boxes fill roughly [0.01, 0.99], which a wrong scale would not.

Two things to handle first:

- **Park slot.** Hidden actors are parked at `pos_x ≈ -50000, pos_z ≈ -49900`.
  Filter on **both** x and z. Filtering on z alone misclassifies real falls.
- **Abyss is the exception, and not a decode fault.** Two runs that fetched the
  constants separately put it at 99.68% and 99.84%; the gap is unexplained and
  neither is picked here, because the mechanism is what matters and both runs
  found it. Of the out-of-range rows roughly six in seven are already below
  z = -3000, and of the rest that sit near the floor, 99-100% have negative
  `vel_z` (median around -1,600 cm/s, against 0 for in-range rows). The map has
  no floor, so players leave the minimap while falling. Nothing to fix -- clamp
  or drop by `vel_z`.

Containment was measured over 12 maps on 69 replays, 121,672,885 live movement
rows, on build 13.02.

### `ReplicatedMovement.location` is world units, at a per-class level

Non-player actors -- ability projectiles, smokes, walls, pawns, dropped weapons --
report their position in `fields.parquet` rows named `ReplicatedMovement`, whose
`value_str` is a JSON object. Its `location` is in the same world units as
`actors.spawn_x/y/z` and `movement.pos_*`; `linear_velocity` is units per
second.

**Exports made before 2026-09-28 are 100x too small on almost every class.** The
reader divided every location by 100, the C# reference's `VectorNetQuantize100`.
The wire packs `round(world * scale)` and sets one bit saying "scaled", never the
scale, and on all but one class the scale is 1 -- so `location` came out as
world/100, a plausible point near the map origin, while every decode counter
read clean (bit consumption does not depend on the divisor). To repair an old
export, multiply `location` by 100 on every class except
`Pawn_Aggrobot_SeekerNade_C`, whose values were already right.

The scale is Unreal's `LocationQuantizationLevel`, a per-class choice the wire
does not carry, so every `RepMovement` entry in the overlay table now states it
(`FieldType::RepMovement { rotation, location }`), the way it already stated
the rotator width.

**Method.** Measured 2026-09-28 over the 1,018 replays audited at `259ed10`
(24 builds; the three one-replay public fixtures, 12.10, 12.11 and 13.00, carry
no `ReplicatedMovement` rows). An independent Python reader of the raw bits,
which agrees with the Rust decoder on all 8,250,603 typed rows, supplies the
packed integers. Two checks per class:

- *Spawn join.* Each `open` row in `actors.parquet`, joined to the same actor's
  first `ReplicatedMovement` row at the same `time_ms` on the same channel, spawn
  position at least 50 units from the origin: `|packed integer| / |spawn xyz|` is
  the scale. Its median on every class and every build lands within 0.01% of 1
  or of 100, and the 1st-99th percentiles within 0.03%. Read at its level, the
  location is within 0.5 units of the spawn on every axis of every join (0.0502
  on SeekerNade) -- the rounding its level allows.
- *Speed.* Consecutive rows of one actor with `0 < dt <= 0.2 s` and
  `|velocity| >= 200`: `(|location step| / dt) / |velocity|` with the location at
  its measured scale. Near 1 means the velocity is whole units too.

| Class | Rotator | Location | Spawn joins | Builds | Ratio p1-p99 | Speed check (median, pairs) |
|---|---|---|---:|---:|---|---|
| `Pawn_Aggrobot_SeekerNade_C` | Short | **two decimals** | 932 | 15 | 99.9984-100.0014 | no moving rows |
| `EquippablePickupProjectile_C` | Byte | whole units | 288,644 | 21 | 0.9997-1.0002 | 1.01 (1,950,150) |
| `GameObject_Smonk_NewSmoke_C` | Byte* | whole units | 27,667 | 21 | 0.9999-1.0001 | no moving rows |
| `Projectile_Wraith_4_Smoke_C` | Byte | whole units | 14,935 | 17 | 0.9998-1.0002 | 1.00 (144,774) |
| `Zone_Wraith_4_Smoke_C` | Byte* | whole units | 14,902 | 17 | 0.9999-1.0001 | no moving rows |
| `Projectile_Vampire_4_NearsightAoE_C` | Byte | whole units | 13,892 | 21 | 0.9998-1.0002 | 0.97 (67,202) |
| `Projectile_Hunter_Q_RevealBolt_C` | Byte | whole units | 12,032 | 14 | 0.9998-1.0002 | 1.00 (170,773) |
| `Projectile_Wushu_4_Smoke_C` | Byte | whole units | 11,976 | 20 | 0.9999-1.0002 | 0.98 (530,104) |
| `Projectile_Guide_E_HawkFlash_C` | Byte | whole units | 8,265 | 15 | 0.9998-1.0001 | 0.99 (1,013,091) |
| `Projectile_E_BountyHunter_Divebomb_C` | Byte | whole units | 5,715 | 14 | 0.9999-1.0001 | 0.99 (120,908) |
| `Projectile_Hunter_4_ExplosiveBolt_C` | Byte | whole units | 5,280 | 5 | 0.9999-1.0001 | 1.00 (55,390) |
| `Projectile_Wraith_Q_NearsightMissile_C` | Byte | whole units | 4,062 | 17 | 0.9998-1.0001 | 1.00 (53,124) |
| `Projectile_Smonk_DecayNade_C` | Byte | whole units | 3,505 | 21 | 0.9999-1.0001 | 1.00 (46,775) |
| `GameObject_Smonk_Q_DecayExplosion_C` | Byte* | whole units | 3,490 | 21 | 0.9999-1.0001 | no moving rows |
| `GameObject_Smonk_NewSmoke_PDS_C` | Byte* | whole units | 3,320 | 21 | 0.9998-1.0002 | no moving rows |
| `Projectile_Neon_C_Tunnel_C` | Byte | whole units | 3,269 | 12 | 0.9998-1.0001 | 0.98 (54,749) |
| `Projectile_Phoenix_Q_FlameWall_ThroughWall_C` | Byte | whole units | 2,765 | 18 | 0.9999-1.0001 | 0.98 (179,899) |
| `Projectile_E_Aggrobot_DiscTurret_PowerWave_C` | Byte | whole units | 1,819 | 15 | 0.9998-1.0002 | 1.00 (30,501) |
| `Projectile_E_Aggrobot_OrbSpawner_C` | Byte | whole units | 1,812 | 15 | 0.9999-1.0002 | 1.06 (13,139) |
| `GameObject_Mage_E_WorldSmoke_C` | Byte* | whole units | 1,046 | 5 | 0.9999-1.0001 | no moving rows |
| `Projectile_Terra_C_TimeSlowGrenade_C` | Byte | whole units | 1,033 | 11 | 0.9999-1.0002 | 1.00 (10,280) |
| `GameObject_Terra_C_TimeSlowGrenade_Explosion_C` | Byte | whole units | 1,029 | 11 | 0.9998-1.0001 | no moving rows |
| `Projectile_Aggrobot_Zamboni_Rocket_C` | Byte | whole units | 1,007 | 15 | 0.9998-1.0002 | 1.00 (1,112) |
| `Projectile_Pandemic_E_SmokeScreen_NoCollision_C` | Byte | whole units | 703 | 8 | 0.9998-1.0001 | 1.00 (8,930) |
| `Projectile_Aggrobot_C_ExplodeyPatch_C` | Byte | whole units | 647 | 15 | 0.9999-1.0001 | 1.00 (9,052) |
| `Projectile_Mage_Q_Wall_C` | Byte | whole units | 596 | 5 | 0.9998-1.0002 | 0.98 (131,617) |

\* The five `Byte*` classes never replicate a rotation, so the wire cannot
tell byte from short components and the width is a prior, not a measurement:
the 13.06 game's class data, by which every observable native `AGameObject`
decodes as byte. The evidence, and the byte-identical re-export of all 903
replays that declare their movement, are at `GAME_OBJECT_BYTE_ROTATOR_GROUPS`
in `tools/apply_type_corrections.py`. A rotator flag, if one ever appears,
decides by exact consumption.

Every class the table types was observed, so no entry rests on the default
alone. The Boom Bot (`Pawn_Clay_E_Boomba_C`), typed through an exact scoped
identity (`tools/fixtures/scoped_type_evidence.json`) rather than the table,
packs two decimals: 2,296 joins over 18 builds, ratio 99.9986-100.0014,
every component within 0.0504 of spawn. HawkFlash's and the Boom Bot's rows
were re-measured when the branches that type them were integrated, with a
separately written join over the same 1,018 exports; SeekerNade's was
reproduced the same way (932 joins, 15 builds, within 0.0502). The two-decimal class is also visible in the raw widths: its packed
components are 17-22 bits wide (median 21), against a median of 14 on every
whole-unit class.

What the evidence does **not** cover:

- **SeekerNade's velocity level.** All 932 of its velocities are the zero vector,
  which reads the same at any divisor, so its speed check has nothing to measure.
  The reader uses whole units for every velocity.
- **Checkpoints.** No checkpoint table in the 1,018 exports holds a single
  `ReplicatedMovement` row, so every figure above is main-stream only.
- **Classes nothing types yet.** The same checks on the 67 classes that
  carried the field untyped when this was measured split cleanly: all ten
  `Pawn_*`/`AIPawn_*` classes pack two decimals and all 57 others pack whole
  units. Two of them are typed now -- the Boom Bot (a pawn, scoped) and
  `Projectile_Guide_E_HawkFlash_C` (the table), both above -- leaving 65:
  nine pawn classes such as `Pawn_Killjoy_E_Turret_C` and 56 others. That is
  a pattern for whoever adds one of them, not a reason to skip measuring it.

**New entries.** The generator (`extract_descriptors.py`,
`REP_MOVEMENT_LOCATION`) gives every entry whole units -- Unreal's own
`FRepMovement` default and the level of 25 of the 26 classes above -- and
`apply_type_corrections.py` pins SeekerNade to two decimals. A default is a
prior, not a measurement, so `tests::overlay` lists every group given a
`RepMovement` type -- by the table or by `scoped_types.rs` -- with its measured
level, and fails on a group it does not list: a new class cannot ship on the
default without somebody running the spawn join first. A `RepMovement` literal
written without a `location:` does not compile.

**This member differs from the C# reference on purpose:** the reference still
emits location/100 for every class. Keeping that reading
([archive 13-J](archive/PROJECT_STATUS.md#13-j-the-ability-pawns-and-projectiles-got-descriptors-done-2026-08-02))
rested on member-for-member parity (13-B) and on no metric reading the field;
this page lists it as a position source, and a position 100x wrong is not
parity worth keeping.

### RPC transforms: `249` is a rotation quaternion, not a rotator

Several RPCs and transition contexts replicate an `FTransform`, and the wire
flattens it into three members: `Translation` and `Scale3D` by name, and the
rotation under the hardcoded name index 249 (`Rotation`), which the export
spells `249`. All three are 192-bit `VectorDouble`s, rendered `(x,y,z)` in
`value_str`.

`Translation` is a location and `Scale3D` a scale. **`249` is neither an Euler
rotator nor a direction: it is the transform's `FQuat`, and the three numbers
are its X, Y and Z.** The checksum says so -- 747197698 reproduces as the member
`Rotation: FQuat` of `Transform: FTransform`, and not as `FRotator` or `FVector`
(`tools/tests/test_compatible_checksum_facts.py`); the 13.06 executable's
reflection has `Transform.Rotation` as a quaternion. W is not sent. Unreal's
`FQuat::NetSerialize` normalizes the quaternion, flips all four signs when W is
negative and writes only X, Y and Z, so a reader rebuilds
`W = sqrt(max(0, 1 - (x*x + y*y + z*z)))`. That convention is the public engine
source's; it was not verified in this binary. The data fit it: over all 1,018
replays (24 builds, main and checkpoint rows), the 12,698,371 rows under
747197698 are all 192 bits with `|xyz| <= 1` (the largest is exactly 1, none
exceeds `1 + 1e-6`), and 12,594,391 of them hold a negative zero -- what the
sign flip leaves on a zero component. No `W` column is exported; compute it
when you need the full quaternion.

| Carries it | `249` checksum | Parent | Rows (1,018 replays) |
|---|---|---|---|
| `EffectManagerComponent:MulticastPlay{Continuous,OneShot}Effect`, the weapons' `AresEquippable:MulticastPlay{Continuous,OneShot}EffectFromClient` | 747197698 | parameter `Transform` | in the 12,698,371 |
| `TransformTransitionContext`, `TransitionContext_Sequoia_X_TeleportInfo_C`, `StateContext_ActorTrailTargetingResult_C` (typed through the checksum table) | 747197698 | property `Transform` | in the 12,698,371 |
| `AresGameStateBase:MulticastResetForRespawn` | 1874998526 | parameter `SpawnTransform` | 183,577, `\|xyz\| <= 1` |
| Viper's and Phoenix's `MulticastAddSmokeScreenPoint`, Astra's `MulticastAddAnchor` | 177696787 | parameter `ValveSetTransform` | 58,598, `\|xyz\| <= 1`; left raw |

A different `249` travels on the effect-placement RPCs
(`ClientPlayOneShotEffectAtLocation`, `ReplayPlay*EffectAtLocation`,
`ReplayRecord*Effect`): checksum 2526428638, which reproduces as a top-level
`Rotation: FRotator`. That one is a rotator, typed `RotationShort` and rendered
in degrees. Same name, different property -- the checksum is what tells them
apart.

## Weapons & loadout

| Data | Source | Status |
|---|---|---|
| Weapon instance class | `actors.parquet` class_path + `tools/equippable_table.py` (display name) | ✅ |
| Shot events (ammo, projectiles, vectors, seed, fire mode) | effect blobs | ✅ typed JSON |
| Magazine ammo over time | `AmmoComponent.AuthResourceAmount` (Int32) | ✅ via the `MagazineAmmo` remap; reads 0..100 |
| Reserve ammo over time | `AmmoComponent.AuthResourceAmount` (Int32) | ✅ via the `ReserveAmmo` remap -- same native component, second instance; reads 0..200, plus a 999 sentinel (below) |
| Equipped weapon (per player, over time) | `AresInventory.CurrentEquippable` / `NewCurrentEquippable` -> actor class | ✅ via InventoryComponent->AresInventory remap (resolve the NetGUID to its equippable actor) |
| Equipped weapon (on damage) | `MulticastNotifyDamage.EquippableUsed` | ✅ |
| Holder of a weapon effect | `AresEquippable:MulticastPlay{Continuous,OneShot}EffectFromClient.EffectManagerComponent` → `net_guids` (`EffectManager`, whose outer is the holding pawn) | ✅ ObjectNetGuid; a just-dropped weapon can still name its previous holder |
| Weapon readying speed | `ReadyingStateComponent.AuthEquipSpeed` | ✅ EnumByte, {0, 1, 2} in the main stream (same values as `AutoEquipSpeed`); member names unknown. Always 0 in checkpoints, so not readying state there |
| Buyer team recorded on an equippable | `AresEquippableDataTracker.OriginalBuyerTeam` | ✅ FName as sent, `Red` or `Blue`; nothing maps it to attacker/defender or to a player |
| Skin / spray / charm | `manifest` playerLoadouts (per subject) | ✅ |

**999 means infinite reserve, not infinite ammo.** It appears on 53 rows over
71 replays, always on `ReserveAmmo` and never on `MagazineAmmo`, and it belongs
to `Gun_Sprinter_X_HeavyLightningGun_Production_C` -- Neon's ultimate. The
correspondence is exact: 22 replays carry a 999 and 22 replays carry that gun,
with no replay on either side of the pair alone. It is written once per gun
instance and never moves, while the same gun's magazine counts down and reloads
normally. Treat it as a sentinel, not a count -- a max() over the column
otherwise reports a 999-round reserve.

## Rounds & score

| Data | Source | Status |
|---|---|---|
| Round result (winning team) | `RoundResults` struct blob | ✅ name-based (survives handle shifts) |
| Team score | derived / `BaseTeamState.Wins`/`Points` | ✅ (R1–R5 invariants verified) |
| Round number | `events.roundStarted` word0 / `RoundNumber` | ✅ |
| Half / side swap / overtime | `events.switchTeams` | ✅ |
| Per-round team economy | `TeamEconomy` (13.01) / `BaseTeamState` (13.02) | ✅ |
| Round-loss streak | `BombGameState_C.CurrentLossStreak` / `LossStreakTeam` (Swiftplay's game state carries `LossStreakTeam` too) | ✅ exact identity since 2026-09-28: Int32, 0..2 on every observed row; ObjectNetGuid, GUID 0 or the `RedTeam` / `BlueTeam` object in `net_guids` |
| Match-timer override flag | `BombGameState_C.ShouldOverrideMatchTimer` (and Swiftplay's) | ✅ Bool, exact identity. Always sent in the same packet as `OverrideMatchTimerText`: true with its formatted-number form, false with its empty form, on every observed row |
| Round-end ceremony and its subject | `{Default,Ace,Clutch,Closer,Flawless,Thrifty,TeamAce}Ceremony_C.bShouldDisplayCeremony`; `ClutchCeremony_C.ClutchPlayer`, `CloserCeremony_C.CloserPlayer`; `FlawlessCeremony_C.FlawlessTeam`, `ThriftyCeremony_C.SalvagingTeam`, `TeamAceCeremony_C.TeamAcingTeam`; `ThriftyCeremony_C.{Blue,Red}TeamStartingAvgInventoryValue` | ✅ exact identity since 2026-09-28: Bool; ObjectNetGuid (GUID 0, or a `BombPlayerState_C` / Swiftplay player-state actor, or the `RedTeam` / `BlueTeam` object in `net_guids`); Int32. In every packet that carries both, the flag is true exactly when the ceremony's player or team is non-null. The ceremony actor chosen for a round is `ChosenCeremonyForRound` |
| Match-timer override text | `BombGameState_C.OverrideMatchTimerText` (and Swiftplay's) | ✅ `FTextTree`, exact identity: the FText history tree as JSON in `value_str` -- the empty history 255 (`{"flags":0,"history":255,"kind":"empty"}`) or history 4, a number the game formats itself (`kind: "as_number"`, a `source.double`, the `format` options -- always two integral and two fractional digits here -- and a `culture`). Observed source values 0.01..36.95. What the number counts down is not established. See [TEXT_HISTORY_EXPANSION.md](TEXT_HISTORY_EXPANSION.md#history-4-a-formatted-number) |

## Spike & objective

| Data | Source | Status |
|---|---|---|
| Plant / defuse / detonation | `events.spikePlanted` / `spikeDefused` / `spikeExploded` | ✅ |
| Spike carrier (who holds it, over time) | `BombEquippable_C.Owner` on the spike's own channel → `tools/extract_spike_carrier.py` | ✅ resolved to manifest `subject`; covers backpack, not just in-hand |
| Spike in hand (vs carried) | `AresInventory.CurrentEquippable` / `NewCurrentEquippable` == bomb GUID | ✅ the `in_hand` flag of the same view |
| Defuser | `TimedBomb.CurrentDefuser` (ObjectNetGuid) | ✅ |
| Planter | carrier at the `spikePlanted` timestamp (`extract_spike_carrier.py`) | ✅ from the Owner chain; the event payload itself carries no planter |
| Spike timer | `TimedBomb.TimeRemainingToExplode` / `DefuseProgress` | ✅ Double |
| Plant site (A/B) | `TimedBomb.PlantedAtSite` (EnumByte) + position derivation | ✅ absent handle = default site (UE default-value skip); 100% via spawn position |
| Detonation source | `events.spikeExploded` is canonical (always emitted) | ✅ `RoundResults` under-counts: it logs win-reason, not detonation |

The gap between 776 plants, 248 defuses and 59 detonations looks like loss and
is not. Checked against independent RPCs over 71 replays, `spikeExploded`
matches `ClientBombExplode` 59 to 59 and `spikeDefused` matches
`BombHasBeenDefused` 248 to 248, with no replay disagreeing;
`TimeRemainingToExplode` agrees too, reaching 0.00 exactly on the detonations
and stopping mid-count otherwise. The remaining 469 are rounds that ended in a
team wipe after the plant, so the fuse never ran out. `BombEquippable` actors
open 1,317 times against 1,317 `roundStarted` events -- one bomb per round,
exactly.

Carrier resolution now misses no plant at all: `extract_spike_carrier.py`
reports `NO CARRIER` 0 times across the 71 exports, against 2 before the
disconnect fix above. Both of those were pawns whose `Owner` pointed at a
character the manifest had lost, and the tool said `NO CARRIER` rather than
guessing -- which is the only reason the failure was findable.

The same loss had a second shape, which the 71 did not contain: a player who
reconnects is given a new pawn, and the manifest keeps only that one. On the
1,018-export audit corpus (parser 259ed10, 2026-09-28) the manifest-only join
left 123 `Owner` intervals in 32 exports `unknown` -- every one an agent pawn
that an earlier `SpawnedCharacter` value names -- and 28 plants in 17 exports
`NO CARRIER`. Joined through the whole `SpawnedCharacter` history
(`tools/player_identity.py`), both are 0 across all 1,018 exports, and those
123 intervals carry `carrier_identity_provenance` naming the earlier pawn. The
same pass removed a quieter fault: the manifest-only map held a `None` key for
a player with no character, so on the four 11-PlayerState exports 265 `loose`
intervals carried that player's subject; now none does.

## Actor / GUID / structure

| Data | Source | Status |
|---|---|---|
| Every actor spawn/despawn + class/archetype/spawn location | `actors.parquet` -- `event` is `open`/`close`/`dormant`; only `close` is a despawn | ✅ |
| GUID → object path | `net_guids.parquet` | ✅ |
| Containment chain (subobject → parent) | `net_guids.outer_net_guid` | ✅ |
| Full declared schema (475 groups, handle→name) | `manifest.net_field_export_groups` | ✅ |

## Replay metadata

| Data | Source |
|---|---|
| Build / branch / version / changelist | `manifest` |
| Duration, timestamp (FDateTime), encryption/compression flags | `manifest` |
| Platform, build config, network checksum/GUID | `manifest` |
| Stats (packets / bunches / blocks / fields / RPCs / malformed / skipped) | `manifest` |
| playerLoadouts (subject → agent/skin/spray), matchID | `manifest` game_specific_data |

## Other

| Data | Source | Status |
|---|---|---|
| Ping / latency (ms) | `BombPlayerState.Ping` (16-bit, ms) | ✅ typed (SerializedInt{65536}) |
| Connection status | `ConnectionStatus` | ✅ |
| Movement-prediction reset on possession | `ShooterCharacter:ClientResetRemoteMovementPrediction.isPossess` (every character and pawn cache) | ✅ Bool; true on every one of 291,346 observed rows |
| Game mode (Bomb / Swiftplay) | group_path (`GROUP_ALIASES` maps Swiftplay) | ✅ parser-side |
| Other Blueprint properties typed by exact identity (2026-09-28) | `OnKillEffect_Base_C.Victim FXC` / `VictimFXC_Planted` (a `FXC_Finisher_*` class in `net_guids`); `FloatTransitionContext_C.Float`; `EquipRequestTransitionContext_C.TargetEquippable`; `ActivatableActorComponent_C.IsActivated` and `ActivatableActor_Selectable_Component_C.IsActivated`; the patches' `Has Succesfully Hit`; the removable objects' and reveal darts' `Target` (with the pre-13.01 net-toss and tracking-dart paths); `Ability_{Wraith_Q_NearsightMissile,Sequoia_Q_FragileMissilePrototypeEquipped}_C.Projectile` / `WarningActor`; the Cashew missile's `EndingStrikeLocation`, `RocketIndex`, `IsCurrentlyDoingInitialFlyOut` and its markers' `Seeking Missile Actor`; `Pawn_Killjoy_Q_StealthAlarmbot_C.IsArmed` / `IsBurrowed`; Breach's `Charge`, `ChargeDistance` and fissure `CharacterLocation`; `DefaultToTargetViewModeTargeting`; `Ability_Iris_Thumper_C.Is State Concuss`; `GameObject_Thorne_E_Wall_Segment_Fortifying_C.IsAlive_0`; `Actor_Sequoia_X_StandardArena_C.AssignedPlayspace`; `Switch_BlackMarket_2_C` lever state and times; `BP_Breakable_Simple_*_C.Destroyed`; the finisher effect objects' fields; `ActiveSlowTimeEffects` | ✅ each typed only at its exact group, name and checksum; the evidence per identity -- game-file property class, recomputed checksum, corpus decode and value checks -- is in `tools/fixtures/scoped_type_evidence.json`. Types say what the bits are, not what the game means by them |

---

## Limitations (replay-format, not parser bugs)

- **Display names** — the replay carries no player names, only account UUIDs.
- **Bare component groups** — a component such as `InventoryComponent`
  replicates under its instance name while the replay declares its layout under
  the native class (`AresInventory`). `KNOWN_SUBOBJECT_CLASS_PATHS` connects
  them (pinned in `crates/vrfkit/src/sink/paths.rs`). The map comes from the
  game's class hierarchy, not from names; a bare group on a new build needs
  "Reading component classes out of the game" below.
- **AbilitiesAndBuffsComponent** — the replay never declares its `_ClassNetCache`
  group, so `function_count` is brute-forced (fc=34). The outer RPC framing is
  fully recovered, and the inner payload is decomposed (a flag bit followed by a
  little-endian `u32` stream -- not the opaque blob it was once assumed to be).
  The word decomposition does not establish a function name, prediction-key
  type, cast, or buff event. A leading pair can recur across actor/object
  identities, and the second word does not always equal the previously observed
  first word. The words stay in `raw_bits`; see the dated
  [inner-stream and reference audit](GAS_AND_PATCHVOLUME_INVESTIGATION.md).
- **FName instance numbers are part of the name.** Unreal stores an FName as a
  string plus a number, where number 0 means "no suffix" and number N means the
  displayed suffix N-1. Both the schema readers and `decode_fname` used to drop
  that number, which merged genuinely distinct properties: on the reference
  replay, 553 rows spelled `MyEquippable` were really `MyEquippable` on
  `EquippableGroundPickup_C` (handle 15, 277 rows) and `MyEquippable_0` on
  `EquippablePickupProjectile_C` (handle 16, 276 rows). `IsAlive` /`IsAlive_0`
  on Thorne's wall segments is the same shape. They are now rendered apart.
  Consequence to know: `table.rs` is generated from C# descriptors that spell
  the projectile's field `MyEquippable`, so that name entry no longer matches
  and the field resolves through the weakest fallback — `compatible_checksum`
  (`checksum_table.rs`, to `ObjectNetGuid`). No row lost its type, but the name
  path for that one entry is dead until the generator learns the number.
- **A reused channel once inherited the previous actor's archetype**: on
  `08aec1e1` packet 28115 a `BP_Destructible_Snowman_B1` decoded as
  `Projectile_Pandemic_4_SmokeGrenade_C`, with typed `215`/`216` fields (13 of
  215 replays, 98 rows). Archetypes are now stamped with the actor GUID they
  were read for, so those rows report the bare group: corpus `Decoded OK` fell
  169,335,818 -> 169,335,720 with every other bucket steady.
- **spikeExploded** — not a limitation: `events.spikeExploded` is the canonical
  detonation signal and is always emitted. `RoundResults` records the round
  *win reason* (elimination/detonate/defuse), not whether the spike detonated,
  so it under-counts detonations and is not a reliable proxy.

---

## What's next (where to start)

Remaining work and its evidence requirements are tracked in
[`FOLLOWUP.md`](FOLLOWUP.md). **Start by bucketing the untyped rows on
`compatible_checksum`**: a known checksum means a resolution bug, an unseen one
a coverage gap, none a value addressed inside a payload
([recipe](USAGE.md#fieldsparquet)).

1. **The next unnamed single handle** — `HANDLE_ADDITIONS` in
   `tools/apply_type_corrections.py` is currently empty: its one entry named
   `MagazineAmmo` handle 2 by hand, and the cooked game showed that group is an
   `AmmoComponent`, which the replay declares properly. The mechanism stays
   because the next bare handle will not necessarily have a native group to
   borrow from. Pin any new one in `crates/vrf-decode/src/tests/overlay.rs`.
2. **AbilitiesAndBuffs inner payload** — structurally decoded (`flag + u32`
   stream in `crates/vrf-decode/src/cnc.rs`), per-word meaning unknown.
   **Checked against the shipped game, not assumed:** the script object map has
   `/Script/ShooterGame.AresAttributeSet` as a class with *zero* members,
   because GAS attributes are `FGameplayAttributeData` fields declared in C++
   and cooked assets carry no member list for them. Unpacking the paks does not
   help. The fc=34 RPC timing and size are already exported.
3. **Keep the component remaps honest.** `KNOWN_SUBOBJECT_CLASS_PATHS` was read
   out of one build; a later one can rename a component and nothing here would
   notice on its own. Run `tools/check_component_remaps.py --export <dir>`
   against a replay from a new build. It needs no game install and no baseline.
   When it or its unclaimed-group list points at a component, re-read the class
   from that build's game with `tools/extract_component_classes` rather than
   guessing one.

**Type nothing you have not seen decode.** `LocalizedStat` was typed `FString`
on the strength of the name and produced null on 3,011 of 3,011 rows while
`Decode errors: 0` held the whole time. A wrong type is not loud. After adding
one, count non-null values on the column before believing it. That same field
now decodes 225 of 225 as an `FText`, which is the check working in the other
direction.

**And check the reason you gave for not doing something.** `LocalizedStat` was
left untyped on the grounds that `Statistic` already carried the same fact.
`Statistic` decodes to a bare integer; that historical argument ignored whether
consumers had a usable dictionary. The current `extract_ability_stats.py` pairs
it with `LocalizedStat` inside the same serialized cast/effect slot and exposes
a build-scoped mapping. All 714 exports yielded 155,150 paired observations,
with no missing partners or mapping conflicts: 31 IDs in each of 13.01, 13.02
and 13.04, and 32 in 13.05. Unobserved IDs remain unknown. On 2026-09-28,
38 13.06 exports gave 14,814 paired observations of 29 IDs, again with no
missing partners or conflicts, each under exactly its 13.05 name. 13.05's
IDs 57, 62 and 65 were not observed on 13.06 and stay unknown for that build.

### Done, and where the reasoning lives

Remaining Blueprint components and `ReserveAmmo` came from the shipped game
("Reading component classes out of the game" below). Checksum propagation is
`checksum_table.rs` (`extract_checksum_types.py`), consulted last in
`resolve_entry`, and `check_checksum_types.py` checks it as type evidence
([CHECKSUM_TYPES.md](CHECKSUM_TYPES.md)). `AbilityCastsThisRound[].Effects[]` is
walked by `ABILITY_CASTS_SCHEMA` in `crates/vrf-decode/src/array/schema.rs`. RPC
signature aliasing and more name-resolved properties were rejected with
measurements (the "Closed" sections below).

The list once said "confirmed wire-limit: the GAS stream is state-sync, not one
RPC per cast". The premise was right and the conclusion did not follow:
`Comp_AbilityStatisticsReplicator` replicates one record per cast, with the
caster's subject UUID, slot, round, time and world location. The rows were
flattened and every member named, but none typed, so every "what is still
untyped" survey -- ranked by row count, these sit at 300-800 rows -- missed
them. **A named field with no type is invisible to a scan that starts from
typed columns.**

### Reading component classes out of the game

The bare group names -- `ZoomStateMachine`, `ReserveAmmo`, `CalloutRegionTracker`
and many others -- are component *instance* names, not classes, so nothing in a
replay says what they are. The installed game does say, and it does not need
decryption: VALORANT's IoStore containers are `Compressed+Signed+Indexed` with a
zero encryption GUID, so the `Encrypted` flag is simply off.

`tools/extract_component_classes` reads it (build and flags:
[USAGE.md](USAGE.md#reading-the-installed-game)). Given the `Paks` directory it
prints one row per component template -- the instance name the replay sends, the
package that owns it, and the class. The chain it follows:

1. `.utoc` -> chunk ids, compression blocks and the directory index. Only TOC
   version 5 is accepted, the one the 13.06 containers use.
2. Each `ExportBundleData` chunk starts with the package header -- name map and
   export map. Only the blocks the header spans are decompressed, with the same
   `oozextract` vrf-container uses for replay chunks, which keeps a full scan of
   the 30 GB of `.ucas` to seconds.
3. A component shows up in one of two export shapes. A Blueprint-added component
   is a `<Name>_GEN_VARIABLE` export; a component the C++ class creates is a
   subobject of the class default object (`Default__<Class>`) under its instance
   name. The first shape alone cannot reproduce the table: in 13.06,
   `InventoryComponent`, `AbilitiesAndBuffsComponent`, `CalloutRegionTracker`,
   `VisionComponent` and `DamageHandlerComponent` exist only in the second.
4. The export's `ClassIndex` is an `FPackageObjectIndex`. A `ScriptImport` is a
   hash, which `global.ucas`'s script object map -- a name batch followed by
   `FScriptObjectEntry` records -- turns into `/Script/<Module>.<Class>` by
   walking the outer chain. A `PackageImport` names a public export of another
   package; the tool follows it to that package's Blueprint class and on up the
   super chain to the first native class.

The game checks the result, which is what makes it more than a parse. On the
13.06 containers (17 containers, 379,670 TOC entries, 289,267 packages; five of
them, the three largest included, were rewritten on 2026-09-25 UTC, two days
after `global.utoc` -- the tool prints each container's id, size and time, so a
run can be matched to the files it read):

- every one of the 80,800 script object paths the tool rebuilds hashes back to
  the index the game stores for it -- CityHash64 of the lowercased UTF-16 path,
  which is how the engine forms that index, so a wrong name or outer link
  cannot pass. The separators are not proved: the hash folds `.` and `:` alike
  into `/`, and the tool writes `/Script/<Module>.<Class>:<Sub>` and joins
  anything deeper with `.`, as the engine's `GetPathName` does;
- every package name hashes back to its chunk id, and agrees with the directory
  index's file name;
- every TOC file is consumed to its last byte, and 0 packages fail.

Anything the tool cannot resolve prints as `?`. One format fact came out of the
first run: UTF-16 names in a name batch are **not** aligned to two bytes.
Aligning them misread 26 packages whose name maps hold a Chinese texture name at
an odd offset.

What it does not read: the 17 legacy `.pak` files beside the containers. Their
indexes are encrypted, so a package stored only there would be invisible. Nothing
so far points at one -- every pair below came from the IoStore containers.

**The existing table reproduces** (2026-09-28, 13.06). Every pair a cooked asset
can hold came back with the class it already had: the 16 read from the game
before, `InventoryComponent -> AresInventory` and
`AbilitiesAndBuffsComponent -> AresAbilitySystemComponent` (first argued from
handle shapes), and the C# reference's four effect components. None was
contradicted. `AresAttributeSet_2` is in no package, as expected of a runtime
subobject. The chain as this section used to state it -- `_GEN_VARIABLE` exports
only -- finds five of those pairs nowhere in 13.06 (step 3), so the class
default object shape was always part of the method, written down or not.

The earlier pass also corrected one guess: `MagazineAmmo` and `ReserveAmmo` are
both `AmmoComponent`, whose handle 2 the replay declares as `AuthResourceAmount`,
so the hand-written `AmmoCount` -- the one entry `HANDLE_ADDITIONS` ever had --
was in the right place with the wrong word. On 02d4d478 that change took
unnamed handles 17,013 -> 2,460 and decoded OK 702,149 -> 714,070, with 215/215
corpus replays at 0 decode errors (a dated before/after; the baseline pins
today's totals).

#### Pairs added from 13.06 (2026-09-28)

Every bare group of at least 9,000 rows in the 1,018-replay corpus inventory
(builds 11.06-13.06) was looked up in the tool's output. "About 10,000" was the
brief; 9,000 is the cut actually applied, and it admits one group under 10,000
(`SwapCameras_StateMachine`, 9,478). A pair went in only when all of these held,
measured over every replay's main-stream rows and, separately, every
checkpoint's rows:

- **one class.** The instance name has exactly one class across every package
  and both export shapes;
- **declared.** The replay declares that class's group wherever the bare group
  carries RepLayout rows -- rows whose `field_name` is null or carries no
  `_cnc_h` / unresolved-payload marker;
- **handles fit.** The handles those rows use are a subset of the handles the
  target declares in the same replay, and checkpoint rows of the handles their
  own checkpoint declares;
- **widths agree** wherever the target group also has rows of its own in the
  same replay, handle for handle.

All are RepLayout-only, like every pair before them, so their ClassNetCache rows
stay bare by design.

| leaves | class | RepLayout rows, main | checkpoint | ClassNetCache rows left bare |
|---|---|---:|---:|---:|
| `Resume_StateMachine`, `Sprint_StateMachine`, `Slide_StateMachine`, `ProjectileStateMachine`, `EquipStateMachine`, `PrimaryTriggerActionStateMachine`, `EquippableStateMachine_Activate`, `LaserStateMachine`, `SelfResStateMachine`, `Ability State Machine (EquippableStateMachine)`, `TimerStateMachine`, `CloakStateMachine`, `SpontaneousEquip_StateMachine`, `EquippableStateMachine_Dart`, `EquippableStateMachine_Attack`, `EquippableStateMachine_PickUpOnCooldown`, `SwapCameras_StateMachine` | `EquippableStateMachineComponent` | 1,786,888 | 259,401 | 107,750 |
| `ShieldDamageSection`, `OverhealDamageSection` | `ChildDamageSectionComponent` | 45,157 | 246,317 | 0 |
| `PreventDeathDamageSection` | `AttachedDamageSectionComponent` | 572 | 10,087 | 10,115 |
| `PMAimToolingTarget` | `/Script/InputTooling.AimToolingSkeletalTargetComponent` | 34,662 | 543,324 | 0 |
| `Usable_PickUp` | `UsableComponent` | 18,512 | 0 | 0 |
| `StealthComp` | `SimpleVisualTimelineStealthComp` | 23,400 | 0 | 0 |
| `StealthV1AddedForAISight` | `StealthComponent` | 11,564 | 0 | 0 |
| `Collision Static Mesh` | `/Script/Engine.StaticMeshComponent` | 10,924 | 1,698 | 0 |
| `Comp_Ability_CooldownComponent1` | Blueprint `Comp_Ability_CooldownComponent_C` | 35,381 | 21,945 | 0 |
| `DamageSection_Vampire_Q_BloodArmor` | Blueprint `DamageSection_Vampire_Q_Heal_BloodArmor_C` | 13,055 | 19,396 | 20,967 |
| `ChooseTeleportSpot_StateComponent` | Blueprint `ChooseMapLocationOnNavMesh_StateComponent_C` | 14,866 | 1,939 | 3,505 |
| `AresAttributeSet_1` | `AresAttributeSet` (wire evidence, below) | 131,014 | 2,550,032 | 0 |
| `AttachedDamageSection` | Blueprint `BasicArmorAttachedDamageSection_C` (its own review, [below](#the-armour-section-attacheddamagesection-2026-09-28)) | 212,885 | 34,124 | 607,676 |

Classes without a module are `/Script/ShooterGame`; the four Blueprint classes
are declared by the replay under their full `/Game/..._C` paths, which is what
the pairs name. In all, the 29 pairs of the first pass move 2,125,995
main-stream and 3,654,139 checkpoint RepLayout rows of the corpus inventory
from a bare group into a declared one; the armour pair, added after its own
review, moves 212,885 and 34,124 more.

`AresAttributeSet_1` is not a component, so it is held to `AresAttributeSet_2`'s
standard instead of the tool's: over all 536 replays that carry it, every
main-stream handle it uses (116 per replay) is declared by the native group in
that replay, every checkpoint's by that checkpoint (10,455 checkpoints), and all
2,681,046 rows are 32 bits wide, the width the named instance has on each handle
(124,280 per-replay comparisons, none different). The only pair whose widths
differ from its target's own rows is `PMAimToolingTarget`, on handle 2:
`AttachParent` is a packed object reference, so 16 bits against 24 is the size
of the NetGUID it carries. For four pairs of the first pass the target has no
rows of its own in any replay, so the width condition says nothing about them
and the other three carry them alone: `StealthComp`, `Collision Static Mesh`,
`DamageSection_Vampire_Q_BloodArmor` and `ChooseTeleportSpot_StateComponent`.
The armour pair is in the same position and was held to the declared property
types instead (below).

Re-exported afterwards (92 replays: one or more for every (pair, build) that
occurs, plus all 38 of 13.06), against the same replays exported by the parent
commit: every row keeps its identity -- time, packet, channel, actor, object,
handle, bit count and raw bits, row for row -- in both `fields.parquet` and
`checkpoint_fields.parquet`; the only rows that change are the remapped groups'
(202,329 main-stream and 334,729 checkpoint rows, every one named, 195,595 and
300,583 typed); `checkpoint_blocks` changes only in the three resolution columns
of the remapped blocks; every other file is byte-identical; `manifest.json`
moves only the overlay counters, where the drop in `overlay_no_field_name`
equals the rows that moved and the rise in `overlay_decoded_ok` plus
`overlay_not_in_table` equals the drop. Decode errors and struct-blob failures
stay 0. `check_component_remaps.py` reads all 29 new pairs `ok` or `absent` on
those exports, none `broken` -- and, run on the parent's exports of the same
replays, `broken` wherever a pair's RepLayout rows appear, so the verdict does
distinguish the two. Rows that are named but untyped -- `CursorWorldLocation`,
`RelativeLocation`, `TargetID`, the two stealth flags, a section's `Life` -- have
no overlay type yet, and none was added -- "type nothing you have not seen
decode", under "What's next" above.

Not added, and why:

| group | rows | why not |
|---|---:|---|
| `PatchVolume` | 113,654 | Class `/Script/DynamicVolume.GroundVolumeComponent`, declared in 955 replays -- but in 915 of them its RepLayout rows include handle 0 (the preserved tails, 2,166 to 36,538 bits), which the target never declares, and handles 20-22 change meaning between builds. Not a name remap. It is a lead for the [PatchVolume investigation](GAS_AND_PATCHVOLUME_INVESTIGATION.md): the declared handle set contains exactly the twelve FastArray item handles that investigation left unassigned (23, 24, 25, 26, 30, 33, 36, 39, 40, 41, 42, 43). |
| `DefenderAnnouncer`, `AttackerAnnouncer` | 52,239 / 41,964 | Blueprint `AnnouncerVOComponent_C`, declared in none of 1,018 replays; every row is ClassNetCache. |
| `BeforePostRoundTransitionSyncTimer` | 36,376 | `SyncedTimerComponent`, declared in 2 of 1,018 replays; every main-stream row is ClassNetCache. |
| `WaitForInitialKillOrAssistState` | 15,993 | Blueprint `StateComponent_WaitForKillOrAssist_Smonk_Child_C`, declared in none; ClassNetCache only. |
| `Comp_Equippable_Subequippable` | 36,702 | No export by that name in 13.06; the group is absent from 13.05 and 13.06 replays too. |
| `Switch_BlackMarket_5`, `RespawningPlummetShootable3_UAID_*` (two), `B_Site_Door_Switch_0`, `RespawningWallPlate2_2`, `RespawningWallPlate2_7`, `Drawbridge6` | 9,607-31,254 | Actors placed in a map, not components: no component export carries these names. |
| `MapTargetingState` | 743,457 | Two classes: the Blueprint `StateComponent_RangeLimited_MultiMapTargeting_C` in 9 packages, native `MapTargetingStateComponent` in 7. No name remap can be right for both. |

**This is the one thing here that a game patch can silently invalidate.** A
renamed component stops matching and its handles go quiet again, and the replay
never named it either, so no unit test can see it.

`tools/check_component_remaps.py` is what watches for that. It needs only an
export -- not the game, not a baseline -- so it works on a replay from a new
build, which is exactly when the question comes up. For each RepLayout pair it
counts the RepLayout rows still bare under the leaf, and any at all is `broken`:
across the 92 re-exported replays not one of the 48 RepLayout pairs left a
single row bare, so healthy is exactly zero. ClassNetCache rows are excluded,
because the RepLayout-only remaps leave their RPC stream bare by design.

The rule used to allow up to 5% of the target's rows, which could not fail where
it mattered: on the 92 replays exported before the 29 pairs above, it read 22
pairs `ok` in 773 (pair, replay) cases whose RepLayout rows were all still bare,
at up to 4.9% of the target (`ShieldDamageSection` beside
`ChildDamageSectionComponent`); only a simulated rename, at 15.6%, tripped it.
The C# reference's four ClassNetCache pairs are judged on the rows they route
-- the leaf's ClassNetCache rows against `<class>_ClassNetCache`, any bare row
`broken`. Their old RepLayout ratio read `DamageHandlerComponent` `broken` on 10
healthy exports of the 1,018 and `absent` on the rest, and left its verdict
unchanged on a scratch build that broke the remap (89,843 rows gone from the
group, 4,563 payloads bare under the leaf on a 13.05 export). RepLayout rows
under a ClassNetCache-only leaf are now a printed count, not a failure.

What the checker cannot see is a rename itself: the old leaf simply vanishes.
The renamed component arrives under its new name, which is why the checker
prints the bare groups no pair claims on every run. When that list or a
`broken` verdict points at a component, re-read its class from the new build's
game with `tools/extract_component_classes` rather than guessing a name.

Of the three names once left here because "the replay declares neither group",
`AttachedDamageSection` is a Blueprint subclass whose group the replay does
declare (a pair now, below), `MapTargetingState` names two classes (above), and
`AresAttributeSet_2` now has `AresAttributeSet_1` beside it.

#### The armour section, `AttachedDamageSection` (2026-09-28)

It met every condition of the pass above and was held back from it, because its
rows are the armour rows the health-and-armour analysis reads. This is that
review. Measured over the same 1,018 exports (main `259ed10`, `--checkpoints`),
with RepLayout rows defined as above and declarations read from `manifest.json`
for the main stream and, for a checkpoint's rows, from that checkpoint's
`checkpoint_export_groups` / `checkpoint_export_fields` joined on
`group_ordinal`:

- **class.** `tools/extract_component_classes --name AttachedDamageSection` on
  the 13.06 containers returns four `_GEN_VARIABLE` exports -- in
  `BasicArmorItem`, `HeavyArmorItem`, `LightArmorItem` and `PlasmaArmorItem`
  -- all of the Blueprint class above, whose native ancestor is
  `AttachedDamageSectionComponent`. No other package uses the name in either
  export shape.
- **what the rows are.** The bare leaf holds 820,561 main-stream rows: 212,885
  RepLayout rows in 529 replays and 607,676 ClassNetCache rows in 900. Every one
  is on an object whose NetGUID path ends in `AttachedDamageSection`, on a
  `PlasmaArmorItem_C` (442,987), `HeavyArmorItem_C` (301,071) or
  `LightArmorItem_C` (76,503) actor. Checkpoints add 34,124 RepLayout rows in
  6,895 checkpoints of the same 529 replays, and no ClassNetCache rows.
- **declared.** The Blueprint group is declared in 1,015 replays, all 529
  among them, always as handle 2 `bAlive` (checksum 622178691) and handle 5
  `LastKnownDamageOwner` (205313645). Each of the 6,895 checkpoints declares it
  as well, 6,357 of them with handle 3 `Life` (962760191) besides.
- **handles fit.** Main-stream rows use handles 2 and 5, checkpoint rows 2, 3
  and 5, and every one is declared by the same replay or the same checkpoint --
  no exceptions.
- **widths fit the declared types.** The Blueprint class has no rows of its own
  to compare widths with, so the rows were held to what the names claim.
  `bAlive` is 1 bit on all 103,363 rows (88,551 main, 14,812 checkpoint).
  `LastKnownDamageOwner` is 8, 16 or 24 bits, and each of its 139,146 rows is a
  packed NetGUID that consumes its window exactly: 0 on 35,208 (20,396 main and
  all 14,812 checkpoint rows), otherwise the `DamageHandlerComponent` whose
  outer is the armour item's own `Instigator` (103,938 of 103,938). `Life` is
  32 bits on all 4,500 checkpoint rows, and read as a float it is never
  negative and never exceeds the item's armour: at most 50.0 on
  `HeavyArmorItem_C` (3,520 rows), 25.0 on `PlasmaArmorItem_C` (698) and 24.56
  on `LightArmorItem_C` (282) -- the game's 50 / 25 / 25 from the armour bullet
  near the top of this page.

Typing: `bAlive` becomes `Bool` through the existing checksum fallback, and
nothing is added to the overlay. 622178691 is the `bAlive` every damage-section
class declares, and `checksum_table.rs` already carries it, learned from the
three native damage-section groups the table types it on
(`AttachedDamageSectionComponent`, `ChildDamageSectionComponent`,
`ChildRegionDamageSectionComponent`). It reads 1 on every one of the 103,363
rows, so the corpus never shows it change; the type rests on the width and on
those same-checksum declarations, not on having seen both values.
`LastKnownDamageOwner` and `Life` have no overlay type and stay named but
untyped (`Not in table`). The checks above say what they are; typing them is a
separate change.

Re-exported with the pair (48 replays covering all 24 builds, among them 25
that carry bare armour rows -- at least one from each of the 20 builds that
have any), against the parent commit's export of the same replays: the other
23 are byte-identical in every Parquet file, and their `manifest.json` is
identical apart from timing and path fields. In the 25, every
row of `fields.parquet` and `checkpoint_fields.parquet` keeps its identity
(time, packet, channel, actor, object, handle, bit count, raw bits), and the
rows that change are exactly the bare RepLayout rows -- 10,578 main-stream and
1,829 checkpoint rows, all now on the Blueprint path. `checkpoint_blocks`
changes only the three resolution columns of 798 blocks (now
`subobject_object_guid_known_remap`, declared), and `manifest.json` only the
six overlay counters: per replay and per stream, the drop in
`overlay_no_field_name` equals the rows moved, the rise in `overlay_decoded_ok`
equals the `bAlive` rows among them (4,341 main, 798 checkpoint), and the rise
in `overlay_not_in_table` equals the rest (6,237 `LastKnownDamageOwner`; in
checkpoints 798 of those and 233 `Life`). Every typed `bAlive` equals its raw
bit (`validate_type_evidence.py --compare-typed`: 5,139 rows, 0 mismatches).
The leaf's 40,838 ClassNetCache rows stay bare, by design. Validate's figures
do not move. `check_component_remaps.py` reads the pair `broken` on each of
the 25 parent exports and `ok` on each of the 25 new ones.

The analysis tools that read these exports were run on both sides of 8 of those
replays (5 with bare armour rows). The healing, section, section-timeline,
kill, match, player-effect, spike-carrier, active-effect, ability, ammo-audit
and FastArray tools write the same output from either export, apart from the
input hashes they record, or refuse the same builds. That includes the healing tool's join on the heal causer's
`class_path`: it reads `Owner` and `Instigator` rows, and the armour group
declares neither name. Three outputs move, each by exactly the moved rows.
`to_valplay_bundle.py` publishes the armour groups' events under the Blueprint
path with `Alive` and `LastKnownDamageOwner` in payloads that were empty, and
counts that many fewer `unnamed_property_rows`; valplay's metrics computed from
the two bundles differ in that loss counter only, and `check_metrics_baseline.py`
passes on 13.01 and 13.04 with no pinned value moving.
`summarize_unresolved_fields.py` moves the untyped rows from the bare leaf's
catalogue entries to the Blueprint group's `LastKnownDamageOwner` and `Life`,
and it and `summarize_value_coverage.py` count the `bAlive` rows as typed.

**It does not reach every armour block.** `unique_leaf_match` runs before this
table and tries the leaf with `Component` appended. In 486 of the 1,018 replays
the replay also declares the native `/Script/ShooterGame.AttachedDamageSectionComponent`
group -- exactly the 486 that carry rows of Phoenix's `PreventDeathDamageSection`,
a native instance -- and there every armour block already resolved to the
native parent before this pair existed: 200,010 main-stream and 32,522
checkpoint rows (`checkpoint_blocks` records those 13,985 checkpoint blocks as
`subobject_object_guid_unique_leaf`). The native group declares `bAlive` only
-- in all 486 main streams and in all 8,806 checkpoints that declare it -- so
there handle 2 is named and typed while handle 5 (116,874 main, 13,985
checkpoint rows) and handle 3 (4,552 checkpoint rows) stay unnamed. No replay
has armour rows in both states. The pair changes nothing in those replays: after
it, the section's properties are under the Blueprint path in 529 replays and
under the native path in 486, and a consumer that wants them has to accept both.
`check_component_remaps.py` reads the pair `absent` in the 486 although the
component is there. Moving those rows as well is a change to the resolution
order, not a table entry, and is not made here.

### Closed: what the three mechanisms cannot reach

After the table, the engine-reference names and checksum propagation, 480,471
rows on 02d4d478 are still untyped across 1,511 `(group, field)` pairs. Sorted
by why:

| rows | why |
|---|---|
| 289,533 | declared `Raw`/`Skip` -- intentional, not a gap |
| 173,535 | has a `compatible_checksum`, but no declared field donates that checksum |
| 17,403 | no checksum at all (the unresolved `AbilitiesAndBuffs` payload) |

The first bucket was 60% of that historical sample: `BaseReplayController`'s
4kbit blob alone contributed 225,808 rows. The previously skipped per-agent
`ReplayLastTransformUpdateTimeStamp` is now typed after the broader wire audit.

Two ordinary additions came out of the second bucket and are now typed --
`StopMovementTime` (`MulticastStopContinuousEffect`; the evidence is at its
`ADDITIONS` entry) and `HandleNumber` (the ForceModule row above). The largest
remaining item does not yield to a `FieldType` at all.

`ClientReplayReceiveInputEventProcessingCapture.InputEventData` (53,605 rows,
one per `PlayerID` row) is a **tagged union**, not a scalar. The leading byte's
top 7 bits are a tag, and the tag fixes the width exactly -- seven tags, five
widths, no exceptions across all 53,605 rows:

| tag | width | rows | | tag | width | rows |
|---|---|---|---|---|---|---|
| 41 | 64 | 18,288 | | 12 | 32 | 3,093 |
| 15 | 32 | 14,724 | | 20 | 40 | 2,961 |
| 6 | 24 | 11,486 | | 28 | 48 | 2,522 |
| | | | | 13 | 32 | 531 |

So the framing is settled and a dedicated decoder in the shape of `effect` or
`structs` could walk it. **What is missing is what any of it means.** Nothing
names the tags: the RPC carries only `PlayerID` beside it, no descriptor
declares the parameter, no checksum donor exists, and the cooked assets do not
help either -- this is engine-side input capture, not a Blueprint property. A
decoder written now would produce seven anonymous payloads, which is what
`raw_bits` already gives. Left alone until there is a source for the tag
meanings.

The September 2026 audit supersedes the earlier decision to leave two time
fields untyped. Both are now `Float`, exposed through `value_f64` while their
ordinary property payloads remain raw as well:

- `ReplayLastTransformUpdateTimeStamp`: 32-bit payloads on 32,978,229 main
  rows across 42 observed character/pawn groups. Values track server time in
  seconds with a file-specific offset from replay time. The audit's median
  offset was near 10 seconds in 672/714 files and 10.8--110.1 seconds in the
  remaining 42. Do not hardcode subtracting 10. Checkpoint rows also exist;
  continuity across checkpoint restoration remains unverified.
- `ServerMovementTime`: 32-bit payloads on 4,571,175 main rows in FiniteSpeed,
  Spline, FloatCurve and Precalculated movement groups. Values behave as an
  actor-relative movement clock in seconds. Channel-open time is an approximate
  reference, not proof of the exact spawn instant; some observed FlareCurve
  actors already report about 0.65 seconds when the channel opens. No named
  checkpoint rows were observed for this field.

These measurements establish wire typing. Game-side epoch semantics and the
cause of recording offsets should not be inferred more precisely than the
observed correlations allow.

### Closed: RPC signature aliasing

Aliasing an undeclared RPC group to a declared sibling -- the `GROUP_ALIASES`
shape -- was measured across all 67 undeclared RPC groups and is **not worth
doing**. The 17 workable pairs would type 165,374 rows, but 139,222 of those are
already reachable by checksum, and the remaining 26,152 rest on nothing but a
shared parameter name, which is the rule this project has already rejected.
`Scale3D` is the example: its alias source is a group the replay's manifest does
not contain at all, so the type would come from an unrelated Blueprint's
same-named property.

Aliasing an RPC group also opens a hazard the class-level aliases do not, since
`resolve_entry` retries the *whole* order against the alias, handle fallback
included, and parameter handles do not correspond between two signatures.
Measured: aliasing `MulticastNotifyHeal` to `MulticastNotifyDamage_Base` reads
`LifeChangeBySection` through the other function's handle table as
`DamageDealt: Float`, on 1,918 rows, every one leaving 145 residual bits.

Bit-width agreement is not evidence here. Every candidate pair consumed its bits
exactly, including the wrong one above; a 1-bit `Bool` read as `EnumByte` also
passes. Width is a necessary condition and nothing more.

### Closed: more name-resolved properties

`ENGINE_OBJECT_REFS` typed 6,048 rows by resolving four `AActor` object
references by name, so the obvious next question was which other properties
deserve the same. The answer, after auditing every field name in the generated
table rather than only the ones this replay spawned, is **none**, and the reason
is worth keeping.

`Owner` was safe because its *encoding* is fixed, not because its name is
standard. `ReplicatedMovement` is just as standard a name and is declared three
different ways in the table -- `RepMovement` with byte rotators and whole units
on 25 groups, with short rotators and two decimals on 1 (Gekko's Wingman), and
`Skip` on 1. The two rotator widths consume different numbers of bits, so a
name rule there would desync the block; the location level consumes the same
bits either way, so a name rule would read a value 100x off without a single
error -- the failure the
[per-class level](#replicatedmovementlocation-is-world-units-at-a-per-class-level)
exists to prevent.
`RelativeScale3D` and `CosmeticRandomSeed` split the same way, and only on
groups this replay never spawned -- measuring against the wire alone would have
called both of them clean.

**RPC parameters are categorically excluded from a *name* rule.** A parameter
name is scoped to one function signature: 38 of the 211 parameter names in this
replay carry more than one `compatible_checksum`, which is exactly the same
statement the checksum makes. Typing them by name would merge signatures that
the schema itself distinguishes.

`AllianceFilter` is not a counterexample, though it was once offered as one:
every group declares it under checksum 2270825073 and every row is 3 bits,
which `EnumByte` and `EnumRemainingBits` read alike. Its two donor types did
make the checksum learner drop it, leaving raw the five RPCs that carry it
with no declaration of their own -- 4,560,248 of the 16,030,813 rows under the
checksum in the 1,018-replay audit, all `AllianceAny` (3) -- until
`apply_type_corrections.py` declared the third donor `EnumByte` too.
`ReplicatedMovement` is still dropped, and should be.

Three names did clear the mechanical bar -- `AttachComponent` (declared `Raw`,
so it would produce no values at all), `PreventPickupCharacter` and
`OwningPrimaryDataAsset`. Together they would fill 59 rows of 1,255,920, and
each is a claim about Valorant's class hierarchy rather than about Unreal, so
each moves with a game patch. Not worth the standing risk.

Two loose ends found on the way, neither a live bug: `RemoteRole` is declared
`ObjectNetGuid` on one group and `Skip` on 36, and `StartTimeStamp` is `Double`
on one and `Float` on another. `RemoteRole` never appears on the wire in this
corpus, so nothing decodes through the odd entry.

Generated files are never hand-edited; the only path is in
[CONTRIBUTING.md](../CONTRIBUTING.md#generated-files--never-hand-edit).
