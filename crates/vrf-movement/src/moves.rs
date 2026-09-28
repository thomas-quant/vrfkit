//! The movement section and the single move record inside it: the innermost
//! layer, written in the numeric vocabulary of [`crate::primitives`].
//!
//! Moves are separated by a 3-bit marker that counts 1 to 7 and wraps to 1. A
//! marker out of sequence means the cursor has drifted and is an error, not
//! skipped: continuing from a desynced position yields well-formed nonsense.

use vrf_bitio::BitReader;

use crate::error::MovementError;
use crate::primitives::{ANGLE_SCALE, read_quantized_vector};
use crate::types::{MovementMove, RpcDecodeResult};

/// Magic byte at the start of a movement section.
pub(crate) const MOVEMENT_MAGIC: u8 = 0x52;

/// The C# reference's `MaxMovementPaddingBits`: with at most this many bits
/// left after a move, the section ends without reading another marker. The
/// bits are not padding: in every stream measured they are a `000` terminator
/// where the next marker would sit, then 8 to 23 bits that are not all zero
/// (crate docs, "Measured on real replays").
const MAX_MOVEMENT_PADDING_BITS: u64 = 31;

/// Parse the movement section: magic byte, then a sequence of moves.
///
/// Returns the bits of its window left unread at a stop the grammar does not
/// explain: a zero marker with bits behind it, or a window too short for the
/// magic or the first marker. The end within `MAX_MOVEMENT_PADDING_BITS` of a
/// move returns 0, as does a window read to its last bit. The caller tallies a
/// nonzero return; [`RpcDecodeResult::sized_section_tails`] says why.
pub(crate) fn parse_movement_section(
    reader: &mut BitReader<'_>,
    shooter_guid: u32,
    result: &mut RpcDecodeResult,
    emit: &mut impl FnMut(MovementMove),
) -> Result<u64, MovementError> {
    if reader.bits_remaining() < 8 {
        return Ok(reader.bits_remaining());
    }

    let magic = reader.read_u8()?;
    if magic != MOVEMENT_MAGIC {
        return Err(MovementError::InvalidMagic(magic));
    }

    if reader.bits_remaining() < 3 {
        return Ok(reader.bits_remaining());
    }

    let mut expected_marker: u8 = 1;
    let mut marker = reader.read_bits(3)? as u8;

    while marker != 0 {
        if marker != expected_marker {
            return Err(MovementError::MarkerMismatch {
                expected: expected_marker,
                actual: marker,
            });
        }

        let mv = parse_single_move(reader, shooter_guid)?;
        emit(mv);
        result.total_moves += 1;

        // The section's end; what is left stays unread.
        if reader.bits_remaining() <= MAX_MOVEMENT_PADDING_BITS {
            return Ok(0);
        }

        expected_marker = next_marker(expected_marker);
        marker = reader.read_bits(3)? as u8;
    }

    // A zero marker, which in the loop means more than
    // MAX_MOVEMENT_PADDING_BITS were left: the rest is a tail (or nothing,
    // straight after the magic).
    Ok(reader.bits_remaining())
}

/// The marker expected after `marker`: 1 to 7, wrapping to 1 and never 0.
#[inline]
pub(crate) fn next_marker(marker: u8) -> u8 {
    let next = (marker + 1) & 7;
    if next < 2 { 1 } else { next }
}

/// Parse one MovementMove from the stream.
fn parse_single_move(
    reader: &mut BitReader<'_>,
    shooter_guid: u32,
) -> Result<MovementMove, MovementError> {
    // -- 25-bit header ----------------------------------------------------
    // Decoded and dropped, but not constant over the 157,457,629 moves in the
    // crate docs' sample: unusedByte (the C#'s name) is non-zero in
    // 155,140,482, rotationYawMultiplier in 29,589,841; rotationInput is off
    // centre in 97,788,473, flag48 set in 150,351,309, the optional byte
    // present in 9,255,640 and variant1Flag set in 1,655.
    let header = reader.read_bits(25)?;
    let move_type_flag = (header & 1) != 0; // bit 0
    let _rotation_yaw_multiplier = ((header >> 1) & 0xFF) as u8; // bits [1..9]
    let movement_state = ((header >> 9) & 0xFF) as u8; // bits [9..17]
    let _unused_byte = ((header >> 17) & 0xFF) as u8; // bits [17..25]

    // -- FixedVector: rotationInput (3 x u16), not exported ----------------
    reader.skip_bits(48)?;

    // -- Timestamp: the C# reference's "VLQ" is Unreal's IntPacked ----------
    let timestamp = reader.read_int_packed()?;

    let (pos_x, pos_y, pos_z) = read_quantized_vector(reader, 100)?;

    let has_optional = reader.read_bit()?;
    if has_optional {
        let _optional_byte = reader.read_u8()?;
    }

    // -- 33-bit flag + packed angles --------------------------------------
    let flag_and_angles = reader.read_bits(33)?;
    let _flag48 = (flag_and_angles & 1) != 0;
    let packed_angles = (flag_and_angles >> 1) as u32;
    let raw_pitch = (packed_angles & 0xFFFF) as u16;
    let raw_yaw = (packed_angles >> 16) as u16;

    let yaw = f64::from(raw_yaw) * ANGLE_SCALE;
    let pitch = f64::from(raw_pitch) * ANGLE_SCALE;

    let (vel_x, vel_y, vel_z) = if move_type_flag {
        let _variant1_flag = reader.read_bit()?;
        read_quantized_vector(reader, 10)?
    } else {
        // Variant 0: 33-bit packed angles, no velocity.
        let variant0_data = reader.read_bits(33)?;
        let has_external_ref = (variant0_data & 1) != 0;
        if has_external_ref {
            return Err(MovementError::Variant0ExternalCharRef);
        }
        (0.0, 0.0, 0.0)
    };

    let error_sentinel = reader.read_bit()?;
    if error_sentinel {
        return Err(MovementError::ErrorSentinel);
    }

    Ok(MovementMove {
        shooter_character_net_guid: shooter_guid,
        pos_x,
        pos_y,
        pos_z,
        yaw,
        pitch,
        vel_x,
        vel_y,
        vel_z,
        timestamp,
        movement_state,
        mode_flags: movement_state, // same field in wire format
        move_type: u8::from(move_type_flag),
    })
}
