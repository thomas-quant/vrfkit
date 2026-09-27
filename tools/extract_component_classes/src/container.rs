//! One IoStore container: its `.utoc` parsed, its `.ucas` read on demand.
//!
//! Read-only by construction. Files are opened with `File::open`, which asks
//! for read access and, on Windows, shares read, write and delete with every
//! other handle -- the game (or its patcher) is never locked out.
//!
//! A chunk is a byte range of the container's uncompressed stream, which is cut
//! into fixed-size compression blocks; block `i` covers
//! `[i * block_size, (i + 1) * block_size)`. Reading part of a chunk means
//! decompressing only the blocks that part touches, which is what keeps a scan
//! of every package's header to a fraction of the 30 GB of `.ucas`.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::reader::{Error, Result, fail};
use crate::toc::{Toc, parse_toc};

pub struct Container {
    /// File stem, e.g. `pakchunk0-WindowsClient`.
    pub name: String,
    pub ucas_path: PathBuf,
    pub toc: Toc,
}

impl Container {
    pub fn open(utoc_path: &Path) -> Result<Container> {
        let bytes =
            std::fs::read(utoc_path).map_err(|e| Error(format!("{}: {e}", utoc_path.display())))?;
        let toc = parse_toc(&bytes).map_err(|e| Error(format!("{}: {e}", utoc_path.display())))?;
        let name = utoc_path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        Ok(Container {
            name,
            ucas_path: utoc_path.with_extension("ucas"),
            toc,
        })
    }

    pub fn open_ucas(&self) -> Result<File> {
        File::open(&self.ucas_path).map_err(|e| Error(format!("{}: {e}", self.ucas_path.display())))
    }

    /// The first `limit` bytes of chunk `entry` (all of it when the chunk is
    /// shorter), decompressing only the blocks that range touches.
    pub fn read_chunk(
        &self,
        ucas: &mut (impl Read + Seek),
        entry: usize,
        limit: u64,
    ) -> Result<Vec<u8>> {
        let Some(chunk) = self.toc.chunks.get(entry) else {
            return fail(format!("{}: chunk {entry} out of range", self.name));
        };
        let want = chunk.length.min(limit);
        if want == 0 {
            return Ok(Vec::new());
        }
        let block_size = u64::from(self.toc.header.compression_block_size);
        let first = chunk.offset / block_size;
        let last = (chunk.offset + want - 1) / block_size;
        let mut out = Vec::with_capacity(((last - first + 1) * block_size) as usize);
        for index in first..=last {
            self.read_block(ucas, index as usize, &mut out)?;
        }
        let start = (chunk.offset - first * block_size) as usize;
        let end = start + want as usize;
        if end > out.len() {
            return fail(format!(
                "{}: chunk {entry} needs {end} bytes of its blocks, they hold {}",
                self.name,
                out.len()
            ));
        }
        out.truncate(end);
        out.drain(..start);
        Ok(out)
    }

    fn read_block(
        &self,
        ucas: &mut (impl Read + Seek),
        index: usize,
        out: &mut Vec<u8>,
    ) -> Result<()> {
        let Some(block) = self.toc.blocks.get(index) else {
            return fail(format!("{}: block {index} out of range", self.name));
        };
        let mut raw = vec![0u8; block.compressed_size as usize];
        ucas.seek(SeekFrom::Start(block.offset))
            .and_then(|_| ucas.read_exact(&mut raw))
            .map_err(|e| {
                Error(format!(
                    "{}: block {index} at {}: {e}",
                    self.name, block.offset
                ))
            })?;
        match self.toc.method_name(block.method)? {
            None => {
                if block.compressed_size != block.uncompressed_size {
                    return fail(format!(
                        "{}: stored block {index} is {} bytes but declares {} uncompressed",
                        self.name, block.compressed_size, block.uncompressed_size
                    ));
                }
                out.extend_from_slice(&raw);
            }
            Some("Oodle") => {
                let size = block.uncompressed_size as usize;
                let start = out.len();
                out.resize(start + size, 0);
                // A fresh extractor per block: `Extractor` keeps decoder state
                // across calls, and a block is an independent stream -- see the
                // same choice, and why, in crates/vrf-container/src/oodle.rs.
                let n = oozextract::Extractor::new()
                    .read_from_slice(&raw, &mut out[start..])
                    .map_err(|e| Error(format!("{}: block {index}: Oodle: {e:?}", self.name)))?;
                if n != size {
                    return fail(format!(
                        "{}: block {index} decompressed to {n} bytes, declares {size}",
                        self.name
                    ));
                }
            }
            Some(other) => {
                return fail(format!(
                    "{}: block {index} uses compression method {other:?}, which this tool does not read",
                    self.name
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::toc::tests::{TocSpec, build_toc};
    use crate::toc::{ChunkId, CompressedBlock, OffsetLength};
    use std::io::Cursor as IoCursor;

    /// A container of stored (uncompressed) blocks: 8-byte blocks, chunk 0 at
    /// stream offset 4 spanning three blocks, so a read crosses two block
    /// boundaries and starts mid-block.
    fn stored() -> (Container, Vec<u8>) {
        let ucas: Vec<u8> = (0u8..32).collect();
        let spec = TocSpec {
            flags: crate::toc::FLAG_INDEXED,
            block_size: 8,
            methods: vec![],
            chunks: vec![(
                ChunkId {
                    id: 1,
                    index: 0,
                    chunk_type: 1,
                },
                OffsetLength {
                    offset: 4,
                    length: 18,
                },
            )],
            blocks: (0..4)
                .map(|i| CompressedBlock {
                    offset: i * 8,
                    compressed_size: 8,
                    uncompressed_size: 8,
                    method: 0,
                })
                .collect(),
            ..TocSpec::default()
        };
        let toc = parse_toc(&build_toc(&spec)).unwrap();
        (
            Container {
                name: "t".to_owned(),
                ucas_path: PathBuf::new(),
                toc,
            },
            ucas,
        )
    }

    #[test]
    fn a_chunk_is_cut_out_of_the_blocks_it_spans() {
        let (c, ucas) = stored();
        let mut f = IoCursor::new(ucas);
        assert_eq!(
            c.read_chunk(&mut f, 0, u64::MAX).unwrap(),
            (4u8..22).collect::<Vec<_>>()
        );
        assert_eq!(c.read_chunk(&mut f, 0, 3).unwrap(), [4, 5, 6]);
    }

    #[test]
    fn a_short_ucas_is_an_error() {
        let (c, mut ucas) = stored();
        ucas.truncate(20);
        let mut f = IoCursor::new(ucas);
        assert!(c.read_chunk(&mut f, 0, u64::MAX).is_err());
    }

    #[test]
    fn a_stored_block_whose_sizes_disagree_is_an_error() {
        let (mut c, ucas) = stored();
        c.toc.blocks[1].uncompressed_size = 9;
        let mut f = IoCursor::new(ucas);
        assert!(c.read_chunk(&mut f, 0, u64::MAX).is_err());
    }
}
