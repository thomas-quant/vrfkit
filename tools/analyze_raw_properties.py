"""Measure preserved, uninterpreted replicated-property payloads.

This is a corpus instrument, not a decoder.  It deliberately does not assign
names or types to unnamed fields.  A handle, compatible checksum, bit width,
and containing export-group path are enough to measure whether the same wire
shape recurs; they are not enough to claim what the property means.

The corpus can be large, so the tool never retains a corpus worth of Parquet
files.  It exports one replay into a private temporary directory, streams only
the columns needed from ``fields.parquet``, merges the aggregate, and removes
the directory before moving to the next replay.  Peak temporary storage is one
export.  The default is a deterministic size-stratified sample; pass ``--all``
when an exhaustive sweep is actually intended.

Output is identifier-redacted by construction.  It contains build labels,
counts, bit widths, and anonymous ranks only.  Replay paths and filenames,
friendly names, group paths, actor/object/channel IDs, handles, compatible
checksums, and raw payload bytes or hashes are never printed.

Usage::

    python tools/analyze_raw_properties.py \
        ./target/release/vrfkit /path/to/corpus
    python tools/analyze_raw_properties.py \
        ./target/release/vrfkit /path/to/corpus \
        --build 13.02 --build 13.04 --limit-per-build 8
    python tools/analyze_raw_properties.py \
        ./target/release/vrfkit /path/to/corpus --all

An unnamed property row without ``raw_bits``, or whose byte length cannot hold
its declared ``bit_count`` exactly, is an integrity failure and makes the
command exit 1.  Such a row cannot be interpreted today *or* recovered by a
future decoder, violating vrfkit's no-silent-loss invariant.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
import sys
import tempfile
from collections import Counter, defaultdict
from dataclasses import dataclass, field
from pathlib import Path
from typing import Iterable

import pyarrow as pa
import pyarrow.compute as pc
import pyarrow.parquet as pq

import corpus_scan


CLASS_NET_CACHE_SUFFIX = "_ClassNetCache"
DEFAULT_BUILDS = ("13.02", "13.04", "13.05")
BUILD_PATTERN = re.compile(r"(?:release-)(\d+\.\d+)")
FIELD_COLUMNS = (
    "packet_id",
    "channel_index",
    "actor_net_guid",
    "object_net_guid",
    "group_path",
    "handle",
    "field_name",
    "compatible_checksum",
    "bit_count",
    "raw_bits",
    "value_i64",
    "value_f64",
    "value_bool",
    "value_str",
)
TYPED_COLUMNS = ("value_i64", "value_f64", "value_bool", "value_str")


# Structural keys are intentionally private implementation details.  They are
# used to compare rows, then discarded.  render_report never includes one.
FieldShape = tuple[str, int, int | None, int]
BlockShape = tuple[str, tuple[tuple[int, int | None, int], ...]]


@dataclass
class Recurrence:
    """Occurrence and payload-variation evidence for one structural shape."""

    rows: int = 0
    replay_ordinals: set[int] = field(default_factory=set)
    builds: set[str] = field(default_factory=set)
    rows_by_build: Counter[str] = field(default_factory=Counter)
    first_payload_digest: bytes | None = None
    payload_varied: bool = False

    def observe(
        self, payload: bytes, replay_ordinal: int, build: str,
    ) -> None:
        digest = hashlib.blake2b(payload, digest_size=16).digest()
        if self.first_payload_digest is None:
            self.first_payload_digest = digest
        elif digest != self.first_payload_digest:
            self.payload_varied = True
        self.rows += 1
        self.replay_ordinals.add(replay_ordinal)
        self.builds.add(build)
        self.rows_by_build[build] += 1


@dataclass
class BlockRecurrence:
    blocks: int = 0
    replay_ordinals: set[int] = field(default_factory=set)
    builds: set[str] = field(default_factory=set)
    blocks_by_build: Counter[str] = field(default_factory=Counter)

    def observe(self, replay_ordinal: int, build: str) -> None:
        self.blocks += 1
        self.replay_ordinals.add(replay_ordinal)
        self.builds.add(build)
        self.blocks_by_build[build] += 1


@dataclass
class Inventory:
    """Aggregate with no source paths or filenames retained."""

    replay_count: int = 0
    build_replays: Counter[str] = field(default_factory=Counter)
    field_rows: Counter[str] = field(default_factory=Counter)
    property_rows: Counter[str] = field(default_factory=Counter)
    property_raw_only_rows: Counter[str] = field(default_factory=Counter)
    named_raw_only_rows: Counter[str] = field(default_factory=Counter)
    unnamed_rows: Counter[str] = field(default_factory=Counter)
    unnamed_raw_rows: Counter[str] = field(default_factory=Counter)
    unnamed_typed_rows: Counter[str] = field(default_factory=Counter)
    unnamed_without_raw_rows: Counter[str] = field(default_factory=Counter)
    unnamed_wrong_length_rows: Counter[str] = field(default_factory=Counter)
    unnamed_checksum_rows: Counter[str] = field(default_factory=Counter)
    unnamed_sentinel_handle_rows: Counter[str] = field(default_factory=Counter)
    unnamed_zero_rows: Counter[str] = field(default_factory=Counter)
    unnamed_byte_aligned_rows: Counter[str] = field(default_factory=Counter)
    unnamed_widths: dict[str, Counter[int]] = field(
        default_factory=lambda: defaultdict(Counter)
    )
    signatures: dict[FieldShape, Recurrence] = field(default_factory=dict)
    block_shapes: dict[BlockShape, BlockRecurrence] = field(default_factory=dict)

    @property
    def integrity_failures(self) -> int:
        return (
            sum(self.unnamed_without_raw_rows.values())
            + sum(self.unnamed_wrong_length_rows.values())
        )


@dataclass(frozen=True)
class ReplayCandidate:
    path: Path
    build: str
    size: int


def parse_build(text: str) -> str | None:
    """Read the public build label from redacted ``vrfkit inspect`` output."""
    match = BUILD_PATTERN.search(text)
    return match.group(1) if match else None


def inspect_build(vrfkit: Path, replay: Path) -> str | None:
    """Inspect one header without allowing a private path into diagnostics."""
    result = subprocess.run(
        [str(vrfkit), "inspect", str(replay), "--redact-identifiers"],
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        return None
    return parse_build(result.stdout)


def stratified_sample(
    candidates: Iterable[ReplayCandidate], limit: int,
) -> list[ReplayCandidate]:
    """Deterministically span the observed file-size range.

    Size is only a sampling coordinate; neither sizes nor source identifiers
    are printed.  Selecting the first N sorted filenames would overrepresent a
    coincidental filename prefix and selecting the newest N would overrepresent
    one capture session.
    """
    ordered = sorted(candidates, key=lambda item: (item.size, item.path.name))
    if limit >= len(ordered):
        return ordered
    if limit == 1:
        return [ordered[len(ordered) // 2]]
    indices = [round(i * (len(ordered) - 1) / (limit - 1)) for i in range(limit)]
    return [ordered[index] for index in indices]


def _payload_is_zero(payload: bytes, bit_count: int) -> bool:
    """Whether every meaningful payload bit is zero.

    The exporter already clears padding bits, but masking the final byte keeps
    this measurement correct for third-party Parquet files that preserve
    arbitrary padding.
    """
    full_bytes, remaining_bits = divmod(bit_count, 8)
    if any(payload[:full_bytes]):
        return False
    if remaining_bits and len(payload) > full_bytes:
        return (payload[full_bytes] & ((1 << remaining_bits) - 1)) == 0
    return True


def analyze_export(
    export_dir: Path, inventory: Inventory, replay_ordinal: int,
) -> str:
    """Stream one completed export into ``inventory`` and return its build."""
    manifest_path = export_dir / "manifest.json"
    fields_path = export_dir / "fields.parquet"
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    build = parse_build(str(manifest.get("replay_build", "")))
    if build is None:
        raise ValueError("manifest has no release build label")

    parquet = pq.ParquetFile(fields_path)
    missing = set(FIELD_COLUMNS) - set(parquet.schema_arrow.names)
    if missing:
        raise ValueError(
            f"fields.parquet is missing {len(missing)} required column(s)"
        )

    # The adapter uses this exact key for one property update.  Actor and object
    # IDs are necessary only while grouping this replay; they are never copied
    # into Inventory and therefore cannot appear in the report.
    unnamed_blocks: dict[
        tuple[int, int, int, int | None, str],
        list[tuple[int, int | None, int]],
    ] = defaultdict(list)

    def add(counter: Counter[str], mask) -> None:
        # Only a nonzero count creates the build's key, as a per-row += 1 did.
        count = pc.sum(pc.cast(mask, pa.int64())).as_py() or 0
        if count:
            counter[build] += count

    def present(column):
        # Decoded first: a dictionary column's null can sit in its values.
        if pa.types.is_dictionary(column.type):
            column = pc.cast(column, pa.string())
        return pc.is_valid(column)

    # Only unnamed replicated-property rows with a payload need per-row work
    # (1,623 of 1,648,356 rows on a 13.06 export), so the other counters are
    # Arrow mask sums and only those rows reach Python, in physical order.
    for batch in parquet.iter_batches(columns=list(FIELD_COLUMNS)):
        group_paths = pc.cast(batch.column("group_path"), pa.string())
        if group_paths.null_count:
            # What `CLASS_NET_CACHE_SUFFIX in None` raised row by row.
            raise TypeError("argument of type 'NoneType' is not iterable")
        if batch.num_rows:
            inventory.field_rows[build] += batch.num_rows
        prop = pc.invert(pc.match_substring(group_paths, CLASS_NET_CACHE_SUFFIX))
        typed = present(batch.column(TYPED_COLUMNS[0]))
        for name in TYPED_COLUMNS[1:]:
            typed = pc.or_(typed, present(batch.column(name)))
        named = present(batch.column("field_name"))
        has_raw = present(batch.column("raw_bits"))
        raw_only = pc.and_(prop, pc.and_(has_raw, pc.invert(typed)))
        unnamed = pc.and_(prop, pc.invert(named))
        add(inventory.property_rows, prop)
        add(inventory.property_raw_only_rows, raw_only)
        add(inventory.named_raw_only_rows, pc.and_(raw_only, named))
        add(inventory.unnamed_rows, unnamed)
        add(inventory.unnamed_typed_rows, pc.and_(unnamed, typed))
        add(inventory.unnamed_without_raw_rows, pc.and_(unnamed, pc.invert(has_raw)))

        selected = batch.filter(pc.and_(unnamed, has_raw))
        columns = selected.to_pydict()
        for row in range(selected.num_rows):
            group_path = columns["group_path"][row]
            raw = columns["raw_bits"][row]
            inventory.unnamed_raw_rows[build] += 1
            bit_count = int(columns["bit_count"][row])
            if len(raw) != (bit_count + 7) // 8:
                inventory.unnamed_wrong_length_rows[build] += 1
            inventory.unnamed_widths[build][bit_count] += 1
            if bit_count % 8 == 0:
                inventory.unnamed_byte_aligned_rows[build] += 1
            if _payload_is_zero(raw, bit_count):
                inventory.unnamed_zero_rows[build] += 1

            handle = int(columns["handle"][row])
            checksum = columns["compatible_checksum"][row]
            if checksum is not None:
                inventory.unnamed_checksum_rows[build] += 1
            if handle == 2**32 - 1:
                inventory.unnamed_sentinel_handle_rows[build] += 1
            signature = (group_path, handle, checksum, bit_count)
            recurrence = inventory.signatures.setdefault(signature, Recurrence())
            recurrence.observe(raw, replay_ordinal, build)

            block_key = (
                int(columns["packet_id"][row]),
                int(columns["channel_index"][row]),
                int(columns["actor_net_guid"][row]),
                columns["object_net_guid"][row],
                group_path,
            )
            unnamed_blocks[block_key].append((handle, checksum, bit_count))

    for block_key, fields in unnamed_blocks.items():
        shape = (block_key[-1], tuple(fields))
        recurrence = inventory.block_shapes.setdefault(shape, BlockRecurrence())
        recurrence.observe(replay_ordinal, build)

    inventory.replay_count += 1
    inventory.build_replays[build] += 1
    return build


def export_and_analyze(
    vrfkit: Path,
    replay: ReplayCandidate,
    inventory: Inventory,
    replay_ordinal: int,
) -> None:
    """Export exactly one replay and delete it after streaming the fields."""
    with tempfile.TemporaryDirectory(prefix="vrfkit-raw-inventory-") as temp:
        output = Path(temp) / "export"
        result = subprocess.run(
            [str(vrfkit), "export", str(replay.path), "--out", str(output)],
            capture_output=True,
            text=True,
            check=False,
        )
        if result.returncode != 0:
            # Do not include captured stdout/stderr: parsers often echo source
            # paths in I/O failures, and this report promises not to expose one.
            raise RuntimeError(f"export exited {result.returncode}")
        build = analyze_export(output, inventory, replay_ordinal)
        if build != replay.build:
            raise RuntimeError("build changed between inspect and export")


def _percent(part: int, whole: int) -> str:
    return "0.00%" if whole == 0 else f"{part / whole * 100:.2f}%"


def summary_document(
    inventory: Inventory,
    eligible_by_build: Counter[str],
    selected_by_build: Counter[str],
    excluded: int,
    recursive: bool,
) -> dict:
    """Return the stable, identifier-free machine-readable report.

    The schema is intentionally aggregate-only.  In particular, neither the
    structural keys in ``inventory.signatures`` nor hashes derived from those
    keys appear here.  A hash would still be a persistent pseudonymous
    identifier and would break the report's privacy contract.
    """
    build_documents = {}
    for build in sorted(eligible_by_build):
        preserved = inventory.unnamed_raw_rows[build]
        zero = inventory.unnamed_zero_rows[build]
        aligned = inventory.unnamed_byte_aligned_rows[build]
        build_documents[build] = {
            "eligible_replays": eligible_by_build[build],
            "analyzed_replays": selected_by_build[build],
            "field_rows": inventory.field_rows[build],
            "replicated_property_rows": inventory.property_rows[build],
            "raw_only_property_rows": inventory.property_raw_only_rows[build],
            "named_raw_only_rows": inventory.named_raw_only_rows[build],
            "unnamed_rows": inventory.unnamed_rows[build],
            "unnamed_raw_rows": preserved,
            "unnamed_typed_rows": inventory.unnamed_typed_rows[build],
            "unnamed_missing_raw_rows": inventory.unnamed_without_raw_rows[build],
            "unnamed_wrong_raw_length_rows": inventory.unnamed_wrong_length_rows[build],
            "unnamed_checksum_rows": inventory.unnamed_checksum_rows[build],
            "unnamed_sentinel_handle_rows": inventory.unnamed_sentinel_handle_rows[build],
            "unnamed_zero_payload_rows": zero,
            "unnamed_nonzero_payload_rows": preserved - zero,
            "unnamed_byte_aligned_rows": aligned,
            "unnamed_non_byte_aligned_rows": preserved - aligned,
            "unnamed_widths_bits": {
                str(bits): count
                for bits, count in sorted(inventory.unnamed_widths[build].items())
            },
        }

    signatures = list(inventory.signatures.values())
    shapes = list(inventory.block_shapes.values())
    selected_builds = sorted(selected_by_build)
    per_build = {}
    for build in selected_builds:
        signature_rows = sum(item.rows_by_build[build] for item in signatures)
        shared_signature_rows = sum(
            item.rows_by_build[build]
            for item in signatures
            if len(item.builds) > 1
        )
        layout_updates = sum(item.blocks_by_build[build] for item in shapes)
        shared_layout_updates = sum(
            item.blocks_by_build[build]
            for item in shapes
            if len(item.builds) > 1
        )
        per_build[build] = {
            "field_signatures": sum(
                item.rows_by_build[build] > 0 for item in signatures
            ),
            "signature_rows": signature_rows,
            "rows_using_cross_build_signature": shared_signature_rows,
            "layouts": sum(item.blocks_by_build[build] > 0 for item in shapes),
            "layout_updates": layout_updates,
            "updates_using_cross_build_layout": shared_layout_updates,
        }

    ranked_shapes = sorted(
        inventory.block_shapes.items(), key=lambda item: item[1].blocks, reverse=True
    )[:12]
    recurrence = {
        "field_signatures": len(signatures),
        "rows_in_repeated_signatures": sum(
            item.rows for item in signatures if item.rows > 1
        ),
        "signatures_seen_in_multiple_replays": sum(
            len(item.replay_ordinals) > 1 for item in signatures
        ),
        "signatures_seen_in_multiple_builds": sum(
            len(item.builds) > 1 for item in signatures
        ),
        "repeated_constant_payload_signatures": sum(
            item.rows > 1 and not item.payload_varied for item in signatures
        ),
        "varying_payload_signatures": sum(item.payload_varied for item in signatures),
        "property_updates_with_unnamed_rows": sum(item.blocks for item in shapes),
        "distinct_unnamed_layouts": len(shapes),
        "updates_in_repeated_layouts": sum(
            item.blocks for item in shapes if item.blocks > 1
        ),
        "layouts_seen_in_multiple_replays": sum(
            len(item.replay_ordinals) > 1 for item in shapes
        ),
        "layouts_seen_in_multiple_builds": sum(
            len(item.builds) > 1 for item in shapes
        ),
        "per_build": per_build,
        "top_anonymous_layouts": [
            {
                "rank": rank,
                "updates": recurrence.blocks,
                "fields_per_update": len(shape[1]),
                "replays": len(recurrence.replay_ordinals),
                "builds": len(recurrence.builds),
            }
            for rank, (shape, recurrence) in enumerate(ranked_shapes, 1)
        ],
    }
    return {
        "schema_version": 1,
        "identifier_redacted": True,
        "typing_inference_performed": False,
        "scope": {
            "recursive": recursive,
            "subdirectory_replays_excluded": excluded,
            "eligible_replays": sum(eligible_by_build.values()),
            "analyzed_replays": sum(selected_by_build.values()),
        },
        "builds": build_documents,
        "structural_recurrence": recurrence,
        "integrity": {
            "passed": inventory.integrity_failures == 0,
            "payload_preservation_violations": inventory.integrity_failures,
        },
    }


def render_report(
    inventory: Inventory,
    eligible_by_build: Counter[str],
    selected_by_build: Counter[str],
    excluded: int,
    recursive: bool,
) -> str:
    """Render aggregate-only output; structural keys never leave this function."""
    lines = [
        "=== Raw replicated-property inventory (identifier-redacted) ===",
        (
            f"corpus scope: {sum(eligible_by_build.values())} eligible replay(s); "
            f"{excluded} replay(s) excluded by non-recursive discovery"
            if not recursive
            else f"corpus scope: {sum(eligible_by_build.values())} eligible replay(s); recursive"
        ),
    ]
    for build in sorted(eligible_by_build):
        lines.append(
            f"release-{build}: {eligible_by_build[build]} eligible, "
            f"{selected_by_build[build]} analyzed"
        )

    lines.append("")
    for build in sorted(selected_by_build):
        rows = inventory.property_rows[build]
        raw = inventory.property_raw_only_rows[build]
        unnamed = inventory.unnamed_rows[build]
        preserved = inventory.unnamed_raw_rows[build]
        zero = inventory.unnamed_zero_rows[build]
        aligned = inventory.unnamed_byte_aligned_rows[build]
        widths = inventory.unnamed_widths[build]
        lines.extend(
            [
                f"=== release-{build} aggregate ===",
                f"replays: {inventory.build_replays[build]}",
                f"field rows: {inventory.field_rows[build]}",
                f"replicated-property rows: {rows}",
                f"raw-only property rows: {raw} ({_percent(raw, rows)})",
                (
                    f"named raw-only / unnamed rows: "
                    f"{inventory.named_raw_only_rows[build]} / {unnamed}"
                ),
                (
                    f"unnamed rows preserving raw_bits: {preserved} "
                    f"({_percent(preserved, unnamed)})"
                ),
                (
                    f"unnamed typed / missing raw_bits: "
                    f"{inventory.unnamed_typed_rows[build]} / "
                    f"{inventory.unnamed_without_raw_rows[build]}"
                ),
                (
                    f"unnamed raw_bits with wrong byte length: "
                    f"{inventory.unnamed_wrong_length_rows[build]}"
                ),
                (
                    f"unnamed rows with compatible checksum / sentinel handle: "
                    f"{inventory.unnamed_checksum_rows[build]} / "
                    f"{inventory.unnamed_sentinel_handle_rows[build]}"
                ),
                (
                    f"unnamed zero / nonzero payload rows: {zero} / "
                    f"{preserved - zero}"
                ),
                (
                    f"unnamed byte-aligned / non-byte-aligned rows: {aligned} / "
                    f"{preserved - aligned}"
                ),
                "top unnamed widths (bits:rows): "
                + (", ".join(f"{bits}:{count}" for bits, count in widths.most_common(12))
                   or "none"),
                "",
            ]
        )

    # The recurrence figures are the JSON document's; only the per-build
    # aggregates above read the inventory, so ties keep its insertion order.
    recurrence = summary_document(
        inventory, eligible_by_build, selected_by_build, excluded, recursive
    )["structural_recurrence"]
    layouts = recurrence["top_anonymous_layouts"]
    lines.extend(
        [
            "=== Anonymous structural recurrence ===",
            f"field signatures: {recurrence['field_signatures']}",
            f"rows in repeated signatures: {recurrence['rows_in_repeated_signatures']}",
            (
                f"signatures seen in multiple replays / builds: "
                f"{recurrence['signatures_seen_in_multiple_replays']} / "
                f"{recurrence['signatures_seen_in_multiple_builds']}"
            ),
            (
                f"repeated signatures with constant / varying payload: "
                f"{recurrence['repeated_constant_payload_signatures']} / "
                f"{recurrence['varying_payload_signatures']}"
            ),
            (
                f"property updates containing unnamed rows: "
                f"{recurrence['property_updates_with_unnamed_rows']}"
            ),
            f"distinct unnamed layouts: {recurrence['distinct_unnamed_layouts']}",
            f"updates in repeated layouts: {recurrence['updates_in_repeated_layouts']}",
            (
                f"layouts seen in multiple replays / builds: "
                f"{recurrence['layouts_seen_in_multiple_replays']} / "
                f"{recurrence['layouts_seen_in_multiple_builds']}"
            ),
            "per-build structural reuse:",
            *(
                f"  release-{build}: {item['field_signatures']} signatures; "
                f"{item['rows_using_cross_build_signature']}/"
                f"{item['signature_rows']} rows use a cross-build signature; "
                f"{item['layouts']} layouts; "
                f"{item['updates_using_cross_build_layout']}/"
                f"{item['layout_updates']} updates use a cross-build layout"
                for build, item in recurrence["per_build"].items()
            ),
            "top anonymous layouts (rank:updates,fields,replays,builds):",
            *(
                f"  {layout['rank']}:{layout['updates']},{layout['fields_per_update']},"
                f"{layout['replays']},{layout['builds']}"
                for layout in layouts
            ),
        ]
    )
    if not layouts:
        lines.append("  none")
    lines.extend(
        [
            "",
            (
                "integrity: FAIL -- unnamed property payload preservation "
                f"violations: {inventory.integrity_failures}"
                if inventory.integrity_failures
                else (
                    "integrity: PASS -- every unnamed property row preserved "
                    "exact-length raw_bits"
                )
            ),
            (
                "typing note: recurrence is structural evidence only; no field name or "
                "type is inferred."
            ),
        ]
    )
    return "\n".join(lines)


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("vrfkit", type=Path)
    parser.add_argument("corpus", type=Path)
    parser.add_argument(
        "--build",
        action="append",
        dest="builds",
        help=f"release label to include; repeatable (default: {', '.join(DEFAULT_BUILDS)})",
    )
    parser.add_argument("--recursive", action="store_true")
    selection = parser.add_mutually_exclusive_group()
    selection.add_argument(
        "--limit-per-build",
        type=int,
        default=6,
        help="deterministic size-stratified sample per build (default: 6)",
    )
    selection.add_argument(
        "--all",
        action="store_true",
        help="analyze every replay in the selected builds",
    )
    parser.add_argument(
        "--format",
        choices=("text", "json"),
        default="text",
        help="aggregate report format (default: text)",
    )
    args = parser.parse_args(argv)
    if args.limit_per_build is not None and args.limit_per_build < 1:
        parser.error("--limit-per-build must be at least 1")
    if any(re.fullmatch(r"\d+\.\d+", build) is None for build in args.builds or ()):
        parser.error("--build must be a numeric release label such as 13.04")
    return args


def main(argv: list[str] | None = None) -> int:
    args = parse_args(sys.argv[1:] if argv is None else argv)
    builds = tuple(dict.fromkeys(args.builds or DEFAULT_BUILDS))
    if not args.vrfkit.is_file():
        print("ERROR: vrfkit executable not found", file=sys.stderr)
        return 2
    if not args.corpus.is_dir():
        print("ERROR: corpus directory not found", file=sys.stderr)
        return 2

    scan = corpus_scan.discover(args.corpus, recursive=args.recursive)
    candidates: dict[str, list[ReplayCandidate]] = defaultdict(list)
    unreadable = 0
    for replay in scan.files:
        build = inspect_build(args.vrfkit, replay)
        if build is None:
            unreadable += 1
            continue
        if build in builds:
            candidates[build].append(
                ReplayCandidate(replay, build, replay.stat().st_size)
            )

    eligible = Counter({build: len(candidates[build]) for build in builds})
    selected: list[ReplayCandidate] = []
    for build in builds:
        build_candidates = candidates[build]
        chosen = (
            sorted(build_candidates, key=lambda item: item.path.name)
            if args.all
            else stratified_sample(build_candidates, args.limit_per_build)
        )
        selected.extend(chosen)
    selected.sort(key=lambda item: (item.build, item.size, item.path.name))
    selected_counts = Counter(item.build for item in selected)

    if unreadable:
        print(
            f"ERROR: {unreadable} replay header(s) unreadable; identifiers redacted",
            file=sys.stderr,
        )
        return 1
    if not selected:
        print("ERROR: no replay matched the selected builds", file=sys.stderr)
        return 2

    inventory = Inventory()
    for ordinal, replay in enumerate(selected, 1):
        try:
            export_and_analyze(args.vrfkit, replay, inventory, ordinal)
        # This is the privacy boundary.  A library exception can include the
        # source replay path or field metadata in its message, so no exception
        # is allowed to escape into a traceback.  The type plus a run-local
        # ordinal still makes the failure visible and the process exits 1.
        except Exception as error:
            print(
                f"ERROR: replay-{ordinal:04d}: {type(error).__name__}; "
                "source identifier redacted",
                file=sys.stderr,
            )
            return 1

    report_args = dict(
        inventory=inventory,
        eligible_by_build=eligible,
        selected_by_build=selected_counts,
        excluded=scan.excluded,
        recursive=scan.recursive,
    )
    if args.format == "json":
        print(json.dumps(summary_document(**report_args), indent=2, sort_keys=True))
    else:
        print(
            render_report(
                **report_args,
            )
        )
    return 1 if inventory.integrity_failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
