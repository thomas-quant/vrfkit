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

When first published on 2026-09-09, the tool accepted the measured main route
on builds 13.01, 13.02, 13.04 and 13.05. Since the
[2026-09-28 re-measurement](#build-scope-re-measured-2026-09-28) it accepts 22
builds: every supported build except 12.10 and 12.11. It preserves malformed or
unvalidated observations with a rejection reason and returns nonzero if any are
rejected. Checkpoint occurrences remain unvalidated, and so does any build that
is not listed, including every future one. Output must be a new directory
outside the source export. Existing Parquets and their typed-value counts do
not change.

The public extractor was run on all 714 exports and its saved observations
were compared with a separate reader. Every source identity, original raw
window, header value, deleted ID, changed ID, and field handle/offset/width
agreed. The retained NDJSON files total 5,181,243,102 bytes; all 2,882,152
observations are structurally exact and the rejection count is zero. This
validates the extraction artifact, not the still-unknown field meanings.

### Build scope re-measured, 2026-09-28

The 2026-09-09 figures above are kept as the original 714-export measurement.
This entry measures a later and larger export set: all 1,018 unique replays
across the 24 supported builds, exported with checkpoints by
`tools/verify_build_corpus.py` at parser commit `259ed10`. The manifests do not
record the parser commit. The audit's provenance links the exports to it
through the executable's SHA-256, and the audit reports that executable
unchanged over the run.

**Method.** Both `fields.parquet` and `checkpoint_fields.parquet` were scanned
for `group_path` `AbilitiesAndBuffsComponent` with `field_name` `_cnc_h1` or
`__vrfkit_chained_cnc_h1__`. This is the extractor's selection without its
handle filter. Each window was read three ways:

- by the extractor's own `decode()`;
- by an independent reader that shares no code with it (the whole window as
  one integer, shifted per read);
- by that independent reader with one extra flag bit after each changed ID.
  This is the checksum-present variant (`ChecksumMode::Present` in
  `crates/vrf-decode/src/fastarray.rs`), run as a control.

The first two were compared on every header word, deleted ID, changed ID and
field handle/offset/width.

| Build | Replays | Windows, all exact | With changed items | Of those, control also closes |
|---|---:|---:|---:|---:|
| 11.06 | 3 | 8,937 | 6,574 | 0 |
| 11.07 | 3 | 23,967 | 21,818 | 11 |
| 11.08 | 3 | 13,931 | 11,799 | 121 |
| 11.09 | 3 | 11,793 | 9,488 | 69 |
| 11.10 | 3 | 8,319 | 6,302 | 0 |
| 11.11 | 3 | 10,784 | 8,082 | 82 |
| 12.00 | 3 | 11,125 | 8,995 | 0 |
| 12.01 | 3 | 14,804 | 12,258 | 0 |
| 12.02 | 3 | 23,344 | 20,935 | 126 |
| 12.03 | 3 | 16,389 | 14,101 | 137 |
| 12.04 | 3 | 23,317 | 20,377 | 0 |
| 12.05 | 3 | 14,236 | 11,924 | 466 |
| 12.06 | 3 | 19,719 | 17,848 | 0 |
| 12.07 | 3 | 14,629 | 12,246 | 95 |
| 12.08 | 3 | 19,901 | 17,575 | 1 |
| 12.09 | 3 | 15,509 | 13,481 | 225 |
| 12.10 | 1 | 0 | 0 | 0 |
| 12.11 | 1 | 0 | 0 | 0 |
| 13.00 | 1 | 6 | 3 | 2 |
| 13.01 | 215 | 762,412 | 619,258 | 20,480 |
| 13.02 | 205 | 855,152 | 704,717 | 3,078 |
| 13.04 | 108 | 459,718 | 386,483 | 1,597 |
| 13.05 | 401 | 1,520,893 | 1,245,517 | 1,873 |
| 13.06 | 38 | 150,608 | 123,425 | 66 |
| Total | 1,018 | 3,999,493 | 3,293,206 | 28,429 |

- All 3,999,493 selected windows are main `fields` rows named `_cnc_h1` with
  handle 1. Both readers consume every one exactly and agree on every
  boundary. Together they carry 1,217,291 deleted IDs, 3,403,315 changed items
  and 51,049,725 raw property windows.
- No `checkpoint_fields` row in any export has group
  `AbilitiesAndBuffsComponent`, so the checkpoint route remains unvalidated.
- The 12.10 and 12.11 exports have no rows with group
  `AbilitiesAndBuffsComponent` in either table. The extractor selects nothing
  from them and reports `rows: 0`, so both builds stay outside the accepted
  set. Their AbilitiesAndBuffs bodies appear only on the chained route
  described below, which the extractor does not select.
- Bodies without changed items close under both variants, so only windows
  with changed items can tell the variants apart. The control also closes
  28,429 of the 3,293,206 windows with changed items. Every build except 13.00
  still has at least 6,302 windows that only the no-checksum variant closes.
- 13.00 is accepted on thin evidence: six windows in one replay, three of them
  deletion-only. The control closes two of the other three, so by closure
  alone only one window separates the variants in that build. Its three
  changed items do show the same handle sequence as every other build (next
  paragraph). Re-measure 13.00 first when more of its replays are available.

The updated extractor was then run end to end on all 1,018 exports. Every run
exited 0 with zero rejections, and every input hash was unchanged. For each
export, a digest of the observations (row identity, original raw window,
header, IDs and every field boundary) equals the digest the independent reader
computed from the Parquet. All 3,403,315 changed items carry the same fifteen
zero-based handle numbers in the same order: 26, 27, 33 to 38, 41, 42, 45 to
48, and 51. That holds in every accepted build, 13.00 included. A parse that
had slipped by even one bit would be unlikely to reproduce one sequence
millions of times, so this checks alignment. It is not a property schema.

The extractor now accepts exactly the 22 builds with observed windows. A
selected row from 12.10, 12.11 or any unlisted build would still be rejected
as `unvalidated_build`, which keeps the exit code nonzero. This
re-measurement covers structure only: replication keys, item IDs and property
boundaries. It assigns no ability, effect, property name or value meaning.

**Outside the extractor's selection.** The selection also names
`__vrfkit_chained_cnc_h1__`, but on these exports that name never occurs
under `AbilitiesAndBuffsComponent`. It occurs only under
`/Script/ShooterGame.AresAbilitySystemComponent`. That is where the parser
places AbilitiesAndBuffs handle-1 bodies recovered from RepLayout tails. The
extractor selects none of these rows, and its receipt does not count them.
The same three readers walked them separately:

| Table | Windows, all exact | With changed items | Of those, control also closes |
|---|---:|---:|---:|
| `fields` | 250,053 | 250,053 | 1,260 |
| `checkpoint_fields` | 181,108 | 60,833 | 273 |

Both readers agree on every boundary. The other 120,275 checkpoint windows
carry neither deletions nor changes. The population includes 12.10 (7 main
and 6 checkpoint windows) and 12.11 (6 and 5).

This does not change the accepted set above, which is defined by the
extractor's selection. It does mean that selection misses a population the
same grammar reads exactly. Admitting those rows would change the route
definition and bring in checkpoint rows, so it is left for a separate,
reviewed change.

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

## Next work

- Keep the four healing reference roles separate in any association schema;
  require source-time lifecycle and identity evidence for further joins.
- Resolve the changed-item property schemas for the now-consumed GAS windows.
  Do not promote raw property widths or GUID-number coincidences to semantics.
- Decide whether the extractor should also select the chained AbilitiesAndBuffs
  bodies filed under `/Script/ShooterGame.AresAbilitySystemComponent`. They
  are main and checkpoint rows, and on 2026-09-28 the grammar read all of them
  exactly.
- Seek an independent PatchVolume subobject/item schema before assigning
  names or decoding property values in its now-consumed windows.
- The section arithmetic discrepancies and InputEventData action meanings
  remain open, as recorded in [the current backlog](CURRENT_RAW_BACKLOG.md).

Private evidence is retained under `gas-reference-census-root`,
`gas-reference-investigation-root`, `healing-role-association-root`,
`gas-inner-investigation`, `fastarray-root-corpus`, `fastarray-observations-corpus`,
`gas-fastarray-main-comparison`, `patchvolume-investigation`,
`patchvolume-fastarray-entries`, `patchvolume-fastarray-root-accepted`, and,
for the 2026-09-28 build-scope entry, `auto-20260928/scratch-fastarray`: the
walkers, per-export results, end-to-end receipts, and the mutation check of
the build gate's tests. The reports keep
sample scope and source/output hashes; private player observations are not
included in this repository document.
