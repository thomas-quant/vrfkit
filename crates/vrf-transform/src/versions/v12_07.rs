//! `++Ares-Core+release-12.07`, recovered from the native seeded reader.

use super::SeededTransform;
use crate::helpers::*;
use crate::sbox::*;

pub struct V12_07;

impl SeededTransform for V12_07 {
    const BRANCH: &'static str = "++Ares-Core+release-12.07";
    const SEED_ADDEND: u32 = 0x2d21d7c3;
    const INIT_A_OFFSET: u32 = 0x3d;

    fn word64(mut v: u64, state: u32) -> u64 {
        v ^= !u64::from(state.rotate_right(8));
        v = v.wrapping_add(u64::from(state.rotate_right(7)));
        v ^= !u64::from(state.rotate_right(6));
        v = swap_adjacent_bits_u64(v);
        v = substitute_bytes_u64(v, &SBOX_64);
        v.wrapping_sub(u64::from(state.rotate_right(3)))
    }

    fn word32(mut v: u32, state: u32) -> u32 {
        v ^= state.rotate_left(8);
        v = v.wrapping_add(state.rotate_left(7));
        v ^= state.rotate_left(6);
        v = swap_adjacent_bits_u32(v);
        v = substitute_bytes_u32(v, &SBOX_32);
        v.wrapping_sub(state.rotate_left(3))
    }

    fn byte(mut v: u8, state: u32) -> u8 {
        v ^= state.wrapping_mul(0x0cc6db61) as u8;
        v = v.wrapping_add(state.wrapping_mul(0x012959c3) as u8);
        v ^= state.wrapping_mul(0x001b0829) as u8;
        v = swap_adjacent_bits_u8(v);
        v = SBOX_8[v as usize];
        v.wrapping_sub(state.wrapping_mul(0x00000533) as u8)
    }
}
