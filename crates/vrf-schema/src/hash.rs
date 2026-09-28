//! The hasher the cache's maps use. Not HashDoS-resistant, by design; the
//! rationale and figures are in docs/PERFORMANCE_NOTES.md#fxhash-over-siphash.
//! If this ever sits behind a network boundary, revert to std's default hasher.

use std::hash::{BuildHasherDefault, Hasher};

/// A `HashMap` using [`FxHasher`].
pub type FxHashMap<K, V> = std::collections::HashMap<K, V, BuildHasherDefault<FxHasher>>;

/// A `HashSet` using [`FxHasher`].
pub type FxHashSet<T> = std::collections::HashSet<T, BuildHasherDefault<FxHasher>>;

/// Copied verbatim from `rustc_hash` (2^64 / pi, rounded), so the mixing is
/// the one rustc exercises rather than something invented here.
const SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;

/// A fast, non-cryptographic hasher for locally-sourced keys.
#[derive(Default, Clone)]
pub struct FxHasher {
    hash: u64,
}

impl FxHasher {
    #[inline]
    fn add(&mut self, word: u64) {
        // Rotating before the XOR is what stops the high bits of successive
        // words from cancelling; the multiply then diffuses low bits upward.
        self.hash = (self.hash.rotate_left(5) ^ word).wrapping_mul(SEED);
    }
}

impl Hasher for FxHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let mut rest = bytes;
        while let Some((chunk, tail)) = rest.split_first_chunk::<8>() {
            self.add(u64::from_le_bytes(*chunk));
            rest = tail;
        }
        if let Some((chunk, tail)) = rest.split_first_chunk::<4>() {
            self.add(u64::from(u32::from_le_bytes(*chunk)));
            rest = tail;
        }
        if let Some((chunk, tail)) = rest.split_first_chunk::<2>() {
            self.add(u64::from(u16::from_le_bytes(*chunk)));
            rest = tail;
        }
        if let Some(&byte) = rest.first() {
            self.add(u64::from(byte));
        }
    }

    #[inline]
    fn write_u8(&mut self, i: u8) {
        self.add(u64::from(i));
    }

    #[inline]
    fn write_u16(&mut self, i: u16) {
        self.add(u64::from(i));
    }

    #[inline]
    fn write_u32(&mut self, i: u32) {
        self.add(u64::from(i));
    }

    #[inline]
    fn write_u64(&mut self, i: u64) {
        self.add(i);
    }

    #[inline]
    fn write_usize(&mut self, i: usize) {
        self.add(i as u64);
    }

    #[inline]
    fn finish(&self) -> u64 {
        // Not a half swap: rotating by 20 brings the multiply's well-mixed high
        // bits down to the low bits hashbrown takes the bucket index from.
        self.hash.rotate_left(20)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::hash::Hash;

    fn hash_of<T: Hash>(value: &T) -> u64 {
        let mut h = FxHasher::default();
        value.hash(&mut h);
        h.finish()
    }

    #[test]
    fn distinct_small_integers_do_not_collide() {
        // NetGUID keys are small and dense; a mix that collapsed them would
        // turn every probe into a bucket walk.
        let mut seen = std::collections::HashSet::new();
        for guid in 0u32..10_000 {
            assert!(seen.insert(hash_of(&guid)), "collision at {guid}");
        }
    }

    #[test]
    fn distinct_paths_do_not_collide_across_the_reference_shapes() {
        // Real group paths share long prefixes; a hash that only looked at the
        // first word would collide en masse on these.
        let paths = [
            "/Script/ShooterGame.AresAttributeSet",
            "/Script/ShooterGame.AresAbilitySystemComponent",
            "/Script/ShooterGame.AresAbilitySystemComponent_ClassNetCache",
            "/Game/Characters/_Core/Jett/Jett_C",
            "/Game/Characters/_Core/Jett/Jett_C_ClassNetCache",
            "/Game/Maps/Ascent/Ascent",
        ];
        let mut seen = std::collections::HashSet::new();
        for p in paths {
            assert!(seen.insert(hash_of(&p)), "collision on {p}");
        }
    }

    #[test]
    fn byte_slice_length_changes_the_hash() {
        // A trailing-chunk bug that ignored the tail would make these equal.
        assert_ne!(hash_of(&"Ares"), hash_of(&"Ares "));
        assert_ne!(hash_of(&"AresAttribute"), hash_of(&"AresAttributeS"));
    }

    #[test]
    fn works_as_a_hashmap_hasher() {
        let mut map: FxHashMap<u32, &str> = FxHashMap::default();
        for i in 0..1000u32 {
            map.insert(i, "x");
        }
        assert_eq!(map.len(), 1000);
        for i in 0..1000u32 {
            assert_eq!(map.get(&i), Some(&"x"));
        }
        assert_eq!(map.get(&1000), None);
    }
}
