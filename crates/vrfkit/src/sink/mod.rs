//! Sink implementation connecting `vrf-net` events to `vrf-export` records.
//!
//! # Design: no skipping
//!
//! Every field/RPC is emitted, even if we cannot resolve the group path or field
//! name. In that case we emit `group_path = "<unknown:{guid}>"` and
//! `field_name = None`. When an unresolved ClassNetCache function table makes
//! the block unsplittable, its whole payload is emitted as one explicitly
//! marked preservation row instead of fabricated fields. This keeps the
//! Parquet output a **lossless** representation of the stream.
//!
//! # Layout
//!
//! - [`intern`] -- the `Arc<str>` pool behind the two name columns.
//! - [`paths`] -- content-block group-path resolution and its memo.
//! - [`rpc`] -- the ClassNetCache RPC parameter walker.
//! - [`blobs`] -- the struct-blob and flattened-array decoders.
//! - [`measured_routes`] -- which structured-array routes each build admits.
//! - [`stream`] -- the `vrf-net` trait impls that drive all of the above.
//! - [`totals`] -- the per-run sum of every packet sink's counters, shared by
//!   `export` (both passes) and `diag`.
//!
//! This module holds what all of them but [`totals`] share: the sink, the
//! per-packet record buffers, and the state that must outlive a packet.
//!
//! # What the sink costs
//!
//! `vrfkit validate` runs this whole path and writes no file, so it measures
//! the sink alone.
//!
//! Sink cost before/after the memo+pool change, reference replay:
//! docs/PERFORMANCE_NOTES.md#what-the-whole-sink-costs.

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

/// Static overlay table built from C# descriptors.
static TABLE: OverlayTable = OverlayTable::with_handles(&OVERLAY_TABLE, &OVERLAY_HANDLE_TABLE);

/// How many stream-failure lines to retain. See [`ChannelState::stream_failures`].
const MAX_STREAM_FAILURE_RECORDS: usize = 32;

/// One BombPlayerState actor's identity, accumulated from its `Subject` and
/// `SpawnedCharacter` fields for the manifest `players` array.
///
/// `Subject` is the account UUID (a `String`); `SpawnedCharacter` is the
/// character actor NetGUID, which equals `movement.character_net_guid`. Together
/// they let every actor-keyed table join to a stable account identity -- the
/// one piece `playerLoadouts`' `characterId` cannot give when two players pick
/// the same agent.
#[derive(Debug, Clone, Default)]
pub struct PlayerIdentity {
    pub subject: Option<String>,
    pub character_net_guid: Option<u32>,
}

/// Persistent per-channel state that must survive across packets and chunks.
///
/// The replay pipeline creates a fresh `ExportSink` for every packet (to
/// satisfy borrow-checker constraints around `NetGuidCache` mutability). This
/// struct holds the state that *must* persist across those boundaries -- the
/// archetype GUID assigned when a channel is opened, which is needed later to
/// resolve ClassNetCache export groups when content blocks arrive, plus the two
/// memos and the name pool, none of which would ever warm up if they were
/// rebuilt half a million times.
#[derive(Debug, Clone, Default)]
pub struct ChannelState {
    /// channel_index -> the archetype and the actor it was read for. The actor
    /// half is load-bearing; see [`ChannelArchetype`].
    archetypes: FxHashMap<u32, ChannelArchetype>,
    /// See [`RpcParamGroupMemo`].
    rpc_param_groups: RpcParamGroupMemo,
    /// See [`BlockPathMemo`].
    block_paths: BlockPathMemo,
    /// See [`NameInterner`].
    names: NameInterner,
    /// Bumped whenever this struct's archetype map changes: the one input to
    /// group-path resolution that lives here rather than in the cache.
    /// [`BlockPathMemo`] stamps itself with this, the cache's
    /// `schema_generation` (declared group paths) and its `guid_generation`
    /// (GUID -> path and GUID -> outer maps); only the three together cover
    /// every input the resolution reads.
    resolution_generation: u64,
    /// One line per content block that framed and decoded but whose inner stream
    /// could not be walked.
    ///
    /// Lives here rather than on the sink because the sink is rebuilt for every
    /// packet, so anything recorded on it is lost immediately. Capped: a build
    /// whose transform is wrong would fail on essentially every block, and the
    /// first few dozen say everything the later million would.
    stream_failures: Vec<String>,
    /// Failure aggregation is opt-in for `diag`; ordinary decode and export
    /// paths keep this as `None` and pay no map or payload-sampling cost.
    failures: Option<FailureAggregate>,
    /// BombPlayerState identity capture for the manifest `players` array. Keyed
    /// by the PlayerState actor's NetGUID; filled in `on_field` as `Subject`
    /// and `SpawnedCharacter` arrive, drained once at the end of the replay.
    players: FxHashMap<u32, PlayerIdentity>,
}

impl ChannelState {
    /// Create an empty channel state.
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

    /// Retained stream-failure lines.
    #[must_use]
    pub fn stream_failures(&self) -> &[String] {
        &self.stream_failures
    }

    /// Enable bounded failure aggregation for a diagnostic pass.
    pub fn enable_failure_aggregate(&mut self, retain_payloads: bool) {
        self.failures = Some(FailureAggregate::new(retain_payloads));
    }

    /// Whether the current pass requested detailed failure positions and
    /// optional payload callbacks from the replication layer.
    pub fn failure_aggregate_enabled(&self) -> bool {
        self.failures.is_some()
    }

    /// Take the failure aggregate out, leaving it empty. The checkpoint pass
    /// builds one channel state per chunk, so its totals are gathered by
    /// draining each chunk's aggregate into the caller's.
    #[must_use]
    pub fn take_failure_aggregate(&mut self) -> FailureAggregate {
        self.failures.take().unwrap_or_default()
    }

    /// Captured player identities (PlayerState actor NetGUID -> identity), for
    /// the manifest `players` array.
    ///
    /// Gated with its only caller: the manifest is written by `driver`, which
    /// is itself `export`-only, so without the feature this is dead code and
    /// the build says so.
    #[cfg(feature = "export")]
    #[must_use]
    pub fn players(&self) -> &FxHashMap<u32, PlayerIdentity> {
        &self.players
    }

    /// Declare that something group-path resolution reads has changed.
    ///
    /// Call sites are deliberately few -- the archetype assignment and
    /// retirement in `paths` (`set_channel_archetype` from `on_actor_open`,
    /// `retire_channel_archetype` from `on_actor_close`) -- because every one
    /// of them is a place the memo could go stale. The cache's GUID maps are
    /// not among them: `NetGuidCache::guid_generation` stamps those. Adding a
    /// resolution input that no stamp covers is silent byte movement, not a
    /// test failure.
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
    /// EffectContainer blobs turned into a `value_str` JSON array.
    ///
    /// Counted because nothing else moves when this decoder works. The overlay
    /// buckets are filled before the additive pass runs, so a successful effect
    /// decode leaves `decoded_ok`, `not_in_table` and the rest exactly where
    /// they were, and the only trace is a larger `fields.parquet`. A silent
    /// improvement is the same failure as a silent loss: the next session
    /// diffs two summaries, sees every counter identical, and concludes
    /// nothing changed. Failures already land in `overlay.decoded_err`.
    pub effect_blobs_decoded: u64,

    /// Struct-blob (`RoundResults`, `TeamEconomy`, `RoundInfos`) parent rows
    /// whose dedicated decoder produced elements.
    pub struct_blobs_decoded: u64,

    /// Item NetGUID rows emitted by the `MultiItemSlot.MultiContents` decoder.
    /// One per item actor reference in a multi-item slot. Surfaced separately
    /// for the same reason [`Self::effect_blobs_decoded`] is: these rows are
    /// already counted under `not_in_table` (the parent stays `Raw`) and under
    /// `fields_emitted`, so this counter is the only signal that the additive
    /// decoder produced typed leaves rather than silently no-op-ing.
    pub multi_contents_items_emitted: u64,

    /// RPC rows emitted by the ClassNetCache brute-force decoder for
    /// unresolved groups (currently `AbilitiesAndBuffsComponent`). The
    /// preservation row stays, and each decoded RPC is an extra row; this
    /// counter is the only signal that the additive decoder produced RPC
    /// structure rather than silently leaving the opaque blob.
    pub cnc_rpcs_emitted: u64,

    /// Unresolved `AbilitiesAndBuffsComponent` payloads offered to that
    /// brute-force walk: the denominator [`Self::cnc_rpcs_emitted`] does not
    /// have, since that counter also counts RepLayout-tail decodes.
    pub cnc_bruteforce_payloads_attempted: u64,

    /// Of those, payloads the fc=34 walk did not fit, so no RPC row was
    /// emitted and only the whole-payload preservation row remains.
    ///
    /// This exit used to be `let Some(..) else { return; }` with no counter,
    /// while only successes were counted -- the shape `struct_blobs_failed`
    /// was added to remove. The constant 34 is empirical and its doc says an
    /// update can fail the walk; if one does, `CNC RPC rows` shrinks and this
    /// is the line that says why. No bits are lost either way: the
    /// preservation row carries the payload whole. Zero on the replays
    /// measured when it was added.
    pub cnc_bruteforce_payloads_unwalked: u64,

    /// Post-RepLayout ClassNetCache tails decoded under verified component
    /// provenance and strict one-RPC framing.
    pub rep_layout_cnc_tails_decoded: u64,

    /// Post-RepLayout tails retained whole because their provenance or framing
    /// was not sufficient for the verified decoder.
    pub rep_layout_cnc_tails_preserved: u64,

    /// Struct-blob decodes that returned an error.
    ///
    /// These used to be `let Ok(..) else { return false }` -- discarded with no
    /// counter and no line. That is how build 13.02 moving `RoundResults` from
    /// handle 93 to 81 read as a completely clean export: every counter on the
    /// summary was identical to a good run and the match score simply was not
    /// in the Parquet. The decoders are additive, so a failure still costs no
    /// rows and no bits; it must not also cost the operator the knowledge that
    /// it happened.
    pub struct_blobs_failed: u64,

    /// The first failure verbatim, so the summary can name the member and the
    /// handle instead of only admitting that something went wrong.
    pub struct_blob_first_error: Option<String>,

    /// Movement-decode problems: per-update soft errors
    /// (`RpcDecodeResult.error_count`) plus hard `Err` failures, summed.
    /// `decode_movement_rpc` used to drop its `Result` wholesale, so a build
    /// that changed the movement section format would silently shorten
    /// `movement.parquet` with every other counter reading clean.
    pub movement_rpc_errors: u64,

    /// The first movement-decode problem verbatim, for the summary to name.
    pub movement_first_error: Option<String>,

    /// Movement sections, in a window sized by `movementBitCount`, that
    /// stopped with bits of it unread -- see
    /// `vrf_movement::RpcDecodeResult::sized_section_tails`. A soft tally:
    /// it does not make a batch count as failed, so it moves no Parquet row.
    pub movement_sized_section_tails: u64,
    /// Bits those sections left unread.
    pub movement_sized_section_tail_bits: u64,
    /// The same for sections whose window ran to the end of the component
    /// stream, where what follows may be other component data; kept apart so
    /// the two readings are never summed into one number.
    pub movement_open_section_tails: u64,
    /// Bits those sections left unread.
    pub movement_open_section_tail_bits: u64,

    /// RPC payloads whose RepLayout parameter loop broke on a malformed read
    /// before the terminating zero handle.
    ///
    /// `try_parse_rpc_params` keeps whatever rows it already parsed and returns
    /// `true`, so a truncated RPC reads as success: fewer parameter rows than
    /// declared, no other counter moves, and `rpcs_emitted` ticks up exactly as
    /// it does for a clean parse. This is the one signal that distinguishes
    /// "completed" from "abandoned mid-stream". Zero on valid replays; a non-zero
    /// value means the wire declared more parameters than the bits could carry.
    pub truncated_rpcs: u64,

    /// Bits left in an RPC payload after its zero-handle terminator, beyond the
    /// one trailing alignment bit the `FunctionParameters` grammar permits.
    ///
    /// The terminator used to end the walk without asking what remained. Any
    /// parameter already emitted set `emitted_any`, which suppressed the
    /// caller's whole-payload fallback row, so the tail reached no row, no
    /// [`Self::truncated_rpcs`] and not even `skipped_bits`. Every *leaf*
    /// payload in this crate is checked for full consumption
    /// (`decode_field` returns `NotFullyConsumed`); the *container's* was not,
    /// which is the same omission one level up.
    ///
    /// Counted rather than rejected, and not lost. The parameters that parsed
    /// keep their rows, and a payload with a suffix also gets a whole-payload
    /// row under the function's name (`try_parse_rpc_params`, or the caller's
    /// raw row when no parameter parsed), so every counted bit is still in
    /// `raw_bits`.
    ///
    /// It is not zero on real replays. In the 259ed10 corpus audit (1,018
    /// unique replays, exported with `--checkpoints`) the main-pass total is
    /// nonzero in 21 of 24 builds -- from 26,766 bits (12.07, 3 replays) to
    /// 9,329,665 (13.05, 401 replays) -- and zero only in the three builds
    /// with a single public fixture (12.10, 12.11, 13.00); some individual
    /// replays read zero too. The checkpoint pass is zero in every build.
    ///
    /// Every counted bit had one source: the handle named
    /// `ActiveGameplayEffects` under
    /// `/Script/ShooterGame.AresAbilitySystemComponent_ClassNetCache`, whose
    /// payload walks as parameters up to a zero handle and then continues.
    /// ClassNetCache framing carries custom-delta properties as well as RPCs
    /// (see `ActiveGameplayEffects` in docs/DATA.md), so this counts the part
    /// of that payload the RPC parameter grammar does not describe; what those
    /// bits encode is not established here. A suffix on any other handle would
    /// be new, and would mean the grammar no longer describes that payload.
    ///
    /// Method (2026-09-28): a separate Python re-walk of the grammar --
    /// checksum bit, IntPacked handle and length pairs to a zero handle, one
    /// trailing bit allowed -- over every bare-named ClassNetCache row with a
    /// payload in `fields.parquet` and `checkpoint_fields.parquet` reproduced
    /// this counter exactly in 90 of 90 (export, stream) pairs: two exports
    /// per build, one for each single-fixture build. In another 40 exports,
    /// 1,416 of 32,391 such rows carried a suffix, every one of them
    /// `ActiveGameplayEffects`, from 128 to 1,239 bits each.
    pub rpc_suffix_bits_dropped: u64,

    /// Flattened array leaves with a resolved type whose payload failed that
    /// decoder. Their raw leaf rows are still emitted; this counts the typed
    /// values that could not be recovered.
    pub array_leaf_decode_errors: u64,

    /// Typed world-location children emitted from the guarded map-click array.
    pub targeting_world_locations_decoded: u64,
}

impl ExportStats {
    /// Record a movement-RPC decode outcome so a silent failure cannot read
    /// as success. Soft per-update errors (caught and counted by the decoder
    /// in `RpcDecodeResult.error_count`) and hard `Err`s both land here; the
    /// first is kept verbatim for the summary.
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
                } = *r;
                self.movement_sized_section_tails += u64::from(sized_section_tails);
                self.movement_sized_section_tail_bits += sized_section_tail_bits;
                self.movement_open_section_tails += u64::from(open_section_tails);
                self.movement_open_section_tail_bits += open_section_tail_bits;
                if error_count > 0 {
                    self.movement_rpc_errors = self
                        .movement_rpc_errors
                        .saturating_add(u64::from(error_count));
                    self.movement_first_error.get_or_insert(format!(
                        "{error_count} movement update(s) skipped mid-decode"
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
            sized_section_tails: 0,
            sized_section_tail_bits: 0,
            open_section_tails: 0,
            open_section_tail_bits: 0,
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
        })
    }

    /// Section tails are a soft tally: they are summed from every decode that
    /// returned `Ok`, with or without soft errors, and they never count as a
    /// movement error -- a nonzero error count is what makes the sink keep a
    /// batch's whole payload as a raw row.
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
        assert!(s.movement_first_error.is_some());
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

/// The record buffers a sink fills for one packet.
///
/// These live outside the sink and are lent to it. The buffers are empty at
/// the end of every packet, so keeping their capacity across packets costs
/// one allocation for the entire run.
///
/// Construct-and-drop cost avoided by reuse, reference replay:
/// docs/PERFORMANCE_NOTES.md#recordbuffers-are-lent-not-owned.
///
/// [`ExportSink::new`] clears them, so a sink always starts empty no matter what
/// the previous holder did. That is what stops a caller which never drains them
/// -- the validation oracle is one -- from accumulating every record in the
/// replay.
#[derive(Debug, Default)]
pub struct RecordBuffers {
    /// Field records to be drained by the driver.
    pub fields: Vec<FieldRecord>,
    /// Movement records to be drained by the driver.
    pub movement: Vec<MovementRecord>,
    /// Actor lifecycle records to be drained by the driver.
    pub actors: Vec<ActorRecord>,
    pub partials: Vec<PartialRecord>,
    pub checkpoint_blocks: Vec<CheckpointBlockRecord>,
}

/// The export sink. Receives decoded events from `vrf-net` and produces records
/// for the Parquet writers.
///
/// The sink borrows the `NetGuidCache` mutably because `vrf-net` calls
/// `GuidPathSink::register_path` during packet processing (for package-map
/// export bunches that declare new GUID->path mappings inline).
pub struct ExportSink<'a> {
    /// Schema cache -- mutable because in-packet path registrations need it.
    pub cache: &'a mut NetGuidCache,
    /// Persistent per-channel state (archetype mappings survive across packets).
    channel_state: &'a mut ChannelState,
    /// Current frame time in milliseconds.
    pub time_ms: u32,
    /// Current packet index.
    pub packet_id: u32,
    /// Output buffers for this packet. See [`RecordBuffers`].
    records: &'a mut RecordBuffers,
    /// Stats.
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
    /// Interned, so a block's rows share one allocation instead of each
    /// carrying its own copy of the path. See [`intern`].
    current_group_path: Arc<str>,
    /// The half-finished overlay key hash for [`current_group_path`](Self::current_group_path).
    ///
    /// A content block probes the overlay ~2M times per replay with the same
    /// group path for every field in it, and the group path is long
    /// (`/Game/Characters/.../AggroBot_PC.AggroBot_PC_C`) while the field names
    /// are short. Caching the group-path fold and finishing only the field-name
    /// half per probe is the saving. Refreshed by `set_current_group_path`.
    ///
    /// A stale value is a performance and typing regression, not a wrong-value
    /// bug: the slot tag and the full string equality check still reject a
    /// mismatching key, so the field degrades to `raw_bits` instead of decoding
    /// to a wrong type.
    current_group_hash: GroupHashState,
    current_group_resolution_source: &'static str,
    current_function_count_source: &'static str,
    current_resolution_memo_hit: bool,
}

impl<'a> ExportSink<'a> {
    /// Build a sink for one packet over caller-owned record buffers.
    ///
    /// The buffers are cleared here rather than trusted to arrive empty: that is
    /// what makes it safe to lend the same buffers to every packet regardless of
    /// whether the caller drains them.
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

    /// Admit the structured-array routes measured for `branch`. The table,
    /// and why each build admits what it does, is in [`measured_routes`].
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

    /// Set `current_group_path` and refresh its cached overlay hash in one step.
    ///
    /// Every assignment to [`current_group_path`](Self::current_group_path) must
    /// go through here, or the cached [`current_group_hash`](Self::current_group_hash)
    /// goes stale. The three sites are all in [`paths`]: the memo-hit return, the
    /// fresh resolution, and the bare-instance-name ClassNetCache replacement.
    /// The hash is a common-subexpression optimisation: a stale value turns
    /// overlay hits into misses (fields degrade to `raw_bits`), never a wrong
    /// value -- the slot tag plus full string equality still guard every hit.
    fn set_current_group_path(&mut self, path: Arc<str>) {
        self.current_group_hash = group_hash_state(&path);
        self.current_group_path = path;
    }

    /// Push one field row, stamped with the current block context.
    ///
    /// Every `FieldRecord` this crate produces is built here. Nine of the
    /// fourteen columns are block context that no call site should be able to
    /// get wrong, and before this they were spelled out at seven of them.
    fn push_field(&mut self, row: FieldValues) {
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

/// The part of a field row that is not block context. See
/// [`ExportSink::push_field`].
#[derive(Debug, Default)]
struct FieldValues {
    handle: u32,
    field_name: Option<Arc<str>>,
    /// The replay's declared `compatible_checksum` for this handle, when there
    /// is one. Defaults to `None`, which is the honest answer for every path
    /// that addresses a value inside a payload rather than by a declared
    /// handle -- array leaves and struct blobs have no checksum to carry.
    compatible_checksum: Option<u32>,
    bit_count: u32,
    raw_bits: Option<SmallVec<[u8; 16]>>,
    value_i64: Option<i64>,
    value_f64: Option<f64>,
    value_bool: Option<bool>,
    value_str: Option<String>,
}

/// The group path a sink starts with, before any content block has been seen.
///
/// A fresh `Arc` per sink would be 530,401 allocations of an empty string over a
/// replay, so this hands out clones of one process-wide value. It is only ever
/// observable if a field arrives before its content block, which the framer
/// does not do.
fn empty_group_path() -> Arc<str> {
    use std::sync::OnceLock;
    static EMPTY: OnceLock<Arc<str>> = OnceLock::new();
    Arc::clone(EMPTY.get_or_init(|| Arc::from("")))
}

impl GuidPathSink for ExportSink<'_> {
    /// Record a GUID -> path mapping the wire declared inline.
    ///
    /// The write is skipped when it would change nothing, which saves the
    /// `path.to_string()` allocation. The memo does not rely on the skip:
    /// `set_net_guid_path` makes the same comparison (a zero outer is `None`
    /// in both) and moves `NetGuidCache::guid_generation`, the stamp
    /// [`BlockPathMemo`] reads for the cache's GUID -> path and GUID -> outer
    /// maps, only when something changed. So nothing needs bumping here: any
    /// call that gets past the check moves that stamp inside
    /// `set_net_guid_path`.
    ///
    /// Both halves of the state are compared, not just the path. A repeat call
    /// carrying the same path but an invalid outer *removes* the outer in
    /// `set_net_guid_path`; skipping that on a path match alone would preserve a
    /// stale outer, which changes resolved group paths and the `outer_net_guid`
    /// column of `net_guids.parquet`.
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
