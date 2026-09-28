# September 8, 2026 schema expansion and evidence audit

**Header-order correction:** The partial missing-initial/rejection figures below
are historical parser classifications. [The corrected header audit](PARTIAL_HEADER_CORRECTION.md)
reassembles all 961,004 observed fragments; the source data was present.

The subsequent [partial preservation and unresolved-data audit](TRANSPORT_PRESERVATION.md)
adds raw transport exports and supersedes the current typed-value percentages.
Figures below describe this earlier batch.

This batch adds 38 primitive overlay entries and expands the diagnostics and
analysis tools. It was built from `4bfbbd8` plus this change using Rust 1.86.0.
The tested executable SHA-256 is
`df9de182f9e5f4a1c8724849231561246d631b3f7fa4f22a769e7159d8e50134`.

All 714 preserved inputs were checked against their inventory SHA-256 and
freshly exported with `--checkpoints`. The population is 215 replays on 13.01,
204 on 13.02, 108 on 13.04 and 187 on 13.05. All 4,284 Parquet files were kept
separately from the earlier export. The 12-worker run took 145.72 seconds on
the measurement machine; this is one observed run, not a portable benchmark.

## New typed data

| Physical field rows | Before | After | Additional typed rows |
| --- | ---: | ---: | ---: |
| Main: 999,655,890 | 698,945,941 (69.9187%) | 699,598,065 (69.9839%) | 652,124 |
| Checkpoint: 53,019,206 | 21,894,478 (41.2954%) | 28,047,677 (52.9010%) | 6,153,199 |

These are output-row measurements. Checkpoints repeat state, and nested
decoders may emit parent and child rows. The percentages do not measure unique
game facts, complete payload preservation, or semantic understanding.

The additions comprise:

- 27 player-state fields: `ProfileName` as FString, observed `G`/`R` byte
  fields, and crosshair outline, line, center-dot, opacity, error-scale and
  option fields. The exact names and primitive types are in
  [type_evidence.json](../tools/fixtures/type_evidence.json).
- 11 Harbor Tidal Wave RPC parameter entries: chunk index/generation/count,
  spacing, velocity, anchor data, the previous chunk reference, and linger/stop
  state. The source spelling `VelocityIn` and observed `Velocity In` differ;
  the correction targets the observed export name.
- The existing Swiftplay player-state alias also applies the 27 player-state
  additions. All 22 matching replays were independently checked against the
  separate [alias evidence specification](../tools/fixtures/type_evidence_aliases.json).

`ProfileName` is not relabelled as a Riot ID or player display name: decoding a
string does not establish its purpose. `B` remains untyped by name because the
same player-state name appears with both 8-bit and 32-bit payloads under
different handles/checksums. Its rejected proposal is kept in
[type_evidence_rejected.json](../tools/fixtures/type_evidence_rejected.json).
`AliveChunks` also remains raw.

The overlay remains generated from the existing descriptor input plus explicit
corrections. No wholesale upstream refresh was performed. The table now has
1,309 entries in 214 groups and 84 explicit handle entries; the correction
guard checks 185 expectations, including 124 additions.

## Verification and baseline changes

Every main/checkpoint field row was compared with the prior export. All
non-value columns, including raw bits, row identity and order, were unchanged.
Existing non-null values were unchanged. The only differences were the
6,805,323 approved null-to-typed additions. All 2,856 non-field Parquet artifacts
(movement, actors, NetGUIDs and events) were byte-identical.

An independent Python reader verified every new value against raw payloads:
6,701,345 primary-specification rows plus 103,978 Swiftplay-alias rows, with
zero raw decode failures or typed-value mismatches. Primitive widths,
terminators and full consumption are required. The verifier uses Unreal's
low-bit-continuation IntPacked format; a conventional LEB128 implementation
was caught by the pilot and replaced, with regression vectors covering the
distinction, truncation and overflow.

The pinned 13.01 export baseline changes only as follows:

- Overlay decoded rows increase by 577, with exactly 577 fewer not-in-table
  rows. No other existing counters or row counts change.
- `fields.parquet` grows from 16,119,220 to 16,121,012 bytes.
- With checkpoints, `checkpoint_fields.parquet` grows from 234,673 to 238,016
  bytes. These two tables' hashes change with the new value columns; all other
  table hashes stay fixed.

The private run artifacts record the exact commands, input identities, binary
and source hashes, per-file exits, full comparison records and primitive
verification partitions. They are under the run name `20260908-improvements`;
no private replays or player values are committed to this repository.

## Partial diagnostics: broader measurement, unchanged recovery

Diagnostic JSON schema 3 separates attempted partial bunches from accepted
fragments and attributes errors by cause. All old `net_main` and
`net_checkpoint` counters match the previous binary across all 714 files.

| Current parser observation | Main | Checkpoint |
| --- | ---: | ---: |
| Attempted partial bunches | 125,037 | 835,967 |
| Missing-initial errors | 125,037 | 835,967 |
| Accepted fragments / completed assemblies | 0 / 0 | 0 / 0 |
| Rejected payload bits | 1,576,094,205 | 10,812,213,337 |
| Errors with the current final flag | 53,249 | 240,471 |
| Errors with the reliable flag | 22,169 | 835,967 |

All other cause counts and both under-/over-attribution residuals are zero.
Attempted bunches equal missing-initial errors on this corpus only; it is not
an invariant, and `partial_bunches` now also counts partials refused at the
channel-state guard.
These are observations under the current header/state interpretation. They do
not independently prove how the game recorded every partial header. A
different final-bit ordering found in another Unreal parser does not restore
missing initial state and was not adopted without replay evidence.

The rejected payloads are non-empty, but their interpretation remains
unresolved. No standalone fragment is promoted to a complete content block.
Their 12,388,307,542 payload bits are outside the content-block denominator and
can include repeated checkpoint state. A zero block-loss score remains narrower
than complete retention of every payload in the input VRF.

## Analysis and maintenance tools

- `summarize_value_coverage.py` can audit an explicit semantic-evidence catalog.
  Reviewed claims require an exact field identity, evidence, and enforceable
  export/build applicability. Null-safe unions prevent duplicate counting;
  catalog hashes and full claim definitions identify the basis. No catalog
  means no semantic-coverage claim.
- `compare_descriptor_sources.py` compares pinned C# inputs without changing
  checkouts. It exposes source-file and parsed type/handle changes plus local
  entries that regeneration would remove or overwrite. It records input and
  extractor hashes. Unsupported C# forms remain visible as source changes.
- `extract_ability_lifecycle.py` emits ability-path actor candidates, observed
  lifecycle events and explicit Owner/Instigator evidence. Player references
  remain separate from proof of casts. Conflicts, same-packet reuse ambiguity,
  component-only references and missing closes remain visible.

The lifecycle view was checked on all 714 replays: 546,773 candidate instances,
472,664 unambiguous player identity links, 74,109 unresolved links, 508,632
observed closes and 38,141 censored lifecycles. These are actor instances and
identity associations, not ability-use counts or measured effect durations.

Further semantic work still needs independent evidence for GAS words,
InputEventData action names and StopEffectType. A checksum-scoped treatment of
the colliding `B` fields and recovery of pre-framing partial payloads require
separate verified changes.

(Later: the checksum-scoped `B` treatment has since landed. The byte-shaped
`B` fields were typed `Byte` in the [partial preservation
batch](TRANSPORT_PRESERVATION.md), and on 2026-09-28 the 32-bit `B`
(checksum 943211507) was typed `UInt32` together with `A`, `C` and `D` as
the words of the player-state GUID. The name-keyed rejection above still
stands: neither is typed by name.)
