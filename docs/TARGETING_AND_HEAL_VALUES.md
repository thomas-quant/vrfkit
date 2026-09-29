# Targeting coordinates and heal references

This batch investigates three additional value routes and one nested targeting
array in the 714-export corpus from builds 13.01, 13.02, 13.04 and 13.05.
The routes are implemented and all 714 replay exports and independent
before/after comparisons pass. Counts below describe the measured populations.

| Exact route | Main rows | Checkpoint rows | Measured representation |
|---|---:|---:|---|
| `MapTargetingStateComponent.CursorWorldLocation` | 291,883 | 7,547 | Three little-endian f64 components |
| `MapTargetingStateComponent:MulticastRespondToValidSingleMapClick.ClickedLocation` | 13,715 | 0 | Three little-endian f64 components |
| `MapTargetingStateComponent:MulticastRespondToValidMapClick.WorldLocation` | 9,899 parents | 0 | Array containing 12,507 vector leaves |
| `DamageableComponent:MulticastNotifyHeal.HealCauser` | 1,518,207 | 0 | Packed u32 actor reference |

The script groups above are under `/Script/ShooterGame.`. RPC parameters are
emitted under the corresponding `_ClassNetCache` group with function-qualified
field names. Parent and nested member populations describe overlapping raw
data; they must not be added as independent gameplay events.

## Export representation

Cursor and click vectors populate `value_str` as `(x,y,z)`, using the existing
double-vector representation. The original 192-bit windows remain in
`raw_bits`. `HealCauser` populates `value_i64` with the packed NetGUID; the
reference has no implied player attribution.

Multi-click children are named
`MulticastRespondToValidMapClick.WorldLocation[index].WorldLocation`. They
appear immediately before the retained raw parent, carry its actor/object,
clock and packet coordinates, and use the enclosing RPC handle (3 in this
corpus). The declaration's member handle 1 describes the inner wire schema;
it is not the exported row's handle. Child checksum is null. Parent and child
payloads overlap and must not be counted as separate clicks.

Scalar types require exact group, field name and checksum through the generated
scoped table. The nested route additionally checks parameter handle 0 and the
member declaration's handle, name and checksum. It requires complete framing,
one unique 192-bit finite vector per emitted element, and validates all children
before emitting any. Valid empty arrays emit no children. Unexpected shapes
retain the raw parent and increment the existing array diagnostics or
`array_leaf_decode_errors`.

`targeting_world_locations_decoded` reports additive children in both main and
checkpoint manifest sink quality. The CLI prints `Target locations` and
`Checkpoint targets`, including zero. Baseline validation reconciles both
printed counts with the manifest.

## Coordinate evidence

Every named cursor and single-click payload consumes exactly 192 bits as three
finite f64 values. The competing six-f32 interpretation produces non-finite
components: 2,255 in main cursors, 62 in checkpoint cursors and 134 in clicks.
All cursor Z components are exactly representable as f32, while their X/Y
components retain additional precision.

There are 7,201 exact X/Y bit matches between a single-click payload and the
latest preceding cursor update on the same actor/component. Separately, 646
click triples match the independently decoded spawn coordinates of the two
measured Cashew map-missile marker classes, within 0.06 per axis and 5 seconds.
Those matches occur in all four builds: 189, 153, 216 and 88 respectively.
Swapping X and Y fails all 280 retained geometry-control examples. These are
coordinate/type cross-checks, not a rule for attributing ability casts.

Cursor values include very large X/Y excursions. There are 29,521 main and
141 checkpoint components with magnitude at least 100,000. Such values must
remain visible; a finite decoded cursor does not establish a valid in-map
target. The data does not justify replacing them with zero or clamping them
to map bounds. Cursor Z and clicked Z can differ, so the two triples are
retained separately.

The multi-click payload is an exact packed array: capacity, one-based element
index, one-based member handle, declared bit width, 192-bit value, member
terminator and array terminator. All 12,507 observed leaves use member handle 1.
All 249 exports containing these arrays declare that member as `WorldLocation`
with checksum 3965480401. The parent checksum is 2052180909. Truncation, extra
tail bits, out-of-bounds indices and unexpected member handles are rejected by
the independent raw reader.

## Heal reference evidence

Every `HealCauser` leaf consumes its whole packed reference window. The corpus
contains 874,747 16-bit windows and 643,460 24-bit windows, totaling 29,438,992
bits. All 1,518,207 references resolve to same-export actors, covering ten
ability/game-object classes. Zero, unresolved and NetGUID-only references were
not observed. This establishes the reference representation; it does not
establish player heal credit or replace the separately serialized heal amount.

The qualified parameter declaration is handle 9, checksum 546618027, under
`DamageableComponent:MulticastNotifyHeal`. Its enclosing function is handle 6,
checksum 791426194. The four exports without the parameter declaration also
contain no matching value rows. No checkpoint value rows were observed;
checkpoint declaration snapshots were checked separately.

## Later addition: instigator and decay references (2026-09-28)

This section is a separate, later measurement on a different corpus: all
1,018 exports of the 2026-09-28 audit at `259ed10`, builds 11.06 to 13.06. It
does not change the 714-export figures above. Five more heal and decay
parameters are now typed `ObjectNetGuid` through the same scoped table. As
with `HealCauser`, the exported rows use the `_ClassNetCache` group and
function-qualified names.

| Exact route (`DamageableComponent:` ...) | Checksum | Main rows | Builds | Resolves to |
|---|---:|---:|---:|---|
| `MulticastNotifyHeal.EventInstigator` | 3087885251 | 2,115,008 | 21 | nothing in the export (a PlayerController) |
| `MulticastNotifyHeal.EventInstigatorPawn` | 3901949544 | 2,115,008 | 21 | an open character actor |
| `MulticastNotifyOverhealDecay.EventInstigator` | 3087885251 | 536,700 | 16 | nothing in the export (a PlayerController) |
| `MulticastNotifyOverhealDecay.EventInstigatorPawn` | 3901949544 | 536,700 | 16 | the decaying character itself |
| `MulticastNotifyOverhealDecay.DecayCauser` | 3648603088 | 536,700 | 16 | an open ability actor, or 0 |

None of these has checkpoint rows. Two independent readers consumed every
payload exactly. The scope is exact because the damage RPCs carry parameters
with the same names under other checksums.

`EventInstigator` never resolves to an actor or a `net_guids` path. On every
row it equals the replicated `Controller` and `Owner` of the
`EventInstigatorPawn` actor, so it is the instigating player's controller,
whose channel the replay never opens. An unresolved join is expected and is
not a decode fault. `DecayCauser` has 39 null references, serialized as one
zero byte, and they export as `value_i64 = 0`: Unreal's null NetGUID, meaning
"no causer", not an actor. As with `HealCauser`, none of this establishes heal
or decay credit.

Per-field evidence and observed builds are in
[`scoped_type_evidence.json`](../tools/fixtures/scoped_type_evidence.json).
A fresh export of 30 replays, at least one from every build that carries
these rows, confirmed the typing: 46,125 + 46,125 heal rows and 3 x 12,813
decay rows, each typed and equal to an independent decode, with zero decode
errors.

## Initial random seed remains unresolved

The same investigation measured `AuthInitialRandomSeed`: 1,752,939 main rows
and 567,321 checkpoint rows, all exactly 32 bits. None sets the high bit, so
signed and unsigned interpretations give the same numbers. No explicit
property type declaration found on the 2026-09-09 recheck settles it,
`InputEventData`'s actions or `StopEffectType`. Its signedness remains
unestablished.

Every main Initial row has an exactly matching Current seed at the same actor,
object, time and packet. Checkpoints contain 434,312 equal and 133,009 differing
pairs. Of 451,364 main actor/object groups, 199,225 carry more than one Initial
value. This is not an immutable value per serialized actor/object; distinguishing
resets from lifecycle reuse needs additional evidence. No new Initial seed type
is introduced by this batch.

## Validation status

The fixed Rust 1.86 binary completed all 714 exports, including checkpoints:
215 for 13.01, 204 for 13.02, 108 for 13.04 and 187 for 13.05. Both full-corpus
guards pass: zero overlay decode
errors, zero structured-blob failures and zero malformed packets. This does
not establish that all input bytes have been interpreted.

The five-file independent pilot comparison passes across all four builds.
It checks bit-exact raw-derived values, every preserved field column, new
child windows and coordinates, eleven unchanged tables, and exact manifest
counter changes. The same checks pass on all 714 files. Every original main
and checkpoint row is preserved except the three explicitly qualified value
promotions; the only additional rows are the 12,507 raw-preserving vector
children. Eleven other tables remain byte-identical.

The comparator accepts both the original pilot and an unchanged Parquet
rewrite, and rejects seven mutated exports. Separate controls prove signed
zero is distinguished in vector parsing and float-column comparison. The
baseline counter test also rejects an unconditional-success replacement of
its production manifest checker.

A direct scan of all four value columns in the new exports measured:

| Population | Physical rows | Rows with a typed value | Presence |
|---|---:|---:|---:|
| Main | 1,020,577,224 | 726,993,845 | 71.2336% |
| Checkpoint | 285,420,158 | 220,648,387 | 77.3065% |
| Combined | 1,305,997,382 | 947,642,232 | 72.5608% |

This is an increase of 1,843,859 populated rows: 1,831,352 existing rows gain
values, and 12,507 typed children are added. Physical parents and children
overlap, and checkpoints repeat state. These ratios are not semantic
completeness; the earlier strict semantic classification has not been rerun.

Pinned main and checkpoint baselines pass after review: the reference file
gains 1,894 HealCauser values, retains all row counts, and reports zero new
targeting children. Its directly measured typed presence is 914,001 of
1,296,660 rows (70.4889%).
