//! ClassNetCache payload brute-forcer for unresolved groups.
//!
//! When a ClassNetCache block's group is never declared in the replay,
//! `function_count` -- the handle width for the RPC stream -- is unknown, and
//! `vrf_net`'s ClassNetCache parser refuses to walk it. The payload is
//! preserved as raw bits (one `__vrfkit_unresolved_class_net_cache_payload__`
//! row), but every RPC inside is lost.
//!
//! This module recovers that structure. For a payload whose group is a known
//! bare instance name (e.g. `AbilitiesAndBuffsComponent`), it brute-forces
//! `function_count` from 2 to 256 by walking the ClassNetCache stream with each
//! candidate and checking whether the walk consumes the buffer exactly.
//!
//! # Wire format
//!
//! The ClassNetCache RPC stream (confirmed against `parse_class_net_cache` in
//! `vrf-net` and the C# `ParseClassNetCachePayload`) is:
//!
//! ```text
//! loop:
//!   handle       = SerializedInt(max(function_count, 2))
//!   payload_bits = IntPacked
//!   payload      = sub-reader of payload_bits bits
//! ```
//!
//! There is no checksum bit and no explicit handle-0 terminator: the loop runs
//! until the bit reader is at end. A "clean walk" is one where every iteration
//! reads a handle, a well-formed payload-length, and a payload that fits, and
//! the final payload ends exactly at the stream boundary.
//!
//! # The function_count ambiguity
//!
//! `SerializedInt(max)` spends `floor(log2(max))` bits unconditionally plus one
//! extra bit conditionally. For handle values that are small relative to `max`,
//! several adjacent `function_count` values produce the same handle width and
//! therefore the same walk. On `AbilitiesAndBuffsComponent` every payload
//! contains a single RPC at handle 1, and `function_count` 34-65 all walk
//! cleanly -- the handle takes 6 bits in every case. The brute-force returns
//! the **minimum** valid `function_count`, which is sufficient to decode the
//! stream: every fc in the valid range produces identical RPC structure.
//!
//! # Payload kind and inner structure
//!
//! ClassNetCache framing also carries custom-delta properties. The legacy
//! `CncRpc` and `function_count` names do not establish that an entry is a
//! function call. The C# reference reader dispatches custom-delta properties
//! separately from `ReceivedRPC` after reading this shared outer framing.
//!
//! See [`AbilitiesActivation`] for the measured `AbilitiesAndBuffsComponent`
//! inner structure and what it does and does not establish.

use vrf_bitio::BitReader;

/// Maximum `function_count` to try. Real ClassNetCache groups in VALORANT
/// declare at most a few dozen functions; 256 is a generous ceiling.
const MAX_FC: u32 = 256;

/// One decoded RPC from a brute-forced ClassNetCache stream.
#[derive(Debug, Clone)]
pub struct CncRpc {
    /// The function handle (0-indexed).
    pub handle: u32,
    /// Payload bit count.
    pub payload_bits: u32,
    /// Bit offset of the payload within the original buffer.
    pub payload_offset: u64,
}

/// The result of brute-forcing a ClassNetCache payload.
#[derive(Debug, Clone)]
pub struct BruteForceResult {
    /// The minimum `function_count` that produces a clean walk.
    pub function_count: u32,
    /// The RPCs decoded with that function_count.
    pub rpcs: Vec<CncRpc>,
    /// Another `function_count` that walks the SAME bits just as cleanly but
    /// into a DIFFERENT set of RPCs, if one exists.
    ///
    /// The search returns the first clean walk, and the module reasoned that
    /// this was safe because adjacent counts inside one handle-width band
    /// produce identical structure. True, and not the whole condition: two
    /// counts in DIFFERENT bands can both divide the same buffer exactly. This
    /// module's own fixtures depend on knowing it -- `build_one_rpc_stream`
    /// fills payloads with 1-bits specifically because a zero-filled one lets
    /// wrong counts walk cleanly.
    ///
    /// `None` means the search checked every candidate and found no competing
    /// parse, which is what makes the returned count a measurement rather than
    /// the first thing that happened to fit.
    pub ambiguous_with: Option<u32>,
}

/// Brute-force `function_count` for a ClassNetCache payload whose group is
/// unresolved.
///
/// Tries each `function_count` from 2 to `MAX_FC`. The minimum value whose walk
/// consumes the entire buffer cleanly (zero residual bits, at least one RPC) is
/// returned. If no value works, returns `None`.
///
/// The payload is the transform-decoded byte buffer of the content block, and
/// `bit_count` is its declared bit length (the same pair the preservation row
/// stores).
///
/// The caller is expected to gate this on a specific group path (e.g.
/// `AbilitiesAndBuffsComponent`) so that the brute-force is not attempted on
/// every unresolved block. A group whose structure is genuinely unknown will
/// return `None` and the preservation row stays as the only record.
#[must_use]
pub fn brute_force_function_count(payload: &[u8], bit_count: u32) -> Option<BruteForceResult> {
    // The whole range is scanned even after a hit. Returning the first clean
    // walk was justified by adjacent counts in one handle-width band producing
    // identical structure -- which is true, and is a narrower statement than
    // the one being relied on. Counts in different bands can also divide the
    // same buffer exactly, and then the first hit is a choice between parses
    // rather than the only reading. See `ambiguous_with`.
    let mut chosen: Option<BruteForceResult> = None;
    for fc in 2..=MAX_FC {
        let Some(rpcs) = decode_cnc_payload(payload, bit_count, fc) else {
            continue;
        };
        match &mut chosen {
            None => {
                chosen = Some(BruteForceResult {
                    function_count: fc,
                    rpcs,
                    ambiguous_with: None,
                });
            }
            Some(first) => {
                if first.ambiguous_with.is_none() && !same_structure(&first.rpcs, &rpcs) {
                    first.ambiguous_with = Some(fc);
                }
            }
        }
    }
    chosen
}

/// Whether two candidate walks recovered the same RPC framing.
///
/// Structure is the handle, size and position of every RPC. Two counts that
/// agree on all three describe the same stream and the choice between them does
/// not matter; a disagreement on any of them means the payload has more than
/// one clean reading.
fn same_structure(a: &[CncRpc], b: &[CncRpc]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| {
            x.handle == y.handle
                && x.payload_bits == y.payload_bits
                && x.payload_offset == y.payload_offset
        })
}

/// Walk a ClassNetCache stream with a known `function_count` and return the
/// RPCs it contains.
///
/// This is the per-payload decode used after the function_count has been
/// determined (e.g. by [`brute_force_function_count`] on a representative
/// sample, or by a hardcoded constant for a known group). Mirrors
/// `parse_class_net_cache` in `vrf-net` but returns the RPC list instead of
/// driving a sink. Returns `None` when the walk is not clean -- a malformed
/// read or no RPC at all -- rather than partial results; the caller should
/// keep the preservation row in that case.
#[must_use]
pub fn decode_cnc_payload(
    payload: &[u8],
    bit_count: u32,
    function_count: u32,
) -> Option<Vec<CncRpc>> {
    let mut reader = BitReader::with_bit_len(payload, u64::from(bit_count)).ok()?;
    let handle_max = function_count.max(2);
    let mut rpcs = Vec::new();
    // Every read fails rather than run past the window, so the loop leaves
    // only with the window consumed exactly (a sub-byte tail fails the next
    // read). That exactness is what separates a decode from a payload of
    // another format that merely parses into something plausible.
    while !reader.at_end() {
        let handle = reader.read_serialized_int(handle_max).ok()?;
        let payload_bits = reader.read_int_packed().ok()?;
        let payload_offset = reader.position();
        reader.skip_bits(u64::from(payload_bits)).ok()?;
        rpcs.push(CncRpc {
            handle,
            payload_bits,
            payload_offset,
        });
    }
    (!rpcs.is_empty()).then_some(rpcs)
}

/// Decoded inner structure of an `AbilitiesAndBuffsComponent` ClassNetCache
/// RPC payload.
///
/// The outer RPC framing -- function handle plus payload size -- is recovered
/// by [`decode_cnc_payload`]. This decomposes the payload itself, which was
/// long believed to be an opaque class-specific blob. It is not: across
/// thousands of payloads the layout is fully deterministic. A single flag bit
/// (always `1`) is followed by a stream of little-endian `u32` words and an
/// optional sub-32-bit trailing residual, and `bit_count == 1 + 32 * words +
/// trailing` holds exactly on every payload. This Rust helper stops there --
/// it is a legacy bit-slicing routine, not a semantic decoder.
///
/// A separate Python reader (`tools/extract_fastarray_observations.py`) reads
/// the same payload under a different grammar and measures it as custom-delta
/// FastArray framing: a support bit, four i32 header words, deleted item IDs,
/// and changed items with packed handle/width property streams.
///
/// Word positions are not semantic field declarations either way. A September
/// 2026 twelve-export audit observed increasing first words within
/// actor/object/channel sequences, but the second word did not always equal
/// the previously observed first word. Leading pairs also occurred on
/// different identities. These observations do not establish an engine
/// prediction-key type, gameplay state-sync event, cast identity, or buff
/// meaning. The complete word list and residual remain available without
/// assigning those roles; see `docs/GAS_AND_PATCHVOLUME_INVESTIGATION.md` for
/// the evidence scope, including the FastArray measurement above.
#[derive(Debug, Clone)]
pub struct AbilitiesActivation {
    /// The leading flag bit. Observed to be `1` on every payload; kept as a
    /// field so a future build that clears it is visible rather than silent.
    pub flag: bool,
    /// The little-endian `u32` words immediately after the flag bit.
    pub words: Vec<u32>,
    /// Trailing bits that did not form a full word, packed LSB-first.
    pub trailing: u32,
    /// Number of valid bits in `trailing` (always `0..32`).
    pub trailing_bit_count: u32,
}

impl AbilitiesActivation {
    /// Return the first two raw words when present.
    ///
    /// The legacy accessor name does not establish a prediction-key type or
    /// an identity suitable for joining ability casts.
    #[must_use]
    pub fn key_pair(&self) -> Option<(u32, u32)> {
        Some((*self.words.first()?, *self.words.get(1)?))
    }
}

/// Decompose the inner payload of an `AbilitiesAndBuffsComponent` ClassNetCache
/// RPC into its deterministic structure.
///
/// Skips the flag bit, reads whole little-endian `u32` words while at least 32
/// bits remain, and captures any trailing residual. Returns `None` only for an
/// empty payload: the decomposition is a pure bit-stream walk, so on a
/// non-empty payload it always succeeds and the words are exactly the bits the
/// wire carried.
#[must_use]
pub fn decode_abilities_and_buffs_inner(
    payload: &[u8],
    bit_count: u32,
) -> Option<AbilitiesActivation> {
    let mut reader = BitReader::with_bit_len(payload, u64::from(bit_count)).ok()?;
    if reader.bits_remaining() == 0 {
        return None;
    }
    let flag = reader.read_bit().ok()?;
    let mut words = Vec::new();
    while reader.bits_remaining() >= 32 {
        match reader.read_u32() {
            Ok(w) => words.push(w),
            Err(_) => break,
        }
    }
    let trailing_bit_count = u32::try_from(reader.bits_remaining()).unwrap_or(0);
    let trailing = if trailing_bit_count > 0 {
        reader.read_bits(trailing_bit_count).unwrap_or(0) as u32
    } else {
        0
    };
    Some(AbilitiesActivation {
        flag,
        words,
        trailing,
        trailing_bit_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_bits::BitWriter;

    /// Build a ClassNetCache stream with one RPC at handle 1, using a given
    /// function_count to determine the handle width.
    ///
    /// The payload is filled with 1-bits (`true`) rather than zeros. This is
    /// load-bearing for the brute-force tests: a zero-filled payload lets
    /// wrong `function_count` values walk cleanly because every misaligned
    /// IntPacked read returns 0 (zero-cost payload), producing many tiny
    /// garbage RPCs that consume the buffer. A one-filled payload makes those
    /// misaligned reads return large values that overrun the stream, so only
    /// the correct handle width walks cleanly.
    fn build_one_rpc_stream(function_count: u32, payload_bits: u32) -> (Vec<u8>, u32) {
        BitWriter::new()
            .serialized_int(1, function_count.max(2))
            .int_packed(payload_bits)
            .repeat(true, payload_bits as usize)
            .finish()
    }

    /// A single-RPC payload at handle 1 should resolve to the minimum
    /// function_count whose handle width matches the one it was written with:
    /// every count in one handle-width band walks the same, and the search
    /// picks the smallest.
    ///
    /// The one-filled fixtures are unambiguous, so `ambiguous_with` must stay
    /// clear on them -- otherwise it would fire on every payload and mean
    /// nothing.
    #[test]
    fn single_rpc_finds_minimum_fc() {
        // fc=34: for handle=1, ilog2(34)=5 bits, then 1+32=33 < 34, so the
        // extra bit is read: 6 bits total. The minimum fc that produces a
        // 6-bit handle for value=1 is 34 (fc=33 gives 5 bits, fc=34 forces
        // the extra read). fc=50 is inside the same band, so it resolves to 34
        // too.
        for (written_with, payload_bits) in [(34, 100), (50, 64)] {
            let (data, bit_count) = build_one_rpc_stream(written_with, payload_bits);
            let result = brute_force_function_count(&data, bit_count).expect("a valid fc");
            assert_eq!(result.function_count, 34, "written with {written_with}");
            assert_eq!(result.rpcs.len(), 1);
            assert_eq!(result.rpcs[0].handle, 1);
            assert_eq!(result.rpcs[0].payload_bits, payload_bits);
            assert_eq!(result.ambiguous_with, None, "{result:?}");
        }
    }

    /// A stream written with a larger fc that changes the handle width should
    /// still resolve correctly.
    #[test]
    fn single_rpc_with_larger_fc() {
        // fc=128: for handle=1, ilog2(128)=7, 1+128=129 >= 128, so 7 bits.
        // fc=65..127 also gives 7 bits for value=1 (1+64=65 < 65..127), but
        // fc=64 gives 6 bits (1+64=65 >= 64). The minimum fc requiring the
        // 7-bit encoding is 66. But wait -- for value=1 with fc=66: ilog2=6,
        // read 6 bits -> 1, 1+64=65 < 66, read extra bit -> 7 bits.
        //
        // For fc=65: ilog2=6, read 6 bits -> 1, 1+64=65 >= 65 -> 6 bits.
        // For fc=66: ilog2=6, read 6 bits -> 1, 1+64=65 < 66 -> 7 bits.
        //
        // So the minimum fc for 7-bit handle is 66, not 128.
        let (data, bit_count) = build_one_rpc_stream(128, 50);
        let result = brute_force_function_count(&data, bit_count);
        assert!(result.is_some());
        let result = result.unwrap();
        assert_eq!(result.function_count, 66);
        assert_eq!(result.rpcs[0].handle, 1);
    }

    /// Multiple RPCs in one stream should all be recovered.
    #[test]
    fn multiple_rpcs() {
        let (data, bit_count) = BitWriter::new()
            // RPC 1: handle=3, payload=16 bits of 1s
            .serialized_int(3, 10)
            .int_packed(16)
            .repeat(true, 16)
            // RPC 2: handle=7, payload=8 bits of 1s
            .serialized_int(7, 10)
            .int_packed(8)
            .repeat(true, 8)
            .finish();
        let result = brute_force_function_count(&data, bit_count);
        assert!(result.is_some());
        let result = result.unwrap();
        assert_eq!(result.rpcs.len(), 2);
        assert_eq!(result.rpcs[0].handle, 3);
        assert_eq!(result.rpcs[0].payload_bits, 16);
        assert_eq!(result.rpcs[1].handle, 7);
        assert_eq!(result.rpcs[1].payload_bits, 8);
    }

    /// A zero-bit payload returns None (no RPCs).
    #[test]
    fn empty_payload_returns_none() {
        let data = vec![];
        let result = brute_force_function_count(&data, 0);
        assert!(result.is_none());
    }

    /// Build a bit buffer from an explicit flag + a sequence of LE u32 words +
    /// an optional trailing residual. Mirrors the wire layout of an
    /// `AbilitiesAndBuffsComponent` RPC payload.
    fn build_activation_stream(
        flag: bool,
        words: &[u32],
        trailing_bits: u32,
        trailing: u32,
    ) -> (Vec<u8>, u32) {
        let mut bits = BitWriter::new();
        bits.bits(u64::from(flag), 1);
        for &w in words {
            bits.bits(u64::from(w), 32);
        }
        bits.bits(u64::from(trailing), trailing_bits).finish()
    }

    /// A real-shape payload: flag(1) + 5 LE u32 words, no trailing. This is
    /// the 161-bit family observed across thousands of payloads, the simplest
    /// and most common `AbilitiesAndBuffsComponent` RPC.
    #[test]
    fn abilities_inner_flag_then_u32_stream() {
        let words = [5u32, 3, 1, 0, 2];
        let (data, bit_count) = build_activation_stream(true, &words, 0, 0);
        let decoded = decode_abilities_and_buffs_inner(&data, bit_count).unwrap();
        assert!(decoded.flag);
        assert_eq!(decoded.words, words);
        assert_eq!(decoded.trailing_bit_count, 0);
        assert_eq!(decoded.key_pair(), Some((5, 3)));
    }

    /// A large payload carries a sub-32-bit trailing residual after the last
    /// whole word. The decoder must capture it without losing bits.
    #[test]
    fn abilities_inner_captures_trailing_residual() {
        // flag(1) + 1 word + 16 trailing bits = 49 bits.
        let (data, bit_count) = build_activation_stream(true, &[7], 16, 0xABCD);
        let decoded = decode_abilities_and_buffs_inner(&data, bit_count).unwrap();
        assert!(decoded.flag);
        assert_eq!(decoded.words, vec![7]);
        assert_eq!(decoded.trailing_bit_count, 16);
        assert_eq!(decoded.trailing & 0xFFFF, 0xABCD);
    }

    /// An empty payload has no flag bit and yields `None`.
    #[test]
    fn abilities_inner_empty_is_none() {
        assert!(decode_abilities_and_buffs_inner(&[], 0).is_none());
    }

    /// A flag bit alone (no words, no trailing) decodes cleanly.
    #[test]
    fn abilities_inner_flag_only() {
        let (data, bit_count) = build_activation_stream(true, &[], 0, 0);
        let decoded = decode_abilities_and_buffs_inner(&data, bit_count).unwrap();
        assert!(decoded.flag);
        assert!(decoded.words.is_empty());
        assert_eq!(decoded.trailing_bit_count, 0);
        assert_eq!(decoded.key_pair(), None);
    }

    /// A single-word payload has a flag but no key pair.
    #[test]
    fn abilities_inner_single_word_has_no_key_pair() {
        let (data, bit_count) = build_activation_stream(true, &[42], 0, 0);
        let decoded = decode_abilities_and_buffs_inner(&data, bit_count).unwrap();
        assert_eq!(decoded.words, vec![42]);
        assert_eq!(decoded.key_pair(), None);
    }

    /// A zero-filled payload can walk cleanly under SEVERAL function counts
    /// that disagree about how many RPCs it holds, and the search must say so
    /// rather than return the first one as though it were the only one.
    ///
    /// This is not hypothetical, and this module already knew it: the doc on
    /// `build_one_rpc_stream` above explains that the fixtures are filled with
    /// 1-bits precisely because "a zero-filled payload lets wrong
    /// `function_count` values walk cleanly ... producing many tiny garbage
    /// RPCs that consume the buffer". The module's own claim -- that every fc
    /// in the valid range produces identical RPC structure -- holds only
    /// within one handle-WIDTH band; it says nothing about two bands whose
    /// per-RPC size happens to divide the same buffer.
    ///
    /// 90 zero bits is exactly that: fc=2 gives a 1-bit handle and a zero
    /// payload, so 10 RPCs of 9 bits; fc=3 gives a 2-bit handle, so 9 RPCs of
    /// 10 bits. Both consume the buffer exactly and they are different parses.
    #[test]
    fn an_ambiguous_payload_reports_the_competing_function_count() {
        let data = vec![0u8; 12]; // 96 bits of storage, 90 declared
        let result = brute_force_function_count(&data, 90).expect("fc=2 walks cleanly");

        assert_eq!(result.function_count, 2);
        assert_eq!(result.rpcs.len(), 10, "fc=2 reads ten 9-bit RPCs");
        assert_eq!(
            result.ambiguous_with,
            Some(3),
            "fc=3 parses the same bits into nine RPCs and must be reported",
        );
    }
}
