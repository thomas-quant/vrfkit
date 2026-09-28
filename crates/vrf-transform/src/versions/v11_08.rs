//! `++Ares-Core+release-11.08`, recovered from the native seeded reader.

use super::SeededTransform;
use crate::helpers::*;

pub struct V11_08;

impl SeededTransform for V11_08 {
    const BRANCH: &'static str = "++Ares-Core+release-11.08";
    const SEED_ADDEND: u32 = 0xacf2cdff;
    const INIT_A_OFFSET: u32 = 0x01;
    const TAIL_XOR: u8 = 0xff;

    fn word64(mut v: u64, state: u32) -> u64 {
        v = !v;
        v = v.rotate_right(state.rotate_right(7) % 63 + 1);
        v = v.rotate_left(state.rotate_right(6) % 63 + 1);
        v = v.rotate_left(state.rotate_right(5) % 63 + 1);
        v = v.wrapping_sub(u64::from(state.rotate_right(2)));
        swap_adjacent_bits_u64(v)
    }

    fn word32(mut v: u32, state: u32) -> u32 {
        v = !v;
        v = v.rotate_right(state.rotate_left(7) % 31 + 1);
        v = v.rotate_left(state.rotate_left(6) % 31 + 1);
        v = v.rotate_left(state.rotate_left(5) % 31 + 1);
        v = v.wrapping_sub(state.rotate_left(2));
        swap_adjacent_bits_u32(v)
    }

    fn byte(mut v: u8, state: u32) -> u8 {
        v = !v;
        v = v.rotate_right(state.wrapping_mul(0x012959c3) % 7 + 1);
        v = v.rotate_left(state.wrapping_mul(0x001b0829) % 7 + 1);
        v = v.rotate_left(state.wrapping_mul(0x0002751b) % 7 + 1);
        v = v.wrapping_sub(state.wrapping_mul(0x00000079) as u8);
        swap_adjacent_bits_u8(v)
    }
}
