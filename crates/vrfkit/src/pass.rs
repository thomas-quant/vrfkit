//! The decode pass every subcommand runs: `validate`, `diag` and `export`, over
//! the ReplayData stream and over each Checkpoint snapshot.

use std::sync::mpsc::sync_channel;
use std::thread;

use vrf_container::{
    ChunkIterator, ChunkType, ContainerError, Preamble, decompress_replay_data_with_trailing,
};
use vrf_decode::OverlayErrorReport;
#[cfg(feature = "export")]
use vrf_export::CheckpointIdentity;
use vrf_frame::{FrameSkips, walk_demo_frames};
use vrf_net::pipeline::ReplicationReader;
use vrf_schema::NetGuidCache;

use crate::error::{CliError, replication_reader};
use crate::sink::{ChannelState, ExportSink, RecordBuffers, SinkTotals};

/// What every pass needs from the preamble.
pub(crate) struct Replay<'a> {
    pub branch: &'a str,
    pub flags: u32,
    pub compressed: bool,
    pub encrypted: bool,
    first_chunk: usize,
}

impl<'a> Replay<'a> {
    pub fn new(preamble: &'a Preamble) -> Self {
        Self {
            branch: &preamble.header.replay_version.branch,
            flags: preamble.header.flags,
            compressed: preamble.info.compressed,
            encrypted: preamble.info.encrypted,
            first_chunk: preamble.remaining_offset,
        }
    }

    /// Each chunk's type and payload in file order, ending after the first
    /// error. `next_chunk` refuses a size that runs past the file, so the
    /// slice is in bounds.
    pub fn chunks<'d>(
        &self,
        data: &'d [u8],
    ) -> impl Iterator<Item = Result<(ChunkType, &'d [u8]), ContainerError>> + 'd {
        let mut chunks = Some(ChunkIterator::new(data, self.first_chunk));
        std::iter::from_fn(move || {
            let next = chunks.as_mut()?.next_chunk();
            let Ok(Some(c)) = next else {
                chunks = None;
                return next.err().map(Err);
            };
            Some(Ok((
                c.chunk_type,
                &data[c.data_offset..][..c.size_in_bytes as usize],
            )))
        })
    }
}

/// One chunk as [`for_each_chunk`] hands it over.
pub(crate) enum Chunk<'d> {
    /// Decompressed frames, and the payload bytes no reader consumed
    /// (`decompress_replay_data_with_trailing`).
    ReplayData(Vec<u8>, usize),
    Checkpoint(&'d [u8]),
    #[cfg_attr(not(feature = "export"), allow(dead_code))]
    Event(&'d [u8]),
    Other,
}

/// Call `on_chunk` for every chunk in file order. A helper thread walks the
/// same chunk list and decompresses the next ReplayData chunk while
/// `on_chunk` handles the ones before it (Oodle is ~20% of `validate` and
/// order-free). Errors still reach the caller at their own chunk, and an early
/// return drops the receiver, which stops the helper.
pub(crate) fn for_each_chunk(
    data: &[u8],
    replay: &Replay<'_>,
    mut on_chunk: impl FnMut(Chunk<'_>) -> Result<(), CliError>,
) -> Result<(), CliError> {
    let (compressed, encrypted) = (replay.compressed, replay.encrypted);
    thread::scope(|scope| {
        // Rendezvous: the helper holds at most one decompressed chunk ahead.
        let (tx, rx) = sync_channel(0);
        let chunks = replay.chunks(data).map_while(Result::ok);
        scope.spawn(move || {
            for (_, payload) in chunks.filter(|(kind, _)| *kind == ChunkType::ReplayData) {
                let plain = decompress_replay_data_with_trailing(payload, compressed, encrypted);
                if tx.send(plain).is_err() {
                    return;
                }
            }
        });
        for chunk in replay.chunks(data) {
            let (kind, payload) = chunk?;
            on_chunk(match kind {
                ChunkType::ReplayData => {
                    // Closed early only by a panic, which the scope re-raises.
                    let decompressed = rx.recv().map_err(|_| {
                        CliError::Usage("the decompression thread stopped".to_owned())
                    })?;
                    let (plain, unread) = decompressed?;
                    Chunk::ReplayData(plain, unread)
                }
                ChunkType::Checkpoint => Chunk::Checkpoint(payload),
                ChunkType::Event => Chunk::Event(payload),
                ChunkType::Header | ChunkType::Unknown(_) => Chunk::Other,
            })?;
        }
        Ok(())
    })
}

/// One replication stream -- the ReplayData stream, or one checkpoint
/// snapshot -- and what walking it counted.
pub(crate) struct Pass<'a> {
    branch: &'a str,
    flags: u32,
    pub cache: NetGuidCache,
    pub reader: ReplicationReader,
    pub channels: ChannelState,
    pub buffers: RecordBuffers,
    /// Packets walked, and so the next packet's id.
    pub packets: u32,
    pub frames: u32,
    pub frame_skips: FrameSkips,
    pub non_finite_frame_times: u64,
    /// `export --checkpoints` only: the snapshot's block rows' checkpoint and
    /// running field-row and block offsets, advanced after every packet.
    #[cfg(feature = "export")]
    pub block_scope: Option<(CheckpointIdentity, u64, u32)>,
}

impl<'a> Pass<'a> {
    pub fn new(replay: &Replay<'a>) -> Result<Self, CliError> {
        Ok(Self {
            branch: replay.branch,
            flags: replay.flags,
            cache: NetGuidCache::new(),
            reader: replication_reader(replay.branch)?,
            channels: ChannelState::new(),
            buffers: RecordBuffers::default(),
            packets: 0,
            frames: 0,
            frame_skips: FrameSkips::default(),
            non_finite_frame_times: 0,
            #[cfg(feature = "export")]
            block_scope: None,
        })
    }

    /// Read every packet in `frames` through a fresh sink, fold its counters
    /// into `sink` and `errors`, then hand its rows to `drain`. The first
    /// `drain` error ends the walk (the frame callback cannot return it).
    pub fn walk(
        &mut self,
        frames: &[u8],
        sink: &mut SinkTotals,
        errors: &mut OverlayErrorReport,
        mut drain: impl FnMut(&mut RecordBuffers) -> Result<(), CliError>,
    ) -> Result<(), CliError> {
        let branch = self.branch;
        let Self {
            cache,
            reader,
            channels,
            buffers,
            packets,
            ..
        } = self;
        #[cfg(feature = "export")]
        let block_scope = &mut self.block_scope;
        let mut failed = None;
        let walk = walk_demo_frames(frames, self.flags, cache, |pkt, cache| {
            if failed.is_some() {
                return;
            }
            let mut packet = ExportSink::new(cache, channels, buffers);
            packet.enable_measured_array_routes(branch);
            #[cfg(feature = "export")]
            if let Some((checkpoint, fields, blocks)) = block_scope {
                packet.enable_checkpoint_block_context(checkpoint.clone(), *fields, *blocks);
            }
            packet.time_ms = pkt.time_ms;
            packet.packet_id = *packets;
            reader.process_packet(pkt.data, *packets as i32, &mut packet);
            // The sink dies here: a counter not absorbed now never existed.
            sink.absorb(&mut packet.stats, errors);
            *packets += 1;
            #[cfg(feature = "export")]
            if let Some((_, fields, blocks)) = block_scope {
                *fields += buffers.fields.len() as u64;
                *blocks += buffers.checkpoint_blocks.len() as u32;
            }
            if let Err(error) = drain(buffers) {
                failed = Some(error);
            }
        })?;
        if let Some(error) = failed {
            return Err(error);
        }
        self.frames += walk.frames;
        self.frame_skips.absorb(walk.skipped);
        self.non_finite_frame_times += u64::from(walk.non_finite_times);
        Ok(())
    }

    /// End of stream: every bunch still in partial reassembly is counted and
    /// becomes a `buffers.partials` row. Until the stream stops, an
    /// unfinished partial cannot be told from one in flight.
    pub fn finish(&mut self) {
        let mut sink = ExportSink::new(&mut self.cache, &mut self.channels, &mut self.buffers);
        self.reader.finish_with_sink(&mut sink);
    }
}

#[cfg(test)]
mod tests {
    use vrf_container::parse_preamble;
    use vrf_testkit::{Info, add_i32, add_u32, chunk, header_payload, replay_info};

    use super::*;

    /// An uncompressed ReplayData chunk of 4 frame bytes declaring
    /// `memory_size`: anything but 4 fails decompression.
    fn replay_data(memory_size: i32) -> Vec<u8> {
        let mut buf = Vec::new();
        add_u32(&mut buf, 0);
        add_u32(&mut buf, 60_000);
        add_i32(&mut buf, 4);
        add_i32(&mut buf, memory_size);
        buf.extend([0; 4]);
        chunk(1, &buf)
    }

    /// The helper decompresses ahead, yet a bad chunk's error arrives only
    /// after the chunk before it, and an `on_chunk` error wins over it.
    #[test]
    fn errors_arrive_at_their_own_chunk_and_on_chunk_errors_stop_the_walk() {
        let mut data = replay_info(&Info::default());
        data.extend(chunk(0, &header_payload()));
        for memory_size in [4, 5, 4] {
            data.extend(replay_data(memory_size));
        }
        let preamble = parse_preamble(&data).unwrap();
        let replay = Replay::new(&preamble);

        let mut walked = 0;
        let result = for_each_chunk(&data, &replay, |chunk| {
            walked += u32::from(matches!(chunk, Chunk::ReplayData(..)));
            Ok(())
        });
        assert!(matches!(result, Err(CliError::Container(_))), "{result:?}");
        assert_eq!(walked, 1, "the chunk before the bad one was walked first");

        let result = for_each_chunk(&data, &replay, |chunk| match chunk {
            Chunk::ReplayData(..) => Err(CliError::Usage("stop".into())),
            _ => Ok(()),
        });
        assert!(
            matches!(&result, Err(CliError::Usage(m)) if m == "stop"),
            "{result:?}"
        );
    }
}
