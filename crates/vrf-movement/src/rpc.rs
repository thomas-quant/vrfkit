//! The RPC framing layers: batch -> updates array -> one update -> the
//! component data stream that holds the movement section (diagrams in the
//! crate docs). The batch and each update are property-style loops; a handle
//! this crate does not decode is skipped by its declared length, so an unknown
//! future field passes without desynchronising the ones around it.

use vrf_bitio::BitReader;

use crate::error::MovementError;
use crate::moves::parse_movement_section;
use crate::types::{MovementMove, RpcDecodeResult};

/// Maximum number of character updates in a single RPC batch.
const MAX_REMOTE_CHARACTER_UPDATES: u32 = 256;

/// Property handles, `pub(crate)` so the tests build payloads from the same numbers.
pub(crate) const REMOTE_CHARACTER_UPDATES_HANDLE: u32 = 1;
pub(crate) const SHOOTER_CHARACTER_NET_GUID_HANDLE: u32 = 2;
pub(crate) const COMPONENT_DATA_STREAM_HANDLE: u32 = 3;

/// Decode the full movement RPC payload, calling `emit` for each decoded move
/// rather than collecting a Vec, so the caller can push straight to the
/// Parquet writer. `reader` must be bounded to the RPC payload's exact length.
pub fn decode_movement_rpc(
    reader: &mut BitReader<'_>,
    mut emit: impl FnMut(MovementMove),
) -> Result<RpcDecodeResult, MovementError> {
    let end_bit = reader.len_bits();
    let mut result = RpcDecodeResult::default();

    // First bit: consumed but value ignored.
    // If no bits remain, the payload is empty.
    if reader.bits_remaining() == 0 {
        return Ok(result);
    }
    let _ = reader.read_bit()?;

    while reader.position() < end_bit {
        let encoded_handle = reader.read_int_packed()?;
        if encoded_handle == 0 {
            break;
        }
        let handle = encoded_handle - 1;
        let payload_bits = reader.read_int_packed()?;

        if handle != REMOTE_CHARACTER_UPDATES_HANDLE {
            reader.skip_bits(u64::from(payload_bits))?;
            continue;
        }

        let mut sub = reader.sub_reader(u64::from(payload_bits))?;
        decode_updates_array(&mut sub, &mut result, &mut emit)?;
    }

    // A 0 handle before `end_bit` leaves the rest unread. A drift can make the
    // very first read return 0, which uncounted is exactly an empty RPC.
    if reader.position() < end_bit {
        result.error_count += 1;
    }

    Ok(result)
}

/// Decode the RemoteCharacterUpdates array.
fn decode_updates_array(
    reader: &mut BitReader<'_>,
    result: &mut RpcDecodeResult,
    emit: &mut impl FnMut(MovementMove),
) -> Result<(), MovementError> {
    let end_bit = reader.len_bits();
    let update_count = reader.read_int_packed()?;

    if update_count > MAX_REMOTE_CHARACTER_UPDATES {
        return Err(MovementError::TooManyUpdates(update_count));
    }

    result.update_count = update_count;

    while reader.position() < end_bit {
        let encoded_index = reader.read_int_packed()?;
        if encoded_index == 0 {
            // Exactly 8 bits left: a trailing IntPacked (never seen).
            // Nothing depends on its value, but one that does not parse means
            // the grammar drifted, so the failure is counted.
            if end_bit.saturating_sub(reader.position()) == 8 && reader.read_int_packed().is_err() {
                result.error_count += 1;
            }
            break;
        }

        let index = encoded_index - 1;
        if index >= update_count {
            // An update the array never declared: the rest of the window goes,
            // and is counted.
            result.error_count += 1;
            reader.skip_remaining();
            break;
        }

        if decode_single_update(reader, result, emit).is_err() {
            result.error_count += 1;
            // Only a framing read (a handle or a payload length) fails out of
            // an update, and after one the next index cannot be located.
            reader.skip_remaining();
            break;
        }
    }

    Ok(())
}

/// Decode one RemoteCharacterUpdate.
fn decode_single_update(
    reader: &mut BitReader<'_>,
    result: &mut RpcDecodeResult,
    emit: &mut impl FnMut(MovementMove),
) -> Result<(), MovementError> {
    let end_bit = reader.len_bits();
    let mut shooter_guid: Option<u32> = None;

    while reader.position() < end_bit {
        let encoded_handle = reader.read_int_packed()?;
        if encoded_handle == 0 {
            break;
        }
        let handle = encoded_handle - 1;
        let payload_bits = reader.read_int_packed()?;

        if u64::from(payload_bits) > reader.bits_remaining() {
            // Longer than the rest of the updates window: the framing no
            // longer describes the payload, and every update queued behind
            // this one goes with the window.
            result.error_count += 1;
            reader.skip_remaining();
            break;
        }

        match handle {
            SHOOTER_CHARACTER_NET_GUID_HANDLE => {
                let mut sub = reader.sub_reader(u64::from(payload_bits))?;
                if payload_bits >= 32 {
                    shooter_guid = Some(sub.read_u32()?);
                } else {
                    // Too narrow for its u32. The field is consumed, so the
                    // framing survives, but the update has no character to
                    // attribute moves to: a loss, not "no moves".
                    result.error_count += 1;
                }
            }
            COMPONENT_DATA_STREAM_HANDLE => {
                let mut sub = reader.sub_reader(u64::from(payload_bits))?;
                if let Some(guid) = shooter_guid {
                    // `sub_reader` has already moved `reader` past the whole
                    // stream, so a failure inside it cannot misplace the next
                    // handle: count it and go on.
                    if decode_component_data_stream(&mut sub, guid, result, emit).is_err() {
                        result.error_count += 1;
                    }
                } else {
                    // No GUID: handle 2 was undersized (counted above) or has
                    // not arrived, and a single pass cannot rewind to it, so
                    // the moves are dropped. An update hitting both adds two.
                    result.error_count += 1;
                }
            }
            _ => {
                reader.skip_bits(u64::from(payload_bits))?;
            }
        }
    }

    Ok(())
}

/// Decode a ComponentDataStream: a u16 envelope byte count, else the u16 is
/// movementBitCount. The u16 is a byte count iff it is non-zero and the
/// envelope fits, and that choice is final: an inner failure never rolls back
/// to the other reading. Every measured stream is wrapped (crate docs,
/// "Measured on real replays").
fn decode_component_data_stream(
    reader: &mut BitReader<'_>,
    shooter_guid: u32,
    result: &mut RpcDecodeResult,
    emit: &mut impl FnMut(MovementMove),
) -> Result<(), MovementError> {
    let first_u16 = read_u16_checked(reader)?;

    let byte_count = u64::from(first_u16);
    if byte_count > 0 && reader.bits_remaining() >= byte_count * 8 {
        // Wrapped: the envelope's payload starts with its own movementBitCount.
        let mut inner = reader.sub_reader(byte_count * 8)?;
        // What follows the envelope is never read; tallied here, before the
        // section can fail, with the stream counted even when nothing follows.
        result.envelope_trailer_streams += 1;
        result.envelope_trailer_bits += reader.bits_remaining();
        let bit_count = read_u16_checked(&mut inner)?;
        parse_movement_with_bit_count(&mut inner, bit_count, shooter_guid, result, emit)
    } else {
        // Not byte-wrapped: first_u16 is the movementBitCount.
        parse_movement_with_bit_count(reader, first_u16, shooter_guid, result, emit)
    }
}

/// Read a u16, failing with `TruncatedComponentHeader` when fewer than 16 bits
/// remain, rather than silently yielding nothing.
fn read_u16_checked(reader: &mut BitReader<'_>) -> Result<u16, MovementError> {
    if reader.bits_remaining() < 16 {
        return Err(MovementError::TruncatedComponentHeader {
            available_bits: reader.bits_remaining(),
        });
    }
    Ok(reader.read_u16()?)
}

/// Parse the movement section in a window of `movement_bit_count` bits, or of
/// all that remain when that is 0 or larger (an open window), tallying a tail.
fn parse_movement_with_bit_count(
    reader: &mut BitReader<'_>,
    movement_bit_count: u16,
    shooter_guid: u32,
    result: &mut RpcDecodeResult,
    emit: &mut impl FnMut(MovementMove),
) -> Result<(), MovementError> {
    let remaining = reader.bits_remaining();

    let uses_all_remaining = movement_bit_count == 0 || u64::from(movement_bit_count) > remaining;
    let bits = if uses_all_remaining {
        remaining
    } else {
        u64::from(movement_bit_count)
    };

    let mut movement_reader = reader.sub_reader(bits)?;
    let tail_bits = parse_movement_section(&mut movement_reader, shooter_guid, result, emit)?;
    if tail_bits > 0 {
        if uses_all_remaining {
            result.open_section_tails += 1;
            result.open_section_tail_bits += tail_bits;
        } else {
            result.sized_section_tails += 1;
            result.sized_section_tail_bits += tail_bits;
        }
    }

    if !uses_all_remaining {
        reader.skip_remaining();
    }

    Ok(())
}
