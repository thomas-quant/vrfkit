//! Round-trip and stress tests for the Parquet export writers.
//!
//! These tests verify:
//! - Write -> read round-trip preserves all values and nulls.
//! - Rows are cut into row groups at exactly the row-group size.
//! - Binary column data is preserved exactly.
//! - Dictionary-encoded columns round-trip correctly.
//! - Every table's file carries a Parquet dictionary for exactly the columns
//!   its `DICTIONARY_COLUMNS` lists (this module also needs the `partials`
//!   and `checkpoint-context` features).
//!
//! Every test here exercises a writer, so the file is empty unless all five
//! table features are on. That is the default; the gate exists so that
//! `--no-default-features` builds this target instead of failing to resolve
//! writers the build deliberately left out.

#![cfg(all(
    feature = "fields",
    feature = "movement",
    feature = "actors",
    feature = "net-guids",
    feature = "events"
))]

use std::fs;
use std::path::{Path, PathBuf};

use arrow_array::cast::AsArray;
use arrow_array::types::{Float32Type, Int32Type, Int64Type, UInt8Type, UInt32Type};
use arrow_array::{Array, ArrayAccessor, ArrayRef, RecordBatch, StringArray};
use arrow_schema::DataType;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::file::reader::{FileReader, SerializedFileReader};

use vrf_export::{
    ActorRecord, ActorsTable, EventRecord, EventsTable, FieldRecord, FieldWriter, FieldsTable,
    MovementRecord, MovementTable, MovementWriter, NetGuidRecord, NetGuidsTable, Table,
    TableWriter, UNRESOLVED_CLASS_NET_CACHE_PAYLOAD_FIELD_NAME,
};

/// Test output directory -- each test writes to a unique file.
///
/// Keyed by this crate's own source path, so two checkouts of the repository
/// cannot write over each other. They previously shared one directory under
/// the system temp dir, which is not per-checkout: a `cargo test` in a git
/// worktree and one in the main tree write the same filenames, and whichever
/// reads second reads the other's Parquet. That surfaced once already, as a
/// column-count mismatch that looked exactly like a schema bug and was not.
///
/// `CARGO_MANIFEST_DIR` is the discriminator because it differs per worktree
/// and is fixed at compile time, so every test in one binary agrees on it.
/// CI may set `VRFKIT_INTEROP_DIR`; that exact root is used so a following
/// language interop step can consume `<root>/interop` without guessing a hash.
fn test_dir() -> PathBuf {
    let dir = test_dir_from_override(std::env::var_os("VRFKIT_INTEROP_DIR").map(PathBuf::from));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn test_dir_from_override(override_root: Option<PathBuf>) -> PathBuf {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    override_root.unwrap_or_else(|| {
        let mut hasher = DefaultHasher::new();
        env!("CARGO_MANIFEST_DIR").hash(&mut hasher);
        std::env::temp_dir().join(format!("vrf_export_tests_{:016x}", hasher.finish()))
    })
}

#[test]
fn an_explicit_interop_root_is_used_verbatim() {
    let root = PathBuf::from("ci-owned-exact-root");
    assert_eq!(test_dir_from_override(Some(root.clone())), root);
}

/// Helper: build a FieldRecord with predictable values based on an index.
fn make_field_record(i: u32) -> FieldRecord {
    FieldRecord {
        time_ms: i * 10,
        packet_id: i,
        channel_index: i % 8,
        actor_net_guid: 1000 + (i % 20),
        // Every third record models a subobject block.
        object_net_guid: if i % 3 == 0 { None } else { Some(9000 + i) },
        group_path: format!("Group_{}", i % 5).into(),
        handle: i % 64,
        field_name: if i % 3 == 0 {
            None
        } else {
            Some(format!("Field_{}", i % 10).into())
        },
        compatible_checksum: None,
        bit_count: (i % 128) + 1,
        raw_bits: if i % 4 == 0 {
            None
        } else {
            Some(vec![(i & 0xFF) as u8; ((i % 16) + 1) as usize].into())
        },
        value_i64: if i % 5 == 0 {
            Some(i as i64 * 100)
        } else {
            None
        },
        value_f64: if i % 5 == 1 {
            Some(i as f64 * 0.1)
        } else {
            None
        },
        value_bool: if i % 5 == 2 { Some(i % 2 == 0) } else { None },
        value_str: if i % 5 == 3 {
            Some(format!("val_{}", i))
        } else {
            None
        },
    }
}

/// Helper: build a MovementRecord with predictable values.
fn make_movement_record(i: u32) -> MovementRecord {
    MovementRecord {
        time_ms: i * 16,
        packet_id: i / 2,
        character_net_guid: 100 + (i % 10),
        pos_x: i as f32 * 1.5,
        pos_y: i as f32 * 2.0,
        pos_z: 100.0 + (i as f32 * 0.1),
        yaw: ((i % 360) as f32) - 180.0,
        pitch: ((i % 180) as f32) - 90.0,
        vel_x: if i % 3 == 0 { 0.0 } else { i as f32 },
        vel_y: if i % 3 == 1 { 0.0 } else { -(i as f32) },
        vel_z: 0.0,
        // Vary all three so a round-trip check discriminates a real copy from
        // a constant fill.
        timestamp: i * 3,
        movement_state: (i % 5) as u8,
        move_type: (i % 2) as u8,
    }
}

#[test]
fn zero_row_group_size_returns_a_controlled_error() {
    let path = test_dir().join("zero_row_group_size.parquet");
    let file = fs::File::create(path).unwrap();

    assert!(FieldWriter::with_row_group_size(file, 0).is_err());
}

/// Read all record batches from a Parquet file.
fn read_all_batches(path: &Path) -> Vec<RecordBatch> {
    let file = fs::File::open(path).unwrap();
    let reader = ParquetRecordBatchReaderBuilder::try_new(file)
        .unwrap()
        .build()
        .unwrap();
    reader.collect::<Result<Vec<_>, _>>().unwrap()
}

/// Write `rows` through table `T`'s writer at `row_group_size`, one `push`
/// per row or all of them through one `push_batch`, and return the path.
fn write_rows<T: Table>(
    name: &str,
    row_group_size: usize,
    rows: impl IntoIterator<Item = T::Row>,
    one_batch: bool,
) -> PathBuf {
    let path = test_dir().join(format!("{name}.parquet"));
    let file = fs::File::create(&path).unwrap();
    let mut writer = TableWriter::<T, fs::File>::with_row_group_size(file, row_group_size).unwrap();
    if one_batch {
        writer.push_batch(rows).unwrap();
    } else {
        for row in rows {
            writer.push(row).unwrap();
        }
    }
    writer.finish().unwrap();
    path
}

/// Write `rows` through table `T` at 1,024 rows per row group, one `push` per
/// row, and read the file back as the single batch it holds.
fn roundtrip<T: Table>(name: &str, rows: impl IntoIterator<Item = T::Row>) -> RecordBatch {
    let mut batches = read_all_batches(&write_rows::<T>(name, 1024, rows, false));
    assert_eq!(batches.len(), 1, "{name}: one batch expected");
    batches.remove(0)
}

/// The column named `name`.
fn col<'a>(batch: &'a RecordBatch, name: &str) -> &'a ArrayRef {
    batch
        .column_by_name(name)
        .unwrap_or_else(|| panic!("no column named {name}"))
}

// --- Field Writer Tests ---------------------------------------------------

#[test]
fn field_roundtrip_basic() {
    let batch = roundtrip::<FieldsTable>("field_roundtrip_basic", (0..50).map(make_field_record));
    assert_eq!(batch.num_rows(), 50);

    let time_ms = col(&batch, "time_ms").as_primitive::<UInt32Type>();
    assert_eq!(time_ms.value(0), 0);
    assert_eq!(time_ms.value(1), 10);
    assert_eq!(time_ms.value(49), 490);
}

#[test]
fn field_null_preservation() {
    let batch = roundtrip::<FieldsTable>(
        "field_null_preservation",
        [
            // Row 0: field_name=None, raw_bits=None, value_i64=Some(0)
            FieldRecord {
                time_ms: 0,
                packet_id: 0,
                channel_index: 0,
                actor_net_guid: 1,
                object_net_guid: None,
                group_path: "Test".into(),
                handle: 0,
                field_name: None,
                compatible_checksum: None,
                bit_count: 0,
                raw_bits: None,
                value_i64: Some(0),
                value_f64: None,
                value_bool: None,
                value_str: None,
            },
            // Row 1: field_name=Some, raw_bits=Some, value_str=Some
            FieldRecord {
                time_ms: 1,
                packet_id: 1,
                channel_index: 0,
                actor_net_guid: 1,
                object_net_guid: None,
                group_path: "Test".into(),
                handle: 1,
                field_name: Some("Health".into()),
                compatible_checksum: None,
                bit_count: 8,
                raw_bits: Some(vec![0xAB].into()),
                value_i64: None,
                value_f64: None,
                value_bool: None,
                value_str: Some("hello".into()),
            },
        ],
    );

    // field_name: row 0 null, row 1 = "Health"
    let field_name = col(&batch, "field_name").as_dictionary::<Int32Type>();
    assert!(field_name.is_null(0));
    assert!(!field_name.is_null(1));
    let field_name_values = field_name.downcast_dict::<StringArray>().unwrap();
    assert_eq!(field_name_values.value(1), "Health");

    // raw_bits: row 0 null, row 1 = [0xAB]
    let raw_bits = col(&batch, "raw_bits").as_binary::<i32>();
    assert!(raw_bits.is_null(0));
    assert_eq!(raw_bits.value(1), &[0xAB]);

    // value_i64: row 0 = Some(0), row 1 = null
    let value_i64 = col(&batch, "value_i64").as_primitive::<Int64Type>();
    assert!(!value_i64.is_null(0));
    assert_eq!(value_i64.value(0), 0);
    assert!(value_i64.is_null(1));

    // value_str: row 0 = null, row 1 = "hello". Now dictionary-encoded like
    // field_name, so read it back through the dictionary view rather than a
    // bare StringArray downcast.
    let value_str = col(&batch, "value_str").as_dictionary::<Int32Type>();
    assert!(value_str.is_null(0));
    assert!(!value_str.is_null(1));
    let value_str_values = value_str.downcast_dict::<StringArray>().unwrap();
    assert_eq!(value_str_values.value(1), "hello");
}

#[test]
fn field_binary_preservation() {
    // Verify that arbitrary binary data (including 0x00 bytes) survives.
    let payload: Vec<u8> = (0..=255).collect();
    let batch = roundtrip::<FieldsTable>(
        "field_binary_preservation",
        [FieldRecord {
            time_ms: 0,
            packet_id: 0,
            channel_index: 0,
            actor_net_guid: 0,
            object_net_guid: None,
            group_path: "Bin".into(),
            handle: 0,
            field_name: None,
            compatible_checksum: None,
            bit_count: 256 * 8,
            raw_bits: Some(payload.clone().into()),
            value_i64: None,
            value_f64: None,
            value_bool: None,
            value_str: None,
        }],
    );

    let raw_bits = col(&batch, "raw_bits").as_binary::<i32>();
    assert_eq!(raw_bits.value(0), payload.as_slice());
}

#[test]
fn unresolved_class_net_cache_payload_marker_roundtrips_exact_bits() {
    let batch = roundtrip::<FieldsTable>(
        "unresolved_class_net_cache_payload",
        [FieldRecord {
            time_ms: 1234,
            packet_id: 56,
            channel_index: 7,
            actor_net_guid: 89,
            object_net_guid: Some(144),
            group_path: "AbilitiesAndBuffsComponent".into(),
            handle: u32::MAX,
            field_name: Some(UNRESOLVED_CLASS_NET_CACHE_PAYLOAD_FIELD_NAME.into()),
            compatible_checksum: None,
            bit_count: 7,
            raw_bits: Some(vec![0x66].into()),
            value_i64: None,
            value_f64: None,
            value_bool: None,
            value_str: None,
        }],
    );
    assert_eq!(batch.num_rows(), 1);

    assert_eq!(
        col(&batch, "handle").as_primitive::<UInt32Type>().value(0),
        u32::MAX
    );

    let field_name = col(&batch, "field_name")
        .as_dictionary::<Int32Type>()
        .downcast_dict::<StringArray>()
        .unwrap();
    assert_eq!(
        field_name.value(0),
        UNRESOLVED_CLASS_NET_CACHE_PAYLOAD_FIELD_NAME
    );

    assert_eq!(
        col(&batch, "bit_count")
            .as_primitive::<UInt32Type>()
            .value(0),
        7
    );

    let raw_bits = col(&batch, "raw_bits").as_binary::<i32>();
    assert_eq!(raw_bits.value(0), &[0x66]);
    assert_eq!(raw_bits.value(0)[0] >> 7, 0);

    for name in ["value_i64", "value_f64", "value_bool", "value_str"] {
        assert!(col(&batch, name).is_null(0), "{name} must stay null");
    }
}

// --- Movement Writer Tests ------------------------------------------------

#[test]
fn movement_roundtrip_basic() {
    let batch = roundtrip::<MovementTable>(
        "movement_roundtrip_basic",
        (0..100).map(make_movement_record),
    );
    assert_eq!(batch.num_rows(), 100);

    let pos_x = col(&batch, "pos_x").as_primitive::<Float32Type>();
    assert!((pos_x.value(0) - 0.0).abs() < f32::EPSILON);
    assert!((pos_x.value(1) - 1.5).abs() < f32::EPSILON);
}

#[test]
fn movement_f32_precision() {
    let batch = roundtrip::<MovementTable>(
        "movement_f32_precision",
        [MovementRecord {
            time_ms: 0,
            packet_id: 0,
            character_net_guid: 1,
            pos_x: std::f32::consts::PI,
            pos_y: -std::f32::consts::E,
            pos_z: f32::MIN_POSITIVE,
            yaw: 179.99,
            pitch: -89.5,
            vel_x: f32::MAX,
            vel_y: f32::MIN,
            vel_z: 0.0,
            timestamp: 0,
            movement_state: 0,
            move_type: 0,
        }],
    );

    let pos_x = col(&batch, "pos_x").as_primitive::<Float32Type>();
    assert_eq!(pos_x.value(0), std::f32::consts::PI);
    let vel_x = col(&batch, "vel_x").as_primitive::<Float32Type>();
    assert_eq!(vel_x.value(0), f32::MAX);
}

#[test]
fn movement_state_columns_keep_their_narrow_types() {
    // The three columns added after vel_z are u32/u8/u8 on the wire. Parquet
    // has no native 8-bit physical type -- it stores them as INT32 with an
    // INTEGER(8, false) logical annotation -- so the assertion that matters is
    // that a reader still hands them back as UInt8, not silently widened.
    let batch =
        roundtrip::<MovementTable>("movement_narrow_types", (0..64).map(make_movement_record));
    let schema = batch.schema();

    assert_eq!(
        schema.field_with_name("timestamp").unwrap().data_type(),
        &DataType::UInt32
    );
    for name in ["movement_state", "move_type"] {
        let field = schema.field_with_name(name).unwrap();
        assert_eq!(field.data_type(), &DataType::UInt8, "{name} was widened");
    }
    // The movement table is dense by contract; python_interop.py asserts the
    // same thing over the whole schema.
    for name in ["timestamp", "movement_state", "move_type"] {
        assert!(
            !schema.field_with_name(name).unwrap().is_nullable(),
            "{name} must not be nullable"
        );
    }

    // Appended after vel_z, not interleaved: consumers that address movement
    // columns by position (column 3 = pos_x, column 8 = vel_x) must keep
    // working.
    let names: Vec<&str> = schema.fields().iter().map(|f| f.name().as_str()).collect();
    assert_eq!(
        names,
        vec![
            "time_ms",
            "packet_id",
            "character_net_guid",
            "pos_x",
            "pos_y",
            "pos_z",
            "yaw",
            "pitch",
            "vel_x",
            "vel_y",
            "vel_z",
            "timestamp",
            "movement_state",
            "move_type",
        ]
    );

    // mode_flags is deliberately not a column: vrf_movement assigns it from
    // the same local as movement_state, so it could only ever duplicate it.
    assert!(schema.field_with_name("mode_flags").is_err());
}

#[test]
fn movement_new_columns_roundtrip_values() {
    // Boundary row: the widest value each column can hold.
    let mut extreme = make_movement_record(20);
    extreme.timestamp = u32::MAX;
    extreme.movement_state = u8::MAX;
    extreme.move_type = 1;
    let batch = roundtrip::<MovementTable>(
        "movement_new_columns_values",
        (0..20).map(make_movement_record).chain([extreme]),
    );
    assert_eq!(batch.num_rows(), 21);

    let timestamp = col(&batch, "timestamp").as_primitive::<UInt32Type>();
    let movement_state = col(&batch, "movement_state").as_primitive::<UInt8Type>();
    let move_type = col(&batch, "move_type").as_primitive::<UInt8Type>();

    for i in 0..20usize {
        let expected = make_movement_record(i as u32);
        assert_eq!(timestamp.value(i), expected.timestamp, "row {i} timestamp");
        assert_eq!(
            movement_state.value(i),
            expected.movement_state,
            "row {i} movement_state"
        );
        assert_eq!(move_type.value(i), expected.move_type, "row {i} move_type");
    }

    assert_eq!(timestamp.value(20), u32::MAX);
    assert_eq!(movement_state.value(20), u8::MAX);
    assert_eq!(move_type.value(20), 1);

    // The helper must actually vary these, or the loop above proves nothing.
    let distinct_states: std::collections::BTreeSet<u8> =
        (0..20).map(|i| movement_state.value(i)).collect();
    assert!(
        distinct_states.len() > 1,
        "movement_state did not vary across rows"
    );
}

/// Write interop files for the Python verification step (requirement section 6).
#[test]
fn write_interop_files() {
    let dir = test_dir().join("interop");
    fs::create_dir_all(&dir).unwrap();

    let field_path = dir.join("fields_interop.parquet");
    let movement_path = dir.join("movement_interop.parquet");

    // Write 10_000 field records.
    {
        let file = fs::File::create(&field_path).unwrap();
        let mut writer = FieldWriter::with_row_group_size(file, 4096).unwrap();
        for i in 0..10_000u32 {
            writer.push(make_field_record(i)).unwrap();
        }
        writer.finish().unwrap();
    }

    // Write 50_000 movement records.
    {
        let file = fs::File::create(&movement_path).unwrap();
        let mut writer = MovementWriter::with_row_group_size(file, 8192).unwrap();
        for i in 0..50_000u32 {
            writer.push(make_movement_record(i)).unwrap();
        }
        writer.finish().unwrap();
    }

    // Verify files exist and have non-trivial size.
    assert!(field_path.exists());
    assert!(movement_path.exists());
    let field_size = fs::metadata(&field_path).unwrap().len();
    let movement_size = fs::metadata(&movement_path).unwrap().len();
    assert!(field_size > 1000, "fields file too small: {field_size}");
    assert!(
        movement_size > 1000,
        "movement file too small: {movement_size}"
    );

    // Print paths for the Python script to find.
    println!("INTEROP_FIELDS={}", field_path.display());
    println!("INTEROP_MOVEMENT={}", movement_path.display());
    println!("FIELD_SIZE_BYTES={field_size}");
    println!("MOVEMENT_SIZE_BYTES={movement_size}");
}

// --- Actor Writer Tests ---

/// Helper: build an ActorRecord with predictable values.
fn make_actor_record(i: u32, is_open: bool) -> ActorRecord {
    ActorRecord {
        time_ms: i * 16,
        packet_id: i / 2,
        channel_index: i % 64,
        actor_net_guid: 2000 + i,
        event: if is_open { "open" } else { "close" },
        class_path: if i % 4 == 0 {
            None
        } else {
            Some(format!(
                "/Game/Characters/Agent_{}/Agent_{}_PC.Agent_{}_PC_C",
                i % 5,
                i % 5,
                i % 5
            ))
        },
        archetype_path: if is_open && i % 3 != 0 {
            Some(format!(
                "/Game/Characters/Agent_{}/Agent_{}_PC.Default__Agent_{}_PC_C",
                i % 5,
                i % 5,
                i % 5
            ))
        } else {
            None
        },
        spawn_x: if is_open { Some(i as f32 * 10.0) } else { None },
        spawn_y: if is_open { Some(i as f32 * 20.0) } else { None },
        spawn_z: if is_open { Some(100.0) } else { None },
        spawn_pitch: if is_open && i % 2 == 0 {
            Some(5.0)
        } else {
            None
        },
        spawn_yaw: if is_open && i % 2 == 0 {
            Some(90.0)
        } else {
            None
        },
        spawn_roll: if is_open && i % 2 == 0 {
            Some(0.0)
        } else {
            None
        },
    }
}

#[test]
fn actor_roundtrip_basic() {
    let batch = roundtrip::<ActorsTable>(
        "actor_roundtrip_basic",
        (0..100).map(|i| make_actor_record(i, i % 3 != 2)),
    );
    assert_eq!(batch.num_rows(), 100);

    let time_ms = col(&batch, "time_ms").as_primitive::<UInt32Type>();
    assert_eq!(time_ms.value(0), 0);
    assert_eq!(time_ms.value(1), 16);

    // Verify event column (string).
    let event = col(&batch, "event").as_string::<i32>();
    assert_eq!(event.value(0), "open");
    // Index 2 is the first "close" (i=2, i%3==2).
    assert_eq!(event.value(2), "close");
}

#[test]
fn actor_null_class_path() {
    // Row 0: class_path = None (i=0, i%4==0); row 1: Some (i=1, i%4!=0).
    let batch = roundtrip::<ActorsTable>(
        "actor_null_class_path",
        [make_actor_record(0, true), make_actor_record(1, true)],
    );

    let class_path = col(&batch, "class_path").as_dictionary::<Int32Type>();
    assert!(class_path.is_null(0));
    assert!(!class_path.is_null(1));
    let class_path_values = class_path.downcast_dict::<StringArray>().unwrap();
    assert_eq!(
        class_path_values.value(1),
        "/Game/Characters/Agent_1/Agent_1_PC.Agent_1_PC_C"
    );
}

#[test]
fn actor_spawn_location_nullable() {
    // An open carries a spawn location; the matching close does not.
    let batch = roundtrip::<ActorsTable>(
        "actor_spawn_location",
        [make_actor_record(5, true), make_actor_record(5, false)],
    );

    let spawn_x = col(&batch, "spawn_x").as_primitive::<Float32Type>();
    // Open row has spawn_x = 50.0
    assert!(!spawn_x.is_null(0));
    assert!((spawn_x.value(0) - 50.0).abs() < f32::EPSILON);
    // Close row has null spawn_x
    assert!(spawn_x.is_null(1));
}

// ---------------------------------------------------------------------------
// net_guids table
// ---------------------------------------------------------------------------

#[test]
fn net_guid_roundtrip_preserves_outer_chain() {
    let batch = roundtrip::<NetGuidsTable>(
        "net_guid_roundtrip",
        [
            // A weapon actor: no outer.
            NetGuidRecord {
                net_guid: 2910,
                path: "/Game/Equippables/Guns/Sidearms/Revolver/RevolverPistol.RevolverPistol_C"
                    .into(),
                outer_net_guid: None,
            },
            // Its FiringState subobject: outer points back at the weapon.
            NetGuidRecord {
                net_guid: 3086,
                path: "FiringState".into(),
                outer_net_guid: Some(2910),
            },
        ],
    );
    assert_eq!(batch.num_rows(), 2);

    let net_guid = col(&batch, "net_guid").as_primitive::<UInt32Type>();
    assert_eq!(net_guid.value(0), 2910);
    assert_eq!(net_guid.value(1), 3086);

    let path_col = col(&batch, "path").as_dictionary::<Int32Type>();
    let path_values = path_col.downcast_dict::<StringArray>().unwrap();
    assert_eq!(path_values.value(1), "FiringState");

    let outer = col(&batch, "outer_net_guid").as_primitive::<UInt32Type>();
    // A GUID with no declared outer must be null, not 0 -- 0 is a real
    // sentinel meaning "invalid GUID" and must stay distinguishable.
    assert!(outer.is_null(0));
    assert_eq!(outer.value(1), 2910);
}

#[test]
fn field_object_net_guid_roundtrips_and_is_nullable() {
    // A content block can describe the actor itself or one of its subobjects.
    // Without the subobject GUID every ItemSlot on a character collapses onto
    // one key downstream, so a player appears to hold a single item.
    // `None` means "this block described the actor", which must stay distinct
    // from any real GUID -- including 0.
    let mut actor_block = make_field_record(1);
    actor_block.object_net_guid = None;
    let mut subobject_block = make_field_record(2);
    subobject_block.object_net_guid = Some(4242);
    let batch = roundtrip::<FieldsTable>("field_object_net_guid", [actor_block, subobject_block]);

    let object_net_guid = col(&batch, "object_net_guid").as_primitive::<UInt32Type>();
    assert!(
        object_net_guid.is_null(0),
        "actor blocks carry no subobject GUID"
    );
    assert_eq!(object_net_guid.value(1), 4242);
}

// ---------------------------------------------------------------------------
// events table
// ---------------------------------------------------------------------------

/// The first `roundStarted` payload from the reference replay, byte for byte.
/// It carries an embedded 0x00 and a tail that is not valid text, which is the
/// point: this column has to survive bytes that are not a string.
const REFERENCE_EVENT_PAYLOAD: [u8; 46] = [
    0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x1E, 0x00, 0x00, 0x00, b'E', b'R', b'e', b'p',
    b'l', b'a', b'y', b'E', b'v', b'e', b'n', b't', b'G', b'r', b'o', b'u', b'p', b':', b':', b'R',
    b'o', b'u', b'n', b'd', b'S', b't', b'a', b'r', b't', 0x00, 0x22, 0xC0, 0x7F, 0x3D,
];

#[test]
fn event_roundtrip_preserves_payload_bytes_exactly() {
    let batch = roundtrip::<EventsTable>(
        "event_roundtrip",
        [
            EventRecord {
                id: "02d4d478_DC4D6C49E0C640FD814D88134F0A8642".into(),
                group: "roundStarted".into(),
                metadata: "0".into(),
                time1: 62,
                time2: 62,
                payload_size: REFERENCE_EVENT_PAYLOAD.len() as i32,
                raw_payload: REFERENCE_EVENT_PAYLOAD.to_vec(),
                word0: None,
                word1: None,
                payload_tag: Some(2),
                payload_name: Some("EReplayEventGroup::RoundStart".into()),
                payload_seconds: Some(f32::from_bits(0x3D7F_C022)),
            },
            // A second group, so the dictionary column carries more than one value.
            EventRecord {
                id: "02d4d478_0B756A9C4B10407DB9D3A4093C057D43".into(),
                group: "characterDeath".into(),
                metadata: String::new(),
                time1: 50402,
                time2: 50402,
                payload_size: 3,
                raw_payload: vec![0x00, 0xFF, 0x80],
                word0: None,
                word1: None,
                payload_tag: None,
                payload_name: None,
                payload_seconds: None,
            },
        ],
    );
    assert_eq!(batch.num_rows(), 2);

    let id = col(&batch, "id").as_string::<i32>();
    assert_eq!(id.value(0), "02d4d478_DC4D6C49E0C640FD814D88134F0A8642");

    let group = col(&batch, "group").as_dictionary::<Int32Type>();
    let group_values = group.downcast_dict::<StringArray>().unwrap();
    assert_eq!(group_values.value(0), "roundStarted");
    assert_eq!(group_values.value(1), "characterDeath");

    // An event with no metadata carries an empty string, not a null.
    let metadata = col(&batch, "metadata").as_string::<i32>();
    assert!(!metadata.is_null(1));
    assert_eq!(metadata.value(0), "0");
    assert_eq!(metadata.value(1), "");

    let time1 = col(&batch, "time1").as_primitive::<UInt32Type>();
    let time2 = col(&batch, "time2").as_primitive::<UInt32Type>();
    assert_eq!(time1.value(1), 50402);
    assert_eq!(time2.value(1), 50402);

    let payload_size = col(&batch, "payload_size").as_primitive::<Int32Type>();
    assert_eq!(payload_size.value(0), 46);
    assert_eq!(payload_size.value(1), 3);

    // The undecoded payload is the whole point of the table: every byte, in
    // order, including the embedded 0x00 and the bytes that are not text.
    let raw = col(&batch, "raw_payload").as_binary::<i32>();
    assert_eq!(raw.value(0), REFERENCE_EVENT_PAYLOAD);
    assert_eq!(raw.value(1), [0x00, 0xFF, 0x80]);
    // The declared size and the stored blob must agree row for row.
    for i in 0..batch.num_rows() {
        assert_eq!(payload_size.value(i) as usize, raw.value(i).len());
    }

    let payload_tag = col(&batch, "payload_tag").as_primitive::<UInt32Type>();
    let payload_name = col(&batch, "payload_name").as_string::<i32>();
    let payload_seconds = col(&batch, "payload_seconds").as_primitive::<Float32Type>();
    assert_eq!(payload_tag.value(0), 2);
    assert_eq!(payload_name.value(0), "EReplayEventGroup::RoundStart");
    assert_eq!(payload_seconds.value(0).to_bits(), 0x3D7F_C022);
    assert!(payload_tag.is_null(1));
    assert!(payload_name.is_null(1));
    assert!(payload_seconds.is_null(1));
}

/// The replay's own `compatible_checksum` survives the round trip, nulls
/// included.
///
/// It is what tells "this field is legitimately undescribed" apart from "this
/// field has a checksum the overlay never learned" -- the Phoenix case, where a
/// whole class was missing from the table and 2,791 rows read null with decode
/// errors at 0. Without the checksum in the export those two look identical
/// offline, and the only reason Phoenix was found at all is that a sibling
/// class happened to share its RPC name.
#[test]
fn compatible_checksum_round_trips_with_its_nulls() {
    let rows = (0..8u32).map(|i| FieldRecord {
        // Odd rows model a handle the replay declares no checksum for.
        compatible_checksum: (i % 2 == 0).then_some(1_000_000 + i),
        ..make_field_record(i)
    });
    let batch = roundtrip::<FieldsTable>("checksum_roundtrip", rows);

    let checksum = col(&batch, "compatible_checksum").as_primitive::<UInt32Type>();
    for i in 0..8usize {
        if i % 2 == 0 {
            assert_eq!(checksum.value(i), 1_000_000 + i as u32, "row {i}");
        } else {
            assert!(checksum.is_null(i), "row {i} should be null");
        }
    }
}

// ---------------------------------------------------------------------------
// One writer serves every table: row groups, empty files, push_batch
// ---------------------------------------------------------------------------

/// An event row that varies with `i`, alternating between two groups.
fn make_event_record(i: u32) -> EventRecord {
    EventRecord {
        id: format!("id_{i}"),
        group: ["characterDeath", "spikePlanted"][(i % 2) as usize].into(),
        metadata: String::new(),
        time1: i * 1000,
        time2: i * 1000,
        payload_size: 4,
        raw_payload: i.to_le_bytes().to_vec(),
        word0: None,
        word1: None,
        payload_tag: None,
        payload_name: None,
        payload_seconds: None,
    }
}

/// Every row group's row count in file order, after checking that the whole
/// file reads back with that many rows.
fn row_groups(path: &Path) -> Vec<i64> {
    let reader = SerializedFileReader::new(fs::File::open(path).unwrap()).unwrap();
    let counts: Vec<i64> = reader
        .metadata()
        .row_groups()
        .iter()
        .map(|group| group.num_rows())
        .collect();
    let read_back: usize = read_all_batches(path)
        .iter()
        .map(RecordBatch::num_rows)
        .sum();
    assert_eq!(
        read_back as i64,
        counts.iter().sum::<i64>(),
        "{}",
        path.display()
    );
    counts
}

#[test]
fn row_groups_close_at_the_row_group_size_not_at_each_batch() {
    // 200,000 rows at 65,536 per row group: three full groups and the rest.
    // Each full group spans eight batches of MAX_BUFFERED_ROWS, so a writer
    // that closed a row group whenever it handed over a batch would write 24
    // groups of 8,192 and one of 3,392 -- which the ">= 3 row groups" these
    // assertions replace accepted. Only the exact vector tells them apart.
    const ROWS: u32 = 200_000;
    const _: () = assert!(vrf_export::writer::MAX_BUFFERED_ROWS < 65_536);
    let expected = vec![65_536, 65_536, 65_536, 3_392];

    let rows = (0..ROWS).map(make_field_record);
    let path = write_rows::<FieldsTable>("row_groups_fields", 65_536, rows, false);
    assert_eq!(row_groups(&path), expected, "fields");
    let rows = (0..ROWS).map(make_movement_record);
    let path = write_rows::<MovementTable>("row_groups_movement", 65_536, rows, false);
    assert_eq!(row_groups(&path), expected, "movement");
    let rows = (0..ROWS).map(make_event_record);
    let path = write_rows::<EventsTable>("row_groups_events", 65_536, rows, false);
    assert_eq!(row_groups(&path), expected, "events");
}

#[test]
fn a_writer_finished_with_no_rows_writes_a_readable_empty_file() {
    fn rows_in_empty_file<T: Table>(name: &str) -> i64 {
        let path = write_rows::<T>(name, T::DEFAULT_ROW_GROUP_SIZE, [], false);
        row_groups(&path).iter().sum()
    }
    assert_eq!(rows_in_empty_file::<FieldsTable>("empty_fields"), 0);
    assert_eq!(rows_in_empty_file::<MovementTable>("empty_movement"), 0);
    assert_eq!(rows_in_empty_file::<ActorsTable>("empty_actors"), 0);
    assert_eq!(rows_in_empty_file::<NetGuidsTable>("empty_net_guids"), 0);
    assert_eq!(rows_in_empty_file::<EventsTable>("empty_events"), 0);
}

/// Write rows `0..count` once through `push_batch` and once through one
/// `push` per row, at 128 rows per row group so the flushes fall inside the
/// batch. Asserts the two files are byte-identical; returns the row groups.
fn push_batch_against_push<T: Table>(name: &str, count: u32, make: fn(u32) -> T::Row) -> Vec<i64> {
    let batched = write_rows::<T>(&format!("{name}_batched"), 128, (0..count).map(make), true);
    let pushed = write_rows::<T>(&format!("{name}_pushed"), 128, (0..count).map(make), false);
    assert!(
        fs::read(&batched).unwrap() == fs::read(pushed).unwrap(),
        "{name}: push_batch wrote a different file"
    );
    row_groups(&batched)
}

#[test]
fn push_batch_writes_the_same_file_as_one_push_per_row() {
    assert_eq!(
        push_batch_against_push::<FieldsTable>("push_batch_fields", 300, make_field_record),
        vec![128, 128, 44]
    );
    assert_eq!(
        push_batch_against_push::<MovementTable>("push_batch_movement", 500, make_movement_record),
        vec![128, 128, 128, 116]
    );
}

// ---------------------------------------------------------------------------
// Dictionary encoding is per column
// ---------------------------------------------------------------------------

/// The writers dictionary-encode exactly the columns each table lists in
/// `Table::DICTIONARY_COLUMNS`, and nothing else.
///
/// The list was once applied over parquet-rs's default of dictionary-encoding
/// every column, so it switched on what was already on and could not move a
/// byte, while every table's comment read as though it decided the encoding.
/// These tests read the written file's footer, not the list.
///
/// Gated on the two table features the file-level gate does not cover, so the
/// rest of this file still builds with only the five main tables.
#[cfg(all(feature = "partials", feature = "checkpoint-context"))]
mod dictionary_encoding {
    use super::*;

    use std::collections::HashSet;
    use std::sync::Arc;

    use parquet::basic::Type as PhysicalType;
    use vrf_export::{
        CheckpointActorRecord, CheckpointActorsTable, CheckpointBlockRecord, CheckpointBlocksTable,
        CheckpointExportFieldRecord, CheckpointExportFieldsTable, CheckpointExportGroupRecord,
        CheckpointExportGroupsTable, CheckpointFieldRecord, CheckpointFieldsTable,
        CheckpointGuidEntriesTable, CheckpointGuidEntryRecord, CheckpointIdentity,
        CheckpointNetGuidRecord, CheckpointNetGuidsTable, PartialRecord, PartialsTable,
    };

    const ROWS: u32 = 8;

    /// Write `rows` through the real writer and return every column whose
    /// footer disagrees with `T::DICTIONARY_COLUMNS`.
    ///
    /// BOOLEAN columns are skipped: parquet-rs never gives them a dictionary,
    /// and `dictionary_lists_name_real_columns_and_cover_every_string` rejects
    /// one in a list. A column with no non-null value is reported rather than
    /// checked, so a fixture that stops populating a column cannot make the
    /// comparison depend on how an all-null chunk happens to be written.
    fn footer_disagreements<T: Table>(table: &str, rows: Vec<T::Row>) -> Vec<String> {
        let name = format!("dictionary_encoding_{table}");
        let path = write_rows::<T>(&name, T::DEFAULT_ROW_GROUP_SIZE, rows, true);
        let reader = SerializedFileReader::new(fs::File::open(&path).unwrap()).unwrap();
        let metadata = reader.metadata();
        assert_eq!(
            metadata.num_row_groups(),
            1,
            "{table}: one row group expected"
        );

        let expected_checked = T::schema()
            .fields()
            .iter()
            .filter(|field| field.data_type() != &DataType::Boolean)
            .count();
        let mut problems = Vec::new();
        let mut checked = 0;
        for column in metadata.row_group(0).columns() {
            let name = column.column_descr().name();
            if column.column_type() == PhysicalType::BOOLEAN {
                continue;
            }
            let nulls = column
                .statistics()
                .and_then(|stats| stats.null_count_opt())
                .unwrap_or_else(|| panic!("{table}.{name}: the writer stopped writing statistics"));
            if nulls >= column.num_values() as u64 {
                problems.push(format!("{table}.{name}: the fixture has no non-null value"));
                continue;
            }
            checked += 1;
            let listed = T::DICTIONARY_COLUMNS.contains(&name);
            let has_dictionary = column.dictionary_page_offset().is_some();
            if listed != has_dictionary {
                problems.push(format!(
                    "{table}.{name}: DICTIONARY_COLUMNS {} it, but the file has {} dictionary page",
                    if listed { "lists" } else { "omits" },
                    if has_dictionary { "a" } else { "no" },
                ));
            }
        }
        if checked != expected_checked {
            problems.push(format!(
                "{table}: checked {checked} columns, the schema has {expected_checked} non-boolean"
            ));
        }
        problems
    }

    fn identity(i: u32) -> CheckpointIdentity {
        CheckpointIdentity {
            checkpoint_index: i / 4,
            checkpoint_id: Arc::from(format!("checkpoint-{}", i / 4)),
        }
    }

    fn field_rows() -> Vec<FieldRecord> {
        (0..ROWS)
            .map(|i| FieldRecord {
                // make_field_record never sets a checksum.
                compatible_checksum: (i % 2 == 0).then_some(1_000 + i),
                ..make_field_record(i)
            })
            .collect()
    }

    fn actor_rows() -> Vec<ActorRecord> {
        (0..ROWS)
            .map(|i| make_actor_record(i, i % 2 == 0))
            .collect()
    }

    fn net_guid_rows() -> Vec<NetGuidRecord> {
        (0..ROWS)
            .map(|i| NetGuidRecord {
                net_guid: 100 + i,
                path: format!("Path_{}", i % 3),
                outer_net_guid: (i % 2 == 0).then_some(i),
            })
            .collect()
    }

    fn event_rows() -> Vec<EventRecord> {
        (0..ROWS)
            .map(|i| EventRecord {
                id: format!("event-{i}"),
                group: format!("group{}", i % 2),
                metadata: format!("meta{}", i % 3),
                time1: 1_000 * i,
                time2: 1_000 * i + 7,
                payload_size: 4 * i as i32,
                raw_payload: vec![i as u8; 4 * i as usize],
                word0: (i % 2 == 0).then_some(i),
                word1: (i % 3 == 0).then_some(i + 1),
                payload_tag: (i % 2 == 1).then_some(2),
                payload_name: (i % 2 == 1).then(|| format!("name{i}")),
                payload_seconds: (i % 2 == 1).then_some(i as f32 * 0.5),
            })
            .collect()
    }

    fn partial_rows() -> Vec<PartialRecord> {
        (0..ROWS)
            .map(|i| PartialRecord {
                source: if i % 2 == 0 { "main" } else { "checkpoint" },
                checkpoint_id: (i % 2 == 1).then(|| format!("checkpoint-{i}")),
                payload_kind: "current_fragment",
                reason: "missing_initial",
                source_packet_id: i as i32,
                source_payload_bit_offset: 64 * i64::from(i),
                rejection_packet_id: (i % 2 == 0).then_some(i as i32 + 1),
                channel_index: i % 3,
                channel_sequence: i as i32,
                open: i % 2 == 0,
                close: false,
                dormant: false,
                replication_paused: false,
                reliable: true,
                partial: true,
                partial_initial: i == 0,
                partial_final: i == ROWS - 1,
                has_package_map_exports: false,
                has_must_be_mapped_guids: false,
                close_reason: (i % 2) as u8,
                source_payload_bit_count: 8 * i as i32,
                bit_count: 8 * u64::from(i),
                raw_bits: vec![i as u8; i as usize + 1],
            })
            .collect()
    }

    fn checkpoint_block_rows() -> Vec<CheckpointBlockRecord> {
        (0..ROWS)
            .map(|i| CheckpointBlockRecord {
                checkpoint: identity(i),
                block_index: i,
                time_ms: 7 * i,
                packet_id: i,
                channel_index: i % 3,
                actor_net_guid: 200 + i,
                object_net_guid: (i % 2 == 0).then_some(300 + i),
                class_net_guid: (i % 2 == 1).then_some(400 + i),
                outer_net_guid: (i % 3 == 0).then_some(500 + i),
                has_rep_layout: i % 2 == 0,
                is_actor: i % 3 == 0,
                is_deleted: false,
                is_stably_named: i % 2 == 1,
                delete_flags: (i % 2) as u8,
                resolved_group_path: Arc::from(format!("Group_{}", i % 2)),
                group_resolution_source: "replay_declared_group",
                group_declared: true,
                resolution_memo_hit: i % 2 == 0,
                function_count: i % 4,
                function_count_source: "rep_layout_not_applicable",
                actor_archetype_path: Some(format!("Archetype_{}", i % 2)),
                actor_archetype_outer_path: Some("ArchetypeOuter".into()),
                actor_guid_path: Some(format!("Actor_{i}")),
                class_guid_path: Some("Class".into()),
                object_guid_path: (i % 2 == 0).then(|| format!("Object_{i}")),
                object_outer_path: (i % 2 == 1).then(|| format!("Outer_{i}")),
                field_row_start: 16 * u64::from(i),
                field_row_count: i % 5,
            })
            .collect()
    }

    fn checkpoint_guid_entry_rows() -> Vec<CheckpointGuidEntryRecord> {
        (0..ROWS)
            .map(|i| CheckpointGuidEntryRecord {
                checkpoint: identity(i),
                ordinal: i,
                net_guid: 600 + i,
                outer_net_guid: 600 + i / 2,
                path_is_string: i % 2 == 0,
                literal_path: (i % 2 == 0).then(|| format!("Literal_{i}")),
                name_index: (i % 2 == 1).then_some(i),
                flags: (i % 4) as u8,
            })
            .collect()
    }

    fn checkpoint_export_group_rows() -> Vec<CheckpointExportGroupRecord> {
        (0..ROWS)
            .map(|i| CheckpointExportGroupRecord {
                checkpoint: identity(i),
                ordinal: i,
                path_name_index: 10 + i,
                group_path: format!("ExportGroup_{i}"),
                declared_slots: i % 3,
            })
            .collect()
    }

    fn checkpoint_export_field_rows() -> Vec<CheckpointExportFieldRecord> {
        (0..ROWS)
            .map(|i| CheckpointExportFieldRecord {
                checkpoint: identity(i),
                group_ordinal: i / 2,
                path_name_index: 10 + i,
                slot: i,
                handle: i + 1,
                compatible_checksum: 7_000 + i,
                rendered_name: format!("Rendered_{i}"),
                exported_flag: (i % 2) as u8,
                fname_kind: (i % 3) as u8,
                fname_base: (i % 2 == 0).then(|| format!("Base_{i}")),
                fname_index: (i % 2 == 1).then_some(i),
                fname_number: (i % 3 == 0).then_some(i as i32),
            })
            .collect()
    }

    #[test]
    fn every_table_writes_a_dictionary_page_for_exactly_its_listed_columns() {
        let fields = field_rows();
        let actors = actor_rows();
        let net_guids = net_guid_rows();
        let mut problems = Vec::new();
        problems.extend(footer_disagreements::<FieldsTable>(
            "fields",
            fields.clone(),
        ));
        problems.extend(footer_disagreements::<MovementTable>(
            "movement",
            (0..ROWS).map(make_movement_record).collect(),
        ));
        problems.extend(footer_disagreements::<ActorsTable>(
            "actors",
            actors.clone(),
        ));
        problems.extend(footer_disagreements::<NetGuidsTable>(
            "net_guids",
            net_guids.clone(),
        ));
        problems.extend(footer_disagreements::<EventsTable>("events", event_rows()));
        problems.extend(footer_disagreements::<PartialsTable>(
            "partials",
            partial_rows(),
        ));
        problems.extend(footer_disagreements::<CheckpointFieldsTable>(
            "checkpoint_fields",
            fields
                .into_iter()
                .zip(0..)
                .map(|(field, i)| CheckpointFieldRecord {
                    checkpoint: identity(i),
                    field,
                })
                .collect(),
        ));
        problems.extend(footer_disagreements::<CheckpointActorsTable>(
            "checkpoint_actors",
            actors
                .into_iter()
                .zip(0..)
                .map(|(actor, i)| CheckpointActorRecord {
                    checkpoint: identity(i),
                    actor,
                })
                .collect(),
        ));
        problems.extend(footer_disagreements::<CheckpointNetGuidsTable>(
            "checkpoint_net_guids",
            net_guids
                .into_iter()
                .zip(0..)
                .map(|(net_guid, i)| CheckpointNetGuidRecord {
                    checkpoint: identity(i),
                    net_guid,
                })
                .collect(),
        ));
        problems.extend(footer_disagreements::<CheckpointBlocksTable>(
            "checkpoint_blocks",
            checkpoint_block_rows(),
        ));
        problems.extend(footer_disagreements::<CheckpointGuidEntriesTable>(
            "checkpoint_guid_entries",
            checkpoint_guid_entry_rows(),
        ));
        problems.extend(footer_disagreements::<CheckpointExportGroupsTable>(
            "checkpoint_export_groups",
            checkpoint_export_group_rows(),
        ));
        problems.extend(footer_disagreements::<CheckpointExportFieldsTable>(
            "checkpoint_export_fields",
            checkpoint_export_field_rows(),
        ));
        assert!(
            problems.is_empty(),
            "{} column(s) disagree with DICTIONARY_COLUMNS:\n  {}",
            problems.len(),
            problems.join("\n  ")
        );
    }

    /// Every name in a table's list must be a real, non-boolean column of that
    /// table, listed once -- parquet-rs silently ignores the other two cases --
    /// and every string column must be listed, because the docs promise that
    /// string columns are dictionary-encoded.
    fn list_problems<T: Table>(table: &str) -> Vec<String> {
        let schema = T::schema();
        let mut problems = Vec::new();
        let mut seen = HashSet::new();
        for &name in T::DICTIONARY_COLUMNS {
            if !seen.insert(name) {
                problems.push(format!("{table}.{name}: listed twice"));
            }
            match schema.field_with_name(name) {
                Err(_) => problems.push(format!(
                    "{table}.{name}: no such column; parquet-rs ignores the name without an error"
                )),
                Ok(field) if field.data_type() == &DataType::Boolean => problems.push(format!(
                    "{table}.{name}: BOOLEAN, which parquet-rs never dictionary-encodes"
                )),
                Ok(_) => {}
            }
        }
        for field in schema.fields() {
            let is_string = match field.data_type() {
                DataType::Utf8 => true,
                DataType::Dictionary(_, value) => value.as_ref() == &DataType::Utf8,
                _ => false,
            };
            if is_string && !T::DICTIONARY_COLUMNS.contains(&field.name().as_str()) {
                problems.push(format!(
                    "{table}.{}: a string column missing from DICTIONARY_COLUMNS",
                    field.name()
                ));
            }
        }
        problems
    }

    #[test]
    fn dictionary_lists_name_real_columns_and_cover_every_string() {
        let problems: Vec<String> = [
            list_problems::<FieldsTable>("fields"),
            list_problems::<MovementTable>("movement"),
            list_problems::<ActorsTable>("actors"),
            list_problems::<NetGuidsTable>("net_guids"),
            list_problems::<EventsTable>("events"),
            list_problems::<PartialsTable>("partials"),
            list_problems::<CheckpointFieldsTable>("checkpoint_fields"),
            list_problems::<CheckpointActorsTable>("checkpoint_actors"),
            list_problems::<CheckpointNetGuidsTable>("checkpoint_net_guids"),
            list_problems::<CheckpointBlocksTable>("checkpoint_blocks"),
            list_problems::<CheckpointGuidEntriesTable>("checkpoint_guid_entries"),
            list_problems::<CheckpointExportGroupsTable>("checkpoint_export_groups"),
            list_problems::<CheckpointExportFieldsTable>("checkpoint_export_fields"),
        ]
        .concat();
        assert!(
            problems.is_empty(),
            "{} problem(s) with DICTIONARY_COLUMNS:\n  {}",
            problems.len(),
            problems.join("\n  ")
        );
    }
}
