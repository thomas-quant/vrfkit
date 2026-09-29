//! The one place a packet sink's counters are accumulated.
//!
//! `ExportSink` is rebuilt for every packet -- ~530,000 times on the reference
//! replay -- so a counter the caller does not read reads as a permanent zero:
//! `cnc_rpcs_emitted`, the only signal the `AbilitiesAndBuffsComponent`
//! brute-force produced RPC structure, once reached no summary, and the
//! checkpoint pass once dropped [`ArrayDecodeStats::errors`], `truncated_rpcs`
//! and the movement errors, so an overrun checkpoint array lost its children
//! with no failure recorded. Both passes and `diag` share
//! [`SinkTotals::absorb`], which is why it lives here, not in the
//! `export`-gated driver: `diag` is built without the feature.
//!
//! `absorb` destructures `ExportStats` with no `..` and sums the overlay and
//! array counters through `OverlayStats::merge_counts_from` and
//! `ArrayDecodeStats::merge_from`, written the same way, so a counter added to
//! any of the three does not compile until it is summed here (bound but not
//! summed, it is an unused variable). A counter summed into the wrong field is
//! caught by `absorb_sums_every_counter_into_its_own_field`. Not covered: the
//! printers -- the summary, the manifest's `quality` block and the `diag` JSON
//! name their lines by hand; only the `diag` list is checked, in `diagnose.rs`.

use vrf_decode::{ArrayDecodeStats, OverlayErrorReport, OverlayStats};

use super::ExportStats;

/// Everything a packet's sink counted, summed across packets.
#[derive(Debug, Default)]
pub(crate) struct SinkTotals {
    /// Every row `push_field` wrote: the `fields.parquet` (or
    /// `checkpoint_fields.parquet`) row count, not NetStats' `fields`.
    pub fields_emitted: u64,
    /// The sink's own count of four events vrf-net counts beside the same
    /// callbacks (RPCs, actor opens and closes, content blocks live and
    /// deleted), so each pair must be equal: a difference is a missing or extra
    /// `+= 1` in the sink, never framing. Published as the manifest's
    /// `sink_rpcs_emitted`, `sink_actor_opens`, `sink_actor_closes` and
    /// `sink_content_blocks`; `tools/verify_build_corpus.py` fails a replay
    /// whose value differs from the `net` block's.
    pub rpcs_emitted: u64,
    pub actor_opens: u64,
    pub actor_closes: u64,
    pub content_blocks: u64,
    pub overlay: OverlayStats,
    pub effect_blobs_decoded: u64,
    pub struct_blobs_decoded: u64,
    pub struct_blobs_failed: u64,
    /// The first failure, never overwritten: it names the build change.
    pub struct_blob_first_error: Option<String>,
    pub multi_contents_items_emitted: u64,
    pub movement_rpc_errors: u64,
    pub movement_first_error: Option<String>,
    /// See `ExportStats::movement_sized_section_tails` and its neighbours.
    pub movement_sized_section_tails: u64,
    pub movement_sized_section_tail_bits: u64,
    pub movement_open_section_tails: u64,
    pub movement_open_section_tail_bits: u64,
    /// See `ExportStats::movement_envelope_trailers`.
    pub movement_envelope_trailers: u64,
    pub movement_envelope_trailer_bits: u64,
    pub array: ArrayDecodeStats,
    pub tracked_rewards_opaque_empty_variants: u64,
    /// See `ExportStats::active_blinds_empty_trailers`.
    pub active_blinds_empty_trailers: u64,
    pub array_leaf_decode_errors: u64,
    pub targeting_world_locations_decoded: u64,
    pub truncated_rpcs: u64,
    pub rpc_suffix_bits_dropped: u64,
    pub cnc_rpcs_emitted: u64,
    pub cnc_bruteforce_payloads_attempted: u64,
    pub cnc_bruteforce_payloads_unwalked: u64,
    /// Post-RepLayout ClassNetCache tails decoded as verified RPC structure.
    pub rep_layout_cnc_tails_decoded: u64,
    /// Post-RepLayout tails retained as whole raw payloads.
    pub rep_layout_cnc_tails_preserved: u64,
}

impl SinkTotals {
    /// Fold one packet's counters in. Both passes merge into the *same*
    /// `error_report`, the only place a checkpoint-only decode error is ever
    /// seen. `stats` is `&mut` so the two first-error strings move, not clone.
    pub(crate) fn absorb(
        &mut self,
        stats: &mut ExportStats,
        error_report: &mut OverlayErrorReport,
    ) {
        // No `..`: see the module doc. A field added to `ExportStats` stops
        // this compiling until it is bound here and summed below.
        let ExportStats {
            fields_emitted,
            rpcs_emitted,
            actor_opens,
            actor_closes,
            content_blocks,
            overlay,
            array,
            tracked_rewards_opaque_empty_variants,
            active_blinds_empty_trailers,
            effect_blobs_decoded,
            struct_blobs_decoded,
            multi_contents_items_emitted,
            cnc_rpcs_emitted,
            cnc_bruteforce_payloads_attempted,
            cnc_bruteforce_payloads_unwalked,
            rep_layout_cnc_tails_decoded,
            rep_layout_cnc_tails_preserved,
            struct_blobs_failed,
            struct_blob_first_error,
            movement_rpc_errors,
            movement_first_error,
            movement_sized_section_tails,
            movement_sized_section_tail_bits,
            movement_open_section_tails,
            movement_open_section_tail_bits,
            movement_envelope_trailers,
            movement_envelope_trailer_bits,
            truncated_rpcs,
            rpc_suffix_bits_dropped,
            array_leaf_decode_errors,
            targeting_world_locations_decoded,
        } = stats;
        self.fields_emitted += *fields_emitted;
        self.rpcs_emitted += *rpcs_emitted;
        self.actor_opens += *actor_opens;
        self.actor_closes += *actor_closes;
        self.content_blocks += *content_blocks;
        self.overlay.merge_counts_from(overlay);
        self.effect_blobs_decoded += *effect_blobs_decoded;
        self.struct_blobs_decoded += *struct_blobs_decoded;
        self.struct_blobs_failed += *struct_blobs_failed;
        if self.struct_blob_first_error.is_none() {
            self.struct_blob_first_error = struct_blob_first_error.take();
        }
        self.multi_contents_items_emitted += *multi_contents_items_emitted;
        self.movement_rpc_errors += *movement_rpc_errors;
        if self.movement_first_error.is_none() {
            self.movement_first_error = movement_first_error.take();
        }
        self.movement_sized_section_tails += *movement_sized_section_tails;
        self.movement_sized_section_tail_bits += *movement_sized_section_tail_bits;
        self.movement_open_section_tails += *movement_open_section_tails;
        self.movement_open_section_tail_bits += *movement_open_section_tail_bits;
        self.movement_envelope_trailers += *movement_envelope_trailers;
        self.movement_envelope_trailer_bits += *movement_envelope_trailer_bits;
        self.array.merge_from(array);
        self.tracked_rewards_opaque_empty_variants += *tracked_rewards_opaque_empty_variants;
        self.active_blinds_empty_trailers += *active_blinds_empty_trailers;
        self.array_leaf_decode_errors += *array_leaf_decode_errors;
        self.targeting_world_locations_decoded += *targeting_world_locations_decoded;
        self.truncated_rpcs += *truncated_rpcs;
        self.rpc_suffix_bits_dropped += *rpc_suffix_bits_dropped;
        self.cnc_rpcs_emitted += *cnc_rpcs_emitted;
        self.cnc_bruteforce_payloads_attempted += *cnc_bruteforce_payloads_attempted;
        self.cnc_bruteforce_payloads_unwalked += *cnc_bruteforce_payloads_unwalked;
        self.rep_layout_cnc_tails_decoded += *rep_layout_cnc_tails_decoded;
        self.rep_layout_cnc_tails_preserved += *rep_layout_cnc_tails_preserved;
        error_report.merge_from(&overlay.error_report);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vrf_decode::{DecodeErrorKind, FieldType, OverlayErrorReport};

    /// Every counter lands in its own `SinkTotals` field, summed across packets,
    /// which the destructures cannot see (a sized/open movement-tail swap once
    /// compiled and passed every test). Each counter gets a distinct value from
    /// `next()`, starting at 2, through no-`..` literals and destructures over
    /// three packets, so one assigned, doubled, bumped per packet or summed into
    /// another field misses its total. Beside `absorb`, so core-only builds run it.
    #[test]
    fn absorb_sums_every_counter_into_its_own_field() {
        let last = std::cell::Cell::new(0u64);
        let next = || {
            last.set(last.get() + 1);
            last.get() + 1
        };
        let mut packet_report = OverlayErrorReport::default();
        packet_report.record(
            "/Group",
            "Field",
            FieldType::Int32,
            32,
            DecodeErrorKind::Eof,
        );
        let sent = ExportStats {
            fields_emitted: next(),
            rpcs_emitted: next(),
            actor_opens: next(),
            actor_closes: next(),
            content_blocks: next(),
            overlay: OverlayStats {
                decoded_ok: next(),
                decoded_err: next(),
                raw_or_skip: next(),
                not_in_table: next(),
                no_field_name: next(),
                handle_conflicts_refused: next(),
                error_report: packet_report,
            },
            array: ArrayDecodeStats {
                elements_decoded: next(),
                fields_emitted: next(),
                truncations: next(),
                errors: next(),
                unconsumed_nested_bits: next(),
                unconsumed_root_bits: next(),
                implicit_terminations: next(),
            },
            tracked_rewards_opaque_empty_variants: next(),
            active_blinds_empty_trailers: next(),
            effect_blobs_decoded: next(),
            struct_blobs_decoded: next(),
            multi_contents_items_emitted: next(),
            cnc_rpcs_emitted: next(),
            cnc_bruteforce_payloads_attempted: next(),
            cnc_bruteforce_payloads_unwalked: next(),
            rep_layout_cnc_tails_decoded: next(),
            rep_layout_cnc_tails_preserved: next(),
            struct_blobs_failed: next(),
            struct_blob_first_error: Some("first blob".to_owned()),
            movement_rpc_errors: next(),
            movement_first_error: Some("first movement".to_owned()),
            movement_sized_section_tails: next(),
            movement_sized_section_tail_bits: next(),
            movement_open_section_tails: next(),
            movement_open_section_tail_bits: next(),
            movement_envelope_trailers: next(),
            movement_envelope_trailer_bits: next(),
            truncated_rpcs: next(),
            rpc_suffix_bits_dropped: next(),
            array_leaf_decode_errors: next(),
            targeting_world_locations_decoded: next(),
        };

        let mut totals = SinkTotals::default();
        let mut report = OverlayErrorReport::default();
        totals.absorb(&mut sent.clone(), &mut report);
        // A later packet's failures must not displace the first, which is the
        // one that names the build change.
        let mut later = sent.clone();
        later.struct_blob_first_error = Some("later blob".to_owned());
        later.movement_first_error = Some("later movement".to_owned());
        totals.absorb(&mut later.clone(), &mut report);
        totals.absorb(&mut later, &mut report);

        let SinkTotals {
            fields_emitted,
            rpcs_emitted,
            actor_opens,
            actor_closes,
            content_blocks,
            overlay:
                OverlayStats {
                    decoded_ok,
                    decoded_err,
                    raw_or_skip,
                    not_in_table,
                    no_field_name,
                    handle_conflicts_refused,
                    // Not a counter: each packet's report is folded into the
                    // shared one checked below.
                    error_report: _,
                },
            effect_blobs_decoded,
            struct_blobs_decoded,
            struct_blobs_failed,
            struct_blob_first_error,
            multi_contents_items_emitted,
            movement_rpc_errors,
            movement_first_error,
            movement_sized_section_tails,
            movement_sized_section_tail_bits,
            movement_open_section_tails,
            movement_open_section_tail_bits,
            movement_envelope_trailers,
            movement_envelope_trailer_bits,
            array:
                ArrayDecodeStats {
                    elements_decoded,
                    fields_emitted: array_fields_emitted,
                    truncations,
                    errors,
                    unconsumed_nested_bits,
                    unconsumed_root_bits,
                    implicit_terminations,
                },
            tracked_rewards_opaque_empty_variants,
            active_blinds_empty_trailers,
            array_leaf_decode_errors,
            targeting_world_locations_decoded,
            truncated_rpcs,
            rpc_suffix_bits_dropped,
            cnc_rpcs_emitted,
            cnc_bruteforce_payloads_attempted,
            cnc_bruteforce_payloads_unwalked,
            rep_layout_cnc_tails_decoded,
            rep_layout_cnc_tails_preserved,
        } = totals;
        let (overlay, array) = (&sent.overlay, &sent.array);
        // (name, total, per-packet value) for every counter.
        macro_rules! landed {
            ($($total:ident = $sent:expr),+ $(,)?) => {
                [$((stringify!($sent), $total, $sent)),+]
            };
        }
        let landed = landed![
            fields_emitted = sent.fields_emitted,
            rpcs_emitted = sent.rpcs_emitted,
            actor_opens = sent.actor_opens,
            actor_closes = sent.actor_closes,
            content_blocks = sent.content_blocks,
            decoded_ok = overlay.decoded_ok,
            decoded_err = overlay.decoded_err,
            raw_or_skip = overlay.raw_or_skip,
            not_in_table = overlay.not_in_table,
            no_field_name = overlay.no_field_name,
            handle_conflicts_refused = overlay.handle_conflicts_refused,
            effect_blobs_decoded = sent.effect_blobs_decoded,
            struct_blobs_decoded = sent.struct_blobs_decoded,
            struct_blobs_failed = sent.struct_blobs_failed,
            multi_contents_items_emitted = sent.multi_contents_items_emitted,
            movement_rpc_errors = sent.movement_rpc_errors,
            movement_sized_section_tails = sent.movement_sized_section_tails,
            movement_sized_section_tail_bits = sent.movement_sized_section_tail_bits,
            movement_open_section_tails = sent.movement_open_section_tails,
            movement_open_section_tail_bits = sent.movement_open_section_tail_bits,
            movement_envelope_trailers = sent.movement_envelope_trailers,
            movement_envelope_trailer_bits = sent.movement_envelope_trailer_bits,
            elements_decoded = array.elements_decoded,
            array_fields_emitted = array.fields_emitted,
            truncations = array.truncations,
            errors = array.errors,
            unconsumed_nested_bits = array.unconsumed_nested_bits,
            unconsumed_root_bits = array.unconsumed_root_bits,
            implicit_terminations = array.implicit_terminations,
            tracked_rewards_opaque_empty_variants = sent.tracked_rewards_opaque_empty_variants,
            active_blinds_empty_trailers = sent.active_blinds_empty_trailers,
            array_leaf_decode_errors = sent.array_leaf_decode_errors,
            targeting_world_locations_decoded = sent.targeting_world_locations_decoded,
            truncated_rpcs = sent.truncated_rpcs,
            rpc_suffix_bits_dropped = sent.rpc_suffix_bits_dropped,
            cnc_rpcs_emitted = sent.cnc_rpcs_emitted,
            cnc_bruteforce_payloads_attempted = sent.cnc_bruteforce_payloads_attempted,
            cnc_bruteforce_payloads_unwalked = sent.cnc_bruteforce_payloads_unwalked,
            rep_layout_cnc_tails_decoded = sent.rep_layout_cnc_tails_decoded,
            rep_layout_cnc_tails_preserved = sent.rep_layout_cnc_tails_preserved,
        ];
        // Every counter was given a different value, so a counter summed into
        // another's field shows up here as two wrong sums.
        for (name, total, per_packet) in landed {
            assert_eq!(total, 3 * per_packet, "{name}");
        }
        assert_eq!(landed.len() as u64, last.get(), "one check per counter");
        assert_eq!(struct_blob_first_error.as_deref(), Some("first blob"));
        assert_eq!(movement_first_error.as_deref(), Some("first movement"));
        // One recorded failure per packet, folded into the shared report.
        assert_eq!(report.total_errors(), 3);
    }
}
