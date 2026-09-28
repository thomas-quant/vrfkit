//! `++Ares-Core+release-11.06`, recovered from the native seeded reader.

use super::SeededTransform;
use crate::helpers::*;
use crate::sbox::*;

pub struct V11_06;

impl SeededTransform for V11_06 {
    const BRANCH: &'static str = "++Ares-Core+release-11.06";
    const SEED_ADDEND: u32 = 0x3325e3bd;
    const INIT_A_OFFSET: u32 = 0x3d;
    const ADD_OFFSET: bool = true;
    const TAIL_XOR: u8 = 0xbd;

    fn word64(mut v: u64, state: u32) -> u64 {
        v = v.wrapping_add(u64::from(state.rotate_right(8)));
        v = substitute_bytes_u64(v, &SBOX_64);
        v ^= !u64::from(state.rotate_right(6));
        v = substitute_bytes_u64(v, &SBOX_64);
        v = substitute_bytes_u64(v, &SBOX_64);
        v = reverse_bits64_without_final_16bit_swap(v);
        swap_adjacent_bits_u64(v)
    }

    fn word32(mut v: u32, state: u32) -> u32 {
        v = v.wrapping_add(state.rotate_left(8));
        v = substitute_bytes_u32(v, &SBOX_32);
        v ^= state.rotate_left(6);
        v = substitute_bytes_u32(v, &SBOX_32);
        v = substitute_bytes_u32(v, &SBOX_32);
        v = v.reverse_bits();
        swap_adjacent_bits_u32(v)
    }

    fn byte(mut v: u8, state: u32) -> u8 {
        v = v.wrapping_add(state.wrapping_mul(0x0cc6db61) as u8);
        v = SBOX_8[v as usize];
        v ^= state.wrapping_mul(0x001b0829) as u8;
        v = SBOX_8[v as usize];
        v = SBOX_8[v as usize];
        v = v.reverse_bits();
        swap_adjacent_bits_u8(v)
    }
}
