# vrfkit

A Rust toolkit that parses VALORANT replay files (`.vrf`, Unreal Engine network
replay format) and exports them to Parquet. A workspace of 10 crates plus a
Python `tools/` validation suite. `#![forbid(unsafe_code)]` is in every crate;
there is no `unsafe` block anywhere in the workspace. The only native FFI the
parser depends on is Oodle decompression, and that lives entirely in the
external `oozextract` crate. Edition 2024, MSRV 1.86, MIT.

![CI](https://github.com/yakisoba0728/vrfkit/actions/workflows/ci.yml/badge.svg)
![license](https://img.shields.io/badge/license-MIT-blue.svg)
![rust](https://img.shields.io/badge/rust-1.86%2B-orange.svg)
![edition](https://img.shields.io/badge/edition-2024-orange.svg)
![builds](https://img.shields.io/badge/builds-11.06--13.06-green.svg)
![unsafe](https://img.shields.io/badge/unsafe-none-success.svg)

Derived from [ValorantReplayParser](https://github.com/michel-giehl/ValorantReplayParser)
by Michel Giehl; see [`NOTICE.md`](NOTICE.md). Not affiliated with, endorsed
by, or approved by Riot Games.

**Verified state (2026-09-28):** Rust has **806 passing** tests; Python has
**1321 passing** tests. All 24 supported builds received the same verification
on **1,018 unique replays**; all **1,018** meet every strict criterion. See
[build verification](docs/BUILD_VERIFICATION.md) for the measured scope, common
checks and remaining limits.

- Run it: [`docs/USAGE.md`](docs/USAGE.md)
- What's extractable: [`docs/DATA.md`](docs/DATA.md)
- Current corpus status and remaining work: [`docs/CURRENT_STATUS.md`](docs/CURRENT_STATUS.md)
- Latest build verification: [`docs/BUILD_VERIFICATION.md`](docs/BUILD_VERIFICATION.md)
- Historical field inventory: [`docs/TARGETING_AND_HEAL_VALUES.md`](docs/TARGETING_AND_HEAL_VALUES.md)
- Upstream parity and 13.06 validation: [`docs/UPSTREAM_PARITY.md`](docs/UPSTREAM_PARITY.md)
- Upstream Warden and Raze review: [`docs/UPSTREAM_RAZE_WARDEN.md`](docs/UPSTREAM_RAZE_WARDEN.md)
- Character-death and KillData state: [`docs/KILL_LEDGER.md`](docs/KILL_LEDGER.md)
- Damage, healing, decay and reset observations: [`docs/SECTION_OBSERVATIONS.md`](docs/SECTION_OBSERVATIONS.md)
- Observed section timelines and explicit continuity gaps: [`docs/SECTION_TIMELINE.md`](docs/SECTION_TIMELINE.md)
- Packet-ordered section comparisons: [`docs/SECTION_PACKET_TIMELINE.md`](docs/SECTION_PACKET_TIMELINE.md)
- Numeric FastArray observations and remaining item semantics: [`docs/GAS_AND_PATCHVOLUME_INVESTIGATION.md`](docs/GAS_AND_PATCHVOLUME_INVESTIGATION.md)
- Ground-area volume cells (molotov, slow, net and wire patches): [`docs/GROUND_VOLUMES.md`](docs/GROUND_VOLUMES.md)
- Build it, test it, open a PR: [`CONTRIBUTING.md`](CONTRIBUTING.md)
- Working conventions (for an AI agent): [`CLAUDE.md`](CLAUDE.md)

## Why this exists

Most replay parsers export only the fields whose type they know. vrfkit's
design premise is the opposite: **export every value the replay carries.** This
is possible because Unreal's property stream is self-describing -- each field
carries a handle and a bit-length *before* its value, so field boundaries are
walkable without knowing the type, and the handle-to-name map ships inside the
replay itself (`NetFieldExportGroup`). Where that map is available, names can
be resolved independently of types. Unknown properties retain raw payloads;
typed values are an additive overlay. This is a preservation strategy, not a
claim that every stream is currently understood: framing failures and missing
schema attribution are measured separately, and successful movement decodes
can be represented by their rows instead of a duplicate raw RPC.

## Supported VALORANT builds

| 🎮 Build | 🌿 Branch | ✅ Clean/checked | 🔎 Verified by |
|---|---|---:|---|
| **13.06** | `release-13.06` | 38/38 | Validation + checkpoints + typed/raw |
| **13.05** | `release-13.05` | 401/401 | Validation + checkpoints + typed/raw |
| **13.04** | `release-13.04` | 108/108 | Validation + checkpoints + typed/raw |
| **13.02** | `release-13.02` | 205/205 | Validation + checkpoints + typed/raw |
| **13.01** | `release-13.01` | 215/215 | Validation + checkpoints + typed/raw |
| **13.00** | `release-13.00` | 1/1 | Validation + checkpoints + typed/raw |
| **12.11** | `release-12.11` | 1/1 | Validation + checkpoints + typed/raw |
| **12.10** | `release-12.10` | 1/1 | Validation + checkpoints + typed/raw |
| **12.09** | `release-12.09` | 3/3 | Validation + checkpoints + typed/raw |
| **12.08** | `release-12.08` | 3/3 | Validation + checkpoints + typed/raw |
| **12.07** | `release-12.07` | 3/3 | Validation + checkpoints + typed/raw |
| **12.06** | `release-12.06` | 3/3 | Validation + checkpoints + typed/raw |
| **12.05** | `release-12.05` | 3/3 | Validation + checkpoints + typed/raw |
| **12.04** | `release-12.04` | 3/3 | Validation + checkpoints + typed/raw |
| **12.03** | `release-12.03` | 3/3 | Validation + checkpoints + typed/raw |
| **12.02** | `release-12.02` | 3/3 | Validation + checkpoints + typed/raw |
| **12.01** | `release-12.01` | 3/3 | Validation + checkpoints + typed/raw |
| **12.00** | `release-12.00` | 3/3 | Validation + checkpoints + typed/raw |
| **11.11** | `release-11.11` | 3/3 | Validation + checkpoints + typed/raw |
| **11.10** | `release-11.10` | 3/3 | Validation + checkpoints + typed/raw |
| **11.09** | `release-11.09` | 3/3 | Validation + checkpoints + typed/raw |
| **11.08** | `release-11.08` | 3/3 | Validation + checkpoints + typed/raw |
| **11.07** | `release-11.07` | 3/3 | Validation + checkpoints + typed/raw |
| **11.06** | `release-11.06` | 3/3 | Validation + checkpoints + typed/raw |

Measured 2026-09-28 on all **1,018 unique available replays** across the 24
supported branches, every row by the [same acceptance rule](docs/BUILD_VERIFICATION.md):
ReplayData validation, checkpoint-enabled export, the independent comparisons on
observed evidence fields, and the strict array and array-leaf error counters.
✅ **1,018/1,018** are clean -- not a claim that every field is understood.
Structured-array child rows are admitted per build and per route: all
measured routes on 13.01--13.06, a measured subset on 11.06--13.00
([legacy route table](docs/LEGACY_BUILD_SUPPORT.md#measured-array-routes-2026-09-28)).

All branches are `++Ares-Core+release-<build>`. Adding a build is one
`SeededTransform` impl; see [Adding a new build](#supported-builds-and-the-cost-of-a-new-build).

## Highlights

- **Preservation with explicit accounting** — unknown property payloads and
  unresolved whole RPCs remain raw; stream failures remain visible. Typed
  values are an *additive* overlay, and the nullable `compatible_checksum`
  column helps distinguish untyped fields from fields lacking a descriptor.
- **Self-describing stream** — field names come from the replay itself
  (`NetFieldExportGroup`); no hardcoded agent or map names in the parser.
- **Main and checkpoint Parquet output** — six main tables are always written;
  `--checkpoints` adds seven checkpoint tables. All are ready for polars,
  pandas, or DuckDB.
- **Spike state** — plant site A/B (`PlantedAtSite` + position), defuser
  (`CurrentDefuser`), timer, and the canonical detonation signal.
- **Combat & abilities** — per-player economy, magazine and reserve ammo,
  equipped weapon over time, cooldowns, and absolute health, armour and overheal
  from the damage log — the value after each change, not a running subtraction.
- **Ability observations** — cast time/location and replicated ability
  statistics can be joined to actors and rounds. Repeated snapshots require
  deduplication; unresolved ownership and incomplete effect pairs remain gaps.
- **Numeric FastArray observations** — the standalone GAS extractor retains
  replication keys, deleted/changed item IDs and raw property boundaries; every
  window it reads in the 1,018-export audit closed exactly
  ([investigation](docs/GAS_AND_PATCHVOLUME_INVESTIGATION.md)). Property names
  and gameplay meanings remain unverified, and this output is separate from
  Parquet typed-value coverage.
- **Status-effect observations** — nearsight, slow, detain and suppress can
  arrive on affected actors. Matched start/stop records support intervals;
  unmatched records must not be assigned an invented duration.
- **Persistent effects** — smoke / wall / molly / slow / trap position and
  lifetime from actor lifecycles; one command via
  `tools/extract_active_effects.py`.
- **Spike custody** — who carried the spike and when, resolved to the account
  UUID, including Gekko's Wingman as a proxy carrier and the planter at each
  `spikePlanted`; one command via `tools/extract_spike_carrier.py`.
- **Account identity & typed events** — `manifest.players` (account UUID →
  actor → character), event `word0`/`word1` (killer/killed NetGUID, round
  index), ping/latency.
- **Cross-validated** against the C# reference parser on a 13.01 replay --
  movement near-bit-identical, CombatReport identical -- and the server-written
  Event chunk independently confirms the kill count. Against a later upstream
  revision on 13.06, movement differed by up to 5.14 per position axis
  ([upstream parity](docs/UPSTREAM_PARITY.md)).
- **Reproducible** — Parquet output is byte-for-byte identical run to run.
- **No `unsafe`** — `#![forbid(unsafe_code)]` in every crate; the only FFI is
  Oodle, isolated in an external crate.
- **806 Rust tests** plus a layered validation suite (framing / bytes / decode
  errors / semantics).

## Table of contents

- [Supported VALORANT builds](#supported-valorant-builds)
- [Highlights](#highlights)
- [Extractable data](docs/DATA.md) — the full inventory of what each table carries
- [Quick start](#quick-start)
- [Output](#output)
- [Status](#status)
- [Performance](#performance)
- [Comparison with the C# reference parser](#comparison-with-the-c-reference-parser)
- [The Event chunk -- the server's own timeline](#the-event-chunk----the-servers-own-timeline)
- [Whole-corpus robustness](#whole-corpus-robustness)
- [Type overlay](#type-overlay)
- [Supported builds and the cost of a new build](#supported-builds-and-the-cost-of-a-new-build)
- [Design](#design)
- [Validation suite](#validation-suite)
- [Generated files](#generated-files)
- [License](#license)

## Quick start

```bash
cargo +1.86.0 build --release -p vrfkit --features export --locked

vrfkit inspect  <file.vrf>                          # header / branch / chunk summary
vrfkit validate <file.vrf> [--diagnostics]          # grammar oracle, writes nothing
vrfkit export   <file.vrf> --out <dir> [--checkpoints]
```

`inspect` prints replay info, header, branch, and a chunk summary; it does no
parsing and returns immediately. `validate` walks every content block through
the RepLayout grammar and reports a pass rate; it writes no files. `export`
writes the Parquet tables and manifest described under [Output](#output) into
`--out`, which must be new, empty or a previous export: anything else in it
is refused, never deleted.

`export` is a default feature. Drop it with `--no-default-features` and
`arrow`/`parquet`/`zstd` never enter the dependency tree:

```bash
cargo +1.86.0 tree -p vrfkit --no-default-features --locked | grep -E "arrow|parquet|zstd"
# (no output)
```

A binary built without `export` **refuses the subcommand rather than failing
silently** -- a subcommand that printed nothing and exited 0 would be
indistinguishable from one that wrote the files.

On `02d4d478` (48,215,213 bytes, build 13.01), `export` produces thirteen
Parquet files plus a manifest when checkpoints are included:

| File | Rows | Bytes |
|---|---|---|
| `fields.parquet` | 1,296,660 | 12,691,368 |
| `movement.parquet` | 1,844,147 | 19,984,802 |
| `actors.parquet` | 3,827 | 68,243 |
| `net_guids.parquet` | 16,167 | 114,423 |
| `events.parquet` | 195 | 12,455 |
| `partials.parquet` | 0 | 2,505 |
| `checkpoint_fields.parquet` | 352,089 | 1,190,437 |
| `checkpoint_actors.parquet` | 3,014 | 24,345 |
| `checkpoint_net_guids.parquet` | 74,270 | 175,916 |
| `checkpoint_blocks.parquet` | 22,247 | 112,649 |
| `checkpoint_guid_entries.parquet` | 74,270 | 219,662 |
| `checkpoint_export_groups.parquet` | 8,307 | 16,481 |
| `checkpoint_export_fields.parquet` | 49,314 | 106,370 |
| `manifest.json` |  | ~660,030 |

`checkpoint_fields.parquet` requires `--checkpoints`. The partials row above
shows the default main-only export; both modes currently contain zero
rejected partial rows and occupy 2,505 bytes. The five original main tables remain
byte-for-byte identical across the checkpoint flag.

> Column schemas, the `tools/` scripts, the full validation suite, and
> per-crate usage live in [`docs/USAGE.md`](docs/USAGE.md).
> This document is about *why it is built this way*.

## Output

The main export writes six Parquet tables plus `manifest.json`; `--checkpoints`
adds seven checkpoint tables. String columns are dictionary-encoded with ZSTD.
The tables, their columns and the join rules are described in
[`docs/USAGE.md` section 3](docs/USAGE.md#3-output). Four traps to know before
reading them:

- `movement.parquet`'s `timestamp` is the 128 Hz server tick and **resets each
  round**; use `time_ms` for a global timeline.
- Posture is `fields.parquet`'s `bCrouchHeld`, not `movement_state`.
- `actors.parquet`'s `event` is `open` / `close` / `dormant`, and **only `close`
  is a despawn**. A dormant actor is alive and keeps its channel's archetype
  (`crates/vrfkit/src/sink/stream.rs`), so the next `open` is a wake-up, not a
  new instance.
- `manifest.json`'s `timestamp_ticks` is a UE `FDateTime` (100-nanosecond ticks
  since 0001-01-01), **not** a Windows FILETIME -- read as one it gives the year
  3626.

## Status

Work in progress. Currently verified: `cargo +1.86.0 test --workspace --locked`
**806 passing**; the full Python suite also has **1321 passing** tests. The
full documentation check passes. The latest [common build audit](docs/BUILD_VERIFICATION.md)
records replay validation, checkpoint export, independent value checks and
the resolved array findings and remaining semantic limits for each supported build.

Tagged Windows releases provide a ZIP and SHA-256 checksum. The release
workflow runs the complete CI on the tagged commit, checks the packaged binary
against a public replay, and publishes the verified ZIP without rebuilding it.
The same packaging checks run on PRs. See [release maintenance](CONTRIBUTING.md#tagged-windows-releases)
for tag naming and validation before publishing.

What CI runs, three pinned public replays included, is in
[CONTRIBUTING.md](CONTRIBUTING.md#what-ci-runs); the private-corpus checks stay
local.

Re-measure per-crate counts with `cargo test -p <crate>`. Counts are omitted
from the table below on purpose -- they go stale, and re-measuring is one line.

| Layer | Crate | Feature flags |
|---|---|---|
| Bit reader / UE wire format | `vrf-bitio` | `alloc` (default; drop it for `no_std`) |
| Payload transform (24 builds) | `vrf-transform` | none (`ALL_VERSIONS` is a length-independent slice) |
| Container (info/header/chunk/event/checkpoint, Oodle) | `vrf-container` | `oodle` `event` `checkpoint` |
| DemoFrame traversal | `vrf-frame` | none (sections are byte ranges for cursor alignment) |
| Replay dynamic schema + GUID cache + checkpoint tables | `vrf-schema` | `checkpoint` |
| Replication (packet/bunch/content block/field) | `vrf-net` | `diagnostics` |
| Field decoder + nested arrays + type overlay + effects | `vrf-decode` | `array` `effect` `overlay` `structs` |
| Movement decoder | `vrf-movement` | none (single protocol) |
| Parquet export | `vrf-export` | `parquet` + per-table |
| Unified CLI | `vrfkit` | `export` (default) |

ZSTD is deliberately *not* feature-gated out -- every writer picks it, so
disabling it would produce files this crate could not explain.

CI checks every core-only and singleton feature of the table. The cases are
listed in [`CONTRIBUTING.md`](CONTRIBUTING.md#before-you-open-a-pr) and in
`ci.yml`'s `$matrix`, and `tools/check_docs.py` (also with `--fast`) fails if
the two differ in membership or order: add a case to both.

## Performance

Historical optimization measurement on `02d4d478` (48,215,213 bytes), August
2026. These timings predate the current decoding additions:

| | Before optimization | After optimization |
|---|---|---|
| `export` | 1.64 s / 201 MB | **0.85 s / 109 MB** |
| `validate` | 1.42 s / 65 MB | **0.693 s / 65 MB** |

Figures are wall-clock / peak memory. Output is **byte-for-byte identical**
before and after. Detail and the optimizations measured and then rejected are
in `docs/archive/PROJECT_STATUS.md` section 25.

## Comparison with the C# reference parser

The same replay (`02d4d478`) was diffed against the output of the existing C#
parser. The CombatReport and RPC-parameter comparisons below were re-measured
on 2026-09-14 against `CliReader export` built from two ValorantReplayParser
commits: upstream `b51d674`, and `8824794`, the descriptor commit vendored in
[`third_party/vrp/`](third_party/vrp/README.md). The structure, movement and
volume figures are from the earlier comparison.

**Structure -- exact match.**

| | C# | vrfkit |
|---|---|---|
| Packets / bunches | 530,401 | 530,401 |
| Actor open / close | 2,028 / 1,799 | 2,028 / 1,799 |
| Export-group path set | 475 | 475 (intersection 475, both differences 0) |

**Movement -- effectively bit-identical.** Over a 50,000-row join (99.98%
matched), the maximum position error is 0.0005 (float rounding); yaw, pitch,
and velocity error is exactly 0. In that earlier comparison, row counts were 1,837,220 (C#) versus 1,839,607
(ours) -- the gap is the C# limitation of "emit only the last move of each
update"; we additionally recover 2,387 intermediate moves.

**CombatReport nested array -- every metric-input value matches.** This
structure is the sole source of K/D/A, ADR, HS%, multi-kills, and wallbangs,
so it was diffed as a multiset of values (`tools/compare_combat_report.py`,
against `8824794`; upstream leaves `Rounds` as a raw payload, and `8824794`
binds upstream's own `CombatRoundReportsDecoder` to it):

```
..Interactions[].AssistType                               364    364  IDENTICAL
..Interactions[].DamageDealt                              553    553  IDENTICAL
..Interactions[].DamageReceived                           553    553  IDENTICAL
..Interactions[].HitsDealt                                553    553  IDENTICAL
..Interactions[].HitsReceived                             553    553  IDENTICAL
..Interactions[].DidKill                                  414    414  IDENTICAL
..Interactions[].DealtInteractions[].Regions[].Hits       390    390  IDENTICAL
..Interactions[].DealtInteractions[].Regions[].Damage     390    390  IDENTICAL
..Interactions[].ReceivedInteractions[].Regions[].Hits    390    390  IDENTICAL
..Interactions[].ReceivedInteractions[].Regions[].Damage  390    390  IDENTICAL
```

**Extraction volume -- we export more.** `(group, field)` pairs break down as
1,450 vrfkit-only / 302 both / 71 C#-only, and 49 of the 71 C#-only are naming
differences (C# uses `CrouchHeld`; we use the wire name `bCrouchHeld`). RPCs
are 342,735 versus 230,893 -- 48% more -- because the C# parser drops RPCs
without a descriptor.

**RPC parameters -- every C# value is also ours, and ours has 14 more
records.** The ~330,000 RPCs had parameter payloads that were entirely raw;
they were decoded using the 84 parameter-schema groups (`<Class>:<Function>`
paths) the replay itself declares. Diffed with `tools/compare_rpc_params.py`
(record counts; every difference is vrfkit-only, none C#-only):

```
                                              upstream  8824794  vrfkit
MulticastNotifyKilledEnemy.KillerCharacter         119      132     132
MulticastNotifyDamage_Point.DamageDealt            580      580     581
MulticastEndRound.NewRoundNumber                    17       17      17
```

The other `KilledEnemy` and `Damage_Point` parameters (`KilledCharacter`,
`MultikillLevel`; `DamageTaken`, `RegionalDamage`, `bDamageKilledTarget`) have
the same counts as the line above them.

The 13 kills are the ones by character 576, Gekko (`AggroBot_PC_C`).
`MulticastNotifyKilledEnemy` is hosted on the killer's character actor, and
upstream's Gekko descriptor spells the class path `Aggrobot` where the replay
says `AggroBot`, so upstream drops every RPC on that actor. `8824794` corrects
the path (`f67ea66`) and agrees with vrfkit, which takes names from the replay
and never needed the descriptor. The existing pipeline papered over the 13
missing kills by recovering them later as CombatReport credit, so they vanished
from the timeline; in vrfkit the timeline itself is complete.

The one extra damage record is a killing blow (29.45 dealt, 20 taken) on the
`DamageableComponent` of Gekko's E-ability projectile -- actor 27232, packet
391880, channel 194 -- whose actor closes six packets later. Neither C# build
can name that component's class: it is stably named `Damageable`, so the wire
carries no class GUID for it, and the C# resolver's fixed table of
stably-named components lacks the name, so the block is skipped undecoded.
With that one name added, C# emits the record with the same 35 values
([how this was established](docs/FOLLOWUP.md#the-damage-record-only-vrfkit-emits)).
`compare_rpc_params.py` lists it as its one expected difference, keyed by
replay, packet, actor, subobject, channel, function and values, and fails if
it stops occurring exactly so.

## The Event chunk -- the server's own timeline

The `.vrf` Event chunk is the event list the server labeled and wrote itself,
stored elsewhere in the file under a different encoding, and **the existing C#
parser does not even open this chunk** (`ReplayChunkDispatcher.cs:152` --
`"Skipping event chunk"`). We now read it.

```
characterDeath 132 | characterUltimateUsed 34 | roundStarted 18
spikePlanted 9 | spikeDefused 1 | switchTeams 1          (02d4d478, 195 events)
```

The 132 `characterDeath` events exactly match the 132 `MulticastNotifyKilledEnemy`
events we extracted from RPCs -- and the C# parser's 119 plus character 576's
13. The two payload words are the killer/killed NetGUIDs, and **132/132 match
in order** (0/132 matched reversed). "We are right and the C# parser missed
them" is no longer our claim; it is the result of diffing against the server's
own record.

Scope, to be precise: the killer/killed pair diff is for **one replay**. A
separate Event-only sweep covered **527 files and 109,126 chunks** across
releases 13.01, 13.02 and 13.04. Every outer chunk and known inner payload was
consumed exactly, with zero unknown groups and zero residual bytes.

The payload layout is `[u32 group tag][N x u32 words][FString
"EReplayEventGroup::<Name>"][f32 seconds]`, and `N` differs per group. The `N`
for all seven groups is derived from the corpus as the residual-zero count
(CharacterDeath = 2, CharacterUltimateUsed / RoundStart / SwitchTeams = 1,
SpikePlanted / Defused / Exploded = 0). The same sweep established one stable
tag per group (respectively 8, 11, 2, 3, 4, 5 and 6), public enum-name strings
of at most 40 bytes, and a maximum `payload_seconds * 1000 - time1` absolute
difference of 0.999878 ms. vrfkit requires all four checks within a 1.001 ms
time tolerance before exporting `word0`/`word1`, `payload_tag`, `payload_name`
and `payload_seconds`; on any mismatch those nullable columns stay empty. The
original is always left intact in `raw_payload`.

## Whole-corpus robustness

The 2026-09-28 [common audit](docs/BUILD_VERIFICATION.md) checks 1,018 unique
replays across 24 builds. Every ReplayData validation and checkpoint export
succeeds. Independent typed/raw comparisons match 13,387,751
observed values. All 1,018 pass the strict quality gate. On 2026-09-25, the
two ActiveBlinds fixes resolved the earlier 81-file findings and recovered 522
additional typed children in the 986-replay corpus; an independent
before/after comparison verified those values and preserved every existing
field row and raw payload.

A 100% block pass rate means every measured block reached an ordinary
field/RPC row or an explicit preservation row; partial reassembly rejections
are outside that score, and `malformed framing 0` does not prove that earlier
transport stages kept every payload. It is **not** a typing claim either: a
block that cannot be assigned a `_ClassNetCache` group has inner handles
nothing can name, so it becomes one reserved row (`handle = u32::MAX`, the
complete decoded payload in `raw_bits`) counted under `RPC unresolved/raw` --
uninterpreted, not lost. The earlier sweeps, from the 215-replay 13.01 run that
first exposed unattributed blocks to the 714-replay tail preservation, are in
[`docs/archive/CORPUS_SWEEPS.md`](docs/archive/CORPUS_SWEEPS.md).

The controller's opening bunch was once framed nine bits early (the
spawn-velocity bit and the net-player-index byte); see
`crates/vrf-net/src/pipeline/spawn.rs` and `docs/archive/PROJECT_STATUS.md` 17-A.

## Type overlay

For any field whose inner stream can be walked, the raw bits are always
exported; when the type is known, the `value_*` columns are filled **as an
overlay.** If the type is unknown, or decoding fails, the row's `raw_bits`
remains. The only exception is a ClassNetCache block whose group cannot be
identified: it cannot be expanded into fields, so it emits one preservation
row (`handle` = `u32::MAX`, full payload in `raw_bits`) and an explicit
unresolved/raw diagnostic rather than pretending the properties were decoded.

The overlay table is extracted mechanically from the C# descriptors
(`tools/extract_descriptors.py`) -- 224 groups, 1,336 entries, 96 handles.
Those descriptors are vendored verbatim in
[`third_party/vrp/`](third_party/vrp/README.md),
and CI regenerates the table from them on every push.
Nothing is transcribed by hand, for the same reason S-boxes and golden vectors
are not: it is the kind of constant where a typo is invisible in review.

Four names resolve without a table entry: `Owner`, `Instigator`, `AttachParent`
and `Controller` are `AActor` / `USceneComponent` object references Unreal
replicates on every actor, always as a NetGUID. The descriptors declare them
only for the classes they happen to cover, which left the same four names typed
on 129 group/field pairs and untyped on 203 more. Since the type is fixed by the
engine rather than by the class, they resolve by name after the table misses --
a claim about Unreal, not a guess about any one Blueprint, and it holds for
groups no replay has spawned yet. In the historical 215-replay release-13.01
export sweep, it typed 6,048 further rows with decode errors still at zero.

`02d4d478` (`02d4d478-1dfb-4412-9a77-29ca29105a9d.vrf`), as recorded by the
committed export baseline `tools/baselines/export_02d4d478.json` after the
partial-header and shot-array corrections and the component remaps read from the
13.06 game:

```
Decoded OK:   822,185      Decode errors:      0
Raw/Skip:      24,747      Not in table: 140,814
No field name:  1,249      Typed:          83.1%
Effect blobs:  61,617
```

The four buckets partition `Rows offered` exactly (822,185 + 24,747 + 140,814 +
1,249 = 988,995), and `Typed` is `Decoded OK / Rows offered`. `check_docs.py`
compares these counters and the Parquet row/byte table with that baseline.

**Effect decoding is additive and does not move these buckets.** The overlay
buckets are settled before the effect pass, so rows that gained a value from an
effect are still counted under `Not in table`; merging them into `Decoded OK`
would double-count and move the baseline for unrelated reasons. `Effect blobs`
is reported separately -- without it, 61,617 rows gain a value yet the summary
prints identically.

Physical value coverage is the fraction of `fields.parquet` rows with at
least one non-null `value_*` column. It cannot be computed by adding overlay,
effect-blob or struct counters: these count different units and may describe
parent/child expansions of the same input. The current reference
baseline has 939,382 typed rows out of 1,296,660 (72.45%), measured directly
from its columns.
Adding raw child windows changes this denominator even when every old typed
value survives; compare raw preservation and newly typed values separately.
That snapshot is not a fraction of all game information understood.

Measure the files you actually use, with checkpoint rows reported separately:

```bash
python tools/summarize_value_coverage.py <export-directory> > coverage.json
python tools/summarize_value_coverage.py <parent-of-export-directories> --jobs 4
```

`Typed` is the ratio printed in the summary: rows the overlay decoded
successfully (`Decoded OK`) over rows it examined (`Rows offered`). The
denominator includes every RPC parameter, so it reads low -- most of `Not in
table` is RPC parameters without a C# descriptor, plus the groups the replay
declares (475) that are not in the table. (Rows with a filled `value_*` also
include additive decoders like effects and structs, so that is a different
population from the overlay's input rows.) Unknown ordinary properties retain
raw bytes. The absence of a typed value does not by itself establish loss;
conversely, a high typing ratio does not establish complete block preservation.

`fields.parquet` also carries the replay's own `compatible_checksum` per row,
which turns that leftover into something searchable. Unreal hashes a property's
type into it, so it identifies the property across builds; bucketing untyped
rows on it separates three situations that otherwise look identical -- a type
the overlay knows and failed to apply, a described property nothing has typed,
and a value addressed inside a payload that declares no handle at all. Over 20
replays that splits 10,062,142 untyped rows 0.5% / 48.6% / 51.0%, and the first
bucket is supposed to be empty. The recipe is in
[`docs/USAGE.md`](docs/USAGE.md#fieldsparquet).

Decode errors are checked by `tools/check_decode_errors_corpus.py`, separately,
because `vrfkit validate` prints no overlay counters and `validate_corpus.py`
alone cannot see a wrong type. Reaching zero on the 215-replay 13.01 sweep found
three places where the wire disagreed with the C# declarations; they are
recorded with evidence in `tools/apply_type_corrections.py` (219 corrections,
verified with `--check`).

| Symptom | Actual | Evidence |
|---|---|---|
| Time-related `Float` field consumes more than 32 bits | Wire is `Double` (64-bit) | Every error is "32 bits consumed, 32 bits residual" |
| `215`/`216` `Int32` field arrives in 3 bits | Variable-width actor bookkeeping | The C# weapon descriptor comment states "width varies per build" |
| SmokeScreen projectile `ReplicatedMovement` EOF | Rotation is `ByteComponents` | Four other projectiles in the same codebase explicitly use `ByteComponents` |

Byte-width handling was also corrected. A byte property inside an array stores
only its significant bits, so a fixed 8-bit read fails -- the C# parser also
reads only `archive.BitsRemaining`. Before this fix, all 364 rows of
`AssistType` (5 bits) were left without a value.

## Supported builds and the cost of a new build

The payload transform changes per game build, but far more is **constant**
across supported releases 11.06 through 13.06: the PRNG and its multipliers, the seed-mix
skeleton, the 64 -> 32 -> 8 -> tail staging, the tail-XOR handling, and even
the S-box table itself. Each build supplies its own `word64` / `word32` /
`byte` operation sequence (order, rotations, complements, and whether the
S-box stage is used) plus these constants:

| | seed addend | offset | sign | S-box |
|---|---|---|---|---|
| release-11.06 | `0x3325e3bd` | `0x3d` | **+** | used |
| release-11.07 | `0x17b077d3` | `0x2d` | - | used |
| release-11.08 | `0xacf2cdff` | `0x01` | - | unused |
| release-11.09 | `0x12cf14e5` | `0x1b` | - | used |
| release-11.10 | `0x34e9d3ec` | `0x14` | - | used |
| release-11.11 | `0xc4445c41` | `0x3f` | - | used |
| release-12.00 | `0x70876679` | `0x07` | - | unused |
| release-12.01 | `0x13fdd831` | `0x31` | **+** | used |
| release-12.02 | `0x9830d09d` | `0x1d` | **+** | used |
| release-12.03 | `0x33d59dff` | `0x01` | - | used |
| release-12.04 | `0xa5684b42` | `0x3e` | - | used |
| release-12.05 | `0xc21d548c` | `0x0c` | **+** | used |
| release-12.06 | `0x8d686ca6` | `0x26` | **+** | unused |
| release-12.07 | `0x2d21d7c3` | `0x3d` | - | used |
| release-12.08 | `0xce2e33e5` | `0x1b` | - | used |
| release-12.09 | `0x7ff2feec` | `0x14` | - | unused |
| release-12.10 | `0x12fd0ee5` | `0x1b` | - | unused |
| release-12.11 | `0x409d36a3` | `0x23` | **+** | unused |
| release-13.00 | `0x2949b6ef` | `0x11` | - | used |
| release-13.01 | `0xe62fcd5c` | `0x24` | - | unused |
| release-13.02 | `0x9e81a37c` | `0x04` | - | used |
| release-13.04 | `0x076dc658` | `0x28` | - | unused |
| release-13.05 | `0x48c26613` | `0x13` | **+** | unused |
| release-13.06 | `0xe974593c` | `0x3c` | **+** | used |

In all twenty-four supported builds the **tail-XOR byte equals the low byte of the seed
addend.** It is a derived value, not an independent constant: `SeededTransform`
defaults `TAIL_XOR` to `SEED_ADDEND as u8` (`versions/mod.rs`), and a build
that broke the pattern would fail its 1- and 7-bit vectors, which
`vectors_cover_the_staging_boundaries` in `crates/vrf-transform/tests/golden.rs`
requires for every registered build.

So adding a build is one `SeededTransform` impl: its branch, `SEED_ADDEND`,
`INIT_A_OFFSET`, optionally `ADD_OFFSET` and `TAIL_XOR` (both defaulted), and
three word functions (`word64` / `word32` / `byte`); everything else is shared.

Every build in the table above is confirmed on live replays, not only by
transform vectors. The 13.04 corpus was also exported with checkpoints on
2026-08-31: all 108 replays carried them and reported zero typed-overlay,
struct-blob and checkpoint failures over 110,152,399 offered rows, 20,756
decoded struct blobs, 3,129,483 decoded checkpoint fields and 1,872 decoded
checkpoint blobs. A machine-local corpus can rotate; the reproducible oracle is
88 mechanically extracted upstream golden vectors (11 staging boundaries per
build, eight builds) plus 1,264 native-machine-code vectors for the sixteen
recovered 11.06--12.09 builds, with a full 48-sample main/checkpoint validation
([build support validation report](docs/LEGACY_BUILD_SUPPORT.md)). 13.06 was
first validated on six real replays ([upstream parity report](docs/UPSTREAM_PARITY.md));
the 2026-09-28 [common audit](docs/BUILD_VERIFICATION.md) checks 38.

The 768-byte S-box is shared across builds, which makes it usable as a
**signature for locating the transform function in a binary.**

## Design

### 1. A clean parallelization point

The content-block **header and declared bit-length are plaintext**; the
transform only touches the payload that follows. So framing (sequential,
unavoidable because of the replication state machine) and block decode (fully
independent) can be separated. The transform is determined solely by
`(bits, seed)`, so it parallelizes per block.

### 2. Output is Parquet

Columnar storage collapses the repeated path and name strings via dictionary
encoding, zstd compresses it well, and it reads directly in `pyarrow` /
`polars` / `pandas` / `duckdb`. NDJSON is reader-bound: on a 1.8-million-row
movement stream, JSON parsing was measured at 84% of processing time.

## Validation suite

The checks are layered, and the layers catch different things; each is in
[`docs/USAGE.md`](docs/USAGE.md) section 6 with what it misses:

- **Common build audit** (`verify_build_corpus.py`) -- one set of checks on
  every available replay; per-build results are the support table above.
- **Framing** (`validate_corpus.py`) -- content-block framing, loss accounting
  and unresolved-payload preservation.
- **Bytes** (`check_export_baseline.py`) -- regression in any export counter
  or in a file's rows, bytes or SHA-256.
- **Decode** (`check_decode_errors_corpus.py`) -- overlay, struct, array,
  movement and brute-force failures, and the work counters behind them.
- **Semantics** (`check_metrics_baseline.py`, 8 builds) -- round count,
  score, K/D/A invariants that need no baseline.

## Generated files

The following files are generated and must never be edited by hand:

| Generated file | Generator | Notes |
|---|---|---|
| `crates/vrf-decode/src/table.rs` | `tools/extract_descriptors.py` then `tools/apply_type_corrections.py` | The overlay table (1,336 entries, 224 groups, 96 handles) and handle table, from the vendored descriptors in `third_party/vrp/` |
| `crates/vrf-decode/src/checksum_table.rs` | `tools/extract_checksum_types.py` | Replay-observed checksum-to-type propagation table; conflicting donors are omitted |
| `crates/vrf-decode/src/scoped_types.rs` | `tools/generate_scoped_types.py` | Exact group/name/checksum types for ambiguous or descriptor-silent field names, including upstream-declared geometry and enum shapes; no cross-group propagation |
| `crates/vrf-transform/src/sbox.rs` | `tools/extract_sboxes.py` | 768-byte S-box, shared across builds |
| `crates/vrf-transform/tests/data/golden_vectors.rs` | `tools/extract_golden.py` | Per-build golden test vectors |
| `crates/vrf-transform/tests/data/native_vectors.rs` | `tools/capture_native_transforms.py` | Expected bytes from pinned original executable readers |
| `tools/equippable_table.py` | `tools/extract_equippables.py` | Weapon class path to display name, from the vendored `ValorantEquippableResolver.cs` |

Regenerate them as [`CONTRIBUTING.md`](CONTRIBUTING.md#generated-files--never-hand-edit)
describes; CI regenerates `table.rs` and `equippable_table.py` from the
vendored input and fails if either differs. The S-box and golden-vector
generators need an upstream checkout, and each refuses to write a table that
fails its integrity check: the S-box must be a permutation of 0..255, and each
golden vector's hex length must match its bit count.

## License

MIT. Derivation and original authorship are in [`NOTICE.md`](NOTICE.md).

This is an independent, community-developed tool. It is not affiliated with,
endorsed by, sponsored by, or approved by Riot Games. VALORANT, Riot Games,
and all related trademarks are the property of Riot Games, Inc.
