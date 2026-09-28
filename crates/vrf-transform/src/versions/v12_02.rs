//! `++Ares-Core+release-12.02`, recovered from the native seeded reader.

use super::SeededTransform;
use crate::helpers::*;
use crate::sbox::*;

pub struct V12_02;

impl SeededTransform for V12_02 {
    const BRANCH: &'static str = "++Ares-Core+release-12.02";
    const SEED_ADDEND: u32 = 0x9830d09d;
    const INIT_A_OFFSET: u32 = 0x1d;
    const ADD_OFFSET: bool = true;
    const TAIL_XOR: u8 = 0x9d;

    fn word64(mut v: u64, state: u32) -> u64 {
        v = v.rotate_left(state.rotate_right(8) % 63 + 1);
        v = v.rotate_right(state.rotate_right(7) % 63 + 1);
        v = reverse_bits64_without_final_16bit_swap(v);
        v = swap_adjacent_bits_u64(v);
        v = substitute_bytes_u64(v, &SBOX_64);
        v = reverse_bits64_without_final_16bit_swap(v);
        v = v.wrapping_sub(u64::from(state.rotate_right(2)));
        reverse_bits64_without_final_16bit_swap(v)
    }

    fn word32(mut v: u32, state: u32) -> u32 {
        v = v.rotate_left(state.rotate_left(8) % 31 + 1);
        v = v.rotate_right(state.rotate_left(7) % 31 + 1);
        v = v.reverse_bits();
        v = swap_adjacent_bits_u32(v);
        v = substitute_bytes_u32(v, &SBOX_32);
        v = v.reverse_bits();
        v = v.wrapping_sub(state.rotate_left(2));
        v.reverse_bits()
    }

    fn byte(mut v: u8, state: u32) -> u8 {
        v = v.rotate_left(state.wrapping_mul(0x0cc6db61) % 7 + 1);
        v = v.rotate_right(state.wrapping_mul(0x012959c3) % 7 + 1);
        v = v.reverse_bits();
        v = swap_adjacent_bits_u8(v);
        v = SBOX_8[v as usize];
        v = v.reverse_bits();
        v = v.wrapping_sub(state.wrapping_mul(0x00000079) as u8);
        v.reverse_bits()
    }
}
