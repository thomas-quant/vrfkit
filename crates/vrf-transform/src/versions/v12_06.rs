//! `++Ares-Core+release-12.06`, recovered from the native seeded reader.

use super::SeededTransform;
use crate::helpers::reverse_bits64_without_final_16bit_swap;

pub struct V12_06;

impl SeededTransform for V12_06 {
    const BRANCH: &'static str = "++Ares-Core+release-12.06";
    const SEED_ADDEND: u32 = 0x8d686ca6;
    const INIT_A_OFFSET: u32 = 0x26;
    const ADD_OFFSET: bool = true;

    fn word64(mut v: u64, state: u32) -> u64 {
        v ^= !u64::from(state.rotate_right(8));
        v = v.rotate_right(state.rotate_right(7) % 63 + 1);
        v = v.wrapping_sub(u64::from(state.rotate_right(6)));
        v = reverse_bits64_without_final_16bit_swap(v);
        v = v.rotate_left(state.rotate_right(3) % 63 + 1);
        v = reverse_bits64_without_final_16bit_swap(v);
        v.wrapping_sub(u64::from(state.rotate_right(1)))
    }

    fn word32(mut v: u32, state: u32) -> u32 {
        v ^= state.rotate_left(8);
        v = v.rotate_right(state.rotate_left(7) % 31 + 1);
        v = v.wrapping_sub(state.rotate_left(6));
        v = v.reverse_bits();
        v = v.rotate_left(state.rotate_left(3) % 31 + 1);
        v = v.reverse_bits();
        v.wrapping_sub(state.rotate_left(1))
    }

    fn byte(mut v: u8, state: u32) -> u8 {
        v ^= state.wrapping_mul(0x0cc6db61) as u8;
        v = v.rotate_right(state.wrapping_mul(0x012959c3) % 7 + 1);
        v = v.wrapping_sub(state.wrapping_mul(0x001b0829) as u8);
        v = v.reverse_bits();
        v = v.rotate_left(state.wrapping_mul(0x00000533) % 7 + 1);
        v = v.reverse_bits();
        v.wrapping_sub(state.wrapping_mul(0x0000000b) as u8)
    }
}
