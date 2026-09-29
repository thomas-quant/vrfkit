//! Sink implementation connecting `vrf-net` events to `vrf-export` records.
//!
//! # Design: no skipping
//!
//! Every field and RPC is emitted, even when its group path or field name does
//! not resolve: then `group_path = "<unknown:{guid}>"` and `field_name = None`.
//! A block an unresolved ClassNetCache function table leaves unsplittable is
//! emitted whole as one marked preservation row, never as fabricated fields, so
//! the Parquet output stays a **lossless** representation of the stream.
//!
//! This module holds the sink, the per-packet record buffers and the state that
//! must outlive a packet; each submodule holds one concern.
//!
//! # What the sink costs
//!
//! `vrfkit validate` runs this whole path and writes no file, so it measures
//! the sink alone: docs/PERFORMANCE_NOTES.md#what-the-whole-sink-costs.

mod blobs;
mod failure_stats;
mod intern;
mod measured_routes;
mod paths;
mod rpc;
mod stream;
mod totals;

pub use failure_stats::FailureAggregate;
pub(crate) use totals::SinkTotals;

use std::sync::Arc;

use smallvec::SmallVec;
use vrf_decode::{
    ArrayDecodeStats, GroupHashState, OVERLAY_HANDLE_TABLE, OVERLAY_TABLE, OverlayStats,
    OverlayTable, group_hash_state,
};
use vrf_export::{
    ActorRecord, CheckpointBlockRecord, CheckpointIdentity, FieldRecord, MovementRecord,
    PartialRecord,
};
use vrf_net::net_guid::GuidPathSink;
use vrf_net::types::NetworkGuid;
use vrf_schema::{FxHashMap, NetGuidCache};

use intern::NameInterner;
use measured_routes::{MeasuredArrayRoute, MeasuredArrayRoutes};
use paths::{BlockPathMemo, ChannelArchetype};
use rpc::RpcParamGroupMemo;

/// Static overlay table.
static TABLE: OverlayTable = OverlayTable::with_handles(&OVERLAY_TABLE, &OVERLAY_HANDLE_TABLE);

/// How many stream-failure lines to retain. See [`ChannelState::stream_failures`].
const MAX_STREAM_FAILURE_RECORDS: usize = 32;

/// One BombPlayerState actor's identity for the manifest `players` array:
/// `Subject`, the account UUID, and `SpawnedCharacter`, the character actor
/// NetGUID (equal to `movement.character_net_guid`). Together they join every
/// actor-keyed table to a stable account -- what `playerLoadouts`'
/// `characterId` cannot do when two players pick the same agent.
#[derive(Debug, Clone, Default)]
pub struct PlayerIdentity {
    pub subject: Option<String>,
    pub character_net_guid: Option<u32>,
}

/// State that must outlive a packet, across packets and chunks. `ExportSink` is
/// rebuilt for every packet (it borrows the `NetGuidCache` mutably), so the
/// channel archetypes later ClassNetCache blocks resolve through, the two memos
/// and the name pool live here; rebuilt half a million times they would never
/// warm up.
#[derive(Debug, Clone, Default)]
pub struct ChannelState {
    /// channel_index -> archetype, stamped with its actor ([`ChannelArchetype`]).
    archetypes: FxHashMap<u32, ChannelArchetype>,
    rpc_param_groups: RpcParamGroupMemo,
    block_paths: BlockPathMemo,
    names: NameInterner,
    /// Bumped when the archetype map changes, the one resolution input that
    /// lives here rather than in the cache: one of [`BlockPathMemo`]'s three
    /// stamps (see [`paths`]).
    resolution_generation: u64,
    /// One line per block that framed and decoded but whose inner stream did not
    /// walk, kept here because the sink dies with its packet. Capped at
    /// [`MAX_STREAM_FAILURE_RECORDS`]: a wrong transform fails nearly every
    /// block and the first few dozen say it all; the population is in `failures`.
    stream_failures: Vec<String>,
    /// Failure aggregation is opt-in for `diag`; ordinary decode and export
    /// paths keep this as `None` and pay no map or payload-sampling cost.
    failures: Option<FailureAggregate>,
    /// PlayerState actor NetGUID -> [`PlayerIdentity`], filled by `on_field`.
    players: FxHashMap<u32, PlayerIdentity>,
}

impl ChannelState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one stream failure, up to the cap.
    pub fn push_stream_failure(&mut self, line: String) {
        if self.stream_failures.len() < MAX_STREAM_FAILURE_RECORDS {
            self.stream_failures.push(line);
        }
    }

    #[must_use]
    pub fn stream_failures(&self) -> &[String] {
        &self.stream_failures
    }

    /// Enable bounded failure aggregation for a diagnostic pass.
    pub fn enable_failure_aggregate(&mut self, retain_payloads: bool) {
        self.failures = Some(FailureAggregate::new(retain_payloads));
    }

    /// Take the failure aggregate out, leaving it empty: the checkpoint pass
    /// drains each chunk's channel state into the caller's totals.
    #[must_use]
    pub fn take_failure_aggregate(&mut self) -> FailureAggregate {
        self.failures.take().unwrap_or_default()
    }

    /// Captured player identities, for the manifest `players` array. Gated like
    /// its only caller, the `export`-only `driver`, or it is dead code without.
    #[cfg(feature = "export")]
    #[must_use]
    pub fn players(&self) -> &FxHashMap<u32, PlayerIdentity> {
        &self.players
    }

    /// Declare that something group-path resolution reads has changed. The
    /// only callers are `set_channel_archetype` and `retire_channel_archetype`
    /// in `paths` (the cache's GUID maps have `NetGuidCache::guid_generation`);
    /// a resolution input no stamp covers is silent byte movement, not a test
    /// failure.
    fn note_resolution_input_changed(&mut self) {
        self.resolution_generation = self.resolution_generation.wrapping_add(1);
    }
}

/// Counters the driver aggregates across packets.
#[derive(Debug, Clone, Default)]
pub struct ExportStats {
    pub fields_emitted: u64,
    pub rpcs_emitted: u64,
    pub actor_opens: u64,
    pub actor_closes: u64,
    pub content_blocks: u64,
    pub overlay: OverlayStats,
    pub array: ArrayDecodeStats,
    /// Exact 24-bit `TrackedRewards` windows with the measured opaque zero
    /// byte. They preserve their parent raw row and emit no child rows.
    pub tracked_rewards_opaque_empty_variants: u64,
    /// Empty ActiveBlinds deltas whose one trailing zero IntPacked the strict
    /// walker was spared (`active_blind_array_bits`). The parent row keeps the
    /// byte; uncounted, a build that made such trailers common would move no
    /// other number.
    pub active_blinds_empty_trailers: u64,
    /// EffectContainer blobs turned into a `value_str` JSON array: the only
    /// signal this decoder worked, since the overlay buckets are filled before
    /// the additive pass and a success moves no other counter (a silent
    /// improvement misleads like a silent loss). Failures land in
    /// `overlay.decoded_err`.
    pub effect_blobs_decoded: u64,

    /// Struct-blob (`RoundResults`, `TeamEconomy`, `RoundInfos`) parent rows
    /// whose dedicated decoder produced elements.
    pub struct_blobs_decoded: u64,

    /// Item NetGUID rows from the `MultiItemSlot.MultiContents` decoder, one per
    /// item reference. The parent stays `Raw` (`not_in_table`), so as with
    /// [`Self::effect_blobs_decoded`] this is the decoder's only signal.
    pub multi_contents_items_emitted: u64,

    /// RPC rows from the fc=34 ClassNetCache decoder for
    /// `AbilitiesAndBuffsComponent` -- extra rows beside an unresolved
    /// payload's preservation row, and verified RepLayout-tail bodies. The only
    /// signal that decoder produced RPC structure.
    pub cnc_rpcs_emitted: u64,

    /// Unresolved `AbilitiesAndBuffsComponent` payloads offered to that
    /// brute-force walk: the denominator [`Self::cnc_rpcs_emitted`] does not
    /// have, since that counter also counts RepLayout-tail decodes.
    pub cnc_bruteforce_payloads_attempted: u64,

    /// Of those, payloads the fc=34 walk did not fit: no RPC row, only the
    /// preservation row, which keeps every bit. 34 is empirical, so if an update
    /// breaks the walk `CNC RPC rows` shrinks and this says why. Zero on the
    /// replays measured when it was added.
    pub cnc_bruteforce_payloads_unwalked: u64,

    /// Post-RepLayout ClassNetCache tails decoded under verified component
    /// provenance and strict one-RPC framing.
    pub rep_layout_cnc_tails_decoded: u64,

    /// Post-RepLayout tails retained whole because their provenance or framing
    /// was not sufficient for the verified decoder.
    pub rep_layout_cnc_tails_preserved: u64,

    /// Struct-blob decodes that returned an error. Additive, so a failure costs
    /// no rows or bits, but it must be seen: uncounted, 13.02 moving
    /// `RoundResults` from handle 93 to 81 exported as a clean run with no
    /// match score in the Parquet.
    pub struct_blobs_failed: u64,

    /// The first failure verbatim, so the summary can name the member and handle.
    pub struct_blob_first_error: Option<String>,

    /// Movement-decode problems: soft errors counted per occurrence
    /// (`RpcDecodeResult.error_count`) plus hard `Err`s. Without it a changed
    /// section format shortens `movement.parquet` with every counter clean.
    pub movement_rpc_errors: u64,

    /// The first movement-decode problem verbatim, for the summary to name.
    pub movement_first_error: Option<String>,

    /// Sections in a `movementBitCount`-sized window that stopped with bits
    /// unread, and those bits (`vrf_movement::RpcDecodeResult::sized_section_tails`).
    /// A soft tally: the batch still counts as clean, so no Parquet row moves.
    pub movement_sized_section_tails: u64,
    pub movement_sized_section_tail_bits: u64,
    /// The same for windows that ran to the end of the component stream, where
    /// what follows may be other data: kept apart, never summed with the above.
    pub movement_open_section_tails: u64,
    pub movement_open_section_tail_bits: u64,
    /// Byte-wrapped movement streams and the bits after their envelopes,
    /// which nothing reads (`vrf_movement::RpcDecodeResult::envelope_trailer_bits`):
    /// 24 per stream on every measured replay. A soft tally like the tails.
    pub movement_envelope_trailers: u64,
    pub movement_envelope_trailer_bits: u64,

    /// RPC payloads whose parameter loop broke on a malformed read before the
    /// zero-handle terminator. The walk keeps its rows and reports success, so
    /// this is the only signal of an abandoned walk. Zero on valid replays.
    pub truncated_rpcs: u64,

    /// Bits after an RPC's zero-handle terminator beyond the one trailing
    /// alignment bit `FunctionParameters` allows. Counted, not rejected, and not
    /// lost: such a payload also gets a whole-payload row under the function's
    /// name, so every counted bit is in `raw_bits`.
    ///
    /// 259ed10 corpus audit (1,018 unique replays, `--checkpoints`): the main
    /// pass is nonzero in 21 of 24 builds, 26,766 bits (12.07, 3 replays) to
    /// 9,329,665 (13.05, 401 replays), and zero in the three single-fixture
    /// builds (12.10, 12.11, 13.00) and on some replays; the checkpoint pass is
    /// zero in every build. One source: `ActiveGameplayEffects` under
    /// `/Script/ShooterGame.AresAbilitySystemComponent_ClassNetCache`, a
    /// payload that continues past its zero handle (ClassNetCache framing also
    /// carries custom-delta properties; see `ActiveGameplayEffects` in
    /// docs/DATA.md). What those bits encode is not established; a suffix on
    /// any other handle would be new. Method (2026-09-28): a Python re-walk of
    /// the grammar over every bare-named ClassNetCache row with a payload in
    /// `fields.parquet` and `checkpoint_fields.parquet` matched this counter in
    /// 90 of 90 (export, stream) pairs (two exports per build, one per
    /// single-fixture build); in 40 more exports 1,416 of 32,391 such rows had a
    /// suffix, all `ActiveGameplayEffects`, 128 to 1,239 bits each.
    pub rpc_suffix_bits_dropped: u64,

    /// Flattened array leaves whose resolved type failed to decode; the raw leaf
    /// rows are still emitted, this counts the typed values lost.
    pub array_leaf_decode_errors: u64,

    /// Typed world-location children emitted from the guarded map-click array.
    pub targeting_world_locations_decoded: u64,
}

impl ExportStats {
    /// Record a movement-RPC decode outcome: soft per-update errors and hard
    /// `Err`s both count, and the first is kept verbatim for the summary.
    pub fn record_movement_decode(
        &mut self,
        result: Result<&vrf_movement::RpcDecodeResult, &vrf_movement::MovementError>,
    ) {
        match result {
            Ok(r) => {
                // No `..`: a counter added to the decoder's result must be
                // read here, or it would reach nothing.
                let vrf_movement::RpcDecodeResult {
                    total_moves: _,
                    update_count: _,
                    error_count,
                    sized_section_tails,
                    sized_section_tail_bits,
                    open_section_tails,
                    open_section_tail_bits,
                    envelope_trailer_streams,
                    envelope_trailer_bits,
                } = *r;
                self.movement_sized_section_tails += u64::from(sized_section_tails);
                self.movement_sized_section_tail_bits += sized_section_tail_bits;
                self.movement_open_section_tails += u64::from(open_section_tails);
                self.movement_open_section_tail_bits += open_section_tail_bits;
                self.movement_envelope_trailers += u64::from(envelope_trailer_streams);
                self.movement_envelope_trailer_bits += envelope_trailer_bits;
                if error_count > 0 {
                    self.movement_rpc_errors = self
                        .movement_rpc_errors
                        .saturating_add(u64::from(error_count));
                    self.movement_first_error.get_or_insert(format!(
                        "{error_count} movement decode error(s) in one RPC batch"
                    ));
                }
            }
            Err(e) => {
                self.movement_rpc_errors = self.movement_rpc_errors.saturating_add(1);
                self.movement_first_error
                    .get_or_insert_with(|| e.to_string());
            }
        }
    }
}

#[cfg(test)]
mod movement_stats_tests {
    use super::ExportStats;
    use vrf_movement::{MovementError, RpcDecodeResult};

    fn ok(
        total_moves: u32,
        update_count: u32,
        error_count: u32,
    ) -> Result<RpcDecodeResult, MovementError> {
        Ok(RpcDecodeResult {
            total_moves,
            update_count,
            error_count,
            ..Default::default()
        })
    }

    fn with_tails(
        error_count: u32,
        sized: (u32, u64),
        open: (u32, u64),
    ) -> Result<RpcDecodeResult, MovementError> {
        Ok(RpcDecodeResult {
            total_moves: 1,
            update_count: 1,
            error_count,
            sized_section_tails: sized.0,
            sized_section_tail_bits: sized.1,
            open_section_tails: open.0,
            open_section_tail_bits: open.1,
            ..Default::default()
        })
    }

    fn with_trailers(
        error_count: u32,
        streams: u32,
        bits: u64,
    ) -> Result<RpcDecodeResult, MovementError> {
        Ok(RpcDecodeResult {
            total_moves: 1,
            update_count: streams,
            error_count,
            envelope_trailer_streams: streams,
            envelope_trailer_bits: bits,
            ..Default::default()
        })
    }

    /// Envelope trailers are summed from every `Ok` decode like the tails, and
    /// never count as a movement error.
    #[test]
    fn envelope_trailers_are_summed_from_every_ok_decode_and_are_not_errors() {
        let mut s = ExportStats::default();
        s.record_movement_decode(with_trailers(0, 3, 72).as_ref());
        s.record_movement_decode(with_trailers(1, 2, 37).as_ref());
        assert_eq!(s.movement_envelope_trailers, 5);
        assert_eq!(s.movement_envelope_trailer_bits, 109);
        assert_eq!(s.movement_rpc_errors, 1, "only the soft error");
        assert_eq!(
            (
                s.movement_sized_section_tails,
                s.movement_open_section_tails
            ),
            (0, 0),
            "not read as section tails"
        );
        s.record_movement_decode(Err(MovementError::ErrorSentinel).as_ref());
        assert_eq!(s.movement_envelope_trailers, 5, "an Err carries no tally");
    }

    /// Section tails are summed from every `Ok` decode, soft errors or not, and
    /// never count as a movement error (which would keep the batch as a raw row).
    #[test]
    fn section_tails_are_summed_from_every_ok_decode_and_are_not_errors() {
        let mut s = ExportStats::default();
        s.record_movement_decode(with_tails(0, (1, 40), (0, 0)).as_ref());
        s.record_movement_decode(with_tails(2, (2, 7), (3, 90)).as_ref());
        assert_eq!(s.movement_sized_section_tails, 3);
        assert_eq!(s.movement_sized_section_tail_bits, 47);
        assert_eq!(s.movement_open_section_tails, 3);
        assert_eq!(s.movement_open_section_tail_bits, 90);
        assert_eq!(s.movement_rpc_errors, 2, "only the soft errors");
        s.record_movement_decode(Err(MovementError::ErrorSentinel).as_ref());
        assert_eq!(s.movement_sized_section_tails, 3, "an Err carries no tally");
    }

    #[test]
    fn a_clean_decode_records_nothing() {
        let mut s = ExportStats::default();
        s.record_movement_decode(ok(5, 1, 0).as_ref());
        assert_eq!(s.movement_rpc_errors, 0);
        assert!(s.movement_first_error.is_none());
    }

    #[test]
    fn soft_errors_are_counted_and_first_error_is_kept() {
        let mut s = ExportStats::default();
        s.record_movement_decode(ok(2, 5, 3).as_ref());
        assert_eq!(s.movement_rpc_errors, 3);
        // `error_count` counts decode problems per occurrence; the updates
        // after a failed stream are still decoded, so none were "skipped".
        assert_eq!(
            s.movement_first_error.as_deref(),
            Some("3 movement decode error(s) in one RPC batch")
        );
        // A later hard failure adds to the count but must not overwrite the
        // first error.
        let first = s.movement_first_error.clone();
        s.record_movement_decode(Err(MovementError::InvalidMagic(0x00)).as_ref());
        assert_eq!(s.movement_rpc_errors, 4);
        assert_eq!(s.movement_first_error, first);
    }

    #[test]
    fn a_hard_error_records_its_display() {
        let mut s = ExportStats::default();
        s.record_movement_decode(Err(MovementError::ErrorSentinel).as_ref());
        assert_eq!(s.movement_rpc_errors, 1);
        let msg = s.movement_first_error.expect("first error recorded");
        assert!(msg.contains("sentinel"), "got: {msg}");
    }
}

/// The record buffers a sink fills for one packet, lent to it so their capacity
/// survives the packet: one allocation for the whole run
/// (docs/PERFORMANCE_NOTES.md#recordbuffers-are-lent-not-owned).
/// [`ExportSink::new`] clears them, so a caller that never drains them -- the
/// validation oracle -- does not accumulate the whole replay.
#[derive(Debug, Default)]
pub struct RecordBuffers {
    pub fields: Vec<FieldRecord>,
    pub movement: Vec<MovementRecord>,
    pub actors: Vec<ActorRecord>,
    pub partials: Vec<PartialRecord>,
    pub checkpoint_blocks: Vec<CheckpointBlockRecord>,
}

/// The export sink: turns `vrf-net` events into records for the Parquet
/// writers. It borrows the `NetGuidCache` mutably because `vrf-net` calls
/// `GuidPathSink::register_path` mid-packet, for package-map export bunches
/// that declare GUID -> path mappings inline.
pub struct ExportSink<'a> {
    pub cache: &'a mut NetGuidCache,
    channel_state: &'a mut ChannelState,
    pub time_ms: u32,
    pub packet_id: u32,
    records: &'a mut RecordBuffers,
    pub stats: ExportStats,
    /// The checksum-gated structured-array routes this replay's branch admits.
    /// See [`measured_routes`]; empty until `enable_measured_array_routes`.
    measured_array_routes: MeasuredArrayRoutes,
    checkpoint_block_scope: Option<(CheckpointIdentity, u64, u32)>,

    // -- per-content-block context (set by on_content_block) ----------------
    current_channel: u32,
    current_actor_guid: u32,
    /// Subobject GUID of the block being walked; `None` for actor blocks.
    current_object_guid: Option<u32>,
    /// True only when the replay's direct, pre-remap object path is exactly the
    /// measured component that carries the chained fc=34 ClassNetCache stream.
    current_is_abilities_and_buffs: bool,
    /// Interned: a block's rows share one allocation of the path ([`intern`]).
    current_group_path: Arc<str>,
    /// The half-finished overlay key hash for [`current_group_path`](Self::current_group_path).
    /// Overlay probes run ~2M times per replay, each block's with one long group
    /// path and short field names, so only the name half is hashed per probe.
    /// A stale value turns hits into misses (`raw_bits` only), never a wrong
    /// type: the slot tag and full string equality still reject the key.
    current_group_hash: GroupHashState,
    current_group_resolution_source: &'static str,
    current_function_count_source: &'static str,
    current_resolution_memo_hit: bool,
}

impl<'a> ExportSink<'a> {
    /// Build a sink for one packet; clears `records` (see [`RecordBuffers`]).
    pub fn new(
        cache: &'a mut NetGuidCache,
        channel_state: &'a mut ChannelState,
        records: &'a mut RecordBuffers,
    ) -> Self {
        records.fields.clear();
        records.movement.clear();
        records.actors.clear();
        records.partials.clear();
        records.checkpoint_blocks.clear();
        let current_group_path = empty_group_path();
        let current_group_hash = group_hash_state(&current_group_path);
        Self {
            cache,
            channel_state,
            time_ms: 0,
            packet_id: 0,
            records,
            stats: ExportStats::default(),
            measured_array_routes: MeasuredArrayRoutes::NONE,
            checkpoint_block_scope: None,
            current_channel: 0,
            current_actor_guid: 0,
            current_object_guid: None,
            current_is_abilities_and_buffs: false,
            current_group_path,
            current_group_hash,
            current_group_resolution_source: "unset",
            current_function_count_source: "unset",
            current_resolution_memo_hit: false,
        }
    }

    /// Admit the structured-array routes measured for `branch` ([`measured_routes`]).
    pub(super) fn enable_measured_array_routes(&mut self, branch: &str) {
        self.measured_array_routes = MeasuredArrayRoutes::for_branch(branch);
    }

    /// Whether `route` may expand its parent in this replay.
    fn admits(&self, route: MeasuredArrayRoute) -> bool {
        self.measured_array_routes.admits(route)
    }

    #[cfg(feature = "export")]
    pub(super) fn enable_checkpoint_block_context(
        &mut self,
        checkpoint: CheckpointIdentity,
        field_row_offset: u64,
        block_index_offset: u32,
    ) {
        self.checkpoint_block_scope = Some((checkpoint, field_row_offset, block_index_offset));
    }

    /// Set `current_group_path` and refresh its cached hash together. Every
    /// assignment goes through here (the three are in [`paths`]: memo hit, fresh
    /// resolution, instance-name replacement), or
    /// [`current_group_hash`](Self::current_group_hash) goes stale.
    fn set_current_group_path(&mut self, path: Arc<str>) {
        self.current_group_hash = group_hash_state(&path);
        self.current_group_path = path;
    }

    /// Push one field row, stamped with the current block context. Every
    /// `FieldRecord` this crate produces is built and counted here, so no call
    /// site can get the six block-context columns or `fields_emitted` wrong.
    fn push_field(&mut self, row: FieldValues) {
        self.stats.fields_emitted += 1;
        self.records.fields.push(FieldRecord {
            time_ms: self.time_ms,
            packet_id: self.packet_id,
            channel_index: self.current_channel,
            actor_net_guid: self.current_actor_guid,
            object_net_guid: self.current_object_guid,
            // A refcount bump, not a copy: this is the 1.25-million-row column
            // the interning exists for.
            group_path: Arc::clone(&self.current_group_path),
            handle: row.handle,
            field_name: row.field_name,
            compatible_checksum: row.compatible_checksum,
            bit_count: row.bit_count,
            raw_bits: row.raw_bits,
            value_i64: row.value_i64,
            value_f64: row.value_f64,
            value_bool: row.value_bool,
            value_str: row.value_str,
        });
        if self.checkpoint_block_scope.is_some() {
            if let Some(block) = self.records.checkpoint_blocks.last_mut() {
                block.field_row_count += 1;
            }
        }
    }
}

/// A field row minus its block context; see [`ExportSink::push_field`].
#[derive(Debug, Default)]
struct FieldValues {
    handle: u32,
    field_name: Option<Arc<str>>,
    /// The replay's declared checksum for this handle; `None` (the default) for
    /// rows addressed inside a payload, such as array leaves and struct blobs.
    compatible_checksum: Option<u32>,
    bit_count: u32,
    raw_bits: Option<SmallVec<[u8; 16]>>,
    value_i64: Option<i64>,
    value_f64: Option<f64>,
    value_bool: Option<bool>,
    value_str: Option<String>,
}

/// The group path a sink starts with: clones of one process-wide empty `Arc`,
/// not the 530,401 fresh allocations one per sink would cost over a replay.
/// Observable only if a field arrived before its content block, which the
/// framer never does.
fn empty_group_path() -> Arc<str> {
    use std::sync::OnceLock;
    static EMPTY: OnceLock<Arc<str>> = OnceLock::new();
    Arc::clone(EMPTY.get_or_init(|| Arc::from("")))
}

impl GuidPathSink for ExportSink<'_> {
    /// Record a GUID -> path mapping the wire declared inline.
    ///
    /// A write that would change nothing is skipped, saving the `to_string`
    /// allocation. The memo does not rely on the skip: `set_net_guid_path` makes
    /// the same comparison (a zero outer is `None` in both) and moves
    /// `NetGuidCache::guid_generation`, [`BlockPathMemo`]'s stamp for the GUID
    /// maps, only on a real change, so nothing is bumped here. The outer is
    /// compared too: a repeat with the same path and an invalid outer *removes*
    /// the outer, and skipping it would keep a stale one in resolved group paths
    /// and in `net_guids.parquet`'s `outer_net_guid` column.
    fn register_path(&mut self, guid: u32, path: &str, outer_guid: NetworkGuid) {
        let outer = if outer_guid.0 != 0 {
            Some(vrf_schema::NetworkGuid(outer_guid.0))
        } else {
            None
        };
        if self.cache.get_path_by_guid(guid) == Some(path)
            && self.cache.get_outer_guid(guid) == outer
        {
            return;
        }
        self.cache.set_net_guid_path(guid, path.to_string(), outer);
    }

    fn path_for_guid(&self, guid: u32) -> Option<&str> {
        self.cache.get_path_by_guid(guid)
    }
}

/// Builders shared by the sink's test modules.
#[cfg(test)]
mod test_fixtures {
    use vrf_net::pipeline::ActorChannelState;
    use vrf_net::types::NetworkGuid;

    /// Append `value` as Unreal's `IntPacked`, LSB-first.
    pub(super) fn packed(bits: &mut Vec<bool>, mut value: u32) {
        loop {
            let byte = ((value & 127) << 1) | u32::from(value > 127);
            bits.extend((0..8).map(|bit| byte & (1 << bit) != 0));
            value >>= 7;
            if value == 0 {
                break;
            }
        }
    }

    /// Pack an LSB-first bit list into bytes.
    pub(super) fn bytes(bits: &[bool]) -> Vec<u8> {
        let mut raw = vec![0; bits.len().div_ceil(8)];
        for (index, bit) in bits.iter().enumerate() {
            raw[index / 8] |= u8::from(*bit) << (index % 8);
        }
        raw
    }

    /// Unpack bytes into an LSB-first bit list.
    pub(super) fn bits_from_bytes(raw: &[u8]) -> Vec<bool> {
        raw.iter()
            .flat_map(|byte| (0..8).map(move |bit| byte & (1 << bit) != 0))
            .collect()
    }

    /// An `ActorChannelState` for one channel open.
    pub(super) fn channel_open(
        channel_index: u32,
        actor: u32,
        archetype: u32,
    ) -> ActorChannelState {
        ActorChannelState {
            channel_index,
            is_open: true,
            is_dormant: false,
            actor_net_guid: NetworkGuid(actor),
            archetype_net_guid: NetworkGuid(archetype),
            level_guid: NetworkGuid(0),
            spawn_location: None,
            spawn_rotation: None,
            spawn_scale: None,
            spawn_velocity: None,
            open_packet_id: 0,
        }
    }
}
