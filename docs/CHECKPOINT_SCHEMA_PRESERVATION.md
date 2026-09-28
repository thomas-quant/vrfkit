# Checkpoint schema preservation

Checkpoint export now retains the initial GUID entries and the export-map
declarations before walking the snapshot frame. Previously, the parser kept
only rendered paths and the resulting cache; the original path discriminator,
flags, FName components, and empty group declarations were unavailable to
downstream analysis.

The optional `--checkpoints` pass adds three tables. Across the measured 714
replays (builds 13.01, 13.02, 13.04, and 13.05), they contain:

| Table | Rows |
|---|---:|
| `checkpoint_guid_entries` | 58,509,199 |
| `checkpoint_export_groups` | 6,644,959 |
| `checkpoint_export_fields` | 39,444,050 |

The 13,782 checkpoints declare 137,243,127 field slots. Populated slots have
rows in the field-declaration table; group capacity and populated handles
retain the remaining sparse holes. Original order and checkpoint index are
part of the output. The original checkpoint ID can repeat.

GUID entries preserve literal paths separately from name indices. The new
table records the initial wire entries; `checkpoint_net_guids` continues to
record the cache after the frame walk. Equal aggregate row counts do not make
those two observation points interchangeable. Field declarations retain the
exact nonzero exported/FName discriminator bytes and nullable name components,
in addition to the existing rendered name and checksum. FString encoding bytes
are not a column in these tables; the original replay remains the wire source.

The schema reader supplies fallible callbacks while retaining its existing
no-op API. Callback errors propagate, and Parquet finalization errors fail the
export. Writers use bounded record batches and an 8 MiB retained-string budget.
Both reader counts and writer counts are checked against the output row counts.
See [the column and join rules](USAGE.md#checkpoint-schema-declarations).

These 104,598,208 added records are registry and schema evidence. They are not
new gameplay field values. In the preservation-only batch at `740688d`, the
typed-field ratio stayed at 71.8781%; the later path-resolution batch changes
gameplay field output and has separate measurements below. A numeric fallback
path matching an export-map index does not prove that
the two indices share a namespace. A named owning actor also does not identify
an unresolved subobject class.

Path resolution now uses the preserved order: a name index selects the
zero-based literal-path entry among earlier GUID entries in the same
checkpoint. Indexed entries do not extend that table, and every checkpoint
starts with an empty table. The raw discriminator, literal, index, outer GUID,
flags, and order remain unchanged in this table even though
`checkpoint_net_guids` and block path columns now contain resolved strings.
See [Checkpoint path resolution](CHECKPOINT_PATH_RESOLUTION.md) for the
independent whole-corpus validation and the remaining provenance limit.

## Initial preservation-only validation (`740688d`)

The following comparison and test counts describe that initial preservation
batch, before path resolution changed three of the existing checkpoint tables.
For the current comparison, see [Checkpoint path resolution](CHECKPOINT_PATH_RESOLUTION.md).

All 714 replays exported successfully. An independent Python parser compared the
three new tables against decompressed original checkpoint archives, checking
all values, row order, and output column types/nullability. It shares the
container's decompression implementation, so this is independent schema
parsing rather than independent decompression.

The previous ten Parquet tables are byte-identical for every replay. Existing
manifest values also agree after accounting for the three new counters and
the measured wall-clock `elapsed_ms`. The root audit independently reconciles
actual Parquet metadata, original input hashes, and reader/writer counts.

Negative controls that alter parser input or exported rows are rejected by the
independent verifier, and every corpus result above comes from one frozen
binary.
