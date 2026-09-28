# Observed section timelines

`tools/extract_section_timeline.py` builds a main-stream section timeline
directly from a Parquet export. It calls the
[raw section extractor](SECTION_OBSERVATIONS.md) in process, then applies the
ordering policy in `tools/section_timeline.py`.

```powershell
python tools/extract_section_timeline.py --export out/replay --out out/timeline.json
```

The input consists of `manifest.json`, `fields.parquet`,
`checkpoint_fields.parquet`, `net_guids.parquet` and `actors.parquet`.
Checkpoint observations are not inserted into the main timeline. The raw
extractor counts remain in provenance so that this scope is visible.

## Values and identity

Each node identifies its original observation and section index, actor,
object, channel, ChangedComponent reference, time and packet. It retains
LifeResult, DeltaLife, bAliveAfterChange, the preceding same-reference node
ID and their numerical difference. Source row ordinals refer to the original
main Parquet table. Unknown section paths remain null; no missing state is
filled with zero, 100 or a prior value.

The output keeps two calculations separate:

- `scalar_relation` comes from the raw extractor and compares an RPC's amount
  with its own section deltas. A scalar-only rounding warning does not discard
  structurally valid section states.
- `route_arithmetic` compares the observed predecessor with the current
  section: f32(previous + DeltaLife) for damage and healing, and
  f32(previous - DeltaLife) for overheal decay. Reset observations have no
  incoming delta relation. These are measured arithmetic hypotheses;
  mismatches and missing predecessors remain visible.

The arithmetic can be reported even when interpreting the link is unsafe.
Values above 100 and zero values with a true alive flag are preserved. Neither
arithmetic equality nor raw-token equality establishes effective health,
armour absorption, death, respawn or player credit.

## Comparison eligibility

`continuity.eligible` means only that the two observed endpoints satisfy this
conservative ordering policy. `continuity.game_life` and
`continuity.component_life` always remain `unproved`.

Both endpoints must have matching active actor and channel open records.
Duplicate or regressed lifecycle clocks invalidate that evidence; dormancy
is not destruction. Same-millisecond lifecycle boundaries remain unresolved
under this policy, even if their packet numbers differ.

All same-time states for the same actor/object/section are marked ambiguous,
including the first member of a tie. A tied predecessor cannot seed an
eligible outgoing link. Actor object changes and clock regressions advance a
persistent actor epoch. Invalid or schema-invalid observations advance a
scope epoch, including for sections that reappear later. Non-scalar raw
ambiguities retain their values but censor both incident links.

Resets advance the scope epoch once per observation, before its sections.
They report absolute states and have no incoming delta edge. A later unique
update can compare with that absolute state if the other checks pass. A
section absent from an update does not acquire a fabricated value.

Raw tokens retain their exact source rows and roles. Comparison requires the
same route, role, one row at each endpoint, non-null raw bytes and bit width.
Missing, different or cross-route words censor comparison eligibility under
this policy; they are not asserted to be respawn transitions. Identically
named reset, damage and inventory fields do not establish a shared domain.

## Integrity and output

The JSON includes `nodes`, `barriers`, `scalar_warnings`, `counts` and
`provenance`. Node IDs pair the observation's first physical row ordinal with
the section index. Predecessor IDs and numerical differences remain available
for inspection even for ineligible links. A barrier retains its source rows
and reason. Counters include explicit zeros.

All five consumed input files and the implementation helpers are hashed before
and after extraction. Source changes or raw/typed integrity conflicts fail
before publication. Output uses an atomic write; aliases of any existing
export file or implementation helper, including hardlinks, are rejected.

## Measured corpus and verification

The producer and independent reference were compared across all 714 accepted
exports: 215 on build 13.01, 204 on 13.02, 108 on 13.04 and 187 on 13.05.
They retain 4,153,928 section nodes, 1,635 barriers and 46 scalar warnings.
There are 1,874,166 eligible ordered comparisons and 2,279,762 ineligible nodes.
These are section observations, not unique gameplay actions or a percentage
of total semantic completeness.

The exact comparison checks complete node and barrier identity sets, serialized
values, predecessors, numerical differences, predicted f32 values, token rows
and roles, scope/actor epochs, eligibility and mapped reason families. Active
lifecycle statuses and open-record IDs agree independently; the complete open
records also match the original actor table. Scalar warnings and their exact
values remain present. All input and implementation hashes match their pinned
receipts and remain unchanged during processing.

| Arithmetic result | Section nodes |
|---|---:|
| Matches the route hypothesis | 3,291,741 |
| Differs from the route hypothesis | 5,806 |
| No predecessor or reset delta edge | 856,381 |

The table excludes the 1,635 barriers. Of the eligible ordered comparisons,
13 still disagree with the route arithmetic hypothesis. Eligibility checks
ordering evidence; it does not assert that every intervening gameplay change
was observed or that the arithmetic hypothesis is universal. These 13 values
and their discrepancies are retained without correction or invented events.
All 13 are OverhealDecay observations of OverhealDamageSection ending at a
serialized LifeResult of zero. The one-step arithmetic leaves positive
residuals from 0.0000019073486328125 to 0.00007577240467071533. This does not
establish a rounding or clamping rule; the original values remain authoritative.

The 714 timeline JSON files occupy 26,885,294,147 bytes.

Twenty-two focused tests pass. Seven changed production guards each cause a
targeted assertion failure, and restored source passes again. The exact
comparison rejects ten altered outputs, including tiny value changes, missing
or extra records, changed epochs, fabricated open metadata and false game-life
claims; normal JSON reserialization passes. This derived view does not
increase typed-row coverage.
