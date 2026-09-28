# Performance notes

This collects the "we measured this optimisation" narratives that used to sit
next to the code as doc comments. **These are dated measurements, not
guarantees.** Each one was taken against one build, one reference replay, and
one version of the surrounding code; none of it is re-verified here, and none
of it should be read as a current benchmark. Where the source comment says a
number predates a later change, that caveat is carried below with it. Section
headings follow the migration plan this doc was written against; within a
section, a replay is named only where its source comment names it, and left
as "the reference replay" where that is what the source says.

Each section names the file and item it was moved from, so the measurement
can be found again next to the code it describes.

## Bit reader (vrf-bitio)

### Cold-path builders stay free functions

Source: `crates/vrf-bitio/src/lib.rs`, `BitReader::need` (the `Eof` error
build, lines ~250-255) and `load_u64_padded` (lines ~654-659). One
measurement, carried by two comments.

At the `Eof` construction inside `need`:

> Constructed inline on purpose. Hoisting this into a `#[cold]` out-of-line
> builder measured neutral at best on the reference replay: taking `&self`
> there made the reader address-taken and cost ~2%, and passing the three
> fields by value instead only got back to parity. The optimiser already sinks
> this into the unlikely branch, so the simpler code stays.

At `load_u64_padded`, the tail path of `BitReader::load_u64`:

> A free function taking the slice, not a method taking `&BitReader`. A cold
> method borrowing the reader makes it address-taken, so the optimiser has to
> keep the reader in memory across every read rather than in registers; that
> cost a measured ~2% when the same shape was tried on the EOF path. Here the
> two forms measured within noise of each other, so this is the form chosen on
> the grounds that it cannot provoke the problem, not on a measured win.

### load_u64: avoiding a memcpy

Source: `crates/vrf-bitio/src/lib.rs`, `BitReader::load_u64` doc comment,
lines ~265-275.

> Load 64 bits starting at absolute byte `byte`, zero-padding past the end.
>
> Padding is safe because callers have already checked that the *bits* they
> want are in range; the padding only ever covers bits that get masked off.
>
> The fast path must be spelled as a fixed-size chunk so the compiler sees one
> unaligned 8-byte load. Copying a runtime-length slice into a stack buffer
> instead compiled to a real `callq memcpy` -- plus zeroing the buffer and
> spilling it -- on *every* bit read, which dominated the reader's cost. Only
> the final seven bytes of `data` need padding, so that case is out of line and
> out of the way.

### read_int_packed: peeling the overflow check

Source: `crates/vrf-bitio/src/lib.rs`, `BitReader::read_int_packed` doc
comment, section "The fifth chunk is peeled", lines ~388-403.

> Only the last chunk can overrun a `u32`: it lands at shift 28, where four
> payload bits fit and seven are on the wire. `16u32 << 28` is zero, so folding
> it in unchecked returned `Ok(0)` for a value that is not zero -- and zero is
> the terminator of every property loop above this crate, so the wrong number
> ended the record rather than merely mis-reporting one field.
>
> The check is *peeled out of the loop* rather than guarded inside it, so the
> one-to-four byte path -- every value below 2^28, which is very nearly all of
> them -- executes exactly the instructions it did before: no added compare, no
> added branch, and no reliance on the optimiser unrolling a five-trip loop to
> fold a constant comparison away. The continuation bit is tested first so a
> runaway value still reports `BitError::MalformedIntPacked` rather than the
> overflow.

### copy_bits_to: the byte-aligned path

Source: `crates/vrf-bitio/src/lib.rs`, inside `BitReader::copy_bits_to`,
lines ~580-585.

> A byte-aligned `copy_from_slice` fast path guarded on `off == 0` was tried
> alongside this and measured exactly neutral on the reference replay --
> 1.224s/1.211s against 1.225s/1.211s over two interleaved best-of-7 runs.
> Payloads sit behind variable-bit headers, so alignment should be the
> exception rather than the rule; either way the second path did not pay for
> itself, so one loop is what is kept.

### copy_bits_to: the tail write stays a memcpy

Source: `crates/vrf-bitio/src/lib.rs`, inside `BitReader::copy_bits_to`,
lines ~604-611.

> This lowers to a `callq memcpy` of 1..=8 bytes, because the length is a
> runtime value; nearly every call reaches it, since only an exact multiple of
> 64 bits leaves no tail. Replacing it with a shift-and-peel loop does remove
> the call -- verified in the emitted asm -- but measured neutral on both
> `validate` and `export`, so the one-liner stays. A plain zip is not an option
> either way: LLVM's loop-idiom pass turns that straight back into the same
> memcpy.

## Frame walking (vrf-frame)

### Measured shape on real replays

Source: `crates/vrf-frame/src/sections.rs`, module doc, lines ~8-16.

> Over the reference replay's 226,190 frames the streaming-level-fixes section
> carries **29** level names in total and the external-data loop terminates
> immediately **every** time (zero blobs). Both loops are therefore effectively
> frame overhead, not throughput: optimising the level-name read to skip its
> `String` allocation would remove 29 allocations from a run that makes
> millions, so it is deliberately left reading (and validating) the string
> rather than blind-skipping the bytes.

The zero-blob figure above was one replay's. Re-measured on 2026-09-28, once
the skips were counted (`vrf_frame::FrameSkips`, printed as `Frame skips:` by
`export` and `validate`, and published in `manifest.json` quality and the
`diag` JSON): `vrfkit diag` over 45 replays -- two from each of the 21 build
directories of the local archive (13.01's two include the pinned 02d4d478)
and the three public fixtures (12.10, 12.11, 13.00), 24 builds in all --
walked 10,614,694 ReplayData frames and 860 checkpoint frames with **0**
ExternalData blobs, **0** ExternalData bytes and **0** GameSpecificFrameData
bytes. The last is structural on that sample: `vrfkit inspect` shows header
flags `0x0002` (`HasStreamingFixes` only) on all 45, so the section never
appears. A build that starts sending either section now moves these counters
in every output instead of nothing.

What keeps them wired is `crates/vrfkit/tests/frame_skips.rs`, not the corpus
guards. It runs the binary on a synthetic uncompressed replay whose ReplayData
and Checkpoint frames carry both sections, with different totals per pass,
and fails if any of the five passes (export main and checkpoint, `validate`,
`diag` main and checkpoint) reports another pass's numbers, only its last
chunk's, or 0. The pinned `frame_*` export baselines cannot do that: they
belong to one 13.01 replay, 02d4d478, whose true value is 0, so they would
still read 0 with a pass's tally disconnected, and a new build's replays
never reach them. `check_export_baseline.py`'s summary-vs-manifest
reconciliation cannot either, because each pass feeds both outputs from one
variable.

## Schema hot path (vrf-schema)

### FxHash over SipHash

Source: `crates/vrf-schema/src/hash.rs`, module doc, lines ~1-42.

> The hasher the cache's internal maps use.
>
> **Why not the standard hasher**
>
> Every map in `NetGuidCache` is keyed by data the replay supplies, and they
> are probed on the export's hottest path: the sink resolves a group for each
> of ~780k content blocks, and each resolution costs several
> `get_group_by_path` probes plus a `get_path_by_guid` per actor, class and
> subobject GUID it touches.
>
> `std`'s default is SipHash-1-3, which is chosen for HashDoS resistance on
> attacker-controlled keys in a network service. Two of these maps
> (`guid_to_path`, `guid_to_outer`, `by_index`) are keyed by a bare `u32`,
> where SipHash's keying, block setup and finalisation cost far more than the
> probe they protect. This hasher reduces a `u32` key to one rotate, one XOR
> and one multiply.
>
> **What is given up, and why that is acceptable here**
>
> This is **not** a HashDoS-resistant hash. A crafted replay could in
> principle pick GUIDs or paths that collide and drive a map probe quadratic.
> That is a real and deliberate trade, made because:
>
> - the map sizes are bounded by the same replay's own declared counts, which
>   are already range-checked (`MAX_GUID_ENTRIES`, `MAX_GROUPS`), so the worst
>   case is bounded work on one local file rather than an unbounded stall in a
>   shared service; and
> - this crate parses local files the operator chose to open, not requests
>   from an untrusted peer.
>
> If this ever moves behind a network boundary, revert these maps to
> `std::collections::HashMap`'s default hasher. The map types inside
> `cache.rs` are private, so that change stays confined there; the hasher
> itself is published (`pub mod hash`) because `vrfkit`'s sink makes the same
> trade for its own locally-sourced, bounded-key maps.
>
> **Provenance**
>
> The mix is rustc's own `FxHasher` (`rustc_hash`), which rustc uses for the
> same reason: bounded, locally-sourced keys where the cryptographic strength
> is not buying anything. It is reproduced here rather than taken as a
> dependency to keep the crate's dependency set at `vrf-bitio` + `thiserror`.

### Path alias enumeration

Source: `crates/vrf-schema/src/path.rs`, `for_each_replay_path_key` doc
comment, section "Why a visitor and not a `Vec<String>`", lines ~32-43.

> This used to return one. The sink calls it once per content block --
> 608,020 times per replay -- and the vector plus its first element were two
> heap allocations on every call, whether or not an alias existed. Most paths
> have no alias at all: `default_object_alias` declines anything qualified
> without the prefix, and `core_alias` declines anything outside
> `/Game/Characters/`. So the common case allocated twice to hand back a copy
> of a string the caller already had.
>
> Handing out `&str` makes that case allocation-free. An alias still costs one
> `String`, because it is genuinely new text.

## Replication pipeline (vrf-net)

### Measured rates, reference replay 02d4d478

Three sites in vrf-net carry pieces of the same shape measurement, plus one
more in vrfkit's sink module doc (quoted under "What the whole sink costs"
below rather than repeated here). One consolidated table, then each source
quoted in full as provenance:

| figure | as written | source(s) |
|---|---|---|
| content blocks | 608 020 | mod.rs Layout table; framing.rs |
| bunches | 530 401 | framing.rs; spawn.rs |
| actor opens | 2 028 (mod.rs Layout table rounds this to ~2 000) | framing.rs; spawn.rs |
| channel closes | ~1 800 | mod.rs Layout table only |
| spawn blocks | ~2 000 | mod.rs Layout table only |
| distinct channel indices | 232 | mod.rs "Allocation strategy" |

The numbers below are not reconciled to each other beyond this table: each
quote carries the rounding (or precision) its own source used.

Source: `crates/vrf-net/src/pipeline/mod.rs`, module doc, section "Layout",
lines ~10-22:

> The public surface (this file) is deliberately thin: the sink trait, the
> types it exchanges, and the packet-level driver. The three stages below it
> live in their own modules so each can be read against the wire format it
> implements:
>
> | module | scope | rate on the reference replay |
> |---|---|---|
> | `channel` | open/close, GUID preambles | ~2 000 opens, ~1 800 closes |
> | `spawn` | dynamic-actor spawn block | ~2 000 |
> | `framing` | content blocks, fields, RPCs | 608 020 blocks |

Source: `crates/vrf-net/src/pipeline/framing.rs`, module doc, lines ~1-11:

> Content-block framing: the per-block hot loop.
>
> This is where the replay's bulk goes -- 608 020 content blocks on the
> reference replay against 530 401 bunches and 2 028 actor opens. Everything
> in this module runs per block, so anything that can be hoisted out of it or
> made conditional on a failure path belongs somewhere else.
>
> The loop is: read a block header, read its declared payload bit count, hand
> the header to the sink (which answers with a function count for
> ClassNetCache blocks), then decode the payload and walk the field or RPC
> stream inside it.

Source: `crates/vrf-net/src/pipeline/spawn.rs`, module doc, lines ~1-7:

> Dynamic-actor spawn data: archetype, level, transform and velocity.
>
> This is the block Unreal writes immediately after the actor GUID when a
> channel opens for a *dynamic* (even, non-zero GUID) actor. It is small and
> rare -- 2 028 opens on the reference replay against 530 401 bunches -- but
> its bit width is load-bearing for everything after it in the same bunch, so
> the reasoning below is kept next to the reads rather than in a design doc.

(The fourth site -- `crates/vrfkit/src/sink/mod.rs`, module doc, section
"What the sink costs" -- carries the same replay's packet and block counts
again as "530,401 packets, 608,020 content blocks"; see "Export sink (vrfkit)
> What the whole sink costs" below for the full quote.)

### Allocation strategy

Source: `crates/vrf-net/src/pipeline/mod.rs`, module doc, section
"Allocation strategy", lines ~23-38.

> The steady state of this reader allocates nothing per packet, per bunch or
> per content block. Three buffers are owned by the reader and reused for the
> whole replay:
>
> - `scratch` holds one decoded content-block payload;
> - `fragment_stage` holds one partial-bunch fragment, byte-aligned;
> - the channel table grows once per distinct channel index (232 on the
>   reference replay) and never per bunch.
>
> Bunch payloads are *views* into the caller's packet bytes: `RawPacketReader`
> hands the framing loop a sub-reader, and content blocks and fields are
> sub-readers of that. An earlier version copied every bunch payload into a
> fresh `Vec<u8>` before processing it, to satisfy a borrow that a field split
> solves instead; see `ReplicationReader::process_packet`.

### Packet processing is interleaved

Source: `crates/vrf-net/src/pipeline/mod.rs`, `ReplicationReader::process_packet`,
lines ~447-461 (the comment continues past 461 into a correctness argument
that is out of scope for this migration and stays with the code).

> Bunches are processed inline, inside the packet reader's callback.
>
> This used to be two phases: parse every bunch header, copying each payload
> into a fresh `Vec<u8>`, and only then walk the copies. The stated reason was
> that "the callback borrows self". It does not have to. Destructuring `self`
> here gives the callback `&mut` handles to the fields it needs while
> `packet_reader` stays borrowed by `read_packet`, which the borrow checker
> accepts because the fields are disjoint.
>
> What the copies cost: 530 401 bunches on the reference replay, one
> `vec![0u8; n]` each (zero-fill, then `copy_bits_to` overwrote the same
> bytes), plus one `Vec` per packet for the staging list. About 1.06 million
> allocate/free pairs and two passes over ~108 MB of payload, to hand the
> framing loop bits it could already see.

## Export sink (vrfkit)

### Name interning

Two sites carry this measurement, with slightly different precision. Both are
quoted in full.

Source: `crates/vrfkit/src/sink/intern.rs`, module doc, lines ~1-19:

> A string pool for the two name columns of `fields.parquet`.
>
> **Why**
>
> The reference replay emits 1,246,812 field rows and the Parquet writer used
> to buffer 131,072 of them before flushing a row group. With `String` columns
> that was up to three heap allocations per row and ~393,000 live allocations
> at the flush peak -- while the whole replay only ever names **475** distinct
> group paths and 4,557 distinct field names between them. Interning replaces
> the allocation with a refcount increment and makes the buffered rows share
> one copy of each name.
>
> The pool is not a cache in front of a slow computation: `intern` still
> hashes the string it is given. What it buys is the allocation, the memcpy,
> and the retained bytes -- not a lookup.
>
> Arrow never sees the `Arc`. The dictionary builders are fed `&str` exactly
> as before, so the value sequence per row group, the dictionary and the
> encoded bytes are unchanged.

Source: `crates/vrf-export/src/record.rs`, `FieldRecord` doc comment, section
"Why the string columns are `Arc<str>`", lines ~8-22 (note this file's own
caveat that its buffered-row count predates a later change to
`MAX_BUFFERED_ROWS`):

> `FieldRecord` is produced 1,246,812 times on the reference replay, and the
> writer buffered 131,072 of them before flushing a row group
> (`MAX_BUFFERED_ROWS` is 8,192 today; the measurement below predates that).
> With `String` that was up to three heap allocations per row and ~393,000
> live allocations at the peak. There are only 475 distinct `group_path`
> values in the whole replay and a few thousand distinct field names, so an
> `Arc<str>` the producer interns once and clones per row replaces the
> allocation with a refcount increment.
>
> Arrow is unaffected: the dictionary builders are fed `&str` either way (see
> `tables::fields`), so the value sequence handed to the encoder -- and
> therefore the bytes on disk -- is identical. All 11 Parquet outputs of the
> reference replay are byte-for-byte what they were before interning.

`intern.rs` gives the field-name count precisely (4,557 distinct names);
`record.rs` gives it loosely ("a few thousand"). Both are carried as written.

`intern.rs` also documents `MAX_POOLED_NAMES` a few lines below this range
(lines ~26-38 in that file, the 1,449,542-intern-calls figure) -- that block
is **outside** what was cited for migration here and is left in place.

### Group-path resolution memo

Source: `crates/vrfkit/src/sink/paths.rs`, module doc, section "The memo",
lines ~38-49.

> Measured on 02d4d478 with a throwaway instrumented build:
>
> ```text
> probes 608,011   hits 489,996 (80.6%)   misses 118,015
> generation changes 1,823      entries held at the end 64
> ```
>
> The entry count is the answer to the obvious objection. `actor_net_guid` is
> part of the key and grows monotonically over a replay, so an unbounded memo
> was the risk; in practice a resolution input moves every ~330 blocks and the
> table is discarded long before it can grow. The memo costs kilobytes and
> removes four fifths of the work.

### RPC parameter walking

Source: `crates/vrfkit/src/sink/rpc.rs`, module doc, lines ~1-6.

> The ClassNetCache RPC payload walker.
>
> A ClassNetCache block carries function calls, not properties. Each call's
> payload is a sub-archive following the RepLayout `FunctionParameters`
> grammar, and walking it turns one opaque blob into one row per named
> parameter -- 559,346 of the reference replay's 1,246,812 field rows.

### What the whole sink costs

Source: `crates/vrfkit/src/sink/mod.rs`, module doc, section "What the sink
costs", lines ~23-33.

> `vrfkit validate` runs this whole path and writes no file, so it measures
> the sink alone. Five interleaved runs against the pre-rewrite binary on
> 02d4d478 (46 MB, 530,401 packets, 608,020 content blocks): median 1.395 s ->
> 1.062 s. That 333 ms is the group-path memo in `paths` plus the name pool in
> `intern`; nothing else in the decode path changed.
>
> Peak working set for `validate` moved 64.5 MB -> 65.0 MB. The memo and the
> pool are the only new state and together they are under a megabyte -- see
> the measured entry counts in those two modules.

### RecordBuffers are lent, not owned

Source: `crates/vrfkit/src/sink/mod.rs`, `RecordBuffers` doc comment,
lines ~380-392.

> The record buffers a sink fills for one packet.
>
> These live outside the sink and are lent to it. The sink is rebuilt for each
> of a replay's ~530 k packets, so a `Vec` allocated in its constructor is
> allocated (and freed) half a million times; that construct-and-drop cost
> measured at ~290 ms of a 1.79 s export, larger than the whole movement
> decoder. The buffers are empty at the end of every packet, so keeping their
> capacity across packets costs one allocation for the entire run.
>
> `ExportSink::new` clears them, so a sink always starts empty no matter what
> the previous holder did. That is what stops a caller which never drains them
> -- the validation oracle is one -- from accumulating every record in the
> replay.

## Columnar output (vrf-export)

### Sparse nullable columns vs Arrow Union

Source: `crates/vrf-export/src/lib.rs`, module doc, section "Schema design
choices", lines ~20-46.

> **`fields` table -- sparse value columns vs. Union**
>
> Every ordinary decoded-field record carries at most one typed value (i64,
> f64, bool, or str); whole-block preservation records carry none. We
> represent this as **four nullable columns** rather than an Arrow DenseUnion
> because:
>
> 1. **Compression**: nullable columns where >90 % of values are null compress
>    to nearly zero -- the validity bitmap itself is run-length-encoded inside
>    Parquet. A Union column, on the other hand, must store type-id + offset
>    arrays that are poorly compressible when the mix is heterogeneous.
> 2. **Ecosystem compatibility**: DuckDB, pandas, and PyArrow handle nullable
>    primitives without issue, while Union support varies across versions and
>    can disable predicate pushdown.
> 3. **Simplicity**: four extra columns with known types are trivial to filter
>    (`WHERE value_i64 IS NOT NULL`); Union requires type-aware dispatch.
>
> **Dictionary encoding**
>
> `group_path` and `field_name` are dominated by a tiny set of repeated
> strings (475 distinct group paths over 1.25 M rows on the reference replay).
> Dictionary encoding stores the distinct values once and references them by
> index, shrinking data pages by 50-200x. The producer interns the same two
> columns as `Arc<str>`; see `record` for why, and note that the interning is
> invisible to Arrow -- the builders are fed `&str` either way.

### raw_bits: SmallVec, and the rejected arena

Source: `crates/vrf-export/src/record.rs`, `FieldRecord::raw_bits` doc
comment, lines ~83-101.

> Raw bit payload; `None` for zero-bit fields.
>
> Inlined as `SmallVec<[u8; 16]>`: most field payloads are <=16 bytes
> (u32/u64/FVector/FString-prefix), so the inline array eliminates the heap
> allocation on the ~1.25 M-row reference export. Larger payloads spill to the
> heap transparently -- SmallVec derefs to `&[u8]`, so the Arrow `BinaryArray`
> sees an identical byte sequence either way and the Parquet output is
> byte-for-byte unchanged.
>
> Not interned, and not an arena. Interning is the wrong shape: these are
> payload bytes rather than names, so the pool would approach one entry per
> row and buy nothing. An arena -- one shared buffer with per-row offsets --
> would be sound, but it has to travel with the rows across the channel to the
> writer thread, which turns the batch type from `Vec<FieldRecord>` into a
> struct carrying a blob. The reason it was not taken is that the case for it
> shrank first: bounding the writer's buffer (see `writer::MAX_BUFFERED_ROWS`)
> cut the live payload vectors from ~390,000 to ~90,000, and `validate` --
> which builds every record and writes no file -- brackets the whole
> remaining writer path at ~41 MB.
