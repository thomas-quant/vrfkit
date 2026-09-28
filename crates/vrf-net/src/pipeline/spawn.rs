//! Dynamic-actor spawn data: archetype, level, transform and velocity.
//!
//! Open-count on the reference replay: docs/PERFORMANCE_NOTES.md#measured-rates-reference-replay-02d4d478.
//!
//! The block Unreal writes right after the actor GUID when a channel opens for
//! a *dynamic* (even, non-zero GUID) actor: small and rare, but its bit width
//! decides everything after it in the same bunch.

use vrf_bitio::BitReader;

use crate::error::Result;
use crate::net_guid::{self, GuidPathSink};
use crate::types::{FRotator, FVector};

use super::ActorChannelState;

/// Default spawn location and velocity when the wire omits them.
const ORIGIN: FVector = FVector {
    x: 0.0,
    y: 0.0,
    z: 0.0,
};

/// Default spawn scale when the wire omits it.
const UNIT_SCALE: FVector = FVector {
    x: 1.0,
    y: 1.0,
    z: 1.0,
};

/// Quantization divisor Unreal uses for the spawn transform components.
const SPAWN_SCALE_FACTOR: i32 = 10;

/// Read the spawn block for a dynamic actor into `state`.
pub(super) fn read_dynamic_spawn_data(
    payload: &mut BitReader<'_>,
    state: &mut ActorChannelState,
    sink: &mut dyn GuidPathSink,
) -> Result<()> {
    // Archetype. Its path is what identifies the replay controller later, so
    // this read must happen before the net-player-index check in `channel.rs`.
    state.archetype_net_guid = net_guid::internal_load_object(payload, false, 0, sink)?;
    state.level_guid = net_guid::internal_load_object(payload, false, 0, sink)?;
    state.spawn_location = Some(read_optional_quantized_vector(
        payload,
        SPAWN_SCALE_FACTOR,
        ORIGIN,
    )?);
    if payload.read_bit()? {
        state.spawn_rotation = Some(read_rotation_short(payload)?);
    }
    state.spawn_scale = Some(read_optional_quantized_vector(
        payload,
        SPAWN_SCALE_FACTOR,
        UNIT_SCALE,
    )?);
    // Velocity is read unconditionally, as NewActorSerializer.cs:69-72 does;
    // gating it cost one invisible bit (docs/archive/PROJECT_STATUS.md 17-A).
    state.spawn_velocity = Some(read_optional_quantized_vector(
        payload,
        SPAWN_SCALE_FACTOR,
        ORIGIN,
    )?);
    Ok(())
}

/// Read an optional, optionally-quantized vector.
///
/// ```text
/// Bit layout:
///   hasValue         : 1 bit
///   [if !hasValue -> return the default]
///   isQuantized      : 1 bit
///   [if isQuantized]
///     componentInfo  : SerializedInt(128)
///     componentBitCount = info & 63
///     extraInfo = info >> 6
///     [if componentBitCount > 0] -> packed quantized
///     [else if extraInfo == 0]   -> 3 x f32
///     [else]                     -> 3 x f64
///   [else] -> 3 x f64
/// ```
///
/// A clear leading bit means "take the default", not "absent":
/// `ArchiveVectorReaders.ReadOptionalQuantizedVector` returns `defaultVector`,
/// and `NewActorSerializer.cs:56-72` passes (0,0,0) for location and velocity
/// and (1,1,1) for scale. So this always yields a vector; only a static actor,
/// which never enters the block, leaves the fields `None` -- unknown, not
/// (0,0,0) (docs/archive/PROJECT_STATUS.md 13-A has the corpus counts).
fn read_optional_quantized_vector(
    reader: &mut BitReader<'_>,
    scale_factor: i32,
    default: FVector,
) -> Result<FVector> {
    if !reader.read_bit()? {
        return Ok(default);
    }

    if !reader.read_bit()? {
        return read_f64_vector(reader);
    }

    let info = reader.read_serialized_int(128)?;
    let component_bit_count = info & 63;
    let extra_info = info >> 6;

    if component_bit_count == 0 {
        return Ok(if extra_info == 0 {
            let x = f64::from(reader.read_f32()?);
            let y = f64::from(reader.read_f32()?);
            let z = f64::from(reader.read_f32()?);
            FVector { x, y, z }
        } else {
            read_f64_vector(reader)?
        });
    }

    let x = reader.read_bits(component_bit_count)?;
    let y = reader.read_bits(component_bit_count)?;
    let z = reader.read_bits(component_bit_count)?;

    let sign_bit = 1u64 << (component_bit_count - 1);
    let sign_bias = sign_bit as i64;
    let fx = (x ^ sign_bit) as i64 - sign_bias;
    let fy = (y ^ sign_bit) as i64 - sign_bias;
    let fz = (z ^ sign_bit) as i64 - sign_bias;

    // `extra_info == 0`: the components are whole units; otherwise they were
    // multiplied by the scale factor before quantizing. Two arms rather than a
    // divide by 1.0, so whole-unit values reach Parquet with no arithmetic.
    Ok(if extra_info > 0 {
        let divisor = f64::from(scale_factor);
        FVector {
            x: fx as f64 / divisor,
            y: fy as f64 / divisor,
            z: fz as f64 / divisor,
        }
    } else {
        FVector {
            x: fx as f64,
            y: fy as f64,
            z: fz as f64,
        }
    })
}

#[inline]
fn read_f64_vector(reader: &mut BitReader<'_>) -> Result<FVector> {
    let x = reader.read_f64()?;
    let y = reader.read_f64()?;
    let z = reader.read_f64()?;
    Ok(FVector { x, y, z })
}

/// Read a compressed short rotator: for each of pitch, yaw and roll, a presence
/// bit, then if set a u16 `value` giving `value * 360 / 65536` degrees.
fn read_rotation_short(reader: &mut BitReader<'_>) -> Result<FRotator> {
    let pitch = read_compressed_short_component(reader)?;
    let yaw = read_compressed_short_component(reader)?;
    let roll = read_compressed_short_component(reader)?;
    Ok(FRotator { pitch, yaw, roll })
}

#[inline]
fn read_compressed_short_component(reader: &mut BitReader<'_>) -> Result<f32> {
    if reader.read_bit()? {
        let raw = reader.read_u16()?;
        Ok(f32::from(raw) * (360.0 / 65536.0))
    } else {
        Ok(0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_bits::{pack, write_byte, write_serialized_int};

    /// A clear leading bit yields the caller's default, which differs by
    /// vector (origin vs unit scale).
    #[test]
    fn absent_vector_takes_the_callers_default() {
        let data = pack(&[false]);
        let mut reader = BitReader::with_bit_len(&data, 1).unwrap();
        assert_eq!(
            read_optional_quantized_vector(&mut reader, SPAWN_SCALE_FACTOR, UNIT_SCALE).unwrap(),
            UNIT_SCALE
        );
        assert_eq!(reader.position(), 1, "exactly one bit is consumed");
    }

    /// The quantized path sign-extends each component and divides by the scale
    /// factor only when `extra_info` is non-zero.
    #[test]
    fn quantized_vector_sign_extends_and_scales() {
        // hasValue=1, isQuantized=1, info = 8 | (1 << 6) = 72 -> 8-bit
        // components with extra_info = 1, so each is divided by 10.
        let mut bits = vec![true, true];
        write_serialized_int(&mut bits, 72, 128); // 7 value bits
        for byte in [0xFFu8, 0x01, 0x80] {
            write_byte(&mut bits, byte);
        }
        let data = pack(&bits);
        let mut reader = BitReader::with_bit_len(&data, bits.len() as u64).unwrap();
        let v = read_optional_quantized_vector(&mut reader, SPAWN_SCALE_FACTOR, ORIGIN).unwrap();
        assert_eq!(v.x, -0.1, "0xFF as i8 is -1");
        assert_eq!(v.y, 0.1);
        assert_eq!(v.z, -12.8, "0x80 as i8 is -128");
    }

    /// A missing rotator component reads as zero degrees and consumes one bit.
    #[test]
    fn rotation_short_skips_absent_components() {
        let mut bits = vec![true];
        for byte in 16384u16.to_le_bytes() {
            write_byte(&mut bits, byte);
        }
        bits.push(false);
        bits.push(false);
        let data = pack(&bits);
        let mut reader = BitReader::with_bit_len(&data, bits.len() as u64).unwrap();
        let r = read_rotation_short(&mut reader).unwrap();
        assert_eq!(r.pitch, 90.0);
        assert_eq!(r.yaw, 0.0);
        assert_eq!(r.roll, 0.0);
        assert_eq!(reader.position(), 19);
    }
}
