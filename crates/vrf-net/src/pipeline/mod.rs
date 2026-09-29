//! Top-level replication reader that drives the full pipeline.
//!
//! One pass connects packets -> bunches -> content blocks -> fields. The
//! caller provides a [`ReplicationSink`] for the decoded events (fields, RPCs,
//! actor lifecycle) and the replay branch that selects the payload transform.
//!
//! Layout: channel (open/close, GUID preambles), spawn (dynamic-actor spawn block), framing (content blocks, fields, RPCs) -- measured rates in docs/PERFORMANCE_NOTES.md#measured-rates-reference-replay-02d4d478.
//!
//! This file is deliberately thin -- the sink trait, the types it exchanges
//! and the packet-level driver -- so each stage module can be read against the
//! wire format it implements.
//!
//! The steady state allocates nothing per packet, bunch or content block
//! (channel-table growth: docs/PERFORMANCE_NOTES.md#allocation-strategy). The
//! reader owns and reuses `scratch` (one decoded content-block payload),
//! `fragment_stage` (one partial-bunch fragment, byte-aligned) and the channel
//! table (a row per channel index in use, never per bunch). Bunch payloads are
//! views into the caller's packet bytes: framing gets a sub-reader, and content
//! blocks and fields are sub-readers of that.

mod channel;
mod framing;
mod spawn;

use vrf_bitio::BitReader;
use vrf_transform::TransformVersion;

use crate::bunch::{
    PartialBunchAccumulator, PartialDiscardCause, PartialResourceLimit, PreservedPartial,
    RawBunchHeader,
};
use crate::content::ContentBlockHeader;
use crate::error::{PartialSequenceKind, Result};
use crate::field::FieldSink;
use crate::net_guid::GuidPathSink;
use crate::packet::RawPacketReader;
use crate::stats::NetStats;
use crate::types::MAX_ACTIVE_CHANNELS;
use crate::types::NetworkGuid;

use std::collections::HashMap;

use framing::BunchContext;

/// Per-channel actor state tracked during replication.
#[derive(Debug, Clone, Default)]
pub struct ActorChannelState {
    pub channel_index: u32,
    pub is_open: bool,
    /// Whether the channel is dormant (closed, actor alive). Public so a
    /// consumer rebuilding actor lifetimes needs no bunch header: dormancy is
    /// not destruction, and only a non-dormant close is a despawn.
    pub is_dormant: bool,
    pub actor_net_guid: NetworkGuid,
    /// Archetype GUID (for dynamic actors).
    pub archetype_net_guid: NetworkGuid,
    pub level_guid: NetworkGuid,
    /// Spawn location (if dynamic and present).
    pub spawn_location: Option<crate::types::FVector>,
    /// Spawn rotation (if present).
    pub spawn_rotation: Option<crate::types::FRotator>,
    /// Spawn scale (if dynamic and present).
    pub spawn_scale: Option<crate::types::FVector>,
    /// Spawn velocity (if dynamic and present).
    pub spawn_velocity: Option<crate::types::FVector>,
    /// Packet that opened this channel.
    pub open_packet_id: i32,
}

/// Which stream grammar failed to parse inside a decoded content block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StreamKind {
    /// A property (RepLayout) stream.
    RepLayout,
    /// An RPC (ClassNetCache) stream.
    Rpc,
}

/// Where inside the walk a stream failure happened.
///
/// Purely diagnostic (no decode or verdict path reads it): it lets a per-group
/// aggregate separate shapes that share the stream-failure counters, above all
/// an unresolved group (payload preserved whole) from a stream that lost
/// structure, which `NetStats::lost_content_blocks` separates only in total.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StreamFailureCause {
    /// The walk returned `Ok` but abandoned bits mid-block (a record overran,
    /// or an RPC record was too short for its length); records before the
    /// break are good.
    AbandonedTail,
    /// The walk returned `Err` -- a read inside the block ran off the end (or
    /// otherwise failed) and the rest of the block is unframeable.
    ReadError,
    /// The ClassNetCache function count was 0 (an unresolved group, handle
    /// width unknown). The whole decoded payload went to
    /// `on_unresolved_class_net_cache_payload`: preserved, not lost.
    UnresolvedFunctionCount,
    /// A valid post-RepLayout window retained whole because its provenance or
    /// strict ClassNetCache shape was not verified: uncertainty, not a failed
    /// read or a missing count.
    UnverifiedRepLayoutTail,
    /// Never constructed: the decoded window always opens, its scratch holding
    /// `ceil(bit_count / 8)` bytes.
    WindowOpenFailed,
}

/// Result of handing a valid post-RepLayout tail to the sink.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepLayoutTailOutcome {
    /// The tail was strictly decoded as this many ClassNetCache RPCs.
    Decoded { rpc_count: u32 },
    /// The tail could not be decoded, but its complete raw bits were retained.
    Preserved { cause: StreamFailureCause },
    /// The tail could neither be decoded nor retained by this sink.
    Unpreserved { cause: StreamFailureCause },
}

/// Context for a content block that framed and decoded but whose inner stream
/// could not be walked. Reported to the sink because it holds the names (group
/// path, function count) a new build's investigation needs; a counter only
/// says that one block failed.
#[derive(Debug, Clone, Copy)]
pub struct StreamFailure {
    /// Which grammar was being parsed.
    pub kind: StreamKind,
    /// Actor whose channel carried the block.
    pub actor_net_guid: NetworkGuid,
    /// Declared payload length of the block.
    pub bit_count: u32,
    /// Function count used for the handle read (`Rpc` only; 0 for `RepLayout`,
    /// and the unresolved-group sentinel). A wrong non-zero count selects the
    /// wrong handle width.
    pub function_count: u32,
    /// Bits consumed before the failure.
    pub consumed_bits: u64,
    /// Bits abandoned as a result.
    pub remaining_bits: u64,
    /// Which stage of the walk failed. See [`StreamFailureCause`].
    pub cause: StreamFailureCause,
    /// Handle of the non-terminator record the walk was inside, once its
    /// handle read succeeded; `None` for a failed handle read or an early zero
    /// terminator, so the previous field is never blamed.
    pub record_handle: Option<u32>,
    /// Bit offset inside the block where the failing record begins, when the
    /// parser tracked one.
    pub record_offset: Option<u64>,
    /// Whether the complete failed stream reached a raw preservation row.
    pub payload_preserved: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartialPayloadReason {
    MissingInitial,
    OverlappingInitial,
    MismatchedContinuation,
    NonByteAlignedFragment,
    ActiveStateLimit,
    BufferedBitsLimit,
    AllocationFailure,
    ChannelStateLimit,
    ChannelClosed,
    EndOfStream,
}

pub struct RejectedPartialFragment<'a> {
    pub header: &'a RawBunchHeader,
    pub payload_kind: &'static str,
    pub reason: PartialPayloadReason,
    pub bit_count: usize,
    pub payload: &'a [u8],
    pub rejection_packet_id: Option<i32>,
}

impl<'a> RejectedPartialFragment<'a> {
    /// An assembly the accumulator gave up, with the fragments it had buffered.
    fn accumulated(
        partial: &'a PreservedPartial,
        reason: PartialPayloadReason,
        rejection_packet_id: Option<i32>,
    ) -> Self {
        Self {
            header: &partial.header,
            payload_kind: "accumulated_payload",
            reason,
            bit_count: partial.bit_count,
            payload: &partial.buffer,
            rejection_packet_id,
        }
    }

    /// The fragment being handled, refused before it was buffered.
    fn current(
        header: &'a RawBunchHeader,
        reason: PartialPayloadReason,
        bit_count: usize,
        payload: &'a [u8],
    ) -> Self {
        Self {
            header,
            payload_kind: "current_fragment",
            reason,
            bit_count,
            payload,
            rejection_packet_id: Some(header.packet_id),
        }
    }
}

/// Trait for receiving all replication events: fields, RPCs and actor
/// lifecycle, with nothing discarded silently.
pub trait ReplicationSink: GuidPathSink + FieldSink {
    fn on_rejected_partial(&mut self, _partial: RejectedPartialFragment<'_>) {}
    /// Whether the sink wants per-record failure positions and decoded-payload
    /// callbacks. The default keeps the normal pipeline on its original walk.
    fn wants_stream_failure_details(&self) -> bool {
        false
    }

    /// Handle a ClassNetCache stream that follows a valid RepLayout zero
    /// terminator in the same content block. The reader is bounded to the tail.
    fn on_rep_layout_tail(
        &mut self,
        _actor_net_guid: NetworkGuid,
        _bit_count: u32,
        _reader: BitReader<'_>,
    ) -> RepLayoutTailOutcome {
        RepLayoutTailOutcome::Unpreserved {
            cause: StreamFailureCause::UnverifiedRepLayoutTail,
        }
    }

    /// Optional raw diagnostic sample for a chained tail that was not decoded,
    /// after its [`Self::on_stream_failure`].
    fn on_rep_layout_tail_failure_payload(
        &mut self,
        _failure: StreamFailure,
        _reader: BitReader<'_>,
    ) {
    }

    /// An actor channel was opened (new actor spawned or re-opened).
    fn on_actor_open(&mut self, state: &ActorChannelState);

    /// An actor channel was closed.
    fn on_actor_close(&mut self, channel_index: u32, actor_net_guid: NetworkGuid, dormant: bool);

    /// A content block header was parsed (before payload).
    /// Returns the function_count for ClassNetCache blocks (0 if unknown).
    fn on_content_block(
        &mut self,
        channel_index: u32,
        actor_net_guid: NetworkGuid,
        header: &ContentBlockHeader,
    ) -> u32;

    /// A content block was flagged as deleted.
    fn on_deleted_block(
        &mut self,
        channel_index: u32,
        actor_net_guid: NetworkGuid,
        header: &ContentBlockHeader,
    );

    /// A block framed and decoded, but its inner stream could not be walked.
    /// Default no-op (the [`NetStats`] counters move either way); override to
    /// attach names the sink holds, the resolved group path in particular.
    /// The three payload callbacks for the same failure always come after it.
    fn on_stream_failure(&mut self, _failure: StreamFailure) {}

    /// The decoded payload of a block whose inner stream could not be walked,
    /// after [`Self::on_stream_failure`], only when
    /// [`Self::wants_stream_failure_details`], and not for unresolved
    /// ClassNetCache blocks ([`Self::on_unresolved_class_net_cache_payload`]).
    /// Diagnostics only; default no-op.
    fn on_stream_failure_payload(&mut self, _failure: StreamFailure, _payload: &[u8]) {}

    /// Preserve one whole decoded ClassNetCache payload whose function table
    /// could not be resolved: a block-level data event, not a fabricated RPC,
    /// after its [`Self::on_stream_failure`]. `payload` is exactly `ceil(failure.bit_count / 8)` bytes, the final
    /// byte's unused high bits cleared; the parser consumed none of it.
    fn on_unresolved_class_net_cache_payload(&mut self, _failure: StreamFailure, _payload: &[u8]) {}
}

/// Leaf asset name of the VALORANT replay controller, the only
/// PlayerController-kind actor in these replays; the net-player-index byte is
/// keyed off it (`channel::is_player_controller_path`
/// normalises its four spellings).
pub const PLAYER_CONTROLLER_LEAF: &str = "BaseReplayController";

/// One channel's row. A channel is counted from its first bunch, which may be
/// a close or a bunch for a channel that never opened, while only an open
/// produces an [`ActorChannelState`]: hence the `Option`.
#[derive(Default)]
struct ChannelSlot {
    /// How many bunches this channel has carried, 1-based. Reported in
    /// diagnostics as `channel_bunch_index`.
    bunch_count: u64,
    /// `None` until the channel opens, and again once an open bunch that did
    /// not complete its open has retired the live actor it held
    /// (`retire_after_failed_open`).
    state: Option<ActorChannelState>,
}

/// Channel index -> channel row, looked up once or twice per bunch.
type ChannelTable = HashMap<u32, ChannelSlot>;

/// Bytes the scratch buffer starts at: above any block one bunch can carry (a
/// bunch payload is capped at `MAX_PACKET_SIZE_BITS`, 2,048 bytes). A block
/// inside a reassembled partial bunch can be larger; the decode path's
/// `resize` covers those, so it is a safety net, not the common path.
const SCRATCH_INITIAL_BYTES: usize = 4096;

/// The mutable state one bunch's processing needs: a borrow split of
/// [`ReplicationReader`], made once per packet so the bunch callback can drive
/// the pipeline inline.
struct Stage<'a> {
    stats: &'a mut NetStats,
    channels: &'a mut ChannelTable,
    transform: TransformVersion,
    scratch: &'a mut Vec<u8>,
}

/// Where a bunch sits in the stream. Every field feeds a `DiagnosticEvent`, so
/// without that feature they are never read; they are threaded through anyway
/// so the hot path reads the same in both builds (the optimiser drops them).
#[cfg_attr(not(feature = "diagnostics"), allow(dead_code))]
#[derive(Clone, Copy)]
struct BunchIds {
    bunch_index_in_packet: u32,
    global_bunch_index: u64,
    channel_bunch_index: u64,
}

/// The main replication reader. Drives the full pipeline for one replay.
pub struct ReplicationReader {
    packet_reader: RawPacketReader,
    accumulator: PartialBunchAccumulator,
    channels: ChannelTable,
    transform: TransformVersion,
    stats: NetStats,
    /// Reusable scratch buffer for payload transforms.
    scratch: Vec<u8>,
    /// Reusable byte-aligned staging buffer for partial-bunch fragments; see
    /// `stage_fragment`.
    fragment_stage: Vec<u8>,
    /// Global bunch counter (0-based, monotonically increasing).
    global_bunch_index: u64,
}

impl ReplicationReader {
    /// Create a reader for a replay with the given branch.
    pub fn new(branch: &str) -> Result<Self> {
        let transform = TransformVersion::require(branch)?;
        Ok(Self {
            packet_reader: RawPacketReader::new(),
            accumulator: PartialBunchAccumulator::new(),
            channels: ChannelTable::default(),
            transform,
            stats: NetStats::default(),
            scratch: vec![0u8; SCRATCH_INITIAL_BYTES],
            fragment_stage: Vec::new(),
            global_bunch_index: 0,
        })
    }

    /// Access accumulated statistics.
    #[must_use]
    pub fn stats(&self) -> &NetStats {
        &self.stats
    }

    /// Account for reassembly state the replay ended in the middle of, and
    /// preserve it through `sink`.
    ///
    /// Call once after the last packet: until the stream stops, an unfinished
    /// partial cannot be told from one in progress. It lands in
    /// [`NetStats::unfinished_partials`] and [`NetStats::unfinished_partial_bits`],
    /// not `partial_errors` (nothing was out of sequence). Idempotent: the
    /// accumulator is drained, so a second call counts nothing.
    pub fn finish_with_sink(&mut self, sink: &mut dyn ReplicationSink) {
        self.finish_partials(Some(sink));
    }

    /// Account for unfinished partials without a preservation consumer.
    pub fn finish(&mut self) {
        self.finish_partials(None);
    }

    /// Count every assembly still in flight and, given a sink, preserve it
    /// there as [`PartialPayloadReason::EndOfStream`].
    fn finish_partials(&mut self, mut sink: Option<&mut dyn ReplicationSink>) {
        for partial in self.accumulator.drain_unfinished() {
            self.stats.unfinished_partials += 1;
            self.stats.unfinished_partial_bits += partial.bit_count as u64;
            if let Some(sink) = sink.as_deref_mut() {
                let reason = PartialPayloadReason::EndOfStream;
                sink.on_rejected_partial(RejectedPartialFragment::accumulated(
                    &partial, reason, None,
                ));
            }
        }
    }

    /// Process one raw packet (byte slice as received from the demo frame).
    pub fn process_packet(
        &mut self,
        packet_data: &[u8],
        packet_id: i32,
        sink: &mut dyn ReplicationSink,
    ) {
        self.stats.packets += 1;

        // Why inline beat two phases, and what the old copies cost on the reference replay: docs/PERFORMANCE_NOTES.md#packet-processing-is-interleaved.
        //
        // Bunches are processed inline, inside the packet reader's callback.
        // Destructuring `self` hands the callback `&mut` to the fields below
        // while `read_packet` borrows `packet_reader`; the borrows are
        // disjoint, and so is the state: header parsing mutates only
        // `packet_reader` (partial tracking, reliable sequence), payload
        // processing only the rest.
        let Self {
            packet_reader,
            accumulator,
            channels,
            transform,
            stats,
            scratch,
            fragment_stage,
            global_bunch_index,
        } = self;

        let mut stage = Stage {
            stats,
            channels,
            transform: *transform,
            scratch,
        };
        let mut bunch_index_in_packet: u32 = 0;

        let result = packet_reader.read_packet(packet_data, packet_id, |header, payload| {
            stage.stats.bunches += 1;

            // The global and in-packet indexes are the values *before* the
            // increment, the per-channel one the value *after*.
            let (global_index, index_in_packet) = (*global_bunch_index, bunch_index_in_packet);
            *global_bunch_index += 1;
            bunch_index_in_packet += 1;
            if header.has_channel_limit_error
                || (!stage.channels.contains_key(&header.ch_index)
                    && stage.channels.len() >= MAX_ACTIVE_CHANNELS)
            {
                stage.stats.channel_state_limit_failures += 1;
                if header.b_partial {
                    // Attempted, then refused: counted here because this
                    // return skips `process_bunch`, which counts the rest.
                    stage.stats.partial_bunches += 1;
                    let bit_count = payload.bits_remaining() as usize;
                    let staged = stage_fragment(payload.clone(), fragment_stage);
                    let reason = PartialPayloadReason::ChannelStateLimit;
                    let row = RejectedPartialFragment::current(header, reason, bit_count, staged);
                    sink.on_rejected_partial(row);
                }
                // Retired before the close, so an open+close bunch closes
                // nothing it did not open.
                Self::abandon_bunch(header, &mut payload.clone(), &mut stage);
                if header.b_close {
                    Self::close_channel(header, &mut stage, accumulator, sink);
                }
                return;
            }
            let slot = stage.channels.entry(header.ch_index).or_default();
            slot.bunch_count += 1;
            let ids = BunchIds {
                bunch_index_in_packet: index_in_packet,
                global_bunch_index: global_index,
                channel_bunch_index: slot.bunch_count,
            };

            Self::process_bunch(
                header,
                payload,
                &mut stage,
                accumulator,
                fragment_stage,
                sink,
                ids,
            );
        });

        if result.is_malformed {
            stage.stats.malformed_packets += 1;
        }
        // `result.partial_error_count` is advisory; see `RawPacketReader`.
    }

    /// Route one bunch: reassemble it if partial, otherwise frame it directly.
    fn process_bunch(
        header: &mut RawBunchHeader,
        payload: BitReader<'_>,
        stage: &mut Stage<'_>,
        accumulator: &mut PartialBunchAccumulator,
        fragment_stage: &mut Vec<u8>,
        sink: &mut dyn ReplicationSink,
        ids: BunchIds,
    ) {
        let ch_index = header.ch_index;
        let bit_count = payload.bits_remaining();

        if header.b_partial {
            stage.stats.partial_bunches += 1;
            let staged = stage_fragment(payload, fragment_stage);

            // The accumulator is the one reassembly authority, so it sees none
            // of the packet reader's partial verdicts (see `RawPacketReader`):
            // the tracker can disagree with it, and its flag would veto valid
            // completions with every cause counter at 0.
            let mut fragment_header = header.clone();
            fragment_header.has_partial_error = false;
            fragment_header.partial_error_kind = None;
            fragment_header.is_partial_completed = false;

            let result = accumulator.add_fragment(
                ch_index,
                fragment_header,
                staged,
                bit_count as usize,
                &mut stage.stats.partial_errors,
                &mut stage.stats.partial_fragments,
                &mut stage.stats.partial_completed,
            );
            *header = result.header;

            let reason = result
                .error_kind
                .map(PartialDiscardCause::Sequence)
                .or_else(|| result.resource_limit.map(PartialDiscardCause::Resource))
                .map(partial_payload_reason);
            for (displaced, cause) in &result.displaced {
                let reason = partial_payload_reason(*cause);
                let row =
                    RejectedPartialFragment::accumulated(displaced, reason, Some(header.packet_id));
                sink.on_rejected_partial(row);
            }
            // A current-fragment row only for a fragment the accumulator
            // refused, under the cause it named. An overlapping initial is not
            // refused (it is buffered; the assembly it replaced is preserved
            // above). No fallback cause: a row under a cause no counter
            // recorded would put the same bits in the table twice.
            if let Some(reason) = reason.filter(|reason| {
                !result.should_process && *reason != PartialPayloadReason::OverlappingInitial
            }) {
                let row =
                    RejectedPartialFragment::current(header, reason, bit_count as usize, staged);
                sink.on_rejected_partial(row);
            }

            if result.overlapping_initial {
                stage.stats.partial_overlapping_initial += 1;
            }
            match result.error_kind {
                Some(PartialSequenceKind::MissingInitial) => {
                    stage.stats.partial_missing_initial += 1;
                    stage.stats.partial_missing_initial_bits += bit_count;
                    if header.b_partial_final {
                        stage.stats.partial_missing_initial_final += 1;
                    }
                    if header.b_reliable {
                        stage.stats.partial_missing_initial_reliable += 1;
                    }
                }
                Some(PartialSequenceKind::OverlappingInitial) => {}
                Some(PartialSequenceKind::MismatchedContinuation) => {
                    stage.stats.partial_mismatched_continuation += 1;
                }
                Some(PartialSequenceKind::NonByteAlignedFragment) => {
                    stage.stats.partial_non_byte_aligned += 1;
                }
                None => {}
            }

            stage.stats.skipped_bits += result.discarded_bits as u64;
            if result.resource_limit.is_some() {
                stage.stats.partial_resource_limit_failures += 1;
            }

            if result.should_process {
                if let Some((buf, total_bits, stored_header)) = accumulator.take_completed(ch_index)
                {
                    let Ok(mut payload_reader) = BitReader::with_bit_len(&buf, total_bits as u64)
                    else {
                        // Returns before the close below as well; see
                        // docs/FOLLOWUP.md on this arm.
                        stage.stats.partial_errors += 1;
                        return;
                    };
                    Self::process_complete_payload(
                        &stored_header,
                        &mut payload_reader,
                        stage,
                        sink,
                        ids,
                    );
                }
            }
        } else if bit_count != 0 {
            let mut payload = payload;
            Self::process_complete_payload(header, &mut payload, stage, sink, ids);
        }

        // For a partial bunch the close flag belongs to the FINAL fragment, not
        // the initial one `stored_header` came from: `UChannel::SendBunch`
        // puts `bOpen` on the first fragment and `bClose` on the last, and
        // `ReceivedNextBunch` copies the last one's close flags onto the
        // reassembled bunch.
        if header.b_close {
            Self::close_channel(header, stage, accumulator, sink);
        }
    }

    /// Close a channel and retire its reassembly state. `bClose` always means
    /// both: a destroyed channel's partial left behind could never complete
    /// yet would never be counted lost. Every `b_close` site calls this.
    fn close_channel(
        header: &RawBunchHeader,
        stage: &mut Stage<'_>,
        accumulator: &mut PartialBunchAccumulator,
        sink: &mut dyn ReplicationSink,
    ) {
        channel::handle_channel_close(header, stage.channels, stage.stats, sink);
        Self::retire_destroyed_channel(header, stage, accumulator, sink);
    }

    fn retire_destroyed_channel(
        header: &RawBunchHeader,
        stage: &mut Stage<'_>,
        accumulator: &mut PartialBunchAccumulator,
        sink: &mut dyn ReplicationSink,
    ) {
        if header.b_dormant {
            return;
        }
        stage.channels.remove(&header.ch_index);
        if let Some(discarded) = accumulator.retire_channel(header.ch_index) {
            stage.stats.partial_errors += 1;
            stage.stats.partial_channel_close += 1;
            stage.stats.skipped_bits += discarded.bit_count as u64;
            let reason = PartialPayloadReason::ChannelClosed;
            let row =
                RejectedPartialFragment::accumulated(&discarded, reason, Some(header.packet_id));
            sink.on_rejected_partial(row);
        }
    }

    /// Count a bunch-header failure, abandon the bunch and retire the actor
    /// an open in it displaced.
    ///
    /// A bunch refused at the channel-state limit, and each header stage that
    /// fails (package-map exports, must-be-mapped GUIDs, the channel open),
    /// leave the reader at an indeterminate bit, so the rest cannot be framed.
    /// The charge is the whole window, never `bits_remaining()`, by the rule on
    /// `framing::abort`.
    fn abandon_bunch(header: &RawBunchHeader, payload: &mut BitReader<'_>, stage: &mut Stage<'_>) {
        stage.stats.bunch_header_failures += 1;
        stage.stats.skipped_bits += payload.len_bits();
        payload.skip_remaining();
        Self::retire_after_failed_open(header, stage);
    }

    /// Take the channel away from the actor it held when an open bunch does not
    /// complete its open.
    ///
    /// `handle_channel_open` writes the new state only after the actor GUID and
    /// spawn block read, so each of the five ways an open bunch stops short
    /// leaves the old actor in place: refused at the channel-state limit, a
    /// failed package-map read, a failed must-be-mapped read, a failed open,
    /// or a package-map export bunch whose exports read cleanly (nothing after
    /// exports is read). Each arm calls this. The wire has given the channel
    /// to someone this reader cannot name, so a live actor's state is cleared
    /// rather than framing later bunches under its schema; they go to
    /// [`Self::drop_unopened`], where they are counted.
    ///
    /// Only an open bunch displaces; one without `b_open` returns untouched.
    /// No close is emitted for the displaced actor (the replay sent none, as
    /// for `channel_reopens_while_open`), and a dormant or closed state is not
    /// live and is left alone.
    fn retire_after_failed_open(header: &RawBunchHeader, stage: &mut Stage<'_>) {
        if !header.b_open {
            return;
        }
        let live = stage
            .channels
            .get_mut(&header.ch_index)
            .filter(|slot| slot.state.as_ref().is_some_and(|state| state.is_open));
        if let Some(slot) = live {
            slot.state = None;
            stage.stats.failed_reopens_while_open += 1;
        }
    }

    /// Count and discard a bunch whose channel has no open actor: only the
    /// bits left after its preambles. `bits_remaining()` is right here, unlike
    /// in [`Self::abandon_bunch`]: nothing failed to read, so the reader stands
    /// where the unframed content begins.
    fn drop_unopened(payload: &mut BitReader<'_>, stage: &mut Stage<'_>) {
        let dropped = payload.bits_remaining();
        if dropped == 0 {
            return;
        }
        stage.stats.bunches_on_unopened_channel += 1;
        stage.stats.unopened_channel_bits += dropped;
        payload.skip_remaining();
    }

    /// Walk one whole (reassembled, if it was partial) bunch payload.
    fn process_complete_payload(
        header: &RawBunchHeader,
        payload: &mut BitReader<'_>,
        stage: &mut Stage<'_>,
        sink: &mut dyn ReplicationSink,
        ids: BunchIds,
    ) {
        let ch_index = header.ch_index;

        // A package-map export bunch ends here on both outcomes: nothing after
        // the exports is read or counted (docs/FOLLOWUP.md says why). So an
        // open it carries is never read, and the actor it displaces is retired
        // either way; on the clean path `failed_reopens_while_open` alone says
        // so. An export counts only when its read succeeds.
        if header.b_has_package_map_exports {
            if channel::read_package_map_exports(payload, stage.stats, sink).is_ok() {
                stage.stats.package_map_exports += 1;
                Self::retire_after_failed_open(header, stage);
            } else {
                Self::abandon_bunch(header, payload, stage);
            }
            return;
        }

        // A failed must-be-mapped read abandons any open behind the list with
        // the bunch; a failed open writes no state.
        if (header.b_has_must_be_mapped_guids
            && channel::read_must_be_mapped_guids(payload, stage.stats).is_err())
            || (header.b_open
                && channel::handle_channel_open(header, payload, stage.channels, stage.stats, sink)
                    .is_err())
        {
            return Self::abandon_bunch(header, payload, stage);
        }

        // No open actor on this channel: nothing to frame the rest under.
        let Some(ch) = stage
            .channels
            .get(&ch_index)
            .and_then(|s| s.state.as_ref())
            .filter(|s| s.is_open)
        else {
            return Self::drop_unopened(payload, stage);
        };
        let (actor_net_guid, archetype_net_guid) = (ch.actor_net_guid, ch.archetype_net_guid);

        // ReadNetPlayerIndex. The cheap flags come first: the path check costs
        // two NetGuidCache lookups and two normalisations, and only open
        // bunches need it -- at most the 2,028 opens among 530,401 bunches on
        // 02d4d478 (`validate` at 061155a). What the byte is:
        // [`channel::is_player_controller_channel`].
        if header.b_open
            && actor_net_guid.is_dynamic()
            && !payload.at_end()
            && channel::is_player_controller_channel(actor_net_guid, archetype_net_guid, sink)
        {
            let _ = payload.read_u8();
        }

        let ctx = BunchContext {
            header,
            ids,
            actor_net_guid,
            archetype_net_guid,
        };
        framing::frame_content_blocks(payload, stage, sink, &ctx);
    }
}

/// Copy a partial bunch's payload into `buffer`, byte-aligned, and return the
/// bytes it occupies: [`PartialBunchAccumulator::add_fragment`] concatenates
/// bytes, but a payload is a bit window at any offset. `buffer` only grows and
/// its tail is stale, but `copy_bits_to` rewrites the whole returned prefix,
/// padding bits included.
fn stage_fragment<'b>(mut payload: BitReader<'_>, buffer: &'b mut Vec<u8>) -> &'b [u8] {
    let bit_count = payload.bits_remaining();
    let byte_count = (bit_count as usize).div_ceil(8);
    if buffer.len() < byte_count {
        buffer.resize(byte_count, 0);
    }
    payload
        .copy_bits_to(buffer, bit_count)
        .expect("the buffer holds the whole window");
    &buffer[..byte_count]
}

/// Map an accumulator discard cause to the reason reported to the sink, shared
/// by a rejected fragment and every assembly it displaced so both name a
/// discard alike.
fn partial_payload_reason(cause: PartialDiscardCause) -> PartialPayloadReason {
    use PartialDiscardCause::{Resource, Sequence};
    match cause {
        Sequence(PartialSequenceKind::MissingInitial) => PartialPayloadReason::MissingInitial,
        Sequence(PartialSequenceKind::OverlappingInitial) => {
            PartialPayloadReason::OverlappingInitial
        }
        Sequence(PartialSequenceKind::MismatchedContinuation) => {
            PartialPayloadReason::MismatchedContinuation
        }
        Sequence(PartialSequenceKind::NonByteAlignedFragment) => {
            PartialPayloadReason::NonByteAlignedFragment
        }
        Resource(PartialResourceLimit::ActiveStates) => PartialPayloadReason::ActiveStateLimit,
        Resource(PartialResourceLimit::BufferedBits) => PartialPayloadReason::BufferedBitsLimit,
        Resource(PartialResourceLimit::Allocation) => PartialPayloadReason::AllocationFailure,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_bits::{
        BunchSpec, build_bunch_packet, build_packet, pack, write_bunch, write_byte,
        write_int_packed, write_serialized_int,
    };

    struct OwnedRejectedPartial {
        header: RawBunchHeader,
        kind: &'static str,
        reason: PartialPayloadReason,
        bit_count: usize,
        payload: Vec<u8>,
        rejection_packet_id: Option<i32>,
    }

    #[derive(Default)]
    struct TestSink {
        fields: Vec<(u32, u32)>,
        rpcs: Vec<(u32, u32)>,
        stream_failures: Vec<StreamFailure>,
        unresolved_payloads: Vec<(StreamFailure, Vec<u8>)>,
        /// `stream_failures.len()` at each payload callback: the failure must
        /// already be there.
        failures_before_payload: Vec<usize>,
        opens: Vec<u32>,
        closes: Vec<u32>,
        paths: Vec<(u32, String)>,
        /// GUID-to-path map for `path_for_guid`; empty by default, so every
        /// lookup is `None`.
        guid_paths: std::collections::HashMap<u32, String>,
        /// Content-block headers, in arrival order.
        content_blocks: Vec<ContentBlockHeader>,
        rep_layout_tails: Vec<(u32, Vec<u8>)>,
        rep_layout_tail_outcome: Option<RepLayoutTailOutcome>,
        rejected_partials: Vec<OwnedRejectedPartial>,
    }

    impl GuidPathSink for TestSink {
        fn register_path(&mut self, guid: u32, path: &str, _outer: NetworkGuid) {
            self.paths.push((guid, path.to_owned()));
        }
        fn path_for_guid(&self, guid: u32) -> Option<&str> {
            self.guid_paths.get(&guid).map(|s| s.as_str())
        }
    }

    impl FieldSink for TestSink {
        fn on_field(&mut self, handle: u32, bit_count: u32, _reader: BitReader<'_>) {
            self.fields.push((handle, bit_count));
        }
        fn on_rpc(&mut self, handle: u32, bit_count: u32, _reader: BitReader<'_>) {
            self.rpcs.push((handle, bit_count));
        }
    }

    impl ReplicationSink for TestSink {
        fn on_rejected_partial(&mut self, p: RejectedPartialFragment<'_>) {
            self.rejected_partials.push(OwnedRejectedPartial {
                header: p.header.clone(),
                kind: p.payload_kind,
                reason: p.reason,
                bit_count: p.bit_count,
                payload: p.payload.to_vec(),
                rejection_packet_id: p.rejection_packet_id,
            });
        }
        fn wants_stream_failure_details(&self) -> bool {
            true
        }

        fn on_actor_open(&mut self, state: &ActorChannelState) {
            self.opens.push(state.channel_index);
        }

        fn on_rep_layout_tail(
            &mut self,
            _actor_net_guid: NetworkGuid,
            bit_count: u32,
            mut reader: BitReader<'_>,
        ) -> RepLayoutTailOutcome {
            let mut bytes = vec![0; (bit_count as usize).div_ceil(8)];
            reader
                .copy_bits_to(&mut bytes, u64::from(bit_count))
                .expect("tail reader is bounded to the reported bit count");
            self.rep_layout_tails.push((bit_count, bytes));
            self.rep_layout_tail_outcome
                .unwrap_or(RepLayoutTailOutcome::Unpreserved {
                    cause: StreamFailureCause::UnverifiedRepLayoutTail,
                })
        }
        fn on_actor_close(&mut self, channel_index: u32, _: NetworkGuid, _: bool) {
            self.closes.push(channel_index);
        }
        fn on_content_block(
            &mut self,
            _channel_index: u32,
            _actor_net_guid: NetworkGuid,
            header: &ContentBlockHeader,
        ) -> u32 {
            self.content_blocks.push(header.clone());
            0
        }
        fn on_deleted_block(
            &mut self,
            _channel_index: u32,
            _actor_net_guid: NetworkGuid,
            _header: &ContentBlockHeader,
        ) {
        }

        fn on_stream_failure(&mut self, failure: StreamFailure) {
            self.stream_failures.push(failure);
        }

        fn on_unresolved_class_net_cache_payload(
            &mut self,
            failure: StreamFailure,
            payload: &[u8],
        ) {
            self.failures_before_payload
                .push(self.stream_failures.len());
            self.unresolved_payloads.push((failure, payload.to_vec()));
        }

        fn on_stream_failure_payload(&mut self, _: StreamFailure, _: &[u8]) {
            self.failures_before_payload
                .push(self.stream_failures.len());
        }

        fn on_rep_layout_tail_failure_payload(&mut self, _: StreamFailure, _: BitReader<'_>) {
            self.failures_before_payload
                .push(self.stream_failures.len());
        }
    }

    /// The reused staging buffer only grows: a short fragment after a long one
    /// must not read the long one's bytes back.
    #[test]
    fn fragment_staging_never_exposes_the_previous_fragment() {
        let mut buffer = Vec::new();

        let long = [0xFFu8; 4];
        let staged = stage_fragment(BitReader::with_bit_len(&long, 32).unwrap(), &mut buffer);
        assert_eq!(staged, &[0xFF; 4]);

        // Five zero bits: one byte, and the three padding bits above them must
        // be cleared even though the buffer still holds 0xFF underneath.
        let short = [0x00u8];
        let staged = stage_fragment(BitReader::with_bit_len(&short, 5).unwrap(), &mut buffer);
        assert_eq!(staged, &[0x00]);
        assert_eq!(
            buffer.len(),
            4,
            "the buffer keeps its capacity, not its data"
        );

        // A zero-bit fragment stages nothing and must report zero bytes.
        assert!(
            stage_fragment(BitReader::with_bit_len(&short, 0).unwrap(), &mut buffer).is_empty()
        );
    }

    /// A partial bunch split across two fragments must reassemble and then
    /// frame exactly as an unsplit one would: the one place a bunch payload is
    /// copied rather than viewed. The reference replay takes this path too
    /// (131 fragments, 56 completed on 02d4d478, `validate` at 061155a), but
    /// only as totals; this pins the exact result.
    #[test]
    fn split_bunch_reassembles_and_frames() {
        // Fragment 1 opens channel 2 as a reliable partial initial; its payload
        // is IntPacked(3), a static (odd) actor GUID, so no spawn block.
        let mut bits = Vec::new();
        let initial = BunchSpec {
            ch_index: 2,
            b_open: true,
            b_partial: true,
            b_partial_initial: true,
            ..Default::default()
        };
        write_bunch(&mut bits, &initial, &guid_three());

        // Fragment 2: the partial final, one actor block with a zero-bit body.
        let mut block = Vec::new();
        write_empty_actor_block(&mut block);
        let last = BunchSpec {
            ch_index: 2,
            b_partial: true,
            b_partial_final: true,
            ..Default::default()
        };
        write_bunch(&mut bits, &last, &block);

        let packet = build_packet(&bits);

        let mut reader = ReplicationReader::new("++Ares-Core+release-13.01").unwrap();
        let mut sink = TestSink::default();
        reader.process_packet(&packet, 0, &mut sink);

        let stats = reader.stats();
        assert_eq!(stats.bunches, 2);
        assert_eq!(stats.partial_fragments, 2);
        assert_eq!(stats.partial_bunches, 2);
        assert_eq!(stats.partial_completed, 1);
        assert_eq!(stats.partial_errors, 0);
        assert_eq!(stats.actor_opens, 1, "the open is read from fragment 1");
        assert_eq!(
            stats.content_blocks, 1,
            "the block header spans neither fragment but sits after the join"
        );
        assert_eq!(stats.rep_layout_blocks, 1);
        assert_eq!(stats.skipped_bits, 0, "nothing may be abandoned");
        assert_eq!(sink.opens, vec![2]);
        // A clean pass records and drops no diagnostic event -- asserted after
        // a real reassembly and framing pass, not on a default `NetStats`.
        #[cfg(feature = "diagnostics")]
        {
            assert!(
                stats.diagnostics.is_empty(),
                "a clean pass recorded diagnostic events: {:?}",
                stats.diagnostics
            );
            assert_eq!(stats.diagnostics_dropped, 0);
        }
    }

    /// A partial final with no initial is one error, counted once, by the
    /// accumulator: the packet reader's tracker sees it too but is advisory.
    #[test]
    fn a_partial_final_without_an_initial_is_counted_once() {
        // Reliable partial final on channel 2, no initial anywhere; its
        // payload, actor GUID 3, is never reached.
        let packet = partial_packet(false, false, true, &guid_three());
        let mut reader = ReplicationReader::new("++Ares-Core+release-13.01").unwrap();
        let mut sink = TestSink::default();
        reader.process_packet(&packet, 0, &mut sink);

        assert_eq!(
            reader.stats().partial_errors,
            1,
            "one missing-initial error, counted once (not twice)"
        );
        assert_eq!(reader.stats().partial_bunches, 1);
        assert_eq!(reader.stats().partial_missing_initial, 1);
        assert_eq!(reader.stats().partial_missing_initial_final, 1);
        assert_eq!(reader.stats().partial_missing_initial_reliable, 1);
        assert_eq!(reader.stats().partial_missing_initial_bits, 8);
        assert_eq!(reader.stats().partial_overlapping_initial, 0);
        assert_eq!(reader.stats().partial_mismatched_continuation, 0);
        assert_eq!(reader.stats().partial_non_byte_aligned, 0);
        assert_eq!(sink.rejected_partials.len(), 1);
        let rejected = &sink.rejected_partials[0];
        assert_eq!(
            (rejected.kind, rejected.reason, rejected.bit_count),
            ("current_fragment", PartialPayloadReason::MissingInitial, 8)
        );
        assert_eq!(rejected.payload, &[6]);
        assert_eq!(rejected.header.ch_index, 2);
        assert_eq!(rejected.rejection_packet_id, Some(0));
        assert_eq!(
            reader.stats().skipped_bits,
            8,
            "the rejected fragment payload must remain in loss accounting"
        );
    }

    /// A bunch whose header stage fails is counted and abandoned: a truncated
    /// must-be-mapped list must not leave the reader to parse on as garbage.
    #[test]
    fn a_truncated_bunch_header_failure_is_counted_not_silent() {
        // A 16-bit payload: must-be-mapped count 1 (u16 LE) and no GUID bits,
        // so the GUID read hits the end.
        let packet = build_bunch_packet(
            &BunchSpec {
                ch_index: 2,
                b_has_must_be_mapped_guids: true,
                ..Default::default()
            },
            &must_be_mapped_count_without_its_guid(),
        );
        let mut reader = ReplicationReader::new("++Ares-Core+release-13.01").unwrap();
        let mut sink = TestSink::default();
        reader.process_packet(&packet, 0, &mut sink);

        assert_eq!(
            reader.stats().bunch_header_failures,
            1,
            "truncated must-be-mapped list counted, not silently dropped"
        );
    }

    // --- bunch builders for the lifecycle tests below ---

    /// Little-endian i32, matching `BitReader::read_i32`.
    fn write_i32_bits(bits: &mut Vec<bool>, value: i32) {
        for byte in value.to_le_bytes() {
            write_byte(bits, byte);
        }
    }

    /// A single actor RepLayout content block with an empty body.
    fn write_empty_actor_block(bits: &mut Vec<bool>) {
        bits.push(true); // hasRepLayout
        bits.push(true); // isActor
        write_int_packed(bits, 0); // contentBits = 0
    }

    /// An out-of-range GUID count drops every path declaration in the bunch:
    /// a header failure with its bits tallied, not an export processed.
    #[test]
    fn a_package_map_export_with_an_impossible_guid_count_is_counted() {
        let mut payload: Vec<bool> = Vec::new();
        payload.push(false); // hasRepLayoutExport
        write_i32_bits(&mut payload, crate::types::MAX_GUID_COUNT as i32 + 1);
        // The declarations that get dropped.
        payload.extend(std::iter::repeat_n(true, 24));

        let packet = build_bunch_packet(
            &BunchSpec {
                ch_index: 2,
                b_has_package_map_exports: true,
                ..Default::default()
            },
            &payload,
        );

        let mut reader = ReplicationReader::new("++Ares-Core+release-13.01").unwrap();
        let mut sink = TestSink::default();
        reader.process_packet(&packet, 0, &mut sink);

        let stats = reader.stats();
        assert_eq!(
            stats.package_map_exports, 0,
            "a bunch whose declarations were all dropped is not an export processed"
        );
        assert_eq!(stats.bunch_header_failures, 1);
        assert_eq!(stats.exported_guids, 0);
        assert_eq!(
            stats.skipped_bits, 57,
            "the whole abandoned payload is tallied, not just the unread tail: \
             the bits the failing stage had already consumed declared exports \
             that were dropped (package_map_exports and exported_guids are both \
             0 above), so they are lost too"
        );
    }

    /// A negative count is the same failure: the range test's `as u32` would
    /// read -1 as 4,294,967,295, so the sign is tested on its own.
    #[test]
    fn a_negative_package_map_guid_count_is_counted() {
        let mut payload: Vec<bool> = Vec::new();
        payload.push(false); // hasRepLayoutExport
        write_i32_bits(&mut payload, -1);
        payload.extend(std::iter::repeat_n(true, 16));

        let packet = build_bunch_packet(
            &BunchSpec {
                ch_index: 2,
                b_has_package_map_exports: true,
                ..Default::default()
            },
            &payload,
        );

        let mut reader = ReplicationReader::new("++Ares-Core+release-13.01").unwrap();
        let mut sink = TestSink::default();
        reader.process_packet(&packet, 0, &mut sink);

        assert_eq!(reader.stats().bunch_header_failures, 1);
        assert_eq!(reader.stats().package_map_exports, 0);
        assert_eq!(reader.stats().skipped_bits, 49);
    }

    /// A RepLayout-export bunch is skipped whole, a deliberate limitation,
    /// counted on its own line rather than in `skipped_bits`.
    #[test]
    fn a_rep_layout_export_bunch_is_counted_separately() {
        let mut payload: Vec<bool> = Vec::new();
        payload.push(true); // hasRepLayoutExport -> unsupported, skipped
        payload.extend(std::iter::repeat_n(true, 32));

        let packet = build_bunch_packet(
            &BunchSpec {
                ch_index: 2,
                b_has_package_map_exports: true,
                ..Default::default()
            },
            &payload,
        );

        let mut reader = ReplicationReader::new("++Ares-Core+release-13.01").unwrap();
        let mut sink = TestSink::default();
        reader.process_packet(&packet, 0, &mut sink);

        let stats = reader.stats();
        assert_eq!(stats.rep_layout_export_bunches, 1);
        assert_eq!(
            stats.bunch_header_failures, 0,
            "not a failure, a limitation"
        );
        assert_eq!(stats.skipped_bits, 0, "no content block was involved");
    }

    /// A reassembled partial bunch applies the close flag its final fragment
    /// carried (see `process_bunch`).
    #[test]
    fn a_reassembled_partial_bunch_applies_its_close_flag() {
        let mut bits = Vec::new();

        // Fragment 1: opens channel 2 for static actor GUID 3, byte-aligned.
        let mut first: Vec<bool> = Vec::new();
        write_int_packed(&mut first, 3);
        write_bunch(
            &mut bits,
            &BunchSpec {
                ch_index: 2,
                b_open: true,
                b_partial: true,
                b_partial_initial: true,
                ..Default::default()
            },
            &first,
        );

        // Fragment 2: the final, carrying the close flag and the actor block.
        let mut last: Vec<bool> = Vec::new();
        write_empty_actor_block(&mut last);
        write_bunch(
            &mut bits,
            &BunchSpec {
                ch_index: 2,
                b_close: true,
                b_partial: true,
                b_partial_final: true,
                ..Default::default()
            },
            &last,
        );

        let packet = build_packet(&bits);
        let mut reader = ReplicationReader::new("++Ares-Core+release-13.01").unwrap();
        let mut sink = TestSink::default();
        reader.process_packet(&packet, 0, &mut sink);

        let stats = reader.stats();
        assert_eq!(stats.partial_completed, 1);
        assert_eq!(stats.actor_opens, 1);
        assert_eq!(stats.content_blocks, 1, "the payload is still framed");
        assert_eq!(
            stats.actor_closes, 1,
            "the final fragment closed the channel"
        );
        assert_eq!(sink.closes, vec![2], "the close row must be emitted");
    }

    #[test]
    fn a_rejected_partial_close_still_retires_the_channel() {
        let mut open_payload = Vec::new();
        write_int_packed(&mut open_payload, 3);
        write_empty_actor_block(&mut open_payload);
        let open = build_open_bunch_packet(2, &open_payload);
        let close = build_bunch_packet(
            &BunchSpec {
                ch_index: 2,
                b_close: true,
                b_partial: true,
                b_partial_final: true,
                ..Default::default()
            },
            &[true; 8],
        );

        let mut reader = ReplicationReader::new("++Ares-Core+release-13.01").unwrap();
        let mut sink = TestSink::default();
        reader.process_packet(&open, 0, &mut sink);
        reader.process_packet(&close, 1, &mut sink);

        assert_eq!(reader.stats().partial_errors, 1);
        assert_eq!(reader.stats().skipped_bits, 8);
        assert_eq!(reader.stats().actor_closes, 1);
        assert!(reader.channels.is_empty());
    }

    /// Opening a channel that is already open replaces its actor, as the wire
    /// says; nothing else moves, so the replacement is counted.
    #[test]
    fn opening_an_already_open_channel_is_counted() {
        let mut bits = Vec::new();
        for actor_guid in [3u32, 5] {
            let mut payload: Vec<bool> = Vec::new();
            write_int_packed(&mut payload, actor_guid); // static (odd): no spawn block
            write_empty_actor_block(&mut payload);
            write_bunch(
                &mut bits,
                &BunchSpec {
                    ch_index: 2,
                    b_open: true,
                    ..Default::default()
                },
                &payload,
            );
        }

        let packet = build_packet(&bits);
        let mut reader = ReplicationReader::new("++Ares-Core+release-13.01").unwrap();
        let mut sink = TestSink::default();
        reader.process_packet(&packet, 0, &mut sink);

        let stats = reader.stats();
        assert_eq!(stats.actor_opens, 2);
        assert_eq!(
            stats.actor_closes, 0,
            "no close is fabricated for the first actor"
        );
        assert_eq!(
            stats.channel_reopens_while_open, 1,
            "the overwrite of a live channel must be counted"
        );
    }

    /// A failed open must not leave the previous actor live. Channel 2 opens
    /// static actor 3; a reopen for dynamic actor 4 stops before its spawn
    /// block. The next bunch must not be framed as actor 3's (the stale-schema
    /// shape CLAUDE.md lists).
    #[test]
    fn a_failed_reopen_does_not_leave_the_previous_actor_live() {
        let mut bits = Vec::new();
        let mut first: Vec<bool> = Vec::new();
        write_int_packed(&mut first, 3); // static actor: no spawn block
        write_empty_actor_block(&mut first);
        write_bunch(
            &mut bits,
            &BunchSpec {
                ch_index: 2,
                b_open: true,
                ..Default::default()
            },
            &first,
        );
        let mut failed: Vec<bool> = Vec::new();
        write_int_packed(&mut failed, 4); // dynamic actor, spawn block missing
        write_bunch(
            &mut bits,
            &BunchSpec {
                ch_index: 2,
                b_open: true,
                ..Default::default()
            },
            &failed,
        );
        let mut later: Vec<bool> = Vec::new();
        write_empty_actor_block(&mut later);
        write_bunch(
            &mut bits,
            &BunchSpec {
                ch_index: 2,
                ..Default::default()
            },
            &later,
        );

        let packet = build_packet(&bits);
        let mut reader = ReplicationReader::new("++Ares-Core+release-13.01").unwrap();
        let mut sink = TestSink::default();
        reader.process_packet(&packet, 0, &mut sink);

        let stats = reader.stats();
        assert_eq!(stats.bunch_header_failures, 1, "the failed open itself");
        assert_eq!(stats.actor_opens, 1);
        assert_eq!(
            stats.content_blocks, 1,
            "the bunch after the failed open has no actor to be framed under"
        );
        assert_eq!(
            sink.content_blocks.len(),
            1,
            "nothing may reach the sink under actor 3's schema"
        );
        assert_eq!(
            stats.actor_closes, 0,
            "no close is fabricated for the displaced actor"
        );
        assert_eq!(stats.failed_reopens_while_open, 1);
        assert_eq!(
            (
                stats.bunches_on_unopened_channel,
                stats.unopened_channel_bits
            ),
            (1, 10),
            "the dropped bunch is counted, with its whole 10-bit block"
        );
    }

    /// A failed first open retires nothing, but the next bunch on the channel
    /// is dropped at the guard and counted: the open's own 8 bits are the
    /// header failure's, the later 10 are counted apart from `skipped_bits`.
    #[test]
    fn a_bunch_after_a_failed_first_open_is_counted_not_silent() {
        let mut bits = Vec::new();
        let mut failed: Vec<bool> = Vec::new();
        write_int_packed(&mut failed, 4); // dynamic actor, spawn block missing
        write_bunch(
            &mut bits,
            &BunchSpec {
                ch_index: 7,
                b_open: true,
                ..Default::default()
            },
            &failed,
        );
        let mut later: Vec<bool> = Vec::new();
        write_empty_actor_block(&mut later);
        write_bunch(
            &mut bits,
            &BunchSpec {
                ch_index: 7,
                ..Default::default()
            },
            &later,
        );

        let packet = build_packet(&bits);
        let mut reader = ReplicationReader::new("++Ares-Core+release-13.01").unwrap();
        let mut sink = TestSink::default();
        reader.process_packet(&packet, 0, &mut sink);

        let stats = reader.stats();
        assert_eq!(stats.bunch_header_failures, 1);
        assert_eq!(
            stats.failed_reopens_while_open, 0,
            "there was no live actor to retire"
        );
        assert_eq!(stats.bunches_on_unopened_channel, 1);
        assert_eq!(stats.unopened_channel_bits, 10);
        assert_eq!(
            stats.skipped_bits, 8,
            "only the failed open's own window; the dropped bunch has its own tally"
        );
        assert_eq!(stats.content_blocks, 0);
        assert!(sink.content_blocks.is_empty());
    }

    /// A dormant channel is not an open one: a failed reopen retires nothing,
    /// and a bunch that then arrives is counted, not framed under the dormant
    /// actor.
    #[test]
    fn a_bunch_on_a_dormant_channel_after_a_failed_reopen_is_counted() {
        let mut bits = Vec::new();
        let mut first: Vec<bool> = Vec::new();
        write_int_packed(&mut first, 3);
        write_empty_actor_block(&mut first);
        write_bunch(
            &mut bits,
            &BunchSpec {
                ch_index: 2,
                b_open: true,
                ..Default::default()
            },
            &first,
        );
        write_bunch(
            &mut bits,
            &BunchSpec {
                ch_index: 2,
                b_close: true,
                dormant: true,
                ..Default::default()
            },
            &[],
        );
        let mut failed: Vec<bool> = Vec::new();
        write_int_packed(&mut failed, 4); // dynamic actor, spawn block missing
        write_bunch(
            &mut bits,
            &BunchSpec {
                ch_index: 2,
                b_open: true,
                ..Default::default()
            },
            &failed,
        );
        let mut later: Vec<bool> = Vec::new();
        write_empty_actor_block(&mut later);
        write_bunch(
            &mut bits,
            &BunchSpec {
                ch_index: 2,
                ..Default::default()
            },
            &later,
        );

        let packet = build_packet(&bits);
        let mut reader = ReplicationReader::new("++Ares-Core+release-13.01").unwrap();
        let mut sink = TestSink::default();
        reader.process_packet(&packet, 0, &mut sink);

        let stats = reader.stats();
        assert_eq!(stats.actor_closes, 1, "the dormant close");
        assert_eq!(stats.bunch_header_failures, 1, "the failed reopen");
        assert_eq!(
            stats.failed_reopens_while_open, 0,
            "a dormant channel holds no live actor"
        );
        assert_eq!(stats.content_blocks, 1, "only the first open's block");
        assert_eq!(
            (
                stats.bunches_on_unopened_channel,
                stats.unopened_channel_bits
            ),
            (1, 10)
        );
    }

    // --- open bunches stopped before their open, on a live channel ---
    //
    // Each probe opens channel 2 for static actor 3, sends an open bunch that
    // stops before its own open is read, then a non-open bunch with an empty
    // block. (The open itself failing is
    // a_failed_reopen_does_not_leave_the_previous_actor_live.)

    /// Channel 2 opens for static actor 3, whose empty block frames.
    fn open_actor_three_on_channel_two() -> Vec<u8> {
        build_open_bunch_packet(2, &open_and_empty_block())
    }

    /// A non-open bunch on channel 2 carrying one empty 10-bit actor block.
    fn later_block_on_channel_two() -> Vec<u8> {
        let mut later: Vec<bool> = Vec::new();
        write_empty_actor_block(&mut later);
        build_bunch_packet(
            &BunchSpec {
                ch_index: 2,
                ..Default::default()
            },
            &later,
        )
    }

    /// A must-be-mapped list that declares one GUID (u16 1, little-endian) and
    /// ends there; anything after it would satisfy the GUID read.
    fn must_be_mapped_count_without_its_guid() -> Vec<bool> {
        let mut bits = vec![true, false, false, false, false, false, false, false];
        bits.extend([false; 8]);
        bits
    }

    /// What every probe must show: actor 3 was retired and counted, and the
    /// later block dropped and counted at the guard, not framed as actor 3's.
    fn assert_actor_three_retired(reader: &ReplicationReader, sink: &TestSink) {
        let stats = reader.stats();
        assert_eq!(stats.actor_opens, 1, "only actor 3's open completed");
        assert_eq!(stats.channel_reopens_while_open, 0, "no reopen succeeded");
        assert_eq!(
            stats.failed_reopens_while_open, 1,
            "the displaced actor must be retired and counted"
        );
        assert_eq!(
            stats.content_blocks, 1,
            "only actor 3's own block; the later bunch has no actor to be framed under"
        );
        assert_eq!(
            sink.content_blocks.len(),
            1,
            "nothing may reach the sink under actor 3's schema"
        );
        assert_eq!(
            stats.actor_closes, 0,
            "no close is fabricated for the displaced actor"
        );
        assert_eq!(
            (
                stats.bunches_on_unopened_channel,
                stats.unopened_channel_bits
            ),
            (1, 10),
            "the later bunch is dropped and counted, with its whole 10-bit block"
        );
    }

    /// An open bunch whose must-be-mapped list fails to read is abandoned
    /// before its open is reached.
    #[test]
    fn an_open_whose_must_be_mapped_read_fails_retires_the_live_actor() {
        let reopen = build_bunch_packet(
            &BunchSpec {
                ch_index: 2,
                b_open: true,
                b_has_must_be_mapped_guids: true,
                ..Default::default()
            },
            &must_be_mapped_count_without_its_guid(),
        );
        let (reader, sink) = run_packets(&[
            open_actor_three_on_channel_two(),
            reopen,
            later_block_on_channel_two(),
        ]);

        let stats = reader.stats();
        assert_eq!(stats.bunch_header_failures, 1, "the must-be-mapped read");
        assert_eq!(stats.skipped_bits, 16, "the abandoned window");
        assert_actor_three_retired(&reader, &sink);
    }

    /// An open bunch whose package-map exports fail to read -- a negative GUID
    /// count -- is abandoned before its open is reached.
    #[test]
    fn an_open_whose_package_map_read_fails_retires_the_live_actor() {
        let mut exports: Vec<bool> = Vec::new();
        exports.push(false); // hasRepLayoutExport
        write_i32_bits(&mut exports, -1);
        exports.extend(open_and_empty_block()); // the open, never reached
        let reopen = build_bunch_packet(
            &BunchSpec {
                ch_index: 2,
                b_open: true,
                b_has_package_map_exports: true,
                ..Default::default()
            },
            &exports,
        );
        let (reader, sink) = run_packets(&[
            open_actor_three_on_channel_two(),
            reopen,
            later_block_on_channel_two(),
        ]);

        let stats = reader.stats();
        assert_eq!(stats.bunch_header_failures, 1, "the package-map read");
        assert_eq!(stats.package_map_exports, 0);
        assert_eq!(stats.skipped_bits, 51, "the abandoned window: 1 + 32 + 18");
        assert_actor_three_retired(&reader, &sink);
    }

    /// Clean exports do not save the open: nothing after an export list is
    /// read. No header failure is counted, `failed_reopens_while_open` alone
    /// says an open was lost, and the unread bits are not tallied
    /// (docs/FOLLOWUP.md), so `skipped_bits` is not asserted.
    #[test]
    fn an_open_behind_clean_package_map_exports_retires_the_live_actor() {
        let mut exports: Vec<bool> = Vec::new();
        exports.push(false); // hasRepLayoutExport
        write_i32_bits(&mut exports, 0); // no GUIDs: a clean, empty export list
        exports.extend(open_and_empty_block()); // the open, never read
        let reopen = build_bunch_packet(
            &BunchSpec {
                ch_index: 2,
                b_open: true,
                b_has_package_map_exports: true,
                ..Default::default()
            },
            &exports,
        );
        let (reader, sink) = run_packets(&[
            open_actor_three_on_channel_two(),
            reopen,
            later_block_on_channel_two(),
        ]);

        let stats = reader.stats();
        assert_eq!(stats.package_map_exports, 1, "the exports read cleanly");
        assert_eq!(stats.bunch_header_failures, 0, "nothing failed to read");
        assert_actor_three_retired(&reader, &sink);
    }

    /// An open bunch refused at the channel-state limit never reaches the
    /// header stages at all.
    #[test]
    fn an_open_refused_at_the_channel_limit_retires_the_live_actor() {
        let mut reader = ReplicationReader::new("++Ares-Core+release-13.01").unwrap();
        let mut sink = TestSink::default();
        reader.process_packet(&open_actor_three_on_channel_two(), 0, &mut sink);

        // A packet reader with no room for reliable-sequence state refuses the
        // reopen while the pipeline still holds actor 3: a refusal on a live slot.
        reader.packet_reader = RawPacketReader::with_max_channels(0);
        let reopen = build_open_bunch_packet(2, &open_and_empty_block());
        reader.process_packet(&reopen, 1, &mut sink);
        // Restored, so the later bunch reaches the channel guard rather than
        // being refused too.
        reader.packet_reader = RawPacketReader::new();
        reader.process_packet(&later_block_on_channel_two(), 2, &mut sink);

        let stats = reader.stats();
        assert_eq!(stats.channel_state_limit_failures, 1);
        assert_eq!(
            stats.bunch_header_failures, 1,
            "the refused bunch is abandoned"
        );
        assert_actor_three_retired(&reader, &sink);
    }

    /// The refusal arm retires before it closes: a refused open+close bunch
    /// carried its own actor's close, not actor 3's, so actor 3 gets no close.
    #[test]
    fn an_open_and_close_refused_at_the_channel_limit_closes_nothing_it_did_not_open() {
        let mut reader = ReplicationReader::new("++Ares-Core+release-13.01").unwrap();
        let mut sink = TestSink::default();
        reader.process_packet(&open_actor_three_on_channel_two(), 0, &mut sink);
        reader.packet_reader = RawPacketReader::with_max_channels(0);
        let reopen = build_bunch_packet(
            &BunchSpec {
                ch_index: 2,
                b_open: true,
                b_close: true,
                ..Default::default()
            },
            &open_and_empty_block(),
        );
        reader.process_packet(&reopen, 1, &mut sink);

        let stats = reader.stats();
        assert_eq!(stats.channel_state_limit_failures, 1);
        assert_eq!(stats.failed_reopens_while_open, 1);
        assert_eq!(stats.actor_closes, 0, "actor 3 was never closed");
        assert!(sink.closes.is_empty(), "no close row for actor 3");
        assert!(
            reader.channels.is_empty(),
            "the destroyed channel is still retired"
        );
    }

    /// Only an open bunch displaces an actor: a non-open bunch whose header
    /// stage fails is abandoned, and actor 3 stays live. Pins the `b_open` gate.
    #[test]
    fn a_header_failure_without_an_open_leaves_the_live_actor_open() {
        let failed = build_bunch_packet(
            &BunchSpec {
                ch_index: 2,
                b_has_must_be_mapped_guids: true,
                ..Default::default()
            },
            &must_be_mapped_count_without_its_guid(),
        );
        let (reader, sink) = run_packets(&[
            open_actor_three_on_channel_two(),
            failed,
            later_block_on_channel_two(),
        ]);

        let stats = reader.stats();
        assert_eq!(stats.bunch_header_failures, 1);
        assert_eq!(stats.failed_reopens_while_open, 0, "nothing was reopened");
        assert_eq!(
            stats.content_blocks, 2,
            "the later bunch is still actor 3's"
        );
        assert_eq!(sink.content_blocks.len(), 2);
        assert_eq!(stats.bunches_on_unopened_channel, 0);
    }

    /// The guard counts what it drops: the payload left after the preambles,
    /// nothing for a bunch whose preamble consumed every bit. A must-be-mapped
    /// list is read and counted before the guard either way.
    #[test]
    fn a_bunch_on_a_never_opened_channel_counts_only_what_it_drops() {
        let mut preamble: Vec<bool> = Vec::new();
        // u16 count = 1, little-endian, then one IntPacked GUID.
        preamble.extend([true, false, false, false, false, false, false, false]);
        preamble.extend([false; 8]);
        write_int_packed(&mut preamble, 6);
        let spec = BunchSpec {
            ch_index: 9,
            b_has_must_be_mapped_guids: true,
            ..Default::default()
        };

        let mut bits = Vec::new();
        write_bunch(&mut bits, &spec, &preamble);
        let mut with_block = preamble.clone();
        write_empty_actor_block(&mut with_block);
        write_bunch(&mut bits, &spec, &with_block);

        let packet = build_packet(&bits);
        let mut reader = ReplicationReader::new("++Ares-Core+release-13.01").unwrap();
        let mut sink = TestSink::default();
        reader.process_packet(&packet, 0, &mut sink);

        let stats = reader.stats();
        assert_eq!(stats.bunches, 2);
        assert_eq!(stats.must_be_mapped_guids, 2);
        assert_eq!(
            stats.bunches_on_unopened_channel, 1,
            "the preamble-only bunch dropped nothing"
        );
        assert_eq!(
            stats.unopened_channel_bits, 10,
            "the 24 preamble bits were read, not dropped"
        );
        assert_eq!(stats.bunch_header_failures, 0);
        assert_eq!(stats.skipped_bits, 0);
        assert_eq!(stats.content_blocks, 0);
    }

    /// A channel that was closed and is opened again is the ordinary case and
    /// must NOT be counted as an overwrite.
    #[test]
    fn reopening_a_closed_channel_is_not_counted_as_an_overwrite() {
        let mut bits = Vec::new();
        let mut payload: Vec<bool> = Vec::new();
        write_int_packed(&mut payload, 3);
        write_empty_actor_block(&mut payload);
        write_bunch(
            &mut bits,
            &BunchSpec {
                ch_index: 2,
                b_open: true,
                ..Default::default()
            },
            &payload,
        );
        // Close with an empty payload.
        write_bunch(
            &mut bits,
            &BunchSpec {
                ch_index: 2,
                b_close: true,
                ..Default::default()
            },
            &[],
        );
        let mut payload: Vec<bool> = Vec::new();
        write_int_packed(&mut payload, 5);
        write_empty_actor_block(&mut payload);
        write_bunch(
            &mut bits,
            &BunchSpec {
                ch_index: 2,
                b_open: true,
                ..Default::default()
            },
            &payload,
        );

        let packet = build_packet(&bits);
        let mut reader = ReplicationReader::new("++Ares-Core+release-13.01").unwrap();
        let mut sink = TestSink::default();
        reader.process_packet(&packet, 0, &mut sink);

        let stats = reader.stats();
        assert_eq!(stats.actor_opens, 2);
        assert_eq!(stats.actor_closes, 1);
        assert_eq!(stats.channel_reopens_while_open, 0);
    }

    #[test]
    fn a_destroyed_channel_is_retired_before_later_reuse() {
        let mut open_payload = Vec::new();
        write_int_packed(&mut open_payload, 3);
        write_empty_actor_block(&mut open_payload);
        let open = build_open_bunch_packet(2, &open_payload);
        let close = build_bunch_packet(
            &BunchSpec {
                ch_index: 2,
                b_close: true,
                ..Default::default()
            },
            &[],
        );

        let mut reader = ReplicationReader::new("++Ares-Core+release-13.01").unwrap();
        let mut sink = TestSink::default();
        reader.process_packet(&open, 0, &mut sink);
        assert_eq!(reader.channels.len(), 1);
        reader.process_packet(&close, 1, &mut sink);
        assert!(
            reader.channels.is_empty(),
            "destroyed channels must not accumulate"
        );

        let mut reopened_payload = Vec::new();
        write_int_packed(&mut reopened_payload, 5);
        write_empty_actor_block(&mut reopened_payload);
        let reopened = build_open_bunch_packet(2, &reopened_payload);
        reader.process_packet(&reopened, 2, &mut sink);
        assert_eq!(reader.stats().actor_opens, 2);
        assert_eq!(reader.stats().channel_reopens_while_open, 0);
        assert_eq!(
            reader.channels.len(),
            1,
            "the channel index remains reusable"
        );
    }

    #[test]
    fn an_open_beyond_the_active_channel_budget_fails_closed() {
        let mut reader = ReplicationReader::new("++Ares-Core+release-13.01").unwrap();
        for ch_index in 0..crate::types::MAX_ACTIVE_CHANNELS as u32 {
            reader.channels.insert(ch_index, ChannelSlot::default());
        }
        let mut payload = Vec::new();
        write_int_packed(&mut payload, 3);
        write_empty_actor_block(&mut payload);
        let payload_len = payload.len() as u64;
        let packet = build_open_bunch_packet(crate::types::MAX_ACTIVE_CHANNELS as u32, &payload);
        let mut sink = TestSink::default();
        reader.process_packet(&packet, 0, &mut sink);

        assert_eq!(reader.channels.len(), crate::types::MAX_ACTIVE_CHANNELS);
        assert_eq!(reader.stats().channel_state_limit_failures, 1);
        assert_eq!(reader.stats().actor_opens, 0);
        assert_eq!(reader.stats().bunch_header_failures, 1);
        assert_eq!(reader.stats().skipped_bits, payload_len);
    }

    #[test]
    fn a_non_open_bunch_cannot_grow_the_channel_table_past_its_budget() {
        let mut reader = ReplicationReader::new("++Ares-Core+release-13.01").unwrap();
        for ch_index in 0..crate::types::MAX_ACTIVE_CHANNELS as u32 {
            reader.channels.insert(ch_index, ChannelSlot::default());
        }
        let payload = vec![true, false, true, false, true];
        let packet = build_bunch_packet(
            &BunchSpec {
                ch_index: crate::types::MAX_ACTIVE_CHANNELS as u32,
                ..Default::default()
            },
            &payload,
        );
        let mut sink = TestSink::default();
        reader.process_packet(&packet, 0, &mut sink);

        assert_eq!(reader.channels.len(), crate::types::MAX_ACTIVE_CHANNELS);
        assert_eq!(reader.stats().channel_state_limit_failures, 1);
        assert_eq!(reader.stats().bunch_header_failures, 1);
        assert_eq!(reader.stats().skipped_bits, payload.len() as u64);
    }

    #[test]
    fn raw_reliable_state_refusal_is_abandoned_and_counted_once() {
        let mut reader = ReplicationReader::new("++Ares-Core+release-13.01").unwrap();
        reader.packet_reader = RawPacketReader::with_max_channels(0);
        let payload = vec![true, false, true, false, true];
        let packet = build_bunch_packet(&BunchSpec::default(), &payload);
        let mut sink = TestSink::default();
        reader.process_packet(&packet, 0, &mut sink);

        assert!(reader.channels.is_empty());
        assert_eq!(reader.stats().channel_state_limit_failures, 1);
        assert_eq!(reader.stats().bunch_header_failures, 1);
        assert_eq!(reader.stats().skipped_bits, payload.len() as u64);
    }

    /// A dynamic actor's spawn block is mandatory: a payload that ends at the
    /// actor GUID is a failed open, not an actor with archetype 0 and no
    /// transform.
    #[test]
    fn a_dynamic_open_without_its_spawn_block_is_a_failure_not_an_actor() {
        let mut payload: Vec<bool> = Vec::new();
        write_int_packed(&mut payload, 2); // dynamic actor GUID, then nothing

        let packet = build_open_bunch_packet(2, &payload);

        let mut reader = ReplicationReader::new("++Ares-Core+release-13.01").unwrap();
        let mut sink = TestSink::default();
        reader.process_packet(&packet, 0, &mut sink);

        let stats = reader.stats();
        assert_eq!(stats.actor_opens, 0, "no actor may be invented");
        assert_eq!(stats.actor_opens_missing_spawn, 1);
        assert_eq!(stats.bunch_header_failures, 1);
        assert!(sink.opens.is_empty(), "no open event may reach the sink");
    }

    /// A static actor has no spawn block, so its open is complete at the actor
    /// GUID.
    #[test]
    fn a_static_open_with_no_payload_left_is_still_an_actor() {
        let mut payload: Vec<bool> = Vec::new();
        write_int_packed(&mut payload, 3); // static (odd) actor GUID

        let packet = build_open_bunch_packet(2, &payload);

        let mut reader = ReplicationReader::new("++Ares-Core+release-13.01").unwrap();
        let mut sink = TestSink::default();
        reader.process_packet(&packet, 0, &mut sink);

        assert_eq!(reader.stats().actor_opens, 1);
        assert_eq!(reader.stats().actor_opens_missing_spawn, 0);
        assert_eq!(reader.stats().bunch_header_failures, 0);
        assert_eq!(sink.opens, vec![2]);
    }

    /// A partial whose continuation never arrives is loss that only `finish`
    /// can name: nothing was out of sequence.
    #[test]
    fn an_unfinished_partial_bunch_is_reported_at_eof() {
        let mut payload: Vec<bool> = Vec::new();
        write_int_packed(&mut payload, 3); // 8 bits, byte-aligned

        let packet = build_bunch_packet(
            &BunchSpec {
                ch_index: 2,
                b_open: true,
                b_partial: true,
                b_partial_initial: true,
                ..Default::default()
            },
            &payload,
        );

        let mut reader = ReplicationReader::new("++Ares-Core+release-13.01").unwrap();
        let mut sink = TestSink::default();
        reader.process_packet(&packet, 0, &mut sink);
        assert_eq!(
            reader.stats().partial_errors,
            0,
            "an in-progress reassembly is not an error until the stream ends"
        );

        reader.finish();

        let stats = reader.stats();
        assert_eq!(stats.unfinished_partials, 1);
        assert_eq!(stats.unfinished_partial_bits, 8);
        assert_eq!(stats.partial_errors, 0, "still not a sequence error");
    }

    /// `finish` after a clean reassembly counts nothing, and a second call
    /// counts nothing either -- the drained state must not be counted twice.
    #[test]
    fn finish_is_idempotent_and_silent_on_a_clean_stream() {
        let mut reader = ReplicationReader::new("++Ares-Core+release-13.01").unwrap();
        let mut sink = TestSink::default();
        reader.process_packet(&[0x01], 0, &mut sink);

        reader.finish();
        reader.finish();

        assert_eq!(reader.stats().unfinished_partials, 0);
        assert_eq!(reader.stats().unfinished_partial_bits, 0);
    }

    /// An unaligned window is realigned to bit zero of the staging buffer, the
    /// only reason the copy exists.
    #[test]
    fn fragment_staging_realigns_an_offset_window() {
        let packet = [0b1111_0000u8, 0b0000_1111];
        let mut reader = BitReader::new(&packet);
        reader.skip_bits(4).unwrap();
        let window = reader.sub_reader(8).unwrap();

        let mut buffer = Vec::new();
        assert_eq!(stage_fragment(window, &mut buffer), &[0b1111_1111]);
    }

    #[test]
    fn reader_requires_valid_branch() {
        assert!(ReplicationReader::new("++Ares-Core+release-13.01").is_ok());
        assert!(ReplicationReader::new("++Ares-Core+release-99.99").is_err());
    }

    #[test]
    fn empty_packet_is_no_op() {
        let mut reader = ReplicationReader::new("++Ares-Core+release-13.01").unwrap();
        let mut sink = TestSink::default();
        reader.process_packet(&[], 0, &mut sink);
        assert_eq!(reader.stats().packets, 1);
        assert_eq!(reader.stats().bunches, 0);
        assert_eq!(reader.stats().malformed_packets, 0);
    }

    #[test]
    fn malformed_packet_counted() {
        let mut reader = ReplicationReader::new("++Ares-Core+release-13.01").unwrap();
        let mut sink = TestSink::default();
        reader.process_packet(&[0x00, 0x00], 0, &mut sink);
        assert_eq!(reader.stats().malformed_packets, 1);
    }

    /// One block handed straight to `decode_and_walk`, bypassing framing.
    struct Run {
        /// What the call returned: `false` only for a failed transform.
        transformed: bool,
        stats: NetStats,
        sink: TestSink,
        /// Where the bunch reader stood afterwards.
        position: u64,
    }

    /// Decode a `bit_count`-bit block for actor 2 from the first `window_bits`
    /// of `wire` into `scratch`: RepLayout when `function_count` is `None`,
    /// ClassNetCache with that count otherwise.
    fn decode_block(
        wire: &[u8],
        window_bits: u64,
        bit_count: usize,
        function_count: Option<u32>,
        mut scratch: Vec<u8>,
        mut sink: TestSink,
    ) -> Run {
        let mut payload = BitReader::with_bit_len(wire, window_bits).unwrap();
        let mut stats = NetStats::default();
        let mut channels = ChannelTable::default();
        let mut stage = Stage {
            stats: &mut stats,
            channels: &mut channels,
            transform: TransformVersion::V1301,
            scratch: &mut scratch,
        };
        let actor = NetworkGuid(2);
        let transformed = framing::decode_and_walk(
            &mut payload,
            bit_count,
            actor,
            function_count,
            &mut stage,
            &mut sink,
        );
        Run {
            transformed,
            stats,
            sink,
            position: payload.position(),
        }
    }

    /// The golden V13.01 vector: wire 0xBF for actor 2 decodes to 0x66, seven
    /// bits. The scratch buffer starts full of 0xFF, so a leaked tail shows.
    fn decode_golden(bit_count: usize, function_count: Option<u32>) -> Run {
        let sink = TestSink::default();
        decode_block(&[0xBF], 7, bit_count, function_count, vec![0xFF; 16], sink)
    }

    /// Decode the block whose decoded bits are `decoded_bits`, starting from
    /// an empty scratch buffer.
    fn decode_bits(decoded_bits: &[bool], function_count: Option<u32>, sink: TestSink) -> Run {
        let bit_count = decoded_bits.len();
        let wire = wire_for_short_decoded(&pack(decoded_bits), bit_count, 2);
        decode_block(
            &wire,
            bit_count as u64,
            bit_count,
            function_count,
            Vec::new(),
            sink,
        )
    }

    /// The unresolved callback receives the exact decoded block, not the wire
    /// bytes or the reusable scratch tail.
    #[test]
    fn unresolved_class_net_cache_exposes_the_decoded_whole_payload() {
        let Run {
            stats,
            sink,
            position,
            ..
        } = decode_golden(7, Some(0));

        assert_eq!(position, 7);
        assert_eq!(sink.unresolved_payloads.len(), 1);
        let (failure, decoded) = &sink.unresolved_payloads[0];
        assert_eq!(failure.kind, StreamKind::Rpc);
        assert_eq!(failure.actor_net_guid, NetworkGuid(2));
        assert_eq!(failure.bit_count, 7);
        assert_eq!(failure.function_count, 0);
        assert_eq!(failure.consumed_bits, 0);
        assert_eq!(failure.remaining_bits, 7);
        assert_eq!(decoded, &[0x66]);
        assert_eq!(decoded[0] >> 7, 0, "high padding bit must stay zero");
        assert_eq!(sink.failures_before_payload, [1], "failure, then payload");
        assert_eq!(stats.rpcs, 0);
        assert_eq!(stats.rpc_stream_failures, 1);
        assert_eq!(stats.unresolved_rpc_payloads_preserved, 1);
        assert_eq!(stats.skipped_bits, 7);
    }

    /// A transform failure goes back to the framing loop, which alone holds the
    /// header and position its `ParseFailure` event needs. Framing never gets
    /// here on its own (see `SkipReason::ParseFailure`), so an 8-bit block over
    /// a 7-bit payload, called directly, is the way in.
    #[test]
    fn a_transform_failure_is_reported_back_to_framing() {
        // RepLayout, then ClassNetCache with a resolved function count.
        for function_count in [None, Some(2)] {
            let Run {
                transformed, stats, ..
            } = decode_golden(7, function_count);
            assert!(
                transformed,
                "a payload whose transform ran is reported decoded"
            );
            assert_eq!(stats.transform_failures, 0);

            let Run {
                transformed,
                stats,
                sink,
                ..
            } = decode_golden(8, function_count);
            assert!(
                !transformed,
                "an 8-bit block cannot be copied out of 7 bits"
            );
            assert_eq!(stats.transform_failures, 1);
            assert_eq!(stats.skipped_bits, 8, "the block's whole declared length");
            assert_eq!(
                stats.field_stream_failures + stats.rpc_stream_failures,
                0,
                "a transform failure is not a stream failure"
            );
            assert!(sink.stream_failures.is_empty());
            assert!(sink.fields.is_empty() && sink.rpcs.is_empty());
            #[cfg(feature = "diagnostics")]
            {
                assert!(
                    stats.diagnostics.is_empty(),
                    "the event is the framing loop's to record"
                );
            }
        }

        // Through the framing loop, a block whose transform ran records no
        // event, whatever its inner stream does (here an unresolved payload).
        let mut bits = vec![false, true]; // ClassNetCache, isActor
        write_int_packed(&mut bits, 7);
        bits.extend([true, true, true, true, true, true, false]);
        let (stats, sink) = frame_bits(&bits);
        assert_eq!(stats.class_net_cache_blocks, 1);
        assert_eq!(stats.transform_failures, 0);
        assert_eq!(sink.unresolved_payloads.len(), 1);
        #[cfg(feature = "diagnostics")]
        {
            assert!(stats.diagnostics.is_empty());
        }
    }

    /// A ClassNetCache walk that returns `Ok` but abandons bits is a stream
    /// failure with those bits in `skipped_bits`. With `function_count` 2 the
    /// golden 0x66 gives one handle bit, then 6 bits -- fewer than the 8 a
    /// payload length needs.
    #[test]
    fn class_net_cache_overrun_ok_path_is_a_stream_failure() {
        let Run {
            stats,
            sink,
            position,
            ..
        } = decode_golden(7, Some(2));

        assert_eq!(position, 7);
        assert_eq!(stats.rpc_stream_failures, 1);
        assert_eq!(stats.unresolved_rpc_payloads_preserved, 0);
        assert_eq!(stats.rpcs, 0);
        // The 6 abandoned bits must be counted, not silently dropped.
        assert!(
            stats.skipped_bits >= 6,
            "abandoned mid-block bits must land in skipped_bits, got {}",
            stats.skipped_bits
        );
        assert_eq!(sink.stream_failures.len(), 1);
        assert_eq!(sink.stream_failures[0].kind, StreamKind::Rpc);
        assert_eq!(sink.stream_failures[0].remaining_bits, 7);
    }

    /// Invert the short V13.01 byte transform for a test payload: below 32
    /// bits it works byte by byte, so each byte's one preimage can be searched
    /// for without duplicating the production transform.
    fn wire_for_short_decoded(decoded: &[u8], bit_count: usize, actor: u32) -> Vec<u8> {
        assert!(bit_count < 32);
        let byte_count = bit_count.div_ceil(8);
        let mut wire = vec![0u8; byte_count];
        for offset in 0..byte_count {
            let mask = if offset + 1 == byte_count && bit_count % 8 != 0 {
                0xff >> (8 - bit_count % 8)
            } else {
                0xff
            };
            wire[offset] = (0u8..=u8::MAX)
                .find(|&candidate| {
                    let mut trial = wire.clone();
                    trial[offset] = candidate;
                    TransformVersion::V1301
                        .apply(
                            &mut trial,
                            bit_count,
                            vrf_transform::seed_for(bit_count, actor),
                        )
                        .unwrap();
                    trial[offset] & mask == decoded[offset] & mask
                })
                .expect("the transform is bijective");
        }
        wire
    }

    /// A zero handle closes only the RepLayout prefix. Bits after it belong to
    /// the chained ClassNetCache stream and must reach the sink exactly.
    #[test]
    fn rep_layout_zero_terminator_hands_the_exact_tail_to_the_sink() {
        let mut decoded_bits = Vec::new();
        decoded_bits.push(false); // property checksum
        write_int_packed(&mut decoded_bits, 0); // RepLayout terminator
        decoded_bits.extend((0..13).map(|index| index % 2 == 0));

        let sink = TestSink {
            rep_layout_tail_outcome: Some(RepLayoutTailOutcome::Decoded { rpc_count: 1 }),
            ..TestSink::default()
        };
        let Run {
            transformed,
            stats,
            sink,
            ..
        } = decode_bits(&decoded_bits, None, sink);

        assert!(transformed, "the block's transform ran");
        assert!(sink.fields.is_empty());
        assert_eq!(sink.rep_layout_tails, vec![(13, vec![0x55, 0x15])]);
        assert!(sink.stream_failures.is_empty());
        assert_eq!(stats.fields, 0);
        assert_eq!(stats.rpcs, 1);
        assert_eq!(stats.field_stream_failures, 0);
        assert_eq!(stats.rpc_stream_failures, 0);
        assert_eq!(stats.skipped_bits, 0);
    }

    #[test]
    fn a_wholly_preserved_rep_layout_tail_is_an_rpc_failure_not_field_loss() {
        let mut decoded_bits = vec![false]; // property checksum
        write_int_packed(&mut decoded_bits, 0); // RepLayout terminator
        decoded_bits.extend((0..13).map(|index| index % 2 == 0));
        let sink = TestSink {
            rep_layout_tail_outcome: Some(RepLayoutTailOutcome::Preserved {
                cause: StreamFailureCause::UnverifiedRepLayoutTail,
            }),
            ..TestSink::default()
        };
        let Run { stats, sink, .. } = decode_bits(&decoded_bits, None, sink);

        assert_eq!(stats.field_stream_failures, 0);
        assert_eq!(stats.rpc_stream_failures, 1);
        assert_eq!(stats.unresolved_rpc_payloads_preserved, 1);
        assert_eq!(stats.skipped_bits, 13);
        assert_eq!(sink.stream_failures.len(), 1);
        let failure = sink.stream_failures[0];
        assert_eq!(failure.kind, StreamKind::Rpc);
        assert_eq!(failure.bit_count, 13);
        assert!(failure.payload_preserved);
        assert_eq!(sink.failures_before_payload, [1], "failure, then payload");
    }

    #[test]
    fn rep_layout_overrun_ok_path_is_a_stream_failure() {
        let mut decoded_bits = Vec::new();
        decoded_bits.push(false); // property checksum
        write_int_packed(&mut decoded_bits, 1); // handle 0
        write_int_packed(&mut decoded_bits, 32); // overruns the remaining 8 bits
        decoded_bits.extend(std::iter::repeat_n(false, 8));
        assert_eq!(decoded_bits.len(), 25);
        let Run { stats, sink, .. } = decode_bits(&decoded_bits, None, TestSink::default());

        assert_eq!(stats.field_stream_failures, 1);
        assert_eq!(stats.skipped_bits, 24);
        assert_eq!(sink.stream_failures.len(), 1);
        assert_eq!(sink.stream_failures[0].kind, StreamKind::RepLayout);
        assert_eq!(sink.stream_failures[0].consumed_bits, 1);
        assert_eq!(sink.stream_failures[0].remaining_bits, 24);
        assert!(
            sink.rep_layout_tails.is_empty(),
            "a declared-length overrun is malformed RepLayout, not a chained tail"
        );
    }

    /// `cause` names the arm that produced each failure, and the record the
    /// walk stopped in: the per-group aggregate keyed on it separates
    /// preserved-unresolved blocks from real loss.
    #[test]
    fn stream_failures_carry_their_cause_and_failing_record() {
        // Arm 1: an unresolved group (function_count 0). The walk never
        // begins: offset 0, no handle, and the payload preserved.
        let Run { sink, .. } = decode_golden(7, Some(0));
        assert_eq!(sink.stream_failures.len(), 1);
        let failure = &sink.stream_failures[0];
        assert_eq!(failure.cause, StreamFailureCause::UnresolvedFunctionCount);
        assert_eq!(failure.record_handle, None);
        assert_eq!(failure.record_offset, Some(0));
        assert_eq!(sink.unresolved_payloads.len(), 1, "payload preserved");

        // Arm 2: a RepLayout Ok walk that abandoned a tail, in the record for
        // handle 0 that began at bit 1, after the checksum.
        let mut decoded_bits = Vec::new();
        decoded_bits.push(false); // property checksum
        write_int_packed(&mut decoded_bits, 1); // handle 0
        write_int_packed(&mut decoded_bits, 32); // overruns the remaining 8 bits
        decoded_bits.extend(std::iter::repeat_n(false, 8));
        let Run { sink, .. } = decode_bits(&decoded_bits, None, TestSink::default());
        assert_eq!(sink.stream_failures.len(), 1);
        let failure = &sink.stream_failures[0];
        assert_eq!(failure.cause, StreamFailureCause::AbandonedTail);
        assert_eq!(failure.record_handle, Some(0));
        assert_eq!(failure.record_offset, Some(1));
    }

    /// The `Err` arm charges the whole block (see
    /// `decode_and_walk`), not the reader's remainder. Nine decoded
    /// bits: the checksum, then 0x01 -- an `IntPacked` chunk promising another
    /// the block lacks -- so the handle read fails with the window consumed and
    /// `bits_remaining() == 0`.
    #[test]
    fn rep_layout_err_at_the_exact_block_end_still_charges_the_block() {
        let mut decoded_bits = vec![false]; // property checksum
        // 0x01: continuation set, payload bits all zero.
        write_byte(&mut decoded_bits, 0x01);
        assert_eq!(decoded_bits.len(), 9);

        let Run { stats, sink, .. } = decode_bits(&decoded_bits, None, TestSink::default());

        assert_eq!(stats.field_stream_failures, 1);
        assert_eq!(stats.fields, 0, "no field was emitted");
        assert_eq!(sink.stream_failures.len(), 1);
        assert_eq!(sink.stream_failures[0].remaining_bits, 0);
        assert_eq!(sink.stream_failures[0].consumed_bits, 9);
        assert_eq!(
            stats.skipped_bits, 9,
            "a stream failure with no bits behind it is the defect this pins"
        );
    }

    #[test]
    fn rep_layout_read_error_keeps_an_already_emitted_prefix_counted() {
        let mut decoded_bits = vec![false]; // property checksum
        write_int_packed(&mut decoded_bits, 1); // handle 0
        write_int_packed(&mut decoded_bits, 0); // valid zero-bit payload
        write_byte(&mut decoded_bits, 0x01);
        assert_eq!(decoded_bits.len(), 25);
        let Run { stats, sink, .. } = decode_bits(&decoded_bits, None, TestSink::default());

        assert_eq!(sink.fields, vec![(0, 0)], "the valid prefix row remains");
        assert_eq!(stats.fields, 1, "NetStats matches the emitted prefix");
        assert_eq!(stats.field_stream_failures, 1);
        assert!(sink.rep_layout_tails.is_empty());
        assert_eq!(sink.stream_failures[0].cause, StreamFailureCause::ReadError);
        assert_eq!(sink.stream_failures[0].record_handle, None);
        assert_eq!(sink.stream_failures[0].record_offset, Some(17));
    }

    /// The RPC `Err` arm, same shape: one handle bit, then 0x01.
    /// `function_count` 2 walks the stream rather than taking the unresolved
    /// arm, which has its own accounting.
    #[test]
    fn class_net_cache_err_at_the_exact_block_end_still_charges_the_block() {
        let mut decoded_bits = Vec::new();
        write_serialized_int(&mut decoded_bits, 0, 2); // one handle bit
        write_byte(&mut decoded_bits, 0x01);
        assert_eq!(decoded_bits.len(), 9);

        let Run { stats, sink, .. } = decode_bits(&decoded_bits, Some(2), TestSink::default());

        assert_eq!(stats.rpc_stream_failures, 1);
        assert_eq!(stats.rpcs, 0, "no RPC was emitted");
        assert_eq!(
            stats.unresolved_rpc_payloads_preserved, 0,
            "this is a walked stream that failed, not an unresolved group"
        );
        assert_eq!(sink.stream_failures.len(), 1);
        assert_eq!(sink.stream_failures[0].remaining_bits, 0);
        assert_eq!(sink.failures_before_payload, [1], "failure, then payload");
        assert_eq!(
            stats.skipped_bits, 9,
            "a stream failure with no bits behind it is the defect this pins"
        );
    }

    #[test]
    fn class_net_cache_read_error_keeps_an_already_emitted_prefix_counted() {
        let mut decoded_bits = Vec::new();
        write_serialized_int(&mut decoded_bits, 0, 2);
        write_int_packed(&mut decoded_bits, 0); // valid zero-bit RPC
        write_serialized_int(&mut decoded_bits, 0, 2);
        write_byte(&mut decoded_bits, 0x01);
        assert_eq!(decoded_bits.len(), 18);
        let Run { stats, sink, .. } = decode_bits(&decoded_bits, Some(2), TestSink::default());

        assert_eq!(sink.rpcs, vec![(0, 0)], "the valid prefix row remains");
        assert_eq!(stats.rpcs, 1, "NetStats matches the emitted prefix");
        assert_eq!(stats.rpc_stream_failures, 1);
        assert_eq!(sink.stream_failures[0].cause, StreamFailureCause::ReadError);
        assert_eq!(sink.stream_failures[0].record_handle, Some(0));
        assert_eq!(sink.stream_failures[0].record_offset, Some(9));
    }

    /// Frame `bits` as one whole bunch payload, straight through
    /// `frame_content_blocks`, on channel 5 for static actor 42.
    fn frame_bits(bits: &[bool]) -> (NetStats, TestSink) {
        let data = pack(bits);
        let mut payload = BitReader::with_bit_len(&data, bits.len() as u64).unwrap();
        let mut reader = ReplicationReader::new("++Ares-Core+release-13.01").unwrap();
        let mut sink = TestSink::default();
        let mut stats = NetStats::default();
        let mut channels = ChannelTable::default();
        let header = RawBunchHeader {
            ch_index: 5,
            payload_bit_count: bits.len() as i32,
            ..Default::default()
        };
        let ctx = BunchContext {
            header: &header,
            ids: BunchIds {
                bunch_index_in_packet: 0,
                global_bunch_index: 0,
                channel_bunch_index: 1,
            },
            actor_net_guid: NetworkGuid(42),
            archetype_net_guid: NetworkGuid(0),
        };
        let mut stage = Stage {
            stats: &mut stats,
            channels: &mut channels,
            transform: reader.transform,
            scratch: &mut reader.scratch,
        };
        framing::frame_content_blocks(&mut payload, &mut stage, &mut sink, &ctx);
        assert!(
            payload.at_end(),
            "an abort leaves nothing behind for a caller to misread"
        );
        (stats, sink)
    }

    /// A subobject header that runs out at its class GUID: hasRepLayout,
    /// isActor = 0, object GUID IntPacked(5), not stably named, not deleted --
    /// 12 bits, all read, and then the class GUID's first byte is not there.
    fn truncated_subobject_header() -> Vec<bool> {
        let mut bits = vec![true, false];
        write_int_packed(&mut bits, 5);
        bits.extend([false, false]);
        bits
    }

    /// A header that fails to read has consumed what it read: the abort charges
    /// those 12 bits, not the 0 left (see `framing::abort`).
    #[test]
    fn a_truncated_block_header_charges_the_bits_it_consumed() {
        let (stats, sink) = frame_bits(&truncated_subobject_header());

        assert_eq!(stats.content_block_framing_failures, 1);
        assert_eq!(stats.skipped_bits, 12, "the whole failed block, not 0");
        assert_eq!(stats.content_blocks, 0);
        assert!(sink.content_blocks.is_empty());
        #[cfg(feature = "diagnostics")]
        {
            use crate::stats::SkipReason;
            let ev = &stats.diagnostics[0];
            assert!(matches!(ev.reason, SkipReason::HeaderReadError));
            assert_eq!(ev.bits_skipped, 12);
            assert_eq!((ev.consumed_bits, ev.remaining_bits), (0, 0));
        }
    }

    /// A `content_bits` continuation byte at the end of the bunch consumes 8
    /// bits before the read fails: with the 2-bit actor header, 10 bits lost.
    #[test]
    fn a_truncated_content_bits_field_charges_the_bits_it_consumed() {
        let mut bits = vec![true, true]; // hasRepLayout, isActor
        bits.extend([true, false, false, false, false, false, false, false]); // 0x01: more follows

        let (stats, _) = frame_bits(&bits);

        assert_eq!(stats.content_block_framing_failures, 1);
        assert_eq!(stats.skipped_bits, 10);
        assert_eq!(stats.content_blocks, 0);
        #[cfg(feature = "diagnostics")]
        {
            use crate::stats::SkipReason;
            let ev = &stats.diagnostics[0];
            assert!(matches!(ev.reason, SkipReason::ContentBitsReadError));
            assert_eq!(ev.bits_skipped, 10);
            assert_eq!((ev.consumed_bits, ev.remaining_bits), (2, 0));
        }
    }

    /// The charge starts at the failing block, not at the bunch: a block that
    /// framed before it keeps its bits out of the loss.
    #[test]
    fn a_framing_abort_does_not_recharge_blocks_that_framed() {
        let mut bits = Vec::new();
        write_empty_actor_block(&mut bits); // 10 bits, frames cleanly
        bits.extend(truncated_subobject_header()); // 12 bits, fails

        let (stats, sink) = frame_bits(&bits);

        assert_eq!(stats.content_blocks, 1);
        assert_eq!(sink.content_blocks.len(), 1);
        assert_eq!(stats.content_block_framing_failures, 1);
        assert_eq!(stats.skipped_bits, 12, "22 bits, of which 10 framed");
    }

    /// A content-block overrun produces a `DiagnosticEvent` with full context:
    /// the path the resolved "malformed 1 / skipped 695" residue took (see the
    /// oracle's module doc). It charges from the block's first bit: 2 + 16 + 8
    /// = 26 bits, while 8 remained after the read.
    #[cfg(feature = "diagnostics")]
    #[test]
    fn content_bits_overrun_emits_diagnostic() {
        use crate::stats::SkipReason;

        let mut reader = ReplicationReader::new("++Ares-Core+release-13.01").unwrap();
        let mut sink = TestSink::default();

        // A 2-bit header (has_rep_layout=0, is_actor=1), content_bits =
        // IntPacked(999) in 16 bits, then 8 bits: 999 overruns what is left.
        let mut bits: Vec<bool> = Vec::new();
        bits.push(false);
        bits.push(true);
        // IntPacked(999): 999 = 0x3E7
        //   chunk0: (999 & 0x7F) = 0x67, more=1 -> byte = (0x67 << 1) | 1 = 0xCF
        //   chunk1: (999 >> 7) = 7, more=0 -> byte = (7 << 1) | 0 = 0x0E
        for byte in [0xCF_u8, 0x0E] {
            write_byte(&mut bits, byte);
        }
        // Add a few more padding bits so remaining > 0 but < 999
        bits.extend(std::iter::repeat_n(false, 8));

        let data = pack(&bits);

        let mut payload_reader = BitReader::with_bit_len(&data, bits.len() as u64).unwrap();
        let mut stats = NetStats::default();
        let mut channels = ChannelTable::default();
        let header = RawBunchHeader {
            packet_id: 42,
            ch_index: 5,
            b_open: true,
            b_reliable: true,
            payload_bit_count: bits.len() as i32,
            ..Default::default()
        };
        let ctx = BunchContext {
            header: &header,
            ids: BunchIds {
                bunch_index_in_packet: 3,
                global_bunch_index: 100,
                channel_bunch_index: 7,
            },
            actor_net_guid: NetworkGuid(42),
            archetype_net_guid: NetworkGuid(0),
        };
        let mut stage = Stage {
            stats: &mut stats,
            channels: &mut channels,
            transform: reader.transform,
            scratch: &mut reader.scratch,
        };

        framing::frame_content_blocks(&mut payload_reader, &mut stage, &mut sink, &ctx);

        assert_eq!(stats.malformed_content_blocks, 1);
        assert_eq!(
            stats.skipped_bits, 26,
            "the block's header and content_bits were read and framed nothing"
        );
        assert_eq!(stats.diagnostics.len(), 1);

        let ev = &stats.diagnostics[0];
        assert_eq!(ev.packet_id, 42);
        assert_eq!(ev.bunch_index_in_packet, 3);
        assert_eq!(ev.global_bunch_index, 100);
        assert_eq!(ev.channel_bunch_index, 7);
        assert_eq!(ev.channel_index, 5);
        assert_eq!(ev.actor_net_guid, 42);
        assert_eq!(ev.block_index_in_bunch, 0);
        assert!(ev.content_bits.is_some());
        assert_eq!(ev.content_bits.unwrap(), 999);
        // 8 bits remained; the loss is the whole block from its first bit.
        assert_eq!(ev.remaining_bits, 8);
        assert_eq!(ev.bits_skipped, 26);
        assert!(
            ev.bunch_flags.b_open && ev.bunch_flags.b_reliable && !ev.bunch_flags.b_partial,
            "flags are snapshotted from the bunch header on the failure path"
        );
        match &ev.reason {
            SkipReason::ContentBitsOverrun {
                declared_content_bits,
                available_bits,
            } => {
                assert_eq!(*declared_content_bits, 999);
                assert_eq!(*available_bits, 8);
            }
            _ => panic!("expected ContentBitsOverrun"),
        }
    }

    /// A diagnostic event names the archetype its channel's open read (9
    /// here), not a plausible 0. Paths are not resolved by framing: `None`.
    #[cfg(feature = "diagnostics")]
    #[test]
    fn a_diagnostic_event_carries_the_channel_archetype() {
        use crate::stats::SkipReason;

        let mut open: Vec<bool> = Vec::new();
        write_int_packed(&mut open, 2); // dynamic actor GUID
        write_minimal_spawn_data(&mut open, 9); // archetype 9, not a controller
        let mut overrun: Vec<bool> = vec![false, true]; // ClassNetCache, isActor
        write_int_packed(&mut overrun, 999); // declares far more than follows
        overrun.extend([false; 8]);

        let (reader, _) = run_packets(&[
            build_open_bunch_packet(2, &open),
            build_bunch_packet(
                &BunchSpec {
                    ch_index: 2,
                    ..Default::default()
                },
                &overrun,
            ),
        ]);

        let state = reader.channels[&2]
            .state
            .as_ref()
            .expect("channel 2 opened");
        assert_eq!(
            state.archetype_net_guid,
            NetworkGuid(9),
            "the open itself must have read archetype 9, or this proves nothing"
        );
        let stats = reader.stats();
        assert_eq!(stats.malformed_content_blocks, 1);
        assert_eq!(stats.diagnostics.len(), 1);
        let ev = &stats.diagnostics[0];
        assert!(matches!(
            ev.reason,
            SkipReason::ContentBitsOverrun {
                declared_content_bits: 999,
                available_bits: 8
            }
        ));
        assert_eq!(
            (ev.packet_id, ev.channel_index, ev.actor_net_guid),
            (1, 2, 2)
        );
        assert_eq!(ev.archetype_net_guid, 9);
        assert!(ev.actor_path.is_none() && ev.class_path.is_none());
    }

    // --- controller property-block regression tests
    // (docs/archive/PROJECT_STATUS.md 17-A) ---

    /// One reliable, non-partial bunch that opens `ch_index` around
    /// `payload_bits`, as a packet ready for `process_packet`.
    fn build_open_bunch_packet(ch_index: u32, payload_bits: &[bool]) -> Vec<u8> {
        let spec = BunchSpec {
            ch_index,
            b_open: true,
            ..Default::default()
        };
        build_bunch_packet(&spec, payload_bits)
    }

    /// Write a dynamic actor's spawn block: archetype, level, and the four
    /// optional transforms (location, rotation, scale, velocity) all absent.
    /// Velocity matches the unconditional read; omitting it is the one-bit
    /// regression.
    fn write_minimal_spawn_data(bits: &mut Vec<bool>, archetype_guid: u32) {
        write_int_packed(bits, archetype_guid); // archetype GUID
        write_int_packed(bits, 0); // level GUID (0 -> not valid, returns early)
        bits.push(false); // location: hasValue = false
        bits.push(false); // rotation: hasComponent = false
        bits.push(false); // scale: hasValue = false
        bits.push(false); // velocity: hasValue = false (unconditional read)
    }

    /// The controller's opening bunch carries nine bits between the spawn
    /// block and the first content-block header: the velocity bit (read
    /// unconditionally) and the net-player-index byte (consumed because the
    /// path cache resolves the archetype). Missing either misframes the header
    /// and the property block (`PlayerState`, `SpawnLocation`) is never walked
    /// (docs/archive/PROJECT_STATUS.md 17-A). Either half alone destroyed seven
    /// real subobject rows in the original experiment, so both fire together.
    #[test]
    fn controller_property_block_is_reached() {
        let mut payload: Vec<bool> = Vec::new();

        // Actor GUID 2: dynamic (even, non-zero), so a spawn block follows.
        write_int_packed(&mut payload, 2);
        // Spawn data. Archetype GUID 9 is what the cache will resolve.
        write_minimal_spawn_data(&mut payload, 9);
        // Net-player-index byte (value 0), consumed only if path_for_guid
        // answers.
        payload.extend(std::iter::repeat_n(false, 8));
        // The actor's own RepLayout block.
        payload.push(true); // hasRepLayout
        payload.push(true); // isActor
        write_int_packed(&mut payload, 0); // contentBits = 0

        let packet = build_open_bunch_packet(2, &payload);

        let mut sink = TestSink::default();
        // The cache knows the archetype path from exports that arrived before
        // this bunch, which a set of `register_path` calls would miss.
        sink.guid_paths
            .insert(9, "Default__BaseReplayController_C".to_string());

        let mut reader = ReplicationReader::new("++Ares-Core+release-13.01").unwrap();
        reader.process_packet(&packet, 0, &mut sink);

        let stats = reader.stats();
        assert_eq!(stats.actor_opens, 1);
        assert_eq!(stats.skipped_bits, 0, "nothing may be abandoned");
        assert_eq!(
            sink.content_blocks.len(),
            1,
            "exactly one content block -- the property block"
        );
        let block = &sink.content_blocks[0];
        assert!(
            block.has_rep_layout,
            "the block must be RepLayout (the property group), not ClassNetCache"
        );
        assert!(
            block.is_actor,
            "the block must describe the actor itself, not a subobject"
        );
    }

    /// The 12.01--12.06 controller, BaseJanusController, has its player-index
    /// byte consumed too, like BaseReplayController from 12.07 on.
    #[test]
    fn legacy_controller_property_block_is_reached() {
        let mut payload = Vec::new();
        write_int_packed(&mut payload, 2);
        write_minimal_spawn_data(&mut payload, 9);
        payload.extend([false; 8]); // Net player index, observed before the header.
        payload.extend([true, true]); // Actor RepLayout block.
        write_int_packed(&mut payload, 0);
        let packet = build_open_bunch_packet(2, &payload);

        let mut sink = TestSink::default();
        sink.guid_paths
            .insert(9, "Default__BaseJanusController_C".to_string());
        let mut reader = ReplicationReader::new("++Ares-Core+release-12.01").unwrap();
        reader.process_packet(&packet, 0, &mut sink);

        assert_eq!(reader.stats().skipped_bits, 0);
        assert_eq!(sink.content_blocks.len(), 1);
        assert!(sink.content_blocks[0].has_rep_layout);
        assert!(sink.content_blocks[0].is_actor);
    }

    /// A non-controller dynamic actor has no net-player-index byte: the header
    /// follows the spawn data directly, and an unconditional byte read would
    /// eat its first eight bits.
    #[test]
    fn non_controller_dynamic_actor_skips_net_player_index_byte() {
        let mut payload: Vec<bool> = Vec::new();
        write_int_packed(&mut payload, 2); // actor GUID 2 (dynamic)
        write_minimal_spawn_data(&mut payload, 9); // archetype 9, no path in cache
        // No net-player-index byte: this actor is not a controller.
        payload.push(true); // hasRepLayout
        payload.push(true); // isActor
        write_int_packed(&mut payload, 0); // contentBits = 0

        let packet = build_open_bunch_packet(2, &payload);

        let mut sink = TestSink::default();
        // Empty guid_paths: no GUID resolves, so no channel is a controller.
        let mut reader = ReplicationReader::new("++Ares-Core+release-13.01").unwrap();
        reader.process_packet(&packet, 0, &mut sink);

        assert_eq!(reader.stats().actor_opens, 1);
        assert_eq!(reader.stats().skipped_bits, 0);
        assert_eq!(sink.content_blocks.len(), 1);
        assert!(sink.content_blocks[0].has_rep_layout);
        assert!(sink.content_blocks[0].is_actor);
    }

    /// With the byte on the wire but no cache path, the byte is not consumed
    /// and the first header misframes, the 17-A failure: the block
    /// re-synchronises and goes down the ClassNetCache path instead of being
    /// lost. Same payload as the controller test; the cache is the difference.
    #[test]
    fn controller_byte_unconsumed_without_cache_path_misframes_header() {
        let mut payload: Vec<bool> = Vec::new();
        write_int_packed(&mut payload, 2); // actor GUID 2
        write_minimal_spawn_data(&mut payload, 9);
        // The byte IS on the wire (this is really a controller bunch)...
        payload.extend(std::iter::repeat_n(false, 8));
        payload.push(true); // hasRepLayout
        payload.push(true); // isActor
        write_int_packed(&mut payload, 0); // contentBits = 0

        let packet = build_open_bunch_packet(2, &payload);

        let mut sink = TestSink::default();
        // ...but the cache does not know the archetype path.
        let mut reader = ReplicationReader::new("++Ares-Core+release-13.01").unwrap();
        reader.process_packet(&packet, 0, &mut sink);

        // So the block that lands is not the actor's property block.
        let actor_blocks: Vec<_> = sink
            .content_blocks
            .iter()
            .filter(|b| b.is_actor && b.has_rep_layout)
            .collect();
        assert!(
            actor_blocks.is_empty(),
            "without the cache path the byte shifts the header, so no block is the actor's RepLayout"
        );
    }

    // --- partial reassembly has exactly one authority ---
    //
    // The packet reader keeps its own partial tracker. Were its
    // `has_partial_error` to reach the accumulator or the rejected-row
    // condition, any disagreement between the two would change what is
    // reassembled and preserved with `partial_errors` at 0. Each test below
    // is one such disagreement.

    /// A static actor open (GUID 3) followed by an empty actor block.
    fn open_and_empty_block() -> Vec<bool> {
        let mut bits = Vec::new();
        write_int_packed(&mut bits, 3);
        write_empty_actor_block(&mut bits);
        bits
    }

    /// The 8-bit payload `write_int_packed(3)` produces: the byte `6`.
    fn guid_three() -> Vec<bool> {
        let mut bits = Vec::new();
        write_int_packed(&mut bits, 3);
        bits
    }

    fn partial_packet(open: bool, initial: bool, last: bool, payload: &[bool]) -> Vec<u8> {
        build_bunch_packet(
            &BunchSpec {
                ch_index: 2,
                b_open: open,
                b_partial: true,
                b_partial_initial: initial,
                b_partial_final: last,
                ..Default::default()
            },
            payload,
        )
    }

    fn run_packets(packets: &[Vec<u8>]) -> (ReplicationReader, TestSink) {
        let mut reader = ReplicationReader::new("++Ares-Core+release-13.01").unwrap();
        let mut sink = TestSink::default();
        for (packet_id, packet) in packets.iter().enumerate() {
            reader.process_packet(packet, packet_id as i32, &mut sink);
        }
        (reader, sink)
    }

    fn rejected_rows(sink: &TestSink) -> Vec<(&'static str, PartialPayloadReason, usize)> {
        sink.rejected_partials
            .iter()
            .map(|row| (row.kind, row.reason, row.bit_count))
            .collect()
    }

    fn rejected_bits(sink: &TestSink) -> u64 {
        sink.rejected_partials
            .iter()
            .map(|row| row.bit_count as u64)
            .sum()
    }

    /// A partial that is both initial and final is a whole bunch every time,
    /// and frames exactly as the same payloads sent unfragmented.
    #[test]
    fn an_initial_final_partial_is_a_whole_bunch_every_time() {
        let mut second = Vec::new();
        write_empty_actor_block(&mut second);
        let (reader, sink) = run_packets(&[
            partial_packet(true, true, true, &open_and_empty_block()),
            partial_packet(false, true, true, &second),
        ]);

        // The same two payloads sent unfragmented are the reference.
        let plain = |open: bool, payload: &[bool]| {
            build_bunch_packet(
                &BunchSpec {
                    ch_index: 2,
                    b_open: open,
                    ..Default::default()
                },
                payload,
            )
        };
        let (reference, reference_sink) =
            run_packets(&[plain(true, &open_and_empty_block()), plain(false, &second)]);
        assert_eq!(
            reference_sink.content_blocks.len(),
            2,
            "the reference must itself frame both bunches, or the comparison proves nothing"
        );

        let stats = reader.stats();
        assert_eq!(stats.partial_completed, 2, "both whole bunches complete");
        assert_eq!(stats.partial_errors, 0);
        assert!(
            sink.rejected_partials.is_empty(),
            "nothing was rejected: {:?}",
            rejected_rows(&sink)
        );
        assert_eq!(
            sink.content_blocks.len(),
            reference_sink.content_blocks.len()
        );
        assert_eq!(stats.actor_opens, reference.stats().actor_opens);
        assert_eq!(stats.skipped_bits, reference.stats().skipped_bits);
    }

    /// An unaligned continuation is rejected by the accumulator alone (the
    /// packet reader keeps its assembly): a whole bunch after it is still
    /// processed, and the rejected bits preserved exactly once.
    #[test]
    fn a_whole_bunch_after_an_unaligned_fragment_is_still_processed() {
        let (reader, sink) = run_packets(&[
            partial_packet(true, true, false, &guid_three()),
            partial_packet(false, false, false, &[true; 5]),
            partial_packet(true, true, true, &open_and_empty_block()),
        ]);

        let stats = reader.stats();
        assert_eq!(stats.actor_opens, 1, "the whole bunch opens its actor");
        assert_eq!(sink.content_blocks.len(), 1);
        assert_eq!(stats.partial_completed, 1);
        assert_eq!(
            stats.partial_errors, 1,
            "only the unaligned continuation is an error"
        );
        assert_eq!(stats.partial_non_byte_aligned, 1);
        assert_eq!(stats.partial_overlapping_initial, 0);
        assert_eq!(
            rejected_rows(&sink),
            vec![
                (
                    "accumulated_payload",
                    PartialPayloadReason::NonByteAlignedFragment,
                    8
                ),
                (
                    "current_fragment",
                    PartialPayloadReason::NonByteAlignedFragment,
                    5
                ),
            ]
        );
        assert_eq!(
            sink.rejected_partials[0].payload,
            vec![6],
            "the displaced initial keeps its bytes"
        );
        assert_eq!(
            rejected_bits(&sink),
            stats.skipped_bits,
            "every rejected bit is preserved once"
        );
    }

    /// The initial that restarts a channel after an unaligned rejection is
    /// reassembled, and must not also be a rejected row: its bits would be in
    /// both the decoded stream and the preservation table.
    #[test]
    fn a_reassembled_fragment_is_never_also_a_rejected_row() {
        let mut block = Vec::new();
        write_empty_actor_block(&mut block);
        let (reader, sink) = run_packets(&[
            partial_packet(true, true, false, &guid_three()),
            partial_packet(false, false, false, &[true; 5]),
            partial_packet(true, true, false, &guid_three()),
            partial_packet(false, false, true, &block),
        ]);

        let stats = reader.stats();
        assert_eq!(stats.partial_completed, 1);
        assert_eq!(stats.actor_opens, 1);
        assert_eq!(sink.content_blocks.len(), 1);
        assert_eq!(stats.partial_overlapping_initial, 0);
        assert_eq!(
            rejected_rows(&sink),
            vec![
                (
                    "accumulated_payload",
                    PartialPayloadReason::NonByteAlignedFragment,
                    8
                ),
                (
                    "current_fragment",
                    PartialPayloadReason::NonByteAlignedFragment,
                    5
                ),
            ]
        );
        assert_eq!(rejected_bits(&sink), stats.skipped_bits);
    }

    /// A zero-bit final that overlaps an assembly is an error, not a
    /// completion, and leaves no complete-but-untaken state behind.
    #[test]
    fn a_zero_bit_overlapping_final_is_not_a_completion() {
        let (reader, sink) = run_packets(&[
            partial_packet(true, true, false, &guid_three()),
            partial_packet(false, true, true, &[]),
        ]);

        let stats = reader.stats();
        assert_eq!(stats.partial_completed, 0);
        assert_eq!(stats.partial_errors, 1);
        assert_eq!(stats.partial_overlapping_initial, 1);
        assert_eq!(
            reader.accumulator.active_count(),
            0,
            "no complete-but-untaken state may linger"
        );
        assert_eq!(
            rejected_rows(&sink),
            vec![(
                "accumulated_payload",
                PartialPayloadReason::OverlappingInitial,
                8
            )]
        );
        assert_eq!(rejected_bits(&sink), stats.skipped_bits);
    }

    /// A refused initial fragment is one rejected row: the empty assembly
    /// started for it is not also a 0-bit `accumulated_payload` row.
    #[test]
    fn a_refused_initial_fragment_is_one_rejected_row() {
        let (reader, sink) = run_packets(&[partial_packet(false, true, false, &[true; 5])]);

        let stats = reader.stats();
        assert_eq!(stats.partial_errors, 1);
        assert_eq!(stats.partial_non_byte_aligned, 1);
        assert_eq!(
            rejected_rows(&sink),
            vec![(
                "current_fragment",
                PartialPayloadReason::NonByteAlignedFragment,
                5
            )]
        );
        assert_eq!(stats.skipped_bits, 5);
        assert_eq!(rejected_bits(&sink), stats.skipped_bits);
        assert_eq!(
            reader.accumulator.active_count(),
            0,
            "the refused initial leaves no assembly behind"
        );

        // Over a buffered assembly, the assembly it replaced is still a row,
        // under the cause that displaced it; only the empty one is not.
        let (reader, sink) = run_packets(&[
            partial_packet(true, true, false, &guid_three()),
            partial_packet(false, true, false, &[true; 5]),
        ]);
        assert_eq!(
            rejected_rows(&sink),
            vec![
                (
                    "accumulated_payload",
                    PartialPayloadReason::OverlappingInitial,
                    8
                ),
                (
                    "current_fragment",
                    PartialPayloadReason::NonByteAlignedFragment,
                    5
                ),
            ]
        );
        assert_eq!(sink.rejected_partials[0].payload, vec![6]);
        assert_eq!(rejected_bits(&sink), reader.stats().skipped_bits);
    }

    // --- the preservation hand-off itself ---
    //
    // The accumulator's tests cover what it displaces, not the step after:
    // pipeline -> `on_rejected_partial`. Five simultaneous mutations of that
    // step (rows not written, payloads emptied, three reasons relabelled,
    // end-of-stream counters zeroed) once left every workspace test green;
    // each test below pins one hand-off by its bytes, reason and counters.

    /// `finish_with_sink` is what the driver calls, not `finish`. It must count
    /// the unfinished assembly and hand the sink its exact bytes, once.
    #[test]
    fn finish_with_sink_preserves_an_unfinished_assembly_exactly() {
        let (mut reader, mut sink) =
            run_packets(&[partial_packet(true, true, false, &guid_three())]);
        reader.finish_with_sink(&mut sink);

        let stats = reader.stats();
        assert_eq!(
            (stats.unfinished_partials, stats.unfinished_partial_bits),
            (1, 8)
        );
        assert_eq!(
            rejected_rows(&sink),
            vec![("accumulated_payload", PartialPayloadReason::EndOfStream, 8)]
        );
        let row = &sink.rejected_partials[0];
        assert_eq!(row.payload, vec![6]);
        assert_eq!(row.rejection_packet_id, None);
        assert_eq!(
            row.header.packet_id, 0,
            "the row names the fragment's packet"
        );

        reader.finish_with_sink(&mut sink);
        assert_eq!(
            sink.rejected_partials.len(),
            1,
            "a second call preserves nothing twice"
        );
    }

    /// A partial refused at the channel-state guard keeps its own reason (not
    /// a missing initial) and still counts in `partial_bunches`, although it
    /// returns before `process_bunch`, where the others are counted.
    #[test]
    fn a_partial_refused_for_channel_state_keeps_that_reason() {
        let mut reader = ReplicationReader::new("++Ares-Core+release-13.01").unwrap();
        reader.packet_reader = RawPacketReader::with_max_channels(0);
        let mut sink = TestSink::default();
        reader.process_packet(
            &partial_packet(false, true, false, &guid_three()),
            0,
            &mut sink,
        );

        assert_eq!(reader.stats().channel_state_limit_failures, 1);
        assert_eq!(
            reader.stats().partial_bunches,
            1,
            "a partial refused at the channel-state guard was still attempted"
        );
        assert_eq!(
            rejected_rows(&sink),
            vec![(
                "current_fragment",
                PartialPayloadReason::ChannelStateLimit,
                8
            )]
        );
        assert_eq!(sink.rejected_partials[0].payload, vec![6]);
        assert_eq!(reader.stats().skipped_bits, 8);
    }

    /// Closing a channel while a partial is buffered preserves the buffered
    /// bytes under `ChannelClosed`, charged to the closing packet.
    #[test]
    fn closing_a_channel_mid_partial_preserves_the_buffered_payload() {
        let close = build_bunch_packet(
            &BunchSpec {
                ch_index: 2,
                b_close: true,
                ..Default::default()
            },
            &[],
        );
        let (reader, sink) =
            run_packets(&[partial_packet(true, true, false, &guid_three()), close]);

        let stats = reader.stats();
        assert_eq!(stats.partial_channel_close, 1);
        assert_eq!(stats.partial_errors, 1);
        assert_eq!(
            rejected_rows(&sink),
            vec![(
                "accumulated_payload",
                PartialPayloadReason::ChannelClosed,
                8
            )]
        );
        assert_eq!(sink.rejected_partials[0].payload, vec![6]);
        assert_eq!(sink.rejected_partials[0].rejection_packet_id, Some(1));
        assert_eq!(stats.skipped_bits, 8);
    }
}
