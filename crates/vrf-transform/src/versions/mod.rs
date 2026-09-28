//! Per-build transform definitions.
//!
//! Each build supplies exactly two constants and three word functions. Every
//! other moving part -- PRNG, staging, tail XOR -- is shared, so a diff between
//! two builds shows precisely what Riot rotated.
//!
//! ## Observed shape of a build change
//!
//! | build | seed addend | offset | offset sign | S-box |
//! |---|---|---|---|---|
//! | release-11.06 | `0x3325e3bd` | `0x3d` | **add** | yes |
//! | release-11.07 | `0x17b077d3` | `0x2d` | subtract | yes |
//! | release-11.08 | `0xacf2cdff` | `0x01` | subtract | no |
//! | release-11.09 | `0x12cf14e5` | `0x1b` | subtract | yes |
//! | release-11.10 | `0x34e9d3ec` | `0x14` | subtract | yes |
//! | release-11.11 | `0xc4445c41` | `0x3f` | subtract | yes |
//! | release-12.00 | `0x70876679` | `0x07` | subtract | no |
//! | release-12.01 | `0x13fdd831` | `0x31` | **add** | yes |
//! | release-12.02 | `0x9830d09d` | `0x1d` | **add** | yes |
//! | release-12.03 | `0x33d59dff` | `0x01` | subtract | yes |
//! | release-12.04 | `0xa5684b42` | `0x3e` | subtract | yes |
//! | release-12.05 | `0xc21d548c` | `0x0c` | **add** | yes |
//! | release-12.06 | `0x8d686ca6` | `0x26` | **add** | no |
//! | release-12.07 | `0x2d21d7c3` | `0x3d` | subtract | yes |
//! | release-12.08 | `0xce2e33e5` | `0x1b` | subtract | yes |
//! | release-12.09 | `0x7ff2feec` | `0x14` | subtract | no |
//! | release-12.10 | `0x12fd0ee5` | `0x1b` | subtract | no |
//! | release-12.11 | `0x409d36a3` | `0x23` | **add** | no |
//! | release-13.00 | `0x2949b6ef` | `0x11` | subtract | yes |
//! | release-13.01 | `0xe62fcd5c` | `0x24` | subtract | no |
//! | release-13.02 | `0x9e81a37c` | `0x04` | subtract | yes |
//! | release-13.04 | `0x076dc658` | `0x28` | subtract | no |
//! | release-13.05 | `0x48c26613` | `0x13` | **add** | no |
//! | release-13.06 | `0xe974593c` | `0x3c` | **add** | yes |
//!
//! In every recovered build, `TAIL_XOR == SEED_ADDEND & 0xff`, so that is the
//! trait default and a build that breaks the pattern overrides it. A wrong tail
//! byte fails the build's 1- and 7-bit vectors, which every build must carry.
//!
//! ## One file per build
//!
//! The per-build `impl`s are deliberately kept in separate files. They are near-
//! identical in shape and differ only in the order of a handful of bit
//! primitives, which is exactly the situation where a copy-paste error is
//! invisible in review; a per-build file makes `git log` on one build show only
//! that build's history. The per-build vectors in `tests/golden.rs` and
//! `tests/native.rs` are what catch such an error: every registered build must
//! carry vectors at each staging boundary, so all three word functions are
//! checked byte for byte.

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

/// One build's payload transform.
///
/// Implemented as a trait with associated constants so the driver monomorphises:
/// there is no virtual dispatch inside the per-word loops, which run once per
/// 8 bytes of every content block (~780k blocks per replay).
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
