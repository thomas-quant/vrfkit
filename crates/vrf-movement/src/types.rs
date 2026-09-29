//! The values the decoder produces. The exporter builds Parquet columns
//! straight from [`MovementMove`], so its field names and units are a contract.

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

/// A single character update descriptor. The decoder never builds one (moves
/// go out through its callback); kept for callers that construct it.
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
    /// Decode problems that cost data, counted per occurrence (loss, not
    /// severity: one update may add several). A failed component stream loses
    /// only itself, being length-delimited; a failed framing read loses the
    /// rest of its updates array, since the cursor is then lost. The framing
    /// anomalies each discard what follows them: an index past the declared
    /// count, a field longer than its window, a shooter-GUID field not 32
    /// bits wide, a stream with no GUID, bits after the array's zero index
    /// other than one IntPacked byte. Uncounted, any of these looks like
    /// well-formed empty updates.
    pub error_count: u32,
    /// Sections in a window sized by `movementBitCount` that stopped with bits
    /// of it unread: at a zero marker with bits behind it, or in a window too
    /// short for the 8-bit magic or the first marker. A drifted cursor that
    /// reads `000` stops here and loses every move after it.
    ///
    /// Not counted: an empty window (no movement magic, but this counts unread
    /// bits), and the end at most 31 bits
    /// after a move. Those bits are not padding but a `000` terminator and 8
    /// to 23 bits that are not all zero, so a zero tally means no section
    /// stopped anywhere else, not that every section was read to its last
    /// bit. No measured stream has a sized window at all (crate docs,
    /// "Measured on real replays").
    ///
    /// A tally, not part of `error_count`: a nonzero `error_count` keeps the
    /// batch's whole payload as a raw row.
    pub sized_section_tails: u32,
    /// Bits left unread by the sections counted in [`Self::sized_section_tails`].
    pub sized_section_tail_bits: u64,
    /// The same, for sections whose window ran to the end of the component
    /// stream (`movementBitCount` 0 or larger than what remained). Kept apart:
    /// after a zero marker in an open window, the rest may be component data
    /// rather than lost moves.
    pub open_section_tails: u32,
    /// Bits left unread by the sections counted in [`Self::open_section_tails`].
    pub open_section_tail_bits: u64,
    /// Byte-wrapped component streams, each counted once its envelope is cut
    /// out, before the section inside is parsed (a failed section still
    /// counts). Counted whether or not bits follow the envelope, so a trailer
    /// that vanished reads as [`Self::envelope_trailer_bits`] short of 24 per
    /// stream, not as streams that were never wrapped.
    pub envelope_trailer_streams: u32,
    /// Bits after those envelopes, which nothing reads: exactly 24 per stream
    /// on every measured replay (crate
    /// docs, "Measured on real replays"). A tally like the section tails.
    pub envelope_trailer_bits: u64,
}
