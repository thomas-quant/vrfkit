# September 8, 2026 partial preservation and unresolved-data audit

**Superseded in part:** its missing-initial figures are the old parser's
misclassification ([header-order correction](PARTIAL_HEADER_CORRECTION.md)).

This batch was built from `f22e2de` plus the changes described below, with
Rust 1.86.0. The frozen executable SHA-256 is
`2e32b3a648a79ab784c9ee3f9a610245c9952178976282a8263798d327e01700`.
The prior [schema expansion](SCHEMA_EXPANSION.md) remains a dated measurement.

## Partial payloads now survive export

`partials.parquet` records rejected current fragments and abandoned accumulated
payloads. It retains the exact bit extent, original packet and payload offset,
channel, sequence, header flags, source stream, checkpoint identity and reason.
An accumulated payload retains its initial header and a separate aggregate bit
count; its rejection packet identifies the later event, or is absent at EOF.
These records are never promoted to successfully reconstructed content blocks.

| Source | Preserved rows | Exact payload bits |
| --- | ---: | ---: |
| Main | 125,037 | 1,576,094,205 |
| Checkpoint | 835,967 | 10,812,213,337 |
| Total | 961,004 | 12,388,307,542 |

All 714 inputs were re-exported with checkpoints, retaining 4,998 Parquet
files. A separate extractor stopped at `RawPacketReader`, bypassing the
replication reader, partial accumulator and export sink. Every preserved row
matched that extractor's original packet window, including bytes, bit count,
position and header flags. The lower-level packet grammar is shared, so this
proves preservation under that grammar, not an independent protocol specification.

Every observed row is a missing-initial current fragment. Accumulator
displacement, channel/resource limits and EOF paths have state-machine tests,
including combined failures with separate causes. They have no observed replay
instances in this corpus. Row and bit counts are separated in the CLI and
manifest; row counts need not equal error counts on a stream that displaces
an earlier assembly as well as rejecting a new fragment.

The partial writer limits retained payload to 8 MiB, allowing one individually
oversized row. Byte-triggered flushes close the Parquet row group as well as
the application buffer. The existing five main tables keep their batching.

## Narrow primitive typing

Sixteen exact group/name/checksum entries decode the byte-shaped `B` fields
on Bomb and observed Swiftplay player-state groups. The 32-bit field with the
same name and checksum `943211507` remains raw. These additions come from
`tools/fixtures/scoped_type_evidence.json`, generated into `scoped_types.rs`;
they never propagate to an unobserved class or another name.

| Physical field rows | Typed before | Typed after | Added |
| --- | ---: | ---: | ---: |
| Main: 999,655,890 | 699,598,065 | 699,617,304 | 19,239 |
| Checkpoint: 53,019,206 | 28,047,677 | 28,222,002 | 174,325 |

The resulting typed fractions are 69.9858% and 53.2298%. Every new value was
independently checked against its complete raw byte. Across all 1,052,675,096
field rows, every old value and non-value column remained unchanged. All
2,856 movement/actor/NetGUID/event Parquets were byte-identical to the previous
exports. Diagnostic differences were limited to the corresponding increases
in successful overlay decodes and decreases in not-in-table counts; all net
counters and failure details remained equal across all 714 inputs.

**2026-09-28 update:** the 32-bit `B` is no longer raw. It is the second word
of the player-state GUID (`A`, `B`, `C`, `D`), and all four are now typed
`UInt32` by their own exact group/name/checksum entries on both player-state
groups. That supersedes the "remains raw" sentence above; the sixteen byte
entries and every figure in this section are unchanged. Evidence is in
[`scoped_type_evidence.json`](../tools/fixtures/scoped_type_evidence.json)
and [DATA.md](DATA.md#player-identity).

## An untyped field row is not necessarily unresolved data

The final raw-priority audit distinguished 300,038,586 untyped main rows:

- 128,823,551 retained a nonempty raw payload of the exact declared size.
- 168,145,154 were successful movement RPC markers with nonzero declared bit
  counts and no duplicate raw payload; their decoded data lives in
  `movement.parquet`.
- 3,069,881 were zero-bit empty/null markers.
- No present raw payload had an incorrect byte length.

The 24,797,204 untyped checkpoint rows all retained exact nonempty raw payloads.
The scoped byte additions moved 19,239 main and 174,325 checkpoint rows out of
the earlier untyped population. They did not change movement markers or raw bits.
Declared bit sums must not be described as preserved raw bytes. Nested raw
parents can also duplicate already decoded child data. The new
`summarize_unresolved_fields.py` reports these categories separately and ranks
by preserved raw bits, with build and exact field identity retained.

The largest raw entries include both the unresolved AbilitiesAndBuffs whole
payload and its `_cnc_h1` view; those are overlapping views, not two independent
losses. Other large candidates include InputEventData, effect argument arrays,
and raw parents of decoded combat/ability arrays. Inspect decoder destinations
before assigning a new implementation to a high-frequency raw row.

## Semantic and event evidence

The initial `semantic_evidence.json` reviews only Subject and SpawnedCharacter,
separately for main and checkpoint rows. Across the preceding 714 exports,
111,268 Subject values matched independently stored replay-header loadouts;
103,891 nonzero character references matched movement actor identities. The
catalog covers 215,726 rows and is intentionally incomplete. Its percentage
is a catalog lower bound, not a new overall semantic-understanding score.
The complete before/after comparison establishes that these values and their
source tables are unchanged in this batch.

An additional audit examined 2,196,424 magazine decreases against a
weapon-scoped continuous-effect RPC within 300 ms. It found 10,703 unique
candidate matches, 19 ambiguous matches and 2,185,702 unmatched decreases;
none matched in the same packet. This does not establish shot counts. The
audit breaks transitions at conflicting same-packet samples, rejects
conflicting identities, and permits dynamic actor GUIDs without static path
registrations. Missing first samples remain left-censored.

## Validation

Both documented corpus guards and the export/checkpoint baselines passed on all
714 files. The block oracle still excludes unreassembled partials from its
denominator; preserving their raw bytes does not make that score end-to-end
completeness.
