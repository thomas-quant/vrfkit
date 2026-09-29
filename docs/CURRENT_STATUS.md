# Current status

The current state is the 2026-09-28 common verification of all 24 supported
builds: [BUILD_VERIFICATION.md](BUILD_VERIFICATION.md) has the per-build replay
counts, the shared acceptance rule and the resolved findings. All 1,018 unique
replays pass, 38 of them 13.06 (32 added since the 986-replay audit of
2026-09-25). The [README support table](../README.md#supported-valorant-builds)
reports the same run, and the README carries the workspace test counts.
Start with [DATA.md](DATA.md) for the schema and [USAGE.md](USAGE.md) for
commands; [LEGACY_BUILD_SUPPORT.md](LEGACY_BUILD_SUPPORT.md) records how
11.06--12.09 were added.

## Historical physical field inventory, 2026-09-09

The accepted inventory of 714 replays, parser `fc50bfe` (later derived-view
commits did not change it):

| Table | Physical rows | Rows with a typed value |
|---|---:|---:|
| Main `fields` | 1,020,577,224 | 726,993,845 |
| `checkpoint_fields` | 285,420,158 | 220,648,387 |
| Combined | 1,305,997,382 | 947,642,232 |

The combined typed-value presence is 72.5608%: physical rows with at least one
non-null typed value column, not semantic coverage, gameplay accuracy or a
share of known field meanings. The untyped rows -- named properties and whole
RPC payloads without an accepted type, anonymous or checksum-unresolved
fields, raw parents whose children may be only partly understood, and decoded
movement rows whose input bytes are not duplicated -- need separate evidence
and denominators; [CURRENT_RAW_BACKLOG.md](CURRENT_RAW_BACKLOG.md) catalogues
them. Numeric FastArray structure over the AbilitiesAndBuffs windows is in the
[GAS and PatchVolume investigation](GAS_AND_PATCHVOLUME_INVESTIGATION.md), and
the PatchVolume cells in [GROUND_VOLUMES.md](GROUND_VOLUMES.md); neither adds
typed Parquet values, so the counts above stand.

## Completed evidence phases

The main completed phases are recorded in
[TRANSPORT_PRESERVATION.md](TRANSPORT_PRESERVATION.md),
[PARTIAL_HEADER_CORRECTION.md](PARTIAL_HEADER_CORRECTION.md),
[CHECKPOINT_SCHEMA_PRESERVATION.md](CHECKPOINT_SCHEMA_PRESERVATION.md),
[CHECKPOINT_PATH_RESOLUTION.md](CHECKPOINT_PATH_RESOLUTION.md),
[STRUCTURED_ARRAY_EXPANSION.md](STRUCTURED_ARRAY_EXPANSION.md),
[NESTED_ARRAY_REFERENCES.md](NESTED_ARRAY_REFERENCES.md),
[TEXT_HISTORY_EXPANSION.md](TEXT_HISTORY_EXPANSION.md),
[REFERENCE_VALUE_EXPANSION.md](REFERENCE_VALUE_EXPANSION.md),
[TARGETING_AND_HEAL_VALUES.md](TARGETING_AND_HEAL_VALUES.md),
[HEALING_OBSERVATIONS.md](HEALING_OBSERVATIONS.md),
[KILL_OBSERVATIONS.md](KILL_OBSERVATIONS.md),
[KILL_LEDGER.md](KILL_LEDGER.md),
[SECTION_OBSERVATIONS.md](SECTION_OBSERVATIONS.md),
[SECTION_TIMELINE.md](SECTION_TIMELINE.md), and
[SECTION_PACKET_TIMELINE.md](SECTION_PACKET_TIMELINE.md). Each document states
its own evidence boundary; derived section and kill views do not prove game HP,
healing attribution, damage attribution, causality, or player credit.
