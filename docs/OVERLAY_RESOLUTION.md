# Overlay resolution

Why the overlay resolution code in `crates/vrf-decode/src/overlay/` and
`crates/vrf-decode/src/types.rs` is shaped the way it is. The code keeps a
one-line summary and points here by section title.

## Why a hash index (and not binary search)

Code: `crates/vrf-decode/src/overlay/index.rs`, the hash index over the
overlay entry slices.

### Why this exists

Resolving the reference replay's 988,995 offered rows costs about 1.5
million `(group_path, field_name)` probes -- one per row, plus a second for
the `b`-prefixed spelling on each of the 511,881 that miss -- and another
~0.5 million `(group_path, handle)` probes. Every one of those was a binary
search over the then 1,310-entry table. That is ~10 comparisons, and every
comparison looks at `group_path` first -- paths like
`/Game/Characters/AggroBot/AggroBot_PC.AggroBot_PC_C` that share 20 to 40
leading bytes with their neighbours, so each comparison is a real memcmp
rather than a first-byte reject. Measured on the reference replay by running
the search twice and differencing, one lookup pass costs ~180 ms of a 1.58 s
export.

The index is open addressing on a 64-bit key hash. A lookup hashes
`group_path` and `field_name` once (8 bytes per multiply) and probes once;
the stored 32-bit tag rejects a non-matching slot without touching the
strings at all. Most lookups on a real replay MISS -- 511,881 of 988,995
offered rows miss the direct `(group_path, field_name)` probe -- and a miss
ends at an empty slot with zero string comparisons.

That 511,881 is the cost this index exists to pay, NOT a coverage figure.
Most of those rows are typed anyway, by the `b`-prefix, handle, alias and
checksum steps that run after this probe; the reference replay ends with
`overlay_not_in_table = 163,650`. Reading the first-probe miss count as the
untyped count overstates the gap more than threefold.

### Answer identity

The hash only chooses *which* entries to compare. Every candidate is still
confirmed by full string equality on both key halves before it is returned,
so a collision costs time and never an answer. `tests::overlay` walks every
entry plus its `b`-stripped spelling plus synthetic misses and asserts this
index agrees with the binary search on every one.

### The b-prefix table

The overlay's boolean fallback asks for `b` + the wire's field name (see
"The b-prefix fallback" below for why). Building that key allocated a
`String` on every miss -- over half a million per replay. Instead, every
entry whose name starts with `b` is *also* inserted under its stripped name,
so the fallback probe reuses the hash already computed for the direct probe
and never builds a key. Stripped keys are unique for the same reason direct
keys are: `(group_path, field_name)` is unique in the generated table, so two
entries in one group cannot both be `b` + X for the same X.

## The b-prefix fallback

Code: `resolve_in_group` in `crates/vrf-decode/src/overlay.rs`, which
resolves a wire field within ONE group, and the name to report it under if
the decode later fails. Order: the declared name, then the `b`-prefixed
spelling of it, then the explicit property handle.

### Why the b-prefix step exists

The C# descriptors bind a property to its handle number and carry a name
only as a label, so a descriptor that spells a boolean
`bDeathMontageEffectOverrideIsQueued` still matches a wire field the replay
declares as `DeathMontageEffectOverrideIsQueued`. Our table is keyed on the
name, so that spelling difference makes the lookup miss and the field stays
raw where the reference has a plain bool.

Narrow by construction: it only fires when the direct lookup already missed,
and it can only hit an entry that the C# author spelled with the Unreal
boolean prefix. Re-measured against the then 1,310-entry table by joining
every distinct `(group, name)` 02d4d478 exports against it, RPC parameters
under the group `sink/rpc.rs` actually asks with: 632 rows resolve this way
and no others. They are ONE property name, arriving on two RPC groups --
`MulticastNotifyDamage_Point` (581 rows) and `_Base` (51). The figure stood
at "581 rows, exactly one field", measured when the table held 1,054
entries; it counted the larger group and not its sibling.

## Fail-closed on a handle conflict

Code: `OverlayStats::handle_conflicts_refused` in
`crates/vrf-decode/src/overlay/stats.rs`, the handle fallbacks refused
because the replay declared a DIFFERENT, unresolved field name at that
handle.

The fallback exists for handles the wire does not name, and for handles it
names only as a bare decimal FName index (`"248"`), which says nothing about
the property. It used to fire for a real conflicting name too: with the
descriptor mapping handle 7 to `OldField: Int32` and the replay declaring
`NewField` there carrying a `Float`, both name probes missed, the stale
handle mapping was reused, and `1.0f32` was reported as
`value_i64 = 1065353216` with `decoded_ok` incremented and `Decode errors`
still zero. That is the exact shape of a game patch moving a property, and
resolution was documented as fail-closed.

Such a field is now left untyped and counted here rather than typed
wrongly. Untyped is a state this export already models honestly (`raw_bits`
is always present); a confident wrong number is not.

It is deliberately NOT routed through `decoded_err`: nothing failed to
decode, the overlay declined to claim a type. Counting it as a decode error
would move the corpus off `Decode errors: 0` for a field that was never
decoded at all.

## FRepMovement finiteness is enforced

Code: `impl fmt::Display for FRepMovement` in `crates/vrf-decode/src/types.rs`.

### Finiteness is enforced, not structural

Every component is finite by construction only on the **packed** quantized
path and for the rotators (an integer quotient of an integer scale factor;
an integer multiple of 360/65536 or 360/256). When `componentBitCount == 0`
the decoder falls back to three raw `f32`s (or `f64`s), which carry whatever
the bits spell. A component of `0x7fc00000` is `NaN`, and neither `NaN` nor
an infinity is a JSON literal, so this `Display` would emit `"x":NaN` -- not
valid JSON -- while every decode counter reported success. An earlier comment
claimed finiteness by construction and missed that case.

So the guarantee is upheld by `DecodeError::NonFiniteComponent` (in
`crate::decode::DecodeError`), which rejects such a payload in
`geometry::read_quantized_vector` before one can reach this formatter -- the
same rejected-rather-than-coerced call `EffectBlobError::NonFiniteFloat`
already makes. Anything that constructs an `FRepMovement` by another route
owes the same check.
