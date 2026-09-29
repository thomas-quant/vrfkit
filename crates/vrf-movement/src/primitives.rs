//! The numeric primitives of a move record. Zero error on yaw, pitch and
//! velocity and at most 0.0005 on position against an independent parser: every
//! constant, width and rounding step is wire layout, so do not restyle it.

use vrf_bitio::BitReader;

use crate::error::MovementError;

/// Angle conversion: raw u16 -> degrees.
pub(crate) const ANGLE_SCALE: f64 = 360.0 / 65536.0;

/// Read a QuantizedVector with the given scale factor (layout: crate docs,
/// "QuantizedVector").
pub(crate) fn read_quantized_vector(
    reader: &mut BitReader<'_>,
    scale_factor: i32,
) -> Result<(f64, f64, f64), MovementError> {
    let info = u64::from(reader.read_serialized_int(128)?);
    let component_bits = (info & 63) as u32;
    let extra_info = info >> 6;

    if component_bits > 0 {
        let (x, y, z) = read_signed_quantized_components(reader, component_bits)?;
        if extra_info > 0 {
            let sf = f64::from(scale_factor);
            Ok((x as f64 / sf, y as f64 / sf, z as f64 / sf))
        } else {
            Ok((x as f64, y as f64, z as f64))
        }
    } else if extra_info == 0 {
        let x = f64::from(reader.read_f32()?);
        let y = f64::from(reader.read_f32()?);
        let z = f64::from(reader.read_f32()?);
        Ok((x, y, z))
    } else {
        let x = reader.read_f64()?;
        let y = reader.read_f64()?;
        let z = reader.read_f64()?;
        Ok((x, y, z))
    }
}

/// Read 3 signed components of `component_bits` bits each: one read when all
/// three fit in 64 bits, one read each otherwise.
pub(crate) fn read_signed_quantized_components(
    reader: &mut BitReader<'_>,
    component_bits: u32,
) -> Result<(i64, i64, i64), MovementError> {
    // 63, the widest `info & 63` allows, must read its 189 bits. A real assert
    // (release has no debug assertions; above 64 the shift is out of range), and
    // a panic, not an error: the only caller masks the width and handles 0, so
    // any other value is a call-site bug.
    assert!(
        (1..=63).contains(&component_bits),
        "component_bits must be 1..=63, got {component_bits}"
    );

    let total_bits = component_bits * 3;

    if total_bits <= 64 {
        let raw = reader.read_bits(total_bits)?;
        let mask = (1u64 << component_bits) - 1;
        let x = sign_extend(raw & mask, component_bits);
        let y = sign_extend((raw >> component_bits) & mask, component_bits);
        let z = sign_extend((raw >> (component_bits * 2)) & mask, component_bits);
        Ok((x, y, z))
    } else {
        let x = read_signed_component(reader, component_bits)?;
        let y = read_signed_component(reader, component_bits)?;
        let z = read_signed_component(reader, component_bits)?;
        Ok((x, y, z))
    }
}

fn read_signed_component(reader: &mut BitReader<'_>, bits: u32) -> Result<i64, MovementError> {
    let raw = reader.read_bits(bits)?;
    Ok(sign_extend(raw, bits))
}

#[inline]
fn sign_extend(raw: u64, bit_count: u32) -> i64 {
    let sign_bit = 1u64 << (bit_count - 1);
    (raw ^ sign_bit).wrapping_sub(sign_bit) as i64
}
