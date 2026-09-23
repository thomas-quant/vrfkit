//! `++Ares-Core+release-13.06`
//!
//! Ported from upstream michel-giehl/ValorantReplayParser `ValorantSeededTransform13_06.cs`
//! (commit 7b4ef8a5, 2026-09-22); the golden vectors are upstream's own. Upstream writes the
//! first-lane offset as `seed + 0x3c`; it is expressed here in the subtracting form every build
//! but 12.11 uses, `seed - 0xffffffc4`, which is the same value mod 2^32 (13.05 did the same).

use super::SeededTransform;
use crate::helpers::{
    reverse_bits_u8, reverse_bits_u32, reverse_bits64_without_final_16bit_swap,
    substitute_bytes_u32, substitute_bytes_u64, swap_adjacent_bits_u8, swap_adjacent_bits_u32,
    swap_adjacent_bits_u64,
};
use crate::sbox::{SBOX_8, SBOX_32, SBOX_64};

/// `++Ares-Core+release-13.06`
pub struct V13_06;

impl SeededTransform for V13_06 {
    const BRANCH: &'static str = "++Ares-Core+release-13.06";
    const SEED_ADDEND: u32 = 0xe974_593c;
    const INIT_A_OFFSET: u32 = 0xffff_ffc4;
    const TAIL_XOR: u8 = 0x3c;

    fn word64(mut v: u64, state: u32) -> u64 {
        v = swap_adjacent_bits_u64(v) ^ !u64::from(state.rotate_right(7));
        v = reverse_bits64_without_final_16bit_swap(swap_adjacent_bits_u64(v));
        v = substitute_bytes_u64(v, &SBOX_64);
        v = u64::from(state.rotate_right(2)).wrapping_add(!v);
        substitute_bytes_u64(v, &SBOX_64)
    }

    fn word32(mut v: u32, state: u32) -> u32 {
        v = state.rotate_left(7) ^ swap_adjacent_bits_u32(v);
        v = reverse_bits_u32(swap_adjacent_bits_u32(v));
        v = substitute_bytes_u32(v, &SBOX_32);
        v = (!v).wrapping_add(state.rotate_left(2));
        substitute_bytes_u32(v, &SBOX_32)
    }

    fn byte(v: u8, state: u32) -> u8 {
        // C#: (byte)((sbyte)state * 0x79) -- only the low byte of the product survives, so the
        // sign of the narrowing cast does not matter.
        let state_byte = (state as u8).wrapping_mul(0x79);
        let mut v = swap_adjacent_bits_u8(v) ^ state_byte.wrapping_mul(0x1b);
        v = reverse_bits_u8(v);
        v = SBOX_8[swap_adjacent_bits_u8(v) as usize];
        SBOX_8[(!v).wrapping_add(state_byte) as usize]
    }
}
