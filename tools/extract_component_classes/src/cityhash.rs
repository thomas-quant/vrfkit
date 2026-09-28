//! CityHash64 (v1.1), used only as a self-check: class indices already match by
//! plain equality. A script object's global index is the hash of its lowercased
//! path and a package's chunk id that of its lowercased name, both as UTF-16LE
//! (Unreal hashes `TCHAR` text). Rebuilding them from decoded strings and getting
//! the game's numbers back proves the name batch and the outer walk. Not the
//! separators: the path hash folds `.` and `:` alike into `/`, as the engine does.

const K0: u64 = 0xc3a5_c85c_97cb_3127;
const K1: u64 = 0xb492_b66f_be98_f273;
const K2: u64 = 0x9ae1_6a3b_2f90_404f;

fn fetch64(s: &[u8], at: usize) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&s[at..at + 8]);
    u64::from_le_bytes(b)
}

fn fetch32(s: &[u8], at: usize) -> u64 {
    let mut b = [0u8; 4];
    b.copy_from_slice(&s[at..at + 4]);
    u64::from(u32::from_le_bytes(b))
}

fn rotate(v: u64, shift: u32) -> u64 {
    if shift == 0 { v } else { v.rotate_right(shift) }
}

fn shift_mix(v: u64) -> u64 {
    v ^ (v >> 47)
}

fn hash_len16_mul(u: u64, v: u64, mul: u64) -> u64 {
    let mut a = (u ^ v).wrapping_mul(mul);
    a ^= a >> 47;
    let mut b = (v ^ a).wrapping_mul(mul);
    b ^= b >> 47;
    b.wrapping_mul(mul)
}

fn hash_len16(u: u64, v: u64) -> u64 {
    hash_len16_mul(u, v, 0x9ddf_ea08_eb38_2d69)
}

fn hash_len0to16(s: &[u8]) -> u64 {
    let len = s.len();
    if len >= 8 {
        let mul = K2.wrapping_add((len as u64).wrapping_mul(2));
        let a = fetch64(s, 0).wrapping_add(K2);
        let b = fetch64(s, len - 8);
        let c = rotate(b, 37).wrapping_mul(mul).wrapping_add(a);
        let d = rotate(a, 25).wrapping_add(b).wrapping_mul(mul);
        return hash_len16_mul(c, d, mul);
    }
    if len >= 4 {
        let mul = K2.wrapping_add((len as u64).wrapping_mul(2));
        let a = fetch32(s, 0);
        return hash_len16_mul((len as u64).wrapping_add(a << 3), fetch32(s, len - 4), mul);
    }
    if len > 0 {
        let a = u32::from(s[0]);
        let b = u32::from(s[len >> 1]);
        let c = u32::from(s[len - 1]);
        let y = a.wrapping_add(b << 8);
        let z = (len as u32).wrapping_add(c << 2);
        return shift_mix(u64::from(y).wrapping_mul(K2) ^ u64::from(z).wrapping_mul(K0))
            .wrapping_mul(K2);
    }
    K2
}

fn hash_len17to32(s: &[u8]) -> u64 {
    let len = s.len();
    let mul = K2.wrapping_add((len as u64).wrapping_mul(2));
    let a = fetch64(s, 0).wrapping_mul(K1);
    let b = fetch64(s, 8);
    let c = fetch64(s, len - 8).wrapping_mul(mul);
    let d = fetch64(s, len - 16).wrapping_mul(K2);
    hash_len16_mul(
        rotate(a.wrapping_add(b), 43)
            .wrapping_add(rotate(c, 30))
            .wrapping_add(d),
        a.wrapping_add(rotate(b.wrapping_add(K2), 18))
            .wrapping_add(c),
        mul,
    )
}

fn hash_len33to64(s: &[u8]) -> u64 {
    let len = s.len();
    let mul = K2.wrapping_add((len as u64).wrapping_mul(2));
    let mut a = fetch64(s, 0).wrapping_mul(K2);
    let mut b = fetch64(s, 8);
    let c = fetch64(s, len - 24);
    let d = fetch64(s, len - 32);
    let e = fetch64(s, 16).wrapping_mul(K2);
    let f = fetch64(s, 24).wrapping_mul(9);
    let g = fetch64(s, len - 8);
    let h = fetch64(s, len - 16).wrapping_mul(mul);
    let u =
        rotate(a.wrapping_add(g), 43).wrapping_add(rotate(b, 30).wrapping_add(c).wrapping_mul(9));
    let v = (a.wrapping_add(g) ^ d).wrapping_add(f).wrapping_add(1);
    let w = u
        .wrapping_add(v)
        .wrapping_mul(mul)
        .swap_bytes()
        .wrapping_add(h);
    let x = rotate(e.wrapping_add(f), 42).wrapping_add(c);
    let y = v
        .wrapping_add(w)
        .wrapping_mul(mul)
        .swap_bytes()
        .wrapping_add(g)
        .wrapping_mul(mul);
    let z = e.wrapping_add(f).wrapping_add(c);
    a = x
        .wrapping_add(z)
        .wrapping_mul(mul)
        .wrapping_add(y)
        .swap_bytes()
        .wrapping_add(b);
    b = shift_mix(
        z.wrapping_add(a)
            .wrapping_mul(mul)
            .wrapping_add(d)
            .wrapping_add(h),
    )
    .wrapping_mul(mul);
    b.wrapping_add(x)
}

fn weak_hash_len32_with_seeds(s: &[u8], at: usize, a: u64, b: u64) -> (u64, u64) {
    let w = fetch64(s, at);
    let x = fetch64(s, at + 8);
    let y = fetch64(s, at + 16);
    let z = fetch64(s, at + 24);
    let mut a = a.wrapping_add(w);
    let mut b = rotate(b.wrapping_add(a).wrapping_add(z), 21);
    let c = a;
    a = a.wrapping_add(x).wrapping_add(y);
    b = b.wrapping_add(rotate(a, 44));
    (a.wrapping_add(z), b.wrapping_add(c))
}

pub fn city_hash64(s: &[u8]) -> u64 {
    let len = s.len();
    if len <= 32 {
        if len <= 16 {
            return hash_len0to16(s);
        }
        return hash_len17to32(s);
    }
    if len <= 64 {
        return hash_len33to64(s);
    }

    let mut x = fetch64(s, len - 40);
    let mut y = fetch64(s, len - 16).wrapping_add(fetch64(s, len - 56));
    let mut z = hash_len16(
        fetch64(s, len - 48).wrapping_add(len as u64),
        fetch64(s, len - 24),
    );
    let mut v = weak_hash_len32_with_seeds(s, len - 64, len as u64, z);
    let mut w = weak_hash_len32_with_seeds(s, len - 32, y.wrapping_add(K1), x);
    x = x.wrapping_mul(K1).wrapping_add(fetch64(s, 0));

    let mut remaining = (len - 1) & !63usize;
    let mut at = 0usize;
    loop {
        x = rotate(
            x.wrapping_add(y)
                .wrapping_add(v.0)
                .wrapping_add(fetch64(s, at + 8)),
            37,
        )
        .wrapping_mul(K1);
        y = rotate(y.wrapping_add(v.1).wrapping_add(fetch64(s, at + 48)), 42).wrapping_mul(K1);
        x ^= w.1;
        y = y.wrapping_add(v.0).wrapping_add(fetch64(s, at + 40));
        z = rotate(z.wrapping_add(w.0), 33).wrapping_mul(K1);
        v = weak_hash_len32_with_seeds(s, at, v.1.wrapping_mul(K1), x.wrapping_add(w.0));
        w = weak_hash_len32_with_seeds(
            s,
            at + 32,
            z.wrapping_add(w.1),
            y.wrapping_add(fetch64(s, at + 16)),
        );
        std::mem::swap(&mut z, &mut x);
        at += 64;
        remaining -= 64;
        if remaining == 0 {
            break;
        }
    }
    hash_len16(
        hash_len16(v.0, w.0)
            .wrapping_add(shift_mix(y).wrapping_mul(K1))
            .wrapping_add(z),
        hash_len16(v.1, w.1).wrapping_add(x),
    )
}

/// The 62 bits an `FPackageObjectIndex` keeps of a hash.
pub const INDEX_MASK: u64 = (1u64 << 62) - 1;

/// Unreal's object-path hash, a script object's global index: lowercase, `.`
/// and `:` become `/`, hashed as UTF-16LE, top two bits cleared.
pub fn hash_path(path: &str) -> u64 {
    let mut bytes = Vec::with_capacity(path.len() * 2);
    for ch in path.chars() {
        let ch = if ch == '.' || ch == ':' {
            '/'
        } else {
            lower(ch)
        };
        let mut units = [0u16; 2];
        for unit in ch.encode_utf16(&mut units) {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
    }
    city_hash64(&bytes) & INDEX_MASK
}

/// Unreal's package id: the lowercased package name hashed as UTF-16LE, all 64
/// bits kept. An `ExportBundleData` chunk id carries it.
pub fn hash_package_name(name: &str) -> u64 {
    let mut bytes = Vec::with_capacity(name.len() * 2);
    for ch in name.chars() {
        let mut units = [0u16; 2];
        for unit in lower(ch).encode_utf16(&mut units) {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
    }
    city_hash64(&bytes)
}

/// One-to-one lowercasing: a character whose lowercase form is several is left
/// alone, so the UTF-16 length never changes and a wrong guess shows as a
/// counted hash mismatch, never an accidental match.
fn lower(ch: char) -> char {
    if ch.is_ascii() {
        return ch.to_ascii_lowercase();
    }
    let mut it = ch.to_lowercase();
    match (it.next(), it.next()) {
        (Some(l), None) => l,
        _ => ch,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every CityHash version returns `k2` for empty input: a pin independent
    /// of anything this tool reads.
    #[test]
    fn empty_input_is_k2() {
        assert_eq!(city_hash64(b""), K2);
    }

    /// Reference values from the game, not from this code: the global index a
    /// 13.06 `global.ucas` stores for each engine path, and the chunk id a 13.06
    /// container stores for the package. They cover the 17-32, 33-64 and
    /// over-64 byte classes (UTF-16 doubles the length), all a real path reaches.
    #[test]
    fn engine_paths_hash_to_the_indices_the_game_stores() {
        for (path, stored) in [
            ("/Script/UMG", 0x7320_1665_2744_f78bu64),
            ("/Script/Engine", 0x51ac_ced3_dc7c_0922),
            ("/Script/CoreUObject", 0x61fe_bf02_cdde_2af3),
            ("/Script/ShooterGame.AresInventory", 0x6106_a046_0675_78c9),
            (
                "/Script/ShooterGame.EquippableStateMachineComponent",
                0x6102_9df2_5193_6cb5,
            ),
        ] {
            assert_eq!(hash_path(path), stored & INDEX_MASK, "{path}");
        }
        assert_eq!(
            hash_package_name("/Game/Labels/Premier/Rewards/V25A5PremierRewards_Label"),
            0x8b5e_0d50_90f3_bdb6
        );
    }

    /// Each length class takes its own code path, and real paths never reach
    /// the short ones. These values come from this implementation after it
    /// reproduced all 80,800 script object hashes and 289,267 package ids of
    /// the 13.06 containers: they pin that behaviour, they do not prove it.
    #[test]
    fn each_length_class_is_pinned() {
        let text = b"/script/shootergame/equippablestatemachinecomponent/abcdefghijklmnopqrstuvwxyz0123456789";
        for (len, want) in [
            (1usize, 0x1d4e_410d_a3ef_3e4bu64),
            (3, 0x609a_9da5_a6c0_eec8),
            (4, 0x24d2_976a_1996_47b8),
            (7, 0x221a_f868_e174_c75d),
            (8, 0xe878_e99b_36c2_0cb7),
            (16, 0xd088_f403_a5d3_3e96),
            (17, 0x66f4_9037_d4fc_7801),
            (32, 0xcb22_9a93_da5a_ae3b),
            (33, 0xfe75_e342_58fe_c8aa),
            (64, 0x1ad2_eeb6_73b7_b30d),
            (65, 0x8109_8aa1_8df7_c6c2),
            (88, 0x1aa8_aaa4_7c22_b4e7),
        ] {
            assert_eq!(city_hash64(&text[..len]), want, "length {len}");
        }
    }

    #[test]
    fn path_hashing_folds_case_and_separators() {
        assert_eq!(
            hash_path("/Script/ShooterGame.AresInventory"),
            hash_path("/script/shootergame/aresinventory")
        );
        assert_ne!(
            hash_path("/Script/ShooterGame.AresInventory"),
            hash_path("/Script/ShooterGame.AresInventor")
        );
        assert_eq!(hash_path("/Script/X.Y") >> 62, 0);
    }
}
