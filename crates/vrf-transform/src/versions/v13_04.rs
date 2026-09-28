//! `++Ares-Core+release-13.04`

use super::SeededTransform;
use crate::helpers::*;

pub struct V13_04;

impl SeededTransform for V13_04 {
    const BRANCH: &'static str = "++Ares-Core+release-13.04";
    const SEED_ADDEND: u32 = 0x076d_c658;
    const INIT_A_OFFSET: u32 = 0x28;

    fn word64(mut v: u64, state: u32) -> u64 {
        let ror1 = state.rotate_right(1);
        let ror2 = state.rotate_right(2);
        let ror3 = state.rotate_right(3);
        let ror5 = state.rotate_right(5);
        let ror6 = state.rotate_right(6);
        let ror7 = state.rotate_right(7);

        v = v.rotate_right((ror7 % 63) + 1);
        v ^= !u64::from(ror6);
        v = v.wrapping_sub(u64::from(ror5));
        v = swap_adjacent_bits_u64(v);
        v = v.wrapping_add(u64::from(ror3));
        v = v.wrapping_add(u64::from(ror2));
        v.rotate_left((ror1 % 63) + 1)
    }

    fn word32(mut v: u32, state: u32) -> u32 {
        let rol1 = state.rotate_left(1);
        let rol2 = state.rotate_left(2);
        let rol3 = state.rotate_left(3);
        let rol5 = state.rotate_left(5);
        let rol6 = state.rotate_left(6);
        let rol7 = state.rotate_left(7);

        v = v.rotate_right((rol7 % 31) + 1);
        v ^= rol6;
        v = v.wrapping_sub(rol5);
        v = swap_adjacent_bits_u32(v);
        v = v.wrapping_add(rol3);
        v = v.wrapping_add(rol2);
        v.rotate_left((rol1 % 31) + 1)
    }

    fn byte(mut v: u8, state: u32) -> u8 {
        let mix_a = state.wrapping_mul(0x0b);
        let mix_b = mix_a.wrapping_mul(0x0b);
        let mix_c = mix_b.wrapping_mul(0x0b);
        let mix_d = mix_c.wrapping_mul(0x79);
        let mix_e = mix_d.wrapping_mul(0x0b);
        let mix_f = mix_e.wrapping_mul(0x0b);

        v = v.rotate_right((mix_f % 7) + 1);
        v = (v ^ mix_e as u8).wrapping_sub(mix_d as u8);
        v = swap_adjacent_bits_u8(v);
        v = v.wrapping_add(mix_b as u8).wrapping_add(mix_c as u8);
        v.rotate_left((mix_a % 7) + 1)
    }
}
