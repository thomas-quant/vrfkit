# GAS references, inner words, and PatchVolume investigation

Measured 2026-09-09 against the accepted `fc50bfe` parser exports. The source
checkout at the start of this investigation was `df13580`. These findings
correct interpretation claims and constrain the next decoder experiments;
they do not add typed Parquet values or change the 72.5608% physical typed
presence reported in [current status](CURRENT_STATUS.md).

## GAS OwnerActor and AvatarActor: full 714-export census

The component remap exposes both field names under
`/Script/ShooterGame.AresAbilitySystemComponent`, but their typed value columns
are empty. An independent raw reader checked exact Unreal packed-u32 windows,
field handles/checksums, and equality with the enclosing actor GUID.

| Field | Main rows | Checkpoint rows | Values different from enclosing actor |
|---|---:|---:|---:|
| OwnerActor, handle 11, checksum 3069409379 | 161,102 | 153,687 | 0 |
| AvatarActor, handle 12, checksum 1829222345 | 161,102 | 153,687 | 0 |
| Combined | 322,204 | 307,374 | 0 |

All 629,578 values use exact 16- or 24-bit packed windows. Every one of the
314,789 packet/channel/actor/object coordinate groups has one of each field,
and both values agree. Checkpoint index is included in checkpoint identity.
Input hashes were checked before and after each read and against the accepted
parser comparison. A separate twelve-file raw reader reproduced its pilot
population with scoped actor-class lookups.

This is useful component-context evidence. It supplies no distinct player
owner that can be substituted for a heal or ability instigator. Actor GUID
equality is not a lifecycle or player-credit proof.

## Healing reference roles: twelve-file pilot

The accepted healing observations retain direct EventInstigatorPawn and the
causer actor's replicated Owner and Instigator separately. Exact equality
comparisons of 21,448 pilot observations produced:

| Comparison | Equal | Different | Unknown |
|---|---:|---:|---:|
| EventInstigatorPawn vs recipient | 20,179 | 1,127 | 142 |
| EventInstigatorPawn vs causer Owner | 19,869 | 1,437 | 142 |
| EventInstigatorPawn vs causer Instigator | 21,306 | 0 | 142 |
| Causer Owner vs causer Instigator | 19,869 | 1,437 | 142 |

The 142 observations have no causer evidence. Equality uses only references
whose accepted status is present; it never fills an unknown reference with
zero. Each association retains the observation identity, raw source ordinals,
causer class, recipient lifecycle status, and original reference values.
Saved observation hashes match the accepted healing-run receipts.

These results reject a universal Owner-equals-Instigator assumption. They
support preserving explicit role edges in the next consumer, not choosing
one generic owner column. They do not establish effective healing amounts or
player credit, and the twelve-file equality proportions are not corpus-wide
measurements. See [healing observations](HEALING_OBSERVATIONS.md).

## AbilitiesAndBuffs: from opaque words to FastArray structure

The twelve-file audit contains 45,015 whole preserved payloads and 45,015
inner `_cnc_h1` windows. Every observed inner window equals the parent after
its 22-bit outer framing. Every leading flag is true; the u32-word count and
sub-word residual length vary.

Within actor/object/channel sequences, 44,497 adjacent pairs have increasing
first words. In 1,828 of those pairs, the second word does not equal the
previously observed first word. Leading pairs also recur across distinct
identities within an export. These are sequence observations, not proof of
an engine `PredictionKey {Current, Base}` type or a unique ability-cast key.
The existing bit-slicing helper accepts arbitrary nonempty bit windows, so
successful decomposition cannot establish the word meanings.

The parser comments and the legacy `key_pair()` accessor documentation now
describe raw words instead of asserting those game-side roles.

A primary wire implementation supplies a stronger interpretation:
[`ReplayReader.cs` at `6931a70`](https://github.com/michel-giehl/ValorantReplayParserPlayground/blob/6931a70b644c3d5157da71f87aba80f7626a99f9/src/Unreal.Core/ReplayReader.cs#L1557-L1635)
reads a custom-delta support bit followed by four signed little-endian i32
words: ArrayReplicationKey, BaseReplicationKey, NumDeletes, NumChanged.
Deleted IDs follow, then each changed ID and its packed handle/width property
stream. This identifies replication bookkeeping, not a PredictionKey or cast.

The cohort-wide variant has **no checksum bit before each changed item's
property stream**. All 45,015 pilot bodies close exactly, including 7,537
deletion-only bodies. A checksum-present reader also closes 93 changed bodies,
so trying variants and accepting the first per-row success is unsafe.

An independent reader then checked all 714 accepted exports, including a scan
of checkpoint fields. Every one of the 2,882,152 main inner windows closes
exactly with the fixed no-checksum variant. No matching checkpoint inner rows
were present. Input hashes match the accepted parser comparison before and
after each read.

| Structural output | Count |
|---|---:|
| Fully consumed inner windows | 2,882,152 |
| Deleted item IDs | 869,423 |
| Changed items | 2,454,327 |
| Raw property windows inside changed items | 36,814,905 |
| Unconsumed or invalid bodies in this population | 0 |

These are serialized updates and numeric fields. Repeated updates do not
become distinct abilities, effects or casts. Item IDs are FastArray IDs,
not actor NetGUIDs. Missing schemas prevent authoritative property naming and
value interpretation. Parent and inner rows overlap, so their counts must not
be added as independent data.

### Extract the numeric observations

```bash
python tools/extract_fastarray_observations.py --export-dir <export-directory> --out-dir <new-output-directory>
```

The new directory contains `observations.ndjson` and `receipt.json`. Each
observation retains the source table, physical row ordinal, packet/channel/
actor/object coordinates, exact original raw bits, header, deletion IDs, and
changed IDs with zero-based numeric handles and bit offsets/lengths. Offsets
refer to the original inner window; raw property values can be sliced without
guessing their types. The receipt includes source and output hashes, explicit
success/rejection counts and item/field totals.

The tool accepts the measured main route on builds 13.01, 13.02, 13.04 and
13.05. It preserves malformed or unvalidated observations with a rejection
reason and returns nonzero if any are rejected. Checkpoint occurrences and
future builds remain unvalidated. Output must be a new directory outside the
source export. Existing Parquets and their typed-value counts do not change.

The public extractor was run on all 714 exports and its saved observations
were compared with a separate reader. Every source identity, original raw
window, header value, deleted ID, changed ID, and field handle/offset/width
agreed. The retained NDJSON files total 5,181,243,102 bytes; all 2,882,152
observations are structurally exact and the rejection count is zero. This
validates the extraction artifact, not the still-unknown field meanings.

## PatchVolume: numeric structure with an unresolved item schema

One positive export per build supplies 60 whole/tail rows; the bounded wire
experiment selects 42 whole payloads and 15 tails. The observed bare
PatchVolume objects have direct actor outers across Deadeye, Sarge and
Aggrobot patch classes. Their enclosing actors' local CNC function tables
differ. Those actor tables do not establish the subobject's own function
table, even when the final GUID cache identifies its outer.

A nominal function-count-2 walk consumes each sampled window as one handle-0
CNC entry. That is only an outer framing candidate. Altering a window can also
produce a clean walk at another function count, so complete outer consumption
alone does not justify a class remap or a named function. The reference C#
reader explicitly dispatches custom-delta properties separately from RPCs
after shared CNC framing. A CNC entry is not automatically a function call.

The same FastArray grammar now consumes all 57 sampled PatchVolume bodies
exactly: 536 changed items, each with twelve numeric handles
`23,24,25,26,30,33,36,39,40,41,42,43`. The checksum-present control closes none.
There are no deleted-item or zero-change cases in this pilot. Several local
export groups contain these handle numbers; numeric inclusion alone cannot
choose a group or authorize field names. The raw windows remain authoritative.

The subsequent full 714-export run finds 19,140 whole unresolved CNC windows
and 7,163 preserved RepLayout tails: **26,303 main windows**, all fully consumed
by the fixed FC2-handle-0 outer framing and no-checksum FastArray body.
They contain 241,721 changed items and 2,900,652 raw property windows. Deleted
item count is zero; no matching checkpoint windows were present. The alternate
checksum mode happens to close eight full-corpus windows, reinforcing why
per-row variant selection is not a reliable grammar decision.

An independent integer-based reader rescanned every source, reproduced the
outer framing and every header/ID/field boundary, and checked each saved raw
window and context. It also corrected the initial agent output's filtered-row
rank to an actual zero-based physical Parquet row ordinal. The accepted 714
NDJSON files total 245,312,914 bytes, including empty files for exports with no
selected PatchVolume rows. Before/after source hashes match the accepted parser
comparison. The original agent output remains separately preserved as v1.

The whole and tail records are separate serialized observations; this does
not establish that they are the same update or that one is a suffix of the
other. No actor-class remap, named PatchVolume property decoder or typed
Parquet change is introduced. The private extraction demonstrates numerical
structure; its property values remain raw pending an independent item schema.

**Follow-up (2026-09-28): schema established.** The replays declare it.
`PatchVolume` is a `/Script/DynamicVolume.GroundVolumeComponent`, the windows
carry its ClassNetCache property `FragmentInfo`, and the twelve item handles
are that class's declared cell members (`X`, `Y`, `ConvexHullPoints`,
`Floor`, `Ceiling`, ...), at handles that move between builds. With the
declared names, checksums and element grammar, all 36,661 windows of the
1,018-export corpus decode exactly with `tools/extract_ground_volumes.py`, and
the values pass a second reader and the owner-spawn geometry checks. See
[ground-area volumes](GROUND_VOLUMES.md).

## Next work

- Keep the four healing reference roles separate in any association schema;
  require source-time lifecycle and identity evidence for further joins.
- Resolve the changed-item property schemas for the now-consumed GAS windows.
  Do not promote raw property widths or GUID-number coincidences to semantics.
- Done: the PatchVolume item schema, from the replays' own declarations --
  see [ground-area volumes](GROUND_VOLUMES.md). `Status` and `bIsActive`
  meanings remain open.
- The section arithmetic discrepancies and InputEventData action meanings
  remain open, as recorded in [the current backlog](CURRENT_RAW_BACKLOG.md).

Private evidence is retained under `gas-reference-census-root`,
`gas-reference-investigation-root`, `healing-role-association-root`,
`gas-inner-investigation`, `fastarray-root-corpus`, `fastarray-observations-corpus`,
`gas-fastarray-main-comparison`, `patchvolume-investigation`,
`patchvolume-fastarray-entries`, and `patchvolume-fastarray-root-accepted`.
The reports keep
sample scope and source/output hashes; private player observations are not
included in this repository document.
