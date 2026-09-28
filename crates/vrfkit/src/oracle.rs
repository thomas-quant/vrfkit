//! `validate` subcommand -- RepLayout grammar oracle.
//!
//! The scored scope is framed content blocks in the **ReplayData** stream;
//! Checkpoint chunks are counted, skipped and declared under `NOT COVERED`
//! ([`checkpoint_scope_note`]). A decoded RepLayout property prefix satisfies
//! this grammar, and a valid zero terminator may be followed by a separate
//! ClassNetCache tail in the block:
//!
//! ```text
//! checksum_bit : 1 bit
//! loop:
//!   handle = IntPacked   (0 -> end)
//!   payload_bits = IntPacked
//!   consume payload_bits
//! property prefix + decoded or preserved tail == declared bit_count
//! ```
//!
//! A wrong transform makes `IntPacked` return nonsense or leaves the consumed
//! bits short of the declared size. This is evidence about blocks that reached
//! framing, not a whole-file losslessness proof. [`verdict_from_stats`] lists
//! what fails the verdict; an unresolved ClassNetCache block whose whole
//! decoded payload was kept is reported apart and is not loss. Diagnostic
//! events carry packet, bunch, channel, actor, header and bit-position context
//! (`validate --diagnostics`).
//!
//! The one-block-per-replay residue, and why its first explanation (a
//! PlayerController omitting spawn velocity) was false: see
//! docs/archive/PROJECT_STATUS.md 17-A.
//!
//! The C# reference's zero `MalformedContentBlockCount` is not comparable
//! with ours. Instrumented to print its `BunchPayloadStats`, which its CLI and
//! manifest never emit, it abandons 34,292 bunches (`MalformedPayloadCount`)
//! and 49,948,659 bits (`ContentPayloadBitsSkipped`, ~6.2 MB) at the payload
//! stage on 02d4d478, never framing them, and counts 563,626 content blocks
//! to the 608,020 vrfkit counted then (~44,000 fewer). Its manifest's
//! `malformed_packet_count` is a packet-level counter from another struct.

use std::fs;
use std::time::Instant;

use vrf_container::{
    ChunkIterator, ChunkType, decompress_replay_data_with_trailing, parse_preamble,
};
use vrf_frame::{FrameSkips, walk_demo_frames};
use vrf_net::stats::{DiagnosticEvent, NetStats, SkipReason};
use vrf_schema::NetGuidCache;

use crate::error::{CliError, replication_reader};
use crate::report;
use crate::sink::{ChannelState, ExportSink, RecordBuffers};

/// What a validation run concluded, and the exit code it earns.
///
/// The two failure outcomes stay apart: "found problems" and "had nothing to
/// look at" are different answers, and a corpus sweep that merged them could
/// not tell a broken build from a file carrying no replication stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Content blocks were found and every one of them framed.
    Passed,
    /// At least one framing, payload, reassembly, or unread-ReplayData failure
    /// was observed (bytes past an archive or left by its codec). See
    /// [`Verdict::decide`].
    ValidationFailed,
    /// No RepLayout or ClassNetCache blocks at all -- nothing was validated.
    NoContentBlocks,
}

impl Verdict {
    /// Decide the verdict from the two counters that carry it.
    ///
    /// Absence of evidence outranks: a file with no content blocks validated
    /// nothing, whatever its other counters say.
    #[must_use]
    pub fn decide(total_with_content: u64, validation_failures: u64) -> Self {
        if total_with_content == 0 {
            Self::NoContentBlocks
        } else if validation_failures > 0 {
            Self::ValidationFailed
        } else {
            Self::Passed
        }
    }

    /// The process exit code for this verdict.
    #[must_use]
    pub fn exit_code(self) -> u8 {
        match self {
            Self::Passed => 0,
            Self::ValidationFailed => 1,
            Self::NoContentBlocks => 2,
        }
    }
}

/// Decide from the hard-failure counters inside the scored validation scope.
///
/// `partial_errors` is not a term: those rejections are discarded before a
/// complete bunch reaches framing, so they are outside the scored population.
/// Bytes an accumulator still holds at EOF were present and abandoned, so
/// `unfinished_partials` is loss. The depth sum (framing, malformed,
/// transform, field stream, RPC) is `NetStats::lost_content_blocks`'s alone;
/// restating it here would let this verdict and `quality.content_blocks_lost`
/// drift apart.
///
/// `bunches_on_unopened_channel` is loss of the `bunch_header_failures` class:
/// a whole bunch dropped before framing because its channel had no open actor.
/// It became a term only after measuring 0 on 45 replays (2026-09-28, `diag`
/// main and checkpoint passes plus `validate`): two from each of the local
/// archive's 21 build directories (13.01's two include the pinned 02d4d478)
/// and the three public fixtures -- 24 builds, 23,818,049 main and 185,244
/// checkpoint bunches. Its two companions are not terms:
/// `unopened_channel_bits` moves only with that count, and a failed reopen is
/// already a `bunch_header_failures` in four of its five arms. The fifth, a
/// package-map export bunch whose exports read cleanly, never reads the open
/// it carries and fails no header stage, but its displaced actor is still
/// retired, so a later payload bunch on that channel is dropped and counted
/// here. A rejected fragment stays unscored even when it carried a channel's
/// open: with no live actor on the channel, the bunches dropped after it are
/// the loss; with one, it retires nothing and later bunches frame under that
/// actor (docs/FOLLOWUP.md).
fn verdict_from_stats(stats: &NetStats, replay_data_trailing_bytes: u64) -> Verdict {
    let total_with_content = stats.rep_layout_blocks + stats.class_net_cache_blocks;
    let failures = stats.malformed_packets
        + stats.unfinished_partials
        + stats.channel_state_limit_failures
        + stats.partial_resource_limit_failures
        + stats.bunch_header_failures
        + stats.bunches_on_unopened_channel
        + stats.lost_content_blocks()
        + u64::from(replay_data_trailing_bytes != 0);
    Verdict::decide(total_with_content, failures)
}

/// Run the validate oracle; `diagnostics` prints every retained event in full.
/// A file that cannot be read is an error, not a verdict; one that was read
/// reports through [`Verdict`].
pub fn run(path: &str, diagnostics: bool) -> Result<Verdict, CliError> {
    let start = Instant::now();

    eprintln!("reading {path}...");
    let data = fs::read(path)?;
    let preamble = parse_preamble(&data)?;
    let branch = &preamble.header.replay_version.branch;
    let flags = preamble.header.flags;
    let compressed = preamble.info.compressed;
    let encrypted = preamble.info.encrypted;

    eprintln!("branch: {branch}");
    eprintln!("validating RepLayout grammar on framed ReplayData content blocks...");

    let mut cache = NetGuidCache::new();
    let mut repl_reader = replication_reader(branch)?;

    let mut total_packets: u32 = 0;
    // Packets are counted inside the frame callback, so a frame that ends
    // before its packet loop moves nothing but this.
    let mut frames_walked: u32 = 0;
    // Length-prefixed, so nothing else moves if a build starts sending them.
    let mut frame_skips = FrameSkips::default();
    // Frames whose NaN or infinite time was read as 0 ms.
    let mut non_finite_frame_times: u64 = 0;
    // Counted, not merely skipped: see `checkpoint_scope_note`.
    let mut checkpoint_chunks: u64 = 0;
    let mut replay_data_trailing_bytes = 0u64;
    let mut chunk_iter = ChunkIterator::new(&data, preamble.remaining_offset);
    let mut channel_state = ChannelState::new();
    // Never drained, and `ExportSink::new` clears them, so they stay bounded by
    // the largest packet.
    let mut buffers = RecordBuffers::default();

    while let Some(chunk) = chunk_iter.next_chunk()? {
        if chunk.chunk_type == ChunkType::Checkpoint {
            checkpoint_chunks += 1;
            continue;
        }
        if chunk.chunk_type != ChunkType::ReplayData {
            continue;
        }

        let payload = &data[chunk.data_offset..chunk.data_offset + chunk.size_in_bytes as usize];
        let (decompressed, trailing) =
            decompress_replay_data_with_trailing(payload, compressed, encrypted)?;
        replay_data_trailing_bytes += trailing as u64;

        let walk = walk_demo_frames(&decompressed, flags, &mut cache, |pkt, packet_cache| {
            let mut sink = ExportSink::new(packet_cache, &mut channel_state, &mut buffers);
            sink.enable_measured_array_routes(branch);
            sink.time_ms = pkt.time_ms;
            sink.packet_id = total_packets;
            repl_reader.process_packet(pkt.data, total_packets as i32, &mut sink);
            total_packets += 1;
        })?;
        frames_walked += walk.frames;
        frame_skips.absorb(walk.skipped);
        non_finite_frame_times += u64::from(walk.non_finite_times);
    }

    repl_reader.finish();
    let stats = repl_reader.stats();
    let elapsed = start.elapsed();

    let total_content = stats.content_blocks;
    let rep_layout = stats.rep_layout_blocks;
    let class_net = stats.class_net_cache_blocks;
    let malformed = stats.malformed_content_blocks;
    let deleted = stats.deleted_blocks;
    let rpc_payloads_lost = stats.rpc_payloads_lost();
    let failed = stats.lost_content_blocks();

    println!();
    println!("=== Validation Oracle ===");
    println!("  Branch:               {branch}");
    println!("  Total content blocks: {total_content}");
    println!("    RepLayout:          {rep_layout}");
    println!("    ClassNetCache:      {class_net}");
    println!("    Deleted:            {deleted}");
    println!("    Malformed packets:  {}", stats.malformed_packets);
    println!("    Partial bunches:    {}", report::partial_bunches(stats));
    println!("    Partial causes:     {}", report::partial_causes(stats));
    println!("    Bunch header failed:{}", stats.bunch_header_failures);
    println!(
        "    Failed reopens:     {}",
        stats.failed_reopens_while_open
    );
    println!(
        "    Unopened channel:   {} bunches / {} bits",
        stats.bunches_on_unopened_channel, stats.unopened_channel_bits
    );
    println!("    Malformed framing:  {malformed}");
    println!("    Transform failed:   {}", stats.transform_failures);
    println!("    Field stream failed:{}", stats.field_stream_failures);
    println!("    RPC payload lost:   {rpc_payloads_lost}");
    println!(
        "    RPC unresolved/raw:{}",
        stats.unresolved_rpc_payloads_preserved
    );
    println!("  Fields emitted:       {}", stats.fields);
    println!("  RPCs emitted:         {}", stats.rpcs);
    println!("  Skipped bits:         {}", stats.skipped_bits);
    println!(
        "  Unfinished partials:  {} ({} bits)",
        stats.unfinished_partials, stats.unfinished_partial_bits
    );
    println!(
        "  State resource limits: {} channel / {} partial reassembly",
        stats.channel_state_limit_failures, stats.partial_resource_limit_failures
    );
    println!(
        "  ReplayData unread:    {} bytes",
        replay_data_trailing_bytes
    );
    println!("  ReplayData frames:    {frames_walked}");
    println!(
        "  Frame skips:          {}",
        report::frame_skips(&frame_skips)
    );
    println!("  Frame times:          {non_finite_frame_times} non-finite");
    println!("  Packets:              {}", stats.packets);
    println!("  Bunches:              {}", stats.bunches);
    println!("  Actor opens:          {}", stats.actor_opens);
    println!("  Actor closes:         {}", stats.actor_closes);
    if let Some(note) = checkpoint_scope_note(checkpoint_chunks) {
        println!("  NOT COVERED:          {note}");
    }
    if let Some(note) = partial_reassembly_scope_note(stats.partial_errors) {
        println!("  NOT COVERED:          {note}");
    }
    println!();

    // The rate is over classified blocks (RepLayout + ClassNetCache).
    let total_with_content = rep_layout + class_net;
    let verdict = verdict_from_stats(stats, replay_data_trailing_bytes);
    if total_with_content == 0 {
        println!("  No content blocks found - cannot validate.");
    } else {
        println!("{}", pass_rate_line(total_with_content, failed));
        if stats.skipped_bits > 0 {
            println!("  (skipped_bits counter: {} bits)", stats.skipped_bits);
        }
    }

    // The counters above say how many payload-stage failures; these say which.
    let stream_failures = channel_state.stream_failures();
    if !stream_failures.is_empty() {
        println!();
        println!("=== Stream failures ({} shown) ===", stream_failures.len());
        for line in stream_failures {
            println!("  {line}");
        }
    }

    if !stats.diagnostics.is_empty() {
        println!();
        // The list is capped (uncapped it reaches ~100 MB on a replay whose
        // transform is wrong), so past the cap `len()` alone under-reports --
        // docs/archive/PROJECT_STATUS.md 5-A's bug, in the display layer.
        if stats.diagnostics_dropped == 0 {
            println!(
                "=== Diagnostic Events ({} total) ===",
                stats.diagnostics.len()
            );
        } else {
            println!(
                "=== Diagnostic Events ({} total, {} shown, {} dropped at the cap) ===",
                stats.diagnostics.len() + stats.diagnostics_dropped as usize,
                stats.diagnostics.len(),
                stats.diagnostics_dropped
            );
        }

        print_skip_breakdown(&stats.diagnostics);

        if diagnostics {
            println!();
            for (i, event) in stats.diagnostics.iter().enumerate() {
                print_diagnostic_event(i, event);
            }
        } else {
            println!();
            println!("  (use --diagnostics to see full event dumps)");
        }
    }

    println!();
    println!("  Elapsed: {:.2?}", elapsed);
    // The exit code in words, so a terminal and `$?` cannot disagree.
    println!("  VERDICT: {}", verdict_line(verdict));

    Ok(verdict)
}

/// The `NOT COVERED` line for the Checkpoint chunks this oracle skips.
///
/// The skip stays, stated rather than implied. A checkpoint is an independent
/// archive (its own GUID cache, export map and DemoFrame re-opening every live
/// actor) and walking it is `export --checkpoints`'s job; folding it in here
/// would move every counter `validate`'s pinned baselines hold and add three
/// hard-failure paths to a command that reports rather than aborts.
fn checkpoint_scope_note(checkpoint_chunks: u64) -> Option<String> {
    (checkpoint_chunks > 0).then(|| {
        format!(
            "{checkpoint_chunks} Checkpoint chunk(s) were NOT walked - this verdict covers the ReplayData stream only (use `export --checkpoints` to decode them)"
        )
    })
}

fn partial_reassembly_scope_note(partial_errors: u64) -> Option<String> {
    (partial_errors > 0).then(|| {
        let rejection = if partial_errors == 1 {
            "rejection was"
        } else {
            "rejections were"
        };
        format!(
            "{partial_errors} partial reassembly {rejection} discarded before content-block framing - excluded from the block score and verdict"
        )
    })
}

/// The `ORACLE PASS RATE` line for `failed` of `total_with_content` blocks.
///
/// `failed` (`lost_content_blocks()`) can exceed the classified total: a block
/// whose header or `content_bits` could not be read is lost before it is
/// classified. So `passed` saturates at 0 (a release build would wrap it) and
/// the rate saturates with it, still as `1 - failed / total` rather than
/// `passed / total`, whose last bit can differ.
fn pass_rate_line(total_with_content: u64, failed: u64) -> String {
    let passed = total_with_content.saturating_sub(failed);
    let pass_rate = 1.0 - (failed.min(total_with_content) as f64 / total_with_content as f64);
    format!(
        "  ORACLE PASS RATE:     {:.6}% ({} / {} blocks passed)",
        pass_rate * 100.0,
        passed,
        total_with_content
    )
}

/// The one-line conclusion printed under `VERDICT:`.
fn verdict_line(verdict: Verdict) -> &'static str {
    match verdict {
        Verdict::Passed => "PASS - ReplayData block validation passed (exit 0)",
        Verdict::ValidationFailed => "FAIL - ReplayData validation found loss (exit 1)",
        Verdict::NoContentBlocks => "CANNOT VALIDATE - no content blocks found (exit 2)",
    }
}

/// Print a breakdown of where skipped bits come from.
fn print_skip_breakdown(events: &[DiagnosticEvent]) {
    let mut overrun_count = 0u32;
    let mut overrun_bits = 0u64;
    let mut header_err_count = 0u32;
    let mut header_err_bits = 0u64;
    let mut bits_read_err_count = 0u32;
    let mut bits_read_err_bits = 0u64;
    let mut parse_fail_count = 0u32;
    let mut parse_fail_bits = 0u64;

    for ev in events {
        match &ev.reason {
            SkipReason::ContentBitsOverrun { .. } => {
                overrun_count += 1;
                overrun_bits += ev.bits_skipped;
            }
            SkipReason::HeaderReadError => {
                header_err_count += 1;
                header_err_bits += ev.bits_skipped;
            }
            SkipReason::ContentBitsReadError => {
                bits_read_err_count += 1;
                bits_read_err_bits += ev.bits_skipped;
            }
            SkipReason::ParseFailure => {
                parse_fail_count += 1;
                parse_fail_bits += ev.bits_skipped;
            }
        }
    }

    println!("  Skip breakdown:");
    // Zeros included, so a category that stops being recorded stays visible.
    println!("    ContentBitsOverrun:   {overrun_count} events, {overrun_bits} bits");
    println!("    HeaderReadError:      {header_err_count} events, {header_err_bits} bits");
    println!("    ContentBitsReadError: {bits_read_err_count} events, {bits_read_err_bits} bits");
    println!("    ParseFailure:         {parse_fail_count} events, {parse_fail_bits} bits");
}

/// Print full details for one diagnostic event.
fn print_diagnostic_event(index: usize, ev: &DiagnosticEvent) {
    println!("  +-- Diagnostic #{index} --");
    println!("  | Reason:              {:?}", ev.reason);
    println!("  | packet_id:           {}", ev.packet_id);
    println!("  | bunch_in_packet:     {}", ev.bunch_index_in_packet);
    println!("  | global_bunch_index:  {}", ev.global_bunch_index);
    println!("  | channel_bunch_index: {}", ev.channel_bunch_index);
    println!("  | channel_index:       {}", ev.channel_index);
    println!("  | actor_net_guid:      {}", ev.actor_net_guid);
    if let Some(ref path) = ev.actor_path {
        println!("  | actor_path:          {path}");
    }
    println!("  | archetype_net_guid:  {}", ev.archetype_net_guid);
    if let Some(ref path) = ev.class_path {
        println!("  | class_path:          {path}");
    }
    println!("  | bunch_flags:");
    let f = &ev.bunch_flags;
    println!(
        "  |   open={} close={} reliable={} partial={} partial_init={} partial_final={} pkg_map={} must_mapped={} dormant={}",
        f.b_open,
        f.b_close,
        f.b_reliable,
        f.b_partial,
        f.b_partial_initial,
        f.b_partial_final,
        f.b_has_package_map_exports,
        f.b_has_must_be_mapped_guids,
        f.b_dormant
    );
    println!("  | payload_bit_count:   {}", ev.payload_bit_count);
    println!("  | consumed_bits:       {}", ev.consumed_bits);
    println!("  | remaining_bits:      {}", ev.remaining_bits);
    if let Some(ref hdr) = ev.content_block_header {
        println!("  | content_block_header:");
        println!(
            "  |   has_rep_layout={} is_actor={} object_net_guid={} is_stably_named={} is_deleted={} class_net_guid={} outer_net_guid={} delete_flags={}",
            hdr.has_rep_layout,
            hdr.is_actor,
            hdr.object_net_guid,
            hdr.is_stably_named,
            hdr.is_deleted,
            hdr.class_net_guid,
            hdr.outer_net_guid,
            hdr.delete_flags
        );
    }
    if let Some(bits) = ev.content_bits {
        println!("  | content_bits:        {bits}");
    }
    println!("  | block_in_bunch:      {}", ev.block_index_in_bunch);
    println!("  | bits_skipped:        {}", ev.bits_skipped);
    println!("  +--------------------");
}

#[cfg(test)]
mod tests {
    use super::{
        Verdict, checkpoint_scope_note, partial_reassembly_scope_note, pass_rate_line,
        verdict_from_stats,
    };
    use vrf_net::stats::NetStats;

    #[test]
    fn skipped_checkpoint_chunks_are_named_rather_than_implied() {
        assert_eq!(
            checkpoint_scope_note(0),
            None,
            "a replay with no checkpoints has no gap to declare"
        );
        let note = checkpoint_scope_note(37).expect("37 skipped chunks must be reported");
        assert!(note.contains("37"), "the note must carry the count: {note}");
    }

    #[test]
    fn partial_reassembly_rejections_are_named_as_unscored_input() {
        assert_eq!(partial_reassembly_scope_note(0), None);
        let note = partial_reassembly_scope_note(125_037)
            .expect("a non-zero rejected population must be disclosed");
        assert!(
            note.contains("125037"),
            "the note must carry the count: {note}"
        );
        assert!(
            note.contains("before content-block framing"),
            "the note must identify the scope boundary: {note}"
        );
        assert!(
            note.contains("excluded from the block score and verdict"),
            "the note must state the scoring consequence: {note}"
        );
    }

    /// It printed "-50.000000% (0 / 2 blocks passed)", which the corpus
    /// sweeps' `([\d.]+)%` cannot read: they reported no rate at all.
    #[test]
    fn the_pass_rate_saturates_like_the_passed_count() {
        assert_eq!(
            pass_rate_line(2, 3),
            "  ORACLE PASS RATE:     0.000000% (0 / 2 blocks passed)"
        );
        assert_eq!(
            pass_rate_line(4, 1),
            "  ORACLE PASS RATE:     75.000000% (3 / 4 blocks passed)"
        );
        assert_eq!(
            pass_rate_line(4, 0),
            "  ORACLE PASS RATE:     100.000000% (4 / 4 blocks passed)"
        );
    }

    #[test]
    fn the_verdict_separates_clean_from_failed_from_unvalidatable() {
        assert_eq!(Verdict::decide(1_000, 0), Verdict::Passed);
        assert_eq!(Verdict::decide(1_000, 1), Verdict::ValidationFailed);
        assert_eq!(Verdict::decide(0, 0), Verdict::NoContentBlocks);
    }

    #[test]
    fn every_unfinished_or_payload_failure_prevents_a_pass() {
        let clean = NetStats {
            rep_layout_blocks: 1,
            ..NetStats::default()
        };
        assert_eq!(verdict_from_stats(&clean, 0), Verdict::Passed);

        for failed in [
            NetStats {
                rep_layout_blocks: 1,
                malformed_packets: 1,
                ..NetStats::default()
            },
            NetStats {
                rep_layout_blocks: 1,
                bunch_header_failures: 1,
                ..NetStats::default()
            },
            NetStats {
                rep_layout_blocks: 1,
                transform_failures: 1,
                ..NetStats::default()
            },
            NetStats {
                rep_layout_blocks: 1,
                field_stream_failures: 1,
                ..NetStats::default()
            },
            NetStats {
                class_net_cache_blocks: 1,
                rpc_stream_failures: 1,
                ..NetStats::default()
            },
            NetStats {
                rep_layout_blocks: 1,
                unfinished_partials: 1,
                ..NetStats::default()
            },
            NetStats {
                rep_layout_blocks: 1,
                partial_resource_limit_failures: 1,
                ..NetStats::default()
            },
            NetStats {
                rep_layout_blocks: 1,
                channel_state_limit_failures: 1,
                ..NetStats::default()
            },
            NetStats {
                rep_layout_blocks: 1,
                content_block_framing_failures: 1,
                ..NetStats::default()
            },
            NetStats {
                rep_layout_blocks: 1,
                bunches_on_unopened_channel: 1,
                unopened_channel_bits: 10,
                ..NetStats::default()
            },
        ] {
            assert_eq!(verdict_from_stats(&failed, 0), Verdict::ValidationFailed);
        }
        assert_eq!(
            verdict_from_stats(&clean, 1),
            Verdict::ValidationFailed,
            "unconsumed decompressed ReplayData bytes must fail validation"
        );

        let unresolved_but_preserved = NetStats {
            class_net_cache_blocks: 1,
            rpc_stream_failures: 1,
            unresolved_rpc_payloads_preserved: 1,
            ..NetStats::default()
        };
        assert_eq!(
            verdict_from_stats(&unresolved_but_preserved, 0),
            Verdict::Passed,
            "an unresolved RPC whose whole decoded payload was preserved is not data loss"
        );

        let reassembly_rejection = NetStats {
            rep_layout_blocks: 1,
            partial_errors: 1,
            ..NetStats::default()
        };
        assert_eq!(
            verdict_from_stats(&reassembly_rejection, 0),
            Verdict::Passed,
            "partial reassembly rejections are reported as unscored, not silently treated as validated blocks"
        );
    }

    #[test]
    fn each_verdict_earns_its_own_exit_code() {
        assert_eq!(Verdict::Passed.exit_code(), 0);
        assert_eq!(Verdict::ValidationFailed.exit_code(), 1);
        assert_eq!(Verdict::NoContentBlocks.exit_code(), 2);
    }
}
