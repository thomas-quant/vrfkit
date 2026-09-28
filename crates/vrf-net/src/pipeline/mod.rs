//! Top-level replication reader that drives the full pipeline.
//!
//! This module connects packets -> bunches -> content blocks -> fields into a
//! single pass. The caller provides:
//!
//! - A [`ReplicationSink`] to receive decoded events (fields, RPCs, actor
//!   lifecycle, etc.)
//! - A replay branch string for payload transform selection
//!
//! Layout: channel (open/close, GUID preambles), spawn (dynamic-actor spawn block), framing (content blocks, fields, RPCs) -- measured rates in docs/PERFORMANCE_NOTES.md#measured-rates-reference-replay-02d4d478.
//!
//! The public surface (this file) is deliberately thin: the sink trait, the
//! types it exchanges, and the packet-level driver. The three stages below it
//! live in their own modules so each can be read against the wire format it
//! implements.
//!
//! Channel-table growth rate on the reference replay: docs/PERFORMANCE_NOTES.md#allocation-strategy.
//!
//! The steady state of this reader allocates nothing per packet, per bunch or
//! per content block. Three buffers are owned by the reader and reused for the
//! whole replay:
//!
//! - `scratch` holds one decoded content-block payload;
//! - `fragment_stage` holds one partial-bunch fragment, byte-aligned;
//! - the channel table grows once per distinct channel index and never per
//!   bunch.
//!
//! Bunch payloads are *views* into the caller's packet bytes:
//! `RawPacketReader` hands the framing loop a sub-reader, and content blocks
//! and fields are sub-readers of that. An earlier version copied every bunch
//! payload into a fresh `Vec<u8>` before processing it, to satisfy a borrow
//! that a field split solves instead; see [`ReplicationReader::process_packet`].

mod channel;
mod framing;
mod spawn;

use vrf_bitio::BitReader;
use vrf_transform::TransformVersion;

use crate::bunch::{PartialBunchAccumulator, RawBunchHeader};
use crate::content::ContentBlockHeader;
use crate::error::Result;
use crate::field::FieldSink;
use crate::net_guid::GuidPathSink;
use crate::packet::RawPacketReader;
use crate::stats::NetStats;
use crate::types::MAX_ACTIVE_CHANNELS;
use crate::types::NetworkGuid;

use std::collections::HashMap;

use framing::BunchContext;

/// Per-channel actor state tracked during replication.
#[derive(Debug, Clone)]
pub struct ActorChannelState {
    /// Channel index.
    pub channel_index: u32,
    /// Whether the channel is currently open.
    pub is_open: bool,
    /// Whether the channel is dormant (closed but actor alive).
    /// Set on open and on close, and deliberately part of the public snapshot
    /// even though this crate's own sink reads `header.b_dormant` directly at
    /// the close callback instead. Dormancy is not destruction -- only a
    /// non-dormant close is a despawn -- so a consumer reconstructing actor
    /// lifetimes from `ActorChannelState` needs it without re-deriving it from
    /// the bunch header.
    pub is_dormant: bool,
    /// Actor's network GUID.
    pub actor_net_guid: NetworkGuid,
    /// Archetype GUID (for dynamic actors).
    pub archetype_net_guid: NetworkGuid,
    /// Level GUID.
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
/// Purely diagnostic: nothing on a decode, success or verdict path reads it.
/// It exists so an aggregate keyed on group path can also separate the distinct
/// shapes that all land in the same `field_stream_failures` /
/// `rpc_stream_failures` counters today -- in particular an unresolved
/// ClassNetCache group (whose payload the sink preserves whole) from a stream
/// that genuinely lost structure, which `NetStats::lost_content_blocks`
/// already distinguishes for the totals but no per-group view could before.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StreamFailureCause {
    /// The walk returned `Ok` but abandoned bits mid-block: a record declared
    /// more payload than the block had left, or an RPC record was too short to
    /// carry its length. Records emitted before the break are good; only the
    /// abandoned tail is lost.
    AbandonedTail,
    /// The walk returned `Err` -- a read inside the block ran off the end (or
    /// otherwise failed) and the rest of the block is unframeable.
    ReadError,
    /// The ClassNetCache function count was 0: the group could not be resolved,
    /// so the handle width is unknown. The sink received the whole decoded
    /// payload through `on_unresolved_class_net_cache_payload`, so this shape
    /// is preserved, not lost.
    UnresolvedFunctionCount,
    /// A valid post-RepLayout bit window was retained whole because its source
    /// provenance or strict ClassNetCache shape was not verified. This names
    /// uncertainty rather than asserting that a read failed or a descriptor
    /// count was unavailable.
    UnverifiedRepLayoutTail,
    /// The decoded payload could not be opened as a bit window at all. The
    /// scratch buffer is sized by the same bit count, so this has never been
    /// observed; it is named rather than folded into [`StreamFailureCause::ReadError`]
    /// so that a future grammar change that does trigger it is countable on
    /// its own.
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
/// could not be walked.
///
/// Reported to the sink rather than only counted because the layer that knows
/// *names* is the sink: it resolved the group path and the function count. A
/// bare counter says "one block failed"; this says which class, which is what a
/// new game build's investigation actually needs.
#[derive(Debug, Clone, Copy)]
pub struct StreamFailure {
    /// Which grammar was being parsed.
    pub kind: StreamKind,
    /// Actor whose channel carried the block.
    pub actor_net_guid: NetworkGuid,
    /// Declared payload length of the block.
    pub bit_count: u32,
    /// Function count used for the handle read (`Rpc` only; 0 for `RepLayout`).
    ///
    /// Worth reporting because a wrong non-zero value can select the wrong
    /// serialized-int width and desynchronise the stream. The RPC parser clamps
    /// counts to at least 2, so declared counts 1 and 2 use the same wire width;
    /// zero remains the explicit unresolved-group sentinel.
    pub function_count: u32,
    /// Bits consumed before the failure.
    pub consumed_bits: u64,
    /// Bits abandoned as a result.
    pub remaining_bits: u64,
    /// Which stage of the walk failed. See [`StreamFailureCause`].
    pub cause: StreamFailureCause,
    /// Handle of the non-terminator record the walk was inside, when its handle
    /// read succeeded. `None` for a failed handle read and for an early zero
    /// terminator, so the previous successful field is never misidentified as
    /// the failure. `record_offset` is exact in every case.
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

/// Trait for receiving all replication events.
///
/// The caller implements this to process fields, RPCs, and actor lifecycle
/// without any data being silently discarded.
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

    /// Optional raw diagnostic sample for a chained tail that was not decoded.
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
    ///
    /// Defaulted to a no-op so a sink that does not care about failure context
    /// need not implement it; the counters in [`NetStats`] are maintained either
    /// way. Override it to attach the names the sink holds -- the resolved group
    /// path in particular, which the replication layer does not know.
    fn on_stream_failure(&mut self, _failure: StreamFailure) {}

    /// The decoded payload of a block whose inner stream could not be walked.
    ///
    /// Follows [`Self::on_stream_failure`] for the same block whenever the
    /// decoded bytes exist -- not for a window that failed to open, and not
    /// for unresolved ClassNetCache blocks, which deliver theirs through
    /// [`Self::on_unresolved_class_net_cache_payload`] instead. Diagnostics
    /// only, failure paths only, so a sink may keep a bounded sample of the
    /// bytes that actually failed to walk; default no-op.
    fn on_stream_failure_payload(&mut self, _failure: StreamFailure, _payload: &[u8]) {}

    /// Preserve one whole decoded ClassNetCache payload whose function table
    /// could not be resolved.
    ///
    /// This is a block-level data event, not a fabricated RPC. `payload` has
    /// exactly `ceil(failure.bit_count / 8)` bytes, with unused high bits in
    /// the final byte cleared. The parser has consumed no payload bits.
    fn on_unresolved_class_net_cache_payload(&mut self, _failure: StreamFailure, _payload: &[u8]) {}
}

/// Leaf asset name of the VALORANT replay controller.
///
/// This is the only `PlayerController`-kind actor in VALORANT replays, and the
/// reference parser keys the net-player-index byte off it (see
/// `channel::is_player_controller_path`, which normalises the four spellings
/// the same asset arrives under).
pub const PLAYER_CONTROLLER_LEAF: &str = "BaseReplayController";

/// One channel's row in the table.
///
/// The bunch counter used to live in a second `HashMap<u32, u64>` keyed by the
/// same channel index, so every bunch hashed that index twice. It cannot simply
/// move into [`ActorChannelState`]: a channel is counted from its first bunch,
/// which may be a close or a bunch for a channel that never opened, while only
/// an *open* bunch produces an `ActorChannelState`. Hence the `Option`.
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

/// Channel index -> channel row. 232 entries on the reference replay, looked up
/// once or twice per bunch.
type ChannelTable = HashMap<u32, ChannelSlot>;

/// Bytes the scratch buffer starts at. A bunch payload is capped at
/// `MAX_PACKET_SIZE_BITS` (16 384 bits = 2 048 bytes) and a content block
/// cannot exceed its bunch, so this is already above any block the wire can
/// declare; the `resize` in the decode path is a safety net, not a growth
/// strategy.
const SCRATCH_INITIAL_BYTES: usize = 4096;

/// The mutable state one bunch's worth of processing needs, gathered so the
/// stage functions take a handful of arguments instead of a dozen.
///
/// This is a borrow split of [`ReplicationReader`], made once per packet so the
/// bunch callback can drive the whole downstream pipeline inline.
struct Stage<'a> {
    stats: &'a mut NetStats,
    channels: &'a mut ChannelTable,
    transform: TransformVersion,
    scratch: &'a mut Vec<u8>,
}

/// Where a bunch sits in the stream. Copied rather than borrowed because all
/// three fields are scalars the caller already holds.
///
/// Every field exists to be written into a `DiagnosticEvent`, so without that
/// feature the whole struct is carried and never read. It is still threaded
/// through, rather than `cfg`-ed out of the signatures, so the hot path reads
/// the same in both builds; the values are dead and the optimiser drops them.
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
    /// Reusable byte-aligned staging buffer for partial-bunch fragments.
    ///
    /// A bunch payload arrives as a bit window at an arbitrary offset inside
    /// the packet, but [`PartialBunchAccumulator::add_fragment`] concatenates
    /// byte slices, so a fragment has to be realigned before it can be
    /// appended. That copy is unavoidable; allocating for it is not.
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

    /// Account for reassembly state the replay ended in the middle of.
    ///
    /// Call this once after the last packet. A partial bunch whose continuation
    /// never arrives sits in the accumulator until the reader is dropped, and
    /// until the stream stops there is nothing to distinguish it from a bunch
    /// still being reassembled -- so this is the only point at which the loss
    /// can be named. It lands in [`NetStats::unfinished_partials`] and
    /// [`NetStats::unfinished_partial_bits`]; `partial_errors` deliberately does
    /// not move, because nothing was out of sequence.
    ///
    /// Idempotent: the accumulator is drained, so a second call counts nothing.
    pub fn finish_with_sink(&mut self, sink: &mut dyn ReplicationSink) {
        for partial in self.accumulator.drain_unfinished() {
            self.stats.unfinished_partials += 1;
            self.stats.unfinished_partial_bits += partial.bit_count as u64;
            sink.on_rejected_partial(RejectedPartialFragment {
                header: &partial.header,
                payload_kind: "accumulated_payload",
                reason: PartialPayloadReason::EndOfStream,
                bit_count: partial.bit_count,
                payload: &partial.buffer,
                rejection_packet_id: None,
            });
        }
    }

    /// Account for unfinished partials without a preservation consumer.
    pub fn finish(&mut self) {
        for partial in self.accumulator.drain_unfinished() {
            self.stats.unfinished_partials += 1;
            self.stats.unfinished_partial_bits += partial.bit_count as u64;
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

        if packet_data.is_empty() {
            return;
        }

        if packet_data[packet_data.len() - 1] == 0 {
            self.stats.malformed_packets += 1;
            return;
        }

        // Why inline beat two phases, and what the old copies cost on the reference replay: docs/PERFORMANCE_NOTES.md#packet-processing-is-interleaved.
        //
        // Bunches are processed inline, inside the packet reader's callback.
        //
        // This used to be two phases: parse every bunch header, copying each
        // payload into a fresh `Vec<u8>`, and only then walk the copies. The
        // stated reason was that "the callback borrows self". It does not have
        // to. Destructuring `self` here gives the callback `&mut` handles to
        // the fields it needs while `packet_reader` stays borrowed by
        // `read_packet`, which the borrow checker accepts because the fields
        // are disjoint.
        //
        // Interleaving is safe because the two phases touch disjoint state:
        // header parsing mutates only `packet_reader` (partial tracking and the
        // reliable sequence), while payload processing mutates only the fields
        // below. `malformed_packets` is bumped between the phases but summed
        // after the loop -- a u64 nothing reads mid-stream, so its total is
        // unchanged. (`partial_errors` is owned by the reassembly accumulator's
        // `validate_sequence` in `process_bunch`; the reader's parallel partial
        // tracker no longer adds its count here -- doing both counted every
        // error twice.)
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

            // The global index is the value *before* the increment and the
            // per-channel one the value *after*; both feed `DiagnosticEvent`
            // and the asymmetry is what the fields have always meant.
            let global_index = *global_bunch_index;
            *global_bunch_index += 1;
            if header.has_channel_limit_error
                || (!stage.channels.contains_key(&header.ch_index)
                    && stage.channels.len() >= MAX_ACTIVE_CHANNELS)
            {
                stage.stats.channel_state_limit_failures += 1;
                if header.b_partial {
                    let bit_count = payload.bits_remaining() as usize;
                    let byte_count = stage_fragment(payload.clone(), fragment_stage);
                    sink.on_rejected_partial(RejectedPartialFragment {
                        header,
                        payload_kind: "current_fragment",
                        reason: PartialPayloadReason::ChannelStateLimit,
                        bit_count,
                        payload: &fragment_stage[..byte_count],
                        rejection_packet_id: Some(header.packet_id),
                    });
                }
                Self::abandon_bunch(&mut payload.clone(), &mut stage);
                // A refused open is an open that did not complete. Retired
                // before the close, so an open+close bunch closes nothing it
                // did not open.
                Self::retire_after_failed_open(header, &mut stage);
                if header.b_close {
                    Self::close_channel(header, &mut stage, accumulator, sink);
                }
                bunch_index_in_packet += 1;
                return;
            }
            let slot = stage.channels.entry(header.ch_index).or_default();
            slot.bunch_count += 1;
            let ids = BunchIds {
                bunch_index_in_packet,
                global_bunch_index: global_index,
                channel_bunch_index: slot.bunch_count,
            };
            bunch_index_in_packet += 1;

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
        // `partial_errors` is counted by `validate_sequence` in `process_bunch`
        // (the reassembly authority). The reader's own `partial_error_count` is
        // NOT summed here -- doing both counted every partial error twice.
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
            let byte_count = stage_fragment(payload, fragment_stage);

            // The accumulator is the one reassembly authority. The packet
            // reader's own tracker can disagree with it -- it keeps an assembly
            // the accumulator has already refused (an unaligned or over-budget
            // fragment), and it used to keep one for every initial that was also
            // final -- and its verdict rode in on `has_partial_error`. That flag
            // then vetoed a valid completion and wrote a "rejected" row for bits
            // that were reassembled anyway, while `partial_errors`, which only
            // the accumulator counts, stayed at zero: a complete bunch dropped
            // with every cause counter reading 0. The accumulator gets a header
            // carrying none of the reader's partial verdicts. The tracker itself
            // stays, as published API for direct `read_packet` callers (see
            // `RawPacketReader`'s doc); this strip is what keeps it advisory.
            let mut fragment_header = header.clone();
            fragment_header.has_partial_error = false;
            fragment_header.partial_error_kind = None;
            fragment_header.is_partial_completed = false;

            let result = accumulator.add_fragment(
                ch_index,
                fragment_header,
                &fragment_stage[..byte_count],
                bit_count as usize,
                &mut stage.stats.partial_errors,
                &mut stage.stats.partial_fragments,
                &mut stage.stats.partial_completed,
            );
            *header = result.header;

            let reason = result
                .error_kind
                .map(reason_for_sequence_kind)
                .or_else(|| result.resource_limit.map(reason_for_resource_limit));
            for (displaced, discard_cause) in &result.displaced {
                let displaced_reason = match discard_cause {
                    crate::bunch::PartialDiscardCause::Sequence(kind) => {
                        reason_for_sequence_kind(*kind)
                    }
                    crate::bunch::PartialDiscardCause::Resource(limit) => {
                        reason_for_resource_limit(*limit)
                    }
                };
                sink.on_rejected_partial(RejectedPartialFragment {
                    header: &displaced.header,
                    payload_kind: "accumulated_payload",
                    reason: displaced_reason,
                    bit_count: displaced.bit_count,
                    payload: &displaced.buffer,
                    rejection_packet_id: Some(header.packet_id),
                });
            }
            // A current-fragment row is written only for a fragment the
            // accumulator refused, under the cause it named. An overlapping
            // initial is not refused: it replaced the old assembly (preserved
            // above as a displaced payload) and is itself buffered, so it writes
            // no row here. There is deliberately no fallback cause -- a row
            // labelled with a cause no counter recorded is how the same bits
            // used to reach the table twice.
            if let Some(reason) = reason.filter(|reason| {
                !result.should_process && *reason != PartialPayloadReason::OverlappingInitial
            }) {
                sink.on_rejected_partial(RejectedPartialFragment {
                    header,
                    payload_kind: "current_fragment",
                    reason,
                    bit_count: bit_count as usize,
                    payload: &fragment_stage[..byte_count],
                    rejection_packet_id: Some(header.packet_id),
                });
            }

            if result.overlapping_initial {
                stage.stats.partial_overlapping_initial += 1;
            }
            match result.error_kind {
                Some(crate::error::PartialSequenceKind::MissingInitial) => {
                    stage.stats.partial_missing_initial += 1;
                    stage.stats.partial_missing_initial_bits += bit_count;
                    if header.b_partial_final {
                        stage.stats.partial_missing_initial_final += 1;
                    }
                    if header.b_reliable {
                        stage.stats.partial_missing_initial_reliable += 1;
                    }
                }
                Some(crate::error::PartialSequenceKind::OverlappingInitial) => {}
                Some(crate::error::PartialSequenceKind::MismatchedContinuation) => {
                    stage.stats.partial_mismatched_continuation += 1;
                }
                Some(crate::error::PartialSequenceKind::NonByteAlignedFragment) => {
                    stage.stats.partial_non_byte_aligned += 1;
                }
                None => {}
            }

            stage.stats.skipped_bits += result.discarded_bits as u64;
            if result.resource_limit.is_some() {
                stage.stats.partial_resource_limit_failures += 1;
            }

            if !result.should_process {
                if header.b_close {
                    Self::close_channel(header, stage, accumulator, sink);
                }
                return;
            }

            // Take completed payload
            if let Some((buf, total_bits, stored_header)) = accumulator.take_completed(ch_index) {
                let Ok(mut payload_reader) = BitReader::with_bit_len(&buf, total_bits as u64)
                else {
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

            // The close flag belongs to the FINAL fragment, not to the initial
            // one `stored_header` came from: Unreal's `UChannel::SendBunch` puts
            // `bOpen` on the first fragment and `bClose` on the last, and
            // `ReceivedNextBunch` copies the last one's close flags onto the
            // reassembled bunch. This branch used to return before ever reaching
            // `handle_channel_close`, so a partial bunch could not close a
            // channel at all: the actor stayed open for the rest of the replay,
            // its close row was never emitted, and `actor_closes` never moved.
            if header.b_close {
                Self::close_channel(header, stage, accumulator, sink);
            }
            return;
        }

        // Non-partial: process directly
        if bit_count == 0 {
            // Handle close
            if header.b_close {
                Self::close_channel(header, stage, accumulator, sink);
            }
            return;
        }

        let mut payload = payload;
        Self::process_complete_payload(header, &mut payload, stage, sink, ids);

        if header.b_close {
            Self::close_channel(header, stage, accumulator, sink);
        }
    }

    /// Close a channel and retire any reassembly state it was holding.
    ///
    /// `bClose` always names both steps together: closing the channel without
    /// also retiring its accumulator entry leaves a partial assembly that can
    /// never complete (when the channel is destroyed rather than dormant) but
    /// is never counted as lost either. Every `if header.b_close` site in this
    /// module calls both, in this order, so they are given one name.
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
            sink.on_rejected_partial(RejectedPartialFragment {
                header: &discarded.header,
                payload_kind: "accumulated_payload",
                reason: PartialPayloadReason::ChannelClosed,
                bit_count: discarded.bit_count,
                payload: &discarded.buffer,
                rejection_packet_id: Some(header.packet_id),
            });
        }
    }

    /// Count a bunch-header failure and the payload bits it abandons.
    ///
    /// All three header stages -- package-map exports, must-be-mapped GUIDs and
    /// the channel open -- leave the reader at an indeterminate bit when they
    /// fail, so the rest of the bunch cannot be framed. Only
    /// `bunch_header_failures` used to move: the abandoned bits appeared in no
    /// tally at all, which is what let an out-of-range GUID count drop a whole
    /// run of path declarations while every bit counter read zero.
    ///
    /// The whole window, not `bits_remaining()`, for the reason
    /// [`super::framing::abandoned_on_error`] already spells out: a failing
    /// `read_int_packed` consumes its chunks *before* discovering the value runs
    /// off the end, so a header stage that expires exactly at the payload end
    /// leaves `bits_remaining() == 0` and charged nothing for a bunch that lost
    /// every bit it had. That is the same undercount, at a different depth, and
    /// it read as a clean zero. `payload` is a sub-reader whose window IS this
    /// bunch's payload, so `len_bits()` is the loss.
    fn abandon_bunch(payload: &mut BitReader<'_>, stage: &mut Stage<'_>) {
        stage.stats.bunch_header_failures += 1;
        stage.stats.skipped_bits += payload.len_bits();
        payload.skip_remaining();
    }

    /// Take the channel away from the actor it held when an open bunch does not
    /// complete its open.
    ///
    /// `handle_channel_open` writes the new state only after the actor GUID and
    /// the spawn block have both read, so every way an open bunch can stop
    /// short of that leaves whatever the slot held: the bunch refused at the
    /// channel-state limit, a package-map or must-be-mapped read that fails
    /// before the open, the open itself failing, and a package-map export bunch
    /// whose exports read cleanly -- nothing after exports is read, so its open
    /// never is. Each of those arms calls this. When the slot held a live
    /// actor, the wire has just said the channel belongs to someone else and
    /// this reader cannot say who: keeping the old state framed every later
    /// bunch on the channel under the old actor's archetype and class. The
    /// state is cleared instead, which sends those bunches to
    /// [`Self::drop_unopened`], where they are counted.
    ///
    /// Only an open bunch displaces anything. A bunch without `b_open` that
    /// fails a header stage says nothing about who owns the channel, so its
    /// actor stays live; this returns without looking.
    ///
    /// No close is emitted for the displaced actor -- the same rule
    /// `channel_reopens_while_open` follows for a reopen that succeeds: the
    /// replay sent no close for it, and a fabricated one would be a row the
    /// wire never carried. A dormant or already-closed state is not live and
    /// is left as it is; bunches after it are dropped at the guard either way.
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

    /// Count and discard a bunch whose channel has no open actor.
    ///
    /// Only bits still unread after the bunch's preambles are dropped, so only
    /// those are counted: a bunch whose must-be-mapped list consumed its whole
    /// payload lost nothing. `bits_remaining()` is the right measure here, not
    /// the whole window [`Self::abandon_bunch`] charges: nothing failed to
    /// read, so the reader stands exactly where the unframed content begins.
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

        // Package map exports. Count the export only when the read succeeds --
        // a partial failure used to inflate `package_map_exports` anyway.
        //
        // The bunch ends here on both outcomes: nothing after the exports is
        // read, and nothing counts what is left (docs/FOLLOWUP.md says why).
        // An open this bunch carries is therefore never read, clean exports or
        // not, so the actor it displaces is retired on both paths. On the clean
        // one no header failure is counted, and `failed_reopens_while_open` is
        // the only counter that says an open was lost.
        if header.b_has_package_map_exports {
            if channel::read_package_map_exports(payload, stage.stats, sink).is_ok() {
                stage.stats.package_map_exports += 1;
            } else {
                Self::abandon_bunch(payload, stage);
            }
            Self::retire_after_failed_open(header, stage);
            return;
        }

        // Must-be-mapped GUIDs. A failure leaves the reader at an indeterminate
        // bit, so the rest of this bunch cannot be framed safely -- count it and
        // abandon the bunch rather than parse on as garbage. An open behind the
        // list is abandoned with it.
        if header.b_has_must_be_mapped_guids
            && channel::read_must_be_mapped_guids(payload, stage.stats).is_err()
        {
            Self::abandon_bunch(payload, stage);
            Self::retire_after_failed_open(header, stage);
            return;
        }

        // Actor channel open. A failed open writes no new state, and it used to
        // leave the old one alone as well: a channel still holding a live
        // actor kept it, and every later bunch there was framed as that actor's
        // -- the stale-schema shape, reported only as one header failure. See
        // `retire_after_failed_open`, which the two arms above and the
        // channel-state limit in `process_packet` call as well.
        if header.b_open
            && channel::handle_channel_open(header, payload, stage.channels, stage.stats, sink)
                .is_err()
        {
            Self::abandon_bunch(payload, stage);
            Self::retire_after_failed_open(header, stage);
            return;
        }

        // Look up channel -- if it never opened, or is not open now, the rest
        // of the bunch has no actor to be framed under and is dropped. Counted:
        // these two returns used to move nothing but `bunches`.
        let Some(ch) = stage.channels.get(&ch_index).and_then(|s| s.state.as_ref()) else {
            Self::drop_unopened(payload, stage);
            return;
        };
        let (actor_net_guid, is_open, archetype_net_guid) =
            (ch.actor_net_guid, ch.is_open, ch.archetype_net_guid);

        if !is_open {
            Self::drop_unopened(payload, stage);
            return;
        }

        // ReadNetPlayerIndex. The three cheap flags are tested before the path
        // lookup, which is the whole reason this reads as a nest rather than a
        // flat `&&` chain: `is_player_controller_channel` costs two NetGuidCache
        // lookups plus two path normalisations, and it used to run on every one
        // of the 530 401 bunches to answer a question that decides something for
        // about 2 000 of them. See [`channel::is_player_controller_channel`] for
        // what the byte is and what skipping it costs.
        if header.b_open
            && actor_net_guid.is_dynamic()
            && !payload.at_end()
            && channel::is_player_controller_channel(actor_net_guid, archetype_net_guid, sink)
        {
            let _ = payload.read_u8();
        }

        // Frame content blocks
        let ctx = BunchContext { header, ids };
        framing::frame_content_blocks(payload, ch_index, actor_net_guid, stage, sink, &ctx);
    }
}

/// Copy a partial bunch's payload into `buffer`, byte-aligned, and return how
/// many bytes it occupies.
///
/// [`PartialBunchAccumulator::add_fragment`] concatenates byte slices, but a
/// bunch payload is a bit window starting at an arbitrary offset inside the
/// packet, so a fragment must be realigned first. `buffer` is the reader's
/// long-lived staging area: it only ever grows, and the bytes past the returned
/// length belong to whatever fragment used it last. `copy_bits_to` rewrites
/// every byte of `[..byte_count]` -- including the padding bits of the final
/// one -- so the prefix the caller slices is never stale.
fn stage_fragment(payload: BitReader<'_>, buffer: &mut Vec<u8>) -> usize {
    let bit_count = payload.bits_remaining();
    let byte_count = (bit_count as usize).div_ceil(8);
    if buffer.len() < byte_count {
        buffer.resize(byte_count, 0);
    }
    if bit_count > 0 {
        let mut src = payload;
        let _ = src.copy_bits_to(buffer, bit_count);
    }
    byte_count
}

/// Map a partial-sequence error to the reason reported to the sink.
///
/// Shared by the current fragment's own rejection and by every earlier
/// assembly the same fragment displaced, so both name a discard with the
/// same [`PartialPayloadReason`] rather than drifting out of step.
fn reason_for_sequence_kind(kind: crate::error::PartialSequenceKind) -> PartialPayloadReason {
    match kind {
        crate::error::PartialSequenceKind::MissingInitial => PartialPayloadReason::MissingInitial,
        crate::error::PartialSequenceKind::OverlappingInitial => {
            PartialPayloadReason::OverlappingInitial
        }
        crate::error::PartialSequenceKind::MismatchedContinuation => {
            PartialPayloadReason::MismatchedContinuation
        }
        crate::error::PartialSequenceKind::NonByteAlignedFragment => {
            PartialPayloadReason::NonByteAlignedFragment
        }
    }
}

/// Map a partial-reassembly resource refusal to the reason reported to the
/// sink. See [`reason_for_sequence_kind`]: same sharing, same reason why.
fn reason_for_resource_limit(limit: crate::bunch::PartialResourceLimit) -> PartialPayloadReason {
    match limit {
        crate::bunch::PartialResourceLimit::ActiveStates => PartialPayloadReason::ActiveStateLimit,
        crate::bunch::PartialResourceLimit::BufferedBits => PartialPayloadReason::BufferedBitsLimit,
        crate::bunch::PartialResourceLimit::Allocation => PartialPayloadReason::AllocationFailure,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        opens: Vec<u32>,
        closes: Vec<u32>,
        paths: Vec<(u32, String)>,
        /// GUID-to-path map consulted by `path_for_guid`. Empty by default, so
        /// existing tests get the pre-cache behaviour (every lookup is `None`).
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
            self.unresolved_payloads.push((failure, payload.to_vec()));
        }
    }

    /// The staging buffer is reused for every partial fragment and only ever
    /// grows, so a short fragment following a long one must not be able to read
    /// the long one's bytes back. This is the failure mode the buffer reuse
    /// could have introduced; it is pinned rather than argued.
    #[test]
    fn fragment_staging_never_exposes_the_previous_fragment() {
        let mut buffer = Vec::new();

        let long = [0xFFu8; 4];
        let byte_count = stage_fragment(BitReader::with_bit_len(&long, 32).unwrap(), &mut buffer);
        assert_eq!(byte_count, 4);
        assert_eq!(&buffer[..byte_count], &[0xFF, 0xFF, 0xFF, 0xFF]);

        // Five zero bits: one byte, and the three padding bits above them must
        // be cleared even though the buffer still holds 0xFF underneath.
        let short = [0x00u8];
        let byte_count = stage_fragment(BitReader::with_bit_len(&short, 5).unwrap(), &mut buffer);
        assert_eq!(byte_count, 1);
        assert_eq!(&buffer[..byte_count], &[0x00]);
        assert_eq!(
            buffer.len(),
            4,
            "the buffer keeps its capacity, not its data"
        );

        // A zero-bit fragment stages nothing and must report zero bytes.
        assert_eq!(
            stage_fragment(BitReader::with_bit_len(&short, 0).unwrap(), &mut buffer,),
            0
        );
    }

    // --- packet builders, for the reassembly test below ---

    fn write_int_packed(bits: &mut Vec<bool>, mut value: u32) {
        loop {
            let mut next_byte = ((value & 0x7F) << 1) as u8;
            value >>= 7;
            if value != 0 {
                next_byte |= 1;
            }
            for i in 0..8 {
                bits.push((next_byte & (1 << i)) != 0);
            }
            if value == 0 {
                break;
            }
        }
    }

    fn write_serialized_int(bits: &mut Vec<bool>, value: u32, max_value: u32) {
        let mut written_value = 0u32;
        let mut mask = 1u32;
        while written_value.saturating_add(mask) < max_value {
            let bit = (value & mask) != 0;
            bits.push(bit);
            if bit {
                written_value |= mask;
            }
            mask <<= 1;
        }
    }

    fn build_packet(bits: &[bool]) -> Vec<u8> {
        let mut packet = vec![0u8; (bits.len() + 1).div_ceil(8)];
        for (i, &bit) in bits.iter().enumerate() {
            if bit {
                packet[i >> 3] |= 1 << (i & 7);
            }
        }
        packet[bits.len() >> 3] |= 1 << (bits.len() & 7);
        packet
    }

    /// A partial bunch split across two fragments must reassemble and then
    /// frame exactly as an unsplit one would.
    ///
    /// The reference replay completes zero partial bunches -- an instrumented
    /// run reported `partial_fragments = 0`, `partial_completed = 0` against
    /// 530 401 bunches -- so the oracle cannot see this path at all. It is the
    /// one place where a bunch payload is copied rather than viewed, which
    /// makes it exactly the path a rewrite of the copy could break silently.
    #[test]
    fn split_bunch_reassembles_and_frames() {
        // Fragment 1: control bunch, opens channel 2, partial initial,
        // reliable. Payload is IntPacked(3): a static (odd) actor GUID, so no
        // spawn block follows.
        let mut bits = vec![
            true,  // bControl
            true,  // bOpen
            false, // bClose
            false, // bIsReplicationPaused
            true,  // bReliable
        ];
        write_int_packed(&mut bits, 2); // ChIndex
        bits.extend_from_slice(&[
            false, // bHasPackageMapExports
            false, // bHasMustBeMappedGUIDs
            true,  // bPartial
            false, // VALORANT bit
            true,  // bPartialInitial
            false, // bPartialFinal
            true,  // FName isHardcoded
        ]);
        write_int_packed(&mut bits, 1); // FName index
        write_serialized_int(&mut bits, 8, crate::types::MAX_PACKET_SIZE_BITS);
        write_int_packed(&mut bits, 3); // payload: actor GUID 3

        // Fragment 2: partial final on the same channel. Payload is one actor
        // content block with a zero-bit body.
        bits.extend_from_slice(&[
            false, // bControl
            false, // bIsReplicationPaused
            true,  // bReliable
        ]);
        write_int_packed(&mut bits, 2); // ChIndex
        bits.extend_from_slice(&[
            false, // bHasPackageMapExports
            false, // bHasMustBeMappedGUIDs
            true,  // bPartial
            false, // VALORANT bit
            false, // bPartialInitial
            true,  // bPartialFinal
            true,  // FName isHardcoded
        ]);
        write_int_packed(&mut bits, 1); // FName index
        write_serialized_int(&mut bits, 10, crate::types::MAX_PACKET_SIZE_BITS);
        bits.extend_from_slice(&[
            true, // hasRepLayout
            true, // isActor
        ]);
        write_int_packed(&mut bits, 0); // contentBits = 0

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
    }

    /// A partial final with no preceding initial is one error, counted once.
    /// It used to be counted twice: once by the packet reader's parallel
    /// partial-state tracker (summed into `partial_errors` at packet end) and
    /// again by the reassembly accumulator's `validate_sequence`. The
    /// accumulator is the reassembly authority, so it owns the count.
    #[test]
    fn a_partial_final_without_an_initial_is_counted_once() {
        // One partial-final bunch on channel 2, reliable, with no initial
        // fragment anywhere. Both the reader and the accumulator detect the
        // missing initial; only the accumulator should count it.
        let mut bits = vec![
            false, // bControl
            false, // bIsReplicationPaused
            true,  // bReliable
        ];
        write_int_packed(&mut bits, 2); // ChIndex
        bits.extend_from_slice(&[
            false, // bHasPackageMapExports
            false, // bHasMustBeMappedGUIDs
            true,  // bPartial
            false, // VALORANT bit
            false, // bPartialInitial (no initial -> MissingInitial)
            true,  // bPartialFinal
            true,  // FName isHardcoded
        ]);
        write_int_packed(&mut bits, 1); // FName index
        write_serialized_int(&mut bits, 8, crate::types::MAX_PACKET_SIZE_BITS);
        write_int_packed(&mut bits, 3); // payload: actor GUID 3 (never reached)

        let packet = build_packet(&bits);
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

    /// A bunch whose header parse fails is counted and abandoned, not silently
    /// dropped. A truncated must-be-mapped GUID list (declares one GUID, carries
    /// none) used to be `let _ =`-ed: the reader was left stuck and the rest of
    /// the bunch (channel open, content framing) parsed garbage, with no counter
    /// moving.
    #[test]
    fn a_truncated_bunch_header_failure_is_counted_not_silent() {
        // Non-partial reliable bunch on channel 2 with a 16-bit payload: a u16
        // must-be-mapped count of 1 (LE) and then no GUID bits, so
        // read_must_be_mapped_guids EOFs on the missing GUID.
        let mut bits = vec![
            false, // bControl
            false, // bIsReplicationPaused
            true,  // bReliable
        ];
        write_int_packed(&mut bits, 2); // ChIndex
        bits.extend_from_slice(&[
            false, // bHasPackageMapExports
            true,  // bHasMustBeMappedGUIDs
            false, // bPartial
            false, // VALORANT bit
            true,  // FName isHardcoded
        ]);
        write_int_packed(&mut bits, 1); // FName index
        write_serialized_int(&mut bits, 16, crate::types::MAX_PACKET_SIZE_BITS); // 16 payload bits
        // payload: u16 count = 1 little-endian, then zero GUID bits.
        bits.extend_from_slice(&[
            true, false, false, false, false, false, false, false, // 0x01
            false, false, false, false, false, false, false, false, // 0x00
        ]);

        let packet = build_packet(&bits);
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

    /// The header flags one synthetic bunch varies. Everything not named here
    /// is fixed: reliable, and the hardcoded channel FName a reliable bunch
    /// always carries.
    #[derive(Default)]
    struct BunchSpec {
        ch_index: u32,
        b_open: bool,
        b_close: bool,
        /// Close reason Dormancy rather than Destroyed; only read with
        /// `b_close`.
        dormant: bool,
        b_has_package_map_exports: bool,
        b_has_must_be_mapped_guids: bool,
        b_partial: bool,
        b_partial_initial: bool,
        b_partial_final: bool,
    }

    /// Append one bunch (header + payload) in `parse_bunch_header` order.
    fn write_bunch(bits: &mut Vec<bool>, spec: &BunchSpec, payload_bits: &[bool]) {
        let b_control = spec.b_open || spec.b_close;
        bits.push(b_control);
        if b_control {
            bits.push(spec.b_open);
            bits.push(spec.b_close);
        }
        if spec.b_close {
            // Close reason Destroyed (0) or Dormancy (1), read as
            // SerializedInt(MAX).
            write_serialized_int(
                bits,
                u32::from(spec.dormant),
                crate::types::ChannelCloseReason::MAX,
            );
        }
        bits.push(false); // bIsReplicationPaused
        bits.push(true); // bReliable
        write_int_packed(bits, spec.ch_index);
        bits.push(spec.b_has_package_map_exports);
        bits.push(spec.b_has_must_be_mapped_guids);
        bits.push(spec.b_partial);
        bits.push(false); // VALORANT bit
        if spec.b_partial {
            bits.push(spec.b_partial_initial);
            bits.push(spec.b_partial_final);
        }
        bits.push(true); // channel FName: isHardcoded
        write_int_packed(bits, 1); // FName index
        write_serialized_int(
            bits,
            payload_bits.len() as u32,
            crate::types::MAX_PACKET_SIZE_BITS,
        );
        bits.extend_from_slice(payload_bits);
    }

    /// One bunch, one packet.
    fn build_bunch_packet(spec: &BunchSpec, payload_bits: &[bool]) -> Vec<u8> {
        let mut bits = Vec::new();
        write_bunch(&mut bits, spec, payload_bits);
        build_packet(&bits)
    }

    /// Little-endian i32, LSB first, matching `BitReader::read_i32`.
    fn write_i32_bits(bits: &mut Vec<bool>, value: i32) {
        for i in 0..32 {
            bits.push((value >> i) & 1 != 0);
        }
    }

    /// A single actor RepLayout content block with an empty body.
    fn write_empty_actor_block(bits: &mut Vec<bool>) {
        bits.push(true); // hasRepLayout
        bits.push(true); // isActor
        write_int_packed(bits, 0); // contentBits = 0
    }

    /// An out-of-range GUID count drops every path declaration in the bunch,
    /// and that has to be counted. It used to report the opposite:
    /// `skip_remaining` swallowed the declarations, `package_map_exports` was
    /// incremented as if the bunch had been read, and `bunch_header_failures`
    /// and `skipped_bits` both stayed at zero -- so actors later missing their
    /// path and class resolution had no counter pointing at the cause.
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
            "the whole abandoned payload is tallied, not just the unread tail:              the bits the failing stage had already consumed declared exports              that were dropped (package_map_exports and exported_guids are both              0 above), so they are lost too"
        );
    }

    /// A negative count is the same failure with a different bit pattern: the
    /// `as u32` cast in the range test would turn -1 into 4 294 967 295, so the
    /// sign has to be tested on its own.
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

    /// A RepLayout-export bunch is skipped whole -- that is a deliberate
    /// limitation -- but it must not be indistinguishable from an export that
    /// was read. It is counted on its own line rather than in `skipped_bits`,
    /// which the oracle reads as bits lost across failed content blocks.
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

    /// A reassembled partial bunch must apply the close flag its final fragment
    /// carried. Unreal writes `bOpen` on the first fragment and `bClose` on the
    /// last, and `UChannel::ReceivedNextBunch` copies the last one's close flags
    /// onto the reassembled bunch. This reader returned from the partial branch
    /// before reaching `handle_channel_close`, so the actor stayed open forever,
    /// its close row was never emitted and `actor_closes` never moved.
    #[test]
    fn a_reassembled_partial_bunch_applies_its_close_flag() {
        let mut bits = Vec::new();

        // Fragment 1: opens channel 2 for static actor GUID 3. Byte-aligned,
        // as every non-final fragment must be.
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

        // Fragment 2: the final fragment, carrying the close flag and the
        // actor's content block.
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
        let open = build_bunch_packet(
            &BunchSpec {
                ch_index: 2,
                b_open: true,
                ..Default::default()
            },
            &open_payload,
        );
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

    /// Opening a channel that is already open replaces the actor silently:
    /// every later block on the channel is attributed to the new actor and the
    /// old one never gets a close. The replacement is what the wire says, so it
    /// stands -- but it is counted, because nothing else moves when it happens.
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

    /// A failed open must not leave the previous actor live on the channel.
    ///
    /// Channel 2 opens static actor 3; a second open on it, for dynamic actor
    /// 4, stops before its mandatory spawn block and fails. The failure used
    /// to leave actor 3's state in place, still open, so the next non-open
    /// bunch on channel 2 was framed as actor 3's -- the stale-schema shape
    /// CLAUDE.md lists -- while only `bunch_header_failures` moved.
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

    /// A failed open on a channel with no actor leaves nothing to retire, but
    /// every later bunch on it is dropped at the channel guard -- and that drop
    /// used to move no counter at all. The failed open's own 8 bits are the
    /// bunch-header failure's; the later bunch's 10 are counted apart from
    /// `skipped_bits`, which the oracle reads as block-level loss.
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

    /// Dormancy is not destruction, and a dormant channel is not an open one.
    /// A reopen of it that fails leaves it not open -- nothing live to retire --
    /// and a bunch that then arrives on it is counted, not framed under the
    /// dormant actor.
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
    // Each probe below opens channel 2 for static actor 3, sends an open bunch
    // on channel 2 that stops before its own open is read, then one non-open
    // bunch with an empty block. The open itself failing is covered above by
    // a_failed_reopen_does_not_leave_the_previous_actor_live; these cover the
    // arms that return earlier, which used to keep actor 3 live and frame the
    // later block under it with `failed_reopens_while_open` at 0.

    /// Channel 2 opens for static actor 3, whose empty block frames.
    fn open_actor_three_on_channel_two() -> Vec<u8> {
        build_bunch_packet(
            &BunchSpec {
                ch_index: 2,
                b_open: true,
                ..Default::default()
            },
            &open_and_empty_block(),
        )
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
    /// ends there. Nothing may follow it: the GUID read would succeed on
    /// whatever did.
    fn must_be_mapped_count_without_its_guid() -> Vec<bool> {
        let mut bits = vec![true, false, false, false, false, false, false, false];
        bits.extend([false; 8]);
        bits
    }

    /// What every probe must show: actor 3 was taken off channel 2 and counted,
    /// so the later block was dropped at the guard -- counted, and failing the
    /// verdict through `bunches_on_unopened_channel` -- rather than framed
    /// under actor 3's schema.
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

    /// Clean exports do not save the open: nothing after a package-map export
    /// list is read, so an open bunch carrying one never has its open read.
    /// Nothing failed to read here, so no header failure is counted, and
    /// `failed_reopens_while_open` is the only counter that says an open was
    /// lost. The unread open's own bits are not tallied (docs/FOLLOWUP.md),
    /// so this does not assert `skipped_bits`.
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

        // A packet reader with no room for reliable-sequence state flags the
        // reopen `has_channel_limit_error`, while the pipeline's own table
        // still holds channel 2 and actor 3: the refusal lands on a live slot.
        reader.packet_reader = RawPacketReader::with_max_channels(0);
        let reopen = build_bunch_packet(
            &BunchSpec {
                ch_index: 2,
                b_open: true,
                ..Default::default()
            },
            &open_and_empty_block(),
        );
        reader.process_packet(&reopen, 1, &mut sink);
        // Restored before the later bunch: the limiting reader would refuse
        // that one too, and a refused bunch says nothing about whether actor 3
        // is still live. This one must reach the channel guard.
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

    /// The refusal arm retires before it closes: an open+close bunch refused on
    /// a live channel carried its own actor's close, not actor 3's, so actor 3
    /// gets no close row -- the corner a failed open+close already has.
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

    /// Only an open bunch displaces an actor. A bunch without `b_open` whose
    /// must-be-mapped read fails is abandoned, but it said nothing about who
    /// owns the channel: actor 3 stays live and the next bunch is framed under
    /// it. Pins the `b_open` gate every retire arm shares.
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

    /// The guard counts what it drops, which is the payload left after the
    /// bunch's preambles -- not the whole window, and nothing at all for a bunch
    /// whose preamble consumed every bit. A must-be-mapped GUID list is read
    /// and counted before the guard whether or not the channel is open.
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
        let open = build_bunch_packet(
            &BunchSpec {
                ch_index: 2,
                b_open: true,
                ..Default::default()
            },
            &open_payload,
        );
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
        let reopened = build_bunch_packet(
            &BunchSpec {
                ch_index: 2,
                b_open: true,
                ..Default::default()
            },
            &reopened_payload,
        );
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
        let packet = build_bunch_packet(
            &BunchSpec {
                ch_index: crate::types::MAX_ACTIVE_CHANNELS as u32,
                b_open: true,
                ..Default::default()
            },
            &payload,
        );
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

    /// A dynamic actor's spawn block is mandatory. A payload that ends at the
    /// actor GUID used to be accepted as a successful open, emitting an actor
    /// with archetype and level GUID 0 and no transforms while `actor_opens`
    /// went up and `bunch_header_failures` stayed at zero -- a plausible actor
    /// row invented out of nothing.
    #[test]
    fn a_dynamic_open_without_its_spawn_block_is_a_failure_not_an_actor() {
        let mut payload: Vec<bool> = Vec::new();
        write_int_packed(&mut payload, 2); // dynamic actor GUID, then nothing

        let packet = build_bunch_packet(
            &BunchSpec {
                ch_index: 2,
                b_open: true,
                ..Default::default()
            },
            &payload,
        );

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
    /// GUID. This is the case the removed `!payload.at_end()` guard could be
    /// mistaken for, and it must keep working.
    #[test]
    fn a_static_open_with_no_payload_left_is_still_an_actor() {
        let mut payload: Vec<bool> = Vec::new();
        write_int_packed(&mut payload, 3); // static (odd) actor GUID

        let packet = build_bunch_packet(
            &BunchSpec {
                ch_index: 2,
                b_open: true,
                ..Default::default()
            },
            &payload,
        );

        let mut reader = ReplicationReader::new("++Ares-Core+release-13.01").unwrap();
        let mut sink = TestSink::default();
        reader.process_packet(&packet, 0, &mut sink);

        assert_eq!(reader.stats().actor_opens, 1);
        assert_eq!(reader.stats().actor_opens_missing_spawn, 0);
        assert_eq!(reader.stats().bunch_header_failures, 0);
        assert_eq!(sink.opens, vec![2]);
    }

    /// A partial bunch whose continuation never arrives is data loss, and at
    /// EOF it is the only kind nothing reports: `partial_fragments` moved when
    /// the initial fragment arrived, `partial_errors` stayed at zero because
    /// nothing was out of sequence, and the buffered bits went out with the
    /// accumulator. `finish` is where that becomes visible.
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

    /// An unaligned window is realigned to bit zero of the staging buffer --
    /// that realignment is the only reason the copy exists.
    #[test]
    fn fragment_staging_realigns_an_offset_window() {
        let packet = [0b1111_0000u8, 0b0000_1111];
        let mut reader = BitReader::new(&packet);
        reader.skip_bits(4).unwrap();
        let window = reader.sub_reader(8).unwrap();

        let mut buffer = Vec::new();
        assert_eq!(stage_fragment(window, &mut buffer), 1);
        assert_eq!(buffer[0], 0b1111_1111);
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
    }

    #[test]
    fn malformed_packet_counted() {
        let mut reader = ReplicationReader::new("++Ares-Core+release-13.01").unwrap();
        let mut sink = TestSink::default();
        reader.process_packet(&[0x00, 0x00], 0, &mut sink);
        assert_eq!(reader.stats().malformed_packets, 1);
    }

    /// The unresolved callback receives the exact decoded block, not the wire
    /// bytes or the reusable scratch tail. The 7-bit literal is a golden V13.01
    /// transform vector: wire 0xBF for actor 2 decodes to 0x66.
    #[test]
    fn unresolved_class_net_cache_exposes_the_decoded_whole_payload() {
        let wire = [0xBF];
        let mut payload = BitReader::with_bit_len(&wire, 7).unwrap();
        let mut scratch = vec![0xFF; 16];
        let mut stats = NetStats::default();
        let mut channels = ChannelTable::default();
        let mut sink = TestSink::default();
        let mut stage = Stage {
            stats: &mut stats,
            channels: &mut channels,
            transform: TransformVersion::V1301,
            scratch: &mut scratch,
        };

        framing::decode_and_parse_class_net_cache(
            &mut payload,
            7,
            NetworkGuid(2),
            0,
            &mut stage,
            &mut sink,
        );

        assert_eq!(payload.position(), 7);
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
        assert_eq!(stats.rpcs, 0);
        assert_eq!(stats.rpc_stream_failures, 1);
        assert_eq!(stats.unresolved_rpc_payloads_preserved, 1);
        assert_eq!(stats.skipped_bits, 7);
    }

    /// A field stream that returns Ok but abandons bits mid-block must be a
    /// stream failure as well as landing those bits in `skipped_bits`. Before
    /// the fix, `parse_class_net_cache`
    /// did `reader.skip_remaining(); break; return Ok(count)` and the framing
    /// layer only counted `skipped_bits` on Err, so the abandoned bits vanished
    /// from every counter.
    ///
    /// Same golden V13.01 vector as the unresolved test (wire 0xBF -> 0x66 for
    /// actor 2), but `function_count=2` so the parser walks the stream instead
    /// of bailing with UnresolvedFunctionCount. Decoded 0x66 yields one handle
    /// bit (handle=0) then leaves 6 bits -- fewer than the 8 an IntPacked
    /// payload-length read needs -- so the stream returns Ok(0) after skipping
    /// those 6 bits. They must be accounted.
    #[test]
    fn class_net_cache_overrun_ok_path_is_a_stream_failure() {
        let wire = [0xBF];
        let mut payload = BitReader::with_bit_len(&wire, 7).unwrap();
        let mut scratch = vec![0xFF; 16];
        let mut stats = NetStats::default();
        let mut channels = ChannelTable::default();
        let mut sink = TestSink::default();
        let mut stage = Stage {
            stats: &mut stats,
            channels: &mut channels,
            transform: TransformVersion::V1301,
            scratch: &mut scratch,
        };

        framing::decode_and_parse_class_net_cache(
            &mut payload,
            7,
            NetworkGuid(2),
            2,
            &mut stage,
            &mut sink,
        );

        assert_eq!(payload.position(), 7);
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

    /// Invert the short V13.01 byte transform for a test payload. Payloads
    /// below 32 bits are transformed byte-by-byte, so each byte has exactly
    /// one preimage and can be found independently without duplicating the
    /// production transform implementation.
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

        let mut decoded = vec![0u8; decoded_bits.len().div_ceil(8)];
        for (index, bit) in decoded_bits.iter().copied().enumerate() {
            if bit {
                decoded[index / 8] |= 1 << (index % 8);
            }
        }
        let wire = wire_for_short_decoded(&decoded, decoded_bits.len(), 2);
        let mut payload = BitReader::with_bit_len(&wire, decoded_bits.len() as u64).unwrap();
        let mut scratch = Vec::new();
        let mut stats = NetStats::default();
        let mut channels = ChannelTable::default();
        let mut sink = TestSink {
            rep_layout_tail_outcome: Some(RepLayoutTailOutcome::Decoded { rpc_count: 1 }),
            ..TestSink::default()
        };
        let mut stage = Stage {
            stats: &mut stats,
            channels: &mut channels,
            transform: TransformVersion::V1301,
            scratch: &mut scratch,
        };

        framing::decode_and_parse_rep_layout(
            &mut payload,
            decoded_bits.len(),
            NetworkGuid(2),
            &mut stage,
            &mut sink,
        );

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
        let mut decoded = vec![0u8; decoded_bits.len().div_ceil(8)];
        for (index, bit) in decoded_bits.iter().copied().enumerate() {
            if bit {
                decoded[index / 8] |= 1 << (index % 8);
            }
        }
        let wire = wire_for_short_decoded(&decoded, decoded_bits.len(), 2);
        let mut payload = BitReader::with_bit_len(&wire, decoded_bits.len() as u64).unwrap();
        let mut scratch = Vec::new();
        let mut stats = NetStats::default();
        let mut channels = ChannelTable::default();
        let mut sink = TestSink {
            rep_layout_tail_outcome: Some(RepLayoutTailOutcome::Preserved {
                cause: StreamFailureCause::UnverifiedRepLayoutTail,
            }),
            ..TestSink::default()
        };
        let mut stage = Stage {
            stats: &mut stats,
            channels: &mut channels,
            transform: TransformVersion::V1301,
            scratch: &mut scratch,
        };

        framing::decode_and_parse_rep_layout(
            &mut payload,
            decoded_bits.len(),
            NetworkGuid(2),
            &mut stage,
            &mut sink,
        );

        assert_eq!(stats.field_stream_failures, 0);
        assert_eq!(stats.rpc_stream_failures, 1);
        assert_eq!(stats.unresolved_rpc_payloads_preserved, 1);
        assert_eq!(stats.skipped_bits, 13);
        assert_eq!(sink.stream_failures.len(), 1);
        let failure = sink.stream_failures[0];
        assert_eq!(failure.kind, StreamKind::Rpc);
        assert_eq!(failure.bit_count, 13);
        assert!(failure.payload_preserved);
    }

    #[test]
    fn rep_layout_overrun_ok_path_is_a_stream_failure() {
        let mut decoded_bits = Vec::new();
        decoded_bits.push(false); // property checksum
        write_int_packed(&mut decoded_bits, 1); // handle 0
        write_int_packed(&mut decoded_bits, 32); // overruns the remaining 8 bits
        decoded_bits.extend(std::iter::repeat_n(false, 8));
        assert_eq!(decoded_bits.len(), 25);
        let mut decoded = vec![0u8; decoded_bits.len().div_ceil(8)];
        for (index, bit) in decoded_bits.iter().copied().enumerate() {
            if bit {
                decoded[index / 8] |= 1 << (index % 8);
            }
        }
        let wire = wire_for_short_decoded(&decoded, decoded_bits.len(), 2);
        let mut payload = BitReader::with_bit_len(&wire, decoded_bits.len() as u64).unwrap();
        let mut scratch = Vec::new();
        let mut stats = NetStats::default();
        let mut channels = ChannelTable::default();
        let mut sink = TestSink::default();
        let mut stage = Stage {
            stats: &mut stats,
            channels: &mut channels,
            transform: TransformVersion::V1301,
            scratch: &mut scratch,
        };

        framing::decode_and_parse_rep_layout(
            &mut payload,
            decoded_bits.len(),
            NetworkGuid(2),
            &mut stage,
            &mut sink,
        );

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

    /// The `cause` field must attribute each failure to the arm that produced
    /// it, and name the record the walk stopped in. The aggregate keyed on
    /// these values is what separates preserved-unresolved blocks from real
    /// loss per group; a cause that did not match its arm would misfile
    /// millions of blocks.
    #[test]
    fn stream_failures_carry_their_cause_and_failing_record() {
        // Arm 1: an unresolved ClassNetCache group (function_count = 0). The
        // walk never begins, so the failing record is offset 0, no handle --
        // and the payload reached the sink as preserved.
        let wire = [0xBF];
        let mut payload = BitReader::with_bit_len(&wire, 7).unwrap();
        let mut scratch = vec![0xFF; 16];
        let mut stats = NetStats::default();
        let mut channels = ChannelTable::default();
        let mut sink = TestSink::default();
        let mut stage = Stage {
            stats: &mut stats,
            channels: &mut channels,
            transform: TransformVersion::V1301,
            scratch: &mut scratch,
        };
        framing::decode_and_parse_class_net_cache(
            &mut payload,
            7,
            NetworkGuid(2),
            0,
            &mut stage,
            &mut sink,
        );
        assert_eq!(sink.stream_failures.len(), 1);
        let failure = &sink.stream_failures[0];
        assert_eq!(failure.cause, StreamFailureCause::UnresolvedFunctionCount);
        assert_eq!(failure.record_handle, None);
        assert_eq!(failure.record_offset, Some(0));
        assert_eq!(sink.unresolved_payloads.len(), 1, "payload preserved");

        // Arm 2: a RepLayout Ok walk that abandoned a tail. The failing
        // record -- handle 0, which began at bit 1 after the checksum -- is
        // named exactly.
        let mut decoded_bits = Vec::new();
        decoded_bits.push(false); // property checksum
        write_int_packed(&mut decoded_bits, 1); // handle 0
        write_int_packed(&mut decoded_bits, 32); // overruns the remaining 8 bits
        decoded_bits.extend(std::iter::repeat_n(false, 8));
        let mut decoded = vec![0u8; decoded_bits.len().div_ceil(8)];
        for (index, bit) in decoded_bits.iter().copied().enumerate() {
            if bit {
                decoded[index / 8] |= 1 << (index % 8);
            }
        }
        let wire = wire_for_short_decoded(&decoded, decoded_bits.len(), 2);
        let mut payload = BitReader::with_bit_len(&wire, decoded_bits.len() as u64).unwrap();
        let mut scratch = Vec::new();
        let mut stats = NetStats::default();
        let mut channels = ChannelTable::default();
        let mut sink = TestSink::default();
        let mut stage = Stage {
            stats: &mut stats,
            channels: &mut channels,
            transform: TransformVersion::V1301,
            scratch: &mut scratch,
        };
        framing::decode_and_parse_rep_layout(
            &mut payload,
            decoded_bits.len(),
            NetworkGuid(2),
            &mut stage,
            &mut sink,
        );
        assert_eq!(sink.stream_failures.len(), 1);
        let failure = &sink.stream_failures[0];
        assert_eq!(failure.cause, StreamFailureCause::AbandonedTail);
        assert_eq!(failure.record_handle, Some(0));
        assert_eq!(failure.record_offset, Some(1));
    }

    /// The `Err` arm must charge the block, not the reader's remainder.
    ///
    /// `read_int_packed` consumes each 8-bit chunk before it can discover the
    /// value does not terminate, so a block whose last `IntPacked` expires
    /// exactly at the block end leaves the reader at `position() == len` with
    /// `bits_remaining() == 0`. The arm used to add that remainder, which
    /// charged **zero** abandoned bits for a block that lost all nine of its
    /// bits: `field_stream_failures` moved to 1 while `skipped_bits` stayed at
    /// 0, a failure counted at block level and absent from the bit accounting
    /// the oracle divides by failed blocks.
    ///
    /// Nine decoded bits: the checksum bit, then 0x01 -- an `IntPacked` chunk
    /// whose low bit says "another chunk follows" when the block has none. The
    /// handle read therefore fails with `Eof` having already consumed the
    /// window whole.
    #[test]
    fn rep_layout_err_at_the_exact_block_end_still_charges_the_block() {
        let mut decoded_bits = vec![false]; // property checksum
        // 0x01 LSB-first: continuation set, payload bits all zero.
        for index in 0..8 {
            decoded_bits.push((0x01u8 & (1 << index)) != 0);
        }
        assert_eq!(decoded_bits.len(), 9);

        let mut decoded = vec![0u8; decoded_bits.len().div_ceil(8)];
        for (index, bit) in decoded_bits.iter().copied().enumerate() {
            if bit {
                decoded[index / 8] |= 1 << (index % 8);
            }
        }
        let wire = wire_for_short_decoded(&decoded, decoded_bits.len(), 2);
        let mut payload = BitReader::with_bit_len(&wire, decoded_bits.len() as u64).unwrap();
        let mut scratch = Vec::new();
        let mut stats = NetStats::default();
        let mut channels = ChannelTable::default();
        let mut sink = TestSink::default();
        let mut stage = Stage {
            stats: &mut stats,
            channels: &mut channels,
            transform: TransformVersion::V1301,
            scratch: &mut scratch,
        };

        framing::decode_and_parse_rep_layout(
            &mut payload,
            decoded_bits.len(),
            NetworkGuid(2),
            &mut stage,
            &mut sink,
        );

        assert_eq!(stats.field_stream_failures, 1);
        assert_eq!(stats.fields, 0, "no field was emitted");
        // The reader is exhausted, so `bits_remaining()` is 0 -- the number the
        // arm used to charge. What was lost is the whole block.
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
        for index in 0..8 {
            decoded_bits.push((0x01u8 & (1 << index)) != 0);
        }
        assert_eq!(decoded_bits.len(), 25);
        let mut decoded = vec![0u8; decoded_bits.len().div_ceil(8)];
        for (index, bit) in decoded_bits.iter().copied().enumerate() {
            if bit {
                decoded[index / 8] |= 1 << (index % 8);
            }
        }
        let wire = wire_for_short_decoded(&decoded, decoded_bits.len(), 2);
        let mut payload = BitReader::with_bit_len(&wire, decoded_bits.len() as u64).unwrap();
        let mut scratch = Vec::new();
        let mut stats = NetStats::default();
        let mut channels = ChannelTable::default();
        let mut sink = TestSink::default();
        let mut stage = Stage {
            stats: &mut stats,
            channels: &mut channels,
            transform: TransformVersion::V1301,
            scratch: &mut scratch,
        };

        framing::decode_and_parse_rep_layout(
            &mut payload,
            decoded_bits.len(),
            NetworkGuid(2),
            &mut stage,
            &mut sink,
        );

        assert_eq!(sink.fields, vec![(0, 0)], "the valid prefix row remains");
        assert_eq!(stats.fields, 1, "NetStats matches the emitted prefix");
        assert_eq!(stats.field_stream_failures, 1);
        assert!(sink.rep_layout_tails.is_empty());
        assert_eq!(sink.stream_failures[0].cause, StreamFailureCause::ReadError);
        assert_eq!(sink.stream_failures[0].record_handle, None);
        assert_eq!(sink.stream_failures[0].record_offset, Some(17));
    }

    /// The RPC parser's `Err` arm, same shape and same reason.
    ///
    /// One handle bit (max clamped to 2) then 0x01, an `IntPacked` that claims
    /// a chunk the block does not carry. `function_count` is 2 so the parser
    /// walks the stream rather than bailing with `UnresolvedFunctionCount`,
    /// which is a different arm with its own accounting.
    #[test]
    fn class_net_cache_err_at_the_exact_block_end_still_charges_the_block() {
        let mut decoded_bits = Vec::new();
        write_serialized_int(&mut decoded_bits, 0, 2); // one handle bit
        for index in 0..8 {
            decoded_bits.push((0x01u8 & (1 << index)) != 0);
        }
        assert_eq!(decoded_bits.len(), 9);

        let mut decoded = vec![0u8; decoded_bits.len().div_ceil(8)];
        for (index, bit) in decoded_bits.iter().copied().enumerate() {
            if bit {
                decoded[index / 8] |= 1 << (index % 8);
            }
        }
        let wire = wire_for_short_decoded(&decoded, decoded_bits.len(), 2);
        let mut payload = BitReader::with_bit_len(&wire, decoded_bits.len() as u64).unwrap();
        let mut scratch = Vec::new();
        let mut stats = NetStats::default();
        let mut channels = ChannelTable::default();
        let mut sink = TestSink::default();
        let mut stage = Stage {
            stats: &mut stats,
            channels: &mut channels,
            transform: TransformVersion::V1301,
            scratch: &mut scratch,
        };

        framing::decode_and_parse_class_net_cache(
            &mut payload,
            decoded_bits.len(),
            NetworkGuid(2),
            2,
            &mut stage,
            &mut sink,
        );

        assert_eq!(stats.rpc_stream_failures, 1);
        assert_eq!(stats.rpcs, 0, "no RPC was emitted");
        assert_eq!(
            stats.unresolved_rpc_payloads_preserved, 0,
            "this is a walked stream that failed, not an unresolved group"
        );
        assert_eq!(sink.stream_failures.len(), 1);
        assert_eq!(sink.stream_failures[0].remaining_bits, 0);
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
        for index in 0..8 {
            decoded_bits.push((0x01u8 & (1 << index)) != 0);
        }
        assert_eq!(decoded_bits.len(), 18);
        let mut decoded = vec![0u8; decoded_bits.len().div_ceil(8)];
        for (index, bit) in decoded_bits.iter().copied().enumerate() {
            if bit {
                decoded[index / 8] |= 1 << (index % 8);
            }
        }
        let wire = wire_for_short_decoded(&decoded, decoded_bits.len(), 2);
        let mut payload = BitReader::with_bit_len(&wire, decoded_bits.len() as u64).unwrap();
        let mut scratch = Vec::new();
        let mut stats = NetStats::default();
        let mut channels = ChannelTable::default();
        let mut sink = TestSink::default();
        let mut stage = Stage {
            stats: &mut stats,
            channels: &mut channels,
            transform: TransformVersion::V1301,
            scratch: &mut scratch,
        };

        framing::decode_and_parse_class_net_cache(
            &mut payload,
            decoded_bits.len(),
            NetworkGuid(2),
            2,
            &mut stage,
            &mut sink,
        );

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
        let mut data = vec![0u8; bits.len().div_ceil(8)];
        for (i, &bit) in bits.iter().enumerate() {
            if bit {
                data[i >> 3] |= 1 << (i & 7);
            }
        }
        let mut payload = BitReader::with_bit_len(&data, bits.len() as u64).unwrap();
        let mut reader = ReplicationReader::new("++Ares-Core+release-13.01").unwrap();
        let mut sink = TestSink::default();
        let mut stats = NetStats::default();
        let mut channels = ChannelTable::default();
        let header = RawBunchHeader {
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
        };
        let mut stage = Stage {
            stats: &mut stats,
            channels: &mut channels,
            transform: reader.transform,
            scratch: &mut reader.scratch,
        };
        framing::frame_content_blocks(
            &mut payload,
            5,
            NetworkGuid(42),
            &mut stage,
            &mut sink,
            &ctx,
        );
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

    /// A header that fails to read has already consumed what it read. The
    /// abort charged `bits_remaining()`, which is 0 here, so
    /// `content_block_framing_failures` moved with no bit tally behind it --
    /// the undercount `abandoned_on_error` and `abandon_bunch` already fixed
    /// one depth down and one depth up.
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

    /// `content_bits` is an IntPacked; a continuation byte followed by the end
    /// of the bunch consumes 8 bits before the read fails. With the 2-bit
    /// actor header that is 10 bits lost, and the abort charged 0.
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

    /// Verifies that a content-block overrun produces a DiagnosticEvent with
    /// full context (packet id, channel, bunch flags, consumed/remaining bits).
    ///
    /// This is the path the resolved "malformed 1 / skipped 695" residue took
    /// (see the oracle's module doc), not something every replay still shows.
    /// The overrun, too, is charged from the failing block's first bit: its
    /// header and `content_bits` were read and framed nothing, so the loss is
    /// 2 + 16 + 8 = 26 bits while 8 remained after the read.
    #[cfg(feature = "diagnostics")]
    #[test]
    fn content_bits_overrun_emits_diagnostic() {
        use crate::stats::SkipReason;

        let mut reader = ReplicationReader::new("++Ares-Core+release-13.01").unwrap();
        let mut sink = TestSink::default();

        // Simulate: bunch payload = 24 bits total
        //   content block header: has_rep_layout=0 (1 bit), is_actor=1 (1 bit) -> 2 bits
        //   content_bits = IntPacked(999) -> 16 bits (0x7CE in IntPacked encoding)
        //   remaining after header+content_bits = 24 - 2 - 16 = 6 bits
        //   999 > 6 -> overrun
        let mut bits: Vec<bool> = Vec::new();
        // has_rep_layout = false
        bits.push(false);
        // is_actor = true
        bits.push(true);
        // IntPacked(999): 999 = 0x3E7
        //   chunk0: (999 & 0x7F) = 0x67, more=1 -> byte = (0x67 << 1) | 1 = 0xCF
        //   chunk1: (999 >> 7) = 7, more=0 -> byte = (7 << 1) | 0 = 0x0E
        let packed_bytes = [0xCF_u8, 0x0E];
        for &byte in &packed_bytes {
            for i in 0..8 {
                bits.push((byte & (1 << i)) != 0);
            }
        }
        // Add a few more padding bits so remaining > 0 but < 999
        bits.extend(std::iter::repeat_n(false, 8));

        let byte_count = bits.len().div_ceil(8);
        let mut data = vec![0u8; byte_count];
        for (i, &bit) in bits.iter().enumerate() {
            if bit {
                data[i >> 3] |= 1 << (i & 7);
            }
        }

        let mut payload_reader = BitReader::with_bit_len(&data, bits.len() as u64).unwrap();
        let mut stats = NetStats::default();
        let mut channels = ChannelTable::default();
        let header = RawBunchHeader {
            packet_id: 42,
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
        };
        let mut stage = Stage {
            stats: &mut stats,
            channels: &mut channels,
            transform: reader.transform,
            scratch: &mut reader.scratch,
        };

        framing::frame_content_blocks(
            &mut payload_reader,
            5,
            NetworkGuid(42),
            &mut stage,
            &mut sink,
            &ctx,
        );

        // Verify diagnostic was emitted
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
        // remaining_bits should be 8 (the padding bits we added); what the
        // overrun lost is the whole block from its first bit.
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

    // --- controller property-block regression tests
    // (docs/archive/PROJECT_STATUS.md 17-A) ---

    /// Build a non-partial open bunch around `payload_bits` and return the
    /// full packet bytes, ready for `process_packet`.
    fn build_open_bunch_packet(ch_index: u32, payload_bits: &[bool]) -> Vec<u8> {
        let mut bits = vec![
            true,  // bControl
            true,  // bOpen
            false, // bClose
            false, // bIsReplicationPaused
            true,  // bReliable
        ];
        write_int_packed(&mut bits, ch_index);
        bits.extend_from_slice(&[
            false, // bHasPackageMapExports
            false, // bHasMustBeMappedGUIDs
            false, // bPartial
            false, // VALORANT bit
            true,  // FName isHardcoded
        ]);
        write_int_packed(&mut bits, 1); // FName index
        write_serialized_int(
            &mut bits,
            payload_bits.len() as u32,
            crate::types::MAX_PACKET_SIZE_BITS,
        );
        bits.extend_from_slice(payload_bits);
        build_packet(&bits)
    }

    /// Write the spawn block for a dynamic actor: archetype, level, and the
    /// four optional transform vectors (location, rotation, scale, velocity),
    /// every one absent (leading `false` bit). Velocity is included to match
    /// the unconditional read -- omitting it is the one-bit regression.
    fn write_minimal_spawn_data(bits: &mut Vec<bool>, archetype_guid: u32) {
        write_int_packed(bits, archetype_guid); // archetype GUID
        write_int_packed(bits, 0); // level GUID (0 -> not valid, returns early)
        bits.push(false); // location: hasValue = false
        bits.push(false); // rotation: hasComponent = false
        bits.push(false); // scale: hasValue = false
        bits.push(false); // velocity: hasValue = false (unconditional read)
    }

    /// The controller's opening bunch carries nine bits between the spawn
    /// block and the first content-block header: one velocity bit (read
    /// unconditionally) plus an eight-bit net-player-index byte (consumed
    /// because `is_player_controller_channel` resolves the archetype through
    /// the path cache). Without either one, the first header misframes and
    /// the controller's own RepLayout property block -- `PlayerState` and
    /// `SpawnLocation` -- is never walked. docs/archive/PROJECT_STATUS.md
    /// 17-A found the mechanism; this pins the fix.
    ///
    /// Applying either half alone is the one-bit-off failure that destroyed
    /// seven real subobject rows in the earlier experiment. This test fires
    /// both together because that is the only combination that works.
    #[test]
    fn controller_property_block_is_reached() {
        let mut payload: Vec<bool> = Vec::new();

        // Actor GUID 2: dynamic (even, non-zero), so a spawn block follows.
        write_int_packed(&mut payload, 2);
        // Spawn data. Archetype GUID 9 is what the cache will resolve.
        write_minimal_spawn_data(&mut payload, 9);
        // Net-player-index byte (value 0). On the wire because this is a
        // PlayerController; consumed only if path_for_guid answers.
        payload.extend(std::iter::repeat_n(false, 8));
        // The actor's own RepLayout block.
        payload.push(true); // hasRepLayout
        payload.push(true); // isActor
        write_int_packed(&mut payload, 0); // contentBits = 0

        let packet = build_open_bunch_packet(2, &payload);

        let mut sink = TestSink::default();
        // The cache knows the archetype path from chunk-level exports that
        // arrived before this bunch; the old pc_guids set did not. That is
        // the asymmetry the cache lookup fixes.
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

    /// The 12.01--12.06 replay controller is named BaseJanusController.
    /// Its player-index byte must be consumed before the first property block,
    /// just like BaseReplayController from 12.07 onward.
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

    /// A non-controller dynamic actor has no net-player-index byte on the wire,
    /// so the byte must NOT be consumed. The content-block header must sit
    /// immediately after the spawn data and frame correctly.
    ///
    /// This is the guard against over-consuming: if the byte read were
    /// unconditional rather than gated on the cache lookup, it would eat the
    /// first eight bits of the content header here.
    #[test]
    fn non_controller_dynamic_actor_skips_net_player_index_byte() {
        let mut payload: Vec<bool> = Vec::new();
        write_int_packed(&mut payload, 2); // actor GUID 2 (dynamic)
        write_minimal_spawn_data(&mut payload, 9); // archetype 9, no path in cache
        // NO net-player-index byte -- this actor is not a controller.
        payload.push(true); // hasRepLayout
        payload.push(true); // isActor
        write_int_packed(&mut payload, 0); // contentBits = 0

        let packet = build_open_bunch_packet(2, &payload);

        let mut sink = TestSink::default();
        // guid_paths is empty: path_for_guid returns None for every GUID,
        // so is_player_controller_channel is false.
        let mut reader = ReplicationReader::new("++Ares-Core+release-13.01").unwrap();
        reader.process_packet(&packet, 0, &mut sink);

        assert_eq!(reader.stats().actor_opens, 1);
        assert_eq!(reader.stats().skipped_bits, 0);
        assert_eq!(sink.content_blocks.len(), 1);
        assert!(sink.content_blocks[0].has_rep_layout);
        assert!(sink.content_blocks[0].is_actor);
    }

    /// If the net-player-index byte is on the wire but the cache does NOT
    /// resolve the channel as a controller, the byte is left unconsumed and
    /// the first content-block header misframes. This is the exact failure
    /// mode 17-A describes: the misframed header re-synchronises, so the
    /// bunch is not lost -- the property block is simply routed to the
    /// ClassNetCache path instead.
    ///
    /// This test proves the cache lookup is what makes the difference: same
    /// payload as the controller test, but without the path mapping, the
    /// block is NOT the actor's RepLayout property block.
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
        // ...but the cache does not know the archetype path. This is the
        // pre-fix state: pc_guids was empty, and the cache lookup that
        // replaced it had not been added yet.
        let mut reader = ReplicationReader::new("++Ares-Core+release-13.01").unwrap();
        reader.process_packet(&packet, 0, &mut sink);

        // The block that lands is NOT the actor's property block. With the
        // fix applied (cache lookup), this misframing cannot happen on a real
        // controller bunch. Without the fix it is exactly what occurred.
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
    // The packet reader keeps a per-channel partial tracker of its own. It used
    // to write `has_partial_error` onto the header, and both the accumulator and
    // the rejected-row condition read that flag, so any disagreement between the
    // two trackers changed what was reassembled and what was preserved -- with
    // `partial_errors` still at zero. Each test below is one such disagreement,
    // reproduced on `main` before the fix.

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

    /// A partial that is both initial and final is a whole bunch every time it
    /// arrives. The packet reader never retired its state for that shape, so
    /// the second one on a channel was flagged as an overlapping initial: the
    /// accumulator refused to complete it, `partial_errors` stayed 0, and the
    /// same bits were written twice as rejected rows labelled with a cause
    /// whose counter never moved.
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

    /// An unaligned continuation is rejected by the accumulator alone; the
    /// packet reader kept its assembly. A clean whole bunch that follows on the
    /// same channel must still be processed, and the rejected bits preserved
    /// exactly once.
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
    /// buffered and later reassembled. It must not also be written as a rejected
    /// overlapping initial -- that put the same bits into both the decoded
    /// stream and the preservation table.
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
    /// completion. The zero-payload path marked it complete without the guard
    /// its non-empty sibling carries, so `partial_completed` reported a bunch
    /// that was never processed and the complete-but-untaken state stayed in
    /// the accumulator until end of stream.
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

    // --- the preservation hand-off itself ---
    //
    // The accumulator's own tests cover what it displaces. What they cannot see
    // is the step after: pipeline -> `on_rejected_partial`. Five simultaneous
    // mutations of that step (rows not written, payloads emptied, three reasons
    // relabelled, end-of-stream counters zeroed) left every workspace test
    // green. Each test below pins one of those hand-offs by its bytes, reason
    // and counters.

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

    /// A partial bunch refused because per-channel state is exhausted keeps its
    /// own reason; it is not a missing initial.
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
