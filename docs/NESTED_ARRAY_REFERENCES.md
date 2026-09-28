# Nested equipment and assist references

The measured 13.01, 13.02, 13.04 and 13.05 routes now expose references inside
two previously opaque array members. Each existing raw container and its
enclosing array remain in the export. New rows follow their raw container
immediately, with the original element indices in the field name.

| Nested field | Main rows added | Checkpoint rows added | Wire value |
|---|---:|---:|---|
| `SelectedV2[i].EquippableAttachments[j].SocketAsset` | 201,810 | 3,882,298 | Object NetGUID |
| `SelectedV2[i].EquippableAttachments[j].AttachmentAsset` | 201,810 | 3,882,298 | Object NetGUID |
| `KillData[i].AssistingPlayers[j].AssistingPlayers` | 31,726 | 324,291 | Actor NetGUID |
| Total | 435,346 | 8,088,887 | 8,524,233 new typed rows |

Values populate `value_i64`; each child retains its exact `raw_bits` and
`bit_count`. An explicitly decoded zero is a null object reference. It is
different from a null `value_i64`, which means no value was decoded.

All 8,168,216 observed equipment references resolve in the corresponding
main or checkpoint GUID table. All 356,017 assist references resolve in the
matching actor table. No zero or unresolved reference occurs in this corpus.
For checkpoint joins, include `checkpoint_index`; identifiers from different
replays or checkpoints must not be combined into one global lookup.

These are replicated observations. A resolved actor reference does not prove
which player owned that actor at an event time, how assist credit is awarded,
or how repeated updates should be deduplicated. Socket and attachment names
come from the replay declaration; the code does not assign additional asset
classes or gameplay meanings from their names.

## Admission and failure behavior

The existing exact build, group, parent name and parent checksum gate remains
in force. Each nested container and member also requires its replay-local
handle, name and compatible checksum:

| Member | Handle | Compatible checksum |
|---|---:|---:|
| `EquippableAttachments` | 13 | 3137596882 |
| `SocketAsset` | 14 | 3666994016 |
| `AttachmentAsset` | 15 | 856446005 |
| Outer `AssistingPlayers` | 6 | 1689463717 |
| Inner `AssistingPlayers` | 7 | 1417448159 |

The container is admitted only when no overlay type contradicts the measured
route. Members accept an absent overlay or `ObjectNetGuid`; explicit `Raw`,
`Skip`, and conflicting types are refused. These member types are not enabled
on unrelated direct fields merely because their handles or names match.

Nested parsing requires explicit element/root terminators, complete bit
consumption, valid indices, at most 4,096 elements and 128 fields per element,
and exactly consumed unsigned packed references. Empty zero-bit windows,
zero-width members and unexpected handles are rejected. An invalid member
discards the entire nested result, including any valid prefix. The old raw
container still survives and existing error counters expose decoding failures.
Array walker counters describe walked windows; on a later value refusal they
can include windows that produced no exported child row.

## Measurement and validation

The frozen candidate exported all 714 replays successfully. Its four-build
pilot, the full independent comparison of all 714 files, both full-corpus
guards and the reference baselines passed.

The comparison checks every old row and decoded value, exact new child
values/raw windows/coordinates, immediate parent-child adjacency, and revised
checkpoint block spans. Ten other Parquet tables must remain byte-identical.
Only the two main array counters, two checkpoint array counters and checkpoint
field-row count may change, by the independently derived amounts.

Typed presence below was counted directly from all four value columns across
all 714 retained exports. Zero, false and empty strings count as populated.

| Scope | Physical field rows | Rows with a typed value | Presence |
|---|---:|---:|---:|
| Main | 1,020,564,717 | 718,211,819 | 70.3740% |
| Checkpoint | 285,420,158 | 219,765,654 | 76.9972% |
| Combined | 1,305,984,875 | 937,977,473 | 71.8215% |

The prior value-only batch measured 71.6363% on fewer rows; this one adds typed
child rows, so numerator and denominator both grow. See
[ARRAY_LEAF_TYPES.md](ARRAY_LEAF_TYPES.md) for the preceding sixteen direct
member types and their remaining interpretation limits.
