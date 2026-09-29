# `FText` wire-layout research [ARCHIVED -- HISTORICAL]

Research snapshot: `vrfkit` commit `922ac1c46d52f0e642cb78ccefc9517792c266d9` (2026-09-08 investigation).

## Proven locally

- `decode_ftext` currently skips 33 bits, reads an 8-bit value, accepts only `5`, then reads `FString table_path`, 32 bits, and `FString key` ([`scalar.rs` lines 76-112](../../crates/vrf-decode/src/decode/scalar.rs#L76)). Its three checked-in vectors start `00 00 00 00 0B ...` and decode with zero residual bits ([tests lines 299-378](../../crates/vrf-decode/src/tests/scalar.rs#L299)). These facts prove the observed bits and successful key extraction, but do **not** prove that bit 33 begins an Unreal history discriminator.
- The descriptor labels `LocalizedRewardName` as `FText` while retaining it as `RawPayload`. This is type evidence, not wire-layout evidence.
- For the existing vectors, interpreting the first 40 bits as little-endian `u32 flags = 0`, followed by byte `0x0B`, is exact. The current 33+8 split instead consumes bit 0 of `0x0B` in the skipped region and reads `(0x0B >> 1) = 5`; the following zero bit then becomes the first bit consumed by the current inline-name/string decoding.
- The new `LocalizedRewardName` h29 population supplied by the concurrent extraction has first 32 bits `1`, byte-at-bit-32 `3`, and bit 40 `0`; the current 33+8 view reports `1`. That arithmetic is likewise exact: `(3 >> 1) = 1` when bit 40 is zero.

## Primary-source facts

- Epic describes `FText` as holding variable internal “text history”; histories rebuild text correctly for culture changes and support “sending `FText` over the network.” This establishes that multiple payload shapes are expected, without specifying replay bits ([Epic, Text Localization, UE 5.8](https://dev.epicgames.com/documentation/en-us/unreal-engine/text-localization-in-unreal-engine)).
- Epic exposes `FText::SerializeText` overloads for `FArchive` and `FStructuredArchive`; the documented implementation is `Runtime/Core/Private/Internationalization/Text.cpp` ([Epic API, UE 5.8](https://dev.epicgames.com/documentation/unreal-engine/API/Runtime/Core/FText/SerializeText)). The public API page does not publish its field order or bit widths.
- Epic documents string-table text as a reference to an entry in a known table, and documents `FText::FromStringTable` as its C++ construction path ([Epic, String Tables, UE 5.8](https://dev.epicgames.com/documentation/en-us/unreal-engine/using-string-tables-for-text-in-unreal-engine)).
- Epic documents `FFormatArgumentData` as an argument-name/value pair for `FText::Format`, with a type tag and possible `FText`, integer, float, double, or gender values ([Epic API, UE 5.8](https://dev.epicgames.com/documentation/en-us/unreal-engine/API/Runtime/Core/FFormatArgumentData)); `FFormatArgumentValue` also supports signed/unsigned 64-bit numbers, float, double, and nested `FText` ([Epic API, UE 5.8](https://dev.epicgames.com/documentation/en-us/unreal-engine/API/Runtime/Core/FFormatArgumentValue)). These pages establish the domain types, not their replay serialization.

## Strong asset-archive hypothesis, not yet replay proof

The best-fitting interpretation is:

```text
u32 flags
i8  ETextHistory
variant payload
```

with history value `11` = `StringTableEntry`. Its payload is an `FName table_id` plus a string key; on this replay wire the observed `FName` appears to start with the one-bit inline/hardcoded selector already handled elsewhere in `scalar.rs`, so the observed successful layout becomes `32 flags + 8 history + 1 inline bit + FString path + i32 number + FString key`.

This is supported by the byte arithmetic above and by a pinned asset parser implementation: UAssetAPI commit [`3228c1e`](https://github.com/atenfyr/UAssetAPI/tree/3228c1e86261aa08131f7ec0ff1a395f5d0b2a84) assigns `StringTableEntry` after `Transform`, making its ordinal 11 ([enum source](https://github.com/atenfyr/UAssetAPI/blob/3228c1e86261aa08131f7ec0ff1a395f5d0b2a84/UAssetAPI/PropertyTypes/Objects/TextHistoryType.cs)); its reader consumes `uint32 Flags`, signed-byte history, then `FName TableId` and `FString` for that variant ([reader lines 264-290](https://github.com/atenfyr/UAssetAPI/blob/3228c1e86261aa08131f7ec0ff1a395f5d0b2a84/UAssetAPI/PropertyTypes/Objects/TextPropertyData.cs#L264-L290)). UAssetAPI is third-party source implementing **asset** archives, so it corroborates the hypothesis but cannot establish Riot's replay/net serializer or engine-version mapping.

Under that same enum ordering, the h29 byte `3` denotes `ArgumentFormat`, not a new discriminator `1`. The asset parser's shape is nested source-format `FText`, an `i32` argument count, then repeated `FString argument_name + u8 argument_type + typed value` ([lines 300-314 and 73-179](https://github.com/atenfyr/UAssetAPI/blob/3228c1e86261aa08131f7ec0ff1a395f5d0b2a84/UAssetAPI/PropertyTypes/Objects/TextPropertyData.cs#L73-L179)). This is a concrete decoding candidate only. Network/replay serialization may use different FName, count, integer, or version-dependent encodings.

## Competing explanations

1. **Archive-compatible history framing:** `u32 flags + i8 history`; `0x0B` is `StringTableEntry`, and the former `5` is a one-bit-shift artifact. This explains every existing prefix and the already-successful inline `FName` parse.
2. **Replay-specific mapping:** the 41-bit prefix may be a net serializer with different flags or enum mapping that merely aligns with archive serialization in the old population. Official public documentation confirms network use but does not expose this grammar.
3. **h29 is `ArgumentFormat`:** the new `3` population may be a formatted reward label whose source is another `FText`. This predicts nested, recursively valid `FText` at bit 40, then a bounded argument list. A single zero at bit 40 is insufficient proof.

## Required byte-level tests

- Repartition all known samples at `32 + 8`; report flags and history byte without shifting. Require existing decoded string-table rows to be exactly history `11`, and preserve zero residual bits after explicitly reading the FName selector.
- For every h29 history `3` sample, recursively parse an `FText` beginning at bit 40. Then read a bounded count and argument-name/type/value records. Require full consumption, stable bounds, valid UTF strings, and no decoder-specific padding assumptions.
- Cluster h29 by complete payload and bit length. Verify that candidate format strings contain placeholders matched by the parsed argument names, and that type tags predict the exact remaining widths. Reject the grammar on any unexplained residual bits.
- Add negative probes at offsets 32, 40, and 41. A plausible string at one offset is weak evidence; recursive structural validity across independent payloads is the discriminator.
- Record engine/build provenance. Do not promote asset enum ordinals or UAssetAPI field widths to replay facts until the replay corpus validates them.

The current comments calling the shifted value `5` an `ETextHistory` discriminator are therefore unsupported and very likely mislabeled. The decoder's extracted keys remain empirically valid for the old corpus.

## Subsequent replay validation

The [complete reward-text measurement](../TEXT_HISTORY_EXPANSION.md) independently validated every one of 873,709 h29 windows across all 714 exports, including 66,488 formatted histories. The hypotheses above record the investigation's initial state; exact replay consumption now supports the measured 11/3/255 tree shapes. This remains corpus evidence, not a universal engine serialization specification.
