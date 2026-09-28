//! `++Ares-Core+release-11.10`, recovered from the native seeded reader.

use super::SeededTransform;
use crate::helpers::*;
use crate::sbox::*;

pub struct V11_10;

impl SeededTransform for V11_10 {
    const BRANCH: &'static str = "++Ares-Core+release-11.10";
    const SEED_ADDEND: u32 = 0x34e9d3ec;
    const INIT_A_OFFSET: u32 = 0x14;
    const TAIL_XOR: u8 = 0xec;

    fn word64(mut v: u64, state: u32) -> u64 {
        v = v.wrapping_sub(u64::from(state.rotate_right(8)));
        v ^= !u64::from(state.rotate_right(7));
        v = v.wrapping_sub(u64::from(state.rotate_right(6)));
        v = v.rotate_left(state.rotate_right(5) % 63 + 1);
        v = v.wrapping_sub(u64::from(state.rotate_right(4)));
        v = reverse_bits64_without_final_16bit_swap(v);
        v = substitute_bytes_u64(v, &SBOX_64);
        !v
    }

    fn word32(mut v: u32, state: u32) -> u32 {
        v = v.wrapping_sub(state.rotate_left(8));
        v ^= state.rotate_left(7);
        v = v.wrapping_sub(state.rotate_left(6));
        v = v.rotate_left(state.rotate_left(5) % 31 + 1);
        v = v.wrapping_sub(state.rotate_left(4));
        v = v.reverse_bits();
        v = substitute_bytes_u32(v, &SBOX_32);
        !v
    }

    fn byte(mut v: u8, state: u32) -> u8 {
        v = v.wrapping_sub(state.wrapping_mul(0x0cc6db61) as u8);
        v ^= state.wrapping_mul(0x012959c3) as u8;
        v = v.wrapping_sub(state.wrapping_mul(0x001b0829) as u8);
        v = v.rotate_left(state.wrapping_mul(0x0002751b) % 7 + 1);
        v = v.wrapping_sub(state.wrapping_mul(0x00003931) as u8);
        v = v.reverse_bits();
        v = SBOX_8[v as usize];
        !v
    }
}
