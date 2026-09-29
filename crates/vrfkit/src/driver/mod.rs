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
    event_payload_seconds_matches_time, known_event_word_count, parse_event_chunk,
    parse_known_event_payload, parse_preamble,
};
use vrf_decode::OverlayErrorReport;
use vrf_export::{
    ActorWriter, CheckpointActorWriter, CheckpointBlockWriter, CheckpointExportFieldWriter,
    CheckpointExportGroupWriter, CheckpointFieldRecord, CheckpointFieldWriter,
    CheckpointGuidEntryWriter, CheckpointNetGuidWriter, EventRecord, EventWriter, FieldRecord,
    FieldWriter, MovementRecord, MovementWriter, NetGuidRecord, NetGuidWriter, PartialRecord,
};
use vrf_schema::NetGuidCache;

use crate::error::CliError;
use crate::manifest::{self, ManifestQuality};
use crate::pass::{Chunk, Pass, Replay, for_each_chunk};
use checkpoints::CheckpointStats;
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
    let replay = Replay::new(&preamble);
    eprintln!(
        "branch: {}, flags: 0x{:04X}, compressed: {}, duration: {} ms",
        replay.branch, replay.flags, replay.compressed, preamble.info.length_in_ms
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

    let mut pass = Pass::new(&replay)?;
    let mut error_report = OverlayErrorReport::default();
    let mut totals = RunTotals::default();
    let mut cp_stats = CheckpointStats::default();

    for_each_chunk(&data, &replay, |chunk| {
        match chunk {
            // The server's own labelled timeline: uncompressed and independent
            // of replication, so written straight out.
            Chunk::Event(payload) => {
                let event = parse_event_chunk(payload)?;
                totals.event_trailing_bytes += event.trailing_bytes as u64;
                // Layout [u32 tag][N x u32 words][FString][f32] for groups whose
                // word count, tag and public name are established; the parse
                // must consume it exactly and its seconds must match Time1. A
                // mismatch is counted, never guessed at; `raw_payload` keeps
                // every byte.
                let word_count = known_event_word_count(&event.group);
                let parsed_payload = match word_count {
                    Some(count) => {
                        let parsed = parse_known_event_payload(&event.group, event.payload).filter(
                            |payload| {
                                event_payload_seconds_matches_time(event.time1, payload.seconds)
                            },
                        );
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
                let (word0, word1, payload_tag, payload_name, payload_seconds) =
                    match parsed_payload {
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
            }
            Chunk::Checkpoint(payload) => {
                if let Some(writers) = checkpoint_writer.as_mut() {
                    let mut partials = Vec::new();
                    checkpoints::process_chunk(
                        payload,
                        &replay,
                        writers,
                        &mut cp_stats,
                        &mut error_report,
                        &mut partials,
                    )?;
                    partial_writer.push_batch(partials)?;
                }
            }
            Chunk::ReplayData(frames, unread) => {
                totals.replay_data_trailing_bytes += unread as u64;
                pass.walk(&frames, &mut totals.sink, &mut error_report, |buffers| {
                    fields.append(&mut buffers.fields)?;
                    totals.movement_rows += buffers.movement.len() as u64;
                    movement.append(&mut buffers.movement)?;
                    actor_writer.push_batch(buffers.actors.drain(..))?;
                    partial_writer.push_batch(label_partials(
                        &mut buffers.partials,
                        "main",
                        None,
                        &mut totals.partial_rows,
                        &mut totals.partial_bits,
                    ))?;
                    Ok(())
                })?;
                totals.chunks_processed += 1;
            }
            Chunk::Other => {}
        }
        Ok(())
    })?;

    // Joined before the elapsed time is taken and any file size is read, so
    // both files are complete and both results are checked.
    fields.finish()?;
    movement.finish()?;
    pass.finish();
    partial_writer.push_batch(label_partials(
        &mut pass.buffers.partials,
        "main",
        None,
        &mut totals.partial_rows,
        &mut totals.partial_bits,
    ))?;
    actor_writer.finish()?;
    event_writer.finish()?;
    partial_writer.finish()?;
    if let Some(w) = checkpoint_writer.take() {
        w.finish()?;
    }

    // After the pass: a GUID's outer may be declared in a later chunk than the
    // one that first referenced it.
    let net_guids = net_guid_rows(&pass.cache);
    totals.net_guid_rows = net_guids.len();
    let mut net_guid_writer = NetGuidWriter::new(create("net_guids.parquet")?)?;
    net_guid_writer.push_batch(net_guids)?;
    net_guid_writer.finish()?;

    let net_stats = pass.reader.stats();
    totals.export_groups = pass.cache.group_count();
    totals.total_packets = pass.packets;
    totals.frames = pass.frames;
    totals.frame_skips = pass.frame_skips;
    totals.non_finite_frame_times = pass.non_finite_frame_times;
    totals.elapsed = start.elapsed();

    // Before the summary, so the path it prints names a file that exists.
    let staged_manifest_path = out_path.join(MANIFEST);
    let mut players: Vec<(u32, Option<String>, Option<u32>)> = pass
        .channels
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
        &pass.cache,
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

/// `records` stamped with the pass that produced them, counted into `rows`
/// and `bits`.
fn label_partials<'r>(
    records: &'r mut Vec<PartialRecord>,
    source: &'static str,
    checkpoint_id: Option<&'r str>,
    rows: &'r mut u64,
    bits: &'r mut u64,
) -> impl Iterator<Item = PartialRecord> + 'r {
    records.drain(..).map(move |mut record| {
        *rows += 1;
        *bits += record.bit_count;
        record.source = source;
        record.checkpoint_id = checkpoint_id.map(str::to_owned);
        record
    })
}

/// The cache's GUID table as rows, sorted: the cache is a HashMap and the file
/// must be byte-reproducible.
fn net_guid_rows(cache: &NetGuidCache) -> Vec<NetGuidRecord> {
    let mut entries = cache.net_guid_entries();
    entries.sort_unstable_by_key(|entry| entry.net_guid);
    entries
        .into_iter()
        .map(|entry| NetGuidRecord {
            net_guid: entry.net_guid,
            path: entry.path.to_owned(),
            outer_net_guid: entry.outer_net_guid,
        })
        .collect()
}
