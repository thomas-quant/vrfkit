//! LSB-first bit reader for Unreal Engine replay streams.
//!
//! Unreal's `FBitWriter` packs bits least-significant-first within each byte
//! and lets values straddle bytes, so a payload is one bit stream: bit `i` is
//! `data[i >> 3] >> (i & 7) & 1`, and a multi-bit read shifts right and masks.
//!
//! [`BitReader::read_int_packed`], [`BitReader::read_serialized_int`] and
//! [`BitReader::read_fstring`] consume a width that depends on the value, so a
//! wrong count desynchronises the rest of the stream instead of failing; each
//! is pinned by tests. Every read is bounds-checked: truncation is a
//! [`BitError`], never a zero.
//!
//! # Features
//!
//! The crate is `no_std` whenever it is not being tested. `alloc` (default)
//! only adds `read_fstring`, the one read that allocates; [`BitError`] keeps
//! its shape either way.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), no_std)]

#[cfg(feature = "alloc")]
extern crate alloc;

#[cfg(feature = "alloc")]
use alloc::string::String;
#[cfg(feature = "alloc")]
use alloc::vec::Vec;
use core::fmt;

/// Five 7-bit chunks cover a `u32` (35 bits); a sixth means a malformed stream.
const MAX_INT_PACKED_BYTES: u32 = 5;

const LAST_INT_PACKED_SHIFT: u32 = 7 * (MAX_INT_PACKED_BYTES - 1);

/// The final chunk's largest legal payload: at shift 28 a `u32` has 4 bits left.
const LAST_INT_PACKED_MAX: u32 = u32::MAX >> LAST_INT_PACKED_SHIFT;

/// Why a bit read failed: the stream is not what the caller assumed, never
/// "the value was empty". Positions are bit offsets within the reader's window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BitError {
    /// Fewer bits remain than the read requires; `length` is the window's.
    Eof {
        position: u64,
        length: u64,
        requested: u64,
    },
    /// An `IntPacked` value did not terminate within five bytes.
    MalformedIntPacked {
        /// Where the value started.
        position: u64,
    },
    /// An `IntPacked` value's fifth chunk does not fit the 4 bits a `u32` has
    /// left at shift 28.
    IntPackedOverflow {
        /// Where the value started.
        position: u64,
    },
    /// `read_serialized_int` was given a zero maximum.
    InvalidSerializedIntMax {
        /// The rejected maximum.
        max: u32,
    },
    /// A length prefix beyond the caller's cap or the remaining stream.
    InvalidLength {
        /// Where the length prefix started.
        position: u64,
        length: i64,
    },
    /// A requested bit length exceeds what a supplied byte slice holds.
    InvalidBitLength {
        /// In bits, as is `available`.
        requested: u64,
        available: u64,
    },
    /// A string's bytes were not valid UTF-8 / UTF-16.
    InvalidString {
        /// Where the string started.
        position: u64,
    },
}

impl fmt::Display for BitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Eof {
                position,
                length,
                requested,
            } => write!(
                f,
                "unexpected end of archive: needed {requested} bit(s) at position {position} of {length}"
            ),
            Self::MalformedIntPacked { position } => write!(
                f,
                "packed integer at position {position} did not terminate within {MAX_INT_PACKED_BYTES} bytes"
            ),
            Self::IntPackedOverflow { position } => write!(
                f,
                "packed integer at position {position} does not fit in a u32"
            ),
            Self::InvalidSerializedIntMax { max } => {
                write!(f, "serialized int maximum must be positive, got {max}")
            }
            Self::InvalidLength { position, length } => {
                write!(f, "invalid length {length} at position {position}")
            }
            Self::InvalidBitLength {
                requested,
                available,
            } => write!(
                f,
                "requested bit length {requested} exceeds {available} available bits"
            ),
            Self::InvalidString { position } => {
                write!(f, "malformed string at position {position}")
            }
        }
    }
}

impl core::error::Error for BitError {}

/// Result alias for bit reads.
pub type Result<T> = core::result::Result<T, BitError>;

/// A cursor over a borrowed bit stream. Sub-readers are views, not copies, so
/// framing a bunch into blocks and a block into fields allocates nothing; a
/// window may start mid-byte, the normal case for field payloads.
#[derive(Debug, Clone)]
pub struct BitReader<'a> {
    data: &'a [u8],
    /// Absolute bit index in `data` where the window begins.
    start_bit: u64,
    /// Bits consumed, relative to `start_bit`.
    pos: u64,
    /// Window length in bits.
    len: u64,
}

impl<'a> BitReader<'a> {
    /// Create a reader over every bit of `data`.
    #[must_use]
    pub fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            start_bit: 0,
            pos: 0,
            len: (data.len() as u64) * 8,
        }
    }

    /// Create a reader over the first `bit_len` bits of `data`, for a payload
    /// whose wire-declared length leaves padding in its final byte.
    ///
    /// [`BitError::InvalidBitLength`] when `bit_len` exceeds `data`.
    pub fn with_bit_len(data: &'a [u8], bit_len: u64) -> Result<Self> {
        let available = (data.len() as u64).saturating_mul(8);
        if bit_len > available {
            return Err(BitError::InvalidBitLength {
                requested: bit_len,
                available,
            });
        }
        Ok(Self {
            data,
            start_bit: 0,
            pos: 0,
            len: bit_len,
        })
    }

    /// Bits consumed so far.
    #[must_use]
    #[inline]
    pub const fn position(&self) -> u64 {
        self.pos
    }

    /// Total window length in bits.
    #[must_use]
    #[inline]
    pub const fn len_bits(&self) -> u64 {
        self.len
    }

    /// Bits left to read.
    #[must_use]
    #[inline]
    pub const fn bits_remaining(&self) -> u64 {
        self.len - self.pos
    }

    /// Whether the window is fully consumed.
    #[must_use]
    #[inline]
    pub const fn at_end(&self) -> bool {
        self.pos >= self.len
    }

    #[inline]
    fn need(&self, bits: u64) -> Result<()> {
        if self.bits_remaining() < bits {
            // Inline on purpose: a #[cold] builder taking &self cost ~2%; see
            // docs/PERFORMANCE_NOTES.md#cold-path-builders-stay-free-functions.
            return Err(BitError::Eof {
                position: self.pos,
                length: self.len,
                requested: bits,
            });
        }
        Ok(())
    }

    /// The 8 bytes at `byte`, zero-padded past the end; the padding only covers
    /// bits callers mask off. A fixed-size chunk is one unaligned load, where a
    /// runtime-length copy was a memcpy on every read
    /// (docs/PERFORMANCE_NOTES.md#load_u64-avoiding-a-memcpy).
    #[inline]
    fn load_u64(&self, byte: usize) -> u64 {
        match self.data.get(byte..).and_then(<[u8]>::first_chunk::<8>) {
            Some(chunk) => u64::from_le_bytes(*chunk),
            None => load_u64_padded(self.data, byte),
        }
    }

    /// Read a single bit.
    #[inline]
    pub fn read_bit(&mut self) -> Result<bool> {
        self.need(1)?;
        let abs = self.start_bit + self.pos;
        // In range (`need`, and the window lies inside `data`). `get` only
        // spares the hottest function a panic path: `unwrap_or` is unreachable.
        let byte = self.data.get((abs >> 3) as usize).copied().unwrap_or(0);
        let bit = (byte >> (abs & 7)) & 1;
        self.pos += 1;
        Ok(bit != 0)
    }

    /// Read `count` bits (0..=64) LSB-first into the low bits of a `u64`.
    #[inline]
    pub fn read_bits(&mut self, count: u32) -> Result<u64> {
        debug_assert!(count <= 64, "read_bits supports at most 64 bits");
        if count == 0 {
            return Ok(0);
        }
        self.need(u64::from(count))?;
        let abs = self.start_bit + self.pos;
        let byte = (abs >> 3) as usize;
        let off = (abs & 7) as u32;

        // One load holds `64 - off` bits. A longer read is short by
        // `count + off - 64 <= 7` bits, so one more byte completes it, not a
        // second word; `got` is then 57..=63, so `high << got` is in range. That
        // byte is in range, and read fallibly, as in `read_bit`.
        let low = self.load_u64(byte) >> off;
        let got = 64 - off;
        let value = if count <= got {
            low & mask_u64(count)
        } else {
            let high = u64::from(self.data.get(byte + 8).copied().unwrap_or(0));
            (low | (high << got)) & mask_u64(count)
        };
        self.pos += u64::from(count);
        Ok(value)
    }

    /// Read 8 bits as a byte.
    #[inline]
    pub fn read_u8(&mut self) -> Result<u8> {
        Ok(self.read_bits(8)? as u8)
    }

    /// Read 16 bits little-endian.
    #[inline]
    pub fn read_u16(&mut self) -> Result<u16> {
        Ok(self.read_bits(16)? as u16)
    }

    /// Read 32 bits little-endian.
    #[inline]
    pub fn read_u32(&mut self) -> Result<u32> {
        Ok(self.read_bits(32)? as u32)
    }

    /// Read 32 bits little-endian as a signed integer.
    #[inline]
    pub fn read_i32(&mut self) -> Result<i32> {
        Ok(self.read_u32()? as i32)
    }

    /// Read 64 bits little-endian.
    #[inline]
    pub fn read_u64(&mut self) -> Result<u64> {
        self.read_bits(64)
    }

    /// Read an IEEE-754 single.
    #[inline]
    pub fn read_f32(&mut self) -> Result<f32> {
        Ok(f32::from_bits(self.read_u32()?))
    }

    /// Read an IEEE-754 double.
    #[inline]
    pub fn read_f64(&mut self) -> Result<f64> {
        Ok(f64::from_bits(self.read_u64()?))
    }

    /// Read Unreal's `SerializeIntPacked`: per byte, 7 payload bits above a
    /// continuation bit, chunks little-endian.
    ///
    /// A byte loop, because its EOF (`requested: 8` after the chunks consumed)
    /// is what callers classify malformed blocks by. Only the fifth chunk, at
    /// shift 28, can overrun a `u32`; unchecked, `16u32 << 28 == 0` returns
    /// `Ok(0)`, the terminator of every property loop above this crate. That
    /// check is peeled out of the loop
    /// (docs/PERFORMANCE_NOTES.md#read_int_packed-peeling-the-overflow-check)
    /// and follows the continuation test, so a runaway still reports
    /// [`BitError::MalformedIntPacked`].
    #[inline]
    pub fn read_int_packed(&mut self) -> Result<u32> {
        let start = self.pos;
        let mut value: u32 = 0;
        let mut shift: u32 = 0;
        for _ in 0..MAX_INT_PACKED_BYTES - 1 {
            let next = self.read_u8()?;
            value |= u32::from(next >> 1) << shift;
            if next & 1 == 0 {
                return Ok(value);
            }
            shift += 7;
        }

        let last = self.read_u8()?;
        if last & 1 != 0 {
            return Err(BitError::MalformedIntPacked { position: start });
        }
        let chunk = u32::from(last >> 1);
        if chunk > LAST_INT_PACKED_MAX {
            return Err(BitError::IntPackedOverflow { position: start });
        }
        Ok(value | (chunk << LAST_INT_PACKED_SHIFT))
    }

    /// Read Unreal's `FBitReader::SerializeInt`: `floor(log2(max))` bits, plus
    /// one more only when the value can still reach `max`, so the width depends
    /// on the value. Inlined so a constant `max` folds `ilog2` and the mask away.
    #[inline]
    pub fn read_serialized_int(&mut self, max: u32) -> Result<u32> {
        if max == 0 {
            return Err(BitError::InvalidSerializedIntMax { max });
        }
        let value_bits = max.ilog2();
        let mut value = if value_bits > 0 {
            self.read_bits(value_bits)? as u32
        } else {
            0
        };
        let bit_mask = 1u32 << value_bits;
        // The top bit would overshoot `max`, so the encoder did not write it.
        if value.saturating_add(bit_mask) >= max {
            return Ok(value);
        }
        if self.read_bit()? {
            value |= bit_mask;
        }
        Ok(value)
    }

    /// Read an Unreal `FString`: a positive length counts UTF-8 bytes, a
    /// negative one UTF-16 units, and `max_bytes` caps the allocation.
    ///
    /// The trailing null is stripped when present but not required: the cursor
    /// advances by the declared width either way and the string is the wire
    /// bytes, so requiring it would prevent no wrong value while failing every
    /// caller (paths, event metadata, string fields) on a writer that omits it.
    #[cfg(feature = "alloc")]
    pub fn read_fstring(&mut self, max_bytes: i64) -> Result<String> {
        let start = self.pos;
        let raw = i64::from(self.read_i32()?);
        let invalid = || BitError::InvalidLength {
            position: start,
            length: raw,
        };
        if max_bytes < 0 {
            return Err(invalid());
        }
        if raw == 0 {
            return Ok(String::new());
        }
        let utf16 = raw < 0;
        // An i32 length is at most 2^31 units, so neither product overflows.
        let units = raw.unsigned_abs();
        let byte_len = if utf16 { units * 2 } else { units };
        if byte_len > max_bytes as u64 || byte_len * 8 > self.bits_remaining() {
            return Err(invalid());
        }
        // Fits: the bytes lie inside the remaining window, which lies in `data`.
        let byte_len = byte_len as usize;

        if utf16 {
            let mut units16 = Vec::with_capacity(byte_len / 2);
            for _ in 0..byte_len / 2 {
                units16.push(self.read_u16()?);
            }
            if units16.last() == Some(&0) {
                units16.pop();
            }
            String::from_utf16(&units16).map_err(|_| BitError::InvalidString { position: start })
        } else {
            let mut bytes = Vec::with_capacity(byte_len);
            for _ in 0..byte_len {
                bytes.push(self.read_u8()?);
            }
            if bytes.last() == Some(&0) {
                bytes.pop();
            }
            String::from_utf8(bytes).map_err(|_| BitError::InvalidString { position: start })
        }
    }

    /// Copy `count` bits into `dst`, LSB-first, zero-filling the final byte's
    /// padding, and advance past them.
    ///
    /// Callers rely on the zero-fill: vrf-net's `decode_into_scratch` and
    /// `stage_fragment` reuse one buffer across blocks and hand the final byte
    /// on whole, and the payload transform leaves padding as it finds it.
    pub fn copy_bits_to(&mut self, dst: &mut [u8], count: u64) -> Result<()> {
        let byte_count =
            usize::try_from(count.div_ceil(8)).map_err(|_| BitError::InvalidLength {
                position: self.pos,
                length: count as i64,
            })?;
        if dst.len() < byte_count {
            return Err(BitError::InvalidBitLength {
                requested: count,
                available: (dst.len() as u64).saturating_mul(8),
            });
        }
        self.need(count)?;
        if count == 0 {
            return Ok(());
        }

        let abs = self.start_bit + self.pos;
        self.pos += count;
        let mut byte = (abs >> 3) as usize;
        let off = (abs & 7) as u32;

        // Each output word needs this word and the low `off` bits of the next,
        // which is the next iteration's first: carrying it halves the loads.
        // `<< (63 - off) << 1` is `<< (64 - off)` kept defined at `off == 0`. A
        // byte-aligned fast path measured neutral
        // (docs/PERFORMANCE_NOTES.md#copy_bits_to-the-byte-aligned-path).
        let full_words = (count / 64) as usize;
        let (words, tail) = dst[..byte_count].split_at_mut(full_words * 8);

        let mut carry = self.load_u64(byte);
        for out in words.chunks_exact_mut(8) {
            byte += 8;
            let next = self.load_u64(byte);
            let word = (carry >> off) | (next << (63 - off) << 1);
            out.copy_from_slice(&word.to_le_bytes());
            carry = next;
        }
        if !tail.is_empty() {
            // The mask zero-fills the padding; `tail.len()` is
            // `ceil(leftover / 8)`, so nothing past `byte_count` is written.
            let leftover = (count % 64) as u32;
            let next = self.load_u64(byte + 8);
            let word = ((carry >> off) | (next << (63 - off) << 1)) & mask_u64(leftover);
            // A runtime-length memcpy; a shift-and-peel loop measured neutral
            // (docs/PERFORMANCE_NOTES.md#copy_bits_to-the-tail-write-stays-a-memcpy).
            tail.copy_from_slice(&word.to_le_bytes()[..tail.len()]);
        }
        Ok(())
    }

    /// A view over the next `count` bits, sharing the buffer; advances past
    /// them. `#[inline]` because out of line, callers built the child in
    /// memory instead of in registers.
    #[inline]
    pub fn sub_reader(&mut self, count: u64) -> Result<BitReader<'a>> {
        self.need(count)?;
        let child = BitReader {
            data: self.data,
            start_bit: self.start_bit + self.pos,
            pos: 0,
            len: count,
        };
        self.pos += count;
        Ok(child)
    }

    /// Skip `count` bits.
    #[inline]
    pub fn skip_bits(&mut self, count: u64) -> Result<()> {
        self.need(count)?;
        self.pos += count;
        Ok(())
    }

    /// Skip the rest of the window.
    #[inline]
    pub fn skip_remaining(&mut self) {
        self.pos = self.len;
    }
}

/// [`BitReader::load_u64`] with fewer than 8 bytes left: absent bytes read as
/// zero. A free function so the reader is never address-taken (neutral here,
/// ~2% on the EOF path):
/// docs/PERFORMANCE_NOTES.md#cold-path-builders-stay-free-functions.
#[cold]
#[inline(never)]
fn load_u64_padded(data: &[u8], byte: usize) -> u64 {
    let mut buf = [0u8; 8];
    if let Some(tail) = data.get(byte..) {
        let n = tail.len().min(8);
        buf[..n].copy_from_slice(&tail[..n]);
    }
    u64::from_le_bytes(buf)
}

/// The low `count` bits set, `count` in `1..=64`. A right shift needs no
/// special case at 64, where `(1 << count) - 1` overflows. Zero would be an
/// over-wide shift: `read_bits` returns early on it, and `copy_bits_to` reaches
/// its tail only when `count % 64 != 0`. The `debug_assert` is live under
/// `profile.test`, which the corpus tests run.
#[inline]
const fn mask_u64(count: u32) -> u64 {
    debug_assert!(count >= 1 && count <= 64, "mask width must be 1..=64");
    u64::MAX >> (64 - count)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pseudo-random bytes: constant ones hide shift and mask mistakes.
    fn pattern(len: u32) -> Vec<u8> {
        (0..len)
            .map(|i| (i.wrapping_mul(97) as u8) ^ 0x99)
            .collect()
    }

    /// Bit `i` of `data`, by the module doc's definition.
    fn bit(data: &[u8], i: u64) -> u8 {
        (data[(i >> 3) as usize] >> (i & 7)) & 1
    }

    /// One bit at a time: a reference with none of the fast reader's folding
    /// (aligned or not, one word or two, padded tail), covering `(offset,
    /// width)` pairs a replay corpus never hits.
    fn reference_bits(data: &[u8], start: u64, count: u32) -> u64 {
        let mut value = 0u64;
        for i in 0..count {
            value |= u64::from(bit(data, start + u64::from(i))) << i;
        }
        value
    }

    #[test]
    fn read_bits_matches_the_reference_at_every_offset_and_width() {
        let data = pattern(40);
        for off in 0..8u64 {
            for count in 0..=64u32 {
                let mut r = BitReader::new(&data);
                r.skip_bits(off).unwrap();
                assert_eq!(
                    r.read_bits(count).unwrap(),
                    reference_bits(&data, off, count),
                    "off={off} count={count}"
                );
                assert_eq!(r.position(), off + u64::from(count));
            }
        }
    }

    #[test]
    fn read_bits_is_exact_where_the_buffer_runs_out() {
        // The padded `load_u64` path: its zeros must never reach the value.
        let data = pattern(40);
        let total = (data.len() as u64) * 8;
        for count in 1..=64u32 {
            let start = total - u64::from(count);
            let mut r = BitReader::new(&data);
            r.skip_bits(start).unwrap();
            assert_eq!(
                r.read_bits(count).unwrap(),
                reference_bits(&data, start, count),
                "count={count}"
            );
            assert!(r.at_end());
        }
    }

    #[test]
    fn read_bits_is_exact_in_a_buffer_smaller_than_one_word() {
        // Every load is padded, and the window ends mid-byte.
        let data = [0xBFu8, 0x5C, 0xE1];
        for bit_len in 1..=24u64 {
            for off in 0..8u64.min(bit_len) {
                let width = (bit_len - off) as u32;
                let mut r = BitReader::with_bit_len(&data, bit_len).unwrap();
                r.skip_bits(off).unwrap();
                assert_eq!(
                    r.read_bits(width).unwrap(),
                    reference_bits(&data, off, width),
                    "bit_len={bit_len} off={off}"
                );
                assert!(r.read_bit().is_err());
            }
        }
    }

    #[test]
    fn copy_bits_to_matches_the_reference_and_stays_inside_byte_count() {
        let data = pattern(64);
        for off in 0..8u64 {
            // 0 is a production input (a zero-bit field payload), and the one
            // count that must not touch `pos` or the window arithmetic.
            for count in [0u64, 1, 7, 8, 9, 63, 64, 65, 71, 72, 127, 128, 200] {
                let byte_count = count.div_ceil(8) as usize;
                // Guard bytes past `byte_count` catch a loop that overruns.
                let mut dst = vec![0xAAu8; byte_count + 4];
                let mut r = BitReader::new(&data);
                r.skip_bits(off).unwrap();
                r.copy_bits_to(&mut dst, count).unwrap();

                for i in 0..count {
                    assert_eq!(
                        bit(&dst, i),
                        bit(&data, off + i),
                        "off={off} count={count} bit={i}"
                    );
                }

                // Padding above `count` must be zero; see `copy_bits_to`.
                let pad = (byte_count as u64) * 8 - count;
                if pad > 0 {
                    assert_eq!(
                        dst[byte_count - 1] >> (8 - pad),
                        0,
                        "off={off} count={count}"
                    );
                }
                assert!(
                    dst[byte_count..].iter().all(|&b| b == 0xAA),
                    "off={off} count={count} wrote past byte_count"
                );
                assert_eq!(r.position(), off + count);
            }
        }
    }

    #[test]
    fn copy_bits_to_is_exact_up_to_the_last_bit_of_the_buffer() {
        // Ending flush with the buffer forces the padded load on the final word.
        let data = pattern(37);
        let total = (data.len() as u64) * 8;
        for count in [1u64, 8, 33, 64, 65, 128, 200] {
            let start = total - count;
            let byte_count = count.div_ceil(8) as usize;
            let mut dst = vec![0xAAu8; byte_count + 4];
            let mut r = BitReader::new(&data);
            r.skip_bits(start).unwrap();
            r.copy_bits_to(&mut dst, count).unwrap();
            for i in 0..count {
                assert_eq!(bit(&dst, i), bit(&data, start + i), "count={count} bit={i}");
            }
            assert!(
                dst[byte_count..].iter().all(|&b| b == 0xAA),
                "count={count}"
            );
        }
    }

    #[test]
    fn copy_bits_to_reports_a_small_destination_before_the_stream() {
        // A wrongly sized `dst` is a call-site bug: it must be reported ahead
        // of the recoverable Eof the short stream would also give.
        let data = [0xFFu8];
        let mut r = BitReader::new(&data);
        let mut dst = [0u8; 1];
        assert_eq!(
            r.copy_bits_to(&mut dst, 64).unwrap_err(),
            BitError::InvalidBitLength {
                requested: 64,
                available: 8,
            }
        );
        assert_eq!(r.position(), 0);
    }

    #[test]
    fn reads_least_significant_bit_first() {
        let data = [0b1010_0101u8];
        let mut r = BitReader::new(&data);
        let bits: Vec<bool> = (0..8).map(|_| r.read_bit().unwrap()).collect();
        assert_eq!(
            bits,
            vec![true, false, true, false, false, true, false, true]
        );
        assert!(r.at_end());
    }

    #[test]
    fn eof_is_reported_not_padded() {
        let data = [0xFFu8];
        let mut r = BitReader::new(&data);
        r.skip_bits(6).unwrap();
        let err = r.read_bits(8).unwrap_err();
        assert_eq!(
            err,
            BitError::Eof {
                position: 6,
                length: 8,
                requested: 8
            }
        );
    }

    #[test]
    fn int_packed_single_byte() {
        let data = [0x3F << 1];
        let mut r = BitReader::new(&data);
        assert_eq!(r.read_int_packed().unwrap(), 0x3F);
        assert_eq!(r.position(), 8);
    }

    #[test]
    fn int_packed_multi_byte_is_little_endian_in_chunks() {
        // value 300 = 0b1_0010_1100 -> chunk0 = 0b010_1100 (44), chunk1 = 0b10 (2)
        // byte0 = 44 << 1 | 1 (more), byte1 = 2 << 1 (last)
        let data = [(44u8 << 1) | 1, 2u8 << 1];
        let mut r = BitReader::new(&data);
        assert_eq!(r.read_int_packed().unwrap(), 300);
        assert_eq!(r.position(), 16);
    }

    #[test]
    fn int_packed_rejects_a_fifth_chunk_that_overflows_u32() {
        // Four empty continuation chunks, then 16 at shift 28 (see the method).
        let data = [0x01u8, 0x01, 0x01, 0x01, 0x20];
        let mut r = BitReader::new(&data);
        assert_eq!(
            r.read_int_packed().unwrap_err(),
            BitError::IntPackedOverflow { position: 0 }
        );
    }

    #[test]
    fn int_packed_runaway_outranks_overflow() {
        // The fifth byte both continues and overflows; callers classify blocks
        // by error shape, so the runaway must win.
        let data = [0xFFu8; 8];
        let mut r = BitReader::new(&data);
        assert_eq!(
            r.read_int_packed().unwrap_err(),
            BitError::MalformedIntPacked { position: 0 }
        );
        assert_eq!(r.position(), 40, "all five bytes are still consumed");
    }

    #[test]
    fn int_packed_accepts_the_largest_representable_fifth_chunk() {
        // u32::MAX: four 0x7F chunks and a fifth of 15, the widest a real
        // encoder emits.
        let data = [0xFFu8, 0xFF, 0xFF, 0xFF, 15 << 1];
        let mut r = BitReader::new(&data);
        assert_eq!(r.read_int_packed().unwrap(), u32::MAX);
        assert_eq!(r.position(), 40);
    }

    #[test]
    fn int_packed_is_bit_aligned_not_byte_aligned() {
        // Chunks are read from the bit position, not the next byte.
        let data = (u16::from(0x3Fu8 << 1) << 1).to_le_bytes();
        let mut r = BitReader::new(&data);
        r.skip_bits(1).unwrap();
        assert_eq!(r.read_int_packed().unwrap(), 0x3F);
    }

    #[test]
    fn serialized_int_spends_log2_bits_then_maybe_one_more() {
        // max 4: 2 bits, and every value + 4 >= 4, so no third bit.
        let data = [0b11u8];
        let mut r = BitReader::new(&data);
        assert_eq!(r.read_serialized_int(4).unwrap(), 3);
        assert_eq!(r.position(), 2);

        // max 5: 2 bits read 0, and 0 + 4 < 5, so a third bit raises it to 4.
        let data = [0b100u8];
        let mut r = BitReader::new(&data);
        assert_eq!(r.read_serialized_int(5).unwrap(), 4);
        assert_eq!(r.position(), 3);
    }

    #[test]
    fn serialized_int_max_one_consumes_nothing() {
        let data = [0xFFu8];
        let mut r = BitReader::new(&data);
        assert_eq!(r.read_serialized_int(1).unwrap(), 0);
        assert_eq!(r.position(), 0);
    }

    #[test]
    fn serialized_int_rejects_zero_max() {
        let data = [0xFFu8];
        let mut r = BitReader::new(&data);
        assert_eq!(
            r.read_serialized_int(0).unwrap_err(),
            BitError::InvalidSerializedIntMax { max: 0 }
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn fstring_utf8_strips_null() {
        let mut data = Vec::new();
        data.extend_from_slice(&4i32.to_le_bytes());
        data.extend_from_slice(b"abc\0");
        let mut r = BitReader::new(&data);
        assert_eq!(r.read_fstring(1024).unwrap(), "abc");
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn fstring_utf16_uses_negative_length() {
        let mut data = Vec::new();
        data.extend_from_slice(&(-3i32).to_le_bytes());
        for u in ['h' as u16, 'i' as u16, 0u16] {
            data.extend_from_slice(&u.to_le_bytes());
        }
        let mut r = BitReader::new(&data);
        assert_eq!(r.read_fstring(1024).unwrap(), "hi");
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn fstring_empty() {
        let data = 0i32.to_le_bytes();
        let mut r = BitReader::new(&data);
        assert_eq!(r.read_fstring(1024).unwrap(), "");
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn fstring_rejects_oversized_length() {
        let data = 1_000_000i32.to_le_bytes();
        let mut r = BitReader::new(&data);
        assert!(matches!(
            r.read_fstring(64).unwrap_err(),
            BitError::InvalidLength { .. }
        ));
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn fstring_rejects_declared_bytes_beyond_remaining_input_before_allocation() {
        let data = 1_000_000i32.to_le_bytes();
        let mut r = BitReader::new(&data);

        assert_eq!(
            r.read_fstring(1_000_000).unwrap_err(),
            BitError::InvalidLength {
                position: 0,
                length: 1_000_000,
            }
        );
        assert_eq!(r.position(), 32);
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn fstring_rejects_negative_maximum_even_for_empty_string() {
        let data = 0i32.to_le_bytes();
        let mut r = BitReader::new(&data);

        assert_eq!(
            r.read_fstring(-1).unwrap_err(),
            BitError::InvalidLength {
                position: 0,
                length: 0,
            }
        );
    }

    #[test]
    fn sub_reader_is_a_window_that_advances_the_parent() {
        let data = [0xFFu8, 0x00, 0xFF];
        let mut parent = BitReader::new(&data);
        let mut child = parent.sub_reader(12).unwrap();
        assert_eq!(child.len_bits(), 12);
        assert_eq!(parent.position(), 12);
        assert_eq!(child.read_bits(12).unwrap(), 0x0FF);
        assert!(child.at_end());
        // The child cannot see past its window even though the buffer continues.
        assert!(child.read_bit().is_err());
    }

    #[test]
    fn sub_reader_of_sub_reader_keeps_absolute_offset() {
        let data = [0x00u8, 0xFF, 0x00];
        let mut parent = BitReader::new(&data);
        parent.skip_bits(8).unwrap();
        let mut child = parent.sub_reader(8).unwrap();
        let mut grandchild = child.sub_reader(4).unwrap();
        assert_eq!(grandchild.read_bits(4).unwrap(), 0xF);
    }

    #[test]
    fn with_bit_len_hides_trailing_padding() {
        let data = [0xFFu8, 0xFF];
        let mut r = BitReader::with_bit_len(&data, 12).unwrap();
        assert_eq!(r.bits_remaining(), 12);
        r.skip_bits(12).unwrap();
        assert!(r.read_bit().is_err());
    }

    #[test]
    fn with_bit_len_does_not_panic_when_length_exceeds_input() {
        assert_eq!(
            BitReader::with_bit_len(&[0u8], 9).unwrap_err(),
            BitError::InvalidBitLength {
                requested: 9,
                available: 8,
            }
        );
    }
}
