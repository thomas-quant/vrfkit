//! `++Ares-Core+release-13.05`

use super::SeededTransform;

/// `++Ares-Core+release-13.05`
pub struct V13_05;

impl SeededTransform for V13_05 {
    const BRANCH: &'static str = "++Ares-Core+release-13.05";
    const SEED_ADDEND: u32 = 0x48c2_6613;
    const INIT_A_OFFSET: u32 = 0xffff_ffed;
    const TAIL_XOR: u8 = 0x13;

    fn word64(mut v: u64, state: u32) -> u64 {
        let ror1 = state.rotate_right(1);
        let ror2 = state.rotate_right(2);
        let ror3 = state.rotate_right(3);
        let ror4 = state.rotate_right(4);
        let ror5 = state.rotate_right(5);
        let ror6 = state.rotate_right(6);
        let ror7 = state.rotate_right(7);
        let ror8 = state.rotate_right(8);

        v ^= !u64::from(ror8);
        v = v.wrapping_sub(u64::from(ror7));
        v = v.rotate_left((ror6 % 63) + 1);
        v = v.wrapping_sub(u64::from(ror5));
        v ^= !u64::from(ror4);
        v ^= !u64::from(ror3);
        v = v.rotate_left((ror2 % 63) + 1);
        v.rotate_left((ror1 % 63) + 1)
    }

    fn word32(mut v: u32, state: u32) -> u32 {
        let rol1 = state.rotate_left(1);
        let rol2 = state.rotate_left(2);
        let rol3 = state.rotate_left(3);
        let rol4 = state.rotate_left(4);
        let rol5 = state.rotate_left(5);
        let rol6 = state.rotate_left(6);
        let rol7 = state.rotate_left(7);
        let rol8 = state.rotate_left(8);

        v ^= rol8;
        v = v.wrapping_sub(rol7);
        v = v.rotate_left((rol6 % 31) + 1);
        v = v.wrapping_sub(rol5);
        v ^= rol4;
        v ^= rol3;
        v = v.rotate_left((rol2 % 31) + 1);
        v.rotate_left((rol1 % 31) + 1)
    }

    fn byte(mut v: u8, state: u32) -> u8 {
        let mix_a = state.wrapping_mul(0x0b);
        let mix_b = mix_a.wrapping_mul(0x0b);
        let mix_c = mix_b.wrapping_mul(0x0b);
        let mix_d = mix_c.wrapping_mul(0x0b);
        let mix_e = mix_d.wrapping_mul(0x0b);
        let mix_f = mix_e.wrapping_mul(0x0b);
        let mix_g = mix_f.wrapping_mul(0x0b);
        let mix_h = mix_g.wrapping_mul(0x0b);

        v ^= mix_h as u8;
        v = v.wrapping_sub(mix_g as u8);
        v = v.rotate_left((mix_f % 7) + 1);
        v = v.wrapping_sub(mix_e as u8);
        v ^= mix_c as u8;
        v ^= mix_d as u8;
        v = v.rotate_left((mix_b % 7) + 1);
        v.rotate_left((mix_a % 7) + 1)
    }
}
