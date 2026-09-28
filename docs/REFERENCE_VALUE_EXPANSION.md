# Reference values and HawkFlash velocity

This expansion types existing raw rows for RequestedIgnoreActors child references,
TransitionContext object references, and the measured HawkFlash PostControlVelocity
vector. Every original raw window remains available. A numeric reference does not
imply that an actor or object identity can be resolved.

## Extracted values

| Field | Main rows newly typed | Checkpoint rows newly typed | Representation |
|---|---:|---:|---|
| RequestedIgnoreActors child | 2,266,671 | 0 | packed NetGUID in value_i64 |
| TransitionContext | 3,698,138 | 460,581 | packed NetGUID in value_i64 |
| HawkFlash PostControlVelocity | 521,801 | 0 | three f64 components in value_str |
| Total | 6,486,610 | 460,581 | 6,947,191 existing rows |

RequestedIgnoreActors already had an exact array framing route. Its new child
admission requires the measured build family, FiniteSpeedMovementComponent parent
identity, child handle 5, declared name RequestedIgnoreActors and CRC 3344674359.
An absent overlay or an agreeing ObjectNetGuid type is accepted; explicit Raw,
Skip or conflicting types remain raw. Truncated packed child data preserves both
child and parent raw rows and increments the array-leaf decode-error counter.

TransitionContext is corrected from Raw to ObjectNetGuid for the exact
EquippableStateMachineComponent group. The C# reference declares a raw payload
labelled UTransitionContext; its raw reader does not establish the type. The
corpus supplies exact packed-U32 consumption and actual transition-context object
identities. All 460,581 checkpoint observations encode zero.

PostControlVelocity is added only for the exact HawkFlash projectile group. All
521,801 observed windows contain three finite little-endian f64 values, and all
vector lengths are within 1e-6 of 1800. A competing six-f32 interpretation has
6,024 non-finite components. Output follows the existing vector convention,
`(x,y,z)` in value_str, retaining full raw bits. This establishes a vector layout,
without claiming gameplay units or interpreting state transitions.

The scalar overlay architecture resolves by group/name. Unlike the measured
array gate, these two scalar entries do not enforce runtime build/handle/CRC
restrictions. Their measured scope is builds 13.01, 13.02, 13.04 and 13.05:
TransitionContext handle 3 / CRC 1215597030; PostControlVelocity handle 17 /
CRC 3686770549. Future formats require fresh validation.

## Reference resolution limits

RequestedIgnoreActors has 1,924,155 same-scope actor matches, 335,957 unresolved
nonzero IDs and 6,559 zero IDs. Main TransitionContext has 1,225,712 matches to
serialized transition-context objects, 689,283 unresolved nonzero IDs and
1,783,143 zero IDs. These are observations, not unique actors or transitions.
Zero remains numeric zero in the raw typed field; consumers distinguish a null
reference from an absent value. Unresolved IDs are retained without assigning a
lifecycle meaning.

## Coverage and validation

A direct scan of all four typed columns in all 714 exports measured (historical:
[TARGETING_AND_HEAL_VALUES.md](TARGETING_AND_HEAL_VALUES.md) has the newer scan):

| Scope | Physical rows | Rows with typed values | Presence |
|---|---:|---:|---:|
| Main | 1,020,564,717 | 725,157,533 | 71.0545% |
| Checkpoints | 285,420,158 | 220,640,840 | 77.3039% |
| Combined | 1,305,984,875 | 945,798,373 | 72.4203% |

Parent and child rows can describe the same input; this is physical presence,
not semantic coverage.

The full-corpus independent comparison passed all 714 files, as did both corpus
guards and both pinned reference exports. The frozen full-corpus executable
SHA-256 is
`569b6cff14e873a75956b7bbfba918d6a340279e7ed8f540c059d769deeddbb8`.
The comparison requires existing coordinates, raw windows, rows and all other
values to remain exact; the eleven other Parquet tables must be byte-identical.
Manifest changes must equal the measured scalar populations: TransitionContext
moves Raw/Skip to Decoded OK, and PostControlVelocity moves Not in table to
Decoded OK. All other manifest values remain exact except elapsed run time.

AuthInitialRandomSeed signedness and CursorWorldLocation interpretation remain
unsettled and were not changed. No checksum donor entries were invented.
