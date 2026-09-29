"""The export summary's counter lines, one table for every tool that reads them.

Each row is a line `crates/vrfkit/src/driver/summary.rs` prints, whitespace
collapsed and `report::` formatters expanded, with one key per placeholder.
Every regex is anchored on the whole line: free-text lines (`Struct blob err:`,
`Movement err:`) can quote a label, and `RPCs:` is a suffix of `Truncated
RPCs:`. A line that is not printed reads as None, never 0.

Standard library only: check_decode_errors_corpus.py runs without pyarrow.
"""
from __future__ import annotations

import re
from typing import NamedTuple

#: Who reads a line. P: check_export_baseline.py pins every key (the committed
#: `*_02d4d478.json` hold exactly these). G: check_decode_errors_corpus.py
#: gates on it. `cp_` keys are the `=== Checkpoints ===` block, printed only
#: under `--checkpoints`, so a default run never reads them.
LINES = (
    ("Chunks: {}", "chunks", "P"),
    ("Frame skips: {} external blobs / {} external bytes / {} game-specific bytes",
     "frame_external_data_blobs frame_external_data_bytes frame_game_specific_bytes", "P"),
    ("Frame times: {} non-finite", "frame_non_finite_times", "P"),
    ("Packets: {}", "packets", "P"),
    ("Export groups: {}", "export_groups", "P"),
    ("Content blocks: {}", "content_blocks", "P"),
    ("RepLayout blocks: {}", "rep_layout_blocks", "P"),
    ("ClassNetCache: {}", "class_net_cache_blocks", "P"),
    ("Fields: {}", "fields", "P"),
    ("RPCs: {}", "rpcs", "P"),
    ("Actor opens: {}", "actor_opens", "P"),
    ("Actor closes: {}", "actor_closes", "P"),
    ("Partial raw rows: {} ({} bits)", "partial_rows partial_bits", "P"),
    ("Bunches: {}", "bunches", "P"),
    ("Malformed pkts: {}", "malformed_packets", "P"),
    ("Skipped bits: {}", "skipped_bits", "P"),
    ("Movement rows: {}", "movement_rows", "PG"),
    ("NetGUID rows: {}", "net_guid_rows", "P"),
    ("Event rows: {}", "event_rows", "P"),
    # `decoded` keeps a decoder that stops running (0 decoded, 0 failed) from
    # reading as clean: 13.02 moved RoundResults from handle 93 to 81 and
    # nothing else on the summary moved.
    ("Struct blobs: {} decoded / {} failed", "struct_blobs_decoded struct_blobs_failed", "PG"),
    ("Movement errors: {}", "movement_rpc_errors", "G"),
    # A measured tally in sized and open windows, not a loss verdict.
    ("Movement tails: {} sized ({} bits) / {} open ({} bits)",
     "movement_sized_section_tails movement_sized_section_tail_bits "
     "movement_open_section_tails movement_open_section_tail_bits", "PG"),
    # Bits after each byte-wrapped movement envelope, which nothing reads:
    # verify_build_corpus.py fails a replay whose bits are not 24 per stream.
    ("Envelope trailers: {} streams / {} bits",
     "movement_envelope_trailers movement_envelope_trailer_bits", "PG"),
    ("Array decode: {} elements / {} fields / {} errors / {} truncations",
     "array_elements_decoded array_fields_emitted array_errors array_truncations", "G"),
    ("Array residual: {} root bits / {} nested bits / {} implicit ends",
     "array_unconsumed_root_bits array_unconsumed_nested_bits array_implicit_terminations", "G"),
    ("Array leaf errs: {}", "array_leaf_decode_errors", "G"),
    ("Target locations: {} array children", "targeting_world_locations_decoded", "P"),
    # A measured opaque shape, legitimately nonzero; not a decode-error count.
    ("Reward opaque: {} empty variants", "tracked_rewards_opaque_empty_variants", "PG"),
    # Empty deltas whose trailing zero byte was spared: a measured tolerance.
    ("ActiveBlinds trailers: {} empty deltas", "active_blinds_empty_trailers", "PG"),
    ("Truncated RPCs: {}", "truncated_rpcs", "G"),
    ("Event layout err: {}", "event_layout_mismatches", "P"),
    ("Event payloads: {} decoded / {} unknown groups",
     "event_payloads_decoded event_payload_unknown_groups", "P"),
    ("CNC RPC rows: {}", "cnc_rpcs_emitted", "P"),
    ("CNC brute force: {} attempted / {} unwalked",
     "cnc_bruteforce_payloads_attempted cnc_bruteforce_payloads_unwalked", "PG"),
    ("RepLayout tails: {} decoded / {} preserved",
     "rep_layout_cnc_tails_decoded rep_layout_cnc_tails_preserved", "P"),
    ("Checkpoints: {}", "cp_chunks", "P"),
    ("Checkpoint partial raw: {} rows / {} bits", "cp_partial_rows cp_partial_bits", "P"),
    ("Trailing bytes: {}", "cp_trailing_bytes", "P"),
    ("GUID entries: {}", "cp_guid_entries", "P"),
    ("GUID paths: {} literals / {} indices / {} resolved",
     "cp_literal_paths cp_indexed_paths cp_resolved_path_indices", "P"),
    ("Group records: {}", "cp_group_records", "P"),
    ("Exported fields: {}", "cp_exported_fields", "P"),
    ("Frames: {}", "cp_frames", "P"),
    ("Frame packets: {}", "cp_frame_packets", "P"),
    ("Checkpoint frame skips: {} external blobs / {} external bytes / {} game-specific bytes",
     "cp_frame_external_data_blobs cp_frame_external_data_bytes cp_frame_game_specific_bytes",
     "P"),
    ("Checkpoint frame times: {} non-finite", "cp_frame_non_finite_times", "P"),
    ("Checkpoint rows: {}", "cp_field_rows", "P"),
    ("Checkpoint actors:{} rows", "cp_actor_rows_written", "P"),
    ("Checkpoint GUID rows: {}", "cp_net_guid_rows_written", "P"),
    ("Checkpoint blocks:{} rows", "cp_block_rows_written", "P"),
    ("Checkpoint GUID entries: {} rows", "cp_guid_entry_rows_written", "P"),
    ("Checkpoint export groups: {} rows", "cp_export_group_rows_written", "P"),
    ("Checkpoint export fields: {} rows", "cp_export_field_rows_written", "P"),
    ("Overlay: {} decoded / {} errors / {} raw-skip / {} not-in-table / {} unnamed / "
     "{} conflicts / {} effect blobs",
     "cp_overlay_decoded_ok cp_overlay_decode_errors cp_overlay_raw_skip "
     "cp_overlay_not_in_table cp_overlay_no_field_name cp_overlay_handle_conflicts_refused "
     "cp_effect_blobs_decoded", "G"),
    ("Checkpoint blobs: {} decoded / {} failed", "cp_struct_blobs_decoded cp_struct_blobs_failed",
     "PG"),
    ("Checkpoint fails: {} array / {} truncated RPC / {} movement",
     "cp_array_errors cp_truncated_rpcs cp_movement_rpc_errors", "G"),
    ("Checkpoint array: {} elements / {} fields / {} truncations / {} root bits / "
     "{} nested bits / {} implicit ends",
     "cp_array_elements_decoded cp_array_fields_emitted cp_array_truncations "
     "cp_array_unconsumed_root_bits cp_array_unconsumed_nested_bits "
     "cp_array_implicit_terminations", "G"),
    ("Checkpoint leaf: {} typed decode errors", "cp_array_leaf_decode_errors", "G"),
    ("Checkpoint targets: {} array children", "cp_targeting_world_locations_decoded", "P"),
    ("Checkpoint reward opaque: {} empty variants",
     "cp_tracked_rewards_opaque_empty_variants", "PG"),
    ("Checkpoint ActiveBlinds trailers: {} empty deltas", "cp_active_blinds_empty_trailers",
     "PG"),
    ("Checkpoint movement tails: {} sized ({} bits) / {} open ({} bits)",
     "cp_movement_sized_section_tails cp_movement_sized_section_tail_bits "
     "cp_movement_open_section_tails cp_movement_open_section_tail_bits", "PG"),
    ("Checkpoint envelope trailers: {} streams / {} bits",
     "cp_movement_envelope_trailers cp_movement_envelope_trailer_bits", "PG"),
    ("Checkpoint CNC: {} RPC rows", "cp_cnc_rpcs_emitted", "P"),
    ("Checkpoint CNC brute force: {} attempted / {} unwalked",
     "cp_cnc_bruteforce_payloads_attempted cp_cnc_bruteforce_payloads_unwalked", "PG"),
    ("Checkpoint tails: {} decoded / {} preserved",
     "cp_rep_layout_cnc_tails_decoded cp_rep_layout_cnc_tails_preserved", "P"),
    # `=== Type overlay ===`: Rows offered = the five buckets above it.
    ("Decoded OK: {}", "overlay_decoded_ok", "PG"),
    ("Decode errors: {}", "overlay_decode_errors", "PG"),
    ("Raw/Skip: {}", "overlay_raw_skip", "PG"),
    ("Not in table: {}", "overlay_not_in_table", "PG"),
    ("No field name: {}", "overlay_no_field_name", "PG"),
    ("Rows offered: {total}", "overlay_rows_offered", "PG"),
    # Outside the ratio: the buckets are decided before the effect pass runs.
    ("Effect blobs: {effect_blobs_decoded}", "effect_blobs_decoded", "P"),
)

PLACEHOLDER = re.compile(r"\{\w*\}")


class Line(NamedTuple):
    fmt: str
    keys: tuple[str, ...]
    readers: str
    pattern: re.Pattern[str]


def _pattern(fmt: str) -> re.Pattern[str]:
    body = r"(\d+)".join(re.escape(part).replace(r"\ ", r"\s+")
                         for part in PLACEHOLDER.split(fmt))
    return re.compile(rf"(?m)^\s*{body}\s*$")


SPEC = tuple(Line(fmt, tuple(keys.split()), readers, _pattern(fmt))
             for fmt, keys, readers in LINES)
#: key -> (its line, its placeholder index).
WHERE = {key: (line, i) for line in SPEC for i, key in enumerate(line.keys)}
assert len(WHERE) == sum(len(line.keys) for line in SPEC), "a key is on two lines"


def lines(reader: str, checkpoint: bool) -> tuple[Line, ...]:
    """The lines `reader` reads in one block."""
    return tuple(line for line in SPEC
                 if reader in line.readers and line.keys[0].startswith("cp_") == checkpoint)


def keys(reader: str, checkpoint: bool) -> tuple[str, ...]:
    return tuple(key for line in lines(reader, checkpoint) for key in line.keys)


def read(text: str, wanted) -> dict[str, int | None]:
    """`{key: int}` for each of `wanted`, None where its line was not printed."""
    found: dict[Line, re.Match[str] | None] = {}
    out = {}
    for key in wanted:
        line, index = WHERE[key]
        if line not in found:
            found[line] = line.pattern.search(text)
        match = found[line]
        out[key] = None if match is None else int(match.group(index + 1))
    return out


def label(key: str) -> str:
    """How a message names `key`: its line's label and the words after its
    number ("Array decode errors"), prefixed "Checkpoint" in that block."""
    line, index = WHERE[key]
    parts = PLACEHOLDER.split(line.fmt)
    unit = re.split(r"[/()]", parts[index + 1])[0].strip()
    name = parts[0].strip().rstrip(":") + (f" {unit}" if unit else "")
    return f"Checkpoint {name}" if key.startswith("cp_") and not name.startswith("Checkpoint") \
        else name


def render(line: Line, values: dict[str, int]) -> str:
    """`line` as summary.rs prints it, with `values` and thousands separators."""
    parts = PLACEHOLDER.split(line.fmt)
    return parts[0] + "".join(f"{values[key]:,}{part}" for key, part in zip(line.keys, parts[1:]))


def tail(text: str, lines: int = 3, width: int = 200) -> str:
    """The last non-blank lines of a process's output, for an error message."""
    return " | ".join(l for l in text.splitlines()[-lines:] if l.strip())[:width]
