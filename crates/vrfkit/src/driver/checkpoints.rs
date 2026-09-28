//! The optional Checkpoint pass.
//!
//! A checkpoint is a full-state snapshot (its own GUID cache, export map and a
//! DemoFrame re-opening every live actor), so each gets a fresh cache, reader,
//! channel state and buffers: sharing any of them would leak snapshot opens
//! and archetype mappings into the ReplayData pass. Its rows go to separate
//! tables because packet, channel and NetGUID namespaces restart in each
//! snapshot, which also leaves the main tables byte-identical either way.

use std::io::Write;

use vrf_container::{decompress_checkpoint, parse_checkpoint_chunk};
use vrf_decode::OverlayErrorReport;
use vrf_export::{
    CheckpointActorRecord, CheckpointActorWriter, CheckpointBlockWriter,
    CheckpointExportFieldRecord, CheckpointExportFieldWriter, CheckpointExportGroupRecord,
    CheckpointExportGroupWriter, CheckpointFieldRecord, CheckpointGuidEntryRecord,
    CheckpointGuidEntryWriter, CheckpointIdentity, CheckpointNetGuidRecord,
    CheckpointNetGuidWriter, NetGuidRecord, PartialWriter,
};
use vrf_frame::{FrameSkips, walk_demo_frames};
use vrf_net::stats::NetStats;
use vrf_schema::{
    CheckpointReadError, CheckpointTableSink, NetGuidCache, read_checkpoint_tables_with_sink,
};

use super::writers::WriterThread;
use crate::error::{CliError, replication_reader};
use crate::sink::{ChannelState, ExportSink, RecordBuffers, SinkTotals};

/// Counters for the optional checkpoint pass. Kept together so the summary
/// cannot report one and quietly omit another.
#[derive(Debug, Default)]
pub(crate) struct CheckpointStats {
    pub chunks: u64,
    /// Sum of [`CheckpointChunk::trailing_bytes`](vrf_container::CheckpointChunk::trailing_bytes):
    /// 0 on every corpus checkpoint measured, counted so bytes a format change
    /// leaves after the archive are not dropped unseen.
    pub trailing_bytes: u64,
    pub guid_entries: u64,
    pub literal_paths: u64,
    pub indexed_paths: u64,
    pub resolved_path_indices: u64,
    pub group_records: u64,
    pub exported_fields: u64,
    /// DemoFrames walked, as `walk_demo_frames` counted them -- not assumed to
    /// be one per chunk.
    pub frames: u64,
    /// Section bytes the snapshot frames stepped over, as in the main pass.
    pub frame_skips: FrameSkips,
    /// Snapshot frames with a NaN or infinite time, as in the main pass.
    pub non_finite_frame_times: u64,
    pub packets: u64,
    pub field_rows: u64,
    pub actor_rows_written: u64,
    pub net_guid_rows_written: u64,
    pub block_rows_written: u64,
    pub guid_entry_rows_written: u64,
    pub export_group_rows_written: u64,
    pub export_field_rows_written: u64,
    pub partial_rows: u64,
    pub partial_bits: u64,
    /// Actor rows that never reached `checkpoint_actors.parquet`: the sink
    /// pushes one per open and one per close, so this is each chunk's opens
    /// and closes less the rows it wrote, measured rather than assumed.
    pub actor_rows_dropped: u64,
    pub movement_rows_dropped: u64,
    /// Everything the checkpoint sinks counted, through the same
    /// [`SinkTotals`] as the main pass but apart from its baseline-pinned
    /// totals, which mixing would move by a flag-dependent amount.
    pub sink: SinkTotals,
    /// Replication/framing counters from every finalized checkpoint reader.
    pub net: NetStats,
}

impl CheckpointStats {
    /// Fold in one finished chunk's reader counters, and count as dropped
    /// every actor row its opens and closes called for beyond the
    /// `actor_rows` it wrote.
    fn absorb_chunk_net(&mut self, chunk_net: &mut NetStats, actor_rows: u64) {
        self.actor_rows_dropped +=
            (chunk_net.actor_opens + chunk_net.actor_closes).saturating_sub(actor_rows);
        self.net.absorb(chunk_net);
    }
}

pub(super) struct CheckpointWriters<W: Write + Send> {
    pub fields: WriterThread<CheckpointFieldRecord>,
    pub actors: CheckpointActorWriter<W>,
    pub net_guids: CheckpointNetGuidWriter<W>,
    pub blocks: CheckpointBlockWriter<W>,
    pub guid_entries: CheckpointGuidEntryWriter<W>,
    pub export_groups: CheckpointExportGroupWriter<W>,
    pub export_fields: CheckpointExportFieldWriter<W>,
}

impl<W: Write + Send> CheckpointWriters<W> {
    pub fn finish(self) -> Result<(), CliError> {
        self.fields.finish()?;
        self.actors.finish()?;
        self.net_guids.finish()?;
        self.blocks.finish()?;
        self.guid_entries.finish()?;
        self.export_groups.finish()?;
        self.export_fields.finish()?;
        Ok(())
    }
}

/// Everything about the replay that the checkpoint pass needs and cannot
/// rediscover from the chunk alone.
pub(super) struct ReplayContext<'a> {
    pub branch: &'a str,
    pub flags: u32,
    pub compressed: bool,
    pub encrypted: bool,
}

struct DeclarationWriter<'a, W: Write + Send> {
    checkpoint: CheckpointIdentity,
    guid_entries: &'a mut CheckpointGuidEntryWriter<W>,
    export_groups: &'a mut CheckpointExportGroupWriter<W>,
    export_fields: &'a mut CheckpointExportFieldWriter<W>,
    guid_rows: u64,
    group_rows: u64,
    field_rows: u64,
}

impl<W: Write + Send> CheckpointTableSink for DeclarationWriter<'_, W> {
    type Error = vrf_export::ExportError;

    fn on_guid_entry(
        &mut self,
        ordinal: u32,
        guid: u32,
        outer: u32,
        path_is_string: bool,
        literal_path: Option<&str>,
        name_index: Option<u32>,
        flags: u8,
    ) -> Result<(), Self::Error> {
        self.guid_entries.push(CheckpointGuidEntryRecord {
            checkpoint: self.checkpoint.clone(),
            ordinal,
            net_guid: guid,
            outer_net_guid: outer,
            path_is_string,
            literal_path: literal_path.map(Into::into),
            name_index,
            flags,
        })?;
        self.guid_rows += 1;
        Ok(())
    }

    fn on_export_group(
        &mut self,
        ordinal: u32,
        path_name_index: u32,
        group_path: &str,
        declared_slots: u32,
    ) -> Result<(), Self::Error> {
        self.export_groups.push(CheckpointExportGroupRecord {
            checkpoint: self.checkpoint.clone(),
            ordinal,
            path_name_index,
            group_path: group_path.into(),
            declared_slots,
        })?;
        self.group_rows += 1;
        Ok(())
    }

    fn on_export_field(
        &mut self,
        group_ordinal: u32,
        path_name_index: u32,
        slot: u32,
        handle: u32,
        checksum: u32,
        rendered_name: &str,
        exported_flag: u8,
        fname_kind: u8,
        base: Option<&str>,
        index: Option<u32>,
        number: Option<i32>,
    ) -> Result<(), Self::Error> {
        self.export_fields.push(CheckpointExportFieldRecord {
            checkpoint: self.checkpoint.clone(),
            group_ordinal,
            path_name_index,
            slot,
            handle,
            compatible_checksum: checksum,
            rendered_name: rendered_name.into(),
            exported_flag,
            fname_kind,
            fname_base: base.map(Into::into),
            fname_index: index,
            fname_number: number,
        })?;
        self.field_rows += 1;
        Ok(())
    }
}

/// Decode one Checkpoint chunk and write its rows. `error_report` is the
/// shared one: the summary's breakdown is the only place a checkpoint-only
/// decode error surfaces.
pub(super) fn process_chunk<W: Write + Send, P: Write + Send>(
    payload: &[u8],
    ctx: &ReplayContext<'_>,
    writers: &mut CheckpointWriters<W>,
    stats: &mut CheckpointStats,
    error_report: &mut OverlayErrorReport,
    partial_writer: &mut PartialWriter<P>,
) -> Result<(), CliError> {
    let cp = parse_checkpoint_chunk(payload)?;
    stats.trailing_bytes += cp.trailing_bytes as u64;
    let plain = decompress_checkpoint(cp.archive, ctx.compressed, ctx.encrypted)?;

    let actor_rows_before = stats.actor_rows_written;
    let checkpoint_index = u32::try_from(stats.chunks)
        .map_err(|_| CliError::Usage("too many checkpoint chunks to index".to_owned()))?;
    let checkpoint = CheckpointIdentity {
        checkpoint_index,
        checkpoint_id: cp.id.clone().into(),
    };
    let mut cache = NetGuidCache::new();
    let mut declarations = DeclarationWriter {
        checkpoint: checkpoint.clone(),
        guid_entries: &mut writers.guid_entries,
        export_groups: &mut writers.export_groups,
        export_fields: &mut writers.export_fields,
        guid_rows: 0,
        group_rows: 0,
        field_rows: 0,
    };
    let tables = read_checkpoint_tables_with_sink(&plain, &mut cache, &mut declarations).map_err(
        |error| match error {
            CheckpointReadError::Schema(error) => {
                CliError::Usage(format!("checkpoint {}: {error}", cp.id))
            }
            CheckpointReadError::Bit(error) => CliError::Usage(format!(
                "checkpoint {}: {}",
                cp.id,
                vrf_schema::SchemaError::Bitio(error)
            )),
            CheckpointReadError::Sink(error) => CliError::Export(error),
        },
    )?;
    if declarations.guid_rows != u64::from(tables.guid_count)
        || declarations.group_rows != u64::from(tables.group_count)
        || declarations.field_rows != u64::from(tables.exported_fields)
    {
        return Err(CliError::Usage(format!(
            "checkpoint {} declaration row counts disagree with parsed tables",
            cp.id
        )));
    }
    stats.guid_entry_rows_written += declarations.guid_rows;
    stats.export_group_rows_written += declarations.group_rows;
    stats.export_field_rows_written += declarations.field_rows;

    let frame = &plain[tables.frame_offset..];
    let mut reader = replication_reader(ctx.branch)?;
    let mut channels = ChannelState::new();
    let mut buffers = RecordBuffers::default();
    let mut packet_count = 0u64;
    let mut block_count = 0u32;
    let mut field_records = Vec::new();
    let mut packet_error = None;
    let walk = walk_demo_frames(frame, ctx.flags, &mut cache, |pkt, packet_cache| {
        if packet_error.is_some() {
            return;
        }
        {
            let mut sink = ExportSink::new(packet_cache, &mut channels, &mut buffers);
            sink.enable_measured_array_routes(ctx.branch);
            sink.enable_checkpoint_block_context(checkpoint.clone(), stats.field_rows, block_count);
            sink.time_ms = pkt.time_ms;
            sink.packet_id = packet_count as u32;
            reader.process_packet(pkt.data, packet_count as i32, &mut sink);
            // The ReplayData pass's aggregation, so both read the same counters.
            stats.sink.absorb(&mut sink.stats, error_report);
        }
        let result = (|| -> Result<(), CliError> {
            let packet_blocks = buffers.checkpoint_blocks.len() as u32;
            stats.block_rows_written += u64::from(packet_blocks);
            writers
                .blocks
                .push_batch(buffers.checkpoint_blocks.drain(..))?;
            block_count += packet_blocks;
            stats.field_rows += buffers.fields.len() as u64;
            field_records.extend(buffers.fields.drain(..).map(|field| CheckpointFieldRecord {
                checkpoint: checkpoint.clone(),
                field,
            }));
            writers.fields.append(&mut field_records)?;
            stats.actor_rows_written += buffers.actors.len() as u64;
            writers
                .actors
                .push_batch(buffers.actors.drain(..).map(|actor| CheckpointActorRecord {
                    checkpoint: checkpoint.clone(),
                    actor,
                }))?;
            stats.movement_rows_dropped += buffers.movement.len() as u64;
            buffers.movement.clear();
            for mut record in buffers.partials.drain(..) {
                stats.partial_rows += 1;
                stats.partial_bits += record.bit_count;
                record.source = "checkpoint";
                record.checkpoint_id = Some(cp.id.clone());
                partial_writer.push(record)?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            packet_error = Some(error);
        }
        packet_count += 1;
    })?;
    if let Some(error) = packet_error {
        return Err(error);
    }
    {
        let mut sink = ExportSink::new(&mut cache, &mut channels, &mut buffers);
        sink.enable_measured_array_routes(ctx.branch);
        sink.enable_checkpoint_block_context(checkpoint.clone(), stats.field_rows, block_count);
        reader.finish_with_sink(&mut sink);
    }
    for mut record in buffers.partials.drain(..) {
        stats.partial_rows += 1;
        stats.partial_bits += record.bit_count;
        record.source = "checkpoint";
        record.checkpoint_id = Some(cp.id.clone());
        partial_writer.push(record)?;
    }
    let mut chunk_net = reader.stats().clone();
    let chunk_actor_rows = stats.actor_rows_written - actor_rows_before;
    stats.absorb_chunk_net(&mut chunk_net, chunk_actor_rows);

    let mut guid_entries = cache.net_guid_entries();
    guid_entries.sort_unstable_by_key(|entry| entry.net_guid);
    stats.net_guid_rows_written += guid_entries.len() as u64;
    writers
        .net_guids
        .push_batch(
            guid_entries
                .into_iter()
                .map(|entry| CheckpointNetGuidRecord {
                    checkpoint: checkpoint.clone(),
                    net_guid: NetGuidRecord {
                        net_guid: entry.net_guid,
                        path: entry.path.to_owned(),
                        outer_net_guid: entry.outer_net_guid,
                    },
                }),
        )?;

    stats.chunks += 1;
    stats.guid_entries += u64::from(tables.guid_count);
    stats.literal_paths += u64::from(tables.literal_paths);
    stats.indexed_paths += u64::from(tables.hardcoded_paths);
    stats.resolved_path_indices += u64::from(tables.resolved_path_indices);
    stats.group_records += u64::from(tables.group_count);
    stats.exported_fields += u64::from(tables.exported_fields);
    stats.frames += u64::from(walk.frames);
    stats.frame_skips.absorb(walk.skipped);
    stats.non_finite_frame_times += u64::from(walk.non_finite_times);
    stats.packets += packet_count;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::{self, Write};

    use super::*;

    struct FailAfterHeader {
        remaining: usize,
    }

    impl Write for FailAfterHeader {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.remaining == 0 {
                return Err(io::Error::other("intentional writer failure"));
            }
            let written = bytes.len().min(self.remaining);
            self.remaining -= written;
            Ok(written)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// Per chunk, so one chunk's surplus cannot cancel another's loss.
    #[test]
    fn actor_rows_a_chunk_opened_or_closed_but_did_not_write_are_dropped() {
        let mut stats = CheckpointStats::default();
        let mut clean = NetStats {
            actor_opens: 3,
            actor_closes: 1,
            ..NetStats::default()
        };
        stats.absorb_chunk_net(&mut clean, 4);
        assert_eq!(stats.actor_rows_dropped, 0);

        let mut lossy = NetStats {
            actor_opens: 2,
            ..NetStats::default()
        };
        stats.absorb_chunk_net(&mut lossy, 1);
        assert_eq!(stats.actor_rows_dropped, 1, "one open wrote no row");

        let mut surplus = NetStats {
            actor_opens: 1,
            ..NetStats::default()
        };
        stats.absorb_chunk_net(&mut surplus, 2);
        assert_eq!(stats.actor_rows_dropped, 1, "a surplus is not a refund");
        assert_eq!((stats.net.actor_opens, stats.net.actor_closes), (6, 1));
    }

    #[test]
    fn declaration_callback_rows_propagate_the_underlying_writer_failure() {
        let writer = |remaining| FailAfterHeader { remaining };
        let mut guid_entries =
            CheckpointGuidEntryWriter::with_row_group_size(writer(4), 1).unwrap();
        let mut export_groups =
            CheckpointExportGroupWriter::with_row_group_size(writer(usize::MAX), 1).unwrap();
        let mut export_fields =
            CheckpointExportFieldWriter::with_row_group_size(writer(usize::MAX), 1).unwrap();
        let mut declarations = DeclarationWriter {
            checkpoint: CheckpointIdentity {
                checkpoint_index: 0,
                checkpoint_id: "cp".into(),
            },
            guid_entries: &mut guid_entries,
            export_groups: &mut export_groups,
            export_fields: &mut export_fields,
            guid_rows: 0,
            group_rows: 0,
            field_rows: 0,
        };

        declarations
            .on_guid_entry(0, 1, 0, true, Some("path"), None, 0)
            .unwrap();
        assert_eq!(declarations.guid_rows, 1);
        drop(declarations);
        let error = guid_entries
            .finish()
            .expect_err("finalizing the callback row must return the Parquet sink failure");
        assert!(error.to_string().contains("intentional writer failure"));
    }
}
