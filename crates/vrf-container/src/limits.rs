//! Magic numbers, version pins and bounds. The C# reference named in each
//! `Source:` line states every value outright, and none may be adjusted to make
//! a file parse: a bound that has to move is a format discovery.

/// Source: `ReplayInfoReader.cs` -- `FileMagic = 0x43F4EFDD`, at byte 0 of every `.vrf`.
pub(crate) const FILE_MAGIC: u32 = 0x43F4_EFDD;

/// Source: `Constants.cs` -- `NetworkMagic = 0x2CF5A13D`, inside the Header chunk.
pub(crate) const NETWORK_MAGIC: u32 = 0x2CF5_A13D;

/// Source: `LocalFileReplayCustomVersions.cs` -- 7 is the only supported legacy
/// file version; older versions used another layout.
pub(crate) const EXPECTED_FILE_VERSION: u32 = 7;

/// Source: `Constants.cs` -- `ExpectedNetworkVersion = 19`. Checked in the Header
/// chunk only; the info section carries an unrelated value (see
/// `ReplayInfo::network_version`).
pub(crate) const EXPECTED_NETWORK_VERSION: u32 = 19;

/// Source: `Constants.cs` -- `ExpectedEngineNetworkProtocolVersion = 32`.
pub(crate) const EXPECTED_ENGINE_NET_PROTO_VERSION: u32 = 32;

/// Source: `Constants.cs` -- `MaxCustomVersionCount = 1024`.
pub(crate) const MAX_CUSTOM_VERSION_COUNT: i32 = 1024;

/// Source: `Constants.cs` -- `MaxFStringSerializedBytes = 1024 * 1024`.
pub(crate) const MAX_FSTRING_BYTES: i64 = 1024 * 1024;

/// Source: `ReplayInfoReader.cs` -- `MaxEncryptionKeySizeBytes = 4096`.
pub(crate) const MAX_ENCRYPTION_KEY_BYTES: i32 = 4096;

/// Source: `ReplayInfoReader.cs` -- `MaxFriendlyNameSerializedBytes = 64 * 1024`.
/// Tighter than `MAX_FSTRING_BYTES` because the friendly name is user-controlled.
pub(crate) const MAX_FRIENDLY_NAME_BYTES: i64 = 64 * 1024;

/// Source: `ReplayHeaderReader.cs` -- `MaxLevelNamesAndTimes = 1024`.
pub(crate) const MAX_LEVEL_NAMES_AND_TIMES: i32 = 1024;

/// Source: `ReplayHeaderReader.cs` -- `MaxGameSpecificDataEntries = 128`.
pub(crate) const MAX_GAME_SPECIFIC_DATA: i32 = 128;

/// Source: `ReplayDataChunkPayloadReader.cs` -- `MaxChunkSize = 1024 * 1024 * 256`.
/// Bounds the decompressed-output allocation against a corrupt size field.
pub(crate) const MAX_CHUNK_SIZE: i32 = 256 * 1024 * 1024;

/// Source: `ReplayHeaderReader.cs` -- `CustomVersionEntryByteCount = 20`: a
/// 16-byte GUID plus an `i32` version.
pub(crate) const CUSTOM_VERSION_ENTRY_BYTES: u32 = 20;

/// Source: `LocalFileReplayCustomVersions.cs` --
/// `Guid.Parse("95A4F03E-7E0B-49E4-BA43-D35694FF87D9")`, stored as four
/// little-endian `u32` in Unreal's GUID serialisation order.
pub(crate) const LOCAL_REPLAY_GUID: [u32; 4] = [0x95A4_F03E, 0x7E0B_49E4, 0xBA43_D356, 0x94FF_87D9];

/// Source: `LocalFileReplayCustomVersions.cs` -- `CustomVersions = 7`.
pub(crate) const LOCAL_REPLAY_VERSION: i32 = 7;
