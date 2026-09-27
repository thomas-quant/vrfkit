# Current raw-data inventory and investigation priorities

Measured 2026-09-09 on the 714 accepted exports from parser commit `fc50bfe`.
The later derived-observation tools through `14e58e5` do not change these
Parquet field counts. See [current status](CURRENT_STATUS.md) for the completed
work and [DATA](DATA.md) for the available values.

Follow-up: [numeric FastArray extraction](GAS_AND_PATCHVOLUME_INVESTIGATION.md)
now fully consumes all 2,882,152 AbilitiesAndBuffs inner windows. The public
standalone extractor retains numeric headers, deletion/change IDs, raw property
boundaries, and the original bits. This output is separate from Parquet typed
values, so the inventory below remains the measured starting point. The
remaining GAS task is item-schema and value interpretation.

Follow-up, 2026-09-28: the extractor's build scope was re-measured on 1,018
exports made by parser `259ed10`. All 3,999,493 inner windows across 22
builds close exactly, and the extractor now accepts those builds. The 12.10
and 12.11 exports have no rows on this route. See
[the build-scope entry](GAS_AND_PATCHVOLUME_INVESTIGATION.md#build-scope-re-measured-2026-09-28).
That entry also records AbilitiesAndBuffs bodies filed under
`/Script/ShooterGame.AresAbilitySystemComponent`, which the extractor does not
select. The inventory below is still the 2026-09-09 measurement, and the open
GAS task is unchanged.

Private evidence also validates a numeric FastArray walk over all 26,303
selected PatchVolume whole/tail windows. PatchVolume still lacks a public
extraction route and an established class/item/property schema.

## What the inventory counts

A physical field row is untyped only when all four value columns are null.
Decoded zero, false and empty strings are values. Main and checkpoint rows
are counted separately; repeated snapshots and overlapping parent/child raw
windows are not deduplicated into game facts.

| Physical untyped rows | Main | Checkpoint | Combined |
|---|---:|---:|---:|
| Preserved raw payload | 122,334,714 | 64,771,771 | 187,106,485 |
| Movement RPC marker, values exported separately | 168,172,728 | 0 | 168,172,728 |
| Zero-bit marker | 3,075,937 | 0 | 3,075,937 |
| Total | 293,583,379 | 64,771,771 | 358,355,150 |

The only observed nonempty route without retained raw bytes in the field
tables is `ReplaysClientReceiveRemoteCharacterUpdatesSingleArrayNoAutonomous`.
The parser sends its decoded movement to `movement.parquet`. This explains
that specific route, not any future missing-raw record. The inventory found
zero wrong raw-byte lengths. It does not prove complete gameplay semantics.

An independent aggregation reconciled all 36,839 catalog keys, all 714
per-export summaries, and all nine raw-count/bit-count metrics with the final
table totals. This is a catalog reconciliation, not a second Parquet scan.
The source scanner checked exact input hashes before and after scanning.

The conservative route classification further partitions the preserved rows:

| Preserved-raw category | Main rows | Checkpoint rows | Combined |
|---|---:|---:|---:|
| Parent on one of 16 reviewed structural routes | 5,826,058 | 1,470,527 | 7,296,585 |
| Reserved unresolved payload or recovered CNC inner window | 7,066,630 | 131,479 | 7,198,109 |
| Not classified by these rules | 109,442,026 | 63,169,765 | 172,611,791 |

Rules match exact observed group/name/checksum identities. The parent category
establishes that a structural reader exists, not that every parent succeeded
or every child has known meaning. The unclassified category may still include
values read by other consumers and other retained containers. It is not a
count of proven unknown game facts. All classified records were independently
reconciled to the original catalog keys, row counts and preserved bit totals.

## Priority populations

These main-stream populations are aggregated across builds 13.01, 13.02,
13.04 and 13.05. Bit totals rank retained payload volume, not information
content or expected semantic gain.

| Group / field | Physical rows | Preserved bits | Next question |
|---|---:|---:|---|
| `AbilitiesAndBuffsComponent` / unresolved CNC payload | 2,882,152 | 7,469,704,527 | Which item/property schema can be independently established? |
| Same group / `_cnc_h1` | 2,882,152 | 7,406,284,351 | Inner window of the previous population; do not count twice |
| BaseReplayController / `InputEventData` RPC parameter | 42,545,425 | 1,888,237,280 | What source establishes tag/action meanings? |
| `PatchVolume` / unresolved CNC payload | 19,140 | 304,471,076 | Which class/item schema explains the validated numeric entries? |
| `PatchVolume` / unparsed RepLayout tail | 7,163 | 236,962,614 | Which property schema explains the validated numeric entries? |

The AbilitiesAndBuffs numeric framing is established, but replication keys,
item IDs, and raw property windows do not identify abilities, casts, buffs,
effects, or player actions. Any interpretation must be tested against
independent references or state transitions while preserving counterexamples
and actor/channel lifetime boundaries.

The seven InputEventData tags and their lengths in DATA are a historical
53,605-row measurement. This new inventory measures the current row/bit
population; it has not independently revalidated tag frequencies or the
historical grammar on all 42,545,425 rows. Historical upstream C# byte-array
storage does not establish the wire grammar or action labels.

PatchVolume framing has private full-population structural evidence, but no
public extraction route or approved class/item/property schema. Any public
decoder must retain exact original windows and must not infer property meanings
from structural closure alone.

## Existing containers are not new decoder opportunities

Large raw parents such as `Rounds`, `SelectedV2`, `KillData`, `TrackedRewards`,
`AbilityCastsThisRound`, `RoundInfos` and section-change arrays already have
structural readers. Their parent rows intentionally remain raw alongside
children. An existing reader does not prove every child value is typed or
that every parent succeeded. Rank unresolved leaves and explicit decoder
failures separately instead of treating the whole parent as missing work.

For example, checkpoint `Rounds` parents alone carry 6,163,228,806 retained
bits. Their size is not evidence that combat-report decoding is absent.

## Follow-on work and admission criteria

1. Establish AbilitiesAndBuffs and PatchVolume item/property schemas against
   independent evidence before adding types. A successful bit walk alone is
   insufficient.
2. Associate existing healing, ability and GAS OwnerActor/AvatarActor
   references while retaining role conflicts, missing references and lifecycle
   uncertainty. Reference association must not become automatic player credit.
3. Investigate the 15 packet-eligible section arithmetic disagreements and
   46 scalar warnings before claiming effective HP or healing totals.
4. Probe StopEffectType width/domain before assigning a numeric type or enum.
   `extract_active_effects.py` currently derives actor lifetimes; it does not
   provide continuous-effect RPC pairing by EffectID.

Any new decoder should first pass a bounded pilot with exact raw/value
comparison and rejecting malformed controls, then a full-corpus comparison
that preserves previous values and reports zero and nonzero failures.
No overall semantic coverage percentage is inferred from this inventory.

## Retained evidence identity

The private `current-raw-inventory` artifacts are bound by SHA-256:

- `raw_untyped_catalog.json`: `fd33768aa1737d0f6c1cad830f8b0f96963d8eeed2b141b9e7c6db6773d5220a`
- `raw_untyped_summary.json`: `ea61ebeeab87f1c4727a1c133b2913ffdcb8e67af61ffa487052208d4fcd82d5`

The independent reconciliation is retained as `current-raw-root-audit/RESULT.json`.
The route classification is `current-raw-triage-terra/triage.json`, SHA-256
`d13413a95a0377ccbb472f21b517961917b9af617d1597049b955650dad69596`;
its per-key reconciliation is `current-raw-root-audit/TRIAGE_ACCEPTANCE.json`.
These artifacts describe physical rows and source bindings, not player data
that should be published with the repository.
