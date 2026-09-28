# Build recovery source research, 2026-09-24

**Result: all seven builds (11.06--11.11 and 12.00) now have recovered code
and verified replay transforms.** This document records the source search
and measured offline recovery. Native-reader comparisons and actual replay
results are in [build support validation](LEGACY_BUILD_SUPPORT.md).

No matching public transform implementation or decrypted executable was found
in the sources checked below. There is one concrete offline Packman decryptor,
but it targets League of Legends 8.14 from 2018. Generic VMProtect emulators
provide implementation references for unpacking the local `stub.dll`; none
of the inspected projects demonstrates that it decrypts these VALORANT builds.
This bounded search does not establish that no suitable dump exists elsewhere.

## Replay implementations and dumps checked

The upstream
[transform directory at `2b66c65`](https://github.com/michel-giehl/ValorantReplayParser/tree/2b66c65a7b116154e18ebb84d9f6795f2b080233/src/Replay.Encoding/PayloadEncryption/VersionedTransforms)
contains 12.10, 12.11, 13.00, 13.01, 13.02, 13.04, 13.05 and 13.06. It has
no transform for the seven target builds. The maintainer's
[methodology comment](https://github.com/michel-giehl/ValorantReplayParser/issues/2#issuecomment-5016680427)
describes locating the transformed reader through
`UActorChannel::ReadContentBlockHeader` in Ghidra. It does not supply an
unpacker for encrypted executables.

The default-branch trees of all nine public forks GitHub returned have
versioned transforms only from 12.10: 33k0 `914039a`, Chyste and
bhaskoro-muthohar `b51d674`, Matthias1590, lincolnchun, xiaowan108 and
bmblChloe `99d9646`, aubwang `64c54b4`, Archers007 `2017487`. Two dump
collections do not provide the code either:
[ZaweSec/Valorant-Dumps at `f7d8fd0`](https://github.com/ZaweSec/Valorant-Dumps/tree/f7d8fd081a69c838aedc54fae1661c61529a92fe)
lists 2023 and January 2024 archives, older than the replay system Riot
[introduced on PC with 11.06 in September 2025](https://playvalorant.com/en-us/news/dev/replays-everything-you-need-to-know/)
(not downloaded), and
[scros22/valorant-dumps at `bef7fc8`](https://github.com/scros22/valorant-dumps/tree/bef7fc8a37ae3ad68a7b2607f70ee028034b3051)
publishes offsets and notes, excluding binaries and game code.

## Offline Packman implementation: useful history, incompatible input

[MythicManiac/lol-unpackman at `5241f93`](https://github.com/MythicManiac/lol-unpackman/blob/5241f938d3bebc442e20067892f7f91593aad887/unpackman.cpp)
is an actual file-to-file decryptor. Its source identifies the supported target
as League of Legends 8.14, dated 20 July 2018. It reads the game executable and
`stub.dll` without starting either one.

The source implements an RC4 key schedule and stream cipher. Its first stage
maintains one cipher state across metadata, `.text` and imports. A second
stage initializes a separate cipher for each 4 KiB code page, cycling through
83 seeds of 121 bytes in the stub. Seed locations, import locations, section
sizes and the original entry point are hardcoded for that particular x86
League executable.

**Unverified hypothesis:** a related cipher or key layout may remain in the
VALORANT stub. The 2018 source does not prove that. Applying its fixed offsets
to a 2025 x64 image is not a valid decryption method. All 19 forks listed by
the [upstream fork API](https://api.github.com/repos/MythicManiac/lol-unpackman/forks?per_page=100)
had last-push dates in 2018; no maintained VALORANT port was identified.

## References for unpacking the stub in an emulator

| Source | What the inspected code provides | Applicability limit |
|---|---|---|
| [aftermathlabs/vmp2 `7f42ca0`, `unpacker.cpp`](https://github.com/aftermathlabs/vmp2/blob/7f42ca00b7bd8d357528806c9bcbb7b9f2248d3d/vmemu/src/unpacker.cpp) | x64 Unicorn image mapping, stack, imported-function hooks, file-mapping emulation and memory-image output. Its library hook reads DLL bytes into emulator memory. | The project targets VMProtect 2. It supplies a scaffold, not verified support for this stub or its dependencies. |
| [milk-analyzer/vmpunpack `c24b1ca`](https://github.com/milk-analyzer/vmpunpack/tree/c24b1ca8b9f60f7370e308070a864c0eae3ae7cb) and [its sogen engine `d3b0a45`](https://github.com/milk-analyzer/sogen/tree/d3b0a45491a5583ec5705002d9abd6453816425f) | Drives a patched Unicorn-backed Windows emulator until an original-code-section execution heuristic fires, then checks and reconstructs a dump. | Setup advertises about 500 MB of runtime downloads. The driver identifies an `.exe` target; this DLL needs adaptation. The tool does not devirtualize protected functions, and no VALORANT result is supplied. |
| [NoVmp `6c23c9a`](https://github.com/can1357/NoVmp/blob/6c23c9a335f70e8d5ed6299668fd802f2314c896/README.md) | Static devirtualization for VMProtect x64 3.x. | Its documented input must already be unpacked. It cannot replace the initial unpacking step. |

## Measured offline recovery

The seven archived EXE/stub pairs are pinned by SHA-256 in
[native_recovery.json](../tools/fixtures/native_recovery.json). All acquisition
inputs passed manifest chunk checks and Riot Authenticode verification.
The reproducible [recovery tool](../tools/recover_native_binaries.py) performs:

1. **Stub section unpacking.** Seven raw LZMA streams per build, identified
   by destination-RVA descriptors, recover the loader instructions. All 49
   streams reached EOF within source/destination bounds. The 11.06 sections
   were compared against execution of the original unpacker; remaining
   differences are protected-section loader fixups, outside the cipher code.
2. **Whole-code decryption.** A native ARX block decrypts the metadata marker
   to `0x504b4d4e`, identifying one local 32-byte source per build. The code
   variant has different round dependencies from standard Salsa/ChaCha.
   Independent Python and Rust arithmetic matched 11 native vectors, including
   randomized keys and offsets. The full 11.06 native loader pass matches all
   144,715,776 bytes except its later one-byte mutation at RVA `0x0793faaf`
   (`0x96` becomes `0x65`). Clean recovered images retain the direct cipher
   output, not that startup mutation. Synthetic native vectors are committed
   in [native_bulk_vectors.json](../tools/fixtures/native_bulk_vectors.json).
3. **Page decryption.** The loader has 97 native page-cipher fragments, chosen
   by page index modulo 97. Each build cycles through its own key-source rows.
   The source bytes determine the 32-byte key and eight-byte nonce locally.
   Bounded Unicorn execution of these fragments yields a 4 KiB XOR stream.
   The key-row/fragment pair repeats after `lcm(row_count, 97)` pages. The
   tool reconstructs all code pages and requires the final pinned image hash
   before writing anything. All 97 fragments in each of the other six stubs
   also matched the 11.06 fragment outputs for independent nonzero synthetic
   keys: 582 cross-build comparisons.

| Build | Key rows | Row stride | Replay reader RVA |
|---|---:|---:|---|
| 11.06 | 80 | 183 | `0x3f16240` |
| 11.07 | 179 | 208 | `0x3f41460` |
| 11.08 | 189 | 120 | `0x3f6a400` |
| 11.09 | 143 | 71 | `0x3f5ae20` |
| 11.10 | 166 | 93 | `0x3f48e10` |
| 11.11 | 136 | 103 | `0x3ff0130` |
| 12.00 | 169 | 196 | `0x3fec210` |

Full loader emulation with modeled Windows APIs was used during discovery.
The final recovery tool only decompresses sections, runs checked arithmetic
and emulates isolated cipher fragments. It needs no running game, kernel
service, external key, display profile or API emulation. Output PE files are
analysis inputs: only the code section is recovered; archived imports and
metadata are retained. They are not reconstructed game installations.

All seven readers now execute to completion with the standard reader layout,
including the original bit-copy helper. Ghidra inspection and 79 independent
native cases per reader establish the per-build arithmetic; real replay and
typed-value checks establish that the selected reader is the replay variant.
11.08 and 11.11 required inspecting instruction boundaries directly because
split unwind records initially omitted their reader prologues.

## Tools that do not solve the offline requirement

- [archercreat/packman-deobfuscator `697abd9`](https://github.com/archercreat/packman-deobfuscator/blob/697abd9aaf65fc1e00a0ef00816cbff3f04e19ed/unpackman.py)
  is a 2019 IDA script that emulates x86 instruction snippets and patches
  opaque predicates. It simplifies existing instructions; it has no code
  section decryption algorithm.
- [nitrog0d/ValorantUnpacker `ed4c189`](https://github.com/nitrog0d/ValorantUnpacker/tree/ed4c18971e4ef4d4a023b84996b7ca9c88c9796c)
  and [lil-skies/val-exception-handler `feee424`](https://github.com/lil-skies/val-exception-handler/blob/feee4240413ccd65f67ac46008269bb8e8857343/dllmain.cpp)
  operate inside a running process. The latter uses the loaded module and
  exception-driven page access before copying memory. They do not independently
  decrypt an archived executable on disk.

The acquisition inputs are archived, manifest-verified files. Game and stub
instructions used in these experiments run inside Unicorn with modeled OS
services. No archived game executable or driver was started natively, no
installed game/security configuration was changed, and no message was sent
to repository maintainers.
