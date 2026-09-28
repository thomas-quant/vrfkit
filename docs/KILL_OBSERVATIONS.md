# KillData serialized observations

`tools/extract_kill_observations.py` converts the measured `KillData` array rows in one export into a versioned JSON observation stream:

Use an export generated with `--checkpoints`; the tool reads both main and
checkpoint field, declaration and reference tables.

```powershell
python tools/extract_kill_observations.py --export out/nested --out out/kill-observations.json
```

**Builds.** The extractor accepts exports from 11.06--12.09 and 13.01--13.06
(`MEASURED_BUILDS`) and refuses 12.10, 12.11, 13.00 and any other build before
reading a row. The legacy builds were added
on 2026-09-28, once the parser emitted KillData children on them (see the
[legacy route table](LEGACY_BUILD_SUPPORT.md#measured-array-routes-2026-09-28)).
The committed extractor then accepted all 48 available legacy exports, three
per build, made by parser `2e7acce` with `--checkpoints`: 7,334 main and 8,876
checkpoint parents, 7,347 and 73,518 element updates (12 of them main partial
updates), and 2,525 and 25,419 assisting references. No nonzero reference was
unresolved in its own scope. Exports of the same replays by parser `259ed10`,
which emits no KillData children there, fail with `child count before parent
differs from raw array`. 12.10, 12.11 and 13.00 stay refused: the parser emits
no KillData children on them, and their only fixtures contain no KillData.

These records are serialized element updates. They are not a deduplicated kill ledger. A replay can repeat a payload, send more than one element in a parent array, or send a partial element containing only `bDidKillTriggerFinisher`. Missing fields therefore remain JSON `null`/absent and are never copied from another record.

Each observation is identified by its source table, checkpoint identity when applicable, physical `KillData` parent-row ordinal, and array element index. It retains packet/object coordinates, a parent-raw digest, exact member raw windows, and the serialized values currently measured for victim, equippable class, weapon theme, assisting players, damage type/value/region, game time, round timestamp/number, and finisher flag.

`time_ms`, `GameTimeElapsed`, and `RoundTimestamp` are separate source values. The tool does not convert one into another or claim gameplay units. Names such as `Victim` and `DamageTaken` are replay declarations; the extractor does not infer attribution, ordering, credit rules, or action semantics from them.

References keep their numeric identifier and scoped resolution status. Victim and assisting-player references are checked against the matching main or checkpoint actor table. Other references are checked against the corresponding NetGUID table; unresolved identifiers remain in the output.

The extractor fails when declarations differ from the measured group/name/checksum identities, array framing is incomplete, a nested handle is unexpected, a typed child differs from its raw window, child coordinates cross scopes, or emitted children are not physically adjacent to their parent. It records the manifest, input Parquet, and extractor SHA-256 values under `provenance`.

## Measured builds

The extractor reads a build only after its KillData identities and values have
been measured: 13.01, 13.02, 13.04 and 13.05 on 2026-09-08 (the 714-export
corpus in the [ledger's validation scope](KILL_LEDGER.md#validation-scope)),
and 11.06--12.09 (above) and 13.06 on 2026-09-28. Any other build, including a
future 13.07 whose
declarations happen to match, fails with `replay build is outside the measured
KillData set` before a row is read. Support in the parser is not enough.

The 13.06 measurement used all 38 13.06 exports that parser `259ed10` wrote
with `export --checkpoints` for the 1,018-replay common audit (executable
SHA-256 `d08b8d18...`). Each export ran
`python -W error tools/extract_kill_observations.py --export <dir> --out <json>`,
four at a time, and all 38 exited 0. So on every export the main and checkpoint
declarations matched the measured identities, every emitted child matched its
parent raw window, and every typed value matched the extractor's own decode of
its raw bits.

| 13.06, 38 exports | Main | Checkpoint |
|---|---:|---:|
| KillData parent rows | 5,371 | 6,434 |
| Element updates | 5,381 | 53,563 |
| Of which partial (finisher only) | 8 | 0 |
| Assisting-player references | 1,819 | 18,550 |
| Unresolved actor / NetGUID references | 0 / 0 | 0 / 0 |
| Null `KillingEquippableClass` references | 15 | 122 |

Every present victim and assisting-player reference resolves to an actor, and
every present damage-type and nonzero equippable-class reference to a NetGUID;
the partial updates carry no references. Two checks read the Parquet directly,
without the extractor's selection code: the `KillData` parent rows (group
`PlayerMatchStatsComponent`, checksum 1493759848) equal the extractor's parent
count in both tables of every export, and all 62,577 main and 577,458
checkpoint rows named `KillData` or `KillData[...` are accounted for as a
parent, a member window or an assisting-player reference. For comparison, the
401 13.05 exports of the same audit also have zero unresolved references and
153 null equippable-class references among 54,412 complete main updates.

Output uses schema version 1 and top-level kind `vrfkit_killdata_observation_export`. Consumers should reject unsupported schema versions.

For component-local base/revision state and explicit links to character-death
events, use the [kill ledger command](KILL_LEDGER.md). It retains this original
observation stream and keeps unmatched records visible.
