//! The RepLayout dynamic-array framing the three struct blobs share. Kept apart
//! from the effect decoder's near-identical framing on purpose: the
//! element-count ceilings differ (128 here, 256 there) and output must stay
//! byte-identical.

use vrf_bitio::BitReader;

use super::{Result, StructBlobError};

const MAX_ARRAY_COUNT: u32 = 128;
const MAX_FIELDS_PER_ELEMENT: u32 = 8;
const MAX_FIELD_PAYLOAD_BITS: u32 = 64 * 1024;

/// Read the declared element count from the stream.
fn read_array_count(reader: &mut BitReader<'_>) -> Result<u32> {
    let count = reader.read_int_packed()?;
    if count > MAX_ARRAY_COUNT {
        return Err(StructBlobError::ArrayCountTooLarge {
            count,
            max: MAX_ARRAY_COUNT,
        });
    }
    Ok(count)
}

/// Read the next element index. Returns `None` if the terminator (0) is read.
fn read_element_index(reader: &mut BitReader<'_>, declared_count: u32) -> Result<Option<u32>> {
    let encoded = reader.read_int_packed()?;
    if encoded == 0 {
        return Ok(None);
    }
    let index = encoded - 1;
    if index >= declared_count {
        return Err(StructBlobError::IndexOutOfBounds {
            index,
            count: declared_count,
        });
    }
    Ok(Some(index))
}

/// The element loop all three blobs share. Per field, in this order: header,
/// field-count limit, window, `name_for(handle)`, then `member` (reads the
/// window into the row and returns the label a leftover is reported under, or
/// `None` for a name without an arm), then the window-consumed check. The
/// order decides which error a malformed blob reports, so it is written once.
pub(super) fn decode_elements<'d, R>(
    reader: &mut BitReader<'_>,
    context: &'static str,
    mut name_for: impl FnMut(u32) -> Result<&'d str>,
    new_row: impl Fn(u32) -> R,
    mut member: impl FnMut(&mut R, &'d str, &mut BitReader<'_>) -> Result<Option<&'d str>>,
) -> Result<Vec<R>> {
    let count = read_array_count(reader)?;
    let mut rows = Vec::new();
    while let Some(index) = read_element_index(reader, count)? {
        let mut row = new_row(index);
        for field_idx in 0..=MAX_FIELDS_PER_ELEMENT {
            let Some((handle, bit_count)) = read_field_header(reader)? else {
                break;
            };
            if field_idx == MAX_FIELDS_PER_ELEMENT {
                return Err(StructBlobError::TooManyFields { context });
            }
            // Advances the parent past the window, whatever `member` reads.
            let mut sub = reader.sub_reader(u64::from(bit_count))?;
            let name = name_for(handle)?;
            let Some(label) = member(&mut row, name, &mut sub)? else {
                return Err(StructBlobError::UnsupportedMember {
                    name: name.to_owned(),
                    handle,
                    context,
                });
            };
            ensure_member_consumed(&sub, label, handle, bit_count, context)?;
        }
        rows.push(row);
    }
    ensure_consumed(reader)?;
    Ok(rows)
}

/// Read the next field handle. Returns `None` if the terminator (0) is read.
/// Also reads the bit_count of the field payload.
fn read_field_header(reader: &mut BitReader<'_>) -> Result<Option<(u32, u32)>> {
    let encoded = reader.read_int_packed()?;
    if encoded == 0 {
        return Ok(None);
    }
    let handle = encoded - 1;
    let bit_count = reader.read_int_packed()?;
    if bit_count > MAX_FIELD_PAYLOAD_BITS || u64::from(bit_count) > reader.bits_remaining() {
        return Err(StructBlobError::PayloadTooLarge {
            bits: bit_count,
            remaining: reader.bits_remaining(),
        });
    }
    Ok(Some((handle, bit_count)))
}

/// Read a byte-width enum whose payload carries only its significant bits.
/// Zero-width and over-wide payloads are errors: no value would make a field
/// the wire sent look absent.
pub(super) fn read_narrow_byte(
    reader: &mut BitReader<'_>,
    name: &str,
    context: &'static str,
) -> Result<u8> {
    let bits = reader.bits_remaining();
    if bits == 0 || bits > 8 {
        return Err(StructBlobError::InvalidEnumWidth {
            name: name.to_owned(),
            bits,
            context,
        });
    }
    Ok(reader.read_bits(bits as u32)? as u8)
}

/// The name the REPLAY declares for `handle`, which is what selects a member.
/// 13.02 deleted `TeamEconomy` and `TeamComponents` from `BombGameState` and
/// added `TeamStates`, moving every later handle down by eight (`RoundResults`
/// 93..=96 -> 81..=84), and a handle-keyed decoder read NOTHING; the
/// declaration moves with the members. Resolution is handle -> name, NEVER the
/// reverse: `WinningTeam` is also the match-winner scalar at handle 50, which a
/// search by name could match.
pub(super) fn member_name<'d>(
    declared: &[Option<&'d str>],
    handle: u32,
    context: &'static str,
) -> Result<&'d str> {
    declared
        .get(handle as usize)
        .copied()
        .flatten()
        .ok_or(StructBlobError::UndeclaredHandle { handle, context })
}

/// Ensure ONE field's sub-reader consumed its whole declared window: per field,
/// because [`ensure_consumed`] cannot see inside a window the parent already
/// skipped (see `StructBlobError::MemberNotFullyConsumed`).
fn ensure_member_consumed(
    sub: &BitReader<'_>,
    name: &str,
    handle: u32,
    declared: u32,
    context: &'static str,
) -> Result<()> {
    let remaining = sub.bits_remaining();
    if remaining > 0 {
        return Err(StructBlobError::MemberNotFullyConsumed {
            name: name.to_owned(),
            handle,
            declared,
            remaining,
            context,
        });
    }
    Ok(())
}

/// Ensure the reader is fully consumed.
fn ensure_consumed(reader: &BitReader<'_>) -> Result<()> {
    if reader.bits_remaining() > 0 {
        return Err(StructBlobError::NotFullyConsumed {
            remaining: reader.bits_remaining(),
        });
    }
    Ok(())
}
