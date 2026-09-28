//! `export` subcommand driver -- full pipeline from .vrf to Parquet.
//!
//! # Architecture
//!
//! DemoFrames and packets are processed in one wire-order pass. The frame
//! iterator applies one frame's ExportData, then lends that exact cache state
//! to the packet callback. Packet-side export mutations therefore precede the
//! next packet, while a later frame's schema cannot leak backward into an
//! earlier packet.
//!
//! # Layout
//!
//! - [`writers`] -- the two large tables' writers, running off the packet loop.
//! - [`checkpoints`] -- the optional full-state snapshot pass.
//! - [`summary`] -- the stderr report, whose every line a Python harness pins.

pub(crate) mod checkpoints;
mod publish;
mod summary;
mod writers;

use std::fs;
use std::io::BufWriter;
use std::path::PathBuf;
use std::time::Instant;

use vrf_container::{
    ChunkIterator, ChunkType, decompress_replay_data_with_trailing,
    event_payload_seconds_matches_time, known_event_word_count, parse_event_chunk,
    parse_known_event_payload, parse_preamble,
};
use vrf_decode::OverlayErrorReport;
use vrf_export::{
    ActorWriter, CheckpointActorWriter, CheckpointBlockWriter, CheckpointExportFieldWriter,
    CheckpointExportGroupWriter, CheckpointFieldWriter, CheckpointGuidEntryWriter,
    CheckpointNetGuidWriter, EventRecord, EventWriter, FieldRecord, FieldWriter, MovementRecord,
    MovementWriter, NetGuidRecord, NetGuidWriter,
};
use vrf_frame::walk_demo_frames;
use vrf_net::pipeline::ReplicationReader;
use vrf_schema::NetGuidCache;

use crate::error::CliError;
use crate::manifest::{self, ManifestQuality};
use crate::sink::{ChannelState, ExportSink, RecordBuffers};
use checkpoints::{CheckpointStats, ReplayContext};
use publish::OutputTransaction;
pub(crate) use summary::RunTotals;
use writers::WriterThread;

/// The six tables every export writes. With [`CHECKPOINT_TABLES`] and
/// [`MANIFEST`], every name `run` creates: `publish` refuses a destination
/// holding anything else, so a table missing here makes the next export to
/// the same directory refuse it.
const MAIN_TABLES: [&str; 6] = [
    "fields.parquet",
    "movement.parquet",
    "actors.parquet",
    "net_guids.parquet",
    "events.parquet",
    "partials.parquet",
];
/// The tables written only when `--checkpoints` asks for them.
const CHECKPOINT_TABLES: [&str; 7] = [
    "checkpoint_fields.parquet",
    "checkpoint_actors.parquet",
    "checkpoint_net_guids.parquet",
    "checkpoint_blocks.parquet",
    "checkpoint_guid_entries.parquet",
    "checkpoint_export_groups.parquet",
    "checkpoint_export_fields.parquet",
];
/// Written after every table is complete.
const MANIFEST: &str = "manifest.json";

pub fn run(vrf_path: &str, out_dir: &str, with_checkpoints: bool) -> Result<(), CliError> {
    let start = Instant::now();

    // -- Read file ---------------------------------------------------------
    eprintln!("reading {vrf_path}...");
    let data = fs::read(vrf_path)?;
    let file_size = data.len();

    // -- Parse preamble ----------------------------------------------------
    let preamble = parse_preamble(&data)?;
    let ctx = ReplayContext {
        branch: &preamble.header.replay_version.branch,
        flags: preamble.header.flags,
        compressed: preamble.info.compressed,
        encrypted: preamble.info.encrypted,
    };

    eprintln!(
        "branch: {}, flags: 0x{:04X}, compressed: {}, duration: {} ms",
        ctx.branch, ctx.flags, ctx.compressed, preamble.info.length_in_ms
    );

    // -- Setup output ------------------------------------------------------
    let destination = PathBuf::from(out_dir);
    let output = OutputTransaction::begin(&destination)?;
    let out_path = output.path();

    let create = |name: &str| -> Result<BufWriter<fs::File>, CliError> {
        Ok(BufWriter::new(fs::File::create(out_path.join(name))?))
    };

    let mut field_writer = FieldWriter::new(create("fields.parquet")?)?;
    let mut movement_writer = MovementWriter::new(create("movement.parquet")?)?;
    let mut actor_writer = ActorWriter::new(create("actors.parquet")?)?;
    // Event chunks are a couple of hundred rows and are written inline for the
    // same reason `actors` is: the encoding cost is far below a thread's worth.
    let mut event_writer = EventWriter::new(create("events.parquet")?)?;
    let mut partial_writer = vrf_export::PartialWriter::new(create("partials.parquet")?)?;
    let mut checkpoint_writer = if with_checkpoints {
        Some(checkpoints::CheckpointWriters {
            fields: CheckpointFieldWriter::new(create("checkpoint_fields.parquet")?)?,
            actors: CheckpointActorWriter::new(create("checkpoint_actors.parquet")?)?,
            net_guids: CheckpointNetGuidWriter::new(create("checkpoint_net_guids.parquet")?)?,
            blocks: CheckpointBlockWriter::new(create("checkpoint_blocks.parquet")?)?,
            guid_entries: CheckpointGuidEntryWriter::new(create(
                "checkpoint_guid_entries.parquet",
            )?)?,
            export_groups: CheckpointExportGroupWriter::new(create(
                "checkpoint_export_groups.parquet",
            )?)?,
            export_fields: CheckpointExportFieldWriter::new(create(
                "checkpoint_export_fields.parquet",
            )?)?,
        })
    } else {
        None
    };

    let mut fields = WriterThread::<FieldRecord>::spawn("fields", move |rx| {
        for batch in rx {
            field_writer.push_batch(batch)?;
        }
        field_writer.finish()
    });
    let mut movement = WriterThread::<MovementRecord>::spawn("movement", move |rx| {
        for batch in rx {
            movement_writer.push_batch(batch)?;
        }
        movement_writer.finish()
    });

    // -- Setup replication reader and schema cache --------------------------
    let mut cache = NetGuidCache::new();
    let mut repl_reader = ReplicationReader::new(ctx.branch)
        .map_err(|e| CliError::Usage(format!("unsupported branch: {e}")))?;

    // -- Iterate chunks ----------------------------------------------------
    let mut chunk_iter = ChunkIterator::new(&data, preamble.remaining_offset);
    let mut channel_state = ChannelState::new();

    // Reusable per-packet record buffers; see `RecordBuffers`.
    let mut buffers = RecordBuffers::default();
    let mut error_report = OverlayErrorReport::default();
    // Every run counter the manifest and the summary report, sink-derived
    // ones included (see `sink::totals`), in one place.
    let mut totals = RunTotals::default();
    let mut cp_stats = CheckpointStats::default();

    while let Some(chunk) = chunk_iter.next_chunk()? {
        // Sliced once for all three chunk kinds. Safe for the kinds this loop
        // ignores too: `next_chunk` refuses a chunk whose declared size runs
        // past the file, so the range is in bounds before it is returned.
        let payload = &data[chunk.data_offset..chunk.data_offset + chunk.size_in_bytes as usize];

        // Event chunks carry the server's own labelled timeline. They are
        // uncompressed and independent of the replication pass, so they are
        // read here and written straight out.
        if chunk.chunk_type == ChunkType::Event {
            let event = parse_event_chunk(payload)?;
            totals.event_trailing_bytes += event.trailing_bytes as u64;
            // Structural payload fields for groups whose word count, tag and
            // public enum-name FString are established. Payload layout:
            // [u32 tag][N x u32 words][FString][f32]. The guarded parser also
            // requires exact consumption; the final filter checks the inner
            // seconds against Time1. A disagreement yields no overlay fields
            // and is counted, never guessed at. `raw_payload` still keeps
            // every byte either way.
            let word_count = known_event_word_count(&event.group);
            let parsed_payload = match word_count {
                Some(count) => {
                    let parsed =
                        parse_known_event_payload(&event.group, event.payload).filter(|payload| {
                            event_payload_seconds_matches_time(event.time1, payload.seconds)
                        });
                    if parsed.is_some() {
                        totals.event_payloads_decoded += 1;
                    } else {
                        totals.event_layout_mismatches += 1;
                        totals.event_first_layout_mismatch.get_or_insert_with(|| {
                            format!(
                                "{} declared {count} word(s), public tag/name and millisecond time but its {}-byte payload does not fit that layout",
                                event.group,
                                event.payload.len()
                            )
                        });
                    }
                    parsed
                }
                None => {
                    totals.event_payload_unknown_groups += 1;
                    None
                }
            };
            let (word0, word1, payload_tag, payload_name, payload_seconds) = match parsed_payload {
                Some(parsed) => (
                    parsed.words.first().copied(),
                    parsed.words.get(1).copied(),
                    Some(parsed.tag),
                    Some(parsed.name),
                    Some(parsed.seconds),
                ),
                None => (None, None, None, None, None),
            };
            event_writer.push(EventRecord {
                id: event.id,
                group: event.group,
                metadata: event.metadata,
                time1: event.time1,
                time2: event.time2,
                payload_size: event.size_in_bytes,
                raw_payload: event.payload.to_vec(),
                word0,
                word1,
                payload_tag,
                payload_name,
                payload_seconds,
            })?;
            totals.event_rows += 1;
            continue;
        }
        if chunk.chunk_type == ChunkType::Checkpoint {
            if let Some(writer) = checkpoint_writer.as_mut() {
                checkpoints::process_chunk(
                    payload,
                    &ctx,
                    writer,
                    &mut cp_stats,
                    &mut error_report,
                    &mut partial_writer,
                )?;
            }
            continue;
        }
        if chunk.chunk_type != ChunkType::ReplayData {
            continue;
        }

        // `_with_trailing` rather than the plain call: the outer chunk can be
        // larger than the inner SizeInBytes, and the plain signature drops that
        // excess with nothing to show for it. Counted, not rejected -- no replay
        // has ever been measured carrying any, so failing on it would be
        // guessing at a format we have not seen.
        let (decompressed, trailing) =
            decompress_replay_data_with_trailing(payload, ctx.compressed, ctx.encrypted)?;
        totals.replay_data_trailing_bytes += trailing as u64;

        // Process each packet before the iterator advances to later ExportData.
        // The callback cannot return a writer error through `FrameError`, so it
        // records the first one and makes later callbacks no-ops until the
        // frame walk finishes and the error can be returned here.
        let mut packet_error = None;
        let walk = walk_demo_frames(&decompressed, ctx.flags, &mut cache, |pkt, packet_cache| {
            if packet_error.is_some() {
                return;
            }
            let pkt_id = totals.total_packets;
            totals.total_packets += 1;

            // Scoped so the sink's borrow of `buffers` ends before they are
            // drained. The buffers outlive the sink; that is the point.
            {
                let mut sink = ExportSink::new(packet_cache, &mut channel_state, &mut buffers);
                sink.enable_measured_array_routes(ctx.branch);
                sink.time_ms = pkt.time_ms;
                sink.packet_id = pkt_id;

                repl_reader.process_packet(pkt.data, pkt_id as i32, &mut sink);

                // The sink is dropped at the end of this scope, so a counter
                // not read here is a counter that never existed. All of them
                // go through one function; see `sink::totals`.
                totals.sink.absorb(&mut sink.stats, &mut error_report);
            }

            // Hand field and movement records to their writer threads.
            let result = (|| -> Result<(), CliError> {
                fields.append(&mut buffers.fields)?;
                totals.movement_rows += buffers.movement.len() as u64;
                movement.append(&mut buffers.movement)?;
                // Drain actor lifecycle records to the inline writer.
                for record in buffers.actors.drain(..) {
                    actor_writer.push(record)?;
                }
                for mut record in buffers.partials.drain(..) {
                    totals.partial_rows += 1;
                    totals.partial_bits += record.bit_count;
                    record.source = "main";
                    partial_writer.push(record)?;
                }
                Ok(())
            })();
            if let Err(error) = result {
                packet_error = Some(error);
            }
        })?;
        if let Some(error) = packet_error {
            return Err(error);
        }

        totals.frames += walk.frames;
        totals.frame_skips.absorb(walk.skipped);
        totals.chunks_processed += 1;

        if totals.chunks_processed % 100 == 0 {
            eprintln!(
                "  chunk {}: {} packets, {} groups",
                totals.chunks_processed,
                totals.total_packets,
                cache.group_count()
            );
        }
    }

    // -- Finish writers ----------------------------------------------------
    //
    // The two offloaded writers are joined here, before the elapsed time is
    // taken and before any file size is read, so both files are complete and
    // both results are checked.
    fields.finish()?;
    movement.finish()?;
    // EOF can turn still-active reassemblies into preservation rows.
    {
        let mut sink = ExportSink::new(&mut cache, &mut channel_state, &mut buffers);
        sink.enable_measured_array_routes(ctx.branch);
        repl_reader.finish_with_sink(&mut sink);
    }
    for mut record in buffers.partials.drain(..) {
        totals.partial_rows += 1;
        totals.partial_bits += record.bit_count;
        record.source = "main";
        partial_writer.push(record)?;
    }
    actor_writer.finish()?;
    event_writer.finish()?;
    partial_writer.finish()?;
    if let Some(w) = checkpoint_writer.take() {
        w.finish()?;
    }

    // -- Write the NetGUID registry ----------------------------------------
    //
    // Written after the replication pass because the cache accumulates over the
    // whole replay: a GUID's outer may be declared in a later chunk than the
    // one that first referenced it. Sorted so the file is byte-reproducible
    // across runs (the cache is HashMap-backed and iterates in arbitrary order).
    let mut net_guid_writer = NetGuidWriter::new(create("net_guids.parquet")?)?;
    let mut guid_entries = cache.net_guid_entries();
    guid_entries.sort_unstable_by_key(|e| e.net_guid);
    totals.net_guid_rows = guid_entries.len();
    for entry in guid_entries {
        net_guid_writer.push(NetGuidRecord {
            net_guid: entry.net_guid,
            path: entry.path.to_owned(),
            outer_net_guid: entry.outer_net_guid,
        })?;
    }
    net_guid_writer.finish()?;

    // Drain fragments that never got their final piece. Without this the
    // accumulator is simply dropped and a partial bunch lost at EOF is
    // indistinguishable from one still legitimately in flight -- the counters
    // it feeds only exist if someone asks for them.
    let net_stats = repl_reader.stats();
    totals.export_groups = cache.group_count();
    totals.elapsed = start.elapsed();

    // -- Write manifest ----------------------------------------------------
    //
    // Before the summary so the path the summary prints names a file that
    // exists by the time it is read.
    let staged_manifest_path = out_path.join(MANIFEST);
    // Drain per-PlayerState identity (Subject + SpawnedCharacter) captured
    // during the walk into a sorted players list for the manifest.
    let mut players: Vec<(u32, Option<String>, Option<u32>)> = channel_state
        .players()
        .iter()
        .filter(|(_, id)| id.subject.is_some())
        .map(|(&g, id)| (g, id.subject.clone(), id.character_net_guid))
        .collect();
    players.sort_unstable_by_key(|(g, _, _)| *g);
    manifest::write_manifest(
        &staged_manifest_path,
        vrf_path,
        file_size,
        &preamble,
        &cache,
        &players,
        &ManifestQuality {
            run: &totals,
            net: net_stats,
            error_report: &error_report,
            checkpoints: with_checkpoints.then_some(&cp_stats),
        },
    )?;

    // Checked against the directory `publish` is about to replace, not the
    // one that exists once it returns: publication is a single atomic
    // rename of the whole destination, so a table this run did not rewrite
    // can only still be found here, before that swap happens. See
    // `summary::stale_checkpoint_note`'s own doc for why checking afterward
    // could never see it.
    totals.stale_checkpoint_note = summary::stale_checkpoint_note(&destination, with_checkpoints);

    // No handle remains open in staging at this point. Replace the destination
    // only after every table and the manifest are complete; a failed run before
    // here drops the guard and removes staging without touching the prior run.
    output.publish()?;
    let manifest_path = destination.join(MANIFEST);

    summary::print(
        &destination,
        net_stats,
        &totals,
        &error_report,
        with_checkpoints.then_some(&cp_stats),
        &manifest_path,
    );

    Ok(())
}
