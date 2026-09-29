//! Magic numbers, version pins and bounds. None may be adjusted to make a file
//! parse: a bound that has to move is a format discovery.

/// The file magic, at byte 0 of every `.vrf`.
pub(crate) const FILE_MAGIC: u32 = 0x43F4_EFDD;

/// The network magic, at the start of the Header chunk.
pub(crate) const NETWORK_MAGIC: u32 = 0x2CF5_A13D;

/// 7 is the only supported legacy file version; older versions used another
/// layout.
pub(crate) const EXPECTED_FILE_VERSION: u32 = 7;

/// Checked in the Header chunk only; the info section carries an unrelated value
/// (see `ReplayInfo::network_version`).
pub(crate) const EXPECTED_NETWORK_VERSION: u32 = 19;

/// The engine network protocol version, checked in the Header chunk.
pub(crate) const EXPECTED_ENGINE_NET_PROTO_VERSION: u32 = 32;

/// Custom-version entries per list, in the info section and the Header chunk.
pub(crate) const MAX_CUSTOM_VERSION_COUNT: i32 = 1024;

/// Serialized bytes of one FString; every reader but the friendly name's.
pub(crate) const MAX_FSTRING_BYTES: i64 = 1024 * 1024;

/// Encryption key length in the info section.
pub(crate) const MAX_ENCRYPTION_KEY_BYTES: i32 = 4096;

/// Serialized bytes of the friendly name. Tighter than `MAX_FSTRING_BYTES`
/// because the friendly name is user-controlled.
pub(crate) const MAX_FRIENDLY_NAME_BYTES: i64 = 64 * 1024;

/// Level name/time entries in the Header chunk.
pub(crate) const MAX_LEVEL_NAMES_AND_TIMES: i32 = 1024;

/// Game-specific data entries in the Header chunk.
pub(crate) const MAX_GAME_SPECIFIC_DATA: i32 = 128;

/// A chunk's declared decompressed size. Bounds the decompressed-output
/// allocation against a corrupt size field.
pub(crate) const MAX_CHUNK_SIZE: i32 = 256 * 1024 * 1024;

/// One custom-version entry in the Header chunk: a 16-byte GUID plus an `i32`
/// version.
pub(crate) const CUSTOM_VERSION_ENTRY_BYTES: u32 = 20;

/// The local-file replay custom version's GUID,
/// `95A4F03E-7E0B-49E4-BA43-D35694FF87D9`, stored as four little-endian `u32`
/// in Unreal's GUID serialisation order.
pub(crate) const LOCAL_REPLAY_GUID: [u32; 4] = [0x95A4_F03E, 0x7E0B_49E4, 0xBA43_D356, 0x94FF_87D9];

/// The version the info section must carry under `LOCAL_REPLAY_GUID`.
pub(crate) const LOCAL_REPLAY_VERSION: i32 = 7;
