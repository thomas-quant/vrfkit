# Reward text histories and kill observations

`TrackedRewards[i].LocalizedRewardName` now exports a complete text-history
tree as JSON in `value_str`. Its original raw bytes remain available. This
recovers table identifiers, keys and ordered format arguments, including
nested text. It does not render a localized label or infer reward amounts.

The separate [KillData extractor](KILL_OBSERVATIONS.md) produces one snapshot
per serialized element update, preserving missing members, raw evidence,
reference status and independent clocks.

## Reward text output

The route requires the measured 13.01, 13.02, 13.04 or 13.05 build, exact
OwnerExclusivePlayerInfo group and TrackedRewards parent identity, and scoped
leaf handle 29 with name `LocalizedRewardName` and checksum `483770233`.
The generated descriptor remains Raw; this exact array route opts into the
full-tree reader. Skip, a conflicting overlay, changed declarations, or
malformed framing cannot silently become a typed value.

History 11 has this shape:

```json
{"flags":0,"history":11,"kind":"string_table","table":{"name":"/Game/GameModes/Bomb/BombMode_Strings.BombMode_Strings","number":0},"key":"Kill"}
```

History 3 has `kind: "format"`, a nested `source` tree and an ordered
`arguments` array. Each argument keeps its name and numeric wire tag. Tag 0
has `value: {"bits_u64":"1"}`: the decimal string preserves all 64 bits
without a JavaScript precision loss or a signedness claim. Tag 4 has a nested
text tree. Repeated argument names remain separate array entries.

The observed history-255 empty form is
`{"flags":0,"history":255,"kind":"empty"}`. Its wire length must be zero.
Unsupported histories or argument tags fail visibly, leaving raw data and
incrementing the array-leaf decode-error counter.

The reader checks UTF-8/UTF-16 validity and terminators, 64 KiB per-string byte
bounds, nonnegative FName suffixes, at most 128 arguments per format, nesting
depth below 16, a total budget of 256 text/argument nodes, and full bit
consumption. Format arguments are not restricted to the observed count of two.
The existing `FieldType::FText` key-string representation is unchanged for
other consumers such as `LocalizedStat`.

## History 4: a formatted number

The same reader also decodes history 4 (`AsNumber`), and the overlay type
`FieldType::FTextTree` applies it to a whole replicated property rather than
to one array leaf. Its first user is `BombGameState_C.OverrideMatchTimerText`
(and the Swiftplay game state's copy), typed by exact group, name and checksum
(`tools/fixtures/scoped_type_evidence.json`). That property sends two forms:
the 72-bit history-255 empty form, and a 376-bit history 4:

```json
{"flags":1,"history":4,"kind":"as_number","source":{"tag":3,"double":15.614009857177734},"format":{"always_sign":false,"use_grouping":true,"rounding_mode":0,"minimum_integral_digits":2,"maximum_integral_digits":2,"minimum_fractional_digits":2,"maximum_fractional_digits":2},"culture":""}
```

The wire layout after the 32 flag bits and the history byte: the source
value's type byte (`FFormatArgumentValue`; only 3, a double, is read -- any
other type is refused rather than read at a guessed width), the double, an
archive bool (a whole u32, 0 or 1) saying whether `FNumberFormattingOptions`
follow, then those options -- two archive bools, the rounding-mode byte and
four i32 digit limits -- and last the target culture as an FString. A
non-finite double is refused: JSON has no spelling for it. The legacy
`FieldType::FText` reader keeps only string-table keys, and refuses both of
these forms.

Measured 2026-09-28 on all 1,018 unique replays (11.06-13.06): 7,096 main and
14,008 checkpoint rows of the two identities, 14,005 of them the empty form
and 7,099 history 4, every one with a double source (0.01..36.95), format
options present and identical (no sign, grouping, rounding mode 0, two
integral and two fractional digits), an empty culture and flags 1. Each row
shares its packet with a `ShouldOverrideMatchTimer` row of the same actor:
true with the number, false with the empty form, without exception. An
independent Python reader (`validate_type_evidence.py`, type `FTextTree`)
agrees with the exported JSON on every row.

## Whole-corpus evidence

All 714 replays were exported with the candidate, covering 215 release-13.01,
204 release-13.02, 108 release-13.04 and 187 release-13.05 files. An independent
integer-shift Python reader checked every new tree against its raw bits.

| Scope | New typed text trees | History 11 | History 3 |
|---|---:|---:|---:|
| Main | 459,104 | 423,844 | 35,260 |
| Checkpoints | 414,605 | 383,377 | 31,228 |
| Combined | 873,709 | 807,221 | 66,488 |

Every old field row, coordinate, raw window and previously decoded value was
preserved. The other eleven Parquet tables were byte-identical, including
checkpoint block spans. The full manifest was unchanged except elapsed run
time. The independent 714-file comparison passed. The candidate executable
SHA-256 is
`5c33558bd3455979bb66a7b39193ccb30270dbcddd0340824f84c3f85f5f14c4`.

A fresh scan of all four value columns measured:

| Scope | Physical rows | Rows with a typed value | Presence |
|---|---:|---:|---:|
| Main | 1,020,564,717 | 718,670,923 | 70.4189% |
| Checkpoints | 285,420,158 | 220,180,259 | 77.1425% |
| Combined | 1,305,984,875 | 938,851,182 | 71.8884% |

Retained parents and their children overlap; this is physical presence, not
semantic completeness. Both pinned output baselines and both full-corpus guards
passed.

## Kill snapshots

The extractor was run on every accepted preceding export. It produced
101,409 main and 1,010,872 checkpoint snapshots. Exactly 197 main updates
contain only the finisher field; missing members remain null. It retained
31,726 main and 324,291 checkpoint assisting-player references.

The root audit independently re-summed every output, bound input/output
hashes, and reproduced the prior study's complete parent-row coordinate and
raw-payload identity digests for all 714 files. Main and checkpoint identities
remain distinct. GUID 0 is an explicit null reference; missing values are
separate from unresolved non-null identifiers.

The strings `Streak`, `IsMax`, `LossStreakFormat` and `MaxLossStreakLabel` are
observed reward-text identifiers. Numeric-looking sibling `Rewards` members
remain raw pending independent type/meaning evidence. Neither text arguments
nor KillData snapshots are promoted into a deduplicated economic or kill ledger.
