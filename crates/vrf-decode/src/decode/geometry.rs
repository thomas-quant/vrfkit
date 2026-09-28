//! Vector, rotator, transform and replicated-movement readers.
//!
//! These all render into `value_str` -- there is no numeric column that can
//! hold three components -- so each one is a bit read followed by a `Display`
//! of the model type in [`crate::types`].

use vrf_bitio::{BitError, BitReader};

use super::{DecodeError, DecodedValue, render};
use crate::types::{
    FQuat, FRepMovement, FRotator, FTransform, FVector, RotatorQuantization, VectorQuantization,
};

pub(super) fn decode_vector_net_quantize(
    r: &mut BitReader<'_>,
    scale: u32,
) -> Result<DecodedValue, DecodeError> {
    if scale == 0 {
        return Err(DecodeError::InvalidQuantizationScale { scale });
    }
    Ok(render(read_quantized_vector(r, scale)?))
}

// -- Shared vector/rotation reading functions ---------------------------------

pub(super) fn read_float_vector(r: &mut BitReader<'_>) -> Result<FVector, BitError> {
    Ok(FVector {
        x: f64::from(r.read_f32()?),
        y: f64::from(r.read_f32()?),
        z: f64::from(r.read_f32()?),
    })
}

pub(super) fn read_double_vector(r: &mut BitReader<'_>) -> Result<FVector, BitError> {
    Ok(FVector {
        x: r.read_f64()?,
        y: r.read_f64()?,
        z: r.read_f64()?,
    })
}

/// Read a quantized vector: 7-bit header encodes component bit count + extra info.
///
/// ```text
/// header = SerializedInt(128)
///   bits [5:0] = componentBitCount
///   bit  [6]   = extraInfo (1 = scaled integer, 0 = float/double fallback)
///
/// if componentBitCount > 0:
///   3 x componentBitCount bits, two's-complement, sign-extended from
///   componentBitCount bits (NOT sign-magnitude: e.g. all-ones reads as -1,
///   not -(max)). See read_packed_quantized_vector below.
///   if extraInfo: divide by scaleFactor
/// else:
///   if extraInfo == 0: 3 x f32 (float vector)
///   else:              3 x f64 (double vector)
/// ```
fn read_quantized_vector(r: &mut BitReader<'_>, scale_factor: u32) -> Result<FVector, DecodeError> {
    let header = r.read_serialized_int(1 << 7)?;
    let component_bit_count = header & 63;
    let extra_info = header >> 6;

    if component_bit_count > 0 {
        return Ok(read_packed_quantized_vector(
            r,
            component_bit_count,
            extra_info,
            scale_factor,
        )?);
    }
    let v = if extra_info == 0 {
        read_float_vector(r)?
    } else {
        read_double_vector(r)?
    };
    // Only this fallback branch needs finite_vector: the packed path above is
    // an integer quotient of a non-zero scale and finite by construction,
    // but this one reads raw IEEE-754 words that can be NaN or infinity. See
    // FRepMovement's Display impl in types.rs for why that distinction
    // matters.
    finite_vector(v, "quantized vector")
}

/// Pass an [`FVector`] through, or reject it if any component is not finite.
fn finite_vector(v: FVector, context: &'static str) -> Result<FVector, DecodeError> {
    if v.x.is_finite() && v.y.is_finite() && v.z.is_finite() {
        Ok(v)
    } else {
        Err(DecodeError::NonFiniteComponent { context })
    }
}

fn read_packed_quantized_vector(
    r: &mut BitReader<'_>,
    component_bit_count: u32,
    extra_info: u32,
    scale_factor: u32,
) -> Result<FVector, BitError> {
    let x_raw = r.read_bits(component_bit_count)?;
    let y_raw = r.read_bits(component_bit_count)?;
    let z_raw = r.read_bits(component_bit_count)?;
    let sign_bit = 1u64 << (component_bit_count - 1);

    let fx = (x_raw ^ sign_bit) as i64 - sign_bit as i64;
    let fy = (y_raw ^ sign_bit) as i64 - sign_bit as i64;
    let fz = (z_raw ^ sign_bit) as i64 - sign_bit as i64;

    let (x, y, z) = if extra_info > 0 {
        let sf = f64::from(scale_factor);
        (fx as f64 / sf, fy as f64 / sf, fz as f64 / sf)
    } else {
        (fx as f64, fy as f64, fz as f64)
    };

    Ok(FVector { x, y, z })
}

/// Fixed-point normal vector: 3 x SerializedInt(65536), bias = 32768, scale = 32767.
pub(super) fn read_fixed_vector_normal(r: &mut BitReader<'_>) -> Result<FVector, BitError> {
    const BIAS: i32 = 1 << 15;
    const SCALE: f64 = (BIAS - 1) as f64;
    const MAX: u32 = 1 << 16;

    let dx = r.read_serialized_int(MAX)?;
    let dy = r.read_serialized_int(MAX)?;
    let dz = r.read_serialized_int(MAX)?;

    Ok(FVector {
        x: (dx as i32 - BIAS) as f64 / SCALE,
        y: (dy as i32 - BIAS) as f64 / SCALE,
        z: (dz as i32 - BIAS) as f64 / SCALE,
    })
}

/// A compressed rotator: three components of `width` bits, 16 for the short
/// form and 8 for the byte form. The degree scale `360 / 2^width` divides by
/// a power of two, so it is exact in `f32` at both widths.
pub(super) fn read_rotation(r: &mut BitReader<'_>, width: u32) -> Result<FRotator, BitError> {
    let scale = 360.0 / (1u32 << width) as f32;
    let pitch = read_compressed_rotation_component(r, width, scale)?;
    let yaw = read_compressed_rotation_component(r, width, scale)?;
    let roll = read_compressed_rotation_component(r, width, scale)?;
    Ok(FRotator { pitch, yaw, roll })
}

/// A presence bit followed by an unsigned component of `width` bits, scaled
/// to degrees.
fn read_compressed_rotation_component(
    r: &mut BitReader<'_>,
    width: u32,
    scale: f32,
) -> Result<f32, BitError> {
    if r.read_bit()? {
        let v = r.read_bits(width)?;
        Ok(v as f32 * scale)
    } else {
        Ok(0.0)
    }
}

fn read_quaternion(r: &mut BitReader<'_>) -> Result<FQuat, BitError> {
    Ok(FQuat {
        x: r.read_f32()?,
        y: r.read_f32()?,
        z: r.read_f32()?,
        w: r.read_f32()?,
    })
}

pub(super) fn read_transform(r: &mut BitReader<'_>) -> Result<FTransform, BitError> {
    let rotation = read_quaternion(r)?;
    let translation = read_float_vector(r)?;
    let scale = read_float_vector(r)?;
    Ok(FTransform {
        rotation,
        translation,
        scale,
    })
}

pub(super) fn read_rep_movement(
    r: &mut BitReader<'_>,
    rotation_quant: RotatorQuantization,
    location_quant: VectorQuantization,
) -> Result<FRepMovement, DecodeError> {
    let simulated_physics_sleep = r.read_bit()?;
    let rep_physics = r.read_bit()?;
    let rep_server_frame = r.read_bit()?;
    let rep_server_handle = r.read_bit()?;

    // The divisor is the table entry's, never a constant here. The header's
    // extra-info bit says only that the integer was scaled; Unreal packs
    // round(world * scale) for the sending class's LocationQuantizationLevel,
    // and classes differ. This read used a literal 100 (the C# reference's
    // VectorNetQuantize100), which returned world/100 on every whole-unit
    // class: measured against actors.parquet spawn positions, 25 of the 26
    // classes the table declares pack whole units and one packs two decimals.
    // Bit consumption does not depend on the divisor, so the wrong constant
    // raised no error and moved no counter. See docs/DATA.md.
    let location = read_quantized_vector(r, location_quant.scale())?;
    let rotation = match rotation_quant {
        RotatorQuantization::ByteComponents => read_rotation(r, 8)?,
        RotatorQuantization::ShortComponents => read_rotation(r, 16)?,
    };
    // Whole units: Unreal's default VelocityQuantizationLevel, and what the
    // wire shows wherever the level is observable -- the displacement between
    // consecutive updates, over dt, over the reported speed, has a median
    // between 0.97 and 1.06 on each of the 18 typed classes that move. It is
    // not observable on the one two-decimal class
    // (Pawn_Aggrobot_SeekerNade_C): all 932 of its velocities are the zero
    // vector, which reads the same at any divisor.
    let linear_velocity = read_quantized_vector(r, 1)?;

    let angular_velocity = if rep_physics {
        Some(read_quantized_vector(r, 1)?)
    } else {
        None
    };

    let server_frame = if rep_server_frame {
        Some(r.read_int_packed()?)
    } else {
        None
    };

    let server_physics_handle = if rep_server_handle {
        Some(r.read_int_packed()?)
    } else {
        None
    };

    Ok(FRepMovement {
        location,
        rotation,
        linear_velocity,
        angular_velocity,
        simulated_physics_sleep,
        rep_physics,
        server_frame,
        server_physics_handle,
    })
}
