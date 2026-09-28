//! The one place a packet sink's counters are accumulated.
//!
//! `ExportSink` is rebuilt for every packet -- ~530,000 times on the reference
//! replay -- so anything it counts and the caller does not read is discarded
//! that many times and reads as a permanent zero. That is not a hypothetical:
//! `cnc_rpcs_emitted` is the only signal that the `AbilitiesAndBuffsComponent`
//! brute-force produced RPC structure rather than leaving an opaque blob, and
//! it reached no summary at all. A build that stopped reaching that decoder
//! would have left "Decode errors: 0" and every other line of the export
//! summary exactly where a good run leaves them.
//!
//! The checkpoint pass had its own copy of the same loop and its own subset of
//! the same omission: [`ArrayDecodeStats::errors`], `truncated_rpcs` and the
//! movement-decode errors were dropped there, so a checkpoint array that
//! overran mid-element wrote its parent raw row, lost its flattened children,
//! and recorded no failure anywhere.
//!
//! Both passes now go through [`SinkTotals::absorb`], and so does `diag`,
//! which used to keep a line-for-line copy of it (`DiagSinkTotals`) and a
//! second list of the same counters. That is why this lives in `sink` and not
//! in the `export`-gated driver: `diag` is built without the feature.
//!
//! `absorb` opens with a destructure of `ExportStats` that has no `..`, and it
//! sums the overlay and array counters through `OverlayStats::merge_counts_from`
//! and `ArrayDecodeStats::merge_from`, which are written the same way. A
//! counter added to any of the three structs therefore does not compile until
//! it is summed here, and binding it without summing it is an unused-variable
//! warning. Before, a new field compiled cleanly and simply never arrived:
//! "one place to be wired in" was a convention with nothing enforcing it.
//!
//! What the destructure cannot see is a counter summed into the wrong field
//! while both bindings are still used. `absorb_sums_every_counter_into_its_own_field`
//! below gives every counter its own value and checks where each one lands.
//!
//! What this does NOT cover: the printers. The export summary, the manifest's
//! `quality` block and the `diag` JSON each still name their lines by hand, so
//! a counter can reach this struct and still be printed nowhere. The `diag`
//! list is checked against this struct by a test in `diagnose.rs`.

use vrf_decode::{ArrayDecodeStats, OverlayErrorReport, OverlayStats};

use super::ExportStats;

/// Everything a packet's sink counted, summed across packets.
#[derive(Debug, Default)]
pub(crate) struct SinkTotals {
    /// Rows pushed at the sites that count them: replicated properties, RPC
    /// parameters and the extra whole-payload row a partial parameter walk
    /// adds, life-change and path-point members, flattened array leaves,
    /// struct-blob members, `_cnc_h*` rows and RepLayout tail rows. Not every
    /// row: movement batch rows, zero-bit RPC markers, the raw row of an RPC
    /// whose parameters did not walk, unresolved-payload preservation rows
    /// and targeting world-location children are pushed without it. So it is
    /// comparable neither with NetStats' `fields` -- framed RepLayout
    /// properties -- nor with the `fields.parquet` row count. On 02d4d478 it
    /// reads 1,060,119 against `Fields: 429,648` and 1,296,660 table rows.
    pub fields_emitted: u64,
    /// The sink's own count of four events vrf-net counts too: RPC callbacks,
    /// actor opens, actor closes, and content blocks, live and deleted.
    /// vrf-net invokes each of these callbacks right beside its own increment,
    /// so each pair is one count taken twice and must be equal. A difference
    /// means the sink's bookkeeping -- a missing or extra `+= 1` -- is broken;
    /// it cannot detect framing going out of step, since both sides see the
    /// same callbacks. Published in the manifest as `sink_rpcs_emitted`,
    /// `sink_actor_opens`, `sink_actor_closes` and `sink_content_blocks`,
    /// where `tools/verify_build_corpus.py` fails a replay whose value differs
    /// from the `net` block's. Before these totals existed, `ExportStats`
    /// counted all five per packet and no caller read them.
    pub rpcs_emitted: u64,
    pub actor_opens: u64,
    pub actor_closes: u64,
    pub content_blocks: u64,
    pub overlay: OverlayStats,
    pub effect_blobs_decoded: u64,
    pub struct_blobs_decoded: u64,
    pub struct_blobs_failed: u64,
    /// First failure verbatim; a later packet must not overwrite the one that
    /// names the build change.
    pub struct_blob_first_error: Option<String>,
    pub multi_contents_items_emitted: u64,
    pub movement_rpc_errors: u64,
    pub movement_first_error: Option<String>,
    /// See `ExportStats::movement_sized_section_tails` and its three
    /// neighbours.
    pub movement_sized_section_tails: u64,
    pub movement_sized_section_tail_bits: u64,
    pub movement_open_section_tails: u64,
    pub movement_open_section_tail_bits: u64,
    pub array: ArrayDecodeStats,
    pub tracked_rewards_opaque_empty_variants: u64,
    pub array_leaf_decode_errors: u64,
    pub targeting_world_locations_decoded: u64,
    pub truncated_rpcs: u64,
    pub rpc_suffix_bits_dropped: u64,
    pub cnc_rpcs_emitted: u64,
    /// See `ExportStats::cnc_bruteforce_payloads_attempted`.
    pub cnc_bruteforce_payloads_attempted: u64,
    /// See `ExportStats::cnc_bruteforce_payloads_unwalked`.
    pub cnc_bruteforce_payloads_unwalked: u64,
    /// Post-RepLayout ClassNetCache tails decoded as verified RPC structure.
    pub rep_layout_cnc_tails_decoded: u64,
    /// Post-RepLayout tails retained as whole raw payloads.
    pub rep_layout_cnc_tails_preserved: u64,
}

impl SinkTotals {
    /// Fold one packet's counters in.
    ///
    /// `error_report` is passed in rather than owned because both passes merge
    /// into the *same* report: a decode error is a decode error wherever it
    /// happened, and the breakdown the summary prints is the only place a
    /// checkpoint-only failure would ever be seen.
    ///
    /// `stats` is taken by `&mut` for the two `Option<String>` fields, which are
    /// moved out rather than cloned -- they are only ever set once per run.
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
        self.array.merge_from(array);
        self.tracked_rewards_opaque_empty_variants += *tracked_rewards_opaque_empty_variants;
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

    /// Every counter a packet's sink produced lands in the `SinkTotals` field
    /// of its own name, summed across packets.
    ///
    /// `ExportSink` is rebuilt for each of a replay's ~530,000 packets, so a
    /// counter `absorb` does not carry is dropped that many times and reads as
    /// a permanent zero -- `cnc_rpcs_emitted`, the only evidence the
    /// AbilitiesAndBuffs brute-force produced RPC structure, once reached no
    /// summary at all. `absorb`'s destructure catches a counter left out, and
    /// the unused-variable lint one bound and not summed; neither sees a
    /// counter summed into the wrong field. Swapping the sized and open
    /// movement-tail targets compiled and passed every test, and would have
    /// reported a future tail under the wrong window kind.
    ///
    /// So every counter gets its own value from `next()` through a literal
    /// with no `..`, two packets are absorbed, and the totals are read back
    /// through a destructure with no `..` and each value checked by name. A
    /// counter added to `ExportStats`, `SinkTotals`, `OverlayStats` or
    /// `ArrayDecodeStats` does not compile until it is given a value and a
    /// binding here, and a binding left unchecked is an unused variable. It
    /// sits next to `absorb` rather than in the `export`-gated driver, so the
    /// core-only build runs it too.
    #[test]
    fn absorb_sums_every_counter_into_its_own_field() {
        let last = std::cell::Cell::new(0u64);
        let next = || {
            last.set(last.get() + 1);
            last.get()
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
        let landed = [
            ("fields_emitted", fields_emitted, sent.fields_emitted),
            ("rpcs_emitted", rpcs_emitted, sent.rpcs_emitted),
            ("actor_opens", actor_opens, sent.actor_opens),
            ("actor_closes", actor_closes, sent.actor_closes),
            ("content_blocks", content_blocks, sent.content_blocks),
            ("overlay.decoded_ok", decoded_ok, overlay.decoded_ok),
            ("overlay.decoded_err", decoded_err, overlay.decoded_err),
            ("overlay.raw_or_skip", raw_or_skip, overlay.raw_or_skip),
            ("overlay.not_in_table", not_in_table, overlay.not_in_table),
            (
                "overlay.no_field_name",
                no_field_name,
                overlay.no_field_name,
            ),
            (
                "overlay.handle_conflicts_refused",
                handle_conflicts_refused,
                overlay.handle_conflicts_refused,
            ),
            (
                "effect_blobs_decoded",
                effect_blobs_decoded,
                sent.effect_blobs_decoded,
            ),
            (
                "struct_blobs_decoded",
                struct_blobs_decoded,
                sent.struct_blobs_decoded,
            ),
            (
                "struct_blobs_failed",
                struct_blobs_failed,
                sent.struct_blobs_failed,
            ),
            (
                "multi_contents_items_emitted",
                multi_contents_items_emitted,
                sent.multi_contents_items_emitted,
            ),
            (
                "movement_rpc_errors",
                movement_rpc_errors,
                sent.movement_rpc_errors,
            ),
            (
                "movement_sized_section_tails",
                movement_sized_section_tails,
                sent.movement_sized_section_tails,
            ),
            (
                "movement_sized_section_tail_bits",
                movement_sized_section_tail_bits,
                sent.movement_sized_section_tail_bits,
            ),
            (
                "movement_open_section_tails",
                movement_open_section_tails,
                sent.movement_open_section_tails,
            ),
            (
                "movement_open_section_tail_bits",
                movement_open_section_tail_bits,
                sent.movement_open_section_tail_bits,
            ),
            (
                "array.elements_decoded",
                elements_decoded,
                array.elements_decoded,
            ),
            (
                "array.fields_emitted",
                array_fields_emitted,
                array.fields_emitted,
            ),
            ("array.truncations", truncations, array.truncations),
            ("array.errors", errors, array.errors),
            (
                "array.unconsumed_nested_bits",
                unconsumed_nested_bits,
                array.unconsumed_nested_bits,
            ),
            (
                "array.unconsumed_root_bits",
                unconsumed_root_bits,
                array.unconsumed_root_bits,
            ),
            (
                "array.implicit_terminations",
                implicit_terminations,
                array.implicit_terminations,
            ),
            (
                "tracked_rewards_opaque_empty_variants",
                tracked_rewards_opaque_empty_variants,
                sent.tracked_rewards_opaque_empty_variants,
            ),
            (
                "array_leaf_decode_errors",
                array_leaf_decode_errors,
                sent.array_leaf_decode_errors,
            ),
            (
                "targeting_world_locations_decoded",
                targeting_world_locations_decoded,
                sent.targeting_world_locations_decoded,
            ),
            ("truncated_rpcs", truncated_rpcs, sent.truncated_rpcs),
            (
                "rpc_suffix_bits_dropped",
                rpc_suffix_bits_dropped,
                sent.rpc_suffix_bits_dropped,
            ),
            ("cnc_rpcs_emitted", cnc_rpcs_emitted, sent.cnc_rpcs_emitted),
            (
                "cnc_bruteforce_payloads_attempted",
                cnc_bruteforce_payloads_attempted,
                sent.cnc_bruteforce_payloads_attempted,
            ),
            (
                "cnc_bruteforce_payloads_unwalked",
                cnc_bruteforce_payloads_unwalked,
                sent.cnc_bruteforce_payloads_unwalked,
            ),
            (
                "rep_layout_cnc_tails_decoded",
                rep_layout_cnc_tails_decoded,
                sent.rep_layout_cnc_tails_decoded,
            ),
            (
                "rep_layout_cnc_tails_preserved",
                rep_layout_cnc_tails_preserved,
                sent.rep_layout_cnc_tails_preserved,
            ),
        ];
        // Every counter was given a different value, so a counter summed into
        // another's field shows up here as two wrong sums.
        for (name, total, per_packet) in landed {
            assert_eq!(total, 2 * per_packet, "{name}");
        }
        assert_eq!(landed.len() as u64, last.get(), "one check per counter");
        assert_eq!(struct_blob_first_error.as_deref(), Some("first blob"));
        assert_eq!(movement_first_error.as_deref(), Some("first movement"));
        // One recorded failure per packet, folded into the shared report.
        assert_eq!(report.total_errors(), 2);
    }

    /// `fields_emitted`/`rpcs_emitted`/`actor_opens`/`actor_closes`/
    /// `content_blocks` must survive `absorb`, across more than one packet.
    /// Before this test (and the fields it checks) existed, these five
    /// counters were incremented on every packet's `ExportStats` and read by
    /// nothing: `absorb` folded in every other field but these, so the sink's
    /// own tally of what it saw never reached the summary.
    #[test]
    fn absorb_sums_the_per_packet_event_counters_across_packets() {
        let mut totals = SinkTotals::default();
        let mut report = OverlayErrorReport::default();

        let mut packet_one = ExportStats {
            fields_emitted: 3,
            rpcs_emitted: 1,
            actor_opens: 2,
            actor_closes: 1,
            content_blocks: 4,
            ..ExportStats::default()
        };
        totals.absorb(&mut packet_one, &mut report);

        let mut packet_two = ExportStats {
            fields_emitted: 5,
            rpcs_emitted: 2,
            actor_opens: 0,
            actor_closes: 3,
            content_blocks: 6,
            ..ExportStats::default()
        };
        totals.absorb(&mut packet_two, &mut report);

        assert_eq!(totals.fields_emitted, 8);
        assert_eq!(totals.rpcs_emitted, 3);
        assert_eq!(totals.actor_opens, 2);
        assert_eq!(totals.actor_closes, 4);
        assert_eq!(totals.content_blocks, 10);
    }
}
