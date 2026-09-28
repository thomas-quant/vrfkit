//! Bunch header structure and partial bunch reassembly.

use crate::error::PartialSequenceKind;
use crate::types::ChannelCloseReason;

/// Maximum simultaneously active partial-bunch assemblies.
///
/// A normal replay measured 0 and Unreal can only advance each one with a
/// packet-sized fragment. 4,096 leaves ample protocol headroom while bounding
/// a stream that sends one initial fragment on each new channel forever.
pub const MAX_ACTIVE_PARTIAL_BUNCHES: usize = 4_096;

/// Maximum raw bits retained across all partial-bunch assemblies (64 MiB).
pub const MAX_BUFFERED_PARTIAL_BITS: usize = 64 * 1024 * 1024 * 8;

/// Parsed bunch header -- all fields that describe one bunch within a packet.
///
/// See `RawPacketReader::parse_bunch_header` in [`crate::packet`] for the bit
/// layout that produces these fields.
#[derive(Debug, Clone, Default)]
pub struct RawBunchHeader {
    /// Packet this bunch belongs to.
    pub packet_id: i32,
    /// Channel index.
    pub ch_index: u32,
    /// Channel is being opened.
    pub b_open: bool,
    /// Channel is being closed.
    pub b_close: bool,
    /// Close reason implies dormancy (actor still alive).
    pub b_dormant: bool,
    /// Replication is paused for this channel.
    pub b_is_replication_paused: bool,
    /// Bunch is reliable (has sequence guarantees).
    pub b_reliable: bool,
    /// Bunch is part of a multi-fragment sequence.
    pub b_partial: bool,
    /// First fragment of a partial bunch.
    pub b_partial_initial: bool,
    /// Last fragment of a partial bunch.
    pub b_partial_final: bool,
    /// Bunch carries package-map export data.
    pub b_has_package_map_exports: bool,
    /// Bunch carries must-be-mapped GUIDs.
    pub b_has_must_be_mapped_guids: bool,
    /// Sequence number (reliable or packet-derived).
    pub ch_sequence: i32,
    /// Reason the channel was closed.
    pub close_reason: ChannelCloseReason,
    /// Payload size in bits.
    pub payload_bit_count: i32,
    /// Bit offset where the payload begins within the packet.
    pub payload_bit_offset: i64,

    // --- tracking flags set by partial-bunch logic ---
    /// A partial-bunch sequence error was detected for this fragment.
    pub has_partial_error: bool,
    /// Exact sequence/alignment failure, when one was identified.
    pub partial_error_kind: Option<PartialSequenceKind>,
    /// This fragment completed a partial bunch (was the valid final).
    pub is_partial_completed: bool,
    /// Per-channel reader state could not admit or advance this channel.
    pub has_channel_limit_error: bool,
}

/// Partial bunch accumulator: reassembles multi-fragment bunches.
///
/// Each non-final fragment must be byte-aligned (bit count % 8 == 0).
/// Fragments are concatenated into a growable buffer; on completion
/// the stitched payload is returned for content-block framing.
pub struct PartialBunchAccumulator {
    /// Per-channel fragment state.
    fragments: std::collections::HashMap<u32, AccumulatorState>,
    total_buffered_bits: usize,
    max_active: usize,
    max_buffered_bits: usize,
}

struct AccumulatorState {
    ch_sequence: i32,
    reliable: bool,
    is_complete: bool,
    stored_header: RawBunchHeader,
    buffer: Vec<u8>,
    bit_count: usize,
}

/// Raw partial payload removed from reassembly without being decoded.
pub struct PreservedPartial {
    pub header: RawBunchHeader,
    pub buffer: Vec<u8>,
    pub bit_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartialDiscardCause {
    Sequence(PartialSequenceKind),
    Resource(PartialResourceLimit),
}

/// Result of adding a fragment to the accumulator.
pub struct PartialBunchResult {
    /// Updated header (may have error flags set).
    pub header: RawBunchHeader,
    /// Whether the caller should process the completed payload.
    pub should_process: bool,
    /// Resource limit that refused this fragment, if any.
    pub resource_limit: Option<PartialResourceLimit>,
    /// Previously/currently buffered bits discarded by the refusal.
    pub discarded_bits: usize,
    /// Sequence/alignment cause when this fragment was rejected.
    pub error_kind: Option<PartialSequenceKind>,
    /// The fragment replaced an incomplete initial, independently of whether
    /// a second error later rejected the replacement.
    pub overlapping_initial: bool,
    /// Earlier in-flight payload displaced while handling this fragment.
    pub displaced: Vec<(PreservedPartial, PartialDiscardCause)>,
}

/// Which bounded partial-reassembly resource refused a fragment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartialResourceLimit {
    /// Too many channel assemblies were simultaneously active.
    ActiveStates,
    /// The checked aggregate bit count overflowed or exceeded its memory cap.
    BufferedBits,
    /// Reserving the already-bounded destination buffer failed.
    Allocation,
}

impl PartialBunchAccumulator {
    /// Create a new empty accumulator.
    #[must_use]
    pub fn new() -> Self {
        Self::with_limits(MAX_ACTIVE_PARTIAL_BUNCHES, MAX_BUFFERED_PARTIAL_BITS)
    }

    fn with_limits(max_active: usize, max_buffered_bits: usize) -> Self {
        Self {
            fragments: std::collections::HashMap::new(),
            total_buffered_bits: 0,
            max_active,
            max_buffered_bits,
        }
    }

    /// Number of channel assemblies currently awaiting a final fragment.
    #[must_use]
    pub fn active_count(&self) -> usize {
        self.fragments.len()
    }

    /// Raw bits retained across all active assemblies.
    #[must_use]
    pub fn total_buffered_bits(&self) -> usize {
        self.total_buffered_bits
    }

    /// Add a fragment. Returns whether the bunch is now complete.
    ///
    /// `payload_bits` / `payload_data` are the raw bits from the bunch payload.
    /// For non-final fragments, the bit count must be byte-aligned.
    #[allow(clippy::too_many_arguments)]
    pub fn add_fragment(
        &mut self,
        ch_index: u32,
        mut header: RawBunchHeader,
        payload_data: &[u8],
        payload_bit_count: usize,
        stats_partial_errors: &mut u64,
        stats_partial_fragments: &mut u64,
        stats_partial_completed: &mut u64,
    ) -> PartialBunchResult {
        if header.b_partial_initial
            && !self.fragments.contains_key(&ch_index)
            && self.fragments.len() >= self.max_active
        {
            *stats_partial_errors += 1;
            header.has_partial_error = true;
            return PartialBunchResult {
                should_process: false,
                header,
                resource_limit: Some(PartialResourceLimit::ActiveStates),
                discarded_bits: payload_bit_count,
                error_kind: None,
                overlapping_initial: false,
                displaced: Vec::new(),
            };
        }
        let (sequence_valid, mut displaced) =
            self.validate_sequence(ch_index, &mut header, stats_partial_errors);
        let sequence_discarded_bits = displaced.iter().map(|(p, _)| p.bit_count).sum::<usize>();
        let overlapping_initial =
            header.partial_error_kind == Some(PartialSequenceKind::OverlappingInitial);
        if !sequence_valid {
            let error_kind = header.partial_error_kind;
            return PartialBunchResult {
                should_process: false,
                header,
                resource_limit: None,
                discarded_bits: sequence_discarded_bits.saturating_add(payload_bit_count),
                error_kind,
                overlapping_initial,
                displaced,
            };
        }

        if payload_bit_count == 0 {
            // A zero-payload fragment is still a fragment that arrived: the
            // non-empty path below always counts one here, unconditionally,
            // before it looks at `b_partial_final`. Skipping it on this path
            // undercounted a bunch that took two fragments to complete as
            // having received only one.
            *stats_partial_fragments += 1;
            if !header.b_partial_final {
                return PartialBunchResult {
                    should_process: false,
                    header,
                    resource_limit: None,
                    discarded_bits: sequence_discarded_bits,
                    error_kind: None,
                    overlapping_initial,
                    displaced,
                };
            }
            // Final with zero payload. The same rule as the non-empty path
            // below: an errored header -- an overlapping initial that is also
            // final -- is not a completion. Marking it complete counted a
            // `partial_completed` that `should_process` (false for an errored
            // header) immediately contradicted, and left the complete-but-untaken
            // state in the map until end of stream. The assembly that header
            // just started holds no bits, so retiring it loses nothing.
            if header.has_partial_error {
                self.take(ch_index);
            } else if let Some(state) = self.fragments.get_mut(&ch_index) {
                state.is_complete = true;
                header.is_partial_completed = true;
                *stats_partial_completed += 1;
            }
            return PartialBunchResult {
                should_process: header.b_partial_final && !header.has_partial_error,
                header,
                resource_limit: None,
                discarded_bits: sequence_discarded_bits,
                error_kind: None,
                overlapping_initial,
                displaced,
            };
        }

        // Non-final fragments must be byte-aligned.
        if !header.b_partial_final && payload_bit_count % 8 != 0 {
            *stats_partial_errors += 1;
            header.has_partial_error = true;
            // For an initial, the state taken here is the empty one
            // `validate_sequence` started for this fragment. It is retired but
            // not a displaced payload: it holds no bits, and the caller already
            // reports the fragment itself, so returning it wrote a phantom
            // 0-bit row beside that one. Same in the two resource arms below.
            let extra = self.take(ch_index).filter(|_| !header.b_partial_initial);
            let discarded_bits = sequence_discarded_bits
                .saturating_add(extra.as_ref().map_or(0, |p| p.bit_count))
                .saturating_add(payload_bit_count);
            return PartialBunchResult {
                should_process: false,
                header,
                resource_limit: None,
                discarded_bits,
                error_kind: Some(PartialSequenceKind::NonByteAlignedFragment),
                overlapping_initial,
                displaced: {
                    displaced.extend(extra.map(|p| {
                        (
                            p,
                            PartialDiscardCause::Sequence(
                                PartialSequenceKind::NonByteAlignedFragment,
                            ),
                        )
                    }));
                    displaced
                },
            };
        }

        // Append bits to accumulator.
        if let Some(state) = self.fragments.get(&ch_index) {
            let state_bits = state.bit_count;
            let new_state_bits = state_bits.checked_add(payload_bit_count);
            let new_total_bits = self.total_buffered_bits.checked_add(payload_bit_count);
            if new_state_bits.is_none()
                || new_total_bits.is_none_or(|bits| bits > self.max_buffered_bits)
            {
                *stats_partial_errors += 1;
                header.has_partial_error = true;
                let prior = self.take(ch_index).filter(|_| !header.b_partial_initial);
                return PartialBunchResult {
                    should_process: false,
                    header,
                    resource_limit: Some(PartialResourceLimit::BufferedBits),
                    discarded_bits: sequence_discarded_bits
                        .saturating_add(prior.as_ref().map_or(0, |p| p.bit_count))
                        .saturating_add(payload_bit_count),
                    error_kind: None,
                    overlapping_initial,
                    displaced: {
                        displaced.extend(prior.map(|p| {
                            (
                                p,
                                PartialDiscardCause::Resource(PartialResourceLimit::BufferedBits),
                            )
                        }));
                        displaced
                    },
                };
            }
        }
        if let Some(state) = self.fragments.get_mut(&ch_index) {
            if !append_bits(
                &mut state.buffer,
                state.bit_count,
                payload_data,
                payload_bit_count,
            ) {
                *stats_partial_errors += 1;
                header.has_partial_error = true;
                let prior = self.take(ch_index).filter(|_| !header.b_partial_initial);
                return PartialBunchResult {
                    should_process: false,
                    header,
                    resource_limit: Some(PartialResourceLimit::Allocation),
                    discarded_bits: sequence_discarded_bits
                        .saturating_add(prior.as_ref().map_or(0, |p| p.bit_count))
                        .saturating_add(payload_bit_count),
                    error_kind: None,
                    overlapping_initial,
                    displaced: {
                        displaced.extend(prior.map(|p| {
                            (
                                p,
                                PartialDiscardCause::Resource(PartialResourceLimit::Allocation),
                            )
                        }));
                        displaced
                    },
                };
            }
            state.bit_count = state
                .bit_count
                .checked_add(payload_bit_count)
                .expect("checked above");
            self.total_buffered_bits = self
                .total_buffered_bits
                .checked_add(payload_bit_count)
                .expect("checked above");
            state.ch_sequence = header.ch_sequence;

            *stats_partial_fragments += 1;

            // Not `if header.b_partial_final` alone: `has_partial_error` can
            // already be set here (e.g. an overlapping `b_partial_initial`
            // that `validate_sequence` flagged but still let through with a
            // freshly-inserted state -- exactly what a header carrying both
            // `b_partial_initial` and `b_partial_final` produces). Marking
            // that state complete would be a lie `should_process` below
            // immediately contradicts (it is `false` for an errored header),
            // so the caller never calls `take_completed` for it -- and
            // `drain_unfinished` at stream end skips anything already marked
            // complete. The buffered bits would then reach no counter at all
            // while `partial_completed` reported a success that never
            // happened.
            if header.b_partial_final && !header.has_partial_error {
                state.is_complete = true;
                header.is_partial_completed = true;
                *stats_partial_completed += 1;
            }
        }

        // The error-final case above: discard rather than leave it to leak,
        // and fold its bits into what this call reports lost.
        let error_final_bits = if header.b_partial_final && header.has_partial_error {
            let removed = self.take(ch_index);
            let bits = removed.as_ref().map_or(0, |p| p.bit_count);
            displaced.extend(removed.map(|p| {
                (
                    p,
                    PartialDiscardCause::Sequence(
                        header
                            .partial_error_kind
                            .unwrap_or(PartialSequenceKind::OverlappingInitial),
                    ),
                )
            }));
            bits
        } else {
            0
        };

        let error_kind = header.partial_error_kind;
        PartialBunchResult {
            should_process: header.b_partial_final && !header.has_partial_error,
            header,
            resource_limit: None,
            discarded_bits: sequence_discarded_bits.saturating_add(error_final_bits),
            error_kind,
            overlapping_initial,
            displaced,
        }
    }

    /// Take the completed payload for a channel, if available.
    ///
    /// Returns `(buffer, bit_count, stored_header)`.
    pub fn take_completed(&mut self, ch_index: u32) -> Option<(Vec<u8>, usize, RawBunchHeader)> {
        if let Some(state) = self.fragments.get(&ch_index) {
            if state.is_complete {
                let state = self.fragments.remove(&ch_index).unwrap();
                self.total_buffered_bits = self.total_buffered_bits.saturating_sub(state.bit_count);
                return Some((state.buffer, state.bit_count, state.stored_header));
            }
        }
        None
    }

    /// Drop every partial bunch still awaiting fragments and return them.
    ///
    /// Called once at the end of a replay. Until the stream stops there is
    /// nothing to distinguish an abandoned reassembly from one still in
    /// progress, so this state cannot be judged any earlier -- which is exactly
    /// why it used to go out with the accumulator unremarked: `partial_errors`
    /// stayed zero because no sequence rule was broken, and `partial_fragments`
    /// had already counted the fragments as received.
    ///
    /// A bunch already marked complete is not counted: it was handed to the
    /// caller by [`Self::take_completed`] only if the caller asked, and a
    /// complete-but-untaken entry is the caller's choice, not a loss here.
    pub fn drain_unfinished(&mut self) -> Vec<PreservedPartial> {
        let mut preserved = Vec::with_capacity(self.fragments.len());
        for (_, state) in self.fragments.drain() {
            if state.is_complete {
                continue;
            }
            preserved.push(PreservedPartial {
                header: state.stored_header,
                buffer: state.buffer,
                bit_count: state.bit_count,
            });
        }
        self.total_buffered_bits = 0;
        preserved
    }

    /// Retire any incomplete assembly for a channel that was destroyed.
    /// Returns the number of buffered bits that could not complete.
    pub fn retire_channel(&mut self, ch_index: u32) -> Option<PreservedPartial> {
        self.take(ch_index)
    }

    fn validate_sequence(
        &mut self,
        ch_index: u32,
        header: &mut RawBunchHeader,
        stats_partial_errors: &mut u64,
    ) -> (bool, Vec<(PreservedPartial, PartialDiscardCause)>) {
        if header.b_partial_initial {
            if let Some(existing) = self.fragments.get(&ch_index) {
                if !existing.is_complete {
                    *stats_partial_errors += 1;
                    header.has_partial_error = true;
                    header.partial_error_kind = Some(PartialSequenceKind::OverlappingInitial);
                }
            }
            let displaced = self.take(ch_index);
            self.fragments.insert(
                ch_index,
                AccumulatorState {
                    ch_sequence: header.ch_sequence,
                    reliable: header.b_reliable,
                    is_complete: false,
                    stored_header: header.clone(),
                    buffer: Vec::new(),
                    bit_count: 0,
                },
            );
            return (
                true,
                displaced
                    .map(|p| {
                        (
                            p,
                            PartialDiscardCause::Sequence(PartialSequenceKind::OverlappingInitial),
                        )
                    })
                    .into_iter()
                    .collect(),
            );
        }

        // Continuation
        let (has_state, is_complete, prev_seq, prev_reliable) = match self.fragments.get(&ch_index)
        {
            Some(s) => (true, s.is_complete, s.ch_sequence, s.reliable),
            None => (false, false, 0, false),
        };

        if !has_state || is_complete {
            *stats_partial_errors += 1;
            header.has_partial_error = true;
            header.partial_error_kind = Some(PartialSequenceKind::MissingInitial);
            return (
                false,
                self.take(ch_index)
                    .map(|p| {
                        (
                            p,
                            PartialDiscardCause::Sequence(PartialSequenceKind::MissingInitial),
                        )
                    })
                    .into_iter()
                    .collect(),
            );
        }

        if prev_reliable != header.b_reliable {
            *stats_partial_errors += 1;
            header.has_partial_error = true;
            header.partial_error_kind = Some(PartialSequenceKind::MismatchedContinuation);
            return (
                false,
                self.take(ch_index)
                    .map(|p| {
                        (
                            p,
                            PartialDiscardCause::Sequence(
                                PartialSequenceKind::MismatchedContinuation,
                            ),
                        )
                    })
                    .into_iter()
                    .collect(),
            );
        }

        let seq_ok = if prev_reliable {
            header.ch_sequence == prev_seq + 1
        } else {
            header.ch_sequence == prev_seq + 1 || header.ch_sequence == prev_seq
        };

        if !seq_ok {
            *stats_partial_errors += 1;
            header.has_partial_error = true;
            header.partial_error_kind = Some(PartialSequenceKind::MismatchedContinuation);
            return (
                false,
                self.take(ch_index)
                    .map(|p| {
                        (
                            p,
                            PartialDiscardCause::Sequence(
                                PartialSequenceKind::MismatchedContinuation,
                            ),
                        )
                    })
                    .into_iter()
                    .collect(),
            );
        }

        if let Some(state) = self.fragments.get_mut(&ch_index) {
            state.ch_sequence = header.ch_sequence;
        }
        (true, Vec::new())
    }

    fn take(&mut self, ch_index: u32) -> Option<PreservedPartial> {
        let state = self.fragments.remove(&ch_index)?;
        let bits = state.bit_count;
        self.total_buffered_bits = self.total_buffered_bits.saturating_sub(bits);
        Some(PreservedPartial {
            header: state.stored_header,
            buffer: state.buffer,
            bit_count: state.bit_count,
        })
    }
}

impl Default for PartialBunchAccumulator {
    fn default() -> Self {
        Self::new()
    }
}

/// Append `src_bit_count` bits from `src` at bit offset `dst_bit_offset` in `dst`.
fn append_bits(dst: &mut Vec<u8>, dst_bit_offset: usize, src: &[u8], src_bit_count: usize) -> bool {
    let Some(new_total) = dst_bit_offset.checked_add(src_bit_count) else {
        return false;
    };
    let new_byte_count = new_total.div_ceil(8);
    if new_byte_count > dst.len()
        && dst
            .try_reserve_exact(new_byte_count.saturating_sub(dst.len()))
            .is_err()
    {
        return false;
    }
    dst.resize(new_byte_count, 0);

    for i in 0..src_bit_count {
        let src_bit = (src[i >> 3] >> (i & 7)) & 1;
        let dest_bit = dst_bit_offset + i;
        if src_bit != 0 {
            dst[dest_bit >> 3] |= 1 << (dest_bit & 7);
        }
        // dst is already zeroed from resize, so no need to clear bits.
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_bits_byte_aligned() {
        let mut dst = vec![0xAA];
        assert!(append_bits(&mut dst, 8, &[0x55], 8));
        assert_eq!(dst, vec![0xAA, 0x55]);
    }

    #[test]
    fn append_bits_unaligned() {
        let mut dst = vec![0x0F]; // bits 0..3 = 1, bits 4..7 = 0
        assert!(append_bits(&mut dst, 4, &[0x03], 4)); // add 4 bits: 1100 -> 0x03 reversed
        // dst should be: low nibble 0x0F, high nibble 0x30 = 0x3F
        assert_eq!(dst[0], 0x3F);
    }

    /// The three counters `add_fragment` moves, threaded through every call.
    #[derive(Default)]
    struct Counters {
        errs: u64,
        frags: u64,
        comps: u64,
    }

    impl Counters {
        /// Add a fragment on its header's own channel.
        fn add(
            &mut self,
            acc: &mut PartialBunchAccumulator,
            header: RawBunchHeader,
            data: &[u8],
            bits: usize,
        ) -> PartialBunchResult {
            let ch_index = header.ch_index;
            acc.add_fragment(
                ch_index,
                header,
                data,
                bits,
                &mut self.errs,
                &mut self.frags,
                &mut self.comps,
            )
        }
    }

    /// An initial fragment on `ch_index`.
    fn initial(ch_index: u32, ch_sequence: i32, b_reliable: bool) -> RawBunchHeader {
        RawBunchHeader {
            ch_index,
            b_partial: true,
            b_partial_initial: true,
            b_reliable,
            ch_sequence,
            ..Default::default()
        }
    }

    /// An unreliable continuation on `ch_index`, final when `last`.
    fn continuation(ch_index: u32, ch_sequence: i32, last: bool) -> RawBunchHeader {
        RawBunchHeader {
            ch_index,
            b_partial: true,
            b_partial_final: last,
            ch_sequence,
            ..Default::default()
        }
    }

    /// The reliable final fragment that follows `initial(ch_index, 1, true)`.
    fn reliable_final(ch_index: u32) -> RawBunchHeader {
        RawBunchHeader {
            b_reliable: true,
            ..continuation(ch_index, 2, true)
        }
    }

    #[test]
    fn accumulator_initial_plus_final() {
        let mut acc = PartialBunchAccumulator::new();
        let mut c = Counters::default();

        let r1 = c.add(&mut acc, initial(1, 1, true), &[0xAB], 8);
        assert!(!r1.should_process);

        let r2 = c.add(&mut acc, reliable_final(1), &[0xCD], 8);
        assert!(r2.should_process);
        assert_eq!(c.errs, 0);

        let (buf, bits, _hdr) = acc.take_completed(1).unwrap();
        assert_eq!(bits, 16);
        assert_eq!(buf[0], 0xAB);
        assert_eq!(buf[1], 0xCD);
        assert_eq!(acc.total_buffered_bits(), 0);
    }

    /// A zero-payload final fragment still took two fragments to complete the
    /// bunch, and `partial_fragments` must say so -- not just
    /// `partial_completed`. Before the fix, the zero-payload path never
    /// touched `partial_fragments` at all, so this reported `fragments: 1,
    /// completed: 1` for a bunch that arrived in two pieces.
    #[test]
    fn a_zero_payload_final_fragment_still_counts_as_a_fragment() {
        let mut acc = PartialBunchAccumulator::new();
        let mut c = Counters::default();

        c.add(&mut acc, initial(1, 1, true), &[0xAB], 8);
        assert_eq!(c.frags, 1);

        let r2 = c.add(&mut acc, reliable_final(1), &[], 0);
        assert!(r2.should_process);
        assert_eq!(c.errs, 0);
        assert_eq!(c.comps, 1);
        assert_eq!(
            c.frags, 2,
            "two fragments arrived to complete this bunch, not one"
        );
    }

    /// A final fragment that arrives already carrying an error (here: it
    /// re-declares `b_partial_initial` over an incomplete in-flight
    /// reassembly, which `validate_sequence` flags but still lets through
    /// with a freshly-inserted state) must not be reported as a completion.
    /// Before the fix, `partial_completed` moved for it anyway, and the
    /// buffered bits reached neither `take_completed` (`should_process` is
    /// false) nor `drain_unfinished` (which skips anything already marked
    /// complete) -- a leak with no counter.
    #[test]
    fn an_error_flagged_final_fragment_is_not_reported_as_a_completion() {
        let mut acc = PartialBunchAccumulator::new();
        let mut c = Counters::default();

        c.add(&mut acc, initial(1, 1, true), &[0xAA], 8);
        assert_eq!(c.frags, 1);

        // Overlapping initial: also final, on the same header.
        let h2 = RawBunchHeader {
            b_partial_final: true,
            ..initial(1, 2, true)
        };
        let r2 = c.add(&mut acc, h2, &[0xBB], 8);

        assert_eq!(c.errs, 1, "the overlapping initial must be flagged");
        assert!(!r2.should_process);
        assert_eq!(
            c.comps, 0,
            "an errored final must not be counted as a completion"
        );
        assert_eq!(c.frags, 2, "the second fragment still arrived");
        assert_eq!(
            r2.discarded_bits, 16,
            "the discarded initial (8 bits) plus the errored final's own \
             buffered bits (8 bits), not left to leak uncounted"
        );
        assert!(
            acc.take_completed(1).is_none(),
            "nothing was left behind to hand to a caller"
        );
        assert!(
            acc.drain_unfinished().is_empty(),
            "nothing was left behind for drain_unfinished to skip, either"
        );
    }

    #[test]
    fn partial_reassembly_refuses_more_active_states_than_its_budget() {
        let mut acc = PartialBunchAccumulator::with_limits(1, 64);
        let mut c = Counters::default();
        let first = c.add(&mut acc, initial(1, 0, false), &[0xAA], 8);
        assert_eq!(first.resource_limit, None);
        let refused = c.add(&mut acc, initial(2, 0, false), &[0xBB], 8);
        assert_eq!(
            refused.resource_limit,
            Some(PartialResourceLimit::ActiveStates)
        );
        assert_eq!(refused.discarded_bits, 8);
        assert_eq!(acc.active_count(), 1);
        assert_eq!(acc.total_buffered_bits(), 8);
    }

    #[test]
    fn partial_reassembly_checks_the_total_before_growing_its_buffer() {
        let mut acc = PartialBunchAccumulator::with_limits(2, 12);
        let mut c = Counters::default();
        c.add(&mut acc, initial(1, 0, false), &[0xAA], 8);
        let refused = c.add(&mut acc, continuation(1, 0, true), &[0x1F], 5);
        assert_eq!(
            refused.resource_limit,
            Some(PartialResourceLimit::BufferedBits)
        );
        assert_eq!(refused.discarded_bits, 13, "8 buffered + 5 current");
        assert_eq!(acc.active_count(), 0, "oversized partial is discarded");
        assert_eq!(acc.total_buffered_bits(), 0);
    }

    #[test]
    fn rejected_partial_fragments_report_every_discarded_bit() {
        let mut acc = PartialBunchAccumulator::new();
        let mut c = Counters::default();
        c.add(&mut acc, initial(1, 1, true), &[0xAA], 8);

        // Unreliable after a reliable initial.
        let rejected = c.add(&mut acc, continuation(1, 2, true), &[0x1F], 5);
        assert_eq!(
            rejected.error_kind,
            Some(PartialSequenceKind::MismatchedContinuation)
        );
        assert_eq!(rejected.discarded_bits, 13, "8 buffered + 5 current");
        assert_eq!(acc.total_buffered_bits(), 0);

        let rejected = c.add(&mut acc, continuation(2, 0, true), &[0x7F], 7);
        assert_eq!(
            rejected.error_kind,
            Some(PartialSequenceKind::MissingInitial)
        );
        assert_eq!(rejected.discarded_bits, 7, "the refused current fragment");
    }

    #[test]
    fn overlapping_initial_reports_the_replaced_payload_but_keeps_the_new_one() {
        let mut acc = PartialBunchAccumulator::new();
        let mut c = Counters::default();
        c.add(&mut acc, initial(1, 1, true), &[0xAA], 8);
        let replacement = c.add(&mut acc, initial(1, 2, true), &[0xBB, 0xCC], 16);
        assert_eq!(replacement.discarded_bits, 8);
        assert_eq!(
            replacement.error_kind,
            Some(PartialSequenceKind::OverlappingInitial)
        );
        assert!(replacement.overlapping_initial);
        assert_eq!(replacement.displaced.len(), 1);
        assert_eq!(
            replacement.displaced[0].1,
            PartialDiscardCause::Sequence(PartialSequenceKind::OverlappingInitial)
        );
        assert_eq!(acc.total_buffered_bits(), 16);
        assert_eq!(acc.active_count(), 1);
    }

    #[test]
    fn non_aligned_nonfinal_reports_buffered_and_current_bits() {
        let mut acc = PartialBunchAccumulator::new();
        let mut c = Counters::default();
        c.add(&mut acc, initial(1, 0, false), &[0xAA], 8);
        let rejected = c.add(&mut acc, continuation(1, 0, false), &[0x07], 3);
        assert_eq!(rejected.discarded_bits, 11);
        assert_eq!(
            rejected.error_kind,
            Some(PartialSequenceKind::NonByteAlignedFragment)
        );
        assert_eq!(acc.total_buffered_bits(), 0);
        assert_eq!(rejected.displaced.len(), 1);
        assert_eq!(
            rejected.displaced[0].1,
            PartialDiscardCause::Sequence(PartialSequenceKind::NonByteAlignedFragment)
        );
    }

    /// A refused initial displaces nothing of its own. `validate_sequence`
    /// starts an empty assembly for every initial before the alignment and
    /// budget checks run, so a refusal retires that assembly -- it held no bits
    /// and belongs to the current fragment, which the caller reports itself --
    /// rather than handing it back as a displaced payload.
    #[test]
    fn a_refused_initial_displaces_nothing_of_its_own() {
        let displaced_bits = |result: &PartialBunchResult| {
            result
                .displaced
                .iter()
                .map(|(p, _)| p.bit_count)
                .collect::<Vec<_>>()
        };
        let mut c = Counters::default();

        let mut acc = PartialBunchAccumulator::new();
        let unaligned = c.add(&mut acc, initial(1, 0, false), &[0x1F], 5);
        assert_eq!(
            unaligned.error_kind,
            Some(PartialSequenceKind::NonByteAlignedFragment)
        );
        assert_eq!(displaced_bits(&unaligned), Vec::<usize>::new());
        assert_eq!(unaligned.discarded_bits, 5);
        assert_eq!((acc.active_count(), acc.total_buffered_bits()), (0, 0));

        let mut acc = PartialBunchAccumulator::with_limits(2, 4);
        let over_budget = c.add(&mut acc, initial(1, 0, false), &[0xAA], 8);
        assert_eq!(
            over_budget.resource_limit,
            Some(PartialResourceLimit::BufferedBits)
        );
        assert_eq!(displaced_bits(&over_budget), Vec::<usize>::new());
        assert_eq!(over_budget.discarded_bits, 8);
        assert_eq!((acc.active_count(), acc.total_buffered_bits()), (0, 0));
        assert_eq!(c.errs, 2);
    }

    #[test]
    fn overlapping_empty_initial_retains_its_cause() {
        let mut acc = PartialBunchAccumulator::new();
        let mut c = Counters::default();
        c.add(&mut acc, initial(1, 0, false), &[0xAA], 8);
        let result = c.add(&mut acc, initial(1, 0, false), &[], 0);
        assert!(result.overlapping_initial);
        assert_eq!(c.errs, 1);
    }

    #[test]
    fn overlapping_unaligned_initial_reports_both_errors() {
        let mut acc = PartialBunchAccumulator::new();
        let mut c = Counters::default();
        c.add(&mut acc, initial(1, 0, false), &[0xAA], 8);
        let result = c.add(&mut acc, initial(1, 0, false), &[0x07], 3);
        assert!(result.overlapping_initial);
        assert_eq!(
            result.error_kind,
            Some(PartialSequenceKind::NonByteAlignedFragment)
        );
        // Both causes stand: `overlapping_initial` and `error_kind` above, and
        // two errors counted. The one displaced payload is the 8-bit assembly
        // the initial replaced; the empty one it started is not a payload.
        assert_eq!(result.displaced.len(), 1);
        assert_eq!(
            result.displaced[0].1,
            PartialDiscardCause::Sequence(PartialSequenceKind::OverlappingInitial)
        );
        assert_eq!(result.displaced[0].0.bit_count, 8);
        assert_eq!(result.discarded_bits, 11, "8 replaced + 3 current");
        assert_eq!(c.errs, 2);
    }

    #[test]
    fn overlapping_initial_over_resource_limit_retains_both_causes() {
        let mut acc = PartialBunchAccumulator::with_limits(2, 12);
        let mut c = Counters::default();
        c.add(&mut acc, initial(1, 0, false), &[0xAA], 8);
        let result = c.add(&mut acc, initial(1, 0, false), &[0xBB, 0xCC], 16);
        assert!(result.overlapping_initial);
        assert_eq!(
            result.resource_limit,
            Some(PartialResourceLimit::BufferedBits)
        );
        // Both causes stand: `overlapping_initial` and `resource_limit` above,
        // and two errors counted. The one displaced payload is the 8-bit
        // assembly the initial replaced; the empty one it started is not.
        assert_eq!(result.displaced.len(), 1);
        assert_eq!(
            result.displaced[0].1,
            PartialDiscardCause::Sequence(PartialSequenceKind::OverlappingInitial)
        );
        assert_eq!(result.displaced[0].0.bit_count, 8);
        assert_eq!(result.discarded_bits, 24, "8 replaced + 16 current");
        assert_eq!(c.errs, 2);
    }
}
