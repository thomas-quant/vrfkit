//! Hand-rolled JSON for manifest.json: no serde, and one String, which is fine
//! because the export groups run to a few hundred entries.
//!
//! The header's game-specific data entries are JSON documents themselves (on
//! 02d4d478 entry 1 is a 219,304-character match roster). They are emitted as
//! JSON strings, never parsed and re-serialised, which could silently alter
//! number formatting, key order or duplicate keys; a consumer recovers the
//! object with one nested parse.

use std::fs;
use std::io::Write;
use std::path::Path;

use vrf_container::Preamble;
use vrf_decode::OverlayErrorReport;
use vrf_frame::FrameSkips;
use vrf_net::stats::NetStats;
use vrf_schema::NetGuidCache;

use crate::driver::RunTotals;
use crate::driver::checkpoints::CheckpointStats;
use crate::error::CliError;
use crate::sink::SinkTotals;

/// Every run-level value needed to judge whether the published tables are
/// complete and how much typed decoding fell back to preserved raw data.
pub(crate) struct ManifestQuality<'a> {
    pub run: &'a RunTotals,
    pub net: &'a NetStats,
    pub error_report: &'a OverlayErrorReport,
    pub checkpoints: Option<&'a CheckpointStats>,
}

pub fn write_manifest(
    path: &Path,
    source_file: &str,
    file_size: usize,
    preamble: &Preamble,
    cache: &NetGuidCache,
    players: &[(u32, Option<String>, Option<u32>)],
    quality: &ManifestQuality<'_>,
) -> Result<(), CliError> {
    let stats = quality.net;
    let header = &preamble.header;
    let ver = &header.replay_version;
    let info = &preamble.info;

    // The game-specific data blob dominates the document size; reserve for it
    // up front rather than growing a quarter-megabyte String by doubling.
    let gsd_bytes: usize = header.game_specific_data.iter().map(String::len).sum();
    let mut out = String::with_capacity(64 * 1024 + gsd_bytes * 2);
    out.push_str("{\n");

    // The first six keys are read by tools/to_valplay_bundle.py; keep their
    // names and types stable.
    wkvs(
        &mut out,
        &[
            ("source_file", json_str(source_file)),
            ("source_size_bytes", file_size.to_string()),
            ("replay_build", json_str(&ver.branch)),
            (
                "replay_version",
                json_str(&format!("{}.{}.{}", ver.major, ver.minor, ver.patch)),
            ),
            ("replay_changelist", ver.changelist.to_string()),
            ("duration_ms", info.length_in_ms.to_string()),
            ("elapsed_ms", quality.run.elapsed.as_millis().to_string()),
            ("friendly_name", json_str(&info.friendly_name)),
            ("is_live", json_bool(info.is_live).to_owned()),
            ("compressed", json_bool(info.compressed).to_owned()),
            ("encrypted", json_bool(info.encrypted).to_owned()),
            // Unreal FDateTime ticks (100 ns since 0001-01-01, not FILETIME's
            // 1601: read as FILETIME the reference replay dates to 3626).
            // Raw, because the wire records no timezone.
            ("timestamp_ticks", info.timestamp.to_string()),
            // The info section's copy, which nothing validates (480767974 on
            // 02d4d478). The header's validated network_version (19 there) is
            // not emitted; `game_network_protocol_version` is another field.
            ("info_network_version", info.network_version.to_string()),
            ("network_checksum", header.network_checksum.to_string()),
            (
                "game_network_protocol_version",
                header.game_network_protocol_version.to_string(),
            ),
            // Four u32 words in serialisation order; a canonical GUID string
            // would impose a byte order nothing here can confirm.
            (
                "guid",
                format!(
                    "[{}, {}, {}, {}]",
                    header.guid[0], header.guid[1], header.guid[2], header.guid[3]
                ),
            ),
            ("ue4_version", header.ue4_version.to_string()),
            ("ue5_version", header.ue5_version.to_string()),
            (
                "package_version_license",
                header.package_version_license.to_string(),
            ),
            ("flags", header.flags.to_string()),
            ("platform", json_str(&header.platform)),
            ("build_config", header.build_config.to_string()),
            ("build_target_type", header.build_target_type.to_string()),
            // Expected zero; see `ReplayHeader::trailing_bytes`.
            ("header_trailing_bytes", header.trailing_bytes.to_string()),
            ("min_record_hz", json_f32(header.min_record_hz)),
            ("max_record_hz", json_f32(header.max_record_hz)),
            ("frame_limit_in_ms", json_f32(header.frame_limit_in_ms)),
            (
                "checkpoint_limit_in_ms",
                json_f32(header.checkpoint_limit_in_ms),
            ),
        ],
        1,
    );

    let levels: Vec<String> = header
        .level_names_and_times
        .iter()
        .map(|(name, time)| format!("{{ \"name\": {}, \"time_ms\": {time} }}", json_str(name)))
        .collect();
    wkv_array(&mut out, "level_names_and_times", &levels, 1);

    // After the scalars, so the readable metadata precedes the large blob.
    let gsd: Vec<String> = header
        .game_specific_data
        .iter()
        .map(|s| json_str(s))
        .collect();
    wkv_array(&mut out, "game_specific_data", &gsd, 1);

    out.push_str("  \"stats\": {\n");
    wkvs(
        &mut out,
        &[
            ("packet_count", quality.run.total_packets.to_string()),
            ("bunch_count", stats.bunches.to_string()),
            (
                "malformed_packet_count",
                stats.malformed_packets.to_string(),
            ),
            ("partial_error_count", stats.partial_errors.to_string()),
            ("partial_fragments", stats.partial_fragments.to_string()),
        ],
        2,
    );
    wkvl(
        &mut out,
        "partial_completed",
        &stats.partial_completed.to_string(),
        2,
    );
    out.push_str("  },\n");

    out.push_str("  \"counts\": {\n");
    wkvs(
        &mut out,
        &[
            ("content_blocks", stats.content_blocks.to_string()),
            ("rep_layout_blocks", stats.rep_layout_blocks.to_string()),
            (
                "class_net_cache_blocks",
                stats.class_net_cache_blocks.to_string(),
            ),
            ("deleted_blocks", stats.deleted_blocks.to_string()),
            ("fields", stats.fields.to_string()),
            ("rpcs", stats.rpcs.to_string()),
            ("actor_opens", stats.actor_opens.to_string()),
            ("actor_closes", stats.actor_closes.to_string()),
            ("exported_guids", stats.exported_guids.to_string()),
            ("skipped_bits", stats.skipped_bits.to_string()),
        ],
        2,
    );
    wkvl(
        &mut out,
        "malformed_content_blocks",
        &stats.malformed_content_blocks.to_string(),
        2,
    );
    out.push_str("  },\n");

    // Every loss and fallback counter, checkpoint pass included; `stats` and
    // `counts` above stay unchanged for the readers they already have.
    out.push_str(&quality_json(quality));
    out.push_str(",\n");

    // Each BombPlayerState actor's account `subject` and `SpawnedCharacter`
    // (== movement.character_net_guid): the join from actor-keyed tables to
    // playerLoadouts identities, even when two players share an agent.
    out.push_str("  \"players\": [");
    if players.is_empty() {
        out.push_str("],\n");
    } else {
        out.push('\n');
        for (i, (guid, subject, character)) in players.iter().enumerate() {
            out.push_str("    { ");
            out.push_str(&format!(
                "\"actor_net_guid\": {guid}, \"subject\": {}, \"character_net_guid\": {}",
                json_opt(subject, |s| json_str(s)),
                json_opt(character, u32::to_string),
            ));
            out.push_str(" }");
            if i + 1 < players.len() {
                out.push(',');
            }
            out.push('\n');
        }
        out.push_str("  ],\n");
    }

    // Net-field exports the cache could not place: an out-of-range handle (an
    // unknown group cannot reach this call site). The C# reference drops these
    // silently; expected zero.
    wkv(
        &mut out,
        "dropped_field_exports",
        &cache.dropped_field_exports().to_string(),
        1,
    );

    out.push_str("  \"net_field_export_groups\": [\n");
    let groups = cache.groups();
    for (gi, group) in groups.iter().enumerate() {
        out.push_str("    {\n");
        wkv(&mut out, "path", &json_str(&group.path), 3);
        wkv(
            &mut out,
            "path_name_index",
            &group.path_name_index.to_string(),
            3,
        );
        out.push_str("      \"fields\": [");
        let populated: Vec<_> = group.populated_fields().collect();
        if populated.is_empty() {
            out.push(']');
        } else {
            out.push('\n');
            for (fi, field) in populated.iter().enumerate() {
                out.push_str("        { ");
                out.push_str(&format!(
                    "\"handle\": {}, \"name\": {}, \"compatible_checksum\": {}",
                    field.handle,
                    json_str(&field.name),
                    field.compatible_checksum
                ));
                out.push_str(" }");
                if fi + 1 < populated.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            out.push_str("      ]");
        }
        out.push('\n');
        out.push_str("    }");
        if gi + 1 < groups.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str("  ]\n");
    out.push_str("}\n");

    let mut file = fs::File::create(path)?;
    file.write_all(out.as_bytes())?;
    Ok(())
}

fn quality_json(quality: &ManifestQuality<'_>) -> String {
    let mut out = String::with_capacity(8 * 1024);
    out.push_str("  \"quality\": {\n");
    // `content_blocks_lost` first: the verdict on whether anything is missing
    // from the tables, from the function `validate` prints, zero included.
    // Everything after it is evidence.
    let q = quality;
    let run = q.run;
    wkvs(
        &mut out,
        &[
            (
                "content_blocks_lost",
                q.net.lost_content_blocks().to_string(),
            ),
            ("chunks_processed", run.chunks_processed.to_string()),
            ("export_groups", run.export_groups.to_string()),
            ("movement_rows", run.movement_rows.to_string()),
            ("net_guid_rows", run.net_guid_rows.to_string()),
            ("event_rows", run.event_rows.to_string()),
            ("partial_rows", run.partial_rows.to_string()),
            ("partial_bits", run.partial_bits.to_string()),
            ("event_trailing_bytes", run.event_trailing_bytes.to_string()),
            (
                "replay_data_trailing_bytes",
                run.replay_data_trailing_bytes.to_string(),
            ),
        ],
        2,
    );
    write_frame_skips(&mut out, "frame_", &run.frame_skips, 2);
    wkv(
        &mut out,
        "frame_non_finite_times",
        &run.non_finite_frame_times.to_string(),
        2,
    );
    wkvs(
        &mut out,
        &[
            (
                "event_layout_mismatches",
                run.event_layout_mismatches.to_string(),
            ),
            (
                "event_first_layout_mismatch",
                json_option(run.event_first_layout_mismatch.as_deref()),
            ),
            (
                "event_payloads_decoded",
                run.event_payloads_decoded.to_string(),
            ),
            (
                "event_payload_unknown_groups",
                run.event_payload_unknown_groups.to_string(),
            ),
            (
                "overlay_error_buckets",
                q.error_report.bucket_count().to_string(),
            ),
            (
                "overlay_errors_reported",
                q.error_report.total_errors().to_string(),
            ),
            (
                "checkpoints_enabled",
                json_bool(q.checkpoints.is_some()).to_owned(),
            ),
        ],
        2,
    );
    write_net_quality(&mut out, "net", quality.net, 2, true);
    write_sink_quality(&mut out, "sink", &run.sink, 2, true);

    match quality.checkpoints {
        Some(checkpoints) => {
            out.push_str("    \"checkpoints\": {\n");
            let cp = checkpoints;
            wkvs(
                &mut out,
                &[
                    (
                        "checkpoint_path_resolution_mode",
                        json_str("preceding_literal_zero_based"),
                    ),
                    ("checkpoint_literal_paths", cp.literal_paths.to_string()),
                    ("checkpoint_indexed_paths", cp.indexed_paths.to_string()),
                    (
                        "checkpoint_resolved_path_indices",
                        cp.resolved_path_indices.to_string(),
                    ),
                    ("checkpoint_chunks", cp.chunks.to_string()),
                    // The summary's `Trailing bytes:`; its ReplayData twin is
                    // `replay_data_trailing_bytes` above.
                    ("checkpoint_trailing_bytes", cp.trailing_bytes.to_string()),
                    ("checkpoint_guid_entries", cp.guid_entries.to_string()),
                    ("checkpoint_group_records", cp.group_records.to_string()),
                    ("checkpoint_exported_fields", cp.exported_fields.to_string()),
                    ("checkpoint_frames", cp.frames.to_string()),
                ],
                3,
            );
            write_frame_skips(&mut out, "checkpoint_frame_", &cp.frame_skips, 3);
            wkv(
                &mut out,
                "checkpoint_frame_non_finite_times",
                &cp.non_finite_frame_times.to_string(),
                3,
            );
            wkvs(
                &mut out,
                &[
                    ("checkpoint_packets", cp.packets.to_string()),
                    ("checkpoint_field_rows", cp.field_rows.to_string()),
                    (
                        "checkpoint_actor_rows_written",
                        cp.actor_rows_written.to_string(),
                    ),
                    (
                        "checkpoint_net_guid_rows_written",
                        cp.net_guid_rows_written.to_string(),
                    ),
                    (
                        "checkpoint_block_rows_written",
                        cp.block_rows_written.to_string(),
                    ),
                    (
                        "checkpoint_guid_entry_rows_written",
                        cp.guid_entry_rows_written.to_string(),
                    ),
                    (
                        "checkpoint_export_group_rows_written",
                        cp.export_group_rows_written.to_string(),
                    ),
                    (
                        "checkpoint_export_field_rows_written",
                        cp.export_field_rows_written.to_string(),
                    ),
                    ("checkpoint_partial_rows", cp.partial_rows.to_string()),
                    ("checkpoint_partial_bits", cp.partial_bits.to_string()),
                    (
                        "checkpoint_actor_rows_dropped",
                        cp.actor_rows_dropped.to_string(),
                    ),
                    (
                        "checkpoint_movement_rows_dropped",
                        cp.movement_rows_dropped.to_string(),
                    ),
                ],
                3,
            );
            write_net_quality(&mut out, "net", &checkpoints.net, 3, true);
            write_sink_quality(&mut out, "sink", &checkpoints.sink, 3, false);
            out.push_str("    }\n");
        }
        None => wkvl(&mut out, "checkpoints", "null", 2),
    }
    out.push_str("  }");
    out
}

/// The three [`FrameSkips`] tallies as `<prefix>external_data_blobs`,
/// `<prefix>external_data_bytes` and `<prefix>game_specific_bytes`.
fn write_frame_skips(out: &mut String, prefix: &str, skips: &FrameSkips, indent: usize) {
    for (key, value) in [
        ("external_data_blobs", skips.external_data_blobs),
        ("external_data_bytes", skips.external_data_bytes),
        ("game_specific_bytes", skips.game_specific_bytes),
    ] {
        wkv(out, &format!("{prefix}{key}"), &value.to_string(), indent);
    }
}

fn write_net_quality(
    out: &mut String,
    key: &str,
    stats: &NetStats,
    indent: usize,
    trailing_comma: bool,
) {
    push_indent(out, indent);
    out.push_str(&format!("\"{key}\": {{\n"));
    let inner = indent + 1;
    for (key, value) in [
        ("packets", stats.packets),
        ("malformed_packets", stats.malformed_packets),
        ("bunches", stats.bunches),
        ("partial_errors", stats.partial_errors),
        ("partial_fragments", stats.partial_fragments),
        ("partial_completed", stats.partial_completed),
        ("unfinished_partials", stats.unfinished_partials),
        ("unfinished_partial_bits", stats.unfinished_partial_bits),
        ("bunch_header_failures", stats.bunch_header_failures),
        ("content_blocks", stats.content_blocks),
        ("rep_layout_blocks", stats.rep_layout_blocks),
        ("class_net_cache_blocks", stats.class_net_cache_blocks),
        ("deleted_blocks", stats.deleted_blocks),
        ("fields", stats.fields),
        ("rpcs", stats.rpcs),
        ("skipped_bits", stats.skipped_bits),
        (
            "content_block_framing_failures",
            stats.content_block_framing_failures,
        ),
        ("malformed_content_blocks", stats.malformed_content_blocks),
        ("transform_failures", stats.transform_failures),
        ("field_stream_failures", stats.field_stream_failures),
        ("rpc_stream_failures", stats.rpc_stream_failures),
        (
            "unresolved_rpc_payloads_preserved",
            stats.unresolved_rpc_payloads_preserved,
        ),
        ("actor_opens", stats.actor_opens),
        ("actor_closes", stats.actor_closes),
        (
            "channel_reopens_while_open",
            stats.channel_reopens_while_open,
        ),
        ("actor_opens_missing_spawn", stats.actor_opens_missing_spawn),
        ("failed_reopens_while_open", stats.failed_reopens_while_open),
        (
            "bunches_on_unopened_channel",
            stats.bunches_on_unopened_channel,
        ),
        ("unopened_channel_bits", stats.unopened_channel_bits),
        (
            "channel_state_limit_failures",
            stats.channel_state_limit_failures,
        ),
        (
            "partial_resource_limit_failures",
            stats.partial_resource_limit_failures,
        ),
        ("package_map_exports", stats.package_map_exports),
        ("rep_layout_export_bunches", stats.rep_layout_export_bunches),
        ("exported_guids", stats.exported_guids),
        ("must_be_mapped_guids", stats.must_be_mapped_guids),
        ("diagnostics_retained", stats.diagnostics.len() as u64),
    ] {
        wkv(out, key, &value.to_string(), inner);
    }
    wkvl(
        out,
        "diagnostics_dropped",
        &stats.diagnostics_dropped.to_string(),
        inner,
    );
    close_object(out, indent, trailing_comma);
}

fn write_sink_quality(
    out: &mut String,
    key: &str,
    sink: &SinkTotals,
    indent: usize,
    trailing_comma: bool,
) {
    push_indent(out, indent);
    out.push_str(&format!("\"{key}\": {{\n"));
    let inner = indent + 1;
    for (key, value) in [
        // Each must equal the `net` block's `rpcs` / `actor_opens` /
        // `actor_closes` / `content_blocks` (tools/verify_build_corpus.py);
        // prefixed because `net` beside it uses those names for another source.
        ("sink_rpcs_emitted", sink.rpcs_emitted),
        ("sink_actor_opens", sink.actor_opens),
        ("sink_actor_closes", sink.actor_closes),
        ("sink_content_blocks", sink.content_blocks),
        ("overlay_decoded_ok", sink.overlay.decoded_ok),
        ("overlay_decoded_err", sink.overlay.decoded_err),
        ("overlay_raw_or_skip", sink.overlay.raw_or_skip),
        ("overlay_not_in_table", sink.overlay.not_in_table),
        ("overlay_no_field_name", sink.overlay.no_field_name),
        (
            "overlay_handle_conflicts_refused",
            sink.overlay.handle_conflicts_refused,
        ),
        ("effect_blobs_decoded", sink.effect_blobs_decoded),
        ("struct_blobs_decoded", sink.struct_blobs_decoded),
        ("struct_blobs_failed", sink.struct_blobs_failed),
        (
            "multi_contents_items_emitted",
            sink.multi_contents_items_emitted,
        ),
        ("movement_rpc_errors", sink.movement_rpc_errors),
        (
            "movement_sized_section_tails",
            sink.movement_sized_section_tails,
        ),
        (
            "movement_sized_section_tail_bits",
            sink.movement_sized_section_tail_bits,
        ),
        (
            "movement_open_section_tails",
            sink.movement_open_section_tails,
        ),
        (
            "movement_open_section_tail_bits",
            sink.movement_open_section_tail_bits,
        ),
        (
            "movement_envelope_trailers",
            sink.movement_envelope_trailers,
        ),
        (
            "movement_envelope_trailer_bits",
            sink.movement_envelope_trailer_bits,
        ),
        ("array_elements_decoded", sink.array.elements_decoded),
        ("array_fields_emitted", sink.array.fields_emitted),
        ("array_truncations", sink.array.truncations),
        ("array_errors", sink.array.errors),
        (
            "array_unconsumed_nested_bits",
            sink.array.unconsumed_nested_bits,
        ),
        (
            "array_unconsumed_root_bits",
            sink.array.unconsumed_root_bits,
        ),
        (
            "array_implicit_terminations",
            sink.array.implicit_terminations,
        ),
        ("array_leaf_decode_errors", sink.array_leaf_decode_errors),
        (
            "targeting_world_locations_decoded",
            sink.targeting_world_locations_decoded,
        ),
        (
            "tracked_rewards_opaque_empty_variants",
            sink.tracked_rewards_opaque_empty_variants,
        ),
        (
            "active_blinds_empty_trailers",
            sink.active_blinds_empty_trailers,
        ),
        ("truncated_rpcs", sink.truncated_rpcs),
        ("rpc_suffix_bits_dropped", sink.rpc_suffix_bits_dropped),
        ("cnc_rpcs_emitted", sink.cnc_rpcs_emitted),
        (
            "cnc_bruteforce_payloads_attempted",
            sink.cnc_bruteforce_payloads_attempted,
        ),
        (
            "cnc_bruteforce_payloads_unwalked",
            sink.cnc_bruteforce_payloads_unwalked,
        ),
        (
            "rep_layout_cnc_tails_decoded",
            sink.rep_layout_cnc_tails_decoded,
        ),
        (
            "rep_layout_cnc_tails_preserved",
            sink.rep_layout_cnc_tails_preserved,
        ),
    ] {
        wkv(out, key, &value.to_string(), inner);
    }
    wkv(
        out,
        "struct_blob_first_error",
        &json_option(sink.struct_blob_first_error.as_deref()),
        inner,
    );
    wkvl(
        out,
        "movement_first_error",
        &json_option(sink.movement_first_error.as_deref()),
        inner,
    );
    close_object(out, indent, trailing_comma);
}

fn close_object(out: &mut String, indent: usize, trailing_comma: bool) {
    push_indent(out, indent);
    out.push('}');
    if trailing_comma {
        out.push(',');
    }
    out.push('\n');
}

fn json_option(value: Option<&str>) -> String {
    value.map(json_str).unwrap_or_else(|| "null".to_string())
}

fn push_indent(out: &mut String, indent: usize) {
    for _ in 0..indent {
        out.push_str("  ");
    }
}

fn wkv(out: &mut String, key: &str, value: &str, indent: usize) {
    push_indent(out, indent);
    out.push_str(&format!("\"{key}\": {value},\n"));
}

fn wkvs(out: &mut String, members: &[(&str, String)], indent: usize) {
    for (key, value) in members {
        wkv(out, key, value, indent);
    }
}

/// [`wkv`] without the trailing comma, for the last member of an object.
fn wkvl(out: &mut String, key: &str, value: &str, indent: usize) {
    push_indent(out, indent);
    out.push_str(&format!("\"{key}\": {value}\n"));
}

/// An array of already-rendered JSON values, one per line. Always followed by
/// a comma, so it must not end its object; add a comma-less variant before
/// moving an array there, rather than dropping the comma by hand.
fn wkv_array(out: &mut String, key: &str, values: &[String], indent: usize) {
    push_indent(out, indent);
    out.push_str(&format!("\"{key}\": ["));
    if values.is_empty() {
        out.push_str("],\n");
        return;
    }
    out.push('\n');
    for (i, value) in values.iter().enumerate() {
        push_indent(out, indent + 1);
        out.push_str(value);
        if i + 1 < values.len() {
            out.push(',');
        }
        out.push('\n');
    }
    push_indent(out, indent);
    out.push_str("],\n");
}

fn json_bool(b: bool) -> &'static str {
    if b { "true" } else { "false" }
}

/// `present(v)` for `Some(v)`, JSON `null` for `None`.
fn json_opt<T>(value: &Option<T>, present: impl FnOnce(&T) -> String) -> String {
    match value {
        Some(v) => present(v),
        None => String::from("null"),
    }
}

/// JSON number for an `f32`, or `null` when it is not finite: Rust prints
/// `NaN`, `inf` and `-inf`, which JSON does not admit, and the recording-rate
/// floats come straight off the wire.
fn json_f32(value: f32) -> String {
    if value.is_finite() {
        format!("{value}")
    } else {
        "null".to_string()
    }
}

/// JSON-escape a string: only what RFC 8259 requires (`"`, `\`, U+0000..U+001F);
/// everything else passes through into the UTF-8 manifest. That holds for the
/// UTF-16 game-specific blob too: `BitReader::read_fstring` decodes with the
/// strict `String::from_utf16`, so no unpaired surrogate reaches this function.
fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c < '\x20' => out.push_str(&format!("\\u{:04x}", c as u32)),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver::checkpoints::CheckpointStats;
    use crate::sink::SinkTotals;
    use vrf_decode::OverlayErrorReport;
    use vrf_net::stats::NetStats;

    #[test]
    fn quality_json_names_every_load_bearing_counter() {
        // Every NetStats scalar and bounded-diagnostic size, and every
        // SinkTotals/OverlayStats/ArrayDecodeStats counter: once at the top
        // level and once in `checkpoints`.
        const NET_AND_SINK_KEYS: &[&str] = &[
            "packets",
            "malformed_packets",
            "bunches",
            "partial_errors",
            "partial_fragments",
            "partial_completed",
            "unfinished_partials",
            "unfinished_partial_bits",
            "bunch_header_failures",
            "content_blocks",
            "rep_layout_blocks",
            "class_net_cache_blocks",
            "deleted_blocks",
            "fields",
            "rpcs",
            "skipped_bits",
            "content_block_framing_failures",
            "malformed_content_blocks",
            "transform_failures",
            "field_stream_failures",
            "rpc_stream_failures",
            "unresolved_rpc_payloads_preserved",
            "actor_opens",
            "actor_closes",
            "channel_reopens_while_open",
            "actor_opens_missing_spawn",
            "failed_reopens_while_open",
            "bunches_on_unopened_channel",
            "unopened_channel_bits",
            "channel_state_limit_failures",
            "partial_resource_limit_failures",
            "package_map_exports",
            "rep_layout_export_bunches",
            "exported_guids",
            "must_be_mapped_guids",
            "diagnostics_retained",
            "diagnostics_dropped",
            "sink_rpcs_emitted",
            "sink_actor_opens",
            "sink_actor_closes",
            "sink_content_blocks",
            "overlay_decoded_ok",
            "overlay_decoded_err",
            "overlay_raw_or_skip",
            "overlay_not_in_table",
            "overlay_no_field_name",
            "overlay_handle_conflicts_refused",
            "effect_blobs_decoded",
            "struct_blobs_decoded",
            "struct_blobs_failed",
            "struct_blob_first_error",
            "multi_contents_items_emitted",
            "movement_rpc_errors",
            "movement_first_error",
            "movement_sized_section_tails",
            "movement_sized_section_tail_bits",
            "movement_open_section_tails",
            "movement_open_section_tail_bits",
            "movement_envelope_trailers",
            "movement_envelope_trailer_bits",
            "array_elements_decoded",
            "array_fields_emitted",
            "array_truncations",
            "array_errors",
            "array_unconsumed_nested_bits",
            "array_unconsumed_root_bits",
            "array_implicit_terminations",
            "array_leaf_decode_errors",
            "tracked_rewards_opaque_empty_variants",
            "active_blinds_empty_trailers",
            "truncated_rpcs",
            "rpc_suffix_bits_dropped",
            "cnc_rpcs_emitted",
            "cnc_bruteforce_payloads_attempted",
            "cnc_bruteforce_payloads_unwalked",
            "rep_layout_cnc_tails_decoded",
            "rep_layout_cnc_tails_preserved",
        ];
        // The overlay error report, run-level completeness and
        // checkpoint-only accounting: once each.
        const RUN_KEYS: &[&str] = &[
            "overlay_error_buckets",
            "overlay_errors_reported",
            "content_blocks_lost",
            "chunks_processed",
            "export_groups",
            "movement_rows",
            "net_guid_rows",
            "event_rows",
            "event_trailing_bytes",
            "replay_data_trailing_bytes",
            "frame_external_data_blobs",
            "frame_external_data_bytes",
            "frame_game_specific_bytes",
            "frame_non_finite_times",
            "event_layout_mismatches",
            "event_first_layout_mismatch",
            "event_payloads_decoded",
            "event_payload_unknown_groups",
            "checkpoint_chunks",
            "checkpoint_trailing_bytes",
            "checkpoint_path_resolution_mode",
            "checkpoint_literal_paths",
            "checkpoint_indexed_paths",
            "checkpoint_resolved_path_indices",
            "checkpoint_guid_entries",
            "checkpoint_group_records",
            "checkpoint_exported_fields",
            "checkpoint_frames",
            "checkpoint_frame_external_data_blobs",
            "checkpoint_frame_external_data_bytes",
            "checkpoint_frame_game_specific_bytes",
            "checkpoint_frame_non_finite_times",
            "checkpoint_packets",
            "checkpoint_field_rows",
            "checkpoint_actor_rows_written",
            "checkpoint_net_guid_rows_written",
            "checkpoint_block_rows_written",
            "checkpoint_guid_entry_rows_written",
            "checkpoint_export_group_rows_written",
            "checkpoint_export_field_rows_written",
            "checkpoint_actor_rows_dropped",
            "checkpoint_movement_rows_dropped",
        ];
        let net = NetStats::default();
        let checkpoints = CheckpointStats::default();
        let errors = OverlayErrorReport::default();
        let json = quality_json(&ManifestQuality {
            run: &RunTotals::default(),
            net: &net,
            error_report: &errors,
            checkpoints: Some(&checkpoints),
        });

        for (keys, expected) in [(NET_AND_SINK_KEYS, 2), (RUN_KEYS, 1)] {
            for key in keys {
                let needle = format!("\"{key}\":");
                assert_eq!(
                    json.matches(&needle).count(),
                    expected,
                    "quality manifest omitted or duplicated {key}: {json}"
                );
            }
        }
    }

    #[test]
    fn tail_and_guid_path_counters_publish_measured_values() {
        let net = NetStats::default();
        let sink = SinkTotals {
            rep_layout_cnc_tails_decoded: 2,
            rep_layout_cnc_tails_preserved: 3,
            ..SinkTotals::default()
        };
        let mut checkpoints = CheckpointStats::default();
        checkpoints.sink.rep_layout_cnc_tails_decoded = 5;
        checkpoints.sink.rep_layout_cnc_tails_preserved = 7;
        checkpoints.literal_paths = 17;
        checkpoints.indexed_paths = 11;
        checkpoints.resolved_path_indices = 11;
        let errors = OverlayErrorReport::default();
        let json = quality_json(&ManifestQuality {
            run: &RunTotals {
                sink,
                ..RunTotals::default()
            },
            net: &net,
            error_report: &errors,
            checkpoints: Some(&checkpoints),
        });

        for expected in [
            "\"rep_layout_cnc_tails_decoded\": 2",
            "\"rep_layout_cnc_tails_preserved\": 3",
            "\"rep_layout_cnc_tails_decoded\": 5",
            "\"rep_layout_cnc_tails_preserved\": 7",
            "\"checkpoint_path_resolution_mode\": \"preceding_literal_zero_based\"",
            "\"checkpoint_literal_paths\": 17",
            "\"checkpoint_indexed_paths\": 11",
            "\"checkpoint_resolved_path_indices\": 11",
        ] {
            assert!(json.contains(expected), "missing {expected}: {json}");
        }
    }

    #[test]
    fn movement_section_tails_publish_measured_values() {
        let net = NetStats::default();
        let sink = SinkTotals {
            movement_sized_section_tails: 41,
            movement_sized_section_tail_bits: 42,
            movement_open_section_tails: 43,
            movement_open_section_tail_bits: 44,
            ..SinkTotals::default()
        };
        let mut checkpoints = CheckpointStats::default();
        checkpoints.sink.movement_sized_section_tails = 51;
        checkpoints.sink.movement_sized_section_tail_bits = 52;
        checkpoints.sink.movement_open_section_tails = 53;
        checkpoints.sink.movement_open_section_tail_bits = 54;
        let errors = OverlayErrorReport::default();
        let json = quality_json(&ManifestQuality {
            run: &RunTotals {
                sink,
                ..RunTotals::default()
            },
            net: &net,
            error_report: &errors,
            checkpoints: Some(&checkpoints),
        });
        for expected in [
            "\"movement_sized_section_tails\": 41",
            "\"movement_sized_section_tail_bits\": 42",
            "\"movement_open_section_tails\": 43",
            "\"movement_open_section_tail_bits\": 44",
            "\"movement_sized_section_tails\": 51",
            "\"movement_sized_section_tail_bits\": 52",
            "\"movement_open_section_tails\": 53",
            "\"movement_open_section_tail_bits\": 54",
        ] {
            assert!(json.contains(expected), "missing {expected}: {json}");
        }
    }

    #[test]
    fn movement_envelope_trailers_publish_measured_values() {
        let net = NetStats::default();
        let sink = SinkTotals {
            movement_envelope_trailers: 61,
            movement_envelope_trailer_bits: 62,
            ..SinkTotals::default()
        };
        let mut checkpoints = CheckpointStats::default();
        checkpoints.sink.movement_envelope_trailers = 71;
        checkpoints.sink.movement_envelope_trailer_bits = 72;
        let errors = OverlayErrorReport::default();
        let json = quality_json(&ManifestQuality {
            run: &RunTotals {
                sink,
                ..RunTotals::default()
            },
            net: &net,
            error_report: &errors,
            checkpoints: Some(&checkpoints),
        });
        for expected in [
            "\"movement_envelope_trailers\": 61",
            "\"movement_envelope_trailer_bits\": 62",
            "\"movement_envelope_trailers\": 71",
            "\"movement_envelope_trailer_bits\": 72",
        ] {
            assert!(json.contains(expected), "missing {expected}: {json}");
        }
    }

    #[test]
    fn active_blinds_empty_trailers_publish_measured_values() {
        let net = NetStats::default();
        let sink = SinkTotals {
            active_blinds_empty_trailers: 81,
            ..SinkTotals::default()
        };
        let mut checkpoints = CheckpointStats::default();
        checkpoints.sink.active_blinds_empty_trailers = 91;
        let errors = OverlayErrorReport::default();
        let json = quality_json(&ManifestQuality {
            run: &RunTotals {
                sink,
                ..RunTotals::default()
            },
            net: &net,
            error_report: &errors,
            checkpoints: Some(&checkpoints),
        });
        for expected in [
            "\"active_blinds_empty_trailers\": 81",
            "\"active_blinds_empty_trailers\": 91",
        ] {
            assert!(json.contains(expected), "missing {expected}: {json}");
        }
    }

    #[test]
    fn sink_event_tallies_publish_measured_values() {
        let net = NetStats::default();
        let sink = SinkTotals {
            rpcs_emitted: 21,
            actor_opens: 22,
            actor_closes: 23,
            content_blocks: 24,
            ..SinkTotals::default()
        };
        let mut checkpoints = CheckpointStats::default();
        checkpoints.sink.rpcs_emitted = 31;
        checkpoints.sink.actor_opens = 32;
        checkpoints.sink.actor_closes = 33;
        checkpoints.sink.content_blocks = 34;
        let errors = OverlayErrorReport::default();
        let json = quality_json(&ManifestQuality {
            run: &RunTotals {
                sink,
                ..RunTotals::default()
            },
            net: &net,
            error_report: &errors,
            checkpoints: Some(&checkpoints),
        });
        for expected in [
            "\"sink_rpcs_emitted\": 21",
            "\"sink_actor_opens\": 22",
            "\"sink_actor_closes\": 23",
            "\"sink_content_blocks\": 24",
            "\"sink_rpcs_emitted\": 31",
            "\"sink_actor_opens\": 32",
            "\"sink_actor_closes\": 33",
            "\"sink_content_blocks\": 34",
        ] {
            assert!(json.contains(expected), "missing {expected}: {json}");
        }
    }

    #[test]
    fn cnc_bruteforce_counters_publish_measured_values() {
        let net = NetStats::default();
        let sink = SinkTotals {
            cnc_bruteforce_payloads_attempted: 11,
            cnc_bruteforce_payloads_unwalked: 2,
            ..SinkTotals::default()
        };
        let mut checkpoints = CheckpointStats::default();
        checkpoints.sink.cnc_bruteforce_payloads_attempted = 13;
        checkpoints.sink.cnc_bruteforce_payloads_unwalked = 3;
        let errors = OverlayErrorReport::default();
        let json = quality_json(&ManifestQuality {
            run: &RunTotals {
                sink,
                ..RunTotals::default()
            },
            net: &net,
            error_report: &errors,
            checkpoints: Some(&checkpoints),
        });
        for expected in [
            "\"cnc_bruteforce_payloads_attempted\": 11",
            "\"cnc_bruteforce_payloads_unwalked\": 2",
            "\"cnc_bruteforce_payloads_attempted\": 13",
            "\"cnc_bruteforce_payloads_unwalked\": 3",
        ] {
            assert!(json.contains(expected), "missing {expected}: {json}");
        }
    }

    /// Six distinct numbers, so a key wired to the wrong field or pass shows.
    #[test]
    fn frame_skips_publish_measured_values_for_both_passes() {
        let net = NetStats::default();
        let errors = OverlayErrorReport::default();
        let mut main = FrameSkips::default();
        main.external_data_blobs = 2;
        main.external_data_bytes = 3;
        main.game_specific_bytes = 5;
        let mut checkpoints = CheckpointStats::default();
        checkpoints.frame_skips.external_data_blobs = 7;
        checkpoints.frame_skips.external_data_bytes = 11;
        checkpoints.frame_skips.game_specific_bytes = 13;
        let json = quality_json(&ManifestQuality {
            run: &RunTotals {
                frame_skips: main,
                ..RunTotals::default()
            },
            net: &net,
            error_report: &errors,
            checkpoints: Some(&checkpoints),
        });

        for expected in [
            "\"frame_external_data_blobs\": 2",
            "\"frame_external_data_bytes\": 3",
            "\"frame_game_specific_bytes\": 5",
            "\"checkpoint_frame_external_data_blobs\": 7",
            "\"checkpoint_frame_external_data_bytes\": 11",
            "\"checkpoint_frame_game_specific_bytes\": 13",
        ] {
            assert!(json.contains(expected), "missing {expected}: {json}");
        }
    }

    /// Distinct per pass, so a key wired to the other pass shows.
    #[test]
    fn non_finite_frame_times_publish_measured_values_for_both_passes() {
        let net = NetStats::default();
        let errors = OverlayErrorReport::default();
        let checkpoints = CheckpointStats {
            non_finite_frame_times: 19,
            ..CheckpointStats::default()
        };
        let json = quality_json(&ManifestQuality {
            run: &RunTotals {
                non_finite_frame_times: 17,
                ..RunTotals::default()
            },
            net: &net,
            error_report: &errors,
            checkpoints: Some(&checkpoints),
        });
        for expected in [
            "\"frame_non_finite_times\": 17",
            "\"checkpoint_frame_non_finite_times\": 19",
        ] {
            assert!(json.contains(expected), "missing {expected}: {json}");
        }
    }

    /// The number a consumer reads before any coverage claim, so it is checked
    /// against `NetStats::lost_content_blocks` with all four failure depths
    /// non-zero and unequal: a constant zero would read as nothing lost.
    #[test]
    fn content_blocks_lost_publishes_the_measured_number() {
        let net = NetStats {
            malformed_content_blocks: 1,
            transform_failures: 2,
            field_stream_failures: 4,
            rpc_stream_failures: 100,
            unresolved_rpc_payloads_preserved: 92,
            ..NetStats::default()
        };
        assert_eq!(net.lost_content_blocks(), 15);

        let errors = OverlayErrorReport::default();
        let json = quality_json(&ManifestQuality {
            run: &RunTotals::default(),
            net: &net,
            error_report: &errors,
            checkpoints: None,
        });
        assert!(
            json.contains("\"content_blocks_lost\": 15"),
            "quality did not publish the measured loss: {json}"
        );
    }

    #[test]
    fn json_str_escapes_only_what_rfc_8259_requires() {
        assert_eq!(json_str("plain"), "\"plain\"");
        assert_eq!(json_str("a\"b"), "\"a\\\"b\"");
        assert_eq!(json_str("a\\b"), "\"a\\\\b\"");
        assert_eq!(json_str("a\nb\rc\td"), "\"a\\nb\\rc\\td\"");
        // Control characters without a short form take the \u escape.
        assert_eq!(json_str("\u{0}\u{1}\u{1f}"), "\"\\u0000\\u0001\\u001f\"");
    }

    #[test]
    fn json_str_passes_non_ascii_through_verbatim() {
        // ASCII source (check_ascii.py), non-ASCII values. DEL and the C1
        // range sit above U+001F, so escaping them would be wrong.
        for s in [
            "\u{7f}",           // DEL
            "\u{80}\u{9f}",     // C1 controls
            "\u{e9}\u{440}",    // Latin-1 supplement, Cyrillic
            "\u{d55c}\u{ad6d}", // Hangul, as a Valorant player name may carry
            "\u{1f600}",        // astral plane (surrogate pair on the wire)
            "\u{2028}\u{2029}", // line/paragraph separator: legal JSON
            "\u{feff}",         // BOM as an interior character
        ] {
            assert_eq!(json_str(s), format!("\"{s}\""), "mangled {s:?}");
        }
    }

    #[test]
    fn json_f32_replaces_non_finite_with_null() {
        assert_eq!(json_f32(30.0), "30");
        assert_eq!(json_f32(-0.5), "-0.5");
        assert_eq!(json_f32(f32::NAN), "null");
        assert_eq!(json_f32(f32::INFINITY), "null");
        assert_eq!(json_f32(f32::NEG_INFINITY), "null");
    }

    #[test]
    fn wkv_array_renders_empty_and_populated_forms() {
        let mut out = String::new();
        wkv_array(&mut out, "empty", &[], 1);
        assert_eq!(out, "  \"empty\": [],\n");

        let mut out = String::new();
        let values = vec!["1".to_string(), "2".to_string()];
        wkv_array(&mut out, "pair", &values, 1);
        assert_eq!(out, "  \"pair\": [\n    1,\n    2\n  ],\n");
    }
}
