//! Fixed-width reads shared by the chunk parsers. A short read becomes
//! [`ContainerError::Truncated`] naming the field and the bytes left.

use vrf_bitio::BitReader;

use crate::error::ContainerError;

/// Little-endian `u32` at `at`; every caller has already checked the length.
pub(crate) fn le_u32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

/// Bytes still available to the reader, for the `available` field of
/// [`ContainerError::Truncated`].
fn bytes_left(reader: &BitReader<'_>) -> usize {
    (reader.bits_remaining() / 8) as usize
}

pub(crate) fn read_u32(
    reader: &mut BitReader<'_>,
    context: &'static str,
) -> Result<u32, ContainerError> {
    reader.read_u32().map_err(|_| ContainerError::Truncated {
        context,
        needed: 4,
        available: bytes_left(reader),
    })
}

pub(crate) fn read_i32(
    reader: &mut BitReader<'_>,
    context: &'static str,
) -> Result<i32, ContainerError> {
    reader.read_i32().map_err(|_| ContainerError::Truncated {
        context,
        needed: 4,
        available: bytes_left(reader),
    })
}

/// `max_bytes` is a parameter rather than [`crate::limits::MAX_FSTRING_BYTES`]
/// because `info` reads one string under a much tighter bound
/// (`MAX_FRIENDLY_NAME_BYTES`); the other three pass the general limit.
pub(crate) fn read_fstring(
    reader: &mut BitReader<'_>,
    context: &'static str,
    max_bytes: i64,
) -> Result<String, ContainerError> {
    reader
        .read_fstring(max_bytes)
        .map_err(|source| ContainerError::FString { context, source })
}
