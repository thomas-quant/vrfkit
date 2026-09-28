# SelectedV2 and KillData values

The structural expansion preserved these arrays as named raw children. This
addition fills typed columns on sixteen qualified child fields. It introduces
no new rows and retains every raw payload and parent. Routes are limited to
the measured 13.01, 13.02, 13.04, and 13.05 builds and require the exact parent
identity plus each child's declared handle, name, and checksum.

Since the [2026-09-23 upstream parity update](UPSTREAM_PARITY.md) the parser
also enables these measured routes on 13.06. For `KillData` that was checked
on 2026-09-28: on all 38 13.06 exports of the common audit,
[`extract_kill_observations.py`](KILL_OBSERVATIONS.md#measured-builds) matched
every typed child against its raw window. `SelectedV2` on 13.06 was not part
of that check.

## Values exposed

| Array | Members | Output | Interpretation boundary |
|---|---|---|---|
| `SelectedV2` | `EquippableDataAsset`, `EquippableSkinDataAsset`, `EquippableSkinLevelDataAsset`, `EquippableSkinChromaDataAsset`, `EquippableCharmDataAsset`, `EquippableCharmLevelDataAsset` | `value_i64` | IntPacked object references; zero is a null reference, not a missing decoded value |
| `KillData` | `Victim`, `KillingEquippableClass`, `DamageType` | `value_i64` | Object references; `Victim` resolves against dynamic actor IDs, while the other references resolve against exported GUID entries |
| `KillData` | `DamageTaken`, `GameTimeElapsed`, `RoundTimestamp` | `value_f64` | Exact f32 values widened to f64; time units and game-event attribution remain unverified |
| `KillData` | `RoundNumber` | `value_i64` | Signed 32-bit wire value, observed 0 through 35 |
| `KillData` | `bDidKillTriggerFinisher` | `value_bool` | One-bit value, including an explicitly decoded false |
| `KillData` | `DamageRegion` | `value_i64` | Unsigned byte code, observed 0, 1, 2, and 5; enum labels are unverified |
| `KillData` | `WeaponTheme` | `value_str` | Observed true prefix followed by an exactly consumed, terminated FString; not classified as FName or SoftObjectPath |

Use `net_guids.parquet` or `actors.parquet` for main-stream references. For
checkpoint rows, also match `checkpoint_index` in the corresponding checkpoint
table. Membership demonstrates a reference target, not a complete event-time
ownership or player-attribution history.

The `SelectedV2` six-reference population contains 28,842,636 values across
714 files. Every nonzero ID resolves in its own replay/checkpoint context;
the null charm references are the only IDs absent from the static GUID table.
All 1,112,084 `KillData.Victim` references resolve to dynamic actor IDs even
though none occur in the static GUID table. These two kinds of reference
evidence must not be conflated.

`WeaponTheme` failed the exact FName interpretation on every observed row.
Its actual observed shape has a true prefix, signed FString length, strictly
valid UTF-8 or UTF-16 text, a null terminator for nonzero lengths, and no suffix.
The local reader checks the terminator explicitly because the generic FString
reader permits legacy unterminated strings. Malformed windows remain raw and
increment `array_leaf_decode_errors`.

## Remaining interpretation limits

`SelectedV2.A` through `.D` are 32-bit zero windows in this corpus; that alone
does not identify their type or purpose. `EquippableAttachments` and
`AssistingPlayers` retain their raw bodies and now expose qualified nested
references; see [NESTED_ARRAY_REFERENCES.md](NESTED_ARRAY_REFERENCES.md).
`SocketAsset` and `AttachmentAsset` are enabled only inside that verified
nesting, not as unrelated direct children.

No direct C# descriptor for these two parent groups exists in the consulted
local reference checkout. This is measured wire interpretation, with explicit
remaining semantic uncertainty. Repeated array updates and checkpoint snapshots
are not independent kills or a deduplicated inventory ledger.

## Historical validation of this value-only batch

The frozen candidate exported all 714 files successfully, and the independent
full comparison passed 714/714: every prior
row coordinate, raw window, and previously decoded value is unchanged; all new
values match independent decoding; eleven other tables are byte-identical.
Export quality counters are unchanged on every file; both corpus guards and the
reference output baselines pass.

The comparison confirms 39,963,673 newly typed values: 2,436,567 main and
37,527,106 checkpoint. There are no additional physical rows in this batch.

| Scope | Physical field rows | Rows with typed values | Presence |
|---|---:|---:|---:|
| Main | 1,020,129,371 | 717,776,473 | 70.3613% |
| Checkpoint | 277,331,271 | 211,676,767 | 76.3263% |
| Combined | 1,297,460,642 | 929,453,240 | 71.6363% |

These are the previously measured typed rows plus newly filled, previously
all-null targets: physical presence, not semantic completeness.

The current measurement includes additional nested reference rows and is
recorded in [NESTED_ARRAY_REFERENCES.md](NESTED_ARRAY_REFERENCES.md).
The prior structural-only coverage measurement is also historical:
[`STRUCTURED_ARRAY_EXPANSION.md`](STRUCTURED_ARRAY_EXPANSION.md).
