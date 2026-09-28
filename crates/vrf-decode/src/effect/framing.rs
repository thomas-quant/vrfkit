//! The RepLayout dynamic-array framing every effect blob shares, and the
//! structural scan that derives an array's element handle pair from it.

use vrf_bitio::BitReader;

use super::{EffectBlobError, EffectHandles, Result};

/// Maximum array element count. Observed: FloatValues ~6, ObjectValues ~4,
/// VectorValues up to ~15 (shotgun pellets); 256 is headroom without runaway
/// allocation.
pub(super) const MAX_ARRAY_COUNT: u32 = 256;

/// Maximum fields per element; `FEffectData*` elements carry two.
pub(super) const MAX_FIELDS_PER_ELEMENT: u32 = 8;

/// Maximum bits in a single field payload. Prevents runaway on corrupt data.
const MAX_FIELD_PAYLOAD_BITS: u32 = 64 * 1024;

/// Build a reader over an exact bit window.
pub(super) fn new_blob_reader(raw: &[u8], bit_count: u32) -> Result<BitReader<'_>> {
    let available = (raw.len() as u64) * 8;
    if u64::from(bit_count) > available {
        return Err(EffectBlobError::BitLengthExceedsBuffer {
            bits: bit_count,
            available,
        });
    }
    Ok(BitReader::with_bit_len(raw, u64::from(bit_count))?)
}

/// Read the declared element count from the stream.
pub(super) fn read_array_count(reader: &mut BitReader<'_>) -> Result<u32> {
    let count = reader.read_int_packed()?;
    if count > MAX_ARRAY_COUNT {
        return Err(EffectBlobError::ArrayCountTooLarge {
            count,
            max: MAX_ARRAY_COUNT,
        });
    }
    Ok(count)
}

/// Read the next element index. Returns `None` if the terminator (0) is read.
pub(super) fn read_element_index(
    reader: &mut BitReader<'_>,
    declared_count: u32,
) -> Result<Option<u32>> {
    let encoded = reader.read_int_packed()?;
    if encoded == 0 {
        return Ok(None);
    }
    let index = encoded - 1;
    if index >= declared_count {
        return Err(EffectBlobError::IndexOutOfBounds {
            index,
            count: declared_count,
        });
    }
    Ok(Some(index))
}

/// Read the next field handle + payload bit count. Returns `None` on terminator.
pub(super) fn read_field_header(reader: &mut BitReader<'_>) -> Result<Option<(u32, u32)>> {
    let encoded_handle = reader.read_int_packed()?;
    if encoded_handle == 0 {
        return Ok(None);
    }
    let handle = encoded_handle - 1;
    let payload_bits = reader.read_int_packed()?;
    if payload_bits > MAX_FIELD_PAYLOAD_BITS || u64::from(payload_bits) > reader.bits_remaining() {
        return Err(EffectBlobError::PayloadTooLarge {
            bits: payload_bits,
            remaining: reader.bits_remaining(),
        });
    }
    Ok(Some((handle, payload_bits)))
}

/// When exactly 8 bits remain after the zero terminator, the C# parser reads
/// one more IntPacked and discards its value and any error, so any appended
/// byte passes as a terminator. Here it must be zero and a read failure
/// propagates: a silently permissive reference is not copied.
pub(super) fn consume_trailing_terminator(reader: &mut BitReader<'_>) -> Result<()> {
    if reader.bits_remaining() != 8 {
        return Ok(());
    }
    let value = reader.read_int_packed()?;
    if value != 0 {
        return Err(EffectBlobError::NonZeroTerminator { value });
    }
    Ok(())
}

/// Reject a value field whose declared width its type cannot occupy.
pub(super) fn expect_width(context: &'static str, expected: u32, found: u32) -> Result<()> {
    if found == expected {
        Ok(())
    } else {
        Err(EffectBlobError::UnexpectedPayloadWidth {
            context,
            expected,
            found,
        })
    }
}

/// Check that a field's type consumed exactly the width its header declared.
/// Past it, the decode has eaten into the next field. Short of it, declared
/// payload went unread: the `IntPacked` tag and object GUID are
/// self-delimiting and the writer measures `payload_bits` from what it wrote,
/// so a short read means the window is not the one this decoder thinks it is.
pub(super) fn settle_field(
    reader: &mut BitReader<'_>,
    start_pos: u64,
    payload_bits: u32,
) -> Result<()> {
    let consumed = reader.position() - start_pos;
    let declared = u64::from(payload_bits);
    if consumed > declared {
        return Err(EffectBlobError::PayloadOverread {
            declared: payload_bits,
            consumed,
        });
    }
    if consumed < declared {
        return Err(EffectBlobError::PayloadUnderread {
            declared: payload_bits,
            consumed,
        });
    }
    Ok(())
}

/// Derive an array's element handle pair by walking its framing: structure
/// only, no payload or handle interpreted, so it needs no knowledge of the
/// function. `None` when no element is populated (nothing to derive or decode).
///
/// Assuming nothing about how Unreal numbers handles makes this an independent
/// check of the rule that element handles start at the array's own handle
/// plus one: on `02d4d478` the derived base is the RPC parameter's handle plus
/// one for all 53,908 blobs.
///
/// # Errors
/// Rejects any array whose elements do not each carry exactly two fields at
/// adjacent handles with one agreed lower handle -- the shape of all 128,000
/// elements on `02d4d478`, and what makes the pair derivable at all.
pub fn scan_element_handles(raw: &[u8], bit_count: u32) -> Result<Option<EffectHandles>> {
    let mut reader = new_blob_reader(raw, bit_count)?;
    let count = read_array_count(&mut reader)?;
    let mut base: Option<u32> = None;

    while !reader.at_end() {
        let Some(_index) = read_element_index(&mut reader, count)? else {
            consume_trailing_terminator(&mut reader)?;
            break;
        };

        let mut seen = [0u32; 2];
        for (found, slot) in seen.iter_mut().enumerate() {
            let Some((handle, payload_bits)) = read_field_header(&mut reader)? else {
                return Err(EffectBlobError::ElementFieldCount {
                    found: found as u32,
                });
            };
            reader.skip_bits(u64::from(payload_bits))?;
            *slot = handle;
        }
        if let Some((_, payload_bits)) = read_field_header(&mut reader)? {
            // Consume it so the error message's position is not misleading if
            // a caller ever reports one; the blob is rejected either way.
            let _ = reader.skip_bits(u64::from(payload_bits));
            return Err(EffectBlobError::ElementFieldCount { found: 3 });
        }

        // Order within the element is not assumed; the tag is the lower handle.
        let (lo, hi) = (seen[0].min(seen[1]), seen[0].max(seen[1]));
        if hi != lo + 1 {
            return Err(EffectBlobError::NonAdjacentHandles {
                first: seen[0],
                second: seen[1],
            });
        }
        match base {
            None => base = Some(lo),
            Some(known) if known != lo => {
                return Err(EffectBlobError::InconsistentHandleBase {
                    expected: known,
                    found: lo,
                });
            }
            Some(_) => {}
        }
    }

    Ok(base.map(EffectHandles::from_base))
}
