//! `++Ares-Core+release-11.07`, recovered from the native seeded reader.

use super::SeededTransform;
use crate::helpers::*;
use crate::sbox::*;

pub struct V11_07;

impl SeededTransform for V11_07 {
    const BRANCH: &'static str = "++Ares-Core+release-11.07";
    const SEED_ADDEND: u32 = 0x17b077d3;
    const INIT_A_OFFSET: u32 = 0x2d;
    const TAIL_XOR: u8 = 0xd3;

    fn word64(mut v: u64, state: u32) -> u64 {
        v ^= !u64::from(state.rotate_right(8));
        v = substitute_bytes_u64(v, &SBOX_64);
        v = reverse_bits64_without_final_16bit_swap(v);
        v = !v;
        v = swap_adjacent_bits_u64(v);
        v ^= !u64::from(state.rotate_right(1));
        v
    }

    fn word32(mut v: u32, state: u32) -> u32 {
        v ^= state.rotate_left(8);
        v = substitute_bytes_u32(v, &SBOX_32);
        v = v.reverse_bits();
        v = !v;
        v = swap_adjacent_bits_u32(v);
        v ^= state.rotate_left(1);
        v
    }

    fn byte(mut v: u8, state: u32) -> u8 {
        v ^= state.wrapping_mul(0x0cc6db61) as u8;
        v = SBOX_8[v as usize];
        v = v.reverse_bits();
        v = !v;
        v = swap_adjacent_bits_u8(v);
        v ^= state.wrapping_mul(0x0000000b) as u8;
        v
    }
}
