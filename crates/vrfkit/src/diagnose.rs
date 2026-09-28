//! `diag` subcommand -- a stats-only pass over the whole replay.
//!
//! `validate`'s failure diagnostics are capped twice (the 32-line
//! `ChannelState::stream_failures` window and the capped `NetStats`
//! diagnostics log): right for reading one replay, useless for counting a
//! population. `validate` walking checkpoints would move every counter its
//! pinned baselines hold (see `oracle.rs`'s `checkpoint_scope_note`), and an
//! `export` without writes would still be the export path, whose contract is
//! writing the files. So `diag` drives the same sink over ReplayData and every
//! Checkpoint chunk, keeps the passes apart, writes no table, and emits one
//! JSON document aggregating every stream failure ([`FailureAggregate`]) by
//! kind, cause, group path, function count and handle. It prints no verdict
//! and exits 0 for any readable replay: judging is `validate`'s job, and a
//! second oracle would drift from the first.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use vrf_container::{
    ChunkIterator, ChunkType, decompress_checkpoint, decompress_replay_data_with_trailing,
    parse_checkpoint_chunk, parse_preamble,
};
use vrf_decode::OverlayErrorReport;
use vrf_frame::{FrameSkips, walk_demo_frames};
use vrf_net::stats::NetStats;
use vrf_schema::{NetGuidCache, read_checkpoint_tables};

use crate::error::{CliError, replication_reader};
use crate::sink::{ChannelState, ExportSink, FailureAggregate, RecordBuffers, SinkTotals};

/// The checkpoint pass's counters and per-chunk metadata, so the walk is
/// auditable rather than a single printed number.
#[derive(Debug, Default)]
struct DiagCheckpointStats {
    chunks: u64,
    frames: u64,
    /// Section bytes the snapshot frames stepped over.
    frame_skips: FrameSkips,
    /// Snapshot frames with a NaN or infinite time, read as 0 ms.
    non_finite_frame_times: u64,
    packets: u64,
    trailing_bytes: u64,
    guid_entries: u64,
    group_records: u64,
    exported_fields: u64,
    /// Field rows the snapshot produced: counted, never written (`export
    /// --checkpoints` writes them to `checkpoint_fields.parquet`).
    field_rows_dropped: u64,
    actor_rows_dropped: u64,
    movement_rows_dropped: u64,
    net: NetStats,
    sink: SinkTotals,
    /// Where `absorb` merges the per-field overlay breakdown; never printed,
    /// as the JSON carries counters only.
    overlay_errors: OverlayErrorReport,
    failures: FailureAggregate,
}

/// Run the diag pass over one replay, writing the aggregate JSON to
/// `json_path` when given, to stdout otherwise.
pub fn run(path: &str, json_path: Option<&str>, include_payloads: bool) -> Result<(), CliError> {
    if let Some(output) = json_path {
        reject_input_output_alias(path, output)?;
    }
    eprintln!("reading {path}...");
    let data = fs::read(path)?;
    let file_size = data.len();
    let preamble = parse_preamble(&data)?;
    let branch = preamble.header.replay_version.branch.clone();
    let flags = preamble.header.flags;
    let compressed = preamble.info.compressed;
    let encrypted = preamble.info.encrypted;
    eprintln!("branch: {branch}");
    eprintln!("diag: walking ReplayData and Checkpoint chunks, writing no table...");

    let mut cache = NetGuidCache::new();
    let mut repl_reader = replication_reader(&branch)?;

    let mut total_packets: u32 = 0;
    let mut replay_data_chunks: u64 = 0;
    let mut replay_data_frames: u64 = 0;
    let mut replay_data_frame_skips = FrameSkips::default();
    let mut replay_data_non_finite_frame_times: u64 = 0;
    let mut event_chunks: u64 = 0;
    let mut replay_data_trailing_bytes: u64 = 0;
    let mut sink_totals = SinkTotals::default();
    // Never printed, like `DiagCheckpointStats::overlay_errors`.
    let mut overlay_errors = OverlayErrorReport::default();
    let mut channel_state = ChannelState::new();
    channel_state.enable_failure_aggregate(include_payloads);
    // Never drained: nothing is written, and `ExportSink::new` clears them.
    let mut buffers = RecordBuffers::default();

    let mut cp_stats = DiagCheckpointStats {
        failures: FailureAggregate::new(include_payloads),
        ..DiagCheckpointStats::default()
    };

    let mut chunk_iter = ChunkIterator::new(&data, preamble.remaining_offset);
    while let Some(chunk) = chunk_iter.next_chunk()? {
        let payload = &data[chunk.data_offset..chunk.data_offset + chunk.size_in_bytes as usize];
        match chunk.chunk_type {
            ChunkType::Event => {
                // Independent of replication, with no stream-failure signal.
                event_chunks += 1;
            }
            ChunkType::Checkpoint => {
                process_checkpoint_chunk(
                    payload,
                    &branch,
                    flags,
                    compressed,
                    encrypted,
                    include_payloads,
                    &mut cp_stats,
                )?;
            }
            ChunkType::ReplayData => {
                let (decompressed, trailing) =
                    decompress_replay_data_with_trailing(payload, compressed, encrypted)?;
                replay_data_trailing_bytes += trailing as u64;
                replay_data_chunks += 1;
                let walk =
                    walk_demo_frames(&decompressed, flags, &mut cache, |pkt, packet_cache| {
                        let pkt_id = total_packets;
                        total_packets += 1;
                        let mut sink =
                            ExportSink::new(packet_cache, &mut channel_state, &mut buffers);
                        sink.enable_measured_array_routes(&branch);
                        sink.time_ms = pkt.time_ms;
                        sink.packet_id = pkt_id;
                        repl_reader.process_packet(pkt.data, pkt_id as i32, &mut sink);
                        sink_totals.absorb(&mut sink.stats, &mut overlay_errors);
                    })?;
                replay_data_frames += u64::from(walk.frames);
                replay_data_frame_skips.absorb(walk.skipped);
                replay_data_non_finite_frame_times += u64::from(walk.non_finite_times);
            }
            ChunkType::Header | ChunkType::Unknown(_) => {}
        }
    }

    repl_reader.finish();
    let net_main = repl_reader.stats().clone();
    let main_failures = channel_state.take_failure_aggregate();
    let mut json = String::with_capacity(1 << 16);
    json.push_str("{\n");
    json.push_str("  \"schema_version\": 3,\n");
    json.push_str("  \"tool\": \"vrfkit diag\",\n");
    json.push_str("  \"file\": ");
    push_json_string(&mut json, path);
    json.push_str(",\n");
    json.push_str(&format!("  \"file_size\": {file_size},\n"));
    json.push_str("  \"branch\": ");
    push_json_string(&mut json, &branch);
    json.push_str(",\n");
    json.push_str("  \"build\": ");
    push_json_string(&mut json, build_label(&branch));
    json.push_str(",\n");
    json.push_str(
        "  \"options\": {\"write_tables\": false, \"walks_checkpoints\": true, \
         \"include_payloads\": ",
    );
    json.push_str(if include_payloads { "true" } else { "false" });
    json.push_str("},\n");
    json.push_str("  \"chunks\": {\"replay_data\": ");
    json.push_str(&replay_data_chunks.to_string());
    json.push_str(", \"replay_data_frames\": ");
    json.push_str(&replay_data_frames.to_string());
    json.push_str(", \"event\": ");
    json.push_str(&event_chunks.to_string());
    json.push_str(", \"replay_data_trailing_bytes\": ");
    json.push_str(&replay_data_trailing_bytes.to_string());
    push_frame_skips(&mut json, "replay_data_", &replay_data_frame_skips);
    json.push_str(", \"replay_data_non_finite_frame_times\": ");
    json.push_str(&replay_data_non_finite_frame_times.to_string());
    json.push_str("},\n");

    json.push_str("  \"net_main\": ");
    push_net_stats(&mut json, &net_main);
    json.push_str(",\n");
    json.push_str("  \"sink_main\": ");
    push_sink_totals(&mut json, &sink_totals);
    json.push_str(",\n");
    json.push_str("  \"checkpoint_meta\": {\"chunks\": ");
    json.push_str(&cp_stats.chunks.to_string());
    json.push_str(", \"frames\": ");
    json.push_str(&cp_stats.frames.to_string());
    json.push_str(", \"packets\": ");
    json.push_str(&cp_stats.packets.to_string());
    json.push_str(", \"trailing_bytes\": ");
    json.push_str(&cp_stats.trailing_bytes.to_string());
    push_frame_skips(&mut json, "", &cp_stats.frame_skips);
    json.push_str(", \"non_finite_frame_times\": ");
    json.push_str(&cp_stats.non_finite_frame_times.to_string());
    json.push_str(", \"guid_entries\": ");
    json.push_str(&cp_stats.guid_entries.to_string());
    json.push_str(", \"group_records\": ");
    json.push_str(&cp_stats.group_records.to_string());
    json.push_str(", \"exported_fields\": ");
    json.push_str(&cp_stats.exported_fields.to_string());
    json.push_str(", \"field_rows_dropped\": ");
    json.push_str(&cp_stats.field_rows_dropped.to_string());
    json.push_str(", \"actor_rows_dropped\": ");
    json.push_str(&cp_stats.actor_rows_dropped.to_string());
    json.push_str(", \"movement_rows_dropped\": ");
    json.push_str(&cp_stats.movement_rows_dropped.to_string());
    json.push_str("},\n");
    json.push_str("  \"net_checkpoint\": ");
    push_net_stats(&mut json, &cp_stats.net);
    json.push_str(",\n");
    json.push_str("  \"sink_checkpoint\": ");
    push_sink_totals(&mut json, &cp_stats.sink);
    json.push_str(",\n");

    json.push_str("  \"failures\": {\n");
    json.push_str("    \"main\": ");
    push_failure_aggregate(&mut json, &main_failures);
    json.push_str(",\n    \"checkpoint\": ");
    push_failure_aggregate(&mut json, &cp_stats.failures);
    json.push_str("\n  }\n");
    json.push_str("}\n");

    match json_path {
        Some(out) => write_json_file(out, &json)?,
        None => println!("{json}"),
    }

    // A one-line stderr receipt wherever the JSON went, so a caller sees the
    // reconciliation shape without parsing JSON.
    eprintln!(
        "diag: main failures {} (payloads preserved {}, real loss {}) | \
         checkpoint failures {} (payloads preserved {}, real loss {})",
        main_failures.total_failures(),
        main_failures.preserved_unresolved(),
        main_failures.real_loss(),
        cp_stats.failures.total_failures(),
        cp_stats.failures.preserved_unresolved(),
        cp_stats.failures.real_loss(),
    );
    Ok(())
}

/// Refuse an output path that resolves to the replay itself: the JSON is built
/// in memory and written last, so a successful run could replace its source.
fn reject_input_output_alias(input: &str, output: &str) -> Result<(), CliError> {
    let input = fs::canonicalize(input)?;
    let output = canonicalize_destination(output)?;
    if input == output {
        return Err(CliError::Usage(
            "--json output must differ from the input replay".to_string(),
        ));
    }
    Ok(())
}

fn canonicalize_destination(path: &str) -> Result<PathBuf, CliError> {
    match fs::canonicalize(path) {
        Ok(path) => Ok(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let path = Path::new(path);
            let parent = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            let name = path.file_name().ok_or_else(|| {
                CliError::Usage("--json requires a file path, not a directory".to_string())
            })?;
            Ok(fs::canonicalize(parent)?.join(name))
        }
        Err(error) => Err(error.into()),
    }
}

/// Publish JSON through a new sibling file rather than opening the destination
/// inode for truncation. This keeps the replay intact when a differently named
/// hard link is supplied as `--json`; replacing that directory entry detaches
/// the link instead of writing through it.
fn write_json_file(path: &str, json: &str) -> Result<(), CliError> {
    let path = Path::new(path);
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = path.file_name().ok_or_else(|| {
        CliError::Usage("--json requires a file path, not a directory".to_string())
    })?;

    let mut created = None;
    for attempt in 0..64u32 {
        let temp = parent.join(format!(
            ".{}.vrfkit-{}-{attempt}.tmp",
            name.to_string_lossy(),
            std::process::id()
        ));
        match OpenOptions::new().write(true).create_new(true).open(&temp) {
            Ok(file) => {
                created = Some((file, temp));
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    let Some((mut file, temp)) = created else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "could not reserve a temporary diagnostic output file",
        )
        .into());
    };

    if let Err(error) = file
        .write_all(json.as_bytes())
        .and_then(|()| file.sync_all())
    {
        drop(file);
        let _ = fs::remove_file(&temp);
        return Err(error.into());
    }
    drop(file);

    #[cfg(windows)]
    if path.try_exists()? {
        if let Err(error) = fs::remove_file(path) {
            let _ = fs::remove_file(&temp);
            return Err(error.into());
        }
    }
    if let Err(error) = fs::rename(&temp, path) {
        let _ = fs::remove_file(&temp);
        return Err(error.into());
    }
    Ok(())
}

/// Walk one Checkpoint chunk as `driver::checkpoints::process_chunk` does,
/// minus the writers: a fresh GUID cache, export map, reader and channel state
/// per archive.
fn process_checkpoint_chunk(
    payload: &[u8],
    branch: &str,
    flags: u32,
    compressed: bool,
    encrypted: bool,
    include_payloads: bool,
    cp: &mut DiagCheckpointStats,
) -> Result<(), CliError> {
    let cp_chunk = parse_checkpoint_chunk(payload)?;
    cp.trailing_bytes += cp_chunk.trailing_bytes as u64;
    let plain = decompress_checkpoint(cp_chunk.archive, compressed, encrypted)?;

    let mut cache = NetGuidCache::new();
    let tables = read_checkpoint_tables(&plain, &mut cache)
        .map_err(|e| CliError::Usage(format!("checkpoint {}: {e}", cp_chunk.id)))?;

    let frame = &plain[tables.frame_offset..];
    let mut reader = replication_reader(branch)?;
    let mut channels = ChannelState::new();
    channels.enable_failure_aggregate(include_payloads);
    let mut buffers = RecordBuffers::default();
    let mut packet_count = 0u64;
    let walk = walk_demo_frames(frame, flags, &mut cache, |pkt, packet_cache| {
        {
            let mut sink = ExportSink::new(packet_cache, &mut channels, &mut buffers);
            sink.enable_measured_array_routes(branch);
            sink.time_ms = pkt.time_ms;
            sink.packet_id = packet_count as u32;
            reader.process_packet(pkt.data, packet_count as i32, &mut sink);
            cp.sink.absorb(&mut sink.stats, &mut cp.overlay_errors);
        }
        cp.field_rows_dropped += buffers.fields.len() as u64;
        cp.actor_rows_dropped += buffers.actors.len() as u64;
        cp.movement_rows_dropped += buffers.movement.len() as u64;
        packet_count += 1;
    })?;
    reader.finish();
    let mut chunk_net = reader.stats().clone();
    cp.net.absorb(&mut chunk_net);
    let mut chunk_failures = channels.take_failure_aggregate();
    cp.failures.absorb(&mut chunk_failures);

    cp.chunks += 1;
    cp.frames += u64::from(walk.frames);
    cp.frame_skips.absorb(walk.skipped);
    cp.non_finite_frame_times += u64::from(walk.non_finite_times);
    cp.packets += packet_count;
    cp.guid_entries += u64::from(tables.guid_count);
    cp.group_records += u64::from(tables.group_count);
    cp.exported_fields += u64::from(tables.exported_fields);
    Ok(())
}

/// `++Ares-Core+release-13.04` -> `13.04`, the corpus label downstream; a
/// branch without the marker stays `unknown` rather than guessed.
fn build_label(branch: &str) -> &str {
    branch
        .rsplit("release-")
        .next()
        .filter(|rest| !rest.is_empty() && *rest != branch)
        .unwrap_or("unknown")
}

/// Append `s` as a quoted, escaped JSON string. Characters outside printable
/// ASCII are emitted as UTF-16 JSON escapes, including surrogate pairs, so the
/// Windows console never receives raw non-ASCII text.
fn push_json_string(out: &mut String, s: &str) {
    out.push('"');
    for byte in s.chars() {
        match byte {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || (c as u32) > 0x7E => {
                let mut units = [0u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Append the three [`FrameSkips`] tallies as `, "<prefix>external_data_blobs": N`
/// and so on, inside an object the caller has already opened.
fn push_frame_skips(out: &mut String, prefix: &str, skips: &FrameSkips) {
    for (key, value) in [
        ("external_data_blobs", skips.external_data_blobs),
        ("external_data_bytes", skips.external_data_bytes),
        ("game_specific_bytes", skips.game_specific_bytes),
    ] {
        out.push_str(&format!(", \"{prefix}{key}\": {value}"));
    }
}

/// `{`, one `"key": value` member per line, then `  }` -- the shape both
/// counter objects share.
fn push_members(out: &mut String, members: &[(&str, u64)]) {
    out.push_str("{\n");
    for (i, (name, value)) in members.iter().enumerate() {
        let comma = if i + 1 < members.len() { "," } else { "" };
        out.push_str(&format!("    \"{name}\": {value}{comma}\n"));
    }
    out.push_str("  }");
}

fn push_net_stats(out: &mut String, s: &NetStats) {
    push_members(
        out,
        &[
            ("packets", s.packets),
            ("malformed_packets", s.malformed_packets),
            ("bunches", s.bunches),
            ("partial_errors", s.partial_errors),
            ("partial_bunches", s.partial_bunches),
            ("partial_missing_initial", s.partial_missing_initial),
            (
                "partial_missing_initial_final",
                s.partial_missing_initial_final,
            ),
            (
                "partial_missing_initial_reliable",
                s.partial_missing_initial_reliable,
            ),
            (
                "partial_missing_initial_bits",
                s.partial_missing_initial_bits,
            ),
            ("partial_overlapping_initial", s.partial_overlapping_initial),
            (
                "partial_mismatched_continuation",
                s.partial_mismatched_continuation,
            ),
            ("partial_non_byte_aligned", s.partial_non_byte_aligned),
            ("partial_channel_close", s.partial_channel_close),
            (
                "partial_unclassified_errors",
                s.partial_unclassified_errors(),
            ),
            (
                "partial_overclassified_errors",
                s.partial_overclassified_errors(),
            ),
            ("partial_fragments", s.partial_fragments),
            ("partial_completed", s.partial_completed),
            ("unfinished_partials", s.unfinished_partials),
            ("unfinished_partial_bits", s.unfinished_partial_bits),
            ("bunch_header_failures", s.bunch_header_failures),
            ("content_blocks", s.content_blocks),
            ("rep_layout_blocks", s.rep_layout_blocks),
            ("class_net_cache_blocks", s.class_net_cache_blocks),
            ("deleted_blocks", s.deleted_blocks),
            ("fields", s.fields),
            ("rpcs", s.rpcs),
            ("skipped_bits", s.skipped_bits),
            (
                "content_block_framing_failures",
                s.content_block_framing_failures,
            ),
            ("malformed_content_blocks", s.malformed_content_blocks),
            ("transform_failures", s.transform_failures),
            ("field_stream_failures", s.field_stream_failures),
            ("rpc_stream_failures", s.rpc_stream_failures),
            (
                "unresolved_rpc_payloads_preserved",
                s.unresolved_rpc_payloads_preserved,
            ),
            ("actor_opens", s.actor_opens),
            ("actor_closes", s.actor_closes),
            ("channel_reopens_while_open", s.channel_reopens_while_open),
            ("actor_opens_missing_spawn", s.actor_opens_missing_spawn),
            ("failed_reopens_while_open", s.failed_reopens_while_open),
            ("bunches_on_unopened_channel", s.bunches_on_unopened_channel),
            ("unopened_channel_bits", s.unopened_channel_bits),
            (
                "channel_state_limit_failures",
                s.channel_state_limit_failures,
            ),
            (
                "partial_resource_limit_failures",
                s.partial_resource_limit_failures,
            ),
            ("package_map_exports", s.package_map_exports),
            ("rep_layout_export_bunches", s.rep_layout_export_bunches),
            ("exported_guids", s.exported_guids),
            ("must_be_mapped_guids", s.must_be_mapped_guids),
            ("content_blocks_lost", s.lost_content_blocks()),
        ],
    );
}

fn push_sink_totals(out: &mut String, s: &SinkTotals) {
    push_members(
        out,
        &[
            ("fields_emitted", s.fields_emitted),
            ("rpcs_emitted", s.rpcs_emitted),
            ("actor_opens", s.actor_opens),
            ("actor_closes", s.actor_closes),
            ("content_blocks", s.content_blocks),
            ("overlay_decoded_ok", s.overlay.decoded_ok),
            ("overlay_decoded_err", s.overlay.decoded_err),
            ("overlay_raw_or_skip", s.overlay.raw_or_skip),
            ("overlay_not_in_table", s.overlay.not_in_table),
            ("overlay_no_field_name", s.overlay.no_field_name),
            (
                "overlay_handle_conflicts_refused",
                s.overlay.handle_conflicts_refused,
            ),
            ("effect_blobs_decoded", s.effect_blobs_decoded),
            ("struct_blobs_decoded", s.struct_blobs_decoded),
            ("struct_blobs_failed", s.struct_blobs_failed),
            (
                "multi_contents_items_emitted",
                s.multi_contents_items_emitted,
            ),
            ("movement_rpc_errors", s.movement_rpc_errors),
            (
                "movement_sized_section_tails",
                s.movement_sized_section_tails,
            ),
            (
                "movement_sized_section_tail_bits",
                s.movement_sized_section_tail_bits,
            ),
            ("movement_open_section_tails", s.movement_open_section_tails),
            (
                "movement_open_section_tail_bits",
                s.movement_open_section_tail_bits,
            ),
            ("movement_envelope_trailers", s.movement_envelope_trailers),
            (
                "movement_envelope_trailer_bits",
                s.movement_envelope_trailer_bits,
            ),
            ("array_elements_decoded", s.array.elements_decoded),
            ("array_fields_emitted", s.array.fields_emitted),
            ("array_truncations", s.array.truncations),
            ("array_errors", s.array.errors),
            (
                "array_unconsumed_nested_bits",
                s.array.unconsumed_nested_bits,
            ),
            ("array_implicit_terminations", s.array.implicit_terminations),
            ("array_unconsumed_root_bits", s.array.unconsumed_root_bits),
            ("array_leaf_decode_errors", s.array_leaf_decode_errors),
            (
                "targeting_world_locations_decoded",
                s.targeting_world_locations_decoded,
            ),
            (
                "tracked_rewards_opaque_empty_variants",
                s.tracked_rewards_opaque_empty_variants,
            ),
            (
                "active_blinds_empty_trailers",
                s.active_blinds_empty_trailers,
            ),
            ("truncated_rpcs", s.truncated_rpcs),
            ("rpc_suffix_bits_dropped", s.rpc_suffix_bits_dropped),
            ("cnc_rpcs_emitted", s.cnc_rpcs_emitted),
            (
                "cnc_bruteforce_payloads_attempted",
                s.cnc_bruteforce_payloads_attempted,
            ),
            (
                "cnc_bruteforce_payloads_unwalked",
                s.cnc_bruteforce_payloads_unwalked,
            ),
            (
                "rep_layout_cnc_tails_decoded",
                s.rep_layout_cnc_tails_decoded,
            ),
            (
                "rep_layout_cnc_tails_preserved",
                s.rep_layout_cnc_tails_preserved,
            ),
        ],
    );
}

fn push_failure_aggregate(out: &mut String, agg: &FailureAggregate) {
    let overflow = agg.overflow();
    out.push_str(&format!(
        "{{\"total_failures\": {}, \"preserved_unresolved\": {}, \"real_loss\": {}, \
         \"cell_limit\": {}, \"overflow\": {{\"count\": {}, \"bit_count_total\": {}, \
         \"consumed_bits_total\": {}, \"abandoned_bits_total\": {}}}, \"payloads_included\": {}, \
         \"cells\": [",
        agg.total_failures(),
        agg.preserved_unresolved(),
        agg.real_loss(),
        FailureAggregate::cell_limit(),
        overflow.count,
        overflow.bit_count_total,
        overflow.consumed_bits_total,
        overflow.abandoned_bits_total,
        agg.retains_payloads(),
    ));
    for (i, (key, cell)) in agg.cells_sorted().iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str("{\"kind\": ");
        push_json_string(out, kind_name(key.kind));
        out.push_str(", \"cause\": ");
        push_json_string(out, cause_name(key.cause));
        out.push_str(", \"group_path\": ");
        push_json_string(out, &key.group_path);
        out.push_str(&format!(
            ", \"function_count\": {}, \"record_handle\": {}, \"consumed_bits\": {}, \
             \"payload_preserved\": {}, \"count\": {}, \"bit_count_total\": {}, \
             \"consumed_bits_total\": {}, \"abandoned_bits_total\": {}, \"samples\": [",
            key.function_count,
            json_number_or_null(key.record_handle),
            key.consumed_bits,
            key.payload_preserved,
            cell.count,
            cell.bit_count_total,
            cell.consumed_bits_total,
            cell.abandoned_bits_total,
        ));
        for (j, sample) in cell.samples.iter().enumerate() {
            if j > 0 {
                out.push_str(", ");
            }
            out.push_str(&format!(
                "{{\"actor_net_guid\": {}, \"bit_count\": {}, \"consumed_bits\": {}, \
                 \"payload_preserved\": {}, \"abandoned_bits\": {}, \"record_offset\": {}, \
                 \"payload_hex\": ",
                sample.actor_net_guid,
                sample.bit_count,
                sample.consumed_bits,
                sample.payload_preserved,
                sample.abandoned_bits,
                json_number_or_null(sample.record_offset),
            ));
            match &sample.payload_hex {
                Some(hex) => push_json_string(out, hex),
                None => out.push_str("null"),
            }
            out.push_str(&format!(
                ", \"payload_truncated\": {}}}",
                sample.payload_truncated
            ));
        }
        out.push_str("]}");
    }
    out.push_str("]}");
}

fn json_number_or_null<T: std::fmt::Display>(value: Option<T>) -> String {
    value.map_or_else(|| "null".to_owned(), |value| value.to_string())
}

fn kind_name(kind: vrf_net::pipeline::StreamKind) -> &'static str {
    match kind {
        vrf_net::pipeline::StreamKind::RepLayout => "RepLayout",
        vrf_net::pipeline::StreamKind::Rpc => "Rpc",
    }
}

fn cause_name(cause: vrf_net::pipeline::StreamFailureCause) -> &'static str {
    match cause {
        vrf_net::pipeline::StreamFailureCause::AbandonedTail => "AbandonedTail",
        vrf_net::pipeline::StreamFailureCause::ReadError => "ReadError",
        vrf_net::pipeline::StreamFailureCause::UnresolvedFunctionCount => "UnresolvedFunctionCount",
        vrf_net::pipeline::StreamFailureCause::UnverifiedRepLayoutTail => "UnverifiedRepLayoutTail",
        vrf_net::pipeline::StreamFailureCause::WindowOpenFailed => "WindowOpenFailed",
    }
}

#[cfg(test)]
mod tests {
    use super::{
        FrameSkips, build_label, json_number_or_null, push_frame_skips, push_json_string,
        push_net_stats, push_sink_totals, reject_input_output_alias, write_json_file,
    };
    use crate::sink::SinkTotals;
    use vrf_decode::{ArrayDecodeStats, OverlayErrorReport, OverlayStats};
    use vrf_net::stats::NetStats;

    #[test]
    fn frame_skips_json_carries_every_tally_under_its_prefix() {
        let mut skips = FrameSkips::default();
        skips.external_data_blobs = 2;
        skips.external_data_bytes = 9;
        let mut json = String::from("{\"first\": 1");
        push_frame_skips(&mut json, "replay_data_", &skips);
        json.push('}');
        assert_eq!(
            json,
            "{\"first\": 1, \"replay_data_external_data_blobs\": 2, \
             \"replay_data_external_data_bytes\": 9, \
             \"replay_data_game_specific_bytes\": 0}"
        );
    }

    /// Distinct values, so a key wired to the wrong field shows.
    #[test]
    fn net_stats_json_carries_the_channel_guard_counters() {
        let stats = NetStats {
            failed_reopens_while_open: 3,
            bunches_on_unopened_channel: 5,
            unopened_channel_bits: 7,
            ..NetStats::default()
        };
        let mut json = String::new();
        push_net_stats(&mut json, &stats);
        for expected in [
            "\"failed_reopens_while_open\": 3",
            "\"bunches_on_unopened_channel\": 5",
            "\"unopened_channel_bits\": 7",
        ] {
            assert!(json.contains(expected), "missing {expected}: {json}");
        }
    }

    #[test]
    fn build_labels_come_from_the_release_marker() {
        assert_eq!(build_label("++Ares-Core+release-13.01"), "13.01");
        assert_eq!(build_label("++Ares-Core+release-13.05"), "13.05");
        assert_eq!(build_label("++Ares-Core+dev"), "unknown");
    }

    /// A replay-declared path is the only free-form text this emitter writes.
    #[test]
    fn json_strings_escape_terminators_and_control_bytes() {
        let mut out = String::new();
        push_json_string(&mut out, "a\"b\\c\nd\u{1}e<f&>");
        assert_eq!(out, "\"a\\\"b\\\\c\\nd\\u0001e<f&>\"");
    }

    #[test]
    fn json_strings_encode_non_bmp_as_surrogate_pairs() {
        let mut out = String::new();
        push_json_string(&mut out, "x\u{1f600}y");
        assert_eq!(out, "\"x\\ud83d\\ude00y\"");
    }

    /// No replay has produced a failure cell with a record handle, so the
    /// present branch is pinned here rather than by any real output.
    #[test]
    fn optional_numbers_render_as_the_number_or_null() {
        assert_eq!(json_number_or_null(Some(7u32)), "7");
        assert_eq!(json_number_or_null(None::<u64>), "null");
    }

    #[test]
    fn json_output_cannot_alias_the_input() {
        let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/diagnose.rs");
        let source = source.to_str().unwrap();
        let error = reject_input_output_alias(source, source).unwrap_err();
        assert!(error.to_string().contains("must differ"));
    }

    #[test]
    fn json_output_replaces_a_hardlink_without_truncating_its_source() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "vrfkit-diag-hardlink-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir(&dir).unwrap();
        let source = dir.join("source.vrf");
        let output = dir.join("report.json");
        std::fs::write(&source, b"original replay bytes").unwrap();
        std::fs::hard_link(&source, &output).unwrap();

        write_json_file(output.to_str().unwrap(), "{\"ok\":true}\n").unwrap();

        assert_eq!(std::fs::read(&source).unwrap(), b"original replay bytes");
        assert_eq!(std::fs::read(&output).unwrap(), b"{\"ok\":true}\n");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A counter missing from the hand-written key list is absent, not 0. The
    /// literal has no `..`, so a new counter stops this compiling until it takes
    /// the next `next()` value, which a printer that omits it then lacks; the
    /// expected set is counted from the literal, so no second number drifts.
    #[test]
    fn push_sink_totals_prints_every_sink_counter_exactly_once() {
        let last = std::cell::Cell::new(0u64);
        let next = || {
            last.set(last.get() + 1);
            last.get()
        };
        let totals = SinkTotals {
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
                error_report: OverlayErrorReport::default(),
            },
            effect_blobs_decoded: next(),
            struct_blobs_decoded: next(),
            struct_blobs_failed: next(),
            // Text, not a counter: the diag JSON carries counters only.
            struct_blob_first_error: None,
            multi_contents_items_emitted: next(),
            movement_rpc_errors: next(),
            movement_first_error: None,
            movement_sized_section_tails: next(),
            movement_sized_section_tail_bits: next(),
            movement_open_section_tails: next(),
            movement_open_section_tail_bits: next(),
            movement_envelope_trailers: next(),
            movement_envelope_trailer_bits: next(),
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
            array_leaf_decode_errors: next(),
            targeting_world_locations_decoded: next(),
            truncated_rpcs: next(),
            rpc_suffix_bits_dropped: next(),
            cnc_rpcs_emitted: next(),
            cnc_bruteforce_payloads_attempted: next(),
            cnc_bruteforce_payloads_unwalked: next(),
            rep_layout_cnc_tails_decoded: next(),
            rep_layout_cnc_tails_preserved: next(),
        };
        let assigned = last.get();

        let mut json = String::new();
        push_sink_totals(&mut json, &totals);
        let mut printed: Vec<u64> = json
            .lines()
            .filter_map(|line| line.split_once("\": "))
            .map(|(_, value)| {
                value
                    .trim_end_matches(',')
                    .parse()
                    .unwrap_or_else(|_| panic!("non-numeric counter {value:?} in {json}"))
            })
            .collect();
        printed.sort_unstable();
        assert_eq!(
            printed,
            (1..=assigned).collect::<Vec<_>>(),
            "diag sink JSON must print each of the {assigned} counters once: {json}"
        );
    }
}
