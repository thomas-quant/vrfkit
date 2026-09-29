# vrfkit

Copyright 2026 vrfkit contributors

Licensed under the Apache License, Version 2.0; see [`LICENSE`](LICENSE).

## Earlier MIT-licensed releases

Releases up to and including v0.2.0 were published under the MIT License, and
contributions made before the change to Apache-2.0 were made under it. The MIT
License allows that code to be distributed under other terms as long as its
notice stays with it, so the notice is kept here:

```
MIT License

Copyright (c) 2026 vrfkit contributors

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

# Third-party notices

## ValorantReplayParser

Parts of this project are derived from **ValorantReplayParser** by Michel Giehl,
used under the MIT License.

- Source: https://github.com/michel-giehl/ValorantReplayParser
- License: MIT

```
MIT License

Copyright (c) 2026 Michel Giehl

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

### What is derived

| Area | Relationship |
|---|---|
| `crates/vrf-transform` | The eight 12.10--13.06 per-build payload transforms and their constants are a port of `Replay.Encoding/PayloadEncryption`. The substitution tables and golden test vectors are extracted mechanically from that source (`tools/extract_sboxes.py`, `tools/extract_golden.py`). |
| `crates/vrf-bitio` | The Unreal wire primitives (`IntPacked`, bounded `SerializedInt`, `FString`, bit copying) follow the semantics implemented in `Replay.Encoding/Archives`. |
| `third_party/vrp` | ValorantReplayParser's `src/Replay.Valorant` C# descriptor sources (upstream `2d2e05e` plus five local descriptor commits), with a selective reveal descriptor update from `2b66c65`, the equippable resolver from `2103d92` and upstream's `LICENSE`. Its README records the exact revisions and local adaptations. |
| `crates/vrf-decode/src/table.rs` | Generated from that copy (`tools/extract_descriptors.py`), then corrected against wire evidence (`tools/apply_type_corrections.py`). |
| `tools/equippable_table.py` | Weapon display names generated from that copy's `Combat/ValorantEquippableResolver.cs` (`tools/extract_equippables.py`). |
| `crates/vrfkit/src/sink` | The ActiveBlinds and projectile path field layouts reference upstream flash descriptors at `d23c13e`, independently validated against preserved replay payloads. |
| `tools/extract_player_effects.py` | The player-body / possessed-device distinction follows the flash and nearsight correction in upstream `2b66c65`; the tool retains non-player observations and does not infer unique hits. |

The reverse engineering of VALORANT's payload transformation originates with that
project. The additional 11.06--12.09 word transforms were recovered independently
from pinned original executables, using the shared primitives established by
that project. Native expected-byte vectors are captured by
`tools/capture_native_transforms.py`; their staging-boundary input is the
upstream test payload. No game executable bytes are distributed here.

## Prior art acknowledged upstream

ValorantReplayParser credits
[FortniteReplayDecompressor](https://github.com/Shiqan/FortniteReplayDecompressor)
for documenting the Unreal replay system. That documentation informs the
replication layer here as well.

## Disclaimer

This project is an independent, community-developed tool and is not affiliated
with, endorsed by, sponsored by, or approved by Riot Games. VALORANT, Riot Games,
and all related trademarks are the property of Riot Games, Inc.
