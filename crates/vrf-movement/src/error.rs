use vrf_bitio::BitError;

/// Errors that can occur during movement RPC payload decoding. Exhaustive on
/// purpose: it is public API, and adding `#[non_exhaustive]` or removing a
/// variant would break downstream matches.
#[derive(Debug, Clone, thiserror::Error)]
pub enum MovementError {
    /// The underlying bit reader ran out of data or was malformed.
    #[error("bit read error: {0}")]
    Bit(#[from] BitError),

    #[error("invalid movement magic: 0x{0:02X} (expected 0x52)")]
    InvalidMagic(u8),

    #[error("movement marker mismatch: expected {expected}, got {actual}")]
    MarkerMismatch { expected: u8, actual: u8 },

    /// The move's error sentinel bit was set: the server flagged it invalid.
    #[error("movement error sentinel was set")]
    ErrorSentinel,

    #[error("variant-0 external character reference is not supported")]
    Variant0ExternalCharRef,

    /// The update count exceeded the limit of 256.
    #[error("update count too large: {0}")]
    TooManyUpdates(u32),

    /// A component stream ended before its mandatory u16 framing header.
    #[error("movement component header needs 16 bits, only {available_bits} remain")]
    TruncatedComponentHeader {
        /// Bits available where the u16 header was required.
        available_bits: u64,
    },

    /// Never emitted: an index past the declared count is counted in
    /// `RpcDecodeResult::error_count` instead. Kept for API compatibility.
    #[error("update index {index} out of range (count={count})")]
    UpdateIndexOutOfRange { index: u32, count: u32 },
}
