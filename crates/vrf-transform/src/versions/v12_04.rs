//! `++Ares-Core+release-12.04`, recovered from the native seeded reader.

use super::SeededTransform;
use crate::helpers::*;
use crate::sbox::*;

pub struct V12_04;

impl SeededTransform for V12_04 {
    const BRANCH: &'static str = "++Ares-Core+release-12.04";
    const SEED_ADDEND: u32 = 0xa5684b42;
    const INIT_A_OFFSET: u32 = 0x3e;

    fn word64(mut v: u64, state: u32) -> u64 {
        v = v.rotate_right(state.rotate_right(8) % 63 + 1);
        v = v.rotate_right(state.rotate_right(7) % 63 + 1);
        v = v.rotate_left(state.rotate_right(6) % 63 + 1);
        v = v.rotate_right(state.rotate_right(5) % 63 + 1);
        v = v.wrapping_add(u64::from(state.rotate_right(4)));
        v = substitute_bytes_u64(v, &SBOX_64);
        v.rotate_left(state.rotate_right(1) % 63 + 1)
    }

    fn word32(mut v: u32, state: u32) -> u32 {
        v = v.rotate_right(state.rotate_left(8) % 31 + 1);
        v = v.rotate_right(state.rotate_left(7) % 31 + 1);
        v = v.rotate_left(state.rotate_left(6) % 31 + 1);
        v = v.rotate_right(state.rotate_left(5) % 31 + 1);
        v = v.wrapping_add(state.rotate_left(4));
        v = substitute_bytes_u32(v, &SBOX_32);
        v.rotate_left(state.rotate_left(1) % 31 + 1)
    }

    fn byte(mut v: u8, state: u32) -> u8 {
        v = v.rotate_right(state.wrapping_mul(0x0cc6db61) % 7 + 1);
        v = v.rotate_right(state.wrapping_mul(0x012959c3) % 7 + 1);
        v = v.rotate_left(state.wrapping_mul(0x001b0829) % 7 + 1);
        v = v.rotate_right(state.wrapping_mul(0x0002751b) % 7 + 1);
        v = v.wrapping_add(state.wrapping_mul(0x00003931) as u8);
        v = SBOX_8[v as usize];
        v.rotate_left(state.wrapping_mul(0x0000000b) % 7 + 1)
    }
}
