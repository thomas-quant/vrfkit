# Partial header order and recovered replay data

The September 8, 2026 reassembly run corrects a header-order bug. An
always-present bit of unknown meaning comes **before** the conditional
partial-initial and partial-final bits. Previously the parser read
initial/final first, then discarded the third bit.

This preserved payload lengths and offsets while assigning the flag roles
incorrectly. Every real initial fragment was consequently reported as a final
fragment without an initial. Unit tests constructed headers in the same wrong
order. A previous independent preservation extractor also shared this header
parser, so it correctly verified bytes and offsets without detecting the role
error. Literal real-packet header fixtures now fail under the old ordering.

The correction is supported by measured packet chains and their successfully
decoded content. The extra bit's purpose remains unknown; the fix does not
assign it an engine feature name.

## Full 714-replay result

All original inputs from 13.01 (215), 13.02 (204), 13.04 (108) and 13.05 (187)
were re-exported with checkpoints using binary SHA-256
`29604c16e11b0e796f40c78b541cd41a7bd31af18849c88bb764a1d7d6d5c151`.
All 714 exports succeeded; all 4,998 Parquet files are retained.

| Transport measure | Main | Checkpoint |
| --- | ---: | ---: |
| Partial fragments | 125,037 | 835,967 |
| Initial fragments, independently read | 53,249 | 240,471 |
| Final fragments, independently read | 53,249 | 240,471 |
| Completed assemblies | 53,249 | 240,471 |
| Partial errors / unfinished assemblies | 0 / 0 | 0 / 0 |

All 961,004 original fragment payloads retain identical bytes, exact bit
lengths and offsets under the corrected raw-packet probe. The initial/final
counts above were read from packet headers, not inferred from completion.
`partials.parquet` is now empty on this corpus because those fragments are
processed successfully. It remains available for actual future reassembly
failures; the earlier raw exports remain retained evidence.

Compared with the isolated effects-only export, the header correction adds:

| Output measure | Main | Checkpoint |
| --- | ---: | ---: |
| Content blocks | 1,210,694 | 4,203,351 |
| Net fields before flattening | 5,285,379 | 19,756,547 |
| RPCs decoded by the net reader | 136,492 | 7,803 |
| Physical field rows in Parquet | 6,299,513 | 148,707,835 |

Movement gains 3,705,163 samples and main actor events gain 20,694 rows.
These are physical observations, including repeated checkpoint state and
flattened array members, not counts of unique game facts.

Physical non-null value coverage was measured separately from the protocol
counters: main 712,305,009 of 1,005,955,403 rows (70.8088%), checkpoint
157,756,177 of 201,727,041 rows (78.2028%). Neither percentage measures how
much of the game's meaning is understood. The checkpoint denominator grows
substantially because the missing snapshot payloads are now available.

Transform failures, malformed content blocks, content-block framing failures
and field-stream failures remain zero. Newly accessible unresolved RPC
payloads are preserved: 47,371 more main and 70,094 more checkpoint records.
Their diagnostic failure counters therefore increase with the new input
population. That is separate from dropping a framed block. Checkpoint actor
snapshot suppression also increases as previously missing snapshots become
readable. Unresolved inner payloads still require interpretation.

## Shot effect arrays

The same candidate adds JSON values for shot RPC FloatValues/ObjectValues/
VectorValues using the existing strict Rust effect decoder, while retaining
raw bytes. The Python adapter reads its shot source directly from those raw
bytes even when JSON is present, and preserves its existing raw RPC payload
contract. Four full exports, one per build, produced byte-identical event and
movement bundles with the previous and updated adapters in an isolated
effects-only comparison.

The full 714-export differential compared 6,940,079 arrays with the Python
decoder: structures, tags and numeric bit patterns matched with no Rust
errors. A separate effects-only export comparison verified all 1,052,675,096
old field rows, exactly 6,940,079 approved null-to-JSON additions, unchanged
raw/identity columns and byte-identical non-field Parquets. These effects-only
checks are separate from the expanded reassembly population. Typed JSON does
not by itself establish the semantic meaning of every tag.

Both full 714-input corpus guards passed. Real packet-header fixtures reject the
old flag order, and deliberate corruptions confirm that the raw and JSON
comparators can fail.

## Superseded conclusions

The missing-initial counts in [the schema audit](SCHEMA_EXPANSION.md) and
[the preservation audit](TRANSPORT_PRESERVATION.md) describe the old parser's
misclassification. They do not prove missing source data. Claims that this
corpus's fragments cannot be recovered without external data are withdrawn.
Their byte-preservation evidence remains useful, with the shared-parser
limitation stated explicitly above.
