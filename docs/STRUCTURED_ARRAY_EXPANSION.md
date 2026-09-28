# Structured array expansion

This report records the structural expansion before the later value typing.
Its 68.5562% combined ratio is historical. Subsequent leaf typing is recorded in
[`ARRAY_LEAF_TYPES.md`](ARRAY_LEAF_TYPES.md); the latest accepted corpus counts
and available observations are in [`CURRENT_STATUS.md`](CURRENT_STATUS.md).

This document records the candidate structured-array expansion evaluated on the September 2026, 714-replay corpus (builds 13.01, 13.02, 13.04, and 13.05). It is a data-preservation and framing result, not a semantic event model.

The routes now run on 13.01--13.06 and on a measured subset of routes for each
legacy build (11.06--13.00); the gate is per build and per route. The
2026-09-28 legacy measurement, and why each held-back route stays raw, is in
[`LEGACY_BUILD_SUPPORT.md`](LEGACY_BUILD_SUPPORT.md#measured-array-routes-2026-09-28).

The candidate export (binary SHA-256 `d55dc77...`) completed on all 714 inputs, and `validate_corpus.py` and `check_decode_errors_corpus.py` passed on all of them. The independent all-file comparison passed 714/714: all prior field values were preserved, new child windows and typed values matched independent decoding, ten unaffected tables stayed byte-identical, and checkpoint block spans and diagnostic/export counters passed.

## Exact qualified routes

Three parent identities are eligible only under their observed group, name, and checksum. Parents retain raw bits. Emitted children are additive physical windows, not independent game events.

| Parent | All-corpus child windows | Typed child windows |
|---|---:|---:|
| `TrackedRewards` | 12,259,830 | 3,510,015 (four reviewed fields only) |
| `SelectedV2` | 51,571,724 | 0 |
| `KillData` | 11,443,889 | 0 |
| Total | 75,275,443 | 3,510,015 |

The four typed reward fields are the only ones the reward-leaf evidence supported.

| Reward member | Output value | Observed values and limits |
|---|---|---|
| `RewardName` | `value_str` (FName) | 14 strings, including `Kill`, `Death`, `WinRound`, and `LoseRound` |
| `InstancesOfReward` | `value_i64` (Int32) | 0 through 6 |
| `RewardGrantStrategy` | `value_i64` (enum code) | 0 and 1; action names are unverified |
| `Source` | `value_i64` (enum code) | 0 through 3; source names are unverified |

At this initial batch, every other reward child remained raw, including the anonymous repeated `Rewards` members and `LocalizedRewardName` (FText). At that stage `SelectedV2` and `KillData` children were raw, including nested bodies such as `EquippableAttachments` and `AssistingPlayers`; declaration labels do not establish types, nested schemas, or game meaning. These are replicated updates, not a deduplicated reward or kill ledger. Checkpoint snapshots and parent/child windows must not be summed as independent events.

## Framing boundary

`SelectedV2` and `KillData` use the documented RepLayout dynamic struct-array grammar with exact consumption. The independent full probe found 291,722 and 223,215 parents respectively, with zero residuals or parse errors, and enforced Rust's 128-fields-per-element limit.

For `TrackedRewards`, 4,470 main-stream parents are the exact opaque empty shape `020000`: packed capacity one, index zero terminating the array, then one opaque zero byte. No checkpoint parent has this shape. It is preserved and counted as an opaque empty variant. It is not a generic trailer rule and must not make arbitrary nonempty trailing bytes acceptable.

## Coverage measurement

The completed candidate measurement reports physical typed-value presence: 715,339,906 / 1,020,129,371 main rows (70.1225%), 174,149,661 / 277,331,271 checkpoint rows (62.7948%), and 889,489,567 / 1,297,460,642 combined rows (68.5562%). The candidate adds 1,851,595 typed main windows and 1,658,420 typed checkpoint windows. These are physical row ratios after child expansion, not semantic completeness, parser correctness, or unique game facts. The completed all-file comparison independently reproduced the added child and typed-value totals.

The prior arrays/context report is historical ([`ARRAY_CONTEXT_EXPANSION.md`](ARRAY_CONTEXT_EXPANSION.md)); its coverage and its statement that `TrackedRewards` was excluded predate this candidate. The checkpoint GUID-path result remains separately documented in [`CHECKPOINT_PATH_RESOLUTION.md`](CHECKPOINT_PATH_RESOLUTION.md); its 72.4914% combined coverage is pre-array-expansion and is not the current ratio.

Subsequent [leaf typing](ARRAY_LEAF_TYPES.md), [nested references](NESTED_ARRAY_REFERENCES.md), and [reward text histories](TEXT_HISTORY_EXPANSION.md) supersede the initial raw-only status above.
