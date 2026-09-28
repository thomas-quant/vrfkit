//! `++Ares-Core+release-12.03`, recovered from the native seeded reader.

use super::SeededTransform;
use crate::helpers::*;
use crate::sbox::*;

pub struct V12_03;

impl SeededTransform for V12_03 {
    const BRANCH: &'static str = "++Ares-Core+release-12.03";
    const SEED_ADDEND: u32 = 0x33d59dff;
    const INIT_A_OFFSET: u32 = 0x01;
    const TAIL_XOR: u8 = 0xff;

    fn word64(mut v: u64, state: u32) -> u64 {
        v = swap_adjacent_bits_u64(v);
        v = v.wrapping_sub(u64::from(state.rotate_right(7)));
        v = v.wrapping_add(u64::from(state.rotate_right(6)));
        v = v.rotate_left(state.rotate_right(5) % 63 + 1);
        v = v.rotate_left(state.rotate_right(4) % 63 + 1);
        v = substitute_bytes_u64(v, &SBOX_64);
        v = v.rotate_left(state.rotate_right(2) % 63 + 1);
        v.wrapping_add(u64::from(state.rotate_right(1)))
    }

    fn word32(mut v: u32, state: u32) -> u32 {
        v = swap_adjacent_bits_u32(v);
        v = v.wrapping_sub(state.rotate_left(7));
        v = v.wrapping_add(state.rotate_left(6));
        v = v.rotate_left(state.rotate_left(5) % 31 + 1);
        v = v.rotate_left(state.rotate_left(4) % 31 + 1);
        v = substitute_bytes_u32(v, &SBOX_32);
        v = v.rotate_left(state.rotate_left(2) % 31 + 1);
        v.wrapping_add(state.rotate_left(1))
    }

    fn byte(mut v: u8, state: u32) -> u8 {
        v = swap_adjacent_bits_u8(v);
        v = v.wrapping_sub(state.wrapping_mul(0x012959c3) as u8);
        v = v.wrapping_add(state.wrapping_mul(0x001b0829) as u8);
        v = v.rotate_left(state.wrapping_mul(0x0002751b) % 7 + 1);
        v = v.rotate_left(state.wrapping_mul(0x00003931) % 7 + 1);
        v = SBOX_8[v as usize];
        v = v.rotate_left(state.wrapping_mul(0x00000079) % 7 + 1);
        v.wrapping_add(state.wrapping_mul(0x0000000b) as u8)
    }
}
