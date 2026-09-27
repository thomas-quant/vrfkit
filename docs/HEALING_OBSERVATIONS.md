# Serialized healing observations

`tools/extract_healing_observations.py` derives healing observations from one
Parquet export generated with checkpoints:

```powershell
python tools/extract_healing_observations.py --export out/replay --out out/healing.json
```

The production extractor and an independent raw-source comparator passed all
714 measured exports. The output retains 1,526,040 main healing observations
and 13,711,667 source rows, including 7,833 valid observations without a
`HealCauser`.

## What the records mean

`MulticastNotifyHeal.HealTaken` and the `LifeChangeBySection` members describe
serialized healing amounts and resulting section state. A coordinate group
retains the export's timestamp, packet, channel, actor, object, group and outer
RPC handle. The export does not supply an independent RPC invocation ID, so
these groups are observations, not proven unique gameplay actions.

The section members are `ChangedComponent`, `LifeResult`, `DeltaLife` and
`bAliveAfterChange`. The raw array parent remains available alongside its
expanded children. Source evidence retains physical row ordinals and original
raw windows so a consumer can check the derived values against the export.

Serialized healing amounts are not established effective HP restored. The
preceding section state, clamping and repeated observations have not been
reconciled. In particular, summing `DeltaLife` does not establish a player's
healing contribution.

## Amounts and identities are separate

A missing `HealCauser` does not invalidate an otherwise verified amount.
Causer actor lifetimes and replicated `Owner`/`Instigator` references can
corroborate a character reference. Static manifest membership is an additional
check, not proof of the character's identity at the observation time or proof
that a player should receive healing credit.

The direct `EventInstigator` and `EventInstigatorPawn` fields are retained
separately from the causer's replicated references. Both are typed packed
NetGUIDs (see [typed direct references](#typed-direct-references-2026-09-28)).
`EventInstigatorPawn` names a character pawn that is open in the same export.
`EventInstigator` names that pawn's **PlayerController**: it equals the pawn's
replicated `Controller` and `Owner`, and in the 2026-09-28 corpus audit it
never resolved to an opened actor or a `net_guids` path -- the replay does not
open the controller as an actor. A failed join from `EventInstigator` to
`actors.parquet` is therefore expected and is not a decode fault; join it
through the pawn's `Controller` or `Owner` instead. A resolved pawn reference is corroborating evidence, not a
player-credit rule.

## Output and validation behavior

The schema-version-1 document contains `observations`, separately retained
`checkpoint_observations`, `counts` and `summaries`. Consumers should check
`amount.status` and `ambiguity_reasons` before aggregating an observation.
The supplied summaries include only validated, unambiguous main observations.
Zero error and checkpoint counts are explicit.

Every observation retains source rows with physical ordinals. The amount
validator decodes the raw array framing, checks its complete bit window and
compares each emitted child with its raw parent. Floats must equal the exact
f32 value promoted to f64, including the sign of zero. Duplicate members,
physical gaps and changed schemas remain visible and exclude a group from
validated summaries.

`source_corroboration` retains the direct RPC edges independently from the
causer actor and its replicated reference history. `recipient_corroboration`
records lifecycle status and static manifest membership separately. Equal-time
lifecycle boundaries, repeated opens, missing opens and conflicting references
must not establish a definite player-credit relation.

Input tables and implementation files carry SHA-256 receipts checked before
and after extraction. Raw/typed integrity violations fail before replacing the
output. A successful JSON write is atomic, and output paths cannot alias an
input or implementation file. Unsupported or incomplete observed amounts
remain as labelled records instead of silently becoming zero.

Each of the three direct source edges (`HealCauser`, `EventInstigator`,
`EventInstigatorPawn`) must carry a typed `value_i64` equal to its raw packed
window. An export written by a parser that predates this typing therefore
fails with `untyped reference` instead of quietly downgrading those edges;
re-export the replay with the current parser. `counts.source_edge_status`
tallies every edge status for the three edges, zeros included, because an
`invalid` edge (for example a malformed reference window) keeps its amount
validated and adds no ambiguity reason.

## Research baseline

Two independent readers and a separate per-file reconciliation examined all
714 accepted exports from builds 13.01, 13.02, 13.04 and 13.05.

| Measured population | Count |
|---|---:|
| Main coordinate groups | 1,526,040 |
| Groups with one section and exact `HealTaken`/`DeltaLife` agreement | 1,526,040 |
| Expanded section member windows checked against raw parents | 6,104,160 |
| Groups with `HealCauser` | 1,518,207 |
| Groups without `HealCauser` | 7,833 |
| Coordinate collisions | 0 |
| Matching checkpoint rows | 0 |
| HealthDamageSection references | 757,264 |
| OverhealDamageSection references | 767,682 |
| ChildRegionDamageSection references | 1,094 |

Observed amounts are positive, from 0.00000762939453125 to 500. Observed
`LifeResult` values range from 0 to 500. Each measured array has one section;
the raw parent is 177 bits in 1,523,416 observations and 185 bits in 2,624.
Those measurements do not guarantee the shape of future builds.

The six amount/section declarations are present with exact identities in all
714 exports. The three optional source declarations are jointly absent in
four exports containing 40 amount-only observations. Their absence does not
invalidate those amounts. Any observed source edge still requires its own
exact matching declaration.

The research run found no checkpoint observations on this route. This does not
establish that a future export cannot contain them; consumers must preserve
population distinctions instead of adding checkpoint state to main amounts.

## Production verification

The final extraction used eight workers and completed in 286.6 seconds from
existing Parquet exports. This is derived JSON extraction time, not replay
parsing time. Independent comparison completed in 149.3 seconds and checked
every observation's raw-derived amounts, physical source rows, child order,
section paths, source edges, lifecycle evidence, reference history, recipient
corroboration and serialized aggregates. All 714 JSON outputs are retained.

Main amount-invalid and ambiguous-group counts are both zero. The independent
comparison checks input and output hashes against the accepted parser corpus.
Normal JSON reserialization passes; eleven corrupted output variants and a
raw signed-zero mismatch are rejected. Separately, removing each of three
production guards causes its targeted behavioral test to fail. One earlier
test changed a section delta and was also rejected by the amount-sum check;
the corrected test changes only `LifeResult` to isolate parent/child agreement.

The full validation suites contain 692 Rust and 725 Python tests. The first
production attempt rejected four exports because it unnecessarily required
unused source declarations; that attempt is retained separately and is not an
accepted corpus. The final run preserves all 40 affected amount observations.

This tool derives a view from already parsed values. It does not increase the
parser's physical typed-row percentage or establish complete gameplay meaning.

## Typed direct references (2026-09-28)

The research and production runs above predate this change: they read
`EventInstigator` and `EventInstigatorPawn` as untyped raw windows. Both are
now typed `ObjectNetGuid` by exact group/name/checksum entries in
[`scoped_type_evidence.json`](../tools/fixtures/scoped_type_evidence.json),
which also records the corpus evidence (1,018 exports, 21 builds, 2,115,008
rows per field, every payload consumed exactly by two independent readers).
The extractor was changed in the same commit to require and cross-check the
typed values. The previous extractor, run on a typed export, still exited 0
but marked every such edge `invalid` with no counter moving; on the 13.01
reference export that was 1,894 `EventInstigator` and 1,894
`EventInstigatorPawn` edges.

Measured on three fresh exports (13.01 `02d4d478`, one 13.05 and one 13.06
replay) with the new parser and extractor: 7,711 observations, all amounts
validated, zero ambiguous groups. Every observation that carries the direct
references has all three edges `present` (7,653); the other 58 carry none of
them and report `absent`. `counts.source_edge_status` reports zero `invalid`,
`null` and `duplicate` edges. The same extractor on the older, untyped export
of `02d4d478` exits 1.
