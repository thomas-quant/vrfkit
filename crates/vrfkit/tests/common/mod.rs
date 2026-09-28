//! What the binary-level tests share: a minimal uncompressed replay
//! preamble, and a way to run the real `vrfkit` binary on it.

use std::path::Path;
use std::process::Command;

pub fn add_u16(buf: &mut Vec<u8>, v: u16) {
    buf.extend_from_slice(&v.to_le_bytes());
}

pub fn add_u32(buf: &mut Vec<u8>, v: u32) {
    buf.extend_from_slice(&v.to_le_bytes());
}

pub fn add_i32(buf: &mut Vec<u8>, v: i32) {
    buf.extend_from_slice(&v.to_le_bytes());
}

pub fn add_f32(buf: &mut Vec<u8>, v: f32) {
    buf.extend_from_slice(&v.to_le_bytes());
}

pub fn add_fstring(buf: &mut Vec<u8>, s: &str) {
    add_i32(buf, (s.len() + 1) as i32);
    buf.extend_from_slice(s.as_bytes());
    buf.push(0);
}

/// The replay info section: uncompressed, unencrypted, carrying the one
/// custom version the container pins. Field order as `info.rs` reads it.
pub fn replay_info() -> Vec<u8> {
    let mut buf = Vec::new();
    add_u32(&mut buf, 0x43F4_EFDD); // file magic
    add_u32(&mut buf, 7); // file version
    add_i32(&mut buf, 1); // one custom version
    for word in [0x95A4_F03E_u32, 0x7E0B_49E4, 0xBA43_D356, 0x94FF_87D9] {
        add_u32(&mut buf, word);
    }
    add_i32(&mut buf, 7); // LocalFileReplay version
    add_i32(&mut buf, 60_000); // length in ms
    add_u32(&mut buf, 19); // network version
    add_u32(&mut buf, 1234); // changelist
    add_fstring(&mut buf, "Match");
    add_u32(&mut buf, 0); // is live
    buf.extend_from_slice(&42i64.to_le_bytes()); // timestamp
    add_u32(&mut buf, 0); // compressed
    add_u32(&mut buf, 0); // encrypted
    add_i32(&mut buf, 0); // encryption key length
    buf
}

/// A 12.10 header with flags `HasStreamingFixes | GameSpecificFrameData`, so
/// every frame carries the game-specific section. Field order as `header.rs`
/// reads it.
pub fn header_payload() -> Vec<u8> {
    let mut buf = Vec::new();
    add_u32(&mut buf, 0x2CF5_A13D); // network magic
    add_u32(&mut buf, 19); // network version
    add_i32(&mut buf, 0); // custom version count
    add_u32(&mut buf, 0x1122_3344); // network checksum
    add_u32(&mut buf, 32); // engine net proto version
    add_u32(&mut buf, 0x5566_7788); // game net proto version
    for word in [0x0011_2233_u32, 0x4455_6677, 0x8899_AABB, 0xCCDD_EEFF] {
        add_u32(&mut buf, word);
    }
    add_u16(&mut buf, 12); // major
    add_u16(&mut buf, 10); // minor
    add_u16(&mut buf, 1); // patch
    add_u32(&mut buf, 123_456); // changelist
    add_fstring(&mut buf, "++Ares-Core+release-12.10");
    buf.extend_from_slice(&[3, 0, 0, 0, 49, 56, 0]); // valorant skip: 3 bytes
    add_u32(&mut buf, 1001); // UE4 version
    add_u32(&mut buf, 1002); // UE5 version
    add_u32(&mut buf, 1003); // package version license
    add_i32(&mut buf, 1); // one level name
    add_fstring(&mut buf, "Ascent");
    add_u32(&mut buf, 42); // level time
    add_u32(&mut buf, 0b1010); // HasStreamingFixes | GameSpecificFrameData
    add_i32(&mut buf, 0); // game-specific data count
    add_f32(&mut buf, 15.0);
    add_f32(&mut buf, 30.0);
    add_f32(&mut buf, 33.3);
    add_f32(&mut buf, 250.0);
    add_fstring(&mut buf, "Windows");
    buf.push(7); // build config
    buf.push(3); // build target type
    buf
}

pub fn chunk(chunk_type: u32, payload: &[u8]) -> Vec<u8> {
    let mut buf = Vec::new();
    add_u32(&mut buf, chunk_type);
    add_i32(&mut buf, payload.len() as i32);
    buf.extend_from_slice(payload);
    buf
}

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
