# Serialized section observations

`tools/extract_section_observations.py` retains and validates the five measured
DamageableComponent routes from one Parquet export:

```powershell
python tools/extract_section_observations.py --export out/replay --out out/sections.json
```

| Route | Array | Serialized amount |
|---|---|---|
| MulticastNotifyDamage_Point | LifeChangeEvents | DamageTaken |
| MulticastNotifyDamage_Base | LifeChangeEvents | DamageTaken |
| MulticastNotifyHeal | LifeChangeBySection | HealTaken |
| MulticastNotifyOverhealDecay | LifeChangeBySection | DecayApplied |
| MulticastSectionLifeChange | LifeChangeEvents | No amount relation assigned |

Each section records ChangedComponent, LifeResult, DeltaLife and
bAliveAfterChange. The tool checks the complete raw array window and compares
every emitted member with its corresponding raw parent bits and wire order.
The declarations used to interpret these members have measured handle/name/
checksum identities from builds 13.01, 13.02, 13.04 and 13.05. Other RPC fields
remain available as raw source evidence without new semantic interpretation.

## Observation identity and missing information

A record groups rows by time, packet, channel, actor, object, export group and
outer RPC handle. Groups retain physical Parquet order. These coordinates are
not an independently proven RPC invocation identifier; group counts must not
be presented as unique gameplay actions.

The output distinguishes a parentless RPC, malformed or orphan array children,
an array with resolved non-health sections, and unresolved section references.
A missing HealthDamageSection match is not evidence of absence when another
section's identity remains unknown. Multiple health references are labelled
separately. No missing value is filled with 100, zero or a preceding state.

An amount can remain readable when its array is absent. The scalar is retained
independently, while the parentless observation supplies no section state.
RespawnNumber, VictimRespawnNumber and LifeChangeEventIndex remain separate raw
tokens. In particular, reset words are not assigned signedness from their
names, and inventory properties are not substituted for component lifetimes.

## What the values establish

LifeResult is reported section state. DeltaLife is a serialized member whose
relationship to that state depends on the route and preceding observations.
The amount comparison sums section deltas in f64, rounds once to f32 and
compares exact f32 bits, including signed zero. Iterative f32 accumulation is
also reported; it can produce a different result and is not silently substituted.

DamageTaken is compared with the negative of the section delta sum. HealTaken
and DecayApplied are compared with the positive sum. This scalar comparison
does not establish a continuous state transition. DecayApplied has positive
magnitude while the corresponding section can decrease. Reset observations
have no assigned delta-edge relation.

Values above 100 and zero values with a true alive flag remain unchanged.
They do not establish pool maxima, death, effective HP restored, armour
absorption, or healing credit. Actor lifetimes, respawns and ambiguous event
ordering need a separate validated join before such conclusions are possible.

## Output contract

The schema-version-1 JSON contains `route_declarations`, `observations`,
`checkpoint_observations`, `counts` and `provenance`.

- `source_rows` preserves every selected row, its raw window, typed columns,
  coordinates and physical ordinal. Parent and child evidence is not extra
  gameplay activity.
- `section_state.status` describes raw-array validation. Check
  `eligible_for_state_comparison`, `schema_errors` and `ambiguity_reasons`
  before using its interpreted members. Eligibility means the local serialized
  observation passed these checks; it does not prove continuity with another
  observation or a player identity.
- `section_state.relation` records the scalar comparison where applicable.
  Disagreement is visible, and does not become a zero amount.
- Checkpoint rows retain checkpoint index and ID and remain separate state
  evidence. They are not appended to main observations or counted as actions.
- Counts include explicit zero buckets and each of the five routes, including
  routes absent from a file. Missing unused declarations do not invalidate
  unrelated observed routes.

Input tables and implementation sources are hashed before and after extraction.
Raw/typed integrity conflicts fail before publishing output. Other malformed
or unsupported observations remain labelled with their source evidence. The
JSON write is atomic, and existing export files and implementation files are
protected from output aliases, including hardlinks.

The existing [healing extractor](HEALING_OBSERVATIONS.md) additionally resolves
causer and recipient corroboration. Its results remain separate; this broader
section view does not replace those checks or add player-credit semantics.

## Measured corpus and verification

The producer and an independent raw-array reader were compared over all 714
accepted exports: 215 on build 13.01, 204 on 13.02, 108 on 13.04 and 187 on
13.05. Every observation's source rows, coordinates, raw/typed values, section
references, scalar comparisons, tokens, declaration evidence and aggregates
matched. The results retain 2,883,168 coordinate groups, 4,153,928 sections and
49,626,044 source rows in 714 JSON files (39,397,631,405 bytes).

| Route | Observations | Raw-validated arrays | Parentless observations |
|---|---:|---:|---:|
| Damage Base | 220,823 | 219,287 | 1,536 |
| Damage Point | 447,688 | 447,589 | 99 |
| Heal | 1,526,040 | 1,526,040 | 0 |
| Overheal Decay | 387,660 | 387,660 | 0 |
| Section Life Change | 300,957 | 300,957 | 0 |

Schema-invalid and invalid-array counts are zero. No matching checkpoint rows
were observed. This does not establish future checkpoint schemas or justify
merging checkpoint state into main observations.

There are 46 scalar-comparison disagreements in 45 exports, all on Damage
Point. Every difference is one positive f32 ULP, at most 0.0000152587890625.
Those 46 scalars agree with iterative f32 accumulation. However, iterative
accumulation disagreed on 1,297 observations in the 11-file pilot, where
rounding once agreed. Neither rule is promoted to a universal game algorithm.
The exact values and both calculations remain available, and all 46 warnings
and their excluded comparison eligibility are independently verified.

All input and implementation hashes matched before and after processing.

Focused behavioral checks cover complete arrays across all five routes,
declaration changes, parent/child disagreement, strict values, ordering,
missing records, section identity and output aliases. Removing each of four
production guards makes its targeted check fail. The independent comparison
rejects eleven altered outputs and three altered exception reports; normal
JSON reserialization passes. This derived view does not increase typed-row
coverage or establish a new percentage of semantic completeness.
