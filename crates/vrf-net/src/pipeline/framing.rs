//! Content-block framing: the per-block hot loop.
//!
//! Measured block/bunch/actor-open rates on the reference replay: docs/PERFORMANCE_NOTES.md#measured-rates-reference-replay-02d4d478.
//!
//! Everything here runs per block, so anything that can be hoisted out or
//! made conditional on a failure path belongs elsewhere. The loop reads a
//! block header and its payload bit count, hands the header to the sink
//! (which answers a function count for ClassNetCache blocks), then decodes
//! the payload and walks its field or RPC stream.
//!
//! # Failure policy
//!
//! Three depths fail and are counted separately:
//!
//! - the block header or its bit count does not read -- the rest of the bunch
//!   is unframeable and is abandoned, charging [`abandoned_from`];
//! - the declared bit count overruns the bunch -- likewise;
//! - the payload decoded but its inner stream did not walk -- only that
//!   block's bits are lost ([`decode_and_parse_rep_layout`]), and the sink is
//!   told which class it was.

use vrf_bitio::BitReader;
use vrf_transform::TransformVersion;

use crate::bunch::RawBunchHeader;
use crate::content::{self, ContentBlockHeader};
use crate::error::NetError;
use crate::field;
use crate::stats::NetStats;
use crate::types::NetworkGuid;

use super::{
    RepLayoutTailOutcome, ReplicationSink, Stage, StreamFailure, StreamFailureCause, StreamKind,
};

/// Per-bunch context for diagnostic events, read only on a failure path. It
/// borrows the header, so the flag snapshot is built only when an event is.
/// Every field is diagnostics-only; see [`super::BunchIds`] for why they are
/// threaded through a build without that feature.
#[cfg_attr(not(feature = "diagnostics"), allow(dead_code))]
pub(super) struct BunchContext<'a> {
    pub header: &'a RawBunchHeader,
    pub ids: super::BunchIds,
    /// The channel's archetype as its open read it: `NetworkGuid(0)` for a
    /// static actor, whose open carries no spawn block.
    pub archetype_net_guid: NetworkGuid,
}

/// Walk a bunch payload as a sequence of content blocks.
pub(super) fn frame_content_blocks(
    payload: &mut BitReader<'_>,
    ch_index: u32,
    actor_net_guid: NetworkGuid,
    stage: &mut Stage<'_>,
    sink: &mut dyn ReplicationSink,
    ctx: &BunchContext<'_>,
) {
    let mut block_index: u32 = 0;

    while !payload.at_end() {
        let consumed_before_header = payload.position();

        let Ok(header) = content::read_content_block_header(payload, actor_net_guid, sink) else {
            let remaining = payload.bits_remaining();
            let abandoned = abandoned_from(payload, consumed_before_header);
            stage.stats.skipped_bits += abandoned;
            stage.stats.content_block_framing_failures += 1;
            diagnostics::header_read_error(
                stage.stats,
                ctx,
                ch_index,
                actor_net_guid,
                block_index,
                consumed_before_header,
                remaining,
                abandoned,
            );
            payload.skip_remaining();
            return;
        };

        if header.is_deleted {
            sink.on_deleted_block(ch_index, actor_net_guid, &header);
            stage.stats.deleted_blocks += 1;
            stage.stats.content_blocks += 1;
            block_index += 1;
            continue;
        }

        let consumed_before_bits_read = payload.position();
        let Ok(content_bits) = payload.read_int_packed() else {
            let remaining = payload.bits_remaining();
            let abandoned = abandoned_from(payload, consumed_before_header);
            stage.stats.skipped_bits += abandoned;
            stage.stats.content_block_framing_failures += 1;
            diagnostics::content_bits_read_error(
                stage.stats,
                ctx,
                ch_index,
                actor_net_guid,
                block_index,
                consumed_before_bits_read,
                remaining,
                abandoned,
                &header,
            );
            payload.skip_remaining();
            return;
        };

        if u64::from(content_bits) > payload.bits_remaining() {
            let remaining = payload.bits_remaining();
            let abandoned = abandoned_from(payload, consumed_before_header);
            stage.stats.malformed_content_blocks += 1;
            stage.stats.skipped_bits += abandoned;
            diagnostics::content_bits_overrun(
                stage.stats,
                ctx,
                ch_index,
                actor_net_guid,
                block_index,
                payload.position(),
                remaining,
                abandoned,
                &header,
                content_bits,
            );
            payload.skip_remaining();
            return;
        }

        let function_count = sink.on_content_block(ch_index, actor_net_guid, &header);

        stage.stats.content_blocks += 1;
        let this_block = block_index;
        block_index += 1;

        // Where this block's payload begins, for the event below.
        let payload_start = payload.position();
        let mut decoded = true;
        if header.has_rep_layout {
            stage.stats.rep_layout_blocks += 1;
            if content_bits != 0 {
                decoded = decode_and_parse_rep_layout(
                    payload,
                    content_bits as usize,
                    actor_net_guid,
                    stage,
                    sink,
                );
            }
        } else {
            stage.stats.class_net_cache_blocks += 1;
            if content_bits != 0 {
                decoded = decode_and_parse_class_net_cache(
                    payload,
                    content_bits as usize,
                    actor_net_guid,
                    function_count,
                    stage,
                    sink,
                );
            }
        }
        // Counted already; this adds the event. Unreachable here by
        // construction -- see `SkipReason::ParseFailure`.
        if !decoded {
            diagnostics::parse_failure(
                stage.stats,
                ctx,
                ch_index,
                actor_net_guid,
                this_block,
                payload_start,
                payload.len_bits() - payload_start,
                &header,
                content_bits,
            );
        }
    }
}

/// Decode a block payload into `stage.scratch` and return its byte length, or
/// `None` when the transform failed (already counted). The scratch tail past
/// `byte_count` is stale: slice to it before anything reaches the sink.
fn decode_into_scratch(
    payload: &mut BitReader<'_>,
    bit_count: usize,
    actor_net_guid: NetworkGuid,
    stage: &mut Stage<'_>,
) -> Option<usize> {
    let byte_count = TransformVersion::output_byte_count(bit_count);
    if stage.scratch.len() < byte_count {
        stage.scratch.resize(byte_count, 0);
    }

    let seed = vrf_transform::seed_for(bit_count, actor_net_guid.0);
    if stage
        .transform
        .decode_from(payload, bit_count, seed, stage.scratch)
        .is_err()
    {
        stage.stats.transform_failures += 1;
        stage.stats.skipped_bits += bit_count as u64;
        return None;
    }
    Some(byte_count)
}

/// Bits a framing abort charges to `skipped_bits`: from the failing block's
/// first bit to the end of the bunch window (`payload` is that window).
///
/// Never `bits_remaining()`: the header reader consumes its flags and GUIDs,
/// and `read_int_packed` its chunks, before discovering the read runs off the
/// end, so one that expires at the window's end leaves 0 remaining and would
/// charge nothing while a failure counter moves; the overrun arm, too, read
/// bits that framed nothing. Blocks that framed earlier keep their bits. The
/// stream `Err` arms ([`decode_and_parse_rep_layout`]) and `abandon_bunch`
/// follow the same rule one depth down and up.
fn abandoned_from(payload: &BitReader<'_>, block_start: u64) -> u64 {
    payload.len_bits() - block_start
}

/// Decode one RepLayout block payload and walk its field stream.
///
/// Returns `false` only when the payload transform failed: that is already
/// counted, and the caller records its `ParseFailure` event, since only it
/// holds the block's header and position. Every other outcome, a stream
/// failure included, is reported here and returns `true`.
///
/// # A stream `Err` charges the whole block
///
/// This and its ClassNetCache twin charge `bit_count`, never
/// `bits_remaining()`, which is 0 when the last `IntPacked` expires at the
/// block end (see [`abandoned_from`]). The whole block is also what the
/// transform and `with_bit_len` failure paths charge, and what
/// [`NetStats::lost_content_blocks`] counts as lost; nothing downstream
/// re-charges it. Records emitted before the failure are then counted in
/// `fields` / `rpcs` *and* here: the doomed record's start is not returned on
/// `Err`, and double-counting errs in the loud direction. The `Ok` arms
/// report their own abandoned tail.
pub(super) fn decode_and_parse_rep_layout(
    payload: &mut BitReader<'_>,
    bit_count: usize,
    actor_net_guid: NetworkGuid,
    stage: &mut Stage<'_>,
    sink: &mut dyn ReplicationSink,
) -> bool {
    let Some(byte_count) = decode_into_scratch(payload, bit_count, actor_net_guid, stage) else {
        return false;
    };

    let Ok(mut field_reader) = BitReader::with_bit_len(stage.scratch, bit_count as u64) else {
        // Never observed (the scratch is sized by the same bit count); told to
        // the sink so its failure aggregate reconciles with the counter.
        sink.on_stream_failure(StreamFailure {
            kind: StreamKind::RepLayout,
            actor_net_guid,
            bit_count: bit_count as u32,
            function_count: 0,
            consumed_bits: 0,
            remaining_bits: bit_count as u64,
            cause: StreamFailureCause::WindowOpenFailed,
            record_handle: None,
            record_offset: None,
            payload_preserved: false,
        });
        stage.stats.field_stream_failures += 1;
        stage.stats.skipped_bits += bit_count as u64;
        return true;
    };
    let detailed = sink.wants_stream_failure_details();
    let mut walk = field::WalkContext::default();
    let context = if detailed { Some(&mut walk) } else { None };
    let result = field::parse_rep_layout_content_block(&mut field_reader, sink, context);
    let (record_handle, record_offset) = if detailed {
        (walk.last_handle, Some(walk.record_offset))
    } else {
        (None, None)
    };
    match result {
        field::WalkOutcome::Complete {
            count,
            remainder: field::RepLayoutRemainder::None,
        } => {
            stage.stats.fields += u64::from(count);
        }
        field::WalkOutcome::Complete {
            count,
            remainder: field::RepLayoutRemainder::ClassNetCache(tail_bits),
        } => {
            stage.stats.fields += u64::from(count);
            let tail_reader = field_reader.clone();
            let outcome =
                sink.on_rep_layout_tail(actor_net_guid, tail_bits as u32, tail_reader.clone());
            let (cause, payload_preserved) = match outcome {
                RepLayoutTailOutcome::Decoded { rpc_count } => {
                    stage.stats.rpcs += u64::from(rpc_count);
                    return true;
                }
                RepLayoutTailOutcome::Preserved { cause } => (cause, true),
                RepLayoutTailOutcome::Unpreserved { cause } => (cause, false),
            };
            let failure = StreamFailure {
                kind: StreamKind::Rpc,
                actor_net_guid,
                bit_count: tail_bits as u32,
                function_count: 0,
                consumed_bits: 0,
                remaining_bits: tail_bits,
                cause,
                record_handle: None,
                record_offset: None,
                payload_preserved,
            };
            if detailed {
                sink.on_rep_layout_tail_failure_payload(failure, tail_reader);
            }
            sink.on_stream_failure(failure);
            stage.stats.rpc_stream_failures += 1;
            stage.stats.skipped_bits += tail_bits;
            if payload_preserved {
                stage.stats.unresolved_rpc_payloads_preserved += 1;
            }
        }
        field::WalkOutcome::Complete {
            count,
            remainder: field::RepLayoutRemainder::Malformed(abandoned_bits),
        } => {
            stage.stats.fields += u64::from(count);
            let failure = StreamFailure {
                kind: StreamKind::RepLayout,
                actor_net_guid,
                bit_count: bit_count as u32,
                function_count: 0,
                consumed_bits: (bit_count as u64).saturating_sub(abandoned_bits),
                remaining_bits: abandoned_bits,
                cause: StreamFailureCause::AbandonedTail,
                record_handle,
                record_offset,
                payload_preserved: false,
            };
            sink.on_stream_failure(failure);
            if detailed {
                sink.on_stream_failure_payload(failure, &stage.scratch[..byte_count]);
            }
            stage.stats.field_stream_failures += 1;
            stage.stats.skipped_bits += abandoned_bits;
        }
        field::WalkOutcome::Failed { count, .. } => {
            stage.stats.fields += u64::from(count);
            let remaining = field_reader.bits_remaining();
            let failure = StreamFailure {
                kind: StreamKind::RepLayout,
                actor_net_guid,
                bit_count: bit_count as u32,
                function_count: 0,
                consumed_bits: field_reader.position(),
                remaining_bits: remaining,
                cause: StreamFailureCause::ReadError,
                record_handle,
                record_offset,
                payload_preserved: false,
            };
            sink.on_stream_failure(failure);
            if detailed {
                sink.on_stream_failure_payload(failure, &stage.scratch[..byte_count]);
            }
            stage.stats.field_stream_failures += 1;
            // The whole block; see this function's doc.
            stage.stats.skipped_bits += bit_count as u64;
        }
    }
    true
}

/// Decode one ClassNetCache block payload and walk its RPC stream; returns
/// and charges as [`decode_and_parse_rep_layout`] does.
pub(super) fn decode_and_parse_class_net_cache(
    payload: &mut BitReader<'_>,
    bit_count: usize,
    actor_net_guid: NetworkGuid,
    function_count: u32,
    stage: &mut Stage<'_>,
    sink: &mut dyn ReplicationSink,
) -> bool {
    let Some(byte_count) = decode_into_scratch(payload, bit_count, actor_net_guid, stage) else {
        return false;
    };

    let Ok(mut rpc_reader) = BitReader::with_bit_len(stage.scratch, bit_count as u64) else {
        // Never observed; see the RepLayout twin.
        sink.on_stream_failure(StreamFailure {
            kind: StreamKind::Rpc,
            actor_net_guid,
            bit_count: bit_count as u32,
            function_count,
            consumed_bits: 0,
            remaining_bits: bit_count as u64,
            cause: StreamFailureCause::WindowOpenFailed,
            record_handle: None,
            record_offset: None,
            payload_preserved: false,
        });
        stage.stats.rpc_stream_failures += 1;
        stage.stats.skipped_bits += bit_count as u64;
        return true;
    };
    let detailed = sink.wants_stream_failure_details();
    let mut walk = field::WalkContext::default();
    let context = if detailed { Some(&mut walk) } else { None };
    let result =
        field::parse_class_net_cache_content_block(&mut rpc_reader, function_count, sink, context);
    let (record_handle, record_offset) = if detailed {
        (walk.last_handle, Some(walk.record_offset))
    } else {
        (None, None)
    };
    match result {
        field::WalkOutcome::Complete {
            count,
            remainder: abandoned_bits,
        } => {
            stage.stats.rpcs += u64::from(count);
            if abandoned_bits != 0 {
                let failure = StreamFailure {
                    kind: StreamKind::Rpc,
                    actor_net_guid,
                    bit_count: bit_count as u32,
                    function_count,
                    consumed_bits: (bit_count as u64).saturating_sub(abandoned_bits),
                    remaining_bits: abandoned_bits,
                    cause: StreamFailureCause::AbandonedTail,
                    record_handle,
                    record_offset,
                    payload_preserved: false,
                };
                sink.on_stream_failure(failure);
                if detailed {
                    sink.on_stream_failure_payload(failure, &stage.scratch[..byte_count]);
                }
                stage.stats.rpc_stream_failures += 1;
            }
            stage.stats.skipped_bits += abandoned_bits;
        }
        field::WalkOutcome::Failed { count, error } => {
            stage.stats.rpcs += u64::from(count);
            let remaining = rpc_reader.bits_remaining();
            let unresolved = matches!(error, NetError::UnresolvedFunctionCount);
            let cause = if unresolved {
                StreamFailureCause::UnresolvedFunctionCount
            } else {
                StreamFailureCause::ReadError
            };
            let failure = StreamFailure {
                kind: StreamKind::Rpc,
                actor_net_guid,
                bit_count: bit_count as u32,
                function_count,
                consumed_bits: rpc_reader.position(),
                remaining_bits: remaining,
                cause,
                record_handle,
                record_offset,
                payload_preserved: unresolved,
            };
            if unresolved {
                sink.on_unresolved_class_net_cache_payload(failure, &stage.scratch[..byte_count]);
                stage.stats.unresolved_rpc_payloads_preserved += 1;
            } else if detailed {
                sink.on_stream_failure_payload(failure, &stage.scratch[..byte_count]);
            }
            sink.on_stream_failure(failure);
            stage.stats.rpc_stream_failures += 1;
            // The whole block, as in `decode_and_parse_rep_layout`.
            stage.stats.skipped_bits += bit_count as u64;
        }
    }
    true
}

/// Diagnostic-event construction. Without the `diagnostics` feature each
/// function keeps its signature and loses its one statement, so the loop above
/// reads the same in both builds and the empty call optimises away.
mod diagnostics {
    use super::{BunchContext, ContentBlockHeader, NetStats, NetworkGuid};

    #[cfg(feature = "diagnostics")]
    use crate::stats::{
        BunchFlagSnapshot, ContentBlockHeaderSnapshot, DiagnosticEvent, SkipReason,
    };

    /// The header fields every event copies verbatim from the bunch context.
    #[cfg(feature = "diagnostics")]
    fn base(
        ctx: &BunchContext<'_>,
        ch_index: u32,
        actor_net_guid: NetworkGuid,
        block_index: u32,
        consumed_bits: u64,
        remaining_bits: u64,
        bits_skipped: u64,
    ) -> DiagnosticEvent {
        let h = ctx.header;
        DiagnosticEvent {
            reason: SkipReason::HeaderReadError,
            packet_id: h.packet_id,
            bunch_index_in_packet: ctx.ids.bunch_index_in_packet,
            global_bunch_index: ctx.ids.global_bunch_index,
            channel_bunch_index: ctx.ids.channel_bunch_index,
            channel_index: ch_index,
            actor_net_guid: actor_net_guid.0,
            actor_path: None,
            archetype_net_guid: ctx.archetype_net_guid.0,
            class_path: None,
            bunch_flags: BunchFlagSnapshot {
                b_open: h.b_open,
                b_close: h.b_close,
                b_reliable: h.b_reliable,
                b_partial: h.b_partial,
                b_partial_initial: h.b_partial_initial,
                b_partial_final: h.b_partial_final,
                b_has_package_map_exports: h.b_has_package_map_exports,
                b_has_must_be_mapped_guids: h.b_has_must_be_mapped_guids,
                b_dormant: h.b_dormant,
            },
            payload_bit_count: h.payload_bit_count,
            consumed_bits,
            remaining_bits,
            content_block_header: None,
            content_bits: None,
            block_index_in_bunch: block_index,
            bits_skipped,
        }
    }

    #[cfg_attr(not(feature = "diagnostics"), allow(unused_variables))]
    #[allow(clippy::too_many_arguments)]
    pub(super) fn header_read_error(
        stats: &mut NetStats,
        ctx: &BunchContext<'_>,
        ch_index: u32,
        actor_net_guid: NetworkGuid,
        block_index: u32,
        consumed_bits: u64,
        remaining_bits: u64,
        bits_skipped: u64,
    ) {
        #[cfg(feature = "diagnostics")]
        stats.record_diagnostic(|| {
            base(
                ctx,
                ch_index,
                actor_net_guid,
                block_index,
                consumed_bits,
                remaining_bits,
                bits_skipped,
            )
        });
    }

    #[cfg_attr(not(feature = "diagnostics"), allow(unused_variables))]
    #[allow(clippy::too_many_arguments)]
    pub(super) fn content_bits_read_error(
        stats: &mut NetStats,
        ctx: &BunchContext<'_>,
        ch_index: u32,
        actor_net_guid: NetworkGuid,
        block_index: u32,
        consumed_bits: u64,
        remaining_bits: u64,
        bits_skipped: u64,
        header: &ContentBlockHeader,
    ) {
        #[cfg(feature = "diagnostics")]
        stats.record_diagnostic(|| DiagnosticEvent {
            reason: SkipReason::ContentBitsReadError,
            content_block_header: Some(ContentBlockHeaderSnapshot::from(header)),
            ..base(
                ctx,
                ch_index,
                actor_net_guid,
                block_index,
                consumed_bits,
                remaining_bits,
                bits_skipped,
            )
        });
    }

    #[cfg_attr(not(feature = "diagnostics"), allow(unused_variables))]
    #[allow(clippy::too_many_arguments)]
    pub(super) fn content_bits_overrun(
        stats: &mut NetStats,
        ctx: &BunchContext<'_>,
        ch_index: u32,
        actor_net_guid: NetworkGuid,
        block_index: u32,
        consumed_bits: u64,
        remaining_bits: u64,
        bits_skipped: u64,
        header: &ContentBlockHeader,
        content_bits: u32,
    ) {
        #[cfg(feature = "diagnostics")]
        stats.record_diagnostic(|| DiagnosticEvent {
            reason: SkipReason::ContentBitsOverrun {
                declared_content_bits: content_bits,
                available_bits: remaining_bits,
            },
            content_block_header: Some(ContentBlockHeaderSnapshot::from(header)),
            content_bits: Some(content_bits),
            ..base(
                ctx,
                ch_index,
                actor_net_guid,
                block_index,
                consumed_bits,
                remaining_bits,
                bits_skipped,
            )
        });
    }

    /// A block that framed but whose payload transform failed. It charges the
    /// block's `content_bits`, as `decode_into_scratch` did; `consumed_bits`
    /// and `remaining_bits` are taken where the payload begins, as for an
    /// overrun.
    #[cfg_attr(not(feature = "diagnostics"), allow(unused_variables))]
    #[allow(clippy::too_many_arguments)]
    pub(super) fn parse_failure(
        stats: &mut NetStats,
        ctx: &BunchContext<'_>,
        ch_index: u32,
        actor_net_guid: NetworkGuid,
        block_index: u32,
        consumed_bits: u64,
        remaining_bits: u64,
        header: &ContentBlockHeader,
        content_bits: u32,
    ) {
        #[cfg(feature = "diagnostics")]
        stats.record_diagnostic(|| DiagnosticEvent {
            reason: SkipReason::ParseFailure,
            content_block_header: Some(ContentBlockHeaderSnapshot::from(header)),
            content_bits: Some(content_bits),
            ..base(
                ctx,
                ch_index,
                actor_net_guid,
                block_index,
                consumed_bits,
                remaining_bits,
                u64::from(content_bits),
            )
        });
    }
}

#[cfg(all(test, feature = "diagnostics"))]
mod tests {
    use super::*;
    use crate::stats::SkipReason;

    /// The `ParseFailure` event names the block whose transform failed, where
    /// it sat and what it charged. Framing cannot reach this helper on real
    /// input (see `SkipReason::ParseFailure`), so it is pinned directly.
    #[test]
    fn a_parse_failure_event_names_the_block_it_skipped() {
        let bunch = RawBunchHeader {
            packet_id: 7,
            b_reliable: true,
            payload_bit_count: 200,
            ..Default::default()
        };
        let ctx = BunchContext {
            header: &bunch,
            ids: super::super::BunchIds {
                bunch_index_in_packet: 1,
                global_bunch_index: 11,
                channel_bunch_index: 4,
            },
            archetype_net_guid: NetworkGuid(9),
        };
        let block = ContentBlockHeader {
            has_rep_layout: true,
            is_actor: true,
            ..Default::default()
        };
        let mut stats = NetStats::default();

        diagnostics::parse_failure(&mut stats, &ctx, 5, NetworkGuid(2), 3, 120, 80, &block, 33);

        assert_eq!(stats.diagnostics.len(), 1);
        assert_eq!(stats.diagnostics_dropped, 0);
        let ev = &stats.diagnostics[0];
        assert!(matches!(ev.reason, SkipReason::ParseFailure));
        assert_eq!(
            (
                ev.packet_id,
                ev.bunch_index_in_packet,
                ev.global_bunch_index,
                ev.channel_bunch_index
            ),
            (7, 1, 11, 4)
        );
        assert_eq!(
            (ev.channel_index, ev.actor_net_guid, ev.archetype_net_guid),
            (5, 2, 9)
        );
        assert_eq!(ev.block_index_in_bunch, 3);
        assert_eq!((ev.consumed_bits, ev.remaining_bits), (120, 80));
        assert_eq!(ev.content_bits, Some(33));
        assert_eq!(
            ev.bits_skipped, 33,
            "the block's declared length, which is what skipped_bits was charged"
        );
        let snapshot = ev
            .content_block_header
            .as_ref()
            .expect("the block header read before the failure");
        assert!(snapshot.has_rep_layout && snapshot.is_actor);
        assert!(ev.bunch_flags.b_reliable && !ev.bunch_flags.b_open);
        assert_eq!(ev.payload_bit_count, 200);
    }
}
