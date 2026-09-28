# September 2026 corpus follow-up

**Header-order correction:** The partial missing-initial/rejection figures below
are historical parser classifications. [The corrected header audit](PARTIAL_HEADER_CORRECTION.md)
reassembles all 961,004 observed fragments; the source data was present.

This is the earlier tail-preservation batch. The subsequent
[schema expansion and full partial-cause audit](SCHEMA_EXPANSION.md) supersedes
its typed-value percentages and four-file-only cause classification. The
before/after figures below remain the dated results of this earlier batch.

This note records the final measurements for the September 2026 batch and
separates completed parser work from unresolved transport loss. Measurements
cover 714 preserved replays: 215 build 13.01, 204 build 13.02, 108 build 13.04
and 187 build 13.05. The original export used commit `794e678`.

## Implemented and measured

All 714 replays were freshly exported with checkpoints. The main table contains
999,655,890 field rows and the checkpoint table contains 53,019,206. The
post-RepLayout change accounts for the exact increases over the earlier tables:

| stream | earlier rows | added tail rows | final rows | decoded CNC tails | whole raw tails |
|---|---:|---:|---:|---:|---:|
| main | 999,415,211 | 240,679 | 999,655,890 | 169,018 | 71,661 |
| checkpoint | 52,965,624 | 53,582 | 53,019,206 | 5,265 | 48,317 |

Every previously abandoned post-terminator tail is now represented. A tail
that passes the verified ClassNetCache framing becomes a
`__vrfkit_chained_cnc_h1__` row; a tail that does not is retained whole as
`__vrfkit_unparsed_rep_layout_tail__`. The latter stays an unresolved raw RPC
payload. This removes 240,679 main and 53,582 checkpoint field-stream failures
without assigning semantics to unknown bytes. The final diagnostics report
3,957,736 unresolved main RPC payloads and 48,317 checkpoint payloads, all
preserved.

The resulting content-block counters are clean: 472,410,605 main blocks and
13,048,185 checkpoint blocks, zero malformed framing, overlay or struct errors,
and zero `content_blocks_lost`. These figures describe blocks that reached
content-block framing. They are not an end-to-end losslessness result.

## The remaining transport gap

The same full run reports 125,037 main and 835,967 checkpoint
`partial_errors`. These are partial-reassembly rejections before content-block
framing; their payloads are discarded and therefore never enter the
`content_blocks_lost` denominator. Both `partial_fragments` and
`partial_completed` are zero in this corpus run. Consequently a 100% block
score or `content_blocks_lost = 0` does not prove that every ReplayData payload
was retained.

A bounded probe using the isolated `794e678` baseline examined one replay from
each of builds 13.01, 13.02, 13.04 and 13.05. Every sampled error was a
continuation without an active partial accumulator; none was an alignment,
resource or sequence failure. Main errors were 131 / 268 / 131 / 169, carrying
1,628,083 / 3,536,358 / 1,633,071 / 2,132,139 discarded bits; checkpoint errors
were 970 / 1,316 / 881 / 1,342. The Rust header and partial-state interpretation
matched the C# parser on these samples. There is no evidence in this bounded
check of an implementation defect or that the standalone continuations are
recoverable, but four files do not establish the cause distribution for all
714. Describe the current result as complete preservation of measured content
blocks, with a known pre-framing transport gap.

## Typing and data dictionaries

Physical typed-value coverage is 698,945,941 of 999,655,890 main rows
(69.9187%) and 21,894,478 of 53,019,206 checkpoint rows (41.2954%). These are
non-null typed-value fractions, not percentages of game facts understood. The
two audited time fields add 37,549,404 verified main values and 52,306
checkpoint values. Their 32-bit Float wire type is established; their exact
game-side epochs are not.

`extract_ability_stats.py` paired 155,150 serialized array-element snapshots
across all 714 exports, including checkpoints, with zero structural issues,
unknown mappings or collisions. Builds 13.01, 13.02 and 13.04 exposed 31 IDs;
13.05 exposed 32. These counts are replicated snapshots, not ability casts,
and unobserved IDs remain unknown.

`extract_match_observations.py` also ran over all 714 exports. Its outputs are
evidence-labelled state changes and snapshots: 2,369,843 ammo changes,
4,677,915 equip intervals, 123,073 reload intervals, 2,596,769 defuse progress
transitions, 482,849 money decreases and 461,657 purchase-state snapshots.
They must not be promoted to shot, reload or purchase event totals without
independent event evidence. The player join for round balances resolved
258,680 of 261,360 rows; unresolved ownership remains null.

## Guardian path mapping

The canonical C# path has directory `Dmr`; build 13.02 onward also uses `DMR`.
The generator adds only this observed alias, including package lookup form. It
retains the old spelling and does not make arbitrary paths case-insensitive.
The 714-export lookup guard covers 5,426 old-path and 11,804 new-path NetGUID
rows, with zero unresolved after the fix. The new path occurs in 488 files;
this is an occurrence count, not a count of affected user-visible matches.

## Reproducibility and baseline reconciliation

The complete 714-export corpus was produced by the release binary with SHA-256
`c546e7c8aeb9242bfea8c038add1419e4e70146e40a772ac133cefa20b017e82`.
An intermediate binary,
`d450544f9290423fbc633d9418868469ff299f09046f3d72e4f8d5fccdb67ea8`,
changes only oracle PASS wording. Eight stratified reexports under that
binary matched all six tables byte-for-byte. This establishes table identity
for those eight controls; it is not a second 714-file export or a claim of
end-to-end ReplayData losslessness.

The final CLI binary is
`ac9894c932f779e6fe39cce663d2a72f7ced11a606213a4df71da22c2e9b540d`.
It additionally names the partial-reassembly scope exclusion and corrects
the skipped-bit annotation, without changing parser logic or exit criteria.
Its repeated diagnostic JSON equals the corpus binary on `02d4d478`, and both
main/checkpoint export baseline guards pass. The original corpus binary and
its outputs remain separately identified.

The export baseline was reconciled against the preserved pre-change binary
before updating it. That binary already differed from the committed fixture:
`overlay_no_field_name` was 2,027 rather than 1,996, `overlay_not_in_table` was
171,276 rather than 171,307, and `skipped_bits` was 19,430,174 rather than
19,135,006. Its field file was 15,880,541 bytes rather than 15,880,976.

On `02d4d478`, the final main table has 1,277,983 rows: 1,277,658 earlier rows
plus 218 decoded and 107 raw tail rows. The checkpoint table has 78,924 rows:
78,850 earlier rows plus 9 decoded and 65 raw tails. Independent raw-to-typed
checks cover 45,993 main and 74 checkpoint time values. Both baseline files
were updated together so their shared main tables and counters remain equal.

## Historical A/B result

The historical 100% example was independently reproduced with exact commits
`185d452` and `a73ee3a`, both built with Rust 1.86. On `02d4d478`, both see
608,020 total / 608,011 non-deleted blocks, 429,637 fields and 342,735 RPCs.
The change adds 325 reported field-stream failures and 295,168 skipped tail
bits, changing the verdict from 100% to 99.946547%. It makes previously
discarded post-terminator data visible; the report's 743,110-to-608,011
population-change explanation does not reproduce for this file and commit pair.
On a second 13.01 input, the same mechanism adds 384 failures on an unchanged
628,215-block denominator. Six exported tables retain their row counts; other
changes bundled into `a73ee3a` alter some field names/string values. This is
evidence about these two inputs and the tail-accounting change, not a proof
that every historical decoder change was regression-free. Both revisions
reject 13.05 as unsupported, so there is no historical 13.05 comparison.

The diagnostic replay of saved tails also corrected the original failure
attribution. The 9-bit shape is a checksum flag plus a packed zero handle. The
185/201/217-bit shapes reach a valid zero terminator after handle 61;
`CachedAttributeSet` is the preceding successful field, not an overrunning
field. Consuming all remaining bits was never sufficient evidence for a
decoder. The implemented path therefore accepts only verified CNC framing and
preserves every other tail whole.

## The damage record only vrfkit emits

Explained 2026-09-28. On `02d4d478` vrfkit exports one
`MulticastNotifyDamage_Point` that neither C# build (upstream `b51d674`,
vendored `8824794`) emits: packet 391880, actor 27232, subobject 27244,
channel 194, a killing blow of 29.45 dealt / 20 taken on Gekko's Dizzy
(`Projectile_E_Aggrobot_DiscTurret_PowerWave_C`), whose channel closes six
packets later. It used to be the last item under *Remaining work*, and
`compare_rpc_params.py` exited 1 on it. The record is on the wire, both parsers
decode it identically once C# knows the component's class, and vrfkit is
right to export it.

**Cause: a C# class-resolution gap, not a parse difference.** Subobject 27244
is stably named `Damageable`, and a stably-named subobject's content-block
header carries no class NetGUID: `ReadSubobject` in
`src/Replay.Unreal/Bunches/ContentBlockHeaderReader.cs` returns before reading
one. For such a block `ResolveSubobjectClassPath` in
`src/Replay.Unreal/Bunches/ContentBlockPathResolver.cs` falls back to
`KnownSubobjectClassPaths`, a leaf-name table with four entries
(`ReplayEffect`, `EffectManager`, `LocationalEffectManager`,
`DamageHandlerComponent`). `Damageable` is not one of them, so the class path
is null, `FrameClassNetCacheContentBlock` in `ContentBlockFramer.cs` skips the
1,522-bit block, and the only event it raises is an undecoded
`ExportGroupReceived`, which `EmitExportGroup` in
`src/CliReader/JsonExport/ReplayExportSink.cs` drops from `events.ndjson` (it
is counted in the manifest's `<unresolved>` `class_net_cache` bucket). Hence
no event at all for the packet. The bunch is not partial, consistent with
vrfkit having exported the record already at `d4731c8`, before the partial
header correction. The resolver is identical in `b51d674` and `8824794`.
Upstream `d23c13e` moved the table into `ValorantDescriptors.cs`
(`AddSubobjectClassPath`) and still has no `Damageable`. At the vendored
commit the table lives in `Replay.Unreal`, not in the vendored
`Replay.Valorant`, so nothing under `third_party/vrp/` changes.

vrfkit reaches the class through its schema-driven fallback instead: in
`crates/vrfkit/src/sink/paths.rs` the bare name matches no declared group, so
`resolve_function_count` calls `resolve_cnc_for_instance_name`
(`crates/vrf-schema/src/resolve.rs`), whose `Component_ClassNetCache` suffix
finds `/Script/ShooterGame.DamageableComponent_ClassNetCache`, the only group
the replay declares with that leaf. This half is read from the code and the
replay's declared groups, not from a runtime trace; what establishes that it
binds the right class is the evidence below.

**Method.**

1. Record diff, keyed by packet, actor, subobject, channel and function, of
   the pinned reference against main `259ed10`'s export: 729 C# records, 730
   vrfkit, exactly one vrfkit-only (this one), none C#-only.
2. A scratch build of `8824794`, instrumented to trace channel 194 over
   packets 391850-391900 and otherwise unchanged: its `rpc_params.ndjson`,
   `combat_report.ndjson` and `manifest.json` are byte-identical to the pinned
   reference, and its full `events.ndjson` has no line for packet 391880. The
   trace shows one reliable, non-partial 1,565-bit bunch on the open channel,
   every pipeline stage continuing, and one ClassNetCache block for object
   27244 (`Damageable`, stably named, class GUID 0) whose class path is null.
3. The same build with one table entry added, `Damageable` ->
   `/Script/ShooterGame.DamageableComponent`. The block then decodes as
   `MulticastNotifyDamage_Point` (handle 1), consuming 1,522 of 1,522 block
   bits and 1,503 of 1,503 RPC bits, 35 fields. The manifest moves exactly one
   1,522-bit block from `<unresolved>` (13,575 to 13,574) to
   `DamageableComponent` (3,707 to 3,708), `rpc_received` rises by one, and
   the other 729 records and `combat_report.ndjson` are byte-identical.
   `compare_rpc_params.py` against that output matches all 730 records. All 35
   C# parameters equal vrfkit's, including the 185 raw bits of
   `LifeChangeEvents`.

**Why vrfkit is right.** Beyond the two parsers agreeing bit for bit, the
record agrees with other streams of the same replay, none of which is derived
from it:

- The shooter is pawn 1466 (`Hunter_PC_C`). Its `PlayerState` is 268, which
  is the record's `DamagerPlayerState` and `KillCreditPlayerState`. Its
  `CurrentEquippable` has been 26796 since packet 385925; 26796 is the
  record's `EquippableUsed`, a `RevolverPistol_C` whose `Owner` is 1466.
- In `movement.parquet`, 1466 stands at (6117.0, -4541.7, 500.4) from
  1,333,856 to 1,333,905 ms. `DamageOrigin` (6116.92, -4541.69, 576.41) is
  that point raised 76.0 units, and 1466's view (yaw 264.1, pitch 6.6
  degrees) matches `DamageDirection` (264.28, 6.45).
- `DamageDirection` is within 0.31 degrees of the unit vector from
  `DamageOrigin` to `DamageImpactLocation` (5748, -8221, 994), 3,721 units
  away. `DamageDealt / FalloffMultiplier` is 55.0, a whole number, as it is
  for all 581 damage records of the replay.
- 1466 and Gekko (576) killed each other three times each. Two-colouring the
  killer/victim pairs of all 132 `MulticastNotifyKilledEnemy` records has no
  conflict and puts them on opposite teams.
- The victim is the projectile itself: `DamagedComponent` 27266 (`Hitbox`)
  and `LifeChangeEvents[0].ChangedComponent` 27242 (`ChildDamageSection`) are
  both subobjects of actor 27232, and the change takes its life from 20 to 0.
  Two packets later the actor's `Destroyed Logic` RPC arrives, its continuous
  effects stop at 391883 and 391885, the channel closes as `Destroyed` at
  391886, and at 391895 `Projectile_E_Aggrobot_OrbSpawner_C` opens on the
  same channel 78 units from the impact point.

**What changed.** `compare_rpc_params.py` now compares records (identity plus
the compared values) as well as per-parameter value multisets, and lists this
record as its one expected difference, keyed by the replay's SHA-256 (read
from the reference's `manifest.json`), packet, actor, subobject, channel,
function and the four compared values. It is excluded only when it occurs
exactly so. On this replay anything else -- C# gaining the record, vrfkit
losing it, other values, a second record at the same identity -- is reported
STALE and exits 1, and any other difference still exits 1. Without the
manifest the replay is unknown, nothing is applied, and a run that otherwise
matches exits 2, because a stale entry could not be seen. Measured on
`02d4d478` with main `259ed10`'s export: against the pinned reference the tool
exits 0; against the output of step 3 it exits 1, reporting the entry STALE;
with the manifest removed it exits 1 on the pinned reference and 2 on step 3's
output. `tools/tests/test_compare_rpc_params.py` drives the real loaders over
written files for each of those cases. Disabling the packet match, the value
check, the replay check, the C#-side check, the multiplicity check, the stale
verdict, the record verdict, the unknown-replay verdict, or treating a missing
manifest as this replay, each makes at least one of its tests fail.

**Scope.** The gap covers every RPC on a subobject named `Damageable`, not
only Dizzy's. In a full-corpus export made with the main `259ed10` release
binary (1,018 unique replays, builds 11.06 to 13.06), vrfkit emits 1,989 such
invocations (1,318 `MulticastNotifyDamage_Point`, 671
`MulticastNotifyDamage_Base`) in 391 replays, on the projectiles of six
classes: `Projectile_Guide_E_HawkFlash_C` 831,
`Projectile_E_Aggrobot_DiscTurret_PowerWave_C` 480,
`Projectile_Killjoy_4_RemoteBees_MultiDetonate_C` 454,
`Projectile_Clay_Q_Satchel_Arming_C` 194, `Projectile_Gumshoe_Q_CageTrap_C`
17 and `Projectile_Gumshoe_4_CageTrap_C` 13. Counted from `net_guids.parquet`
(path exactly `Damageable`) and the `_ClassNetCache` parameter rows on those
objects, one invocation per distinct packet, channel, actor, object and
function. Only the record above was checked against C#; no comparison covers
the rest.

## Remaining work

- ~~Add full-population reason counters for the 125,037 main and 835,967
  checkpoint partial-reassembly rejections.~~ Superseded: the
  [header-order correction](PARTIAL_HEADER_CORRECTION.md) reassembles all of
  them, per-cause counters and `partials.parquet` preservation now exist, and a
  2026-09-13 sweep of 839 replays (main and checkpoint) found 0 rejected rows.
  The counters themselves were then shown able to read zero over a dropped
  bunch when the packet reader's partial tracker disagreed with the
  accumulator; that was fixed on 2026-09-13, with tests in
  `crates/vrf-net/src/pipeline/mod.rs` that fail if it returns.
- Establish semantics for whole raw post-RepLayout tails, unresolved RPC
  payloads, InputEventData tags, GAS words and StopEffectType before adding
  typed fields or game-action labels.
- Treat derived match observations as replicated evidence. Require ownership
  joins and event-specific evidence before publishing cast, shot, reload,
  damage or purchase counts.
- Re-run the component-remap and mapping guards on each new supported build;
  the replay does not reveal Blueprint-to-native class aliases by itself.
- Type Raze's satchel, Paint Shells and rocket `ReplicatedMovement` if they
  are wanted. They were declined only because the reader read every location
  at /100 ([UPSTREAM_RAZE_WARDEN.md](UPSTREAM_RAZE_WARDEN.md)); the level is
  per class now ([DATA.md](DATA.md#replicatedmovementlocation-is-world-units-at-a-per-class-level)),
  so each needs an entry with its measured level and a spawn-join line in
  `REP_MOVEMENT_LOCATION_EVIDENCE`.

Considered on 2026-09-14 and deliberately not done, each with the reason:

- Clearing `has_partial_error` / `partial_error_kind` at the top of
  `PartialBunchAccumulator::add_fragment` as well. Its only caller, the
  pipeline, already strips the packet reader's verdicts before the call, and
  the pipeline tests fail if it stops; no `bunch.rs` test presets the flag. It
  would only matter to a second caller, so it belongs with the first one.
- The `BitReader::with_bit_len` failure arm after `take_completed` in
  `crates/vrf-net/src/pipeline/mod.rs`. The accumulator sizes the buffer to the
  bit count it returns, so no input reaches it. If one ever did, the partial
  error would surface as unclassified in the cause cross-check, but the payload
  would not reach `partials.parquet` and the close handling after it would be
  skipped. Preserving it needs a new `PartialPayloadReason`, a
  `partials.parquet` change for a path nothing reaches.
- Committing raw bits cut from private replays so CI's real-bytes type check
  covers KillData, SelectedV2, HealCauser and the other fields typed in #10. CI
  covers the eight typed fields the public fixtures carry; the rest would put
  private replay content in this public repository, which
  [SCHEMA_EXPANSION.md](SCHEMA_EXPANSION.md) rules out. They stay covered by
  the machine-local corpus sweeps.
- Updating the test counts in [CURRENT_STATUS.md](CURRENT_STATUS.md). They
  describe its dated 2026-09-09 validation run and say so.

Considered on 2026-09-28 and deliberately not done:

- Deleting the packet reader's partial tracker (`track_partial_bunch` in
  `crates/vrf-net/src/packet.rs`). Nothing in this workspace reads what it
  produces -- the pipeline strips its header flags, and never sums
  `PacketReadResult::partial_error_count` -- but `RawPacketReader::read_packet`
  and that field are published API, and an extractor built directly on the
  reader is on record in [TRANSPORT_PRESERVATION.md](TRANSPORT_PRESERVATION.md).
  Deleting the tracker would remove the field or leave it a permanent 0, which
  is the counter-that-cannot-move shape this repository refuses. It stays,
  documented as advisory, and the pipeline's strip keeps it that way.
