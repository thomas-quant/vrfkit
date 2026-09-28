//! The frame-skip tallies, followed from the DemoFrame walk to every output.
//!
//! Five passes carry `walk_demo_frames`' skip tally to an output with one line
//! of glue each: export's main and checkpoint passes (summary and manifest),
//! `validate`, and `diag`'s two passes. No corpus guard can see a cut line:
//! the value is 0 on every replay measured, and the baseline's
//! summary-vs-manifest reconciliation compares two outputs fed from one
//! variable. So this builds an uncompressed replay whose frames carry both
//! sections, two chunks per pass with distinct totals per pass and tally, runs
//! the real binary, and requires each output to report its own pass's totals:
//! a pass that absorbs nothing, the other pass's walk or only its last chunk
//! reads the wrong numbers. With no packets, `validate` ends in exit 2 (no
//! content blocks), asserted so a crash (exit 1) cannot pass for it.

mod common;

use std::path::{Path, PathBuf};

use common::{
    add_f32, add_fstring, add_i32, add_u32, chunk, header_payload, path_arg, replay_info, vrfkit,
};

/// What the ReplayData pass must report: `(blobs, bytes, game-specific
/// bytes)`, summed over its two chunks -- (2, 5, 4) and (1, 1, 7).
const MAIN: (u64, u64, u64) = (3, 6, 11);
/// What the checkpoint pass must report, summed over (1, 2, 3) and (0, 0, 2).
/// All six values differ, so a swapped field or pass cannot match by accident.
const CHECKPOINT: (u64, u64, u64) = (1, 2, 5);

fn add_u64(buf: &mut Vec<u8>, v: u64) {
    buf.extend_from_slice(&v.to_le_bytes());
}

/// Byte-aligned IntPacked: seven bits per byte, low bit = "more follows".
fn add_int_packed(buf: &mut Vec<u8>, mut value: u32) {
    loop {
        let mut byte = ((value & 0x7f) << 1) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 1;
        }
        buf.push(byte);
        if value == 0 {
            return;
        }
    }
}

/// One DemoFrame under the header's flags: no exports, no streaming levels,
/// the given ExternalData blobs `(numBits, netGuid)` -- each followed by
/// `ceil(numBits / 8)` payload bytes -- a GameSpecificFrameData block of
/// `game_bytes`, and no packets.
fn frame(time_seconds: f32, external: &[(u32, u32)], game_bytes: usize) -> Vec<u8> {
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
    add_int_packed(&mut buf, 0); // seenLevelIndex
    add_i32(&mut buf, 0); // packetSize 0: the frame ends with no packet
    buf
}

/// An uncompressed ReplayData chunk payload: SizeInBytes equals
/// MemorySizeInBytes, and the frames follow.
fn replay_data(frames: &[u8]) -> Vec<u8> {
    let mut buf = Vec::new();
    add_u32(&mut buf, 0); // time1
    add_u32(&mut buf, 60_000); // time2
    add_i32(&mut buf, frames.len() as i32);
    add_i32(&mut buf, frames.len() as i32);
    buf.extend_from_slice(frames);
    buf
}

/// A Checkpoint chunk payload whose uncompressed archive declares no GUIDs
/// and no export groups, then carries `frames`. The prologue's first word is
/// the frame offset minus 8, and the tables end at byte 24.
fn checkpoint(index: u32, frames: &[u8]) -> Vec<u8> {
    let mut archive = Vec::new();
    add_u32(&mut archive, 16); // frame offset word: 24 - 8
    for _ in 0..3 {
        add_u32(&mut archive, 0); // reserved words
    }
    add_u32(&mut archive, 0); // GUID entries
    add_u32(&mut archive, 0); // export groups
    archive.extend_from_slice(frames);

    let mut buf = Vec::new();
    add_fstring(&mut buf, &format!("checkpoint{index}"));
    add_fstring(&mut buf, "checkpoint");
    add_fstring(&mut buf, &(index + 1).to_string());
    add_u32(&mut buf, 1000 * (index + 1)); // time1
    add_u32(&mut buf, 1000 * (index + 1)); // time2
    add_i32(&mut buf, archive.len() as i32);
    buf.extend_from_slice(&archive);
    buf
}

/// The whole file: info, header, then ReplayData and Checkpoint chunks
/// interleaved, two of each, with the totals `MAIN` and `CHECKPOINT` name.
fn replay() -> Vec<u8> {
    let mut data = replay_info();
    data.extend(chunk(0, &header_payload()));
    data.extend(chunk(1, &replay_data(&frame(1.0, &[(12, 6), (17, 9)], 4))));
    data.extend(chunk(2, &checkpoint(0, &frame(1.0, &[(9, 5)], 3))));
    data.extend(chunk(1, &replay_data(&frame(2.0, &[(1, 4)], 7))));
    data.extend(chunk(2, &checkpoint(1, &frame(2.0, &[], 2))));
    data
}

/// A fresh directory under Cargo's per-target scratch area, and the replay
/// written into it.
fn scratch(name: &str) -> (PathBuf, PathBuf) {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("frame_skips-{name}-{}", std::process::id()));
    if dir.exists() {
        std::fs::remove_dir_all(&dir).expect("clear a stale scratch directory");
    }
    std::fs::create_dir_all(&dir).expect("create the scratch directory");
    let replay_path = dir.join("frame_skips.vrf");
    std::fs::write(&replay_path, replay()).expect("write the synthetic replay");
    (dir, replay_path)
}

/// The one line starting with `label` after its indent, label removed and
/// spacing normalised: padding differs between the summary and `validate`,
/// the numbers must not. A second such line would make the reading ambiguous.
fn skips_line(output: &str, label: &str) -> String {
    let lines: Vec<&str> = output
        .lines()
        .filter_map(|line| line.trim_start().strip_prefix(label))
        .collect();
    assert_eq!(
        lines.len(),
        1,
        "expected one `{label}` line, found {}:\n{output}",
        lines.len()
    );
    lines[0].split_whitespace().collect::<Vec<_>>().join(" ")
}

fn rendered((blobs, bytes, game): (u64, u64, u64)) -> String {
    format!("{blobs} external blobs / {bytes} external bytes / {game} game-specific bytes")
}

/// The unsigned integer after `"key": `, which must occur exactly once. The
/// leading quote keeps `"external_data_blobs"` from matching inside
/// `"replay_data_external_data_blobs"` or `"checkpoint_frame_..."`.
fn json_u64(json: &str, key: &str) -> u64 {
    let needle = format!("\"{key}\": ");
    let hits: Vec<usize> = json.match_indices(&needle).map(|(at, _)| at).collect();
    assert_eq!(hits.len(), 1, "expected one {needle:?} in:\n{json}");
    let digits: String = json[hits[0] + needle.len()..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits
        .parse()
        .unwrap_or_else(|_| panic!("{needle:?} is not followed by an integer in:\n{json}"))
}

fn json_skips(json: &str, prefix: &str) -> (u64, u64, u64) {
    (
        json_u64(json, &format!("{prefix}external_data_blobs")),
        json_u64(json, &format!("{prefix}external_data_bytes")),
        json_u64(json, &format!("{prefix}game_specific_bytes")),
    )
}

/// `validate` walks ReplayData only; its line must carry the main totals.
#[test]
fn validate_reports_the_replay_data_frame_skips() {
    let (dir, replay_path) = scratch("validate");
    let run = vrfkit(&["validate", path_arg(&replay_path)]);

    assert_eq!(
        run.code,
        Some(2),
        "a replay with no content blocks ends in the no-content verdict, not an error\n{}\n{}",
        run.stdout,
        run.stderr
    );
    assert_eq!(skips_line(&run.stdout, "ReplayData frames:"), "2");
    assert_eq!(skips_line(&run.stdout, "Frame skips:"), rendered(MAIN));
    std::fs::remove_dir_all(dir).ok();
}

/// `diag` reports the main pass under `chunks` with a `replay_data_` prefix,
/// the checkpoint pass under `checkpoint_meta` with none.
#[test]
fn diag_reports_each_pass_frame_skips() {
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
    assert_eq!(json_u64(&json, "replay_data_frames"), 2);
    assert_eq!(json_skips(&json, "replay_data_"), MAIN);
    assert_eq!(json_u64(&json, "frames"), 2, "checkpoint frames walked");
    assert_eq!(json_skips(&json, ""), CHECKPOINT);
    std::fs::remove_dir_all(dir).ok();
}

/// `export --checkpoints` reports each pass twice: a summary line on stderr
/// and a manifest key set. Both must carry the pass's own totals.
#[cfg(feature = "export")]
#[test]
fn export_reports_each_pass_frame_skips_in_summary_and_manifest() {
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

    assert_eq!(skips_line(&run.stderr, "Frame skips:"), rendered(MAIN));
    assert_eq!(
        skips_line(&run.stderr, "Checkpoint frame skips:"),
        rendered(CHECKPOINT)
    );

    let manifest =
        std::fs::read_to_string(out.join("manifest.json")).expect("export wrote a manifest");
    assert_eq!(json_skips(&manifest, "frame_"), MAIN);
    assert_eq!(json_skips(&manifest, "checkpoint_frame_"), CHECKPOINT);
    assert_eq!(json_u64(&manifest, "checkpoint_frames"), 2);
    std::fs::remove_dir_all(dir).ok();
}
