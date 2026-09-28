# Ground-area volumes: GroundVolumeComponent cells

Measured 2026-09-28 on the 1,018 exports of the common audit corpus (parser
`259ed10`, checkpoints on; builds 11.06 to 13.06). This closes the question
[the PatchVolume investigation](GAS_AND_PATCHVOLUME_INVESTIGATION.md) left
open -- an independent item schema for the numerically consumed PatchVolume
windows -- and decodes those windows with it. Nothing here changes Parquet
output; the decoder is a separate tool.

```bash
python tools/extract_ground_volumes.py --export-dir <export-directory> --out-dir <new-output-directory>
```

The output directory holds `items.ndjson` (one record per decoded cell
update), `windows.ndjson` (every selected source window with its raw bits and
a status) and `receipt.json` (counts including zeros, rejection reasons, the
replay's resolved declarations, input hashes before/after, the tool's own
hash). The exit status is nonzero if any window is rejected.

## What was established

A ground-area patch actor -- the `Patch_*` classes below: molotov, slow,
net-toss and barbed-wire patches and one circular `X` patch, going by their
class names -- owns a `/Script/DynamicVolume.GroundVolumeComponent`. The
component's cells travel as the FastArray property `FragmentInfo`, handle 0 of
the replay's `GroundVolumeComponent_ClassNetCache` group. Each changed item is
one cell: an integer grid position `X`/`Y`, a polygon of world-space points
(`ConvexHullPoints`), per-point ceilings and travel distances, a floor, a
ceiling, a mean travel distance, a 3-bit `Status`, a `bIsActive` bit, the
item value the replay names `253`, and `ExteriorSegments` index pairs into the
polygon. Every name here is the one the replay itself declares.

On the whole corpus:

| Population | Count |
|---|---:|
| Exports / exports with windows | 1,018 / 915 |
| Selected windows, all in the main stream | 36,661 |
| -- bare `PatchVolume`, whole ClassNetCache payload | 26,632 |
| -- bare `PatchVolume`, RepLayout tail | 9,967 |
| -- declared `GroundVolumeComponent`, RepLayout tail | 62 |
| Windows decoded exactly (0 leftover bits) | 36,661 |
| Windows rejected | 0 |
| Changed items (cells) / deleted item IDs | 346,186 / 0 |
| Items carrying every member their replay declares | 346,186 |
| Array elements decoded | 4,867,849 |
| Owner actor class resolved from `actors.parquet` | 346,186 |
| Component object's outer is the owning actor (`net_guids.parquet`) | 346,186 |
| `TJunctions` members kept raw (all 16 zero bits) | 7,232 |

Neither route has a checkpoint row. The 1,018 receipts carry one extractor
hash and unchanged input hashes.

## The two routes

The parser does not decode custom-delta properties, so it preserves these
windows raw. Both kinds of window start at the first ClassNetCache field
header.

| Route | `group_path` | Windows | Why the parser leaves it raw |
|---|---|---|---|
| `bare_patch_volume` | `PatchVolume` | whole payloads (`__vrfkit_unresolved_class_net_cache_payload__`, handle u32::MAX) and tails (`__vrfkit_unparsed_rep_layout_tail__`, handle 0) | The subobject `PatchVolume` is stably named under its patch actor; the parser cannot resolve its class, groups its rows by the object name, and its RepLayout prefix rows carry no names |
| `declared_class` | `/Script/DynamicVolume.GroundVolumeComponent` | tails only | The class resolves, but the ClassNetCache tail is a custom-delta property |

`PatchVolume` is a GroundVolumeComponent by the replay's own evidence, not by
its name:

- every one of the 36,599 bare windows decodes exactly under its replay's
  GroundVolumeComponent declaration, and every member handle in them maps to a
  declared GroundVolumeComponent identity;
- every one of the 77,055 bare RepLayout prefix rows carries a handle the same
  replay declares for GroundVolumeComponent, at a width that fits the declared
  member: 1 bit for `bIsActive`/`bShouldBeAttached`, 16 or 24 for
  `AttachParent` (the widths the declared-class rows show), 64 for
  `AttachChildren`, 192 for `RelativeScale3D` (and `Translation`/`Scale3D` in
  11.06-11.09), 16 for `VolumeMaxExtentShape`, 32 for `FinalCount`;
- the decoded cells lie on a grid anchored at the owning actor's spawn
  position (below).

A separate IoStore read of the game's component classes (a parallel branch,
not in this tree) reached the same class.

Route completeness: the same strict decoder, with each replay's declaration,
was run on every other preserved ClassNetCache window or tail in both field
tables of all 1,018 exports -- 5,552,491 main and 1,666 checkpoint windows of
every other group. 312,179 of them are in the 63 exports that declare no
ClassNetCache slot count for the component, where no window can be tried; the
other 5,241,978 were tried and none decodes.

Owners, by the class of the actor that owns the object:

| Owner class | Route | Objects | Cell updates |
|---|---|---:|---:|
| `Patch_Deadeye_E_Slow_Large_C` | bare | 3,150 | 132,781 |
| `Patch_Phoenix_MolotovFire_C` | bare | 3,447 | 91,131 |
| `Patch_Cable_4_NetToss_C` | bare | 775 | 36,639 |
| `Patch_Aggrobot_C_ExplodeyPatch_C` | bare | 645 | 30,246 |
| `Patch_Nox_BarbedWire_C` | bare | 486 | 23,090 |
| `Patch_Sarge_Q_Molotov_Production_C` | bare | 618 | 16,180 |
| `Patch_Pandemic_AcidMolotov_NewMolotov_C` | bare | 274 | 7,544 |
| `Patch_NetToss_C` | bare | 71 | 3,383 |
| `Patch_Pandemic_X_Circular_C` | declared | 62 | 5,192 |

Not every PatchVolume sends cells: all 3,457 `PatchVolume` objects of
`Patch_Thorne_4_SlowField_Production_C`, and 37 of
`Patch_Deadeye_E_Slow_Large_C`, replicate only the RepLayout prefix.

## Wire grammar

```text
window  := entry+                          -- until the window's last bit
entry   := handle:SerializeInt(max(slots, 2)) width:packed body[width]
body    := support:1(=1) array_key:i32 base_key:i32 deletes:i32 changed:i32
           deleted_id:i32 * deletes
           (item_id:i32 members) * changed
members := (handle+1:packed width:packed payload[width])* 0:packed
array   := count:packed (index+1:packed members)* 0:packed
```

- `slots` is the ClassNetCache group's declared slot count. The manifest does
  not record it for the main stream, so it is read from the checkpoint
  declarations: every checkpoint declaration of
  `GroundVolumeComponent_ClassNetCache` in the corpus says 2, including one in
  each of the 915 exports with windows. `SerializeInt(2)` is one bit. Handle 0
  is declared as `FragmentInfo` with checksum 2225407835 in every build.
- `body` is the FastArray grammar the investigation measured: no per-item
  checksum bit. Every window here holds exactly one entry, none deletes an
  item, and every entry changes at least one.
- `members` is Unreal's backwards-compatible property stream. The member
  handle is a GroundVolumeComponent RepLayout command index, so array element
  members have handles of their own and each array's command range ends with
  an undeclared return command.
- Every array in the corpus is sent whole: element indices run 0..count-1 in
  order, every element carries all of its members, and nothing follows the
  terminator.

## Item schema per build, from the replays' declarations

Method: for each export, the union of the main-stream declaration
(`manifest.json` `net_field_export_groups`) and every checkpoint declaration
(`checkpoint_export_groups` joined to `checkpoint_export_fields` on
`checkpoint_index` and `path_name_index`) of both groups; then, per build, the
set of (name, compatible_checksum) seen at each handle. No handle carries two
identities in any export, in any build, or between the main stream and the
checkpoints.

Member handles, element member handles in parentheses:

| Member (name, compatible_checksum) | 11.06-11.09 | 11.10-12.02 | 12.03-12.04 | 12.05-12.07 | 12.08-13.06 |
|---|---:|---:|---:|---:|---:|
| `253` (1175316786) | -- | -- | -- | -- | 23 |
| `bIsActive` (518428974) | 24 | 22 | 22 | 23 | 24 |
| `Status` (2380676387) | 25 | 23 | 23 | 24 | 25 |
| `ExteriorSegments` (3326329067); `Begin` (3658211664), `End` (1988330146) | 26 (27, 28) | 24 (25, 26) | 24 (25, 26) | 25 (26, 27) | 26 (27, 28) |
| `ConvexHullPoints` (3039966384); element (2749781999) | 30 (31) | 28 (29) | 28 (29) | 29 (30) | 30 (31) |
| `ConvexHullCeilings` (3975907906); element (1547370894) | -- | 31 (empty) | 31 (empty) | 32 (33 from 12.06) | 33 (34) |
| `ConvexHullTravelDistances` (1031017464); element (1566128181) | -- | 34 (empty) | 34 (empty) | 35 (36 from 12.06) | 36 (37) |
| `TJunctions` (1038854951) | -- | 37 | -- | -- | -- |
| `X` (2123226522) | 33 | 41 | 37 | 38 | 39 |
| `Y` (2134384775) | 34 | 42 | 38 | 39 | 40 |
| `TravelDistance` (956522941) | 35 | 43 | 39 | 40 | 41 |
| `Ceiling` (1959526051) | 36 | 44 | 40 | 41 | 42 |
| `Floor` (3454040167) | 37 | 45 | 41 | 42 | 43 |
| Declared class-group slots | 40 | 48 | 44 | 45 | 46 |

"Empty" means the array is declared and sent, but with no element in any
window of those builds, so its element identity is never declared there.
That is the only difference between 12.05 and 12.06-12.07, so the corpus has
six distinct declared maps over these five member layouts. 12.10, 12.11 and
13.00 carry no GroundVolumeComponent declaration at all.

The same member keeps its name and checksum across every build while its
handle moves by up to eight. The RepLayout prefix changes too: 11.06-11.09
declare `Translation` (20), `Scale3D` (21) and `FinalCount` (22);
`VolumeMaxExtentShape` appears at 19 from 11.10, with `FinalCount` at 20 or 21.
The tool therefore maps each handle through its own replay's declaration and
types the member by (name, checksum), never by handle.

## Types

| Member | Width | Read as | Evidence beyond exact consumption |
|---|---:|---|---|
| `253` | 32 | signed 32-bit | small integers 0..134; never negative, so signedness is unobserved |
| `bIsActive` | 1 | bool | -- |
| `Status` | 3 | unsigned | values 0..3; re-sent items change only this member (below) |
| `ExteriorSegments` | array | `{Begin, End}`, 8-bit unsigned each | every pair indexes the item's own polygon |
| `ConvexHullPoints` | array | 3 x f64 (192 bits) | grid, spawn and floor relations below |
| `ConvexHullCeilings`, `ConvexHullTravelDistances` | array | f32 each | one per polygon point; max / mean equal `Ceiling` / `TravelDistance` |
| `X`, `Y` | 32 | signed 32-bit | cell indices of the polygon on a regular grid |
| `TravelDistance`, `Ceiling`, `Floor` | 32 | f32 | exact relations below |
| `TJunctions` | 16 | raw, untyped | always 16 zero bits (7,232 of 7,232) |

The compatible checksums are identity keys only. The Unreal formula (CRC-32
chained over the lower-cased name, C++ type and array index) did not reproduce
any declared value under any variant tried: reflected and MSB-first CRC-32,
CRC-32C and CRC-32K; UTF-32LE, UTF-16LE and UTF-8; with and without
lower-casing and the index; seeded with 0 or with the parent array's
checksum. So a checksum is not evidence of a type here.

`TJunctions` stays raw on purpose: 16 zero bits read equally well as an empty
array or as a 16-bit value, and no non-empty instance exists to tell them
apart. `253` is kept as the replay renders it: a hardcoded engine name index
whose name this repository does not resolve.

## Validation independent of the decoder

**A second reader agrees on every value.** A separately written reader shares
no code with the tool: it selects rows per Parquet row group by dictionary
code, expands each window to a bit string, converts IEEE-754 bit patterns
with integer arithmetic, and recognizes arrays from the declaration. On all
1,018 exports it agrees with the tool on the physical ordinal and status of
all 36,661 windows, every entry's offset, width, header words and deleted and
changed counts, the identity of all 346,186 items, and all 4,129,661
item-level member values including the 4,867,849 array elements inside them,
with 0 mismatches. Planting any one of ten perturbations into a copy of the
tool's output -- one ulp on a float, a grid coordinate, a reversed polygon, an
item ID, a bool written as an integer, a dropped item, a segment index, an
entry offset, a window status, a hull view -- is reported as a mismatch.

**Members that are serialized separately agree with each other.** Counts are
items per route, bare / declared.

| Relation | Bare | Declared |
|---|---:|---:|
| `Floor` equals the lowest polygon Z rounded to f32, builds 12.06-13.06 | 323,040 / 323,040 | 5,192 / 5,192 |
| The same, builds 11.06-12.05 | 17,872 / 17,954 | -- |
| `Ceiling` equals the largest per-point ceiling (non-empty arrays) | 323,040 / 323,040 | 5,192 / 5,192 |
| `TravelDistance` equals the mean per-point travel distance (1e-6 relative; largest deviation 1.35e-6) | 323,040 / 323,040 | 5,192 / 5,192 |
| One per-point ceiling and travel distance per polygon point | 323,040 / 323,040 | 5,192 / 5,192 |
| Every `ExteriorSegments` pair indexes the polygon | 232,637 / 232,637 | 2,934 / 2,934 |
| `Ceiling` above `Floor` | 340,975 / 340,994 | 5,192 / 5,192 |

The 82 older-build floors that differ are all in 11.06-12.05, before the
per-point arrays carried data; there the relation is not exact. Of the 19
cells without a ceiling above the floor, 18 have the two equal and one, in
12.04, has the ceiling lower.

**The polygons sit on a grid at the owner's spawn.** Per object, the cell size
is the most common side of its square four-point polygons, the origin is the
most common corner offset of those squares, and the frame is the best of the
eight axis-aligned orientations. Each item's polygon must then lie inside the
cell its own `X`/`Y` name, within 0.01 units.

| Grid check | Bare | Declared |
|---|---:|---:|
| Items inside their own cell | 340,780 / 340,842 | 4,924 / 5,192 |
| Objects fully explained | 9,438 / 9,453 | 15 / 62 |
| Objects without a square cell (not measured) | 13 (152 items) | 0 |
| Cell size | 160 on 9,437 objects, 158 on 16 | 200 on 62 |
| Frame against the owner's spawn yaw (objects whose owner has one) | world-aligned on 9,303 / 9,304, whatever the yaw | the yaw rounded to a multiple of 90 degrees on 62 / 62 |
| Grid origin to owner spawn XY, median / p99 / max | 0.041 / 1.03 / 160.0 | 0.036 / 0.07 / 1.02 |

The spawn position in `actors.parquet` carries one decimal, so a median of
0.04 puts the grid origin at the spawn; the yaw is a separate column the fit
never reads, and it predicts the declared-class frame every time. 149 bare
objects have an owner without a spawn yaw. The declared-class misses are
cells with `Status` 1-3 whose points reach at most 0.124 cells beyond the
named cell. Of the 15 bare objects not fully explained, 13 have points at most
0.5 cells outside the named cell, and two are fits the estimate gets wrong: a
158 size taken from a clipped square, and a mirrored frame explaining 5 of 17
items.

**The rest of the geometry.**

- Per cell, the polygon point farthest from the owner's spawn XY: bare
  median 440, max 615 units; declared median 849, max 1,737.
- `Floor` minus the owner's spawn Z: bare median -1.0 (p01 -306, p99 97);
  declared median -115.
- Inside the XY extent of every other actor's spawn position plus 1,000
  units: 340,928 / 340,994 bare, 5,192 / 5,192 declared. The 66 exceptions
  belong to three owners; two of them (64 items) spawned outside that extent
  themselves.
- Polygon orientation: 340,982 counter-clockwise, 11 clockwise, 1 zero area
  (bare); all 5,192 counter-clockwise (declared). 77 bare polygons have an
  area below 1 square unit.
- Not every polygon named `ConvexHullPoints` is convex: 522 bare and 166
  declared polygons turn both ways. 519 of the 522 and all 166 have
  `Status` 1-3.
- A per-point travel distance, in cells, is at least the straight-line
  distance from the grid origin within 0.001 for 1,491,343 of 1,494,461 bare
  points and 20,813 of 21,327 declared points (median ratio 1.07). Its exact
  meaning, including where the declared class measures from, is not
  established.

**Time and identity.**

- No cell update precedes its owner actor's first `open` or follows its last
  `close`: 0 and 0 of 346,186. 1,894 updates belong to owners with no `close`
  in the replay.
- 120 updates re-send an item ID already seen on the same object. All 120
  change `Status` (2 to 3 in 119, 1 to 3 in one) and nothing else: `253`,
  `X`, `Y` and `TravelDistance` never change for an item ID.
- Per object, the largest `TravelDistance` among newly appearing items never
  decreases from window to window on 8,810 of 9,466 bare objects and on all
  62 declared ones: new cells mostly appear in order of increasing
  `TravelDistance`, but not strictly.

## Tests and mutations

`tools/tests/test_extract_ground_volumes.py` builds every fixture bit by bit
from the grammar above; no replay bytes are used. Each of 26 mutations applied
to a scratch copy of the tool made at least one test fail, and in each case
the test named for the broken property:

| Mutation | Test that fails |
|---|---|
| Member handle read as `encoded`, not `encoded - 1` | `test_member_handle_is_encoded_minus_one` |
| Support bit not checked | `test_support_bit_must_be_set` |
| Unconsumed entry bits accepted | `test_unconsumed_entry_bits_reject` |
| Scalar width not checked | `test_scalar_width_must_match_type` |
| f32 read as an integer; vector read as two doubles | `test_floats_are_ieee754_and_vectors_three_doubles` |
| Array index order / bounds / completeness not checked | `test_array_elements_must_be_whole_and_in_order` |
| Unconsumed array bits accepted | `test_array_payload_must_close_exactly` |
| Element missing a member accepted | `test_element_missing_a_member_rejects` |
| Member placement or duplicates not checked | `test_member_placement_and_declaration` |
| Route identity not checked | `test_route_identity_is_checked` |
| Unmeasured build accepted | `test_unmeasured_builds_and_checkpoint_rows_stay_raw` |
| Declaration conflict ignored | `test_declaration_conflict_is_detected` |
| Slot count not required | `test_cnc_slot_count_must_be_declared_and_agree` |
| ClassNetCache handle hardcoded to one bit | `test_cnc_handle_width_follows_declared_slots` |
| ClassNetCache field identity not checked | `test_cnc_field_must_be_declared_fragment_info` |
| Non-finite floats accepted | `test_nonfinite_float_rejects` |
| 32-bit values read unsigned | `test_integers_are_signed_32_bit` |
| Untyped member width not checked | `test_untyped_member_kept_raw_and_counted` |
| Counters not written when zero | `test_every_counter_is_written_even_when_zero` |
| Input change during a run not detected | `test_changed_input_does_not_publish` |
| Object outer not compared with the actor; unresolved object not counted | `test_object_outer_is_checked_against_the_actor` |

## What is not established

- The meaning of `Status` values, `bIsActive` (true on 994 of 340,994 bare
  cells but 5,176 of 5,192 declared ones), `253`, the per-point travel
  distance and `TJunctions`.
- Which player or ability cast a volume belongs to, beyond its owner actor's
  class, and whether a cell's presence means an effect is applied there.
- Anything in the checkpoint stream: neither route has a checkpoint row.
- Builds outside the measured list. `ACCEPTED_BUILDS` in the tool is exactly
  the builds with windows in this corpus; 12.10, 12.11 and 13.00 have none,
  and the declared-class route has none before 12.09 or in 13.06. Rows from
  any other build are kept raw and rejected as `unvalidated_build`.
- The RepLayout prefix of the bare rows (`FinalCount`,
  `VolumeMaxExtentShape`, attachment and scale) stays unnamed in the export;
  this tool reads only the ClassNetCache windows.

Parquet is unchanged. Two parser changes would move this into the export: a
component remap of the `PatchVolume` subobject to
`/Script/DynamicVolume.GroundVolumeComponent`, which would name the prefix
rows; and a custom-delta reader for `FragmentInfo` -- the FastArray body in
`crates/vrf-decode/src/fastarray.rs` with no per-item checksum, then the
member grammar above -- emitting item members as rows typed by declared
(name, checksum). Neither is attempted here.

Private evidence -- the declaration survey, the per-export outputs, the second
reader's comparison, the validation and route-completeness runs and the
mutation log -- is retained outside the repository. This document quotes only
aggregate counts: no replay bytes, paths or player data.
