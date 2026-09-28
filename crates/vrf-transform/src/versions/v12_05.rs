//! `++Ares-Core+release-12.05`, recovered from the native seeded reader.

use super::SeededTransform;
use crate::helpers::*;
use crate::sbox::*;

pub struct V12_05;

impl SeededTransform for V12_05 {
    const BRANCH: &'static str = "++Ares-Core+release-12.05";
    const SEED_ADDEND: u32 = 0xc21d548c;
    const INIT_A_OFFSET: u32 = 0x0c;
    const ADD_OFFSET: bool = true;

    fn word64(mut v: u64, state: u32) -> u64 {
        v = v.rotate_right(state.rotate_right(8) % 63 + 1);
        v = !v;
        v = reverse_bits64_without_final_16bit_swap(v);
        v = v.wrapping_sub(u64::from(state.rotate_right(4)));
        v = v.rotate_right(state.rotate_right(3) % 63 + 1);
        v = substitute_bytes_u64(v, &SBOX_64);
        !v
    }

    fn word32(mut v: u32, state: u32) -> u32 {
        v = v.rotate_right(state.rotate_left(8) % 31 + 1);
        v = !v;
        v = v.reverse_bits();
        v = v.wrapping_sub(state.rotate_left(4));
        v = v.rotate_right(state.rotate_left(3) % 31 + 1);
        v = substitute_bytes_u32(v, &SBOX_32);
        !v
    }

    fn byte(mut v: u8, state: u32) -> u8 {
        v = v.rotate_right(state.wrapping_mul(0x0cc6db61) % 7 + 1);
        v = !v;
        v = v.reverse_bits();
        v = v.wrapping_sub(state.wrapping_mul(0x00003931) as u8);
        v = v.rotate_right(state.wrapping_mul(0x00000533) % 7 + 1);
        v = SBOX_8[v as usize];
        !v
    }
}
