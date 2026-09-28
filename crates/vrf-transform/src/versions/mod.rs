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
//! In every recovered build, `TAIL_XOR == SEED_ADDEND & 0xff`. That is asserted per version
//! rather than assumed, so a future build that breaks the pattern fails a test
//! instead of silently corrupting the final partial byte of every payload.
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
    const TAIL_XOR: u8;

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

#[cfg(test)]
mod tests {
    use super::*;

    /// `(branch, seed addend, init-a offset, adds offset, tail xor)` for every
    /// registered build.
    ///
    /// Collected at runtime rather than compared as associated constants: a
    /// `assert_eq!` between two consts folds to a tautology that the compiler (and
    /// clippy) can see through, which defeats the point of checking it.
    fn build_table() -> Vec<(&'static str, u32, u32, bool, u8)> {
        fn row<T: SeededTransform>() -> (&'static str, u32, u32, bool, u8) {
            (
                T::BRANCH,
                T::SEED_ADDEND,
                T::INIT_A_OFFSET,
                T::ADD_OFFSET,
                T::TAIL_XOR,
            )
        }
        vec![
            row::<V11_06>(),
            row::<V11_07>(),
            row::<V11_08>(),
            row::<V11_09>(),
            row::<V11_10>(),
            row::<V11_11>(),
            row::<V12_00>(),
            row::<V12_01>(),
            row::<V12_02>(),
            row::<V12_03>(),
            row::<V12_04>(),
            row::<V12_05>(),
            row::<V12_06>(),
            row::<V12_07>(),
            row::<V12_08>(),
            row::<V12_09>(),
            row::<V12_10>(),
            row::<V12_11>(),
            row::<V13_00>(),
            row::<V13_01>(),
            row::<V13_02>(),
            row::<V13_04>(),
            row::<V13_05>(),
            row::<V13_06>(),
        ]
    }

    /// Across every known build the tail XOR byte is the low byte of the seed
    /// addend. Encoding that as a test (not as a derivation) means a future build
    /// that breaks the pattern is caught here rather than corrupting the last
    /// partial byte of every payload it decodes.
    #[test]
    fn tail_xor_is_low_byte_of_seed_addend() {
        for (branch, seed_addend, _, _, tail_xor) in build_table() {
            assert_eq!(
                tail_xor,
                (seed_addend & 0xff) as u8,
                "{branch}: TAIL_XOR should be the low byte of SEED_ADDEND {seed_addend:#010x}"
            );
        }
    }

    /// Compare each recovered offset sign with its native implementation.
    /// Pinning the exact set means a new transform that
    /// copy-pastes the wrong sign is caught here rather than in a corpus sweep.
    #[test]
    fn offset_signs_match_recovered_builds() {
        let adding: Vec<&str> = build_table()
            .into_iter()
            .filter(|row| row.3)
            .map(|row| row.0)
            .collect();
        assert_eq!(
            adding,
            vec![
                V11_06::BRANCH,
                V12_01::BRANCH,
                V12_02::BRANCH,
                V12_05::BRANCH,
                V12_06::BRANCH,
                V12_11::BRANCH,
                V13_05::BRANCH,
                V13_06::BRANCH
            ]
        );
    }

    #[test]
    fn build_constants_are_all_distinct() {
        // Two builds sharing a seed addend would almost certainly mean a
        // copy-paste error in a newly added transform.
        let table = build_table();
        for (i, a) in table.iter().enumerate() {
            for b in &table[i + 1..] {
                assert_ne!(a.1, b.1, "{} and {} share a seed addend", a.0, b.0);
            }
        }
    }
}
