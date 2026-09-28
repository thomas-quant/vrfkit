//! Bit-exactness of every registered transform against vectors this port did
//! not produce: the upstream builds' vectors are lifted mechanically from the
//! reference fixture (`tools/extract_golden.py`), and the legacy builds' are
//! captured from the original executables' reader functions
//! (`tools/capture_native_transforms.py`).
//!
//! Everything downstream -- field framing, schema binding, metrics -- is built
//! on the assumption that these bytes are right, so a failure here invalidates
//! all of it.

include!("data/golden_vectors.rs");
include!("data/native_vectors.rs");

use vrf_bitio::BitReader;
use vrf_transform::{ALL_VERSIONS, TransformVersion, seed_for};

/// `(branch, bit count, seed, input hex, expected output hex)` for every
/// vector. The golden vectors share one payload and derive their seed from
/// the bit count; the native ones carry both.
fn vectors() -> impl Iterator<Item = (&'static str, usize, u32, &'static str, &'static str)> {
    VECTORS
        .iter()
        .map(|&(branch, bits, expected)| {
            let seed = seed_for(bits, ACTOR_NET_GUID);
            (branch, bits, seed, PAYLOAD_HEX, expected)
        })
        .chain(NATIVE_VECTORS.iter().copied())
}

fn from_hex(hex: &str) -> Vec<u8> {
    assert!(hex.len() % 2 == 0, "odd-length hex: {hex}");
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("valid hex"))
        .collect()
}

#[test]
fn transforms_match_the_golden_and_native_vectors() {
    let mut failures = Vec::new();
    let mut total = 0;
    for (branch, bits, seed, input, expected) in vectors() {
        total += 1;
        let version = TransformVersion::require(branch).expect("branch is registered");
        let mut out = vec![0u8; TransformVersion::output_byte_count(bits)];
        version
            .decode_from(&mut BitReader::new(&from_hex(input)), bits, seed, &mut out)
            .expect("input is long enough for the vector");
        if out != from_hex(expected) {
            let actual: String = out.iter().map(|b| format!("{b:02X}")).collect();
            failures.push(format!(
                "{branch} @ {bits} bits, seed {seed:#x}\n     expected {expected}\n     actual   {actual}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {total} vectors mismatched:\n  {}",
        failures.len(),
        failures.join("\n  ")
    );
}

#[test]
fn vectors_cover_the_staging_boundaries() {
    // The transform stages 64 -> 32 -> 8 -> tail. If a build's vectors skipped a
    // boundary, an error in one stage could pass unnoticed; a build with no
    // vectors at all fails the first boundary.
    for version in ALL_VERSIONS.iter().copied() {
        let bits: Vec<usize> = vectors()
            .filter(|v| v.0 == version.branch())
            .map(|v| v.1)
            .collect();
        for required in [0usize, 1, 7, 8, 31, 32, 63, 64, 65] {
            assert!(
                bits.contains(&required),
                "{} lacks a vector at {required} bits (have {bits:?})",
                version.branch()
            );
        }
    }
}
