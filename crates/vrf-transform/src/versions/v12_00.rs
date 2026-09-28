//! `++Ares-Core+release-12.00`, recovered from the native seeded reader.

use super::SeededTransform;
use crate::helpers::reverse_bits64_without_final_16bit_swap;

pub struct V12_00;

impl SeededTransform for V12_00 {
    const BRANCH: &'static str = "++Ares-Core+release-12.00";
    const SEED_ADDEND: u32 = 0x70876679;
    const INIT_A_OFFSET: u32 = 0x07;
    const TAIL_XOR: u8 = 0x79;

    fn word64(mut v: u64, state: u32) -> u64 {
        v = v.rotate_right(state.rotate_right(8) % 63 + 1);
        v = reverse_bits64_without_final_16bit_swap(v);
        v ^= !u64::from(state.rotate_right(6));
        v = v.wrapping_sub(u64::from(state.rotate_right(5)));
        v ^= !u64::from(state.rotate_right(4));
        v ^= !u64::from(state.rotate_right(3));
        !v
    }

    fn word32(mut v: u32, state: u32) -> u32 {
        v = v.rotate_right(state.rotate_left(8) % 31 + 1);
        v = v.reverse_bits();
        v ^= state.rotate_left(6);
        v = v.wrapping_sub(state.rotate_left(5));
        v ^= state.rotate_left(4);
        v ^= state.rotate_left(3);
        !v
    }

    fn byte(mut v: u8, state: u32) -> u8 {
        v = v.rotate_right(state.wrapping_mul(0x0cc6db61) % 7 + 1);
        v = v.reverse_bits();
        v ^= state.wrapping_mul(0x001b0829) as u8;
        v = v.wrapping_sub(state.wrapping_mul(0x0002751b) as u8);
        v ^= state.wrapping_mul(0x00003931) as u8;
        v ^= state.wrapping_mul(0x00000533) as u8;
        !v
    }
}
