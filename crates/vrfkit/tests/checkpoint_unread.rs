//! Checkpoint archive bytes the Oodle codec never read, followed to the export
//! summary and the diag JSON.
//!
//! `decompress_checkpoint_with_trailing` returns them, and they are 0 in every
//! corpus checkpoint archive (vrf-container's census), so no corpus guard can
//! see a call site that drops the count. This builds a compressed replay whose
//! archives are single uncompressed Kraken blocks -- the shape vrf-container's
//! `archive_with_unread_input` test uses -- with bytes left after each block,
//! and requires both outputs to report them with the framing residual after
//! the archive, and apart from the ReplayData archive's own unread bytes,
//! which `ReplayData unread` already counts.

mod common;

use std::path::{Path, PathBuf};

use common::{
    add_f32, add_fstring, add_i32, add_u32, chunk, header_payload, path_arg, replay_info, vrfkit,
};

/// Bytes after the block in each of the two checkpoint archives.
const CHECKPOINT_UNREAD: [usize; 2] = [7, 5];
/// Bytes after the first checkpoint's archive, inside its chunk.
const CHECKPOINT_CHUNK_TRAILING: usize = 2;
/// Everything the checkpoint pass must report: 7 + 5 + 2.
const CHECKPOINT_TOTAL: u64 = 14;
/// Bytes after the block in the one ReplayData archive.
const REPLAY_DATA_UNREAD: u64 = 3;

/// `replay_info()` with its `compressed` word set. The section ends with the
/// compressed, encrypted and key-length words, four bytes each.
fn compressed_replay_info() -> Vec<u8> {
    let mut info = replay_info();
    let at = info.len() - 12;
    assert_eq!(info[at..], [0u8; 12], "compressed, encrypted, key length");
    info[at..at + 4].copy_from_slice(&1u32.to_le_bytes());
    info
}

/// One DemoFrame under the header's flags with nothing in it: no exports,
/// levels, ExternalData, game-specific bytes or packets.
fn empty_frame() -> Vec<u8> {
    let mut buf = Vec::new();
    add_i32(&mut buf, 0); // currentLevelIndex
    add_f32(&mut buf, 1.0); // timeSeconds
    buf.push(0); // no layout exports
    buf.push(0); // no export GUIDs
    buf.push(0); // no streaming levels
    buf.extend_from_slice(&0u64.to_le_bytes()); // externalOffset
    buf.push(0); // ExternalData terminator
    buf.extend_from_slice(&0u64.to_le_bytes()); // no game-specific bytes
    buf.push(0); // seenLevelIndex
    add_i32(&mut buf, 0); // packetSize 0: the frame ends with no packet
    buf
}

/// An Oodle archive around one uncompressed Kraken block holding `plain`, then
/// `unread` bytes inside the declared compressed size that no block reads.
/// `0x4C` is block-header nibble `0xC` with the uncompressed bit set and `0x06`
/// Kraken without checksums, as vrf-container's test states for oozextract.
fn archive(plain: &[u8], unread: usize) -> Vec<u8> {
    let mut archive = Vec::new();
    add_i32(&mut archive, plain.len() as i32); // decompressed_size
    add_i32(&mut archive, (2 + plain.len() + unread) as i32); // compressed_size
    archive.extend_from_slice(&[0x4C, 0x06]);
    archive.extend_from_slice(plain);
    archive.extend(std::iter::repeat_n(0xAB, unread));
    archive
}

/// A compressed ReplayData chunk payload: SizeInBytes is the archive's length
/// and MemorySizeInBytes the frames'.
fn replay_data(frames: &[u8], unread: usize) -> Vec<u8> {
    let archive = archive(frames, unread);
    let mut buf = Vec::new();
    add_u32(&mut buf, 0); // time1
    add_u32(&mut buf, 60_000); // time2
    add_i32(&mut buf, archive.len() as i32);
    add_i32(&mut buf, frames.len() as i32);
    buf.extend_from_slice(&archive);
    buf
}

/// A Checkpoint chunk payload whose archive declares no GUIDs and no export
/// groups, then carries one empty frame; `chunk_trailing` bytes follow the
/// archive inside the chunk. The prologue's first word is the frame offset
/// minus 8, and the tables end at byte 24.
fn checkpoint(index: u32, unread: usize, chunk_trailing: usize) -> Vec<u8> {
    let mut plain = Vec::new();
    add_u32(&mut plain, 16); // frame offset word: 24 - 8
    for _ in 0..3 {
        add_u32(&mut plain, 0); // reserved words
    }
    add_u32(&mut plain, 0); // GUID entries
    add_u32(&mut plain, 0); // export groups
    plain.extend(empty_frame());
    let archive = archive(&plain, unread);

    let mut buf = Vec::new();
    add_fstring(&mut buf, &format!("checkpoint{index}"));
    add_fstring(&mut buf, "checkpoint");
    add_fstring(&mut buf, &(index + 1).to_string());
    add_u32(&mut buf, 1000 * (index + 1)); // time1
    add_u32(&mut buf, 1000 * (index + 1)); // time2
    add_i32(&mut buf, archive.len() as i32);
    buf.extend_from_slice(&archive);
    buf.extend(std::iter::repeat_n(0xCD, chunk_trailing));
    buf
}

fn replay() -> Vec<u8> {
    let mut data = compressed_replay_info();
    data.extend(chunk(0, &header_payload()));
    data.extend(chunk(
        1,
        &replay_data(&empty_frame(), REPLAY_DATA_UNREAD as usize),
    ));
    data.extend(chunk(
        2,
        &checkpoint(0, CHECKPOINT_UNREAD[0], CHECKPOINT_CHUNK_TRAILING),
    ));
    data.extend(chunk(2, &checkpoint(1, CHECKPOINT_UNREAD[1], 0)));
    data
}

/// A fresh directory under Cargo's per-target scratch area, and the replay
/// written into it.
fn scratch(name: &str) -> (PathBuf, PathBuf) {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("checkpoint_unread-{name}-{}", std::process::id()));
    if dir.exists() {
        std::fs::remove_dir_all(&dir).expect("clear a stale scratch directory");
    }
    std::fs::create_dir_all(&dir).expect("create the scratch directory");
    let replay_path = dir.join("checkpoint_unread.vrf");
    std::fs::write(&replay_path, replay()).expect("write the synthetic replay");
    (dir, replay_path)
}

/// The text after `label` on the one line that starts with it.
fn line_value(output: &str, label: &str) -> String {
    let lines: Vec<&str> = output
        .lines()
        .filter_map(|line| line.trim_start().strip_prefix(label))
        .collect();
    assert_eq!(lines.len(), 1, "expected one `{label}` line:\n{output}");
    lines[0].trim().to_owned()
}

/// The unsigned integer after `"key": `, which must occur exactly once; the
/// leading quote keeps `"trailing_bytes"` from matching inside
/// `"replay_data_trailing_bytes"`.
fn json_u64(json: &str, key: &str) -> u64 {
    let needle = format!("\"{key}\": ");
    let hits: Vec<usize> = json.match_indices(&needle).map(|(at, _)| at).collect();
    assert_eq!(hits.len(), 1, "expected one {needle:?} in:\n{json}");
    json[hits[0] + needle.len()..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .unwrap_or_else(|_| panic!("{needle:?} is not followed by an integer in:\n{json}"))
}

#[test]
fn diag_counts_checkpoint_bytes_the_codec_never_read() {
    let (dir, replay_path) = scratch("diag");
    let json_path = dir.join("diag.json");
    let run = vrfkit(&[
        "diag",
        path_arg(&replay_path),
        "--json",
        path_arg(&json_path),
    ]);
    assert_eq!(run.code, Some(0), "{}\n{}", run.stdout, run.stderr);

    let json = std::fs::read_to_string(&json_path).expect("diag wrote its JSON");
    assert_eq!(json_u64(&json, "trailing_bytes"), CHECKPOINT_TOTAL);
    assert_eq!(
        json_u64(&json, "replay_data_trailing_bytes"),
        REPLAY_DATA_UNREAD
    );
    std::fs::remove_dir_all(dir).ok();
}

#[cfg(feature = "export")]
#[test]
fn export_counts_checkpoint_bytes_the_codec_never_read() {
    let (dir, replay_path) = scratch("export");
    let out = dir.join("out");
    let run = vrfkit(&[
        "export",
        path_arg(&replay_path),
        "--out",
        path_arg(&out),
        "--checkpoints",
    ]);
    assert_eq!(run.code, Some(0), "{}\n{}", run.stdout, run.stderr);

    assert_eq!(
        line_value(&run.stderr, "Trailing bytes:"),
        CHECKPOINT_TOTAL.to_string()
    );
    assert_eq!(
        line_value(&run.stderr, "ReplayData unread:"),
        format!("{REPLAY_DATA_UNREAD} bytes")
    );
    std::fs::remove_dir_all(dir).ok();
}
