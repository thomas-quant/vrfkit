//! Shared types used across the replication layer.

/// Maximum packet payload in bits: Unreal's `MAX_PACKET_SIZE * 8` (2 KB =
/// 16 384 bits), the `read_serialized_int` bound for a bunch's bit count.
pub const MAX_PACKET_SIZE_BITS: u32 = 2 * 1024 * 8;

/// Maximum recursion depth for `InternalLoadObject`.
pub const MAX_NET_GUID_RECURSION: u32 = 16;

/// Maximum number of GUIDs in a single package-map export bunch.
pub const MAX_GUID_COUNT: u32 = 2048;

/// Maximum number of simultaneously tracked channel indices.
///
/// The reference replay peaks at 232 distinct slots. 4,096 is wide enough for
/// ordinary Unreal channel populations while bounding adversarial IntPacked
/// indices that otherwise create one hash-table entry per bunch forever.
pub const MAX_ACTIVE_CHANNELS: usize = 4_096;

/// Reason a channel was closed by the server, read as `SerializedInt(15)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum ChannelCloseReason {
    /// Actor was destroyed.
    #[default]
    Destroyed = 0,
    /// Actor entered dormancy: still alive, so not a despawn.
    Dormancy = 1,
}

impl ChannelCloseReason {
    /// The `MAX` passed to `read_serialized_int`.
    pub const MAX: u32 = 15;

    /// Any wire value but 1 (Dormancy) reads as Destroyed.
    #[must_use]
    pub fn from_raw(v: u32) -> Self {
        match v {
            1 => Self::Dormancy,
            _ => Self::Destroyed,
        }
    }
}

/// A network GUID as transmitted on the wire: `0` is no object, `1` the
/// default object (its export flags are always read), odd values static
/// (level-placed) actors and even non-zero values dynamic (spawned) ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct NetworkGuid(pub u32);

impl NetworkGuid {
    #[must_use]
    #[inline]
    pub const fn is_valid(self) -> bool {
        self.0 != 0
    }

    #[must_use]
    #[inline]
    pub const fn is_default(self) -> bool {
        self.0 == 1
    }

    #[must_use]
    #[inline]
    pub const fn is_dynamic(self) -> bool {
        self.is_valid() && (self.0 & 1) == 0
    }
}

/// Flags read after a net GUID when exporting path information.
///
/// ```text
/// Bit layout: 1 byte (8 bits), only low 3 meaningful
///   bit 0 -- HasPath
///   bit 1 -- NoLoad
///   bit 2 -- HasNetworkChecksum
/// ```
#[derive(Debug, Clone, Copy)]
pub struct ExportFlags(pub u8);

impl ExportFlags {
    pub const HAS_PATH: u8 = 1 << 0;
    pub const NO_LOAD: u8 = 1 << 1;
    pub const HAS_NETWORK_CHECKSUM: u8 = 1 << 2;

    #[must_use]
    #[inline]
    pub const fn has_path(self) -> bool {
        self.0 & Self::HAS_PATH != 0
    }

    #[must_use]
    #[inline]
    pub const fn has_network_checksum(self) -> bool {
        self.0 & Self::HAS_NETWORK_CHECKSUM != 0
    }
}

/// 3D vector as decoded from the spawn data.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FVector {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

/// Rotation as decoded from compressed short format.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FRotator {
    pub pitch: f32,
    pub yaw: f32,
    pub roll: f32,
}
