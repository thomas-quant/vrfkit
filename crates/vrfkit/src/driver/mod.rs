//! `export` subcommand driver -- full pipeline from .vrf to Parquet.
//!
//! One wire-order pass: the frame walk applies a frame's ExportData, then
//! lends that exact cache state to its packets, so packet-side export
//! mutations precede the next packet and a later frame's schema cannot leak
//! backward. [`writers`] runs the large tables off the packet loop,
//! [`checkpoints`] is the optional snapshot pass, [`publish`] stages and
//! publishes the directory, and [`summary`] prints the stderr report whose
//! labels the Python harnesses parse.

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
    CheckpointExportGroupWriter, CheckpointFieldRecord, CheckpointFieldWriter,
    CheckpointGuidEntryWriter, CheckpointNetGuidWriter, EventRecord, EventWriter, FieldRecord,
    FieldWriter, MovementRecord, MovementWriter, NetGuidRecord, NetGuidWriter,
};
use vrf_frame::walk_demo_frames;
use vrf_schema::NetGuidCache;

use crate::error::{CliError, replication_reader};
use crate::manifest::{self, ManifestQuality};
use crate::sink::{ChannelState, ExportSink, RecordBuffers};
use checkpoints::{CheckpointStats, ReplayContext};
use publish::OutputTransaction;
pub(crate) use summary::RunTotals;
use writers::WriterThread;

/// The six tables every export writes. With [`CHECKPOINT_TABLES`] and
/// [`MANIFEST`], every name `run` creates: `publish` refuses a destination
/// holding anything else, so a table missing here makes the next export to
/// that directory refuse it.
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

    eprintln!("reading {vrf_path}...");
    let data = fs::read(vrf_path)?;
    let file_size = data.len();

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

    let destination = PathBuf::from(out_dir);
    let output = OutputTransaction::begin(&destination)?;
    let out_path = output.path();

    let create = |name: &str| -> Result<BufWriter<fs::File>, CliError> {
        Ok(BufWriter::new(fs::File::create(out_path.join(name))?))
    };

    let mut field_writer = FieldWriter::new(create("fields.parquet")?)?;
    let mut movement_writer = MovementWriter::new(create("movement.parquet")?)?;
    let mut actor_writer = ActorWriter::new(create("actors.parquet")?)?;
    // A couple of hundred rows, so inline like `actors`: far below a thread's
    // worth of encoding.
    let mut event_writer = EventWriter::new(create("events.parquet")?)?;
    let mut partial_writer = vrf_export::PartialWriter::new(create("partials.parquet")?)?;
    let mut checkpoint_writer = if with_checkpoints {
        let mut cp_fields = CheckpointFieldWriter::new(create("checkpoint_fields.parquet")?)?;
        Some(checkpoints::CheckpointWriters {
            // The one checkpoint table large enough to take off the decode
            // thread, for the reason `writers` gives for fields and movement.
            fields: WriterThread::<CheckpointFieldRecord>::spawn("checkpoint_fields", move |rx| {
                for batch in rx {
                    cp_fields.push_batch(batch)?;
                }
                cp_fields.finish()
            }),
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

    let mut cache = NetGuidCache::new();
    let mut repl_reader = replication_reader(ctx.branch)?;

    let mut chunk_iter = ChunkIterator::new(&data, preamble.remaining_offset);
    let mut channel_state = ChannelState::new();

    let mut buffers = RecordBuffers::default();
    let mut error_report = OverlayErrorReport::default();
    let mut totals = RunTotals::default();
    let mut cp_stats = CheckpointStats::default();

    while let Some(chunk) = chunk_iter.next_chunk()? {
        // In bounds for every kind: `next_chunk` refuses a chunk whose declared
        // size runs past the file.
        let payload = &data[chunk.data_offset..chunk.data_offset + chunk.size_in_bytes as usize];

        // The server's own labelled timeline: uncompressed and independent of
        // replication, so written straight out.
        if chunk.chunk_type == ChunkType::Event {
            let event = parse_event_chunk(payload)?;
            totals.event_trailing_bytes += event.trailing_bytes as u64;
            // Layout [u32 tag][N x u32 words][FString][f32] for groups whose
            // word count, tag and public name are established; the parse must
            // consume it exactly and its seconds must match Time1. A mismatch
            // is counted, never guessed at; `raw_payload` keeps every byte.
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

        // `_with_trailing`: the outer chunk can exceed the inner SizeInBytes,
        // which the plain call drops unseen. Counted, not rejected: no replay
        // has been measured carrying any, so failing would be a guess.
        let (decompressed, trailing) =
            decompress_replay_data_with_trailing(payload, ctx.compressed, ctx.encrypted)?;
        totals.replay_data_trailing_bytes += trailing as u64;

        // The callback cannot return a writer error through `FrameError`, so
        // the first one is parked and later callbacks become no-ops.
        let mut packet_error = None;
        let walk = walk_demo_frames(&decompressed, ctx.flags, &mut cache, |pkt, packet_cache| {
            if packet_error.is_some() {
                return;
            }
            let pkt_id = totals.total_packets;
            totals.total_packets += 1;

            // Scoped so the sink's borrow of `buffers` ends before the drain.
            {
                let mut sink = ExportSink::new(packet_cache, &mut channel_state, &mut buffers);
                sink.enable_measured_array_routes(ctx.branch);
                sink.time_ms = pkt.time_ms;
                sink.packet_id = pkt_id;

                repl_reader.process_packet(pkt.data, pkt_id as i32, &mut sink);

                // The sink dies with this scope: a counter not absorbed here
                // never existed. See `sink::totals`.
                totals.sink.absorb(&mut sink.stats, &mut error_report);
            }

            let result = (|| -> Result<(), CliError> {
                fields.append(&mut buffers.fields)?;
                totals.movement_rows += buffers.movement.len() as u64;
                movement.append(&mut buffers.movement)?;
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

    // Joined before the elapsed time is taken and any file size is read, so
    // both files are complete and both results are checked.
    fields.finish()?;
    movement.finish()?;
    // Drain fragments that never got their final piece into preservation
    // rows; a dropped accumulator would make a partial bunch lost at EOF look
    // like one still in flight.
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

    // After the pass, because a GUID's outer may be declared in a later chunk
    // than the one that first referenced it; sorted, because the cache is a
    // HashMap and the file must be byte-reproducible.
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

    let net_stats = repl_reader.stats();
    totals.export_groups = cache.group_count();
    totals.elapsed = start.elapsed();

    // Before the summary, so the path it prints names a file that exists.
    let staged_manifest_path = out_path.join(MANIFEST);
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

    // Before `publish`: see `summary::stale_checkpoint_note`.
    totals.stale_checkpoint_note = summary::stale_checkpoint_note(&destination, with_checkpoints);

    // Every table and the manifest are complete and closed. A run that failed
    // before here dropped the guard, removing staging only.
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
