//! A string pool for the two name columns of `fields.parquet`: a row's name
//! costs a refcount instead of an allocation, and buffered rows share one copy.
//! It saves the allocation, memcpy and retained bytes, not a lookup (`intern`
//! still hashes). Arrow never sees the `Arc`: the dictionary builders get `&str`
//! as before, so dictionaries and encoded bytes are unchanged. Counts (shared
//! with vrf-export's `FieldRecord`): docs/PERFORMANCE_NOTES.md#name-interning.

use std::fmt::Write as _;
use std::sync::Arc;

use vrf_schema::FxHashSet;

/// Ceiling on pooled names: a bound on wire-driven input, not a memory knob. An
/// unnamed RPC parameter is `"{function}._h{handle}"` with the handle straight
/// off the wire, so a corrupt or unknown-build payload could mint unbounded
/// names; past the cap `intern` still returns a correct `Arc<str>`, only
/// unshared. Measured on 02d4d478 with an instrumented build: 1,449,542 intern
/// calls, 4,557 distinct names -- the cap is an order of magnitude above that.
const MAX_POOLED_NAMES: usize = 65_536;

/// Pool of interned names, plus the scratch buffer [`Self::intern_fmt`] builds
/// them in so no caller allocates a `String` only to throw it away.
#[derive(Debug, Clone, Default)]
pub struct NameInterner {
    pool: FxHashSet<Arc<str>>,
    scratch: String,
}

impl NameInterner {
    /// Pool `s` and return the shared handle.
    pub fn intern(&mut self, s: &str) -> Arc<str> {
        pooled(&mut self.pool, s)
    }

    /// Build a name with `f` in the scratch buffer, then pool it: the
    /// allocation-free path for the composed names (RPC parameters, array
    /// leaves, struct-blob members), which are the majority of rows.
    pub fn intern_fmt(&mut self, f: impl FnOnce(&mut String)) -> Arc<str> {
        // Split the borrow: `f` writes into `scratch` while `pool` is untouched.
        let Self { pool, scratch } = self;
        scratch.clear();
        f(scratch);
        pooled(pool, scratch)
    }

    /// [`Self::intern_fmt`] for `"{a}{sep}{b}"`.
    pub fn intern_join(&mut self, a: &str, sep: char, b: &str) -> Arc<str> {
        self.intern_fmt(|out| {
            out.push_str(a);
            out.push(sep);
            out.push_str(b);
        })
    }

    /// Number of distinct names currently pooled. Diagnostic only.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.pool.len()
    }
}

/// The pooled handle for `s`, pooled now if it is new and the pool is under
/// [`MAX_POOLED_NAMES`].
fn pooled(pool: &mut FxHashSet<Arc<str>>, s: &str) -> Arc<str> {
    if let Some(existing) = pool.get(s) {
        return Arc::clone(existing);
    }
    let interned: Arc<str> = Arc::from(s);
    if pool.len() < MAX_POOLED_NAMES {
        pool.insert(Arc::clone(&interned));
    }
    interned
}

/// Write `args` into `out`. A `String` write cannot fail, and `let _ = write!`
/// at each call site would read as an ignored error.
pub fn put(out: &mut String, args: std::fmt::Arguments<'_>) {
    out.write_fmt(args)
        .expect("formatting into a String cannot fail");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Equal text shares one allocation; `Arc::ptr_eq` is how to observe it.
    #[test]
    fn equal_names_share_one_allocation() {
        let mut interner = NameInterner::default();
        let a = interner.intern("/Script/ShooterGame.ShooterCharacter");
        let b = interner.intern("/Script/ShooterGame.ShooterCharacter");
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(interner.len(), 1);
    }

    /// The built-in-place path must agree with the plain one, including sharing.
    #[test]
    fn a_formatted_name_pools_with_its_plain_twin() {
        let mut interner = NameInterner::default();
        let plain = interner.intern("Fire.Damage");
        let built = interner.intern_join("Fire", '.', "Damage");
        assert_eq!(&*built, "Fire.Damage");
        assert!(Arc::ptr_eq(&plain, &built));
        assert_eq!(interner.len(), 1);
    }

    /// Past the cap the pool stops growing but `intern` still returns the
    /// right text: a name storm costs sharing, never correctness.
    #[test]
    fn the_pool_stops_growing_but_never_stops_being_correct() {
        let mut interner = NameInterner::default();
        for i in 0..(MAX_POOLED_NAMES + 64) {
            let name = interner.intern_fmt(|out| put(out, format_args!("Fn._h{i}")));
            assert_eq!(&*name, format!("Fn._h{i}"));
        }
        assert_eq!(interner.len(), MAX_POOLED_NAMES);
        // A name minted after the cap is still correct, just unshared.
        let over = interner.intern("a name the pool never saw");
        assert_eq!(&*over, "a name the pool never saw");
    }
}
