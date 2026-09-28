//! The numeric primitives a move record is built from.
//!
//! # These are format, not style
//!
//! Every constant, width and rounding step below is validated against the C#
//! reference to **zero** error on yaw, pitch and velocity and a maximum of
//! 0.0005 on position. The `SerializedInt(128)` header on a QuantizedVector
//! and the sign-extension of arbitrary-width components are wire layout. So
//! are the 25-bit move header, the 48-bit FixedVector it skips and the VLQ
//! timestamp that [`crate::moves`] reads alongside them. Rewriting any of the
//! arithmetic here -- even into a form that looks equivalent -- changes
//! decoded output, so it is left exactly as validated.

use vrf_bitio::BitReader;

use crate::error::MovementError;

/// Angle conversion: raw u16 -> degrees.
pub(crate) const ANGLE_SCALE: f64 = 360.0 / 65536.0;

/// Read a QuantizedVector with the given scale factor.
///
/// ## Wire format
///
/// ```text
/// componentBitCountAndExtraInfo : SerializedInt(128)  -- 7 bits via serialized_int
///   componentBits = value & 63
///   extraInfo = value >> 6
///
/// IF componentBits > 0:
///   3 signed components of `componentBits` bits each
///   IF extraInfo > 0: divide each by scaleFactor
/// ELIF extraInfo == 0:
///   3 x f32 (raw IEEE-754)
/// ELSE:
///   3 x f64 (raw IEEE-754)
/// ```
pub(crate) fn read_quantized_vector(
    reader: &mut BitReader<'_>,
    scale_factor: i32,
) -> Result<(f64, f64, f64), MovementError> {
    // SerializedInt(128) -- uses up to 7 bits.
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
        // 3 x f32
        let x = f64::from(reader.read_f32()?);
        let y = f64::from(reader.read_f32()?);
        let z = f64::from(reader.read_f32()?);
        Ok((x, y, z))
    } else {
        // 3 x f64
        let x = reader.read_f64()?;
        let y = reader.read_f64()?;
        let z = reader.read_f64()?;
        Ok((x, y, z))
    }
}

/// Read 3 signed components of `component_bits` bits each.
///
/// When the total (3 x component_bits) fits in 64 bits, all three are packed
/// into a single read. Otherwise they are read individually.
pub(crate) fn read_signed_quantized_components(
    reader: &mut BitReader<'_>,
    component_bits: u32,
) -> Result<(i64, i64, i64), MovementError> {
    // `componentBits` is `info & 63`, so 63 is the largest width the header can
    // express -- and it is fully readable: `read_bits` takes up to 64 and
    // `sign_extend`'s sign bit lands at `1 << 62`. The bound used to be 62,
    // which made a declared width of 63 return `(0, 0, 0)` *without consuming
    // the 189 bits it declared*. That is the worst available answer: a
    // world-origin position that looks like a real sample, a move that reports
    // no error, and a cursor left 189 bits behind so everything after it
    // decodes from the wrong offset. Zero is the only width that legitimately
    // reads nothing, and the caller already handles it.
    // A real assert, not a `debug_assert`: `[profile.release]` does not enable
    // debug assertions, so a debug-only check would be absent from exactly the
    // binary that exports the corpus. Above 64 this is not a wrong answer but
    // an out-of-range shift -- `mask_u64(65)` is `u64::MAX >> (64 - 65)`,
    // which release masks into a nonsense mask instead of trapping.
    //
    // Panicking rather than returning an error is deliberate and follows
    // `copy_bits_to`'s precedent for the same shape: the only caller derives
    // this width as `info & 63` and handles 0 itself, so any other value is a
    // bug at the call site and not malformed input, and a recoverable error
    // would imply the wire can produce it. Once per vector header.
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

/// Read a single signed component of `bits` width.
fn read_signed_component(reader: &mut BitReader<'_>, bits: u32) -> Result<i64, MovementError> {
    let raw = reader.read_bits(bits)?;
    Ok(sign_extend(raw, bits))
}

/// Sign-extend a `bit_count`-wide unsigned value to i64.
#[inline]
fn sign_extend(raw: u64, bit_count: u32) -> i64 {
    let sign_bit = 1u64 << (bit_count - 1);
    (raw ^ sign_bit).wrapping_sub(sign_bit) as i64
}
