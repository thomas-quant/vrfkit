# Build support validation, 2026-09-24

The latest common all-build audit is [BUILD_VERIFICATION.md](BUILD_VERIFICATION.md).
The results below retain the date and sample scope of this implementation update.

**Status: all sixteen builds from 11.06 through 12.09 have verified payload
transforms.** All 48 available replays pass ReplayData validation and
checkpoint-enabled export. Together with the eight existing versions, the
parser supports 24 exact branches. Unknown branches still fail closed.

## Measured array routes, 2026-09-28

The checksum-gated structured-array routes were measured on 13.01--13.06,
and until this update every other build kept their parents as single raw
rows, with no child and no counter saying why. The gate is now per build
and per route, in [`measured_routes.rs`](../crates/vrfkit/src/sink/measured_routes.rs).
A route is admitted on a build only when, on every available replay of it:

- the replay declares every handle the route's tables are keyed on (the
  parent, the typed members, the nested container and its members) with the
  same name and checksum as the measured 13.x layout;
- every array counter stays zero on the main stream and in checkpoints:
  errors, truncations, implicit terminations, unconsumed root and nested
  bits, and array-leaf decode errors;
- an independent Python re-parse of every parent reproduces every emitted
  child: count, physical adjacency, name, handle, context, raw window and
  typed value, with no refused parent, no refused nested array, no
  zero-width member and no measured member left null;
- the route emits at least one child on that build. A route never observed
  there is unobserved, not verified, and stays off.

| Build | Replays | Admitted routes | Main rows added (typed) | Checkpoint rows added (typed) |
|---|---:|---|---:|---:|
| 11.06 | 3 | KillData, ServerActiveEffects, RequestedIgnoreActors | 31,342 (26,504) | 57,320 (50,918) |
| 11.07 | 3 | as 11.06, plus the projectile path | 26,854 (23,359) | 58,851 (52,325) |
| 11.08 | 3 | as 11.07 | 28,637 (24,731) | 63,384 (56,378) |
| 11.09 | 3 | as 11.07, plus SelectedV2 | 42,633 (33,546) | 327,309 (221,774) |
| 11.10 | 3 | as 11.09 | 34,076 (26,862) | 281,718 (188,373) |
| 11.11 | 3 | as 11.09 | 50,801 (40,960) | 353,780 (240,317) |
| 12.00 | 3 | as 11.09 | 35,850 (28,355) | 326,040 (219,319) |
| 12.01 | 3 | as 11.09 | 37,360 (29,668) | 304,730 (204,409) |
| 12.02 | 3 | as 11.09 | 40,056 (31,735) | 333,768 (225,019) |
| 12.03 | 3 | as 11.09 | 35,325 (28,012) | 308,703 (206,901) |
| 12.04 | 3 | every route but ActiveBlinds | 80,512 (48,281) | 464,924 (306,815) |
| 12.05 | 3 | every route but ActiveBlinds | 66,178 (39,720) | 341,460 (220,047) |
| 12.06 | 3 | every route but ActiveBlinds and the path | 61,880 (38,151) | 280,502 (180,526) |
| 12.07 | 3 | every route but ActiveBlinds and the path | 66,924 (40,451) | 363,603 (235,470) |
| 12.08 | 3 | every route but ActiveBlinds | 73,881 (46,395) | 340,929 (219,927) |
| 12.09 | 3 | every route but ActiveBlinds | 70,625 (42,852) | 347,023 (224,619) |
| 12.10 | 1 | AllPlayersObfuscatedPlayerInformation, TrackedRewards, SelectedV2, ServerActiveEffects | 524 (299) | 3,067 (1,875) |
| 12.11 | 1 | as 12.10 | 510 (294) | 2,627 (1,606) |
| 13.00 | 1 | as 12.10 | 510 (294) | 2,639 (1,614) |
| 13.01--13.06 | -- | every route, unchanged | -- | -- |

"The path" is `MulticastSetPath.NetworkedProjectilePath`. Across the 51
replays the admitted routes add 784,478 main rows (550,469 typed) and
4,562,377 checkpoint rows (3,058,232 typed). Every added row is an additive
child of a parent that is still exported whole; typed counts include nested
`EquippableAttachments` and `AssistingPlayers` references. Rows added per
route, main / checkpoint:

| Route | Rows added | Typed |
|---|---:|---:|
| `AllPlayersObfuscatedPlayerInformation` | 7,587 / 108,402 | 5,778 / 72,268 |
| `TrackedRewards` | 173,562 / 154,630 | 62,224 / 55,225 |
| `SelectedV2` | 161,760 / 3,283,500 | 100,236 / 2,034,584 |
| `KillData` | 78,144 / 783,354 | 75,887 / 760,599 |
| `ServerActiveEffects` | 145,513 / 232,491 | 88,432 / 135,556 |
| `RequestedIgnoreActors` | 195,640 / 0 | 195,640 / 0 |
| `MulticastSetPath.NetworkedProjectilePath` | 22,272 / 0 | 22,272 / 0 |

The 12.10, 12.11 and 13.00 entries rest on one short public fixture each:
their four admitted routes emit a few hundred main rows, and KillData,
RequestedIgnoreActors, ActiveBlinds and the path never occur in them.

### Why routes stay off

- **`AllPlayersObfuscatedPlayerInformation` and `TrackedRewards`, before
  12.04.** Two members (`StartOfRoundMoneyCache`,
  `StartOfRoundLoadoutValueCache`) sit at `OwnerExclusivePlayerInfo`
  handles 19--20, so `TrackedRewards` is at 21 instead of 18 and every later
  member moves by +3, with unchanged names and checksums. The member tables
  are keyed by handle: on the probe, 26,325--32,416 main-stream
  `TrackedRewards` children per build, and none of them typed. That is not
  the measured route.
- **`SelectedV2`, 11.06--11.08.** `DynamicMappings` (checksum 149045687) is
  declared at handle 13 and arrives with zero-width payloads, moving the
  nested `EquippableAttachments` array to handle 14. The nested route never
  matched, and the walker skips zero-width members without a counter:
  20,740, 20,160 and 21,328 of them per build on the probe.
- **`ActiveBlinds`, every legacy build.** Through 12.04 `SourceID` and
  `EffectID` swap handles 4 and 5. From 12.05 the handles match 13.x, but
  some `SourceID` values arrive as the 9-bit hardcoded-name form (flag 1,
  index 0) instead of the 297-bit inline string. The route refuses both and
  counts each refusal in `array_leaf_decode_errors`: 1,846 main and 2
  checkpoint parents on the probe, all ActiveBlinds.
  `validate_ability_array_evidence.py` independently rejects the same 1,846
  main parents. None occurred on 13.x in the 986-replay audit.
- **Unobserved.** The path on 11.06, 12.06 and 12.07, and four routes in each
  fixture, never produced a child.

### Method and results

A probe binary, `259ed10` with the gate open on every branch, exported all
48 legacy samples described under [Replay evidence](#replay-evidence) (three
per build, 11.06--12.09) and the three fixtures with `--checkpoints`. For each route, a private Python checker
written from the RepLayout array grammar re-parsed every parent's raw bits,
derived the expected children with the replay's own main or checkpoint
declarations, and compared them with the exported rows. A mutation run
confirmed that it reports changed values, raw windows, typing and dropped
children. The only nonzero array counter on the probe was the ActiveBlinds
refusal count above. `tracked_rewards_opaque_empty_variants` also moved on
every legacy build: it counts the documented `02 00 00` empty variant, not
an error.

The final binary, `2e7acce`, re-exported the same 51 replays plus the
reference 13.01 replay:

- All six array counters are zero on the main stream and in checkpoints of
  every replay. `tracked_rewards_opaque_empty_variants` is 11, 2, 22, 26, 13
  and 15 on 12.04--12.09 (main), and zero on the other legacy builds and the
  fixtures.
- The independent checker matched every child row of every admitted route
  and found no child for any route the build does not admit.
- `extract_kill_observations.py`, with its build set extended in-process for
  this run only, accepted all 48 legacy exports: 7,334 main and 8,876
  checkpoint parents, 7,347 and 73,518 element updates, and 2,525 and 25,419
  assisting references, none unresolved. It now admits 11.06--12.09, and the
  committed extractor and kill ledger both complete all 48 exports; see
  [KILL_OBSERVATIONS.md](KILL_OBSERVATIONS.md) and
  [KILL_LEDGER.md](KILL_LEDGER.md#legacy-builds-2026-09-28).
- `validate_ability_array_evidence.py --compare-typed` matched all 22,272 path
  children on 464 parents. Its only failures are the ActiveBlinds parents that
  now stay raw.
- Against `259ed10`, ten tables are byte-identical on all 52 replays. In the
  three that differ, removing the route children from `fields` and
  `checkpoint_fields` leaves exactly the baseline rows, in order and in every
  column. `checkpoint_blocks` changes only `field_row_start` and
  `field_row_count`, by exactly the children in each span. The manifest
  differs only in the array element and field tallies, the checkpoint
  field-row count and the opaque-variant counter. The 13.01 replay is
  byte-identical in every table. A separate row-multiset comparison of the
  same pairs agrees: no `fields` or `checkpoint_fields` baseline row is
  missing or changed, the 784,478 and 4,562,377 new rows are exactly the
  route children, and the 1,191,332 changed `checkpoint_blocks` rows are the
  shifted spans. Validation verdicts and oracle lines are identical.
- `verify_build_corpus.py`, the common strict audit, passes all 51 replays
  at `2e7acce` with every array and array-leaf counter at zero. Its
  independent scalar comparison reproduces the per-build counts of the
  2026-09-25 audit exactly.
- `verify_build_corpus.py` and `validate_type_evidence.py --compare-typed` on
  the three public fixtures, the CI audit, pass.

The pinned table is tested for every supported branch, and wiring tests check
that a branch expands exactly its admitted routes for both the flattened
arrays and the projectile-path RPC. Seven deliberate gate mutations each
failed at least one of them. The flattened-array test first covered four of
the seven routes on five branches, and an admission arm reading another
route's bit passed it wherever the two routes agreed on those five:
ServerActiveEffects reading SelectedV2's bit dropped its children on
11.06-11.08, RequestedIgnoreActors reading the projectile path's on 11.06,
12.06 and 12.07, and AllPlayersObfuscatedPlayerInformation reading
ActiveBlinds' on 12.04-13.00, with no counter moving. It now runs a case for
every flattened route on every supported branch and checks each case's
identity against the route map that selects the exact walker; those
mutations, and a mis-mapped identity, fail it.
AllPlayersObfuscatedPlayerInformation and TrackedRewards, and KillData and
RequestedIgnoreActors, are admitted on identical branch sets, so a swap
within either pair changes no output on any build.

To reproduce, export with checkpoints and audit with the repository tools:

```powershell
cargo +1.86.0 build --release -p vrfkit --locked
vrfkit export '<replay-root>/12.09/sample-1.vrf' --out '<exports>/12.09/sample-1' --checkpoints
python tools/verify_build_corpus.py --exe target/release/vrfkit.exe --corpus '<replay-root>/12.09' --work-dir '<new-work-dir>' --output '<report.json>'
python tools/validate_ability_array_evidence.py '<exports>/12.09/sample-1' --compare-typed
```

### Follow-ups

- Key the `OwnerExclusivePlayerInfo` and nested `SelectedV2` member typing by
  declared name and checksum, as TeamEconomy already selects its 12.01--12.05
  handles, before admitting those routes on the shifted builds.
- ActiveBlinds needs both declaration-keyed member widths for the
  11.06--12.04 swap and an explicit rule for the 9-bit hardcoded `SourceID`,
  with the matching change in `validate_ability_array_evidence.py`.
- The exact array walker skips zero-width members without a counter. Only
  the held-back `SelectedV2` layout produced them here.

## 11.06--12.00 recovery and replay results

The implementation following `6cbf2bec0645c5278c099839d5db9708e9a45f21`
adds seven per-build transforms without changing shared parser behavior.
Resolve its commit with
`git log -1 --format=%H -- crates/vrf-transform/src/versions/v11_06.rs`.
The original binaries have two layers of encrypted code. Both were recovered
fully offline from each matching EXE and `stub.dll`, using bounded native
cipher fragments and independently checked arithmetic. The reproducible
[recovery tool](../tools/recover_native_binaries.py) pins all seven input
pairs and recovered outputs in its [catalog](../tools/fixtures/native_recovery.json).
The [recovery research](BUILD_RECOVERY_RESEARCH.md) records the method and scope.

Each recovered reader was decompiled separately and emulated independently of
the Rust implementation. Its 79 native cases match, for **553 additional
cases** and **1,264 native cases** across all sixteen recovered builds. The
88 upstream vectors remain unchanged. Changing an 11.06 rotation count made
the native comparison fail; restoring it passed. All referenced substitution
tables match the shared S-boxes byte for byte. No installed game or driver
was started or changed.

| Build | Replays | ReplayData scored blocks | Checkpoint blocks | Main typed overlay |
|---|---:|---:|---:|---:|
| 11.06 | 3 | 1,983,592 | 73,760 | 82.5% |
| 11.07 | 3 | 1,987,726 | 72,553 | 80.8% |
| 11.08 | 3 | 1,969,145 | 76,440 | 79.4% |
| 11.09 | 3 | 2,158,062 | 78,172 | 82.7% |
| 11.10 | 3 | 1,839,369 | 67,573 | 82.3% |
| 11.11 | 3 | 2,205,209 | 83,268 | 83.9% |
| 12.00 | 3 | 2,031,069 | 77,828 | 85.1% |

All 21 replays report **100%** ReplayData oracle success over **14,174,172**
scored blocks, and **529,594** checkpoint blocks were walked. Required main
and checkpoint counters are present and reconcile. Malformed/framing,
transform, field-stream, lost-RPC, typed-overlay, movement, array-leaf and
struct-blob failures are zero. Checkpoint skipped bits are zero.

The main overlay decodes **18,544,499 of 22,502,298 offered rows**, with
**1,517,794** decoded checkpoint rows. These ratios measure typed coverage,
not full semantic understanding. Unresolved RPC schemas and untyped properties
remain raw, as on the existing supported builds. They are not hidden as
successful typed values. Independent Python decoding matches **299,827**
values across all 42 main/checkpoint field tables, with no width failures,
missing specifications or typed mismatches. A separate direct bit walk
matches **6,311 TeamEconomy child values from 899 parent rows**. The existing
controller and declared-handle compatibility fixes also cover these samples.

The eight previously supported reference replays were revalidated and
exported with checkpoints. All **104 Parquet files remain byte-identical**.
This is the complete available three-sample-per-build collection, not a claim
that every possible replay or field on these branches has been tested.

To reproduce recovery and native comparison, use this directory layout for
each build: `<root>/<build>/ShooterGame/Binaries/Win64/` containing the
archived `VALORANT-Win64-Shipping.exe` and matching `stub.dll`. The recovered
root is separate; the capture tool uses it only for the seven protected builds.

```powershell
python tools/recover_native_binaries.py --binaries '<archive-root>' --output '<recovered-root>'
python tools/capture_native_transforms.py --binaries '<archive-root>' --recovered-binaries '<recovered-root>' --check
cargo +1.86.0 test -p vrf-transform --locked
cargo +1.86.0 build --release -p vrfkit --locked
vrfkit validate '<replay-root>/11.06/sample-1.vrf' --diagnostics
vrfkit export '<replay-root>/11.06/sample-1.vrf' --out '<exports>/11.06/sample-1' --checkpoints
python tools/validate_type_evidence.py '<exports>' tools/fixtures/public_fixture_type_evidence.json --compare-typed
```

Repeat validate/export for samples 1--3 of all seven builds. Required-counter,
nonzero-work and reconciliation checks use `check_decode_errors_corpus.py`.
Recovery needs optional `pefile`, `unicorn` and `numpy`; ordinary Rust tests
need no game binaries or emulator.

## 12.01--12.09 recovery and replay results

The implementation following base commit `0d7a798` adds nine transforms and
two compatibility fixes. The containing implementation commit can be resolved
with `git log -1 --format=%H -- crates/vrf-transform/src/versions/v12_01.rs`.

Each executable was imported with Ghidra 12.1.2, using PE runtime-function
records and chained unwind entries to identify complete function boundaries.
The PRNG multiplier, reader object layout and caller argument shape located
the candidate readers. The replay reader was checked independently by
emulating its original x86-64 instructions with Unicorn, including its native
bit-copy helper. No game process was launched. The catalog records the exact
[executable SHA-256 and reader RVA](../tools/fixtures/native_transform_readers.json).
All three 256-byte S-box tables referenced by the applicable readers match
the existing tables byte for byte.

There are 79 native cases per build: the eleven upstream staging boundaries,
36 cases covering six edge seeds at six lengths, and 32 deterministic random
cases up to 512 bits. All **711 expected-byte cases** match Rust, in addition
to the unchanged 88 upstream golden vectors. Reversing one 12.09 rotation's
count makes the native test fail; restoring it passes. The capture tool
verifies executable hashes before emulation and rejects failure to return,
instruction/time exhaustion, or an incorrect reader position.

Two real-corpus failures required fixes beyond the arithmetic:

- 12.01--12.06 call the replay controller `BaseJanusController`. Recognizing
  that exact class consumes its net-player-index byte before framing. Without
  this, the first bunch and checkpoint controller bunches lose data. The new
  pipeline regression fails before the name fix and passes afterward.
- 12.01--12.05 declare `TeamEconomy` members at handles 53--55, rather than
  56--58. Their names and checksums match the later members. The exporter now
  selects by the replay's declaration, including hardcoded FName `241` for
  the IntPacked replication ID. Unknown declarations still fail. The original
  fixed-handle API remains available for compatibility.

| Build | Replays | ReplayData scored blocks | Checkpoint blocks | Main typed overlay |
|---|---:|---:|---:|---:|
| 12.01 | 3 | 1,905,287 | 72,795 | 84.6% |
| 12.02 | 3 | 2,027,973 | 77,588 | 84.2% |
| 12.03 | 3 | 1,944,438 | 74,386 | 84.2% |
| 12.04 | 3 | 2,425,410 | 86,588 | 84.7% |
| 12.05 | 3 | 1,886,622 | 69,916 | 85.0% |
| 12.06 | 3 | 1,642,411 | 58,280 | 84.9% |
| 12.07 | 3 | 1,934,113 | 77,802 | 84.8% |
| 12.08 | 3 | 1,977,496 | 69,130 | 81.2% |
| 12.09 | 3 | 2,170,389 | 74,575 | 81.4% |

Every replay reports **100%** ReplayData oracle success: 17,914,139 scored
blocks in total. All 661,060 checkpoint blocks were walked. Main and checkpoint
passes report zero malformed/framing, transform, field-stream, lost-RPC,
partial-reassembly, typed-overlay and struct-blob failures. Checkpoint
`skipped_bits` is 906 in 12.05 sample-2: six unresolved RPC blocks are retained
whole as raw payloads, rather than lost. All other checkpoint skipped-bit
counters are zero.

The main overlay decoded 23,902,871 offered rows, and the checkpoint overlay
decoded 1,919,133. The percentages above are the main overlay's decoded/offered
ratio, not semantic completeness. Unknown properties, unresolved RPC schema,
and unnamed fields remain explicitly raw, as on previously supported builds.
Independent Python decoding of the existing public-fixture evidence types
matches **410,350 values**, with no missing specification, width failure or
typed mismatch, across all 54 main/checkpoint field tables. A separate direct
bit walk of TeamEconomy matches **7,797 child values from 1,110 parent rows**,
including the 12.01--12.05 nonzero loadout values. Captured regression fixtures pin
the first two 12.01 updates and reject unknown/missing declarations.

Eight previously supported builds were revalidated and re-exported, one replay
each with checkpoints. Their **104 Parquet files are byte-identical** to the
pre-change outputs. This is the full available 27-file sample for 12.01--12.09, not a
claim about every replay ever recorded on these builds.

The committed 13.01 export baseline was stale from the earlier scoped smoke
field additions. Both the pre-change `18a0c60` binary and this implementation
produce the same four differences: 116 more decoded rows, 116 fewer
not-in-table rows, `fields.parquet` growing by 133 bytes to 16,455,178, and its
SHA-256 changing to
`b186a9b50b4f73f1c3a7d8482431732af5413420ba2ca9992c314bc61cb41b93`.
The 116 values are 20 `CreatedByCharacter` GUIDs and 96 `bInPersistentData`
booleans on the previously accepted Smonk/Wushu scopes. Independent Python
IntPacked/Bool decoding matches all of them. No rows or other main tables change.
The baseline and its quoted documentation figures are refreshed for those
already-present values; the 12.01--12.09 support changes introduce no 13.01 drift.
The paired checkpoint baseline additionally gains 48 GUID and 48 Bool values
from those same scopes, all independently matched to raw bits. Its field file
shrinks by 320 bytes to 1,218,992, with SHA-256
`21ce023b9a7c3b67c0d892cc519f50b097a7e97c88d6da93d98ad78cae00806e`.
The archived pre-upstream binary reproduces both original baseline hashes;
column comparison confirms only these previously-null typed cells change.
All rows, raw bytes, names, identities and other columns are identical.

To reproduce, use the acquired binaries and the three pinned source samples
for each build. Native capture needs optional `pefile` and `unicorn` Python
packages; ordinary Rust tests use the committed vectors and need neither.

```powershell
python tools/capture_native_transforms.py --binaries '<binary-root>' --recovered-binaries '<recovered-root>' --check
cargo +1.86.0 test -p vrf-transform --locked
cargo +1.86.0 build --release -p vrfkit --locked
vrfkit validate '<replay-root>/12.01/sample-1.vrf' --diagnostics
vrfkit export '<replay-root>/12.01/sample-1.vrf' --out '<exports>/12.01/sample-1' --checkpoints
python tools/validate_type_evidence.py '<exports>' tools/fixtures/public_fixture_type_evidence.json --compare-typed
```

Repeat validate/export for samples 1--3 of all nine builds. The same exports
were checked with the required-counter and reconciliation routines from
`check_decode_errors_corpus.py`, alongside the framing/loss counters.

## Replay evidence

The 48 samples are from
[`Matthias1590/unsupported-replays`](https://github.com/Matthias1590/unsupported-replays/tree/c5ba8419b96afa8e88e7afb3e594cf4cce9c75fe):
three per build, 11.06--11.11 and 12.00--12.09. The downloaded `.replay` files
were renamed `.vrf` without changing their bytes. The repository's SHA-256
manifest identifies the source payloads.

Before the fix, `inspect` failed on all 36 samples through 12.05. The first
four builds ran past the end of the header, while larger headers from 11.10
onward reached an invalid level-name count. All twelve samples from
12.06--12.09 already passed inspection but were rejected for missing payload
transforms.

The legacy header places UE4Version directly after the replay branch string:
the next three little-endian u32 values are 522, 1009 and 77. The current
reader instead interpreted 522 as the length of a VALORANT extension. The
extension's length and bytes first appear in the 12.06 samples. Reading it
only outside the twelve measured legacy branches restores the correct
package-version, level-name, game-specific-data and recording-parameter
boundaries. Malformed modern headers are not retried with the legacy layout.

After the fix, all 48 samples pass inspection and the container corpus smoke
test. That smoke test also decompresses the first ReplayData chunk and checks
known Event layouts; it does not decode replication payloads. Synthetic
regressions cover every legacy branch, the 12.06 boundary, zero/nonzero
extensions and rejection of missing modern extensions. The legacy regression
was run against the original reader and failed on the same misplaced length.

## Earlier identity-transform probe

A temporary identity-transform probe was run on `sample-1` from every build.
All sixteen validations failed, with framing oracle rates of only
0.711762%--3.804203%. The experimental registrations were removed. Neither
plaintext passthrough nor merely accepting the branch is a decoding fix.

The upstream parser at
[`2b66c65`](https://github.com/michel-giehl/ValorantReplayParser/tree/2b66c65a7b116154e18ebb84d9f6795f2b080233/src/Replay.Encoding/PayloadEncryption/VersionedTransforms)
has transforms only for the eight previously supported builds.
The additional nine transforms above were recovered from the acquired
game executables listed below. The upstream maintainer describes
locating the transformed reader through `UActorChannel::ReadContentBlockHeader`
in [issue #2](https://github.com/michel-giehl/ValorantReplayParser/issues/2).

Each new transform must come with independent expected-byte vectors, followed
by validation and checkpoint-enabled export of all three available samples
for its build. Frame success alone is not evidence that typed values agree
with the wire.

## Binary analysis feasibility check

The analysis path was exercised on the installed 13.06 executable, without
launching the game or modifying its files. Its SHA-256 is
`8f033b34913a2e16fb6630fe67ac758baf3612e08315b8b74241ed7f5eb537a2`.
Ghidra 12.1.2 headless import and targeted decompilation located a seeded bit
reader at RVA `0x04534320` (VA `0x144534320` at image base `0x140000000`).
Its seed addend `0xe974593c`, initialization offset `0x3c`, PRNG multiplier
`0x2545f4914f6cdd1d` and transform operations match the 13.06 implementation.

For an independent check, Unicorn emulated that original x86-64 function
and its native bit-copy helper directly from the PE image. The Windows x64
arguments were a reader pointer, output pointer and bit count. In this
executable the reader's source pointer, total bits, current bit position and
seed are at offsets `0x98`, `0xa8`, `0xb0` and `0xb8`. Each case reset the
reader and input bytes, used bit position zero, and set the seed to
`bit_count ^ actor_net_guid`. Emulation required return to the caller within
the instruction/time limit before comparing output bytes.

All eleven 13.06 cases from
[`golden_vectors.rs`](../crates/vrf-transform/tests/data/golden_vectors.rs)
matched: 0, 1, 7, 8, 31, 32, 63, 64, 65, 287 and 288 bits. This verifies a
practical way to obtain an independent native-code oracle; this initial feasibility check did not itself add
build support. Addresses, reader layouts and algorithms must
be recovered and checked for each executable, not assumed to carry over.

The manifest-link archive
[`Morilli/riot-manifests` at `573d6e7`](https://github.com/Morilli/riot-manifests/tree/573d6e78edc51395a03513800230eab3dbadbf92/VALORANT/na)
contains 29 patch entries covering all sixteen missing builds. These are
links to Riot manifests, not archived game executables. Acquisition was
initially blocked in the measured environment. A browser check of the CDN
hostname's HTTP root displayed an SK Broadband school-network notice stating
that firewall policy blocks the page. DNS resolves several Riot hostnames to
that warning server, whose expired, mismatched certificate caused the HTTPS
failures. The earlier TLS error is therefore evidence of the local network
block, not evidence that Riot's own CDN certificate is invalid or that the
archived files have disappeared.

After the user requested a retry, DNS resolved to Riot's CloudFront endpoint
and certificate-verified HTTPS downloads succeeded. All sixteen executables
below were acquired from the latest recorded patch for each replay branch.
Only `ShooterGame/Binaries/Win64/VALORANT-Win64-Shipping.exe` was selected;
the installed game was not replaced or launched. Every downloaded file:

- Passed `ManifestDownloader --verify-only` against its RMAN chunk hashes.
- Contained its expected `++Ares-Core+release-<build>` branch label.
- Had a valid Windows Authenticode signature from `Riot Games, Inc.`.
- Was recorded with its SHA-256, manifest ID/hash, source URL and archive
  commit in the local acquisition catalog.

| Replay build | Acquired patch | Executable bytes |
|---|---|---:|
| 11.06 | 11.06.00.3836880 | 201,181,352 |
| 11.07 | 11.07.00.3855133 | 201,408,608 |
| 11.08 | 11.08.00.3918089 | 202,602,752 |
| 11.09 | 11.09.00.3920876 | 203,230,824 |
| 11.10 | 11.10.00.4002057 | 202,949,848 |
| 11.11 | 11.11.00.4091853 | 208,825,552 |
| 12.00 | 12.00.00.4183428 | 208,267,728 |
| 12.01 | 12.01.00.4211771 | 208,325,576 |
| 12.02 | 12.02.00.4226954 | 210,556,856 |
| 12.03 | 12.03.00.4322591 | 210,990,720 |
| 12.04 | 12.04.00.4354757 | 212,279,904 |
| 12.05 | 12.05.00.4440267 | 213,755,024 |
| 12.06 | 12.06.00.4440219 | 214,659,936 |
| 12.07 | 12.07.00.4488404 | 214,767,368 |
| 12.08 | 12.08.00.4578383 | 214,750,840 |
| 12.09 | 12.09.00.4704114 | 184,074,872 |

The selected manifest file sizes sum to **3,312,627,760 bytes** (3.313 GB,
3.085 GiB). The sixteen RMAN files add 148,679,917 bytes. Summing the compressed
chunks referenced by those executables gives 1,688,334,507 bytes, or
**1,837,014,424 bytes** including manifests for the nominal download payload;
HTTP/TLS overhead and retries are not measured by that figure. Generated
metadata and future Ghidra databases need additional space.

All sixteen executables and the seven matching protected-build stubs were
acquired and verified. The 11.06--12.00 executables have encrypted `.text`, a
NOP entry point and an imported `stub.dll!packman`; their stubs themselves
require section decompression. This initially blocked native reader capture.
The offline recovery above resolves that dependency and pins each recovered
image by SHA-256. The installation and Vanguard configuration are unchanged.

The first-hand [Packman analysis](https://hypercall.net/posts/Packman/) helped
identify the runtime loader architecture. Public in-process unpackers were
examined as references but were not used to run or inject into the game. The
committed tool reconstructs the code directly from the archived local bytes.

## Regression scope and commands

Eight previously supported builds were checked before and after this change:
12.10, 12.11, 13.00, 13.01, 13.02, 13.04, 13.05 and 13.06, one replay each.
All validations and checkpoint-enabled exports passed. All 104 Parquet files
(13 per replay) were byte-identical between the two binaries.

The baseline binary was built from `18a0c607ff85cbd7cc785210dbc9add1d6e6e166`.
To repeat the legacy container checks, set `VRFKIT_CORPUS_DIR` to one build's
directory and require that it exists:

```powershell
$env:VRFKIT_CORPUS_DIR = '<replay-root>/11.06'
$env:VRFKIT_REQUIRE_CORPUS = '1'
cargo +1.86.0 test -p vrf-container --test corpus parse_all_vrf_files --locked -- --exact --nocapture
vrfkit inspect '<replay-root>/11.06/sample-1.vrf' --redact-identifiers
vrfkit validate '<replay-root>/11.06/sample-1.vrf' --diagnostics
```

Repeat for every affected directory. For the supported-build comparison, run
both binaries on the same replay with `validate` and
`export <replay> --out <separate-directory> --checkpoints`, then compare the
SHA-256 of each output Parquet file. Replay bytes and local export bundles are
not committed.
