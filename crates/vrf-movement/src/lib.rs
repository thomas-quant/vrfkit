//! Decoder for the VALORANT remote-character-movement RPC payload.
//!
//! # Wire format overview
//!
//! The `ReplaysClientReceiveRemoteCharacterUpdatesSingleArrayNoAutonomous` RPC
//! carries a batch of per-character movement updates. Each update contains a
//! `ComponentDataStream` -- a nested binary protocol encoding one or more
//! movement "moves" (position, rotation, velocity snapshots).
//!
//! ## RPC top-level structure
//!
//! ```text
//! +-------------------------------------------------------------------------+
//! | skip_bit            : 1 bit  (if false -> empty payload)                |
//! | loop (property-style framing):                                          |
//! |   encodedHandle     : IntPacked  (0 -> break)                           |
//! |   handle = encodedHandle - 1                                            |
//! |   payloadBits       : IntPacked                                         |
//! |   payload           : [payloadBits] bits                                |
//! |   * handle 1 = RemoteCharacterUpdates array                             |
//! |   * other handles -> skip                                               |
//! +-------------------------------------------------------------------------+
//! ```
//!
//! ## RemoteCharacterUpdates array (handle 1)
//!
//! ```text
//! +-------------------------------------------------------------------------+
//! | updateCount         : IntPacked                                         |
//! | loop:                                                                   |
//! |   encodedIndex      : IntPacked  (0 -> break)                           |
//! |   index = encodedIndex - 1                                              |
//! |   update = ReadRemoteCharacterUpdate(...)                               |
//! | (trailing: if exactly 8 bits remain, consume IntPacked padding)         |
//! +-------------------------------------------------------------------------+
//! ```
//!
//! ## RemoteCharacterUpdate (per-character)
//!
//! ```text
//! +-------------------------------------------------------------------------+
//! | loop (property-style framing):                                          |
//! |   encodedHandle     : IntPacked  (0 -> break)                           |
//! |   handle = encodedHandle - 1                                            |
//! |   payloadBits       : IntPacked                                         |
//! |   payload           : [payloadBits] bits                                |
//! |   * handle 2 = ShooterCharacterNetGuidValue (u32)                       |
//! |   * handle 3 = ComponentDataStream                                      |
//! |   * other handles -> skip                                               |
//! +-------------------------------------------------------------------------+
//! ```
//!
//! ## ComponentDataStream
//!
//! ```text
//! +-------------------------------------------------------------------------+
//! | Option A: byte-wrapped envelope                                         |
//! |   byteCount       : u16  (> 0, and fits in remaining bits)              |
//! |   [byteCount*8 bits]: inner ComponentPayload                            |
//! |                                                                         |
//! | Option B: direct                                                        |
//! |   (falls through to ComponentPayload)                                   |
//! +-------------------------------------------------------------------------+
//!
//! ComponentPayload:
//! +-------------------------------------------------------------------------+
//! | movementBitCount  : u16                                                 |
//! | * if movementBitCount == 0 || > remaining -> movement uses all remaining|
//! | * else -> movement uses exactly movementBitCount bits                   |
//! | MovementSection   : [movementBitCount or remaining] bits                |
//! +-------------------------------------------------------------------------+
//!
//! MovementSection:
//! +-------------------------------------------------------------------------+
//! | magic             : u8  (must be 0x52)                                  |
//! | loop:                                                                   |
//! |   marker          : 3 bits  (0 -> break)                                |
//! |   MovementMove                                                          |
//! |   (if remaining <= 31 bits -> end; the bits stay unread, see below)     |
//! |   nextMarker expected = NextMarker(prev)                                |
//! +-------------------------------------------------------------------------+
//! ```
//!
//! ## MovementMove (single move record)
//!
//! ```text
//! +-------------------------------------------------------------------------+
//! | header            : 25 bits packed:                                     |
//! |   [0]     moveType (bool: 0=variant0, 1=variant1)                       |
//! |   [1..9]  rotationYawMultiplier (u8)                                    |
//! |   [9..17] movementState (u8)                                            |
//! |   [17..25] unusedByte (u8)                                              |
//! | rotationInput     : FixedVector (3 x u16 -> signed x 1/65536)           |
//! | timestamp         : VLQ (u32)                                           |
//! | position          : QuantizedVector (scaleFactor=100)                   |
//! | hasOptionalByte   : 1 bit                                               |
//! |   [if true] optionalByte : u8                                           |
//! | flagAndPackedAngles : 33 bits                                           |
//! |   [0]     flag48 (bool)                                                 |
//! |   [1..33] packedAngles: pitch=[0..16], yaw=[16..32]                     |
//! | IF moveType == 1 (variant 1):                                           |
//! |   variant1Flag    : 1 bit                                               |
//! |   velocity        : QuantizedVector (scaleFactor=10)                    |
//! | ELSE (variant 0):                                                       |
//! |   flagAndAngles   : 33 bits                                             |
//! |     [0]     hasExternalCharacterRef                                     |
//! |     [1..33] variant0PackedAngles (u32)                                  |
//! |   (if hasExternalCharacterRef -> error, not decoded)                    |
//! | errorSentinel     : 1 bit  (must be false)                              |
//! +-------------------------------------------------------------------------+
//! ```
//!
//! ## QuantizedVector
//!
//! ```text
//! +-------------------------------------------------------------------------+
//! | componentBitCountAndExtraInfo : SerializedInt(128)  [7 bits]            |
//! |   componentBits = value & 63                                            |
//! |   extraInfo = value >> 6                                                |
//! |                                                                         |
//! | IF componentBits > 0:                                                   |
//! |   Read 3 signed components of `componentBits` each                      |
//! |   IF extraInfo > 0: divide by scaleFactor                               |
//! | ELIF extraInfo == 0:                                                    |
//! |   3 x f32                                                               |
//! | ELSE:                                                                   |
//! |   3 x f64                                                               |
//! +-------------------------------------------------------------------------+
//! ```
//!
//! # Measured on real replays
//!
//! Counted on 2026-09-28 with temporary counters in `vrfkit validate`, over 80
//! replays from 24 builds, 11.06 to 13.06: 18,488,787 RPCs, 156,407,150
//! component streams, 157,457,629 moves.
//!
//! - Every stream is byte-wrapped and every inner `movementBitCount` is 0, so
//!   every movement window runs to the end of its envelope.
//! - The `<= 31` bits a section ends with after its last move are not padding.
//!   The 3 bits where the next marker would sit are `000`, and the 8 to 23
//!   bits after them are not all zero. Nothing reads them; the C# reference
//!   (`MaxMovementPaddingBits`) stops at the same place.
//! - Exactly 24 bits follow the envelope in every stream (3,753,771,600 bits in
//!   all). The decoder skips them unread, as the C# reference does; what they
//!   carry is not established.
//! - Implemented after the C# reference but never seen: the direct form, a
//!   sized movement window, the updates array's trailing 8-bit IntPacked,
//!   variant-0 moves, and the f32 and f64 QuantizedVector forms.

//! # Module map
//!
//! The nesting above is mirrored by the module layout, outermost first:
//!
//! | Module | Layer |
//! |--------|-------|
//! | `rpc` | Batch, updates array, one update, the component data stream |
//! | `moves` | The movement section and one move record |
//! | `primitives` | QuantizedVector, sign extension, the angle scale |
//! | `types` | [`MovementMove`], [`MovementUpdate`], [`RpcDecodeResult`] |
//! | `error` | [`MovementError`] |
//!
//! # Accuracy
//!
//! See the module doc at the top of `src/primitives.rs` ("These are format,
//! not style") for the validated error bounds and why the arithmetic is not
//! restyled.
//!
//! # Cargo features
//!
//! None. This crate decodes exactly one RPC, and every layer above is required
//! to locate the bits of the layer below -- there is no sub-part of it a
//! consumer could decline and still get a move out.

#![forbid(unsafe_code)]

mod error;
mod moves;
mod primitives;
mod rpc;
mod types;

pub use error::MovementError;
pub use rpc::decode_movement_rpc;
pub use types::{MovementMove, MovementUpdate, RpcDecodeResult};

#[cfg(test)]
mod tests;
