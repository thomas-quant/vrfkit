//! `++Ares-Core+release-13.06`

use super::SeededTransform;
use crate::helpers::*;
use crate::sbox::*;

pub struct V13_06;

impl SeededTransform for V13_06 {
    const BRANCH: &'static str = "++Ares-Core+release-13.06";
    const SEED_ADDEND: u32 = 0xe974_593c;
    const INIT_A_OFFSET: u32 = 0x3c;
    const ADD_OFFSET: bool = true;

    fn word64(mut v: u64, state: u32) -> u64 {
        v = swap_adjacent_bits_u64(v) ^ !u64::from(state.rotate_right(7));
        v = reverse_bits64_without_final_16bit_swap(swap_adjacent_bits_u64(v));
        v = substitute_bytes_u64(v, &SBOX_64);
        v = u64::from(state.rotate_right(2)).wrapping_add(!v);
        substitute_bytes_u64(v, &SBOX_64)
    }

    fn word32(mut v: u32, state: u32) -> u32 {
        v = state.rotate_left(7) ^ swap_adjacent_bits_u32(v);
        v = swap_adjacent_bits_u32(v).reverse_bits();
        v = substitute_bytes_u32(v, &SBOX_32);
        v = (!v).wrapping_add(state.rotate_left(2));
        substitute_bytes_u32(v, &SBOX_32)
    }

    fn byte(mut v: u8, state: u32) -> u8 {
        let state_byte = (state as u8).wrapping_mul(0x79);
        v = swap_adjacent_bits_u8(v) ^ state_byte.wrapping_mul(0x1b);
        v = v.reverse_bits();
        v = SBOX_8[swap_adjacent_bits_u8(v) as usize];
        SBOX_8[(!v).wrapping_add(state_byte) as usize]
    }
}
