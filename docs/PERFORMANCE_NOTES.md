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
appears. A build that starts sending either section now moves these counters,
and the pinned `frame_*` export baselines, instead of nothing.

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

### Registering a new export group

Source: `crates/vrf-schema/src/cache.rs`, `NetGuidCache::index_group`, whose
doc comment points here. Measured 2026-09-28 against main 259ed10, on a
shared machine that other jobs kept fully loaded throughout.

`add_export_group` used to end every successful call by clearing `by_path`,
`by_index` and `by_leaf` and registering every group again, one `String` per
path spelling plus one per leaf. The checkpoint pass reads each checkpoint
into a fresh cache and adds its groups one at a time, all of them new, so a
checkpoint of n groups paid n(n+1)/2 registrations: 3.6-4.5 million per
export on the replays below, where 14-17 thousand are needed. A new group now
registers only itself; a merge or a reused index still rebuilds.

`vrfkit export <replay> --out <new dir> --checkpoints`, 7 A/B pairs per
replay alternating which side runs first, both sides at ABOVE_NORMAL
priority, wall clock around the process, medians:

| replay | checkpoints / groups | before | after | paired difference | faster |
|---|---|---|---|---|---|
| 13.05 `f2872006` (88 MB) | 33 / 17,102 | 5.574 s | 4.445 s | 1.082 s | 7 of 7 |
| 13.05 `535c22e5` (84 MB) | 30 / 15,152 | 5.182 s | 4.191 s | 1.092 s | 6 of 7 |
| 13.06 `c129014f` (89 MB) | 28 / 13,925 | 5.451 s | 4.621 s | 0.764 s | 6 of 7 |

The manifest's `elapsed_ms` moved the same way (paired medians 1,154, 1,124
and 836 ms). The same checkpoint group rows fed in-process through the crate
before and after the change -- fresh cache per checkpoint, the reader's
collision probe, then `add_export_group`, 9 alternating rounds -- took 1.30 s
-> 18 ms, 1.72 s -> 22 ms and 1.00 s -> 11 ms (medians). Peak commit did not
move: 200-202 MiB on both sides for `f2872006` over 3 pairs. Every Parquet
file stays byte-identical.

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

That paragraph is about the string columns. Which other columns get a
dictionary is the next section.

### Dictionary encoding is chosen per column

Source: `crates/vrf-export/src/writer.rs`, `Table::DICTIONARY_COLUMNS` and
`TableWriter::writer_properties`; each table's list, with its figures, is in
`crates/vrf-export/src/tables/`. Measured 2026-09-28 against main `259ed10`,
parquet-rs 59.1.0.

**What was wrong.** parquet-rs dictionary-encodes every non-boolean column
unless told otherwise (`DEFAULT_DICTIONARY_ENABLED = true`, for every physical
type, not only strings). `writer_properties` never turned that default off; it
only switched *on* the names in each table's `DICTIONARY_COLUMNS`. The lists
therefore selected nothing. All 14 movement columns, every time, packet and
GUID column, and `actors.event` were dictionary-encoded, while the comments
said movement had "nothing to dictionary" and that `event` was "cheaper as
plain Utf8". The loop dates from the crate's first commit (`9ded7ae`), and
nothing read the encodings back, so no test could see it.

**Method.** A throwaway build (never committed) read `VRFKIT_DICT_MODE`. `all`
reproduced main's writer properties: all 13 files of the reference replay
matched the committed baseline SHA-256s. `none` turned dictionary encoding off
for every column. Both exported, with `--checkpoints`, the 45 replays the A/B
harness selects: two per build folder from 11.06 to 13.06 (for 13.01, the
reference replay and one other), plus the 12.10, 12.11 and 13.00 public
fixtures. For every (replay, table, column), the column chunk's
`total_compressed_size` was summed over row groups. Parquet encodes each
column chunk independently, so a per-column choice can be predicted from
those two runs. On the reference replay the rebuilt writer came within 907
bytes of the prediction for every table, always smaller; the difference is
metadata outside the column chunks, mostly the footer.

**The rule.**

1. Every string column, `Utf8` or `Dictionary<_, Utf8>`, keeps its dictionary.
   The docs promise dictionary-encoded strings, and this leaves every string
   column's encoding exactly as it was.
2. Any other column gets a dictionary only where the dictionary was smaller
   than PLAIN, summed over the 45 replays.

Each table's comment gives its dictionary/plain ratios. The roundtrip tests
check each written file's footer against its list, and reject a list entry
that names no column or a BOOLEAN one; parquet-rs ignores both silently.

The measurement also rejected the obvious fix: honour the old lists, which
held only strings. Summed over the sample, that makes `checkpoint_fields` 1.40x
the size it had under the everything-dictionary default, because checkpoints
restate the same payloads: on the reference replay 343,683 checkpoint
`raw_bits` values hold 7,084 distinct payloads, against 321,735 distinct in
1,065,872 in `fields`. So `checkpoint_fields` keeps dictionaries on `raw_bits`
and `value_f64`, both of which are smaller PLAIN in `fields`.

**Result.** Old and new bytes of every Parquet file, summed over the same 45
replays (from the A/B run described below):

| Table | Old bytes | New bytes | Change |
|---|---:|---:|---:|
| `fields` | 751,968,735 | 575,691,555 | -23.4% |
| `movement` | 1,496,335,072 | 948,646,955 | -36.6% |
| `actors` | 4,164,501 | 3,214,316 | -22.8% |
| `net_guids` | 7,669,274 | 5,444,904 | -29.0% |
| `events` | 620,604 | 576,286 | -7.1% |
| `partials` | 112,725 | 112,725 | 0 (zero rows; byte-identical) |
| `checkpoint_fields` | 53,543,325 | 52,044,311 | -2.8% |
| `checkpoint_actors` | 1,312,330 | 1,200,837 | -8.5% |
| `checkpoint_net_guids` | 13,733,516 | 9,274,695 | -32.5% |
| `checkpoint_blocks` | 8,391,956 | 5,319,390 | -36.6% |
| `checkpoint_guid_entries` | 48,934,940 | 34,795,162 | -28.9% |
| `checkpoint_export_groups` | 1,642,624 | 1,207,575 | -26.5% |
| `checkpoint_export_fields` | 14,153,793 | 11,665,771 | -17.6% |
| **all 13** | 2,402,583,395 | 1,649,194,482 | -31.4% |

The lists were chosen on those 45 replays, so the same export was repeated on
20 that were not among them, four each from 13.01, 13.02, 13.04, 13.05 and
13.06. No table grew on any of them; 1,086,341,147 bytes became 750,058,550
(-31.0%), with row counts unchanged. (`partials` held zero rows there too.)

The reference replay, as pinned in `tools/baselines/`:

| Table | Old bytes | New bytes | Change |
|---|---:|---:|---:|
| `fields` | 16,455,178 | 12,680,657 | -22.9% |
| `movement` | 31,886,449 | 19,984,802 | -37.3% |
| `actors` | 87,281 | 68,243 | -21.8% |
| `net_guids` | 153,606 | 114,423 | -25.5% |
| `events` | 13,411 | 12,455 | -7.1% |
| `partials` | 2,505 | 2,505 | 0 (zero rows; byte-identical) |
| `checkpoint_fields` | 1,218,992 | 1,183,936 | -2.9% |
| `checkpoint_actors` | 27,118 | 24,345 | -10.2% |
| `checkpoint_net_guids` | 277,718 | 175,916 | -36.7% |
| `checkpoint_blocks` | 175,103 | 112,704 | -35.6% |
| `checkpoint_guid_entries` | 928,714 | 651,660 | -29.8% |
| `checkpoint_export_groups` | 27,041 | 20,799 | -23.1% |
| `checkpoint_export_fields` | 287,130 | 241,210 | -16.0% |

Values are unchanged. `ab_compare.py` (pyarrow rows hashed into a multiset)
found no row present in only one side, in any table of any of the 45 replays.
DuckDB 1.5.5, an independent Parquet reader, agrees on all 585 table pairs:
equal row counts and an empty `EXCEPT ALL` both ways. On the reference replay
pyarrow's `Table.equals(check_metadata=True)` holds for all 13 files; the
embedded Arrow schema is unchanged, so `Dictionary` columns still read back as
dictionaries, and DuckDB's `parquet_metadata()` shows a dictionary page on
every string column of the new files. `to_valplay_bundle.py` writes
byte-identical `events.ndjson`, `movement.ndjson` and `manifest.json` from the
old and new exports of that replay.

**Timing.** Measured on a shared 32-thread machine that other jobs kept at
70-100% CPU, so wall time is noisy. Each pair ran both binaries back to back,
alternating which went first, into a fresh output directory. Process CPU time
(user plus kernel, all threads) is given beside wall time, because the Parquet
writers run on threads of their own:

| Export | Pairs | Wall, main -> new (median) | CPU, main -> new (median) | New used less CPU |
|---|---:|---|---|---:|
| `02d4d478` (13.01, 48 MB) | 9 | 1.706 -> 1.748 s | 3.641 -> 2.922 s (-20%) | 9 of 9 |
| `712b571b` (13.06, 77 MB) | 9 | 2.539 -> 2.437 s | 4.969 -> 4.172 s (-16%) | 9 of 9 |
| `02d4d478 --checkpoints` | 7 | 2.874 -> 2.633 s | 4.625 -> 3.922 s (-15%) | 7 of 7 |
| `712b571b --checkpoints` | 7 | 5.041 -> 4.105 s | 7.359 -> 5.703 s (-23%) | 7 of 7 |

Wall time does not move beyond the noise: the median per-pair ratio is
0.94-1.00 in all four rows. The writers are off the critical path, so the
encoding work saved shows up as CPU, not as a shorter run. `bench.json`'s
0.791 s cannot be compared on this machine state, since main itself took
1.706 s here.

Read side, reference replay, 9 alternating pairs: pyarrow `read_table` takes
28.9 -> 16.2 ms on `movement.parquet` and 24.3 -> 19.6 ms on `fields.parquet`.
DuckDB `sum(pos_x), sum(pos_y), sum(time_ms)` over movement takes 17.9 ->
9.4 ms. Two DuckDB queries over `fields` stay within noise: `GROUP BY
group_path` (a string column whose encoding did not change) at 9.7 -> 10.2 ms,
and `sum(bit_count) WHERE time_ms > 600000` at 4.8 -> 5.0 ms.
`to_valplay_bundle.py` took 18.87 -> 18.27 s over 3 pairs.

**Left alone, on purpose.**

- Twelve string columns measured larger as dictionaries, and rule 1 keeps
  them. Together they cost 4,375,159 bytes over the 45 replays, 3,228,193 of it
  `checkpoint_guid_entries.literal_path` (1.19x). The ratios:
  `events.metadata` 1.49, `checkpoint_actors.checkpoint_id` 1.37,
  `checkpoint_export_groups.checkpoint_id` 1.34, `checkpoint_actors.event`
  1.33, `checkpoint_guid_entries.checkpoint_id` 1.30,
  `checkpoint_export_fields.checkpoint_id` 1.30,
  `checkpoint_export_groups.group_path` 1.25,
  `checkpoint_guid_entries.literal_path` 1.19,
  `checkpoint_export_fields.rendered_name` 1.17,
  `checkpoint_export_fields.fname_base` 1.15, `events.id` 1.07,
  `checkpoint_blocks.checkpoint_id` 1.02. Writing them PLAIN means narrowing
  the documented promise first.
- `partials` wrote zero rows on all 45 replays, so its non-string columns take
  the PLAIN default with no measurement behind it.
- The byte-budget flush cuts the checkpoint declaration tables
  (`checkpoint_guid_entries`, `checkpoint_export_groups`,
  `checkpoint_export_fields`) into row groups: 10 row groups for the reference
  replay's 74,270 GUID entries. A dictionary is per row group, so those
  figures depend on the cuts. A fix in review at the time closes those row
  groups only when the budget is crossed (one row group each on the reference
  replay), so the same two runs were repeated on a build carrying it. The
  per-column sums predict that, with these lists, none of the 45 replays grows
  any table against that build's everything-dictionary output, and that the
  three tables shrink 41.1%, 29.2% and 17.3% in total. A build with both
  changes wrote, on the reference replay, 219,662 bytes for
  `checkpoint_guid_entries` (396,821 with the fix alone), 16,481 for
  `checkpoint_export_groups` (22,627) and 106,370 for
  `checkpoint_export_fields` (121,648), with values equal to main's. Four
  small columns flip to favour a dictionary there:
  `checkpoint_index` in two of the tables, and `exported_flag` and
  `fname_number` in `checkpoint_export_fields`. Each is worth at most 41 bytes
  per file on average, against 112-222 bytes the other way under the cuts
  measured here. The strings that measured larger as dictionaries become
  smaller under one row group: `literal_path` 0.51, `rendered_name` 0.79,
  `fname_base` 0.77. Re-measure once that fix lands.
- One decision splits by build. `checkpoint_export_fields.slot` and `handle`
  measure 0.89 on the 37 replays from 11.06-13.01 (fixtures included), but
  2.01 on the 8 from 13.02-13.06, so the total picks PLAIN.
  `checkpoint_actors.channel_index` also flips (0.96 against 1.06), on a
  column of about 2 KB per file.
- Only dictionary on or off was measured. Other encodings were not tried,
  such as BYTE_STREAM_SPLIT for the float columns and DELTA_BINARY_PACKED for
  `time_ms`/`packet_id`.

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
