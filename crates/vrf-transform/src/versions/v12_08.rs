//! `++Ares-Core+release-12.08`, recovered from the native seeded reader.

use super::SeededTransform;
use crate::helpers::*;
use crate::sbox::*;

pub struct V12_08;

impl SeededTransform for V12_08 {
    const BRANCH: &'static str = "++Ares-Core+release-12.08";
    const SEED_ADDEND: u32 = 0xce2e33e5;
    const INIT_A_OFFSET: u32 = 0x1b;
    const TAIL_XOR: u8 = 0xe5;

    fn word64(mut v: u64, state: u32) -> u64 {
        v = v.rotate_left(state.rotate_right(7) % 63 + 1);
        v = substitute_bytes_u64(v, &SBOX_64);
        v ^= !u64::from(state.rotate_right(4));
        v = reverse_bits64_without_final_16bit_swap(v);
        v = v.wrapping_add(u64::from(state.rotate_right(2)));
        v.rotate_right(state.rotate_right(1) % 63 + 1)
    }

    fn word32(mut v: u32, state: u32) -> u32 {
        v = v.rotate_left(state.rotate_left(7) % 31 + 1);
        v = substitute_bytes_u32(v, &SBOX_32);
        v ^= state.rotate_left(4);
        v = v.reverse_bits();
        v = v.wrapping_add(state.rotate_left(2));
        v.rotate_right(state.rotate_left(1) % 31 + 1)
    }

    fn byte(mut v: u8, state: u32) -> u8 {
        v = v.rotate_left(state.wrapping_mul(0x012959c3) % 7 + 1);
        v = SBOX_8[v as usize];
        v ^= state.wrapping_mul(0x00003931) as u8;
        v = v.reverse_bits();
        v = v.wrapping_add(state.wrapping_mul(0x00000079) as u8);
        v.rotate_right(state.wrapping_mul(0x0000000b) % 7 + 1)
    }
}
