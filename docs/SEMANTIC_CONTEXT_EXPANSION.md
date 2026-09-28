# Checkpoint context and semantic evidence expansion

The 2026-09-08 audit uses all 714 exports from the corrected partial-header
parser, base commit `e4dec67`. Builds are 13.01 (215), 13.02 (204), 13.04 (108),
and 13.05 (187). Counts are physical observations, including repeated snapshots.

## Preserve checkpoint-local context

Every checkpoint creates its own GUID cache and replication state. The old
export discarded its actor rows and final GUID cache, while retaining fields
without a checkpoint identity. Bare numeric group names could not be reliably
investigated through the main-stream GUID table.

The output now adds non-null `checkpoint_index: UInt32` (zero-based source chunk
order) and `checkpoint_id: Utf8` (original wire string) before the existing
checkpoint field columns. Separate checkpoint actor and NetGUID tables use the
same identity columns. Main-stream writers and schemas are unchanged. Repeated
wire IDs remain distinguishable by index within a replay. Checkpoint actor opens
describe snapshot state and are not new spawns in the main timeline.

The GUID table reflects the cache after that checkpoint's frame walk. It does
not establish a temporal history of metadata updates inside a snapshot.
Movement snapshot rows remain explicitly counted if discarded; the full
714-export measurement found zero such rows. The new tables preserve 2,535,314
actor opens and 58,509,199 GUID cache entries. All main Parquet files remained
byte-identical, and every previous checkpoint field value and row order matched
across all 201,727,041 rows.

## Reviewed participant identity

All 2,883,039 synthesized participant subject observations matched exactly one
subject from independently parsed replay-header `playerLoadouts` data:

| Build | Main fields | Checkpoint fields |
|---|---:|---:|
| 13.01 | 75,252 | 753,739 |
| 13.02 | 76,916 | 783,998 |
| 13.04 | 38,075 | 368,789 |
| 13.05 | 70,620 | 715,650 |
| Total | 260,863 | 2,622,176 |

There were zero null, empty, unmatched, or ambiguous subject values. All rows
used `/Game/GameModes/Bomb/Bomb_CombatReportComponent.Bomb_CombatReportComponent_C`
and the exact indexed grammar
`Rounds[].Reports[].Interactions[].ParticipantSubject`. The corpus contains
298 concrete indexed paths. Synthesized checksums are null.

The [catalog](../tools/fixtures/semantic_evidence.json) records separate main and
checkpoint claims, exact group identity, and the four measured builds. Schema 2
supports literal identifier segments and decimal array indices only; generic
regex, prefix and suffix claims are excluded.

Existing BombPlayerState `Subject` checks also matched header identities:
6,924 main and 136,350 checkpoint rows. Main `SpawnedCharacter` has 6,993 nonzero
movement references and 131 zero sentinels. The old **checkpoint**
`SpawnedCharacter` comparison to main movement GUIDs crosses independent ID
spaces and cannot prove actor identity. It has been replaced with a full
checkpoint-local join: all 128,867 nonzero values match an actor open in the
same checkpoint, with a unique character class and archetype. The 473 zero
values retain the separately documented absent-character interpretation.

The resulting catalog selects 3,162,777 verified identity-property observations.
Numeric checkpoint group names remain unresolved: 15,875,413 rows lack enough
content-block context to establish their class. An enclosing actor's class is
not necessarily the class of a replicated subobject.

This is an explicit collection of verified identity properties, not a measure
of all game information understood. Typed-value coverage and semantic evidence
remain separate.

## Reload observation boundaries

The observation extractor recognizes the two observed reload state leaves,
`ReloadState` and `ReloadStateEmpty`. First/unknown entries are left-censored;
unknown exits, round resets and stream ends are right-censored. It preserves
the observed span separately from duration, which stays null when censored.
Round events split intervals at their actual timestamp.

Positive magazine transitions are supporting evidence only when linked through
a non-null, unambiguous same-weapon outer GUID strictly inside the observed
packet interval. Same-packet boundary updates and transitions whose previous
ammo sample crosses a round reset are excluded. No completed-reload or shot
count is inferred from an ammo increase.

The actual extractor was run on all 714 exports: 124,177 reload observation
intervals and 106,397 associated positive magazine transitions. It retained
per-export output and passed packet-boundary and censor checks. On eight
build-stratified exports, non-reload data sections matched the previous extractor.

## Next structural candidates

A strict independent framing probe walked every qualified raw parent in the
corrected corpus. It preserved every parent and required exact consumption:

| Parent | Rows | Strict framing successes | Failures |
|---|---:|---:|---:|
| OwnerExclusivePlayerInfo.AllPlayersObfuscatedPlayerInformation | 377,867 | 377,867 | 0 |
| EffectManagerComponent.ServerActiveEffects | 103,789 | 103,789 | 0 |
| FiniteSpeedMovementComponent.RequestedIgnoreActors | 98,408 | 98,408 | 0 |
| OwnerExclusivePlayerInfo.TrackedRewards | 644,129 | 639,659 | 4,470 |

All 4,470 rejected reward rows leave exactly one zero byte. That is a measured
shape, not permission to ignore it. These probes do not add production decoder
routes. RequestedIgnoreActors children consume as packed integers, but their
nonzero values do not resolve in the checked main GUID table, so they must not
be labelled ObjectNetGUIDs on that evidence.

The catalog records the verification digest and export-set digest.
