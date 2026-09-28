# Character-death events and KillData state

`tools/extract_kill_ledger.py` produces one JSON document that retains labelled
character-death events, component-local KillData state, and explicit links
between them. Use an export generated with `--checkpoints` from a measured
build, 11.06--12.09 or 13.01--13.06; 12.10, 12.11, 13.00 and any other build
are refused (see [measured builds](KILL_OBSERVATIONS.md#measured-builds)):

```powershell
python tools/extract_kill_ledger.py --export out/replay --out out/kill-ledger.json
```

The document has schema version 1 and kind
`vrfkit_character_death_ledger`. It retains the complete
[serialized observation stream](KILL_OBSERVATIONS.md) in `source_observations`.
Consumers should reject unsupported versions.

## State and checkpoint snapshots

`state_projection.entities` uses `(export_id, object_net_guid, element_index)`
as its identity. A complete main observation creates a base state. Subsequent
finisher-only updates to that exact key create revisions, including updates
that repeat the existing value. `base`, `latest` and every revision retain raw
member windows and the source table, physical parent ordinal, packet, clock,
actor and object coordinates.

`tools/kill_state.py` validates this projection. Partial updates before a base,
duplicate complete keys, conflicting component ownership, duplicate physical
coordinates and unsupported partial member sets fail explicitly. It does not
infer a missing base or silently apply an unmeasured partial shape.

Checkpoint observations are snapshots. They never create entities or increment
the main entity count. Each snapshot is compared against the base and all
revisions using exact values and raw windows; matches retain every matching
revision index. Missing entities, incomplete snapshots and unmatched states
remain in `checkpoint_snapshots.unresolved`. These comparisons establish state
equality, not the wall-clock ordering of checkpoint serialization.

## Event identity and matching

All `characterDeath` and `roundStarted` rows retain their physical event-row
ordinal, original columns and raw payload hex. The tool independently validates
the entire measured payload: tag, reference/round words, bounded FString,
terminator, exact float value and agreement with the outer millisecond time.
Invalid payloads remain visible and cannot establish a join.

The character-death words reference character pawns. KillData owner and victim
references identify PlayerState actors. The tool bridges each pawn through its
top-level `PlayerState` field within its active actor lifetime, checking the
reference against raw bits. Reopening resets the available mapping; dormancy
does not close an actor. Unknown or conflicting latest values remain unresolved.
An actor or mapping change at exactly the event timestamp is unresolved because
events do not carry a packet ID that would establish ordering.

A match requires all of the following:

- Both pawn identities resolve to the KillData owner/victim PlayerState pair.
- KillData `round_number` equals the unique, strictly prior validated
  `roundStarted` word. Missing, duplicate, invalid or equal-time round boundaries
  remain unresolved.
- KillData's main `time_ms` is between 0 and 50 ms after the event's `time1`.
- Each side has exactly one candidate. A greedy traversal or nearest timestamp
  never resolves ambiguity.

The time bound is a measured corroboration window, not a guarantee for every
future replay. `GameTimeElapsed`, `RoundTimestamp` and the other source clocks
remain separate. The tool does not convert them into a shared gameplay clock.

`death_events[].killdata_join` records matched, no-candidate or ambiguous status.
Unmatched main observations retain candidate event ordinals. Their optional
`different_killer_same_victim_context` records same-round nearby events with the
same victim and a different killer identity; it does not turn those events into
matches. `same_player_state` describes identity equality and is not a suicide
classification. These populations must not be summed as ordinary player kills.

## Provenance and failure behavior

The output records SHA-256 values for nine input Parquet tables, the manifest,
and four implementation files. Sources are checked before and after extraction.
An optional `--observations observations.json` verifies an existing observation
document: both its receipts and its entire content must match fresh extraction.
This option checks a saved document; it does not skip reading the source data.

Missing references stay null with explicit status. Count keys include zero
values. Input/schema/value violations return a nonzero exit; a successful
document is written atomically. Output paths that alias input tables, the
observation document or the implementation files are refused.

## Legacy builds, 2026-09-28

The ledger reads KillData through the [observation extractor](KILL_OBSERVATIONS.md),
so it accepts the builds that extractor admits. 11.06--12.09 were added once the
parser emitted KillData children on them (see the
[legacy route table](LEGACY_BUILD_SUPPORT.md#measured-array-routes-2026-09-28)).
The committed command completed all 48 available exports, three per build,
made by parser `2e7acce` with `--checkpoints`, one process at a time.

| Population | Count |
|---|---:|
| Character-death events, with fully validated payloads | 7,381 |
| Complete main KillData entities | 7,335 |
| Mutually unique same-round identity joins | 7,332 |
| Unmatched character-death events | 49 |
| Unmatched main KillData observations | 3 |
| Ambiguous event/main-observation joins | 0 / 0 |
| Finisher revisions that change the previous state | 12 |
| Finisher revisions that repeat the previous state | 0 |
| Checkpoint snapshots matching an existing state | 73,518 |
| Unresolved checkpoint snapshots | 0 |
| Validated round-start events | 985 |

Every killer and victim pawn resolved to a PlayerState, and no death round was
unresolved. Matched replication lags range from 8 to 35 ms. All 49 unmatched
death events have the same resolved PlayerState on both sides, and all three
unmatched KillData observations retain same-round, same-victim context with a
different killer identity. As on 13.x, these are observed attribution
differences, not suicide or kill-credit rules. This run checks the tool's own
invariants on these exports; it did not repeat the independent identity
reconstruction and join comparison recorded for the 714 13.x exports below.

## Validation scope

The final command completed all 714 retained exports: 215 from 13.01, 204 from
13.02, 108 from 13.04 and 187 from 13.05.

| Population | Count |
|---|---:|
| Character-death events, with fully validated payloads | 101,966 |
| Complete main KillData entities | 101,212 |
| Mutually unique same-round identity joins | 101,179 |
| Unmatched character-death events | 787 |
| Unmatched main KillData observations | 33 |
| Ambiguous event/main-observation joins | 0 / 0 |
| Finisher revisions that change the previous state | 197 |
| Finisher revisions that repeat the previous state | 0 |
| Checkpoint snapshots matching an existing state | 1,010,872 |
| Unresolved checkpoint snapshots | 0 |
| Validated round-start events | 13,772 |

Independent state reconstruction verified every base, latest state, revision
and checkpoint match. All 197 partial updates change `false` to `true`. An
earlier investigation inferred nine unchanged updates by subtracting the 188
slots with two complete observed variants from 197 partials. Direct comparison
against each prior main state disproved that inference.

Synthetic Parquet/CLI tests exercise raw-value mismatches, forged observation
receipts/content, missing event times, source-output aliases and contextual
events that must remain unjoined. Six deliberately broken implementations were
rejected by their behavioral tests. Independent identity reconstruction and
join comparison passed
all 714 exports. Matched replication lags range from 5 to 41 ms; increasing
the matching cap from 50 to 100 ms changed no match in this corpus. A separate
audit verified every original event column and every candidate/unmatched edge.

Of the 787 unmatched death events, 760 have the same resolved PlayerState on
both sides, 25 have distinct resolved PlayerStates, and two have an unresolved
identity. All 33 unmatched KillData observations retain nearby same-round,
same-victim context with a different killer identity. These are observed
attribution differences; they do not establish suicide or kill-credit rules.

This derived view does not change the parser's physical typed-row percentage
or establish an overall gameplay-semantic coverage percentage.

### 13.06, measured 2026-09-28

With 13.06 admitted, the same command completed all 38 13.06 exports that
parser `259ed10` wrote with `--checkpoints` for the 1,018-replay common audit.
Both extractors exited 0 on every export:
`kill_state.py` accepted every base, revision and checkpoint snapshot, and
every death and round-start payload passed its independent validation. The
figures above remain the 13.01--13.05 measurement.

| 13.06 population, 38 exports | Count |
|---|---:|
| Character-death events, with fully validated payloads | 5,408 |
| Complete main KillData entities | 5,373 |
| Mutually unique same-round identity joins | 5,370 |
| Unmatched character-death events | 38 |
| Unmatched main KillData observations | 3 |
| Ambiguous event/main-observation joins | 0 / 0 |
| Finisher revisions that change the previous state | 8 |
| Finisher revisions that repeat the previous state | 0 |
| Checkpoint snapshots matching an existing state | 53,563 |
| Unresolved checkpoint snapshots | 0 |
| Validated round-start events | 726 |

Every killer and victim pawn resolved to a PlayerState, and every death found
its round. Matched replication lags range from 7 to 30 ms, and rerunning the
join with a 100 ms cap changed no match. Of the 38 unmatched death events, 37
have the same resolved PlayerState on both sides and one has distinct ones.
All three unmatched KillData observations retain same-round, same-victim
context with a different killer identity. Two further checks sit outside the
tools: each export's `characterDeath` and `roundStarted` row counts, read
straight from `events.parquet`, equal the document's; and the retained
`source_observations` equal the standalone observation document after
canonical JSON. For comparison, the 401 13.05 exports of the same audit
give 54,872 validated death events, 54,382 joins, 490 unmatched events,
30 unmatched observations and lags of 7 to 41 ms, again with no ambiguous join
or unresolved checkpoint snapshot.
