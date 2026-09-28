//! Error types for the DemoFrame layer.

use thiserror::Error;

/// A chunk malformed at the framing level; content-block errors live in
/// `vrf-net`.
#[derive(Debug, Error)]
pub enum FrameError {
    /// A bit read failed (truncation or malformed primitive).
    #[error("bit-IO error during frame parsing: {0}")]
    Bit(String),

    /// A schema-reader error (net-field export or export-GUID parsing).
    #[error("schema error during frame parsing: {0}")]
    Schema(String),

    /// Packet size declared as negative.
    #[error("negative packet size: {size}")]
    NegativePacketSize { size: i32 },

    /// Packet size exceeds the protocol maximum (2 KiB).
    #[error("packet size {size} exceeds maximum {max}")]
    PacketTooLarge { size: i32, max: i32 },

    /// A finite `timeSeconds` outside the `u32` millisecond range; see
    /// [`walk_demo_frames`](crate::walk_demo_frames).
    #[error("frame time {seconds} s is outside the representable millisecond range")]
    TimeOutOfRange { seconds: f32 },

    /// The data was truncated mid-frame.
    #[error("{context}: needed {needed} bytes, only {available} available")]
    Truncated {
        context: &'static str,
        needed: usize,
        available: usize,
    },
}

// `Bit` and `Schema` hold the rendered string, not the source error, so a
// vrf-bitio or vrf-schema error-shape change is not a breaking change here.
impl From<vrf_bitio::BitError> for FrameError {
    fn from(e: vrf_bitio::BitError) -> Self {
        Self::Bit(e.to_string())
    }
}

impl From<vrf_schema::SchemaError> for FrameError {
    fn from(e: vrf_schema::SchemaError) -> Self {
        Self::Schema(e.to_string())
    }
}
