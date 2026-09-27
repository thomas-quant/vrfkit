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
//! What this does NOT cover: the printers. The export summary, the manifest's
//! `quality` block and the `diag` JSON each still name their lines by hand, so
//! a counter can reach this struct and still be printed nowhere. The `diag`
//! list is checked against this struct by a test in `diagnose.rs`.

use vrf_decode::{ArrayDecodeStats, OverlayErrorReport, OverlayStats};

use super::ExportStats;

/// Everything a packet's sink counted, summed across packets.
#[derive(Debug, Default)]
pub(crate) struct SinkTotals {
    /// Rows emitted at the sites that count them: property rows, RPC
    /// parameter rows and whole-payload fallbacks, flattened array leaves,
    /// `_cnc_h*` rows and RepLayout tail rows. A row count, and not every row
    /// (movement batches, zero-bit RPC markers and unresolved-payload
    /// preservation rows are not counted), so it is comparable neither with
    /// NetStats' `fields` -- framed RepLayout properties -- nor with
    /// `fields.parquet`. On 02d4d478 it reads 1,060,119 against `Fields:
    /// 429,648` and 1,296,660 table rows.
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
    use vrf_decode::OverlayErrorReport;

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
