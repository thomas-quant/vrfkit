//! The numeric primitives of a move record. Zero error on yaw, pitch and
//! velocity and at most 0.0005 on position against an independent parser: every
//! constant, width and rounding step is wire layout, so do not restyle it.

use vrf_bitio::BitReader;

use crate::error::MovementError;

/// Angle conversion: raw u16 -> degrees.
pub(crate) const ANGLE_SCALE: f64 = 360.0 / 65536.0;

/// [`BitReader::read_quantized_vector`] as a tuple.
pub(crate) fn read_quantized_vector(
    reader: &mut BitReader<'_>,
    scale: u32,
) -> Result<(f64, f64, f64), MovementError> {
    let [x, y, z] = reader.read_quantized_vector(scale)?;
    Ok((x, y, z))
}

/// For the width test in `tests.rs`, which reaches the assert through here.
#[cfg(test)]
pub(crate) fn read_signed_quantized_components(
    reader: &mut BitReader<'_>,
    component_bits: u32,
) -> Result<[i64; 3], MovementError> {
    Ok(reader.read_quantized_components(component_bits)?)
}
