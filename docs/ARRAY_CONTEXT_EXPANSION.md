# Measured arrays and checkpoint block context

This report describes the array/context batch at `217bf39`, before checkpoint
GUID path reconstruction. Its counts are historical; see
[Checkpoint path resolution](CHECKPOINT_PATH_RESOLUTION.md) for the subsequent
field expansion, then [structured-array expansion](STRUCTURED_ARRAY_EXPANSION.md)
for the current candidate measurement and validation state.

The 2026-09-08 corpus contains 714 replays: 13.01 (215), 13.02 (204),
13.04 (108), and 13.05 (187). The candidate was exported with checkpoints;
all 714 exports succeeded.

## Additional array fields

Three exact group/name/checksum routes now expose their struct-array children.
The release branch must also match one of the four measured builds. Existing
raw parent rows remain; children appear immediately before their parent.

| Parent | Main children | Checkpoint children | Newly typed children, combined |
|---|---:|---:|---:|
| `OwnerExclusivePlayerInfo.AllPlayersObfuscatedPlayerInformation` | 301,957 | 4,139,586 | 2,989,962 |
| `EffectManagerComponent.ServerActiveEffects` | 1,562,088 | 0 | 953,064 |
| `FiniteSpeedMovementComponent.RequestedIgnoreActors` | 2,266,671 | 0 | 0 |
| Total | 4,130,716 | 4,139,586 | 3,943,026 |

These are additional views of existing payload windows, not independent game
events. All 8,270,302 child windows preserve raw bits. Typing is limited to
the independently measured bool, enum, packed-reference, f32 and f64-vector
windows. `Translation` and `Scale3D` have no ordinary property overlay and
are typed only inside the qualified effect array. Other members remain raw.
`RequestedIgnoreActors` packed integers are not promoted to actor identities.

New routes require explicit terminators and exact consumption. The legacy
array decoder accepts an optional trailer for its existing callers; the new
flat-array entry point does not. A failed walk emits no new children and
retains the raw parent and diagnostics. At this historical point
`TrackedRewards` remained excluded: 4,470 observed parents left eight bits
beyond the strict terminator. The later candidate has a separate exact
opaque-empty-variant rule; see `STRUCTURED_ARRAY_EXPANSION.md`.

## Checkpoint block context

`checkpoint_blocks.parquet` contains 17,251,536 blocks, including 5,303,355
blocks with no fields. Its physical spans cover all 205,866,627 checkpoint
field rows. The table preserves the class GUID's presence separately from
an invalid zero value, the resolver's selected branch, its memo-hit status,
and the paths available when the block was processed.

The numeric-group population is unchanged: 9,441,882 blocks with 15,875,413
field rows. Their observed resolution branches are:

| Unresolved fallback | Blocks | Field rows |
|---|---:|---:|
| Actor GUID path | 7,358 | 14,716 |
| Class GUID path | 82,455 | 182,293 |
| Object GUID path | 9,352,069 | 15,678,404 |

None of these observations alone establishes a class. The object-path subset
with a named outer has 651,948 blocks / 2,114,198 fields and is a candidate
for further schema matching. An enclosing actor or outer instance name must
not be substituted for a subobject's class.

## Counting coverage

The final physical field counts are 1,010,086,119 main rows with 713,488,311
typed values, and 205,866,627 checkpoint rows with 160,515,901 typed values.
Physical typing is therefore 70.6364% main, 77.9708% checkpoint, and 71.8781%
combined. It is not the fraction of game information understood. The ratio
can decrease as more raw child windows become separately accessible, even
when all existing values survive and millions of new values become typed.

Candidate binary SHA-256:
`6c9b2eb16e793f587ffced6921631a4d2836661b3ed50f2e58fcf352f6827095`.

See [output schemas](USAGE.md) for consumer-facing column definitions.
