//! Per-build transform definitions, one file per build so `git log` on a file
//! shows only that build. The files are near-identical in shape, which hides a
//! copy-paste slip from review; what catches one is the build's golden or
//! native vectors (`tests/golden.rs`), required at every staging boundary.
//!
//! `TAIL_XOR == SEED_ADDEND as u8` in all 24 builds, hence the default; a build
//! that broke it would fail its 1- and 7-bit vectors.
//!
//! # Keys
//!
//! A word function's step `k` (1..=8) keys `word64` with
//! `state.rotate_right(k)`, `word32` with `state.rotate_left(k)` and `byte`
//! with `state.wrapping_mul(11^k)`: the u32 product for a rotation count, its
//! low byte otherwise. The three lanes use the same steps (13.04's and 13.05's
//! `byte` swap two adjacent commutative ones). 11^1..11^8 are `0x0b`, `0x79`,
//! `0x533`, `0x3931`, `0x2751b`, `0x1b0829`, `0x12959c3` and `0x0cc6db61`; the
//! 12.10+ ports fold the byte multipliers into low bytes, product chains or
//! sums, e.g. 12.11's `0x23` = -(11^4 + 11^3 + 11^2) mod 256.
//!
//! A `word64` XOR operand is complemented after zero-extension,
//! `!u64::from(..)`, so its upper 32 bits are ones; negating before widening
//! would differ. `word32` and `byte` XOR operands are not complemented. 13.00
//! is the exception: its `word64` has no complement, and its `word32` and
//! `byte` complement the value they XOR instead.
//!
//! Checked against all 24 files on 2026-09-28 by a script that maps every key
//! to its step (rotation direction, or multiplier with folds evaluated) and
//! records each XOR operand's complement.

use crate::helpers::initial_prng_a;

mod v11_06;
mod v11_07;
mod v11_08;
mod v11_09;
mod v11_10;
mod v11_11;
mod v12_00;
mod v12_01;
mod v12_02;
mod v12_03;
mod v12_04;
mod v12_05;
mod v12_06;
mod v12_07;
mod v12_08;
mod v12_09;
mod v12_10;
mod v12_11;
mod v13_00;
mod v13_01;
mod v13_02;
mod v13_04;
mod v13_05;
mod v13_06;

pub use v11_06::V11_06;
pub use v11_07::V11_07;
pub use v11_08::V11_08;
pub use v11_09::V11_09;
pub use v11_10::V11_10;
pub use v11_11::V11_11;
pub use v12_00::V12_00;
pub use v12_01::V12_01;
pub use v12_02::V12_02;
pub use v12_03::V12_03;
pub use v12_04::V12_04;
pub use v12_05::V12_05;
pub use v12_06::V12_06;
pub use v12_07::V12_07;
pub use v12_08::V12_08;
pub use v12_09::V12_09;
pub use v12_10::V12_10;
pub use v12_11::V12_11;
pub use v13_00::V13_00;
pub use v13_01::V13_01;
pub use v13_02::V13_02;
pub use v13_04::V13_04;
pub use v13_05::V13_05;
pub use v13_06::V13_06;

/// One build's payload transform. A trait with associated constants, so the
/// driver monomorphises: no dispatch inside the word loops, which run per 8
/// payload bytes of every content block (~780k blocks per replay).
pub trait SeededTransform {
    /// Replay branch string this transform decodes, e.g. `++Ares-Core+release-13.01`.
    const BRANCH: &'static str;
    /// Added to the seed when deriving the first PRNG lane.
    const SEED_ADDEND: u32;
    /// Offset applied to the raw seed when deriving the first PRNG lane.
    const INIT_A_OFFSET: u32;
    /// Whether the offset is added (`true`) or subtracted (`false`).
    const ADD_OFFSET: bool = false;
    /// XORed into the final partial byte alongside the keystream byte.
    const TAIL_XOR: u8 = Self::SEED_ADDEND as u8;

    /// Seed the first PRNG lane.
    #[must_use]
    fn initial_prng_a(seed: u32) -> u64 {
        initial_prng_a(
            seed,
            Self::SEED_ADDEND,
            Self::INIT_A_OFFSET,
            Self::ADD_OFFSET,
        )
    }

    /// Transform one aligned 64-bit word.
    #[must_use]
    fn word64(value: u64, state: u32) -> u64;
    /// Transform one aligned 32-bit word.
    #[must_use]
    fn word32(value: u32, state: u32) -> u32;
    /// Transform one byte.
    #[must_use]
    fn byte(value: u8, state: u32) -> u8;
}
