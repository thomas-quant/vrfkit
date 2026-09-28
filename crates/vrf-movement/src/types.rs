//! The values the decoder produces.
//!
//! Kept separate from the decoding logic because these cross the crate
//! boundary: the exporter builds Parquet columns directly from
//! [`MovementMove`]'s fields, so their names and units are part of the
//! contract, not an implementation detail.

/// A single decoded movement sample (one "move" from one character update).
#[derive(Debug, Clone, Copy)]
pub struct MovementMove {
    /// The character's network GUID (identifies which player/character).
    pub shooter_character_net_guid: u32,
    /// Position in Unreal world coordinates (cm).
    pub pos_x: f64,
    pub pos_y: f64,
    pub pos_z: f64,
    /// Yaw in degrees [0, 360).
    pub yaw: f64,
    /// Pitch in degrees [0, 360).
    pub pitch: f64,
    /// Velocity (cm/s). Only present for variant-1 moves; zero for variant-0.
    pub vel_x: f64,
    pub vel_y: f64,
    pub vel_z: f64,
    /// Server-assigned timestamp (VLQ-encoded tick).
    pub timestamp: u32,
    /// Movement state byte.
    pub movement_state: u8,
    /// Mode flags byte (same as movement_state in the wire format).
    pub mode_flags: u8,
    /// 0 = variant0, 1 = variant1.
    pub move_type: u8,
}

/// A single character update descriptor (carries moves).
///
/// Retained for callers that construct this public descriptor, even though
/// the decoder currently emits moves directly through its callback.
#[derive(Debug, Clone)]
pub struct MovementUpdate {
    /// Index within the batch.
    pub index: u32,
    /// The character GUID this update belongs to.
    pub shooter_character_net_guid: Option<u32>,
    /// Number of moves decoded for this update.
    pub move_count: u32,
}

/// Result of decoding the full RPC payload.
#[derive(Debug, Clone, Copy, Default)]
pub struct RpcDecodeResult {
    /// Total moves decoded across all updates.
    pub total_moves: u32,
    /// Number of character updates in the batch.
    pub update_count: u32,
    /// Number of decode problems that cost data, counted per occurrence.
    ///
    /// A component stream that fails mid-parse loses the rest of that stream
    /// only: it is a length-delimited field, so decoding goes on with the next
    /// field and update. A framing read that fails inside an update leaves
    /// the bit cursor at an indeterminate position, so the rest of its array
    /// is skipped rather than guessed at. The count is what makes either loss
    /// visible instead of silent.
    ///
    /// It covers the framing anomalies too, and for the same reason: an update
    /// index past the declared count, a field declaring more bits than its
    /// window holds, a shooter-GUID field too narrow to hold a `u32`, a
    /// component stream with no GUID to attribute it to, and a trailing
    /// padding byte that does not parse. None of those can be recovered from
    /// mid-stream, so each one still discards what follows it -- but every one
    /// of them used to return `Ok` with this field at zero, which is
    /// bit-for-bit the shape of a batch of well-formed empty updates. What is
    /// counted here is loss, not severity: one update may contribute more than
    /// one.
    pub error_count: u32,
    /// Movement sections whose window was sized by `movementBitCount` and
    /// that stopped with bits of that window unread.
    ///
    /// A section ends at a 3-bit zero marker, or when at most 31 bits remain
    /// after a move -- the padding the grammar allows, which is not counted
    /// here. Every other stop used to return `Ok` with no trace: a zero
    /// marker read with bits still behind it, a window too short for the
    /// 8-bit magic, or one too short for the first marker. A cursor that has
    /// drifted and happens to read `000` takes the first of those exits, and
    /// every move after it is gone -- the shape `decode_movement_rpc` already
    /// counts one layer up as an early terminator.
    ///
    /// A tally, not an error, and deliberately not part of `error_count`: a
    /// batch with a nonzero `error_count` keeps its whole payload as a raw
    /// row, and how often a tail is legitimate has not been measured. A
    /// window with no bits at all is not counted; the C# reference still
    /// reports "Missing movement magic" for it, but this counts unread bits,
    /// not missing fields.
    pub sized_section_tails: u32,
    /// Bits left unread by the sections counted in
    /// [`Self::sized_section_tails`].
    pub sized_section_tail_bits: u64,
    /// The same, for sections whose window ran to the end of the component
    /// stream because `movementBitCount` was 0 or larger than what remained.
    ///
    /// Kept apart from [`Self::sized_section_tails`] because the two cannot be
    /// read the same way: after a zero marker in an open window, the rest may
    /// be component data that is not movement at all, rather than lost moves.
    pub open_section_tails: u32,
    /// Bits left unread by the sections counted in
    /// [`Self::open_section_tails`].
    pub open_section_tail_bits: u64,
}
