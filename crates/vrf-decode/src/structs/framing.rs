//! The RepLayout dynamic-array framing the three struct blobs share.
//!
//! Kept apart from the effect decoder's near-identical framing on purpose: the
//! two disagree on the element-count ceiling (128 here, 256 there) and on what
//! they do with a malformed element, and the acceptance bar for this crate is
//! byte-identical output. One shared abstraction that quietly changed either
//! would be a worse trade than two small honest copies.

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

/// The element loop all three blobs share.
///
/// Per field, in this order: the header, the field-count limit, the field's
/// window, the name `name_for` gives its handle, then `member`, which reads
/// the window into the row and returns the name a leftover is reported under
/// -- or `None` for a name it has no arm for -- and last the check that the
/// window was consumed. The order decides which error a malformed blob
/// reports, so it is written once.
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
            // The sub-reader consumes the bits from the parent, so a field we
            // do not interpret still advances the stream correctly.
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
///
/// Zero-width and over-wide payloads are malformed: returning no value would
/// make a field the wire explicitly sent indistinguishable from an absent one.
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
///
/// Handle numbers are not stable across game builds. Build 13.02 deleted
/// `TeamEconomy` and `TeamComponents` from `BombGameState` and added
/// `TeamStates`, which moved every later handle down by eight: `RoundResults`'s
/// members went from 93..=96 to 81..=84. A decoder keyed on the old numbers
/// does not misread them, it reads NOTHING, because the first handle it meets
/// is one it has no arm for. The declaration moves with the members, so it is
/// the only key that survives a reshuffle.
///
/// Resolution is handle -> name and NEVER the reverse. A name can be declared
/// at more than one handle: `WinningTeam` is both the `BombGameState` scalar
/// naming the match winner (handle 50) and the `RoundResults` member (81 on
/// 13.02, 93 on 13.01). Searching the declaration BY NAME can therefore match
/// the wrong slot and yield a plausible wrong value, where asking what a
/// handle the wire just handed us is called cannot -- handle 50 never appears
/// inside the blob.
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

/// Ensure ONE field's sub-reader consumed the whole window its header declared.
///
/// `reader.sub_reader(bit_count)` advances the PARENT past the entire window
/// the moment it is created, so the blob stays aligned no matter how much of it
/// the member actually reads -- every later member decodes and the closing
/// [`ensure_consumed`] is satisfied. That is why a member reading half its
/// window was invisible: alignment is preserved and only interpretation is
/// lost.
///
/// Called per field rather than per blob for exactly that reason: the blob-level
/// check cannot see inside a window the parent has already skipped.
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
