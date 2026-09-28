//! Aggregate statistics and diagnostics for a replication stream pass.
//!
//! Every discard, skip, or error is counted here. Silent data loss is a bug.
//!
//! # Diagnostics
//!
//! A content block that cannot be framed (its header or `content_bits` does not
//! read, or `content_bits` overruns the bunch), or whose payload transform
//! fails, records a `DiagnosticEvent` with full context: packet, bunch,
//! channel, actor, header fields and the exact failing bit position -- what
//! debugging a new game build's transform needs. A field or RPC stream that
//! fails to walk is counted and reported to the sink instead (see
//! `SkipReason::ParseFailure`).
//!
//! Only the event log is capped (`MAX_DIAGNOSTIC_EVENTS` says why), never the
//! counters. On a healthy replay it stays empty: 02d4d478 prints no events
//! under `validate --diagnostics`. The log exists only with the `diagnostics`
//! feature (see the crate docs' "Features").

#[cfg(feature = "diagnostics")]
use crate::content::ContentBlockHeader;

/// Upper bound on [`NetStats::diagnostics`].
///
/// A replay whose transform is wrong can fail one block per bunch -- 530,401
/// bunches on 02d4d478 -- and at about 200 bytes an event, an unbounded log
/// would hold ~100 MB for a run whose counters already say it failed. This
/// caps it near 3 MB; overflow is counted in [`NetStats::diagnostics_dropped`].
#[cfg(feature = "diagnostics")]
pub const MAX_DIAGNOSTIC_EVENTS: usize = 16_384;

/// Cumulative counters for one replay's replication pass.
#[derive(Debug, Clone, Default)]
pub struct NetStats {
    /// Total packets processed (including malformed ones).
    pub packets: u64,
    /// Packets whose last byte was zero (sentinel missing).
    pub malformed_packets: u64,
    /// Total bunches parsed (header successfully read).
    pub bunches: u64,
    /// Partial-bunch sequence errors (fragment discarded).
    pub partial_errors: u64,
    /// Bunches whose header declared `b_partial`, including rejected ones.
    pub partial_bunches: u64,
    /// Rejected continuations for which no initial fragment was buffered.
    pub partial_missing_initial: u64,
    /// Missing-initial rejects that were marked as the final fragment.
    pub partial_missing_initial_final: u64,
    /// Missing-initial rejects on reliable channels.
    pub partial_missing_initial_reliable: u64,
    /// Payload bits in missing-initial fragments, all discarded before framing.
    pub partial_missing_initial_bits: u64,
    /// Initial fragments that replaced an incomplete assembly on the channel.
    pub partial_overlapping_initial: u64,
    /// Continuations whose reliability or sequence did not match the assembly.
    pub partial_mismatched_continuation: u64,
    /// Non-final fragments rejected because their payload was not byte-aligned.
    pub partial_non_byte_aligned: u64,
    /// Buffered assemblies discarded by a destructive channel close.
    pub partial_channel_close: u64,
    /// Partial fragments accumulated (initial + continuations).
    pub partial_fragments: u64,
    /// Partial bunches that completed successfully.
    pub partial_completed: u64,
    /// Partial bunches still awaiting fragments when the replay ended, moved
    /// only by [`crate::ReplicationReader::finish`]: until the stream stops,
    /// such state cannot be told from reassembly in progress. Not a
    /// `partial_errors`: nothing was out of sequence.
    pub unfinished_partials: u64,
    /// Bits buffered by those unfinished partial bunches, and therefore lost.
    ///
    /// Kept out of [`Self::skipped_bits`] not because they never reached
    /// framing (other partial discards did not either, and are in it) but
    /// because this loss is attributable only once the stream ends, not while
    /// a specific bunch is processed.
    pub unfinished_partial_bits: u64,
    /// Bunch payloads whose header stage failed -- package-map exports,
    /// must-be-mapped GUIDs, or the channel open -- abandoning the bunch.
    pub bunch_header_failures: u64,
    /// Content blocks framed (actor + subobject + deleted).
    pub content_blocks: u64,
    /// Content blocks with RepLayout (property) payloads.
    pub rep_layout_blocks: u64,
    /// Content blocks with ClassNetCache (RPC) payloads.
    pub class_net_cache_blocks: u64,
    /// Content blocks flagged as deleted.
    pub deleted_blocks: u64,
    /// Total fields emitted (handle + payload pairs).
    pub fields: u64,
    /// Total RPC invocations emitted.
    pub rpcs: u64,
    /// Bits not walked into a field or RPC because of one of these: a failed
    /// or abandoned block payload (unresolved ones included, even when
    /// preserved whole), an abandoned bunch, or a refused or discarded partial
    /// fragment. Not every unwalked bit: the losses behind
    /// [`Self::unfinished_partial_bits`], [`Self::unopened_channel_bits`] and
    /// [`Self::rep_layout_export_bunches`] are among those kept out.
    pub skipped_bits: u64,
    /// Content blocks whose header, or `content_bits` IntPacked, did not read:
    /// the framing depths before [`Self::malformed_content_blocks`] can apply,
    /// with the same loss (the rest of the bunch). Part of
    /// [`Self::lost_content_blocks`], so the verdict sees a grammar shift here.
    pub content_block_framing_failures: u64,
    /// Malformed content block payloads (overrun).
    pub malformed_content_blocks: u64,
    /// Content blocks whose payload transform or bit copy failed: framed, but
    /// unreadable, so the whole declared length is skipped.
    pub transform_failures: u64,
    /// Content blocks whose decoded RepLayout field stream failed to walk. A
    /// partly wrong transform can leave framing intact and the streams
    /// unreadable; counting only framing would report a perfect pass rate.
    pub field_stream_failures: u64,
    /// Content blocks whose decoded ClassNetCache (RPC) stream failed to parse.
    pub rpc_stream_failures: u64,
    /// RPC stream failures whose whole decoded payload reached the sink because
    /// no usable function count existed: an inclusive subset of
    /// [`Self::rpc_stream_failures`], naming a preserved, unattributed payload
    /// rather than lost structure.
    pub unresolved_rpc_payloads_preserved: u64,
    /// Actor channels opened.
    pub actor_opens: u64,
    /// Actor channels closed.
    pub actor_closes: u64,
    /// Opens that replaced an actor still open on that channel. Nothing
    /// misframes, so no other counter moves: the replacement stands (the wire
    /// says the new actor owns the channel) and no close is fabricated for the
    /// old one.
    pub channel_reopens_while_open: u64,
    /// Dynamic-actor opens whose payload ended before the mandatory spawn block
    /// (the reference reads it unconditionally). Such an open fails like any
    /// truncated read; this names the shape so a corpus run can say whether it
    /// occurs.
    pub actor_opens_missing_spawn: u64,
    /// Open bunches that did not complete their open on a channel still
    /// holding a live actor, which is retired (no close fabricated) so later
    /// bunches are not framed under its schema. The five arms are listed on
    /// `retire_after_failed_open`; for the clean package-map-export arm, which
    /// is no [`Self::bunch_header_failures`], this alone names the lost open.
    /// Opens carried by partial fragments are not covered (docs/FOLLOWUP.md).
    pub failed_reopens_while_open: u64,
    /// Bunches that reached framing with payload left and no open actor on
    /// their channel (never opened, retired by a failed open, destroyed or
    /// dormant), dropped whole. Their block count is unknowable -- they were
    /// never framed -- so they count as bunches and bits, not in
    /// [`Self::lost_content_blocks`].
    pub bunches_on_unopened_channel: u64,
    /// Payload bits those bunches still held after their preambles; kept out
    /// of [`Self::skipped_bits`] for the reason
    /// [`Self::rep_layout_export_bunches`] is.
    pub unopened_channel_bits: u64,
    /// Bunches refused because a channel-state table was at capacity or a
    /// reliable sequence could not advance representably.
    pub channel_state_limit_failures: u64,
    /// Partial fragments refused because active reassembly state, buffered
    /// bits, checked arithmetic, or allocation reached its bound.
    pub partial_resource_limit_failures: u64,
    /// Package-map export bunches processed.
    pub package_map_exports: u64,
    /// Package-map export bunches carrying a RepLayout export, a variant not
    /// parsed: skipped whole. Kept out of [`Self::skipped_bits`], which the
    /// oracle reads as bits lost across failed content blocks; none is here.
    pub rep_layout_export_bunches: u64,
    /// Net GUIDs exported via package-map.
    pub exported_guids: u64,
    /// Must-be-mapped GUIDs consumed.
    pub must_be_mapped_guids: u64,
    /// Diagnostic events, capped at [`MAX_DIAGNOSTIC_EVENTS`]: the context to
    /// locate a failure in the replay and compare with the C# reference.
    #[cfg(feature = "diagnostics")]
    pub diagnostics: Vec<DiagnosticEvent>,
    /// Events the cap refused. Non-zero means [`Self::diagnostics`] is a
    /// prefix; the failure counters are complete either way.
    #[cfg(feature = "diagnostics")]
    pub diagnostics_dropped: u64,
}

impl NetStats {
    /// Partial errors not explained by the mutually exclusive cause counters: a
    /// residual, so a new error path shows up instead of being filed under the
    /// nearest known cause.
    #[must_use]
    pub fn partial_unclassified_errors(&self) -> u64 {
        self.partial_errors
            .saturating_sub(self.classified_partial_errors())
    }

    /// Cause counts in excess of `partial_errors`, if attribution double-counted.
    #[must_use]
    pub fn partial_overclassified_errors(&self) -> u64 {
        self.classified_partial_errors()
            .saturating_sub(self.partial_errors)
    }

    fn classified_partial_errors(&self) -> u64 {
        self.partial_missing_initial
            + self.partial_overlapping_initial
            + self.partial_mismatched_continuation
            + self.partial_non_byte_aligned
            + self.partial_channel_close
            + self.partial_resource_limit_failures
    }

    /// Add every counter from a completed independent replication pass; events
    /// are appended up to the cap and the excess counted as dropped.
    pub fn absorb(&mut self, other: &mut Self) {
        self.packets += other.packets;
        self.malformed_packets += other.malformed_packets;
        self.bunches += other.bunches;
        self.partial_errors += other.partial_errors;
        self.partial_bunches += other.partial_bunches;
        self.partial_missing_initial += other.partial_missing_initial;
        self.partial_missing_initial_final += other.partial_missing_initial_final;
        self.partial_missing_initial_reliable += other.partial_missing_initial_reliable;
        self.partial_missing_initial_bits += other.partial_missing_initial_bits;
        self.partial_overlapping_initial += other.partial_overlapping_initial;
        self.partial_mismatched_continuation += other.partial_mismatched_continuation;
        self.partial_non_byte_aligned += other.partial_non_byte_aligned;
        self.partial_channel_close += other.partial_channel_close;
        self.partial_fragments += other.partial_fragments;
        self.partial_completed += other.partial_completed;
        self.unfinished_partials += other.unfinished_partials;
        self.unfinished_partial_bits += other.unfinished_partial_bits;
        self.bunch_header_failures += other.bunch_header_failures;
        self.content_blocks += other.content_blocks;
        self.rep_layout_blocks += other.rep_layout_blocks;
        self.class_net_cache_blocks += other.class_net_cache_blocks;
        self.deleted_blocks += other.deleted_blocks;
        self.fields += other.fields;
        self.rpcs += other.rpcs;
        self.skipped_bits += other.skipped_bits;
        self.content_block_framing_failures += other.content_block_framing_failures;
        self.malformed_content_blocks += other.malformed_content_blocks;
        self.transform_failures += other.transform_failures;
        self.field_stream_failures += other.field_stream_failures;
        self.rpc_stream_failures += other.rpc_stream_failures;
        self.unresolved_rpc_payloads_preserved += other.unresolved_rpc_payloads_preserved;
        self.actor_opens += other.actor_opens;
        self.actor_closes += other.actor_closes;
        self.channel_reopens_while_open += other.channel_reopens_while_open;
        self.actor_opens_missing_spawn += other.actor_opens_missing_spawn;
        self.failed_reopens_while_open += other.failed_reopens_while_open;
        self.bunches_on_unopened_channel += other.bunches_on_unopened_channel;
        self.unopened_channel_bits += other.unopened_channel_bits;
        self.channel_state_limit_failures += other.channel_state_limit_failures;
        self.partial_resource_limit_failures += other.partial_resource_limit_failures;
        self.package_map_exports += other.package_map_exports;
        self.rep_layout_export_bunches += other.rep_layout_export_bunches;
        self.exported_guids += other.exported_guids;
        self.must_be_mapped_guids += other.must_be_mapped_guids;
        #[cfg(feature = "diagnostics")]
        {
            let available = MAX_DIAGNOSTIC_EVENTS.saturating_sub(self.diagnostics.len());
            let keep = available.min(other.diagnostics.len());
            self.diagnostics.extend(other.diagnostics.drain(..keep));
            self.diagnostics_dropped += other.diagnostics_dropped + other.diagnostics.len() as u64;
            other.diagnostics.clear();
        }
    }

    /// Content blocks whose payload never reached the exported tables: the one
    /// "did anything get lost" number, deliberately not the oracle's pass rate.
    /// Five terms, one per failure depth, because framing can look fine while
    /// the payload inside is unreadable. Published as manifest.json
    /// `quality.content_blocks_lost`; non-zero means the tables miss
    /// replicated state, so a recount of them undercounts by an unknown size.
    ///
    /// RPC failures enter netted ([`Self::rpc_payloads_lost`]): an unresolved
    /// ClassNetCache block is exported whole as one reserved row
    /// (`UNRESOLVED_CLASS_NET_CACHE_PAYLOAD_FIELD_NAME` in `vrf-export`), so it
    /// is not a loss. On 02d4d478 that is 6,490 `RPC unresolved/raw` blocks
    /// and 0 lost (`validate` at 061155a); an oracle without the netting
    /// scored the same shape 98.94% (7,889 such blocks, 2026-08-18).
    pub fn lost_content_blocks(&self) -> u64 {
        self.content_block_framing_failures
            + self.malformed_content_blocks
            + self.transform_failures
            + self.field_stream_failures
            + self.rpc_payloads_lost()
    }

    /// RPC stream failures whose payload was not preserved: the RPC share of
    /// [`Self::lost_content_blocks`]. `saturating_sub`: the two counters move
    /// on different paths, and more preserved than failed must read 0, not wrap.
    #[must_use]
    pub fn rpc_payloads_lost(&self) -> u64 {
        self.rpc_stream_failures
            .saturating_sub(self.unresolved_rpc_payloads_preserved)
    }

    /// Record one diagnostic event, or count it dropped if the log is full; the
    /// closure builds the event only when it will be kept.
    #[cfg(feature = "diagnostics")]
    pub fn record_diagnostic(&mut self, event: impl FnOnce() -> DiagnosticEvent) {
        if self.diagnostics.len() < MAX_DIAGNOSTIC_EVENTS {
            self.diagnostics.push(event());
        } else {
            self.diagnostics_dropped += 1;
        }
    }
}

/// Why a content block or bunch tail was skipped.
#[cfg(feature = "diagnostics")]
#[derive(Debug, Clone)]
pub enum SkipReason {
    /// `content_bits` (from `ReadIntPacked`) exceeded `bits_remaining` in the
    /// bunch payload -- the stream is irrecoverably misaligned for this bunch.
    ContentBitsOverrun {
        /// The declared content payload size that was too large.
        declared_content_bits: u32,
        /// How many bits actually remained in the bunch payload.
        available_bits: u64,
    },
    /// Reading the content block header failed (e.g. not enough bits for a
    /// GUID or the deletion flags). The remaining bunch payload is discarded.
    HeaderReadError,
    /// Reading the `IntPacked` content-bits field itself failed.
    ContentBitsReadError,
    /// The block framed, but its payload transform (the copy out of the bunch
    /// and the build's decode) failed; only that block's `content_bits` are
    /// skipped.
    ///
    /// Unreachable from framing by construction: framing refuses a
    /// `content_bits` larger than the bunch has left
    /// ([`Self::ContentBitsOverrun`]) and sizes the scratch buffer to the
    /// block, so `validate`'s skip breakdown reads 0 here unless a guard
    /// changes. Stream failures are not events: they go to the sink's
    /// `on_stream_failure`, and a healthy replay has thousands (6,490
    /// `RPC unresolved/raw` on 02d4d478, `validate` at 061155a).
    ParseFailure,
}

/// Full context snapshot at the point a content block was skipped or malformed,
/// so one event dump can identify the root cause.
#[cfg(feature = "diagnostics")]
#[derive(Debug, Clone)]
pub struct DiagnosticEvent {
    /// Why this event was recorded.
    pub reason: SkipReason,
    /// Packet index (0-based, global across the replay).
    pub packet_id: i32,
    /// Bunch index within this packet (0-based).
    pub bunch_index_in_packet: u32,
    /// Global bunch index across the entire replay (0-based).
    pub global_bunch_index: u64,
    /// Per-channel bunch count (how many bunches this channel has seen).
    pub channel_bunch_index: u64,
    /// Channel index.
    pub channel_index: u32,
    /// Actor network GUID on this channel.
    pub actor_net_guid: u32,
    /// Always `None`: framing does not resolve paths (the sink owns the GUID
    /// cache), and nothing fills this in later.
    pub actor_path: Option<String>,
    /// Archetype GUID the channel's open read; 0 for a static actor (no spawn
    /// block), the wire's no-object GUID rather than a failed read.
    pub archetype_net_guid: u32,
    /// Always `None`, for the reason [`Self::actor_path`] is.
    pub class_path: Option<String>,
    /// Bunch header flags.
    pub bunch_flags: BunchFlagSnapshot,
    /// Declared bunch payload bit count (from bunch header).
    pub payload_bit_count: i32,
    /// Bits consumed in the bunch payload before this event.
    pub consumed_bits: u64,
    /// Bits remaining in the bunch payload at the point of failure.
    pub remaining_bits: u64,
    /// Content block header (if successfully read before the failure).
    pub content_block_header: Option<ContentBlockHeaderSnapshot>,
    /// The `content_bits` value read from `IntPacked` (if available).
    pub content_bits: Option<u32>,
    /// Which content block within this bunch (0-based).
    pub block_index_in_bunch: u32,
    /// Bits this event charged to [`NetStats::skipped_bits`]: for a framing
    /// abort, from the failing block's first bit to the end of the bunch (so it
    /// can exceed `remaining_bits`); for a [`SkipReason::ParseFailure`], the
    /// block's `content_bits`.
    pub bits_skipped: u64,
}

/// Snapshot of all bunch header flags for diagnostic reporting.
#[cfg(feature = "diagnostics")]
#[derive(Debug, Clone)]
pub struct BunchFlagSnapshot {
    pub b_open: bool,
    pub b_close: bool,
    pub b_reliable: bool,
    pub b_partial: bool,
    pub b_partial_initial: bool,
    pub b_partial_final: bool,
    pub b_has_package_map_exports: bool,
    pub b_has_must_be_mapped_guids: bool,
    pub b_dormant: bool,
}

/// Snapshot of the content block header fields for diagnostic reporting.
#[cfg(feature = "diagnostics")]
#[derive(Debug, Clone)]
pub struct ContentBlockHeaderSnapshot {
    pub has_rep_layout: bool,
    pub is_actor: bool,
    pub object_net_guid: u32,
    pub is_stably_named: bool,
    pub is_deleted: bool,
    pub class_net_guid: u32,
    pub outer_net_guid: u32,
    pub delete_flags: u8,
}

#[cfg(feature = "diagnostics")]
impl From<&ContentBlockHeader> for ContentBlockHeaderSnapshot {
    fn from(h: &ContentBlockHeader) -> Self {
        Self {
            has_rep_layout: h.has_rep_layout,
            is_actor: h.is_actor,
            object_net_guid: h.object_net_guid.0,
            is_stably_named: h.is_stably_named,
            is_deleted: h.is_deleted,
            class_net_guid: h.class_net_guid.0,
            outer_net_guid: h.outer_net_guid.0,
            delete_flags: h.delete_flags,
        }
    }
}

/// Loss-accounting tests, outside the `diagnostics`-gated module below:
/// `lost_content_blocks` is read by the manifest in every build.
#[cfg(test)]
mod loss_tests {
    use super::*;

    /// Each depth listed here reaches the loss total, and they add rather than
    /// mask. The fifth term, `content_block_framing_failures`, is pinned
    /// through the verdict by vrfkit's oracle test
    /// `every_unfinished_or_payload_failure_prevents_a_pass`.
    #[test]
    fn every_failure_depth_reaches_the_loss_total() {
        for (label, stats) in [
            (
                "malformed framing",
                NetStats {
                    malformed_content_blocks: 3,
                    ..NetStats::default()
                },
            ),
            (
                "transform failed",
                NetStats {
                    transform_failures: 3,
                    ..NetStats::default()
                },
            ),
            (
                "field stream failed",
                NetStats {
                    field_stream_failures: 3,
                    ..NetStats::default()
                },
            ),
            (
                "rpc payload lost",
                NetStats {
                    rpc_stream_failures: 3,
                    ..NetStats::default()
                },
            ),
        ] {
            assert_eq!(
                stats.lost_content_blocks(),
                3,
                "{label} must reach the loss total on its own"
            );
        }

        let all_four = NetStats {
            malformed_content_blocks: 1,
            transform_failures: 2,
            field_stream_failures: 4,
            rpc_stream_failures: 8,
            ..NetStats::default()
        };
        assert_eq!(
            all_four.lost_content_blocks(),
            15,
            "the four depths add; a total that matches one of them alone would hide the rest"
        );
    }

    /// The reference-replay shape: unattributed blocks whose payloads were all
    /// preserved as reserved rows lose nothing (7,889 is the 2026-08-18 count;
    /// see `lost_content_blocks` for today's). One not preserved is a loss.
    #[test]
    fn preserved_unresolved_rpc_payloads_are_not_a_loss() {
        let stats = NetStats {
            rpc_stream_failures: 7889,
            unresolved_rpc_payloads_preserved: 7889,
            ..NetStats::default()
        };
        assert_eq!(stats.rpc_payloads_lost(), 0);
        assert_eq!(stats.lost_content_blocks(), 0);

        let partly_preserved = NetStats {
            rpc_stream_failures: 7889,
            unresolved_rpc_payloads_preserved: 7000,
            ..NetStats::default()
        };
        assert_eq!(partly_preserved.rpc_payloads_lost(), 889);
        assert_eq!(
            partly_preserved.lost_content_blocks(),
            889,
            "a failure whose payload was NOT preserved is a real loss"
        );
    }

    /// The counters are incremented on different paths. More preserved than
    /// failed must read as zero loss, never as a wrapped u64.
    #[test]
    fn more_preserved_than_failed_does_not_wrap() {
        let stats = NetStats {
            rpc_stream_failures: 1,
            unresolved_rpc_payloads_preserved: 5,
            ..NetStats::default()
        };
        assert_eq!(stats.rpc_payloads_lost(), 0);
        assert_eq!(stats.lost_content_blocks(), 0);
    }

    /// Loud but normal counters are not loss: 02d4d478 skips 18,217,181 bits
    /// (`validate` at 061155a; the literal below is an older count) with a
    /// complete export. Counting them would mark every healthy replay lossy.
    #[test]
    fn normal_noise_counters_are_not_loss() {
        let stats = NetStats {
            skipped_bits: 19_135_006,
            partial_fragments: 4096,
            deleted_blocks: 128,
            actor_closes: 1799,
            must_be_mapped_guids: 64,
            ..NetStats::default()
        };
        assert_eq!(stats.lost_content_blocks(), 0);
    }
}

#[cfg(all(test, feature = "diagnostics"))]
mod tests {
    use super::*;

    fn dummy_event(block_index: u32) -> DiagnosticEvent {
        DiagnosticEvent {
            reason: SkipReason::HeaderReadError,
            packet_id: 0,
            bunch_index_in_packet: 0,
            global_bunch_index: 0,
            channel_bunch_index: 0,
            channel_index: 0,
            actor_net_guid: 0,
            actor_path: None,
            archetype_net_guid: 0,
            class_path: None,
            bunch_flags: BunchFlagSnapshot {
                b_open: false,
                b_close: false,
                b_reliable: false,
                b_partial: false,
                b_partial_initial: false,
                b_partial_final: false,
                b_has_package_map_exports: false,
                b_has_must_be_mapped_guids: false,
                b_dormant: false,
            },
            payload_bit_count: 0,
            consumed_bits: 0,
            remaining_bits: 0,
            content_block_header: None,
            content_bits: None,
            block_index_in_bunch: block_index,
            bits_skipped: 0,
        }
    }

    /// Past [`MAX_DIAGNOSTIC_EVENTS`] the log stops growing and the overflow is
    /// counted, not dropped quietly.
    #[test]
    fn diagnostics_are_capped_and_the_overflow_is_counted() {
        let mut stats = NetStats::default();
        for i in 0..(MAX_DIAGNOSTIC_EVENTS as u32 + 5) {
            stats.record_diagnostic(|| dummy_event(i));
        }
        assert_eq!(stats.diagnostics.len(), MAX_DIAGNOSTIC_EVENTS);
        assert_eq!(stats.diagnostics_dropped, 5);
        assert_eq!(
            stats.diagnostics[0].block_index_in_bunch, 0,
            "the log keeps the earliest events, which are the ones that explain the rest"
        );
        assert_eq!(
            stats.diagnostics[MAX_DIAGNOSTIC_EVENTS - 1].block_index_in_bunch,
            MAX_DIAGNOSTIC_EVENTS as u32 - 1
        );
    }

    #[test]
    fn unknown_partial_error_paths_remain_visible_as_a_residual() {
        let stats = NetStats {
            partial_errors: 9,
            partial_missing_initial: 2,
            partial_overlapping_initial: 1,
            partial_mismatched_continuation: 1,
            partial_non_byte_aligned: 1,
            partial_channel_close: 1,
            partial_resource_limit_failures: 1,
            ..Default::default()
        };
        assert_eq!(stats.partial_unclassified_errors(), 2);
        assert_eq!(stats.partial_overclassified_errors(), 0);

        let over = NetStats {
            partial_errors: 1,
            partial_missing_initial: 2,
            ..Default::default()
        };
        assert_eq!(over.partial_unclassified_errors(), 0);
        assert_eq!(over.partial_overclassified_errors(), 1);
    }

    /// Every counter at its own multiple of `n`, so absorbing `counted(1)` twice
    /// must give `counted(2)`. The literal names every field: a new counter does
    /// not compile until listed, and one `absorb` drops or overwrites fails.
    fn counted(n: u64) -> NetStats {
        NetStats {
            packets: n,
            malformed_packets: 2 * n,
            bunches: 3 * n,
            partial_errors: 4 * n,
            partial_bunches: 35 * n,
            partial_missing_initial: 36 * n,
            partial_missing_initial_final: 41 * n,
            partial_missing_initial_reliable: 42 * n,
            partial_missing_initial_bits: 43 * n,
            partial_overlapping_initial: 37 * n,
            partial_mismatched_continuation: 38 * n,
            partial_non_byte_aligned: 39 * n,
            partial_channel_close: 40 * n,
            partial_fragments: 5 * n,
            partial_completed: 6 * n,
            unfinished_partials: 7 * n,
            unfinished_partial_bits: 8 * n,
            bunch_header_failures: 9 * n,
            content_blocks: 10 * n,
            rep_layout_blocks: 11 * n,
            class_net_cache_blocks: 12 * n,
            deleted_blocks: 13 * n,
            fields: 14 * n,
            rpcs: 15 * n,
            skipped_bits: 16 * n,
            content_block_framing_failures: 34 * n,
            malformed_content_blocks: 17 * n,
            transform_failures: 18 * n,
            field_stream_failures: 19 * n,
            rpc_stream_failures: 20 * n,
            unresolved_rpc_payloads_preserved: 21 * n,
            actor_opens: 22 * n,
            actor_closes: 23 * n,
            channel_reopens_while_open: 24 * n,
            actor_opens_missing_spawn: 25 * n,
            failed_reopens_while_open: 44 * n,
            bunches_on_unopened_channel: 45 * n,
            unopened_channel_bits: 46 * n,
            channel_state_limit_failures: 26 * n,
            partial_resource_limit_failures: 27 * n,
            package_map_exports: 28 * n,
            rep_layout_export_bunches: 29 * n,
            exported_guids: 30 * n,
            must_be_mapped_guids: 31 * n,
            diagnostics: vec![dummy_event(32); n as usize],
            diagnostics_dropped: 33 * n,
        }
    }

    #[test]
    fn absorbing_checkpoint_stats_keeps_every_counter() {
        let mut totals = NetStats::default();
        totals.absorb(&mut counted(1));
        assert_eq!(format!("{totals:?}"), format!("{:?}", counted(1)));
        totals.absorb(&mut counted(1));
        assert_eq!(totals.packets, 2);
        assert_eq!(totals.diagnostics.len(), 2);
        assert_eq!(totals.diagnostics_dropped, 66);
        assert_eq!(
            format!("{totals:?}"),
            format!("{:?}", counted(2)),
            "every counter, the event log and its dropped count add up"
        );
    }
}
