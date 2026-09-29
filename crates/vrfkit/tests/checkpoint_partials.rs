//! Partial rows from the checkpoint pass, which runs on its own thread and
//! hands them back for the main thread to write into `partials.parquet`.
//! Every measured replay has none, so no corpus guard sees a dropped hand-off.

#![cfg(feature = "export")]

mod common;

use common::*;

/// A packet whose one reliable bunch opens a partial reassembly on channel 2
/// and never finishes it: 8 payload bits at end of stream.
fn unfinished_partial_packet() -> Vec<u8> {
    let mut payload = BitWriter::new();
    payload.int_packed(3);
    let mut bits = BitWriter::new();
    bits.extend_bits(&[true, true, false, false, true]) // control, open, close, paused, reliable
        .int_packed(2) // channel index
        .extend_bits(&[false, false, true, false]) // exports, must be mapped, partial, Valorant
        .extend_bits(&[true, false]) // partial initial, partial final
        .bit(true) // hardcoded channel name
        .int_packed(1)
        .serialized_int(payload.len() as u32, 2 * 1024 * 8)
        .extend_bits(&payload)
        .bit(true); // end-of-packet marker
    pack(&bits)
}

/// The only unfinished bunch is in a checkpoint frame, so `partials.parquet`
/// holds a row with `--checkpoints` and none without, and the summary counts
/// the row the file received.
#[test]
fn checkpoint_partial_rows_reach_the_partials_table() {
    let dir = scratch("checkpoint_partials", "export");
    let frames = frame(1.0, &[], 0, &[]);
    let packet = unfinished_partial_packet();
    let snapshot = checkpoint_tables(&frame(1.5, &[], 0, &[&packet]));
    let mut data = replay_info(&Info::default());
    data.extend(chunk(0, &header_payload()));
    data.extend(chunk(1, &replay_data(&frames, frames.len())));
    data.extend(chunk(2, &checkpoint(0, &snapshot, 0)));
    let replay = dir.join("checkpoint_partials.vrf");
    std::fs::write(&replay, data).expect("write the synthetic replay");

    let export = |name: &str, extra: &[&str]| {
        let out = dir.join(name);
        let mut args = vec!["export", path_arg(&replay), "--out", path_arg(&out)];
        args.extend(extra);
        let run = vrfkit(&args);
        assert_eq!(run.code, Some(0), "{}\n{}", run.stdout, run.stderr);
        let table = std::fs::read(out.join("partials.parquet")).expect("partials.parquet");
        (run, table)
    };
    let (plain, without) = export("plain", &[]);
    let (checkpoints, with) = export("checkpoints", &["--checkpoints"]);

    assert_eq!(line_value(&plain.stderr, "Partial raw rows:"), "0 (0 bits)");
    assert_eq!(
        line_value(&checkpoints.stderr, "Partial raw rows:"),
        "0 (0 bits)"
    );
    assert_eq!(
        line_value(&checkpoints.stderr, "Checkpoint partial raw:"),
        "1 rows / 8 bits"
    );
    assert_ne!(
        with, without,
        "the checkpoint row never reached partials.parquet"
    );
    std::fs::remove_dir_all(dir).ok();
}
