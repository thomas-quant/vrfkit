"""Check that pyarrow reads the Parquet files the Rust write_interop_files test
writes: row counts, column names and order, types and nullability."""
import os
import sys
from pathlib import Path

# The check marks below need UTF-8 on a Windows console.
sys.stdout.reconfigure(encoding='utf-8')

import pyarrow
import pyarrow.parquet as pq
import pyarrow.types

# Only the exact directory the Rust test wrote: the newest temp directory can
# be another checkout's stale fixture. argv[1] is that directory (as CI passes
# it); VRFKIT_INTEROP_DIR is the root whose `interop` child holds the files.
def _find_interop_dir() -> Path:
    if len(sys.argv) > 1:
        return Path(sys.argv[1]).resolve()
    configured = os.environ.get("VRFKIT_INTEROP_DIR")
    if configured:
        return (Path(configured) / "interop").resolve()
    sys.exit(
        "explicit interop directory required: pass the fixture directory as "
        "argv[1], or set VRFKIT_INTEROP_DIR to the root the Rust "
        "write_interop_files test wrote under"
    )


INTEROP_DIR = _find_interop_dir()
FIELDS_PATH = INTEROP_DIR / "fields_interop.parquet"
MOVEMENT_PATH = INTEROP_DIR / "movement_interop.parquet"


class CheckFailed(Exception):
    """A correctness gate in this script did not hold."""


def check(condition, message):
    """Gate that survives `python -O` and PYTHONOPTIMIZE, which compile
    `assert` out (it once let this script pass files it never checked)."""
    if not condition:
        raise CheckFailed(message)


def _assert_gates_are_live():
    """Prove `check` still fails before trusting anything it reports."""
    try:
        check(False, "self-test")
    except CheckFailed:
        return
    raise SystemExit("FATAL: check() did not raise -- the gates below are not enforced")

def verify_fields():
    print("=" * 60)
    print("FIELDS TABLE VERIFICATION")
    print("=" * 60)

    table = pq.read_table(str(FIELDS_PATH))
    schema = table.schema

    print(f"\nRow count: {table.num_rows}")
    check(table.num_rows == 10_000, f"Expected 10000 rows, got {table.num_rows}")
    print("  ✓ Row count matches (10,000)")

    print(f"\nColumn count: {len(schema)}")
    expected_cols = [
        "time_ms", "packet_id", "channel_index", "actor_net_guid",
        "object_net_guid",
        "group_path", "handle", "field_name",
        "compatible_checksum",
        "bit_count",
        "raw_bits", "value_i64", "value_f64", "value_bool", "value_str",
    ]
    actual_cols = [f.name for f in schema]
    check(
        actual_cols == expected_cols,
        f"Column mismatch:\n  expected: {expected_cols}\n  actual:   {actual_cols}",
    )
    print("  ✓ Column names match")

    print("\nSchema:")
    for field in schema:
        print(f"  {field.name:20s}  {str(field.type):30s}  nullable={field.nullable}")

    gp_type = schema.field("group_path").type
    check(
        pyarrow.types.is_dictionary(gp_type),
        f"group_path should be dictionary-encoded, got {gp_type}",
    )
    print("\n  ✓ group_path is dictionary-encoded")

    fn_type = schema.field("field_name").type
    check(
        pyarrow.types.is_dictionary(fn_type),
        f"field_name should be dictionary-encoded, got {fn_type}",
    )
    print("  ✓ field_name is dictionary-encoded")

    check(schema.field("field_name").nullable, "field_name should be nullable")
    check(schema.field("raw_bits").nullable, "raw_bits should be nullable")
    check(schema.field("value_i64").nullable, "value_i64 should be nullable")
    print("  ✓ Nullable columns are correct")

    fn_col = table.column("field_name")
    null_count = fn_col.null_count
    print(f"\n  field_name null count: {null_count} / {table.num_rows}")
    check(null_count > 0, "Expected some null field_names")
    print("  ✓ Null values present where expected")

    file_size = os.path.getsize(FIELDS_PATH)
    print(f"\n  Parquet file size: {file_size:,} bytes ({file_size/1024:.1f} KB)")


def verify_movement():
    print("\n" + "=" * 60)
    print("MOVEMENT TABLE VERIFICATION")
    print("=" * 60)

    table = pq.read_table(str(MOVEMENT_PATH))
    schema = table.schema

    print(f"\nRow count: {table.num_rows}")
    check(table.num_rows == 50_000, f"Expected 50000 rows, got {table.num_rows}")
    print("  ✓ Row count matches (50,000)")

    expected_cols = [
        "time_ms", "packet_id", "character_net_guid",
        "pos_x", "pos_y", "pos_z", "yaw", "pitch",
        "vel_x", "vel_y", "vel_z",
        # Appended after vel_z, not interleaved: consumers read by position.
        "timestamp", "movement_state", "move_type",
    ]
    actual_cols = [f.name for f in schema]
    check(
        actual_cols == expected_cols,
        f"Column mismatch:\n  expected: {expected_cols}\n  actual:   {actual_cols}",
    )
    print("  ✓ Column names match")

    print("\nSchema:")
    for field in schema:
        print(f"  {field.name:24s}  {str(field.type):15s}  nullable={field.nullable}")

    for field in schema:
        check(not field.nullable, f"{field.name} should not be nullable")
    print("\n  ✓ No nullable columns (all dense)")

    file_size = os.path.getsize(MOVEMENT_PATH)
    print(f"\n  Parquet file size: {file_size:,} bytes ({file_size/1024:.1f} KB)")


if __name__ == "__main__":
    _assert_gates_are_live()
    print(f"Interop dir: {INTEROP_DIR}")
    print(f"pyarrow version: {pyarrow.__version__}")
    print()

    if not FIELDS_PATH.exists() or not MOVEMENT_PATH.exists():
        print("ERROR: Interop parquet files not found.")
        print("       Run `cargo test -p vrf-export` first.")
        exit(1)

    verify_fields()
    verify_movement()

    print("\n" + "=" * 60)
    print("ALL CHECKS PASSED ✓")
    print("=" * 60)
