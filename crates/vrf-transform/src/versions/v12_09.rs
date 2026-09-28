//! `++Ares-Core+release-12.09`, recovered from the native seeded reader.

use super::SeededTransform;

pub struct V12_09;

impl SeededTransform for V12_09 {
    const BRANCH: &'static str = "++Ares-Core+release-12.09";
    const SEED_ADDEND: u32 = 0x7ff2feec;
    const INIT_A_OFFSET: u32 = 0x14;

    fn word64(mut v: u64, state: u32) -> u64 {
        v = v.rotate_left(state.rotate_right(8) % 63 + 1);
        v ^= !u64::from(state.rotate_right(7));
        v = v.rotate_left(state.rotate_right(6) % 63 + 1);
        v = v.rotate_left(state.rotate_right(5) % 63 + 1);
        v = !v;
        v ^= !u64::from(state.rotate_right(3));
        v = v.rotate_left(state.rotate_right(2) % 63 + 1);
        v.rotate_left(state.rotate_right(1) % 63 + 1)
    }

    fn word32(mut v: u32, state: u32) -> u32 {
        v = v.rotate_left(state.rotate_left(8) % 31 + 1);
        v ^= state.rotate_left(7);
        v = v.rotate_left(state.rotate_left(6) % 31 + 1);
        v = v.rotate_left(state.rotate_left(5) % 31 + 1);
        v = !v;
        v ^= state.rotate_left(3);
        v = v.rotate_left(state.rotate_left(2) % 31 + 1);
        v.rotate_left(state.rotate_left(1) % 31 + 1)
    }

    fn byte(mut v: u8, state: u32) -> u8 {
        v = v.rotate_left(state.wrapping_mul(0x0cc6db61) % 7 + 1);
        v ^= state.wrapping_mul(0x012959c3) as u8;
        v = v.rotate_left(state.wrapping_mul(0x001b0829) % 7 + 1);
        v = v.rotate_left(state.wrapping_mul(0x0002751b) % 7 + 1);
        v = !v;
        v ^= state.wrapping_mul(0x00000533) as u8;
        v = v.rotate_left(state.wrapping_mul(0x00000079) % 7 + 1);
        v.rotate_left(state.wrapping_mul(0x0000000b) % 7 + 1)
    }
}
