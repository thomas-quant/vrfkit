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

A primary wire implementation supplies a stronger interpretation: its replay
reader reads a custom-delta support bit followed by four signed little-endian
i32 words: ArrayReplicationKey, BaseReplicationKey, NumDeletes, NumChanged.
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

First published on 2026-09-09 for the main route on 13.01, 13.02, 13.04 and
13.05, the tool accepts that route on 22 builds since the
[2026-09-28 re-measurement](#build-scope-re-measured-2026-09-28) (all but 12.10
and 12.11), and since the [chained-route change](#chained-route-admitted-2026-09-28)
reads two routes, labelling every observation with its `route` and stream
(`population`):

| Route | Exported identity (handle 1) | Main `fields` | `checkpoint_fields` |
|---|---|---|---|
| `cnc_h1` | `AbilitiesAndBuffsComponent` / `_cnc_h1` | 22 builds | none (never observed) |
| `chained_cnc_h1` | `/Script/ShooterGame.AresAbilitySystemComponent` / `__vrfkit_chained_cnc_h1__` | all 24 builds | all 24 builds |

It preserves malformed or unvalidated observations with a rejection reason and
returns nonzero if any are rejected. The `cnc_h1` checkpoint stream remains
unvalidated, and so does any build that is not listed for a route and stream,
including every future one. The receipt (schema 2) counts rows, exact walks,
rejections, items and fields for every route and stream, printing zeros. It
also counts rows that carry a route's field name under any other group; they
are not selected. Output must be a new directory outside the source export.
Existing Parquets and their typed-value counts do not change.

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
  set on this route. Their AbilitiesAndBuffs bodies appear only on the chained
  route, which the extractor did not select until the
  [chained-route change](#chained-route-admitted-2026-09-28).
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

On the `cnc_h1` route the extractor accepts exactly the 22 builds with
observed windows. A selected `cnc_h1` row from 12.10, 12.11 or any unlisted
build would still be rejected as `unvalidated_build`, which keeps the exit
code nonzero. This re-measurement covers structure only: replication keys,
item IDs and property boundaries. It assigns no ability, effect, property name
or value meaning.

### Chained route admitted, 2026-09-28

**Found outside the selection.** Until this change the extractor matched the
group `AbilitiesAndBuffsComponent` with either inner name. The name
`__vrfkit_chained_cnc_h1__` never occurs under that group; it occurs only
under `/Script/ShooterGame.AresAbilitySystemComponent`. That is where the
parser files AbilitiesAndBuffs handle-1 bodies recovered from RepLayout tails.
So on every export the second name selected nothing, and the receipt did not
count those rows. The same three readers walked them separately, using the
same 1,018 exports, the exact group and field name, and any handle:

| Table | Windows, all exact | With changed items | Of those, control also closes |
|---|---:|---:|---:|
| `fields` | 250,053 | 250,053 | 1,260 |
| `checkpoint_fields` | 181,108 | 60,833 | 273 |

Both readers agree on every boundary. The other 120,275 checkpoint windows
carry neither deletions nor changes.

**Same window shape.** In `crates/vrfkit/src/sink/stream.rs`, both routes take
the handle-1 payload of the same fc=34 ClassNetCache walk, from its payload
offset and for its payload length. `_cnc_h1` rows come from whole unresolved
payloads (`emit_brute_forced_cnc_rpcs`). Chained rows come from a tail after a
RepLayout prefix (`on_rep_layout_tail`). That path runs only for the pre-remap
AbilitiesAndBuffs identity, and only when the tail holds exactly one handle-1
RPC. It checks the first bit on a copy of the reader, so the support bit stays
in `raw_bits`. `decode()` therefore reads both routes unchanged, and the exact
walks above confirm it.

**Per build.** Every supported build has chained windows in both streams, and
every one of them is exact:

| Build | Replays | Main windows | Main, control also closes | Checkpoint windows | Checkpoint with changed items | Checkpoint, control also closes |
|---|---:|---:|---:|---:|---:|---:|
| 11.06 | 3 | 920 | 0 | 572 | 199 | 0 |
| 11.07 | 3 | 850 | 0 | 570 | 209 | 0 |
| 11.08 | 3 | 831 | 0 | 595 | 237 | 0 |
| 11.09 | 3 | 823 | 0 | 618 | 210 | 0 |
| 11.10 | 3 | 760 | 0 | 540 | 203 | 0 |
| 11.11 | 3 | 962 | 6 | 660 | 239 | 0 |
| 12.00 | 3 | 884 | 6 | 610 | 223 | 0 |
| 12.01 | 3 | 705 | 0 | 570 | 201 | 0 |
| 12.02 | 3 | 917 | 0 | 610 | 246 | 4 |
| 12.03 | 3 | 811 | 0 | 569 | 224 | 0 |
| 12.04 | 3 | 1,070 | 48 | 713 | 247 | 14 |
| 12.05 | 3 | 684 | 0 | 550 | 168 | 0 |
| 12.06 | 3 | 618 | 0 | 450 | 139 | 0 |
| 12.07 | 3 | 731 | 0 | 590 | 234 | 0 |
| 12.08 | 3 | 763 | 0 | 550 | 175 | 0 |
| 12.09 | 3 | 839 | 0 | 559 | 180 | 0 |
| 12.10 | 1 | 7 | 0 | 6 | 6 | 0 |
| 12.11 | 1 | 6 | 0 | 5 | 5 | 0 |
| 13.00 | 1 | 6 | 0 | 5 | 5 | 0 |
| 13.01 | 215 | 52,454 | 171 | 37,952 | 12,566 | 37 |
| 13.02 | 205 | 53,923 | 256 | 38,540 | 13,354 | 45 |
| 13.04 | 108 | 24,974 | 169 | 18,725 | 6,040 | 36 |
| 13.05 | 401 | 96,191 | 488 | 69,671 | 23,205 | 115 |
| 13.06 | 38 | 9,324 | 116 | 6,878 | 2,318 | 22 |
| Total | 1,018 | 250,053 | 1,260 | 181,108 | 60,833 | 273 |

Every main chained window carries changed items. 12.10, 12.11 and 13.00 rest
on five to seven windows per stream. All of those carry changed items, and
the control closes none of them.

**Admitted.** The extractor now selects exact (group, field) pairs instead of
one group with either name. It gates each route and stream separately:
`cnc_h1` main rows on its 22 builds, and chained rows in both streams on all
24. The `cnc_h1` checkpoint stream stays unvalidated, because no such row has
been observed. Rows from an unlisted build, or on an unvalidated stream, keep
their raw bits with a named rejection (`unvalidated_build`,
`unvalidated_checkpoint_route`), and the exit is nonzero. Every record now
carries `route` beside `population` (the stream). The receipt, now schema 2,
counts every route and stream with zeros printed. It also counts rows that
carry a route's field name under any other group. They are never selected, so
a mismatch like the one found here shows up as a number.

The route-aware extractor was then run end to end on all 1,018 exports. Every
run exited 0 with zero rejections, and every input hash was unchanged. Across
the corpus it produced 4,430,654 observations:

| Route and stream | Observations |
|---|---:|
| `cnc_h1` main | 3,999,493 |
| `chained_cnc_h1` main | 250,053 |
| `chained_cnc_h1` checkpoint | 181,108 |
| `cnc_h1` checkpoint | 0 |

The receipts count 0 unselected route-name rows in either stream. For every
export and route, the receipt's rows, deleted and changed items and raw fields
equal the independent readers' figures. The digest of that route's
observations equals the reader's digest as well. All 3,829,374 changed items,
on both routes and in both streams, carry the fifteen-handle sequence given
above.

## PatchVolume: numeric structure with an unresolved item schema

A 57-body pilot (42 whole payloads and 15 tails of the 60 rows one positive
export per build supplied, on direct Deadeye, Sarge and Aggrobot actor outers)
and then the full 714-export run showed the FastArray
grammar consuming every PatchVolume window: **26,303 main windows** (19,140
whole unresolved CNC windows, 7,163 preserved RepLayout tails), all closed by a
fixed function-count-2, handle-0 outer framing and the no-checksum body --
241,721 changed items, 2,900,652 raw property windows, each item with the
twelve handles `23,24,25,26,30,33,36,39,40,41,42,43`, no deleted items and no
checkpoint windows. That outer framing is a candidate, not a class: an altered
window can also walk cleanly at another function count, and the checksum mode
closes eight full-corpus windows, so per-row variant selection is not a grammar
decision. An independent integer-based reader reproduced every boundary (the
714 NDJSON files total 245,312,914 bytes); the whole and tail records are
separate observations, not one update.

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
- Observe a `_cnc_h1` checkpoint stream (the chained route has both).
- Establish the meanings of the ground-volume `Status` and `bIsActive`.
- The section arithmetic discrepancies and InputEventData action meanings
  remain open, as recorded in [the current backlog](CURRENT_RAW_BACKLOG.md).

The walkers, per-export results and receipts are private; the sections above
keep sample scope and source/output hashes.
