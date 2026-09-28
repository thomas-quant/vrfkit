//! Fixed-width reads shared by the chunk parsers. A short read becomes
//! [`ContainerError::Truncated`] naming the field and the bytes left.

use vrf_bitio::BitReader;

use crate::error::ContainerError;

/// Little-endian `u32` at `at`; every caller has already checked the length.
pub(crate) fn le_u32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

/// A read of `needed` bytes for `context` that ran out, with the bytes still
/// left to the reader.
fn truncated(reader: &BitReader<'_>, context: &'static str, needed: usize) -> ContainerError {
    ContainerError::Truncated {
        context,
        needed,
        available: (reader.bits_remaining() / 8) as usize,
    }
}

pub(crate) fn read_u16(
    reader: &mut BitReader<'_>,
    context: &'static str,
) -> Result<u16, ContainerError> {
    reader.read_u16().map_err(|_| truncated(reader, context, 2))
}

pub(crate) fn read_u32(
    reader: &mut BitReader<'_>,
    context: &'static str,
) -> Result<u32, ContainerError> {
    reader.read_u32().map_err(|_| truncated(reader, context, 4))
}

pub(crate) fn read_i32(
    reader: &mut BitReader<'_>,
    context: &'static str,
) -> Result<i32, ContainerError> {
    read_u32(reader, context).map(|v| v as i32)
}

pub(crate) fn read_f32(
    reader: &mut BitReader<'_>,
    context: &'static str,
) -> Result<f32, ContainerError> {
    read_u32(reader, context).map(f32::from_bits)
}

/// Two `u32` halves, so a short read reports the 4-byte half that failed,
/// the width every other `Truncated` in this crate reports.
pub(crate) fn read_i64(
    reader: &mut BitReader<'_>,
    context: &'static str,
) -> Result<i64, ContainerError> {
    let lo = read_u32(reader, context)?;
    let hi = read_u32(reader, context)?;
    Ok(i64::from(lo) | (i64::from(hi) << 32))
}

/// An Unreal GUID: four little-endian `u32`.
pub(crate) fn read_guid(reader: &mut BitReader<'_>) -> Result<[u32; 4], ContainerError> {
    Ok([
        read_u32(reader, "guid")?,
        read_u32(reader, "guid")?,
        read_u32(reader, "guid")?,
        read_u32(reader, "guid")?,
    ])
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
