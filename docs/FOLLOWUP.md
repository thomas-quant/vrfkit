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
- Find out why neither C# build (upstream `b51d674`, vendored `8824794`) emits
  any event for packet 391880 of `02d4d478`, where vrfkit decodes a
  killing-blow `MulticastNotifyDamage_Point` on the `DamageableComponent` of
  Gekko's E-ability projectile (actor 27232, channel 194; the actor closes six
  packets later). vrfkit already produced it at `d4731c8`, before the partial
  header correction. `compare_rpc_params.py` exits 1 on this one record until
  it is explained; `compare_combat_report.py` matches all ten shapes.

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
- Retiring a live actor whose channel was reopened by a partial fragment the
  reassembly accumulator refuses or discards. Every arm that stops an open
  bunch before its open is read now retires the live actor it displaced
  (`retire_after_failed_open` in `crates/vrf-net/src/pipeline/mod.rs`); the
  accumulator path does not. A probe -- a 7-bit, non-final initial fragment
  with `bOpen` on a channel holding a live actor -- leaves that actor live,
  frames the next bunch's block under it, and counts only the partial error.
  Doing it needs a rule for each discard mode -- a refused current fragment,
  a displaced in-flight assembly, an overlapping initial that is itself an
  open, an errored final -- inside a path the verdict deliberately leaves
  unscored, and no replay reaches it (see the measurement below).
- Counting the bits left after a package-map export list reads cleanly.
  `process_complete_payload` returns after the exports, so an open,
  must-be-mapped GUIDs or content blocks behind them are never read and reach
  no counter; only an open is named now, through `failed_reopens_while_open`,
  and only when it displaced a live actor. What those bits are is not
  established: no replay carries a package-map export bunch, so neither
  reading on nor a counter with a stated meaning has anything to be checked
  against.

Measured for both items on 2026-09-28 by summing `quality.net` and
`quality.checkpoints.net` over the 1,018 export manifests of the common audit
of main `259ed10` (one export per unique replay, checkpoints on):
`package_map_exports`, `must_be_mapped_guids`, `bunch_header_failures`,
`channel_state_limit_failures`, `channel_reopens_while_open` and
`partial_errors` are 0 in both passes, over 563,030,549 main and 4,346,884
checkpoint bunches.
