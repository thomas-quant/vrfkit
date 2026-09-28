# Vendored descriptor sources (ValorantReplayParser)

`Replay.Valorant/` holds the 163 C# descriptor files that
`tools/extract_descriptors.py` reads to generate
`crates/vrf-decode/src/table.rs`. They are copied verbatim from the commit
below, with the selective updates recorded below. Nothing here is compiled;
it is generator input. Two other tools read it
by default: `tools/extract_equippables.py` generates `tools/equippable_table.py`
from `Replay.Valorant/Combat/ValorantEquippableResolver.cs`, and
`tools/analyze_coverage.py` lists the descriptor paths.

The directory is `vrp/Replay.Valorant`, not upstream's
`ValorantReplayParser/src/Replay.Valorant`, to keep paths short: without
`core.longpaths`, Git on Windows refuses to check out a file whose full path is
longer than 259 characters. The deepest file is 100 characters below
`Replay.Valorant`. Under the upstream names the repository's longest path would
be 137 characters, which fails in any checkout directory longer than 121
characters; here it is 116, which allows 142. (Measured 2026-09-13: a
144-character directory failed on 11 files under the upstream names and on 1
here, exactly the files past 259.)

## Why they are in this repository

`table.rs` used to be generated from a local branch of a ValorantReplayParser
clone that existed on one machine and was never pushed. Nothing else could
regenerate the table, so nothing could confirm the committed file still matched
its source. Extracting from upstream instead is not a substitute: at `b51d674`
it yields 724 entries where this input yields 1,185 (491 removed, 30 added, 6
retyped) and changes 16 handles.

CI now regenerates `table.rs` from this directory and fails if the result
differs from the committed file, and runs `extract_equippables.py --check`
against it.

## Provenance

- Upstream: https://github.com/michel-giehl/ValorantReplayParser, MIT; `LICENSE`
  here is upstream's, from the same commit as the sources.
- Copied from commit `882479446baef070490ee073bf4dd4da429668cd`, directory
  `src/Replay.Valorant`, `.cs` files only (the `.csproj` is left out; the
  extractor reads `*.cs`).
- That commit is upstream `2d2e05e8c2d14dffb644b6da54819cd1e3fe1ee0`
  (2026-07-21) plus five commits that exist only in that local branch. Together
  they change 31 files under `src/Replay.Valorant`:

  | Commit | Subject |
  |---|---|
  | `fe5343a` | feat: descriptors for weapons, inventory, purchases, bomb and effects |
  | `f67ea66` | fix(descriptors): Gekko's path is AggroBot, not Aggrobot |
  | `d2b76f2` | feat(descriptors): bind the ability pawns and projectiles that had none |
  | `f0dd7e7` | merge: bind the ability pawns and projectiles that had no descriptor |
  | `8824794` | fix(descriptors): the spawn transform is three components, not one FTransform |

- Checked on 2026-09-13, when the copy was made: the 162 files were
  byte-identical to that commit's, `tools/compare_descriptor_sources.py`
  reported 0 field changes and 0 handle changes between the two, and
  regenerating `table.rs` from this directory reproduced the committed file
  byte for byte. A checkout that converts line endings changes the files'
  bytes but not the generated table.

## Changing a descriptor

### Selective update, 2026-09-24

`Reveals/Descriptors/RevealDescriptors.cs` is copied verbatim from upstream
[`2b66c65a7b116154e18ebb84d9f6795f2b080233`](https://github.com/michel-giehl/ValorantReplayParser/commit/2b66c65a7b116154e18ebb84d9f6795f2b080233).
The Hunter and BountyHunter catalogs register its five descriptors. The old
local `HunterRevealBoltDescriptor` is replaced by the equivalent upstream
`SovaRevealProjectileDescriptor`, retaining byte-component rotation and adding
explicit handles. Its previous 13.01 measurement was 486 exact byte-rotation
payloads; short rotation failed on 225 (222 EOF, three residual).
The other local ability descriptors remain registered. See
[`UPSTREAM_REVEALS.md`](../../docs/UPSTREAM_REVEALS.md) for replay verification.

### Selective update, 2026-09-28

`Combat/ValorantEquippableResolver.cs` is copied verbatim from upstream
[`2103d924c59a629fb3eaecd4dedf283b0267a761`](https://github.com/michel-giehl/ValorantReplayParser/commit/2103d924c59a629fb3eaecd4dedf283b0267a761)
("feat: Add warden"). The copy it replaces was byte-identical to that file at
`2d2e05e` and at `2b66c65` (git blob `bd79109`); the new copy is blob
`1b512e1`, byte-identical to `2103d92`'s. The only difference is one `Define`
naming `/Game/Equippables/Guns/Rifles/BattleRifle/BattleRifle.BattleRifle_C`
`Warden` (rifle). The file declares no descriptor, so `table.rs` regenerates
byte for byte; it feeds `tools/extract_equippables.py` only. Every other file
here keeps the provenance above. Upstream's Raze descriptor commit `8b7afcb` is
not vendored: the generator cannot read its descriptors, and its adopted types
are exact group/name/checksum identities instead
([`UPSTREAM_RAZE_WARDEN.md`](../../docs/UPSTREAM_RAZE_WARDEN.md)).

### Regeneration

Edit the file here, regenerate, and commit the descriptor change together with
the regenerated table:

```bash
python tools/extract_descriptors.py third_party/vrp/Replay.Valorant \
    crates/vrf-decode/src/table.rs
python tools/apply_type_corrections.py
cargo +1.86.0 fmt -p vrf-decode
git diff crates/vrf-decode/src/table.rs   # empty unless a descriptor changed
```

Do not refresh the whole directory from upstream. Upstream has diverged since
`2d2e05e`, and a wholesale copy would drop the descriptors above (see
[`docs/REFERENCE_REPOSITORIES.md`](../../docs/REFERENCE_REPOSITORIES.md)). To
review what upstream changed, compare it against this directory and port what
you need:

```bash
python tools/compare_descriptor_sources.py \
    --baseline third_party/vrp \
    --candidate <upstream clone>::<revision> \
    --downstream-table crates/vrf-decode/src/table.rs --output audit.json
```
