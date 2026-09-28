//! Payload transforms for VALORANT replay content blocks.
//!
//! Only a content block's payload is transformed; block headers and their
//! declared bit lengths are plaintext, so a replay is framed sequentially and
//! the per-block decode can run in parallel. The key comes from the stream:
//! `seed = (bit_count as u32) ^ actor_net_guid` ([`seed_for`]).
//!
//! The skeleton is shared from release-11.06 to release-13.06. A build supplies
//! its branch, `SEED_ADDEND`, `INIT_A_OFFSET`, optionally `ADD_OFFSET` and
//! `TAIL_XOR` (both defaulted), and three word functions; see [`versions`].
//!
//! # Example
//!
//! ```
//! use vrf_bitio::BitReader;
//! use vrf_transform::{TransformVersion, seed_for};
//!
//! let payload = [0xBFu8, 0xDF, 0x6F];
//! let bit_count = 20;
//! let version = TransformVersion::from_branch("++Ares-Core+release-13.01").unwrap();
//!
//! let mut reader = BitReader::new(&payload);
//! let mut out = vec![0u8; TransformVersion::output_byte_count(bit_count)];
//! version.decode_from(&mut reader, bit_count, seed_for(bit_count, 2), &mut out).unwrap();
//! ```
//!
//! No Cargo features: per-build gating would remove publicly named
//! [`TransformVersion`] variants, and the only sizeable data is the three
//! shared S-box tables.

#![forbid(unsafe_code)]

pub mod helpers;
pub mod sbox;
pub mod versions;

use versions::{
    SeededTransform, V11_06, V11_07, V11_08, V11_09, V11_10, V11_11, V12_00, V12_01, V12_02,
    V12_03, V12_04, V12_05, V12_06, V12_07, V12_08, V12_09, V12_10, V12_11, V13_00, V13_01, V13_02,
    V13_04, V13_05, V13_06,
};
use vrf_bitio::{BitError, BitReader, Result as BitResult};

/// The transform seed for a content block: its declared payload length in
/// bits, XOR the network GUID of the actor channel carrying it.
#[must_use]
#[inline]
pub const fn seed_for(bit_count: usize, actor_net_guid: u32) -> u32 {
    (bit_count as u32) ^ actor_net_guid
}

/// Run a build's transform over `buf`, which holds the payload's bits
/// LSB-first as [`BitReader::copy_bits_to`] writes them. The final byte's
/// padding is left as found (the tail XOR is masked); callers that hand that
/// byte on whole rely on `copy_bits_to` having zeroed it.
///
/// Staged 64 bits at a time, then 32, then 8, then the last 1..7, with one PRNG
/// advance per stage iteration. The staging order is part of the format.
pub fn transform_in_place<T: SeededTransform>(
    buf: &mut [u8],
    bit_count: usize,
    seed: u32,
) -> BitResult<()> {
    if bit_count == 0 {
        return Ok(());
    }
    let available = (buf.len() as u64).saturating_mul(8);
    if bit_count as u64 > available {
        return Err(BitError::InvalidBitLength {
            requested: bit_count as u64,
            available,
        });
    }

    let mut state = seed;
    // Until the first advance the keystream byte is the seed's low byte; a
    // payload under 8 bits never advances.
    let mut stream_byte = seed as u8;
    let mut prng_a = T::initial_prng_a(seed);
    let mut prng_b = helpers::initial_prng_b(seed);
    let mut offset = 0usize;
    let mut left = bit_count;

    while left > 63 {
        let value = T::word64(helpers::read_u64(buf, offset), state);
        helpers::write_u64(buf, offset, value);
        stream_byte = helpers::advance_state(&mut state, &mut prng_a, &mut prng_b);
        offset += 8;
        left -= 64;
    }
    while left > 31 {
        let value = T::word32(helpers::read_u32(buf, offset), state);
        helpers::write_u32(buf, offset, value);
        stream_byte = helpers::advance_state(&mut state, &mut prng_a, &mut prng_b);
        offset += 4;
        left -= 32;
    }
    while left > 7 {
        buf[offset] = T::byte(buf[offset], state);
        stream_byte = helpers::advance_state(&mut state, &mut prng_a, &mut prng_b);
        offset += 1;
        left -= 8;
    }
    if left != 0 {
        // The mask confines the XOR to payload bits.
        let mask = 0xffu8 >> (7 - ((bit_count - 1) & 7));
        buf[offset] ^= mask & (stream_byte ^ T::TAIL_XOR);
    }
    Ok(())
}

/// A game build's payload transform, selected by replay branch string.
/// Dispatch is once per content block; each arm calls a monomorphised
/// [`transform_in_place`], so the word loops carry no indirection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TransformVersion {
    /// `++Ares-Core+release-11.06`
    V1106,
    /// `++Ares-Core+release-11.07`
    V1107,
    /// `++Ares-Core+release-11.08`
    V1108,
    /// `++Ares-Core+release-11.09`
    V1109,
    /// `++Ares-Core+release-11.10`
    V1110,
    /// `++Ares-Core+release-11.11`
    V1111,
    /// `++Ares-Core+release-12.00`
    V1200,
    /// `++Ares-Core+release-12.01`
    V1201,
    /// `++Ares-Core+release-12.02`
    V1202,
    /// `++Ares-Core+release-12.03`
    V1203,
    /// `++Ares-Core+release-12.04`
    V1204,
    /// `++Ares-Core+release-12.05`
    V1205,
    /// `++Ares-Core+release-12.06`
    V1206,
    /// `++Ares-Core+release-12.07`
    V1207,
    /// `++Ares-Core+release-12.08`
    V1208,
    /// `++Ares-Core+release-12.09`
    V1209,
    /// `++Ares-Core+release-12.10`
    V1210,
    /// `++Ares-Core+release-12.11`
    V1211,
    /// `++Ares-Core+release-13.00`
    V1300,
    /// `++Ares-Core+release-13.01`
    V1301,
    /// `++Ares-Core+release-13.02`
    V1302,
    /// `++Ares-Core+release-13.04`
    V1304,
    /// `++Ares-Core+release-13.05`
    V1305,
    /// `++Ares-Core+release-13.06`
    V1306,
}

/// Every transform this build of the crate knows about. A slice, so adding a
/// build does not change the public type; [`TransformVersion`] is
/// non-exhaustive for the same reason.
pub const ALL_VERSIONS: &[TransformVersion] = &[
    TransformVersion::V1106,
    TransformVersion::V1107,
    TransformVersion::V1108,
    TransformVersion::V1109,
    TransformVersion::V1110,
    TransformVersion::V1111,
    TransformVersion::V1200,
    TransformVersion::V1201,
    TransformVersion::V1202,
    TransformVersion::V1203,
    TransformVersion::V1204,
    TransformVersion::V1205,
    TransformVersion::V1206,
    TransformVersion::V1207,
    TransformVersion::V1208,
    TransformVersion::V1209,
    TransformVersion::V1210,
    TransformVersion::V1211,
    TransformVersion::V1300,
    TransformVersion::V1301,
    TransformVersion::V1302,
    TransformVersion::V1304,
    TransformVersion::V1305,
    TransformVersion::V1306,
];

/// A replay branch with no registered transform: reported, never guessed,
/// because a guessed transform yields plausible garbage instead of an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsupportedBranch {
    /// The replay branch that could not be matched.
    pub branch: String,
}

impl core::fmt::Display for UnsupportedBranch {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "no payload transform is registered for replay branch '{}'; known branches: {}",
            self.branch,
            ALL_VERSIONS
                .iter()
                .copied()
                .map(TransformVersion::branch)
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

impl core::error::Error for UnsupportedBranch {}

impl TransformVersion {
    /// Look up a transform by exact replay branch string.
    #[must_use]
    pub fn from_branch(branch: &str) -> Option<Self> {
        ALL_VERSIONS.iter().copied().find(|v| v.branch() == branch)
    }

    /// Look up a transform, returning a descriptive error when unknown.
    pub fn require(branch: &str) -> core::result::Result<Self, UnsupportedBranch> {
        Self::from_branch(branch).ok_or_else(|| UnsupportedBranch {
            branch: branch.to_owned(),
        })
    }

    /// The replay branch string this transform decodes.
    #[must_use]
    pub const fn branch(self) -> &'static str {
        match self {
            Self::V1106 => V11_06::BRANCH,
            Self::V1107 => V11_07::BRANCH,
            Self::V1108 => V11_08::BRANCH,
            Self::V1109 => V11_09::BRANCH,
            Self::V1110 => V11_10::BRANCH,
            Self::V1111 => V11_11::BRANCH,
            Self::V1200 => V12_00::BRANCH,
            Self::V1201 => V12_01::BRANCH,
            Self::V1202 => V12_02::BRANCH,
            Self::V1203 => V12_03::BRANCH,
            Self::V1204 => V12_04::BRANCH,
            Self::V1205 => V12_05::BRANCH,
            Self::V1206 => V12_06::BRANCH,
            Self::V1207 => V12_07::BRANCH,
            Self::V1208 => V12_08::BRANCH,
            Self::V1209 => V12_09::BRANCH,
            Self::V1210 => V12_10::BRANCH,
            Self::V1211 => V12_11::BRANCH,
            Self::V1300 => V13_00::BRANCH,
            Self::V1301 => V13_01::BRANCH,
            Self::V1302 => V13_02::BRANCH,
            Self::V1304 => V13_04::BRANCH,
            Self::V1305 => V13_05::BRANCH,
            Self::V1306 => V13_06::BRANCH,
        }
    }

    /// Bytes needed to hold `bit_count` bits.
    #[must_use]
    pub const fn output_byte_count(bit_count: usize) -> usize {
        bit_count.div_ceil(8)
    }

    /// Transform `buf` in place.
    pub fn apply(self, buf: &mut [u8], bit_count: usize, seed: u32) -> BitResult<()> {
        match self {
            Self::V1106 => transform_in_place::<V11_06>(buf, bit_count, seed),
            Self::V1107 => transform_in_place::<V11_07>(buf, bit_count, seed),
            Self::V1108 => transform_in_place::<V11_08>(buf, bit_count, seed),
            Self::V1109 => transform_in_place::<V11_09>(buf, bit_count, seed),
            Self::V1110 => transform_in_place::<V11_10>(buf, bit_count, seed),
            Self::V1111 => transform_in_place::<V11_11>(buf, bit_count, seed),
            Self::V1200 => transform_in_place::<V12_00>(buf, bit_count, seed),
            Self::V1201 => transform_in_place::<V12_01>(buf, bit_count, seed),
            Self::V1202 => transform_in_place::<V12_02>(buf, bit_count, seed),
            Self::V1203 => transform_in_place::<V12_03>(buf, bit_count, seed),
            Self::V1204 => transform_in_place::<V12_04>(buf, bit_count, seed),
            Self::V1205 => transform_in_place::<V12_05>(buf, bit_count, seed),
            Self::V1206 => transform_in_place::<V12_06>(buf, bit_count, seed),
            Self::V1207 => transform_in_place::<V12_07>(buf, bit_count, seed),
            Self::V1208 => transform_in_place::<V12_08>(buf, bit_count, seed),
            Self::V1209 => transform_in_place::<V12_09>(buf, bit_count, seed),
            Self::V1210 => transform_in_place::<V12_10>(buf, bit_count, seed),
            Self::V1211 => transform_in_place::<V12_11>(buf, bit_count, seed),
            Self::V1300 => transform_in_place::<V13_00>(buf, bit_count, seed),
            Self::V1301 => transform_in_place::<V13_01>(buf, bit_count, seed),
            Self::V1302 => transform_in_place::<V13_02>(buf, bit_count, seed),
            Self::V1304 => transform_in_place::<V13_04>(buf, bit_count, seed),
            Self::V1305 => transform_in_place::<V13_05>(buf, bit_count, seed),
            Self::V1306 => transform_in_place::<V13_06>(buf, bit_count, seed),
        }
    }

    /// Copy `bit_count` bits from `reader` into `out`, typically a reused
    /// scratch buffer, and transform them there.
    pub fn decode_from(
        self,
        reader: &mut BitReader<'_>,
        bit_count: usize,
        seed: u32,
        out: &mut [u8],
    ) -> BitResult<()> {
        reader.copy_bits_to(out, bit_count as u64)?;
        self.apply(
            &mut out[..Self::output_byte_count(bit_count)],
            bit_count,
            seed,
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_is_bit_count_xor_actor_guid() {
        assert_eq!(seed_for(287, 2), 287 ^ 2);
        assert_eq!(seed_for(0, 0), 0);
    }

    #[test]
    fn branch_lookup_round_trips() {
        for v in ALL_VERSIONS.iter().copied() {
            assert_eq!(TransformVersion::from_branch(v.branch()), Some(v));
        }
    }

    #[test]
    fn public_registry_type_does_not_encode_its_length() {
        // Fails to compile if the registry becomes a `[TransformVersion; N]`,
        // whose length would then be public API.
        let _: &'static [TransformVersion] = ALL_VERSIONS;
    }

    #[test]
    fn unknown_branch_is_an_error_naming_the_branch() {
        let err = TransformVersion::require("++Ares-Core+release-99.99").unwrap_err();
        assert_eq!(err.branch, "++Ares-Core+release-99.99");
        let text = err.to_string();
        assert!(text.contains("release-99.99"), "{text}");
        for v in ALL_VERSIONS {
            assert!(text.contains(v.branch()), "{text}");
        }
    }

    #[test]
    fn zero_bits_is_a_noop() {
        let mut buf = [0xAAu8; 4];
        for v in ALL_VERSIONS.iter().copied() {
            v.apply(&mut buf, 0, 1234).unwrap();
            assert_eq!(buf, [0xAAu8; 4]);
        }
    }

    #[test]
    fn apply_does_not_panic_on_an_undersized_buffer() {
        let mut buf = [0u8; 1];
        assert_eq!(
            TransformVersion::V1301.apply(&mut buf, 65, 0).unwrap_err(),
            BitError::InvalidBitLength {
                requested: 65,
                available: 8,
            }
        );
    }
}
