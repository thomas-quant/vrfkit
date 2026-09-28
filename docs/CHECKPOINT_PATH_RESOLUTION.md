# Checkpoint path resolution

Checkpoint GUID entries encode a path either as a literal string or as an
integer. vrfkit resolves the integer against a checkpoint-local literal path
table. The rule is:

1. Start with an empty table for each checkpoint index.
2. Append each literal path when its GUID entry is read.
3. For an indexed entry, use `name_index` as a zero-based position in the
   literals that appeared earlier in that checkpoint.
4. Do not append indexed entries to the literal table.
5. Register the resolved path in the checkpoint GUID cache. A later entry for
   the same GUID replaces its path and outer-GUID association.

The default checkpoint readers and sink path use
`CheckpointPathMode::LiteralPathTable`. A caller that needs the historical
decimal rendering can explicitly select `CheckpointPathMode::LegacyDecimal`.
The export manifest identifies the default as
`checkpoint_path_resolution_mode = "preceding_literal_zero_based"` and reports
literal, indexed, and resolved-index counts. The CLI prints the same three
counts for every checkpoint export.

`checkpoint_guid_entries.parquet` remains the raw declaration record. Its
`path_is_string`, `literal_path`, `name_index`, `outer_net_guid`, `flags`, and
ordinal are unchanged. `checkpoint_net_guids.parquet` records the final cache,
after last-entry-per-GUID replacement and the checkpoint frame walk.
`checkpoint_blocks.parquet` records paths available to each content block.
Join all three using replay identity and `checkpoint_index`; a checkpoint ID
can repeat, and a main-stream GUID with the same number is a different scope.

## Evidence

The rule was first tested on 714 replays across builds 13.01, 13.02, 13.04, and
13.05. An independent Python oracle reconstructed the final 58,509,199-row
checkpoint GUID cache from the original 58,509,199 raw entries and checked
17,251,536 checkpoint blocks. Every file matched. All manifests reported zero
frame-added or frame-exported GUIDs, which makes this raw-entry reconstruction
sufficient for that corpus.

One positive fixture checked last-entry-per-GUID replacement and
checkpoint-local references. Eight negative fixtures changed exported Parquet
or oracle input to represent a wrong base, a cache leak across checkpoints with
the same ID, insertion of a reference into the literal table, a changed raw
index, zero instead of null for an absent outer, a changed block outer path, a
forward reference, or a nonzero frame-added GUID counter. The oracle rejected
all eight. These fixtures test the independent comparison; they do not claim
that eight serializer implementations were run. The six main tables and three
raw checkpoint declaration tables stayed byte-identical across all 714
exports.

This is a measured serialization rule for the supported corpus. A matching
path does not prove a numeric export group is a particular gameplay class, and
the rule has not been matched to an authoritative current upstream serializer.
The [main-stream cross-check](#cross-check-against-the-main-stream) below
extends the measured scope to all 24 supported builds; it is agreement between
two independent readers of the same files, not a serializer specification.
Future files with an out-of-range index fail checkpoint parsing instead of
silently falling back to decimal text. If a checkpoint frame adds or exports
GUIDs, its final cache also depends on those frame operations; the manifest
counters make that condition visible.

### Cross-check against the main stream

The oracle above applies the same rule to the raw entries, so it confirms that
the reader follows the rule; it cannot show that the rule is right. The main
replay stream is an independent source. It declares the same server NetGUIDs
through a different reader (frame-level ExportData and export-GUID bunches,
`read_export_guids`) into the replay-wide cache that `net_guids.parquet`
records when the export ends. Each checkpoint is read into a fresh cache, so no
main-stream path passes through the literal-table rule.

`checkpoint_guid_crosscheck` in
[`tools/check_export_baseline.py`](../tools/check_export_baseline.py) compares
the two sources for one export:

1. Order `checkpoint_guid_entries.parquet` by `(checkpoint_index, ordinal)`.
   Every checkpoint must hold ordinals `0..n-1`, and each row's path columns
   must agree with its `path_is_string`; otherwise nothing is compared and the
   check fails.
2. Rebuild each indexed path by the rule above, grouping by `checkpoint_index`.
3. Join every entry, literal or indexed, to `net_guids.parquet` by `net_guid`.
   A repeated main-stream `net_guid` fails the check. An entry whose GUID the
   main stream never declared is counted as unjoined and not compared.
4. Compare the path, and the outer GUID under an explicit rule: a checkpoint
   outer of 0 must meet a null main outer, and any other value must meet the
   same main value. A null is never read as 0, so a main table that started
   writing 0 would fail.

The check fails on any path or outer difference, on an index past the
preceding literals, and when no indexed entry joined at all. It prints every
count, zeros included. `check_export_baseline.py --checkpoints` runs it after
the export and exits non-zero on failure; `verify_build_corpus.py` runs it on
every export and reports the counts per build as `guid_crosscheck_*`.

Measured on 2026-09-28 over the 1,018 exports retained by the full-corpus
`verify_build_corpus.py` run at `259ed10`: every unique replay of the 24
supported builds, exported with `--checkpoints`. The counts come from the check
itself and from a separately written one-off rebuild, not kept in the tree,
which also evaluated the wrong rules below; the two agreed on every export.

| Build | Exports | Indexed joined | Indexed path equal | Literal joined | Literal path equal | Literal unjoined |
|---|---:|---:|---:|---:|---:|---:|
| 11.06 | 3 | 61,738 | 61,738 | 214,642 | 214,642 | 240 |
| 11.07 | 3 | 59,979 | 59,979 | 211,181 | 211,181 | 293 |
| 11.08 | 3 | 63,188 | 63,188 | 217,150 | 217,150 | 340 |
| 11.09 | 3 | 65,179 | 65,179 | 237,816 | 237,816 | 320 |
| 11.10 | 3 | 56,379 | 56,379 | 196,782 | 196,782 | 274 |
| 11.11 | 3 | 69,720 | 69,720 | 234,869 | 234,869 | 386 |
| 12.00 | 3 | 64,758 | 64,758 | 232,203 | 232,203 | 293 |
| 12.01 | 3 | 60,414 | 60,414 | 214,145 | 214,145 | 271 |
| 12.02 | 3 | 64,823 | 64,823 | 228,813 | 228,813 | 343 |
| 12.03 | 3 | 61,796 | 61,796 | 213,015 | 213,015 | 311 |
| 12.04 | 3 | 72,094 | 72,094 | 274,661 | 274,661 | 352 |
| 12.05 | 3 | 58,810 | 58,810 | 197,986 | 197,986 | 295 |
| 12.06 | 3 | 49,816 | 49,816 | 171,765 | 171,765 | 223 |
| 12.07 | 3 | 63,894 | 63,894 | 207,717 | 207,717 | 361 |
| 12.08 | 3 | 59,120 | 59,120 | 190,158 | 190,158 | 291 |
| 12.09 | 3 | 62,701 | 62,701 | 204,596 | 204,596 | 270 |
| 12.10 | 1 | 539 | 539 | 3,670 | 3,670 | 6 |
| 12.11 | 1 | 483 | 483 | 3,274 | 3,274 | 5 |
| 13.00 | 1 | 508 | 508 | 3,324 | 3,324 | 5 |
| 13.01 | 215 | 4,198,397 | 4,198,397 | 12,969,307 | 12,969,307 | 18,941 |
| 13.02 | 205 | 4,291,803 | 4,291,803 | 13,371,198 | 13,371,198 | 19,678 |
| 13.04 | 108 | 2,037,483 | 2,037,483 | 6,051,318 | 6,051,318 | 9,796 |
| 13.05 | 401 | 7,706,687 | 7,706,687 | 22,814,208 | 22,814,208 | 34,506 |
| 13.06 | 38 | 763,685 | 763,685 | 2,371,485 | 2,371,485 | 3,498 |
| **All** | **1,018** | **19,993,994** | **19,993,994** | **61,035,283** | **61,035,283** | **91,298** |

No indexed entry was unjoined, unresolved or different, and no outer differed
in either kind. Every joined indexed entry has an outer on both sides; the
"no outer" branch of the outer rule was exercised by 26,831,216 literal
entries. No export repeats a main-stream `net_guid`. Of the joined indexed
entries, 16,549,610 are even-numbered (dynamic) GUIDs, and those agree too, so
the agreement is not confined to static objects.

The comparison can fail. Under the same join, rules other than the reader's
disagree with the main stream on the same entries:

| Rule applied to the raw indices | Path equal | Path differs |
|---|---:|---:|
| Zero-based among preceding literals, reset per checkpoint (the reader's rule) | 19,993,994 | 0 |
| One-based position | 0 | 19,993,994 |
| References appended to the table | 1,511,097 | 18,482,897 |
| One table shared by all checkpoints of a replay | 4,422,823 | 15,571,171 |
| Decimal text (`LegacyDecimal`) | 0 | 19,993,994 |

The check's tests encode the three wrong index rules as serializer output, and
edit one main-stream path, and the check rejects each; see
[`tools/tests/test_check_export_baseline.py`](../tools/tests/test_check_export_baseline.py).

This agreement is evidence for the path-index rule, not for actor identity.
The join is by GUID number: it shows that each checkpoint entry and the main
stream associate that number with the same path and outer, not that a
checkpoint GUID denotes a particular actor at a particular time, so the
caution about joining the streams by number still applies. The check also
assumes that a GUID's path does not change within a replay:
`net_guids.parquet` is the main stream's final registry, while each checkpoint
is a snapshot. That held for every compared entry. A future difference
therefore means either a different index rule or a GUID whose path changed
during the replay, and needs investigating before the rule is trusted for that
build.

## Measured field expansion

Against the preservation-only output at `740688d`, the full 714-replay run
resolved 14,403,610 indexed GUID path entries and changed the group path or
resolution source of 9,441,882 blocks. Checkpoint fields grew from 205,866,627
to 212,099,080 rows (+6,232,453, exactly the new indexed array children), with
11,975,340 more rows typed and 20,113,218 more named -- overlapping counts,
since existing rows can gain names and types without a new row. Typed presence
was then 70.6364% main (713,488,311 of 1,010,086,119 rows), 81.3258%
checkpoint (172,491,241) and 72.4914% combined, before the
[structured-array expansion](STRUCTURED_ARRAY_EXPANSION.md); neither the share
of bytes decoded nor of meaning understood.

Of 198,461,491 previous raw checkpoint rows, 198,344,356 kept their handle, bit
count and raw bytes within the same block. The other 117,135 tails split into
2,576,970 header and 47,352,050 body bits, each body matching the candidate
output; those `__vrfkit_chained_cnc_h1__` bodies stay raw, and the compatible
capacity 34 is not an established function count or body decoder.

For the 7,809,654 blocks with unchanged group paths and resolution sources,
all 189,991,214 field rows matched across every column, comparing valid
floating-point values by their IEEE bits. Signed zero and NaN payloads were
included in the comparator's mutation controls. All six main tables, all
three raw declaration tables and checkpoint actor output stayed byte-identical.
