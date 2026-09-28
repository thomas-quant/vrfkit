# Upstream value declaration recheck [ARCHIVED -- HISTORICAL]

Checked 2026-09-09 against vrfkit `14e58e52e8f412927c6bde1909abd89e38f5d2b6`.
Only source owned by `michel-giehl/ValorantReplayParser` and that repository's
pull-request commits was treated as declaration evidence.

## Result

There is no new upstream declaration that establishes the signedness of
`AuthInitialRandomSeed`, the action meanings inside `InputEventData`, or the
members of `StopEffectType`.

The upstream `main` branch is still exactly
[`b51d67423b7b4952d59051cf91e55efa1c42da05`](https://github.com/michel-giehl/ValorantReplayParser/commit/b51d67423b7b4952d59051cf91e55efa1c42da05),
the same commit pinned in `REFERENCE_REPOSITORIES.md`. Its commit timestamp is
2026-09-02T12:06:39Z. A fetch of every pull-request head found only PR #1 and
PR #5; neither supplies any newer declaration for the three targets.

## Target-by-target evidence

- `AuthInitialRandomSeed` does not occur in upstream `main` or either fetched
  PR head. The nearby `CosmeticRandomSeed` declaration is explicitly `uint`,
  but it belongs to a different property and descriptor
  ([`CageTrapAbilityDescriptor.cs`, lines 15–26](https://github.com/michel-giehl/ValorantReplayParser/blob/b51d67423b7b4952d59051cf91e55efa1c42da05/src/Replay.Valorant/Descriptors/Agents/Gumshoe/CageTrapAbilityDescriptor.cs#L15-L26)).
  It cannot establish `AuthInitialRandomSeed` signedness. The existing vrfkit
  evidence therefore remains correctly unresolved between `i32` and `u32`.

- In merged PR #1, `InputEventData` was explicitly declared as `byte[]?` and
  bound to `ByteArrayOrRaw(MaxInputEventBytes)`, but that decoder is opaque
  byte storage with a raw fallback, not evidence for this RPC's wire framing
  or for any tag or action enum
  ([`BaseReplayControllerClassNetCacheDescriptor.cs`, lines 38–54](https://github.com/michel-giehl/ValorantReplayParser/blob/8e8768aca1943c319aef7c7f4bcc85d65a2f0649/src/Replay.Valorant/Descriptors/BaseReplayControllerClassNetCacheDescriptor.cs#L38-L54)).
  Upstream subsequently removed that RPC as telemetry in
  [`2b9b9e9`](https://github.com/michel-giehl/ValorantReplayParser/commit/2b9b9e9e85c8cf76bca63ab697dd24f1d2b7e01c),
  before the pinned commit. Current `main` retains only a generic `ByteArray`
  decoder test: the name is assigned to `FieldDecodeContext`, then three
  arbitrary bytes are decoded
  ([`PrimitiveDecodersScalarTests.cs`, lines 65–84](https://github.com/michel-giehl/ValorantReplayParser/blob/b51d67423b7b4952d59051cf91e55efa1c42da05/tests/Replay.Unreal.Tests/Parsing/PrimitiveDecodersScalarTests.cs#L65-L84)).
  Neither the historical storage declaration nor the surviving generic test
  establishes this RPC's wire envelope or its internal tag/action meanings.

- `StopEffectType` does not occur in upstream `main` or either PR head. The
  ReplayEffect class-net-cache registers only the RPC names, including
  `MulticastStopContinuousEffect`
  ([`ReplayEffectComponentClassNetCacheDescriptor.cs`, lines 7–29](https://github.com/michel-giehl/ValorantReplayParser/blob/b51d67423b7b4952d59051cf91e55efa1c42da05/src/Replay.Valorant/Descriptors/Effects/Replay/ReplayEffectComponentClassNetCacheDescriptor.cs#L7-L29)).
  That function-name registration does not declare its parameter enum or
  ordinal mapping.

## Pull requests and newly named raw payloads

Closed PR [#1](https://github.com/michel-giehl/ValorantReplayParser/pull/1)
was merged on 2026-07-18 and is already contained in the pinned `main` commit.
It therefore adds nothing beyond the source already reviewed and already
compared with vrfkit.

Open PR [#5](https://github.com/michel-giehl/ValorantReplayParser/pull/5) remains
at [`ce4f62a1a47207646cbc02cd4d9c85cb7386cc9d`](https://github.com/michel-giehl/ValorantReplayParser/tree/ce4f62a1a47207646cbc02cd4d9c85cb7386cc9d),
last updated 2026-08-21. Its three changed files add
`OwnerExclusivePlayerInfo` and `AresPlayerRoundInfo`; the decoder reads handles
40–44 as `Int32`
([`AresPlayerRoundInfo.cs`, lines 8-22](https://github.com/michel-giehl/ValorantReplayParser/blob/ce4f62a1a47207646cbc02cd4d9c85cb7386cc9d/src/Replay.Valorant/GameState/AresPlayerRoundInfo.cs#L8-L22)
and [lines 81-113](https://github.com/michel-giehl/ValorantReplayParser/blob/ce4f62a1a47207646cbc02cd4d9c85cb7386cc9d/src/Replay.Valorant/GameState/AresPlayerRoundInfo.cs#L81-L113)).
Those fields and the array framing are already implemented in vrfkit. The PR
contains no `AuthInitialRandomSeed` or `StopEffectType` occurrence and no
Valorant binding or action enum for `InputEventData`.

The upstream source still uses `ValorantRawPayload(typeName, bitCount, data)`
as an opaque preservation object
([`ValorantRawPayload.cs`, lines 1–6](https://github.com/michel-giehl/ValorantReplayParser/blob/b51d67423b7b4952d59051cf91e55efa1c42da05/src/Replay.Valorant/Descriptors/ValorantRawPayload.cs#L1-L6)).
Consequently, names passed to `RawPayload(...)` describe intended payload
labels and do not establish a C# value type, enum members, or wire grammar.

No additional newly declared raw type in the current upstream or PR heads is a
clear, unimplemented vrfkit value-type candidate beyond the Harbor/ProfileName
and round/combat work already recorded in `REFERENCE_REPOSITORIES.md` and now
present in the main codebase.
