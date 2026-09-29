//! What the binary-level tests share: the real `vrfkit` binary, scratch
//! directories, readers for its text and JSON, and the replay pieces
//! vrf-testkit leaves to its callers.

// Each test crate uses a different subset.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

pub use vrf_testkit::*;

pub struct Run {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

pub fn vrfkit(args: &[&str]) -> Run {
    let output = Command::new(env!("CARGO_BIN_EXE_vrfkit"))
        .args(args)
        .output()
        .expect("run the vrfkit binary");
    Run {
        code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

pub fn path_arg(path: &Path) -> &str {
    path.to_str().expect("scratch paths are UTF-8")
}

/// A fresh directory under Cargo's per-target scratch area.
pub fn scratch(test: &str, name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("{test}-{name}-{}", std::process::id()));
    if dir.exists() {
        std::fs::remove_dir_all(&dir).expect("clear a stale scratch directory");
    }
    std::fs::create_dir_all(&dir).expect("create the scratch directory");
    dir
}

/// The text after `label` on the one line that starts with it (after its
/// indent), spacing normalised: padding differs between outputs, the numbers
/// must not.
pub fn line_value(output: &str, label: &str) -> String {
    let lines: Vec<&str> = output
        .lines()
        .filter_map(|line| line.trim_start().strip_prefix(label))
        .collect();
    assert_eq!(lines.len(), 1, "expected one `{label}` line:\n{output}");
    lines[0].split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The unsigned integer after `"key": `, which must occur exactly once; the
/// leading quote keeps `"frames"` from matching inside `"replay_data_frames"`.
pub fn json_u64(json: &str, key: &str) -> u64 {
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

/// One DemoFrame under `header_payload`'s flags: no exports or streaming
/// levels, the ExternalData blobs `(numBits, netGuid)` -- each followed by
/// `ceil(numBits / 8)` payload bytes -- a GameSpecificFrameData block of
/// `game_bytes`, then `packets`.
pub fn frame(
    time_seconds: f32,
    external: &[(u32, u32)],
    game_bytes: usize,
    packets: &[&[u8]],
) -> Vec<u8> {
    let mut buf = Vec::new();
    add_i32(&mut buf, 0); // currentLevelIndex
    add_f32(&mut buf, time_seconds);
    add_int_packed(&mut buf, 0); // no layout exports
    add_int_packed(&mut buf, 0); // no export GUIDs
    add_int_packed(&mut buf, 0); // no streaming levels
    add_u64(&mut buf, 0); // externalOffset
    for &(num_bits, net_guid) in external {
        add_int_packed(&mut buf, num_bits);
        add_int_packed(&mut buf, net_guid);
        buf.extend(std::iter::repeat_n(0xA5, num_bits.div_ceil(8) as usize));
    }
    add_int_packed(&mut buf, 0); // ExternalData terminator
    add_u64(&mut buf, game_bytes as u64);
    buf.extend(std::iter::repeat_n(0x5A, game_bytes));
    for packet in packets.iter().copied().chain([&[][..]]) {
        add_int_packed(&mut buf, 0); // seenLevelIndex
        add_i32(&mut buf, packet.len() as i32); // 0 ends the frame
        buf.extend_from_slice(packet);
    }
    buf
}

/// A ReplayData chunk payload: SizeInBytes is `body`'s length (the frames,
/// or their archive in a compressed replay), MemorySizeInBytes `frames_len`.
pub fn replay_data(body: &[u8], frames_len: usize) -> Vec<u8> {
    let mut buf = Vec::new();
    add_u32(&mut buf, 0); // time1
    add_u32(&mut buf, 60_000); // time2
    add_i32(&mut buf, body.len() as i32);
    add_i32(&mut buf, frames_len as i32);
    buf.extend_from_slice(body);
    buf
}

/// A checkpoint archive's plain bytes: no GUIDs and no export groups, then
/// `frames`. The first word is the frame offset minus 8; the tables end at
/// byte 24.
pub fn checkpoint_tables(frames: &[u8]) -> Vec<u8> {
    let mut plain = Vec::new();
    add_u32(&mut plain, 16); // frame offset word: 24 - 8
    for _ in 0..5 {
        add_u32(&mut plain, 0); // three reserved words, GUID entries, export groups
    }
    plain.extend_from_slice(frames);
    plain
}

/// A Checkpoint chunk payload around `archive`, then `trailing` bytes inside
/// the chunk.
pub fn checkpoint(index: u32, archive: &[u8], trailing: usize) -> Vec<u8> {
    let mut buf = Vec::new();
    add_fstring(&mut buf, &format!("checkpoint{index}"));
    add_fstring(&mut buf, "checkpoint");
    add_fstring(&mut buf, &(index + 1).to_string());
    add_u32(&mut buf, 1000 * (index + 1)); // time1
    add_u32(&mut buf, 1000 * (index + 1)); // time2
    add_i32(&mut buf, archive.len() as i32);
    buf.extend_from_slice(archive);
    buf.extend(std::iter::repeat_n(0xCD, trailing));
    buf
}
