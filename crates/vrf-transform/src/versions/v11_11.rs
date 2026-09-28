//! `++Ares-Core+release-11.11`, recovered from the native seeded reader.

use super::SeededTransform;
use crate::helpers::*;
use crate::sbox::*;

pub struct V11_11;

impl SeededTransform for V11_11 {
    const BRANCH: &'static str = "++Ares-Core+release-11.11";
    const SEED_ADDEND: u32 = 0xc4445c41;
    const INIT_A_OFFSET: u32 = 0x3f;

    fn word64(mut v: u64, state: u32) -> u64 {
        v ^= !u64::from(state.rotate_right(8));
        v = substitute_bytes_u64(v, &SBOX_64);
        v = v.rotate_left(state.rotate_right(6) % 63 + 1);
        v = swap_adjacent_bits_u64(v);
        v = substitute_bytes_u64(v, &SBOX_64);
        v.rotate_left(state.rotate_right(1) % 63 + 1)
    }

    fn word32(mut v: u32, state: u32) -> u32 {
        v ^= state.rotate_left(8);
        v = substitute_bytes_u32(v, &SBOX_32);
        v = v.rotate_left(state.rotate_left(6) % 31 + 1);
        v = swap_adjacent_bits_u32(v);
        v = substitute_bytes_u32(v, &SBOX_32);
        v.rotate_left(state.rotate_left(1) % 31 + 1)
    }

    fn byte(mut v: u8, state: u32) -> u8 {
        v ^= state.wrapping_mul(0x0cc6db61) as u8;
        v = SBOX_8[v as usize];
        v = v.rotate_left(state.wrapping_mul(0x001b0829) % 7 + 1);
        v = swap_adjacent_bits_u8(v);
        v = SBOX_8[v as usize];
        v.rotate_left(state.wrapping_mul(0x0000000b) % 7 + 1)
    }
}
