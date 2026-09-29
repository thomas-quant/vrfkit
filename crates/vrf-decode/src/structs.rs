//! Decoders for Valorant struct-array blobs that arrive as opaque raw bits.
//!
//! | Field | Export group | Purpose | Module |
//! |-------|-------------|---------|--------|
//! | `RoundResults` | `BombGameState` | Per-round winning team and outcome | `round_results` |
//! | `TeamEconomy` | `BombGameState` | Team loadout value per round | `team_economy` |
//! | `RoundInfos` | `OwnerExclusivePlayerInfo` | Per-player per-round credit | `round_infos` |
//!
//! All three use the RepLayout dynamic-array framing the effect arrays use
//! (diagrammed in the `effect` module docs); an FName member goes through
//! `FieldType::FName`'s reader with a 1024-byte string cap.
//!
//! # Members are selected by DECLARED NAME, not by handle number
//!
//! Handle numbers are a per-build layout detail: 13.02 moved `RoundResults`'
//! members down by eight, which a handle-keyed decoder reads as nothing. So the
//! decoders take, besides a `reader` over exactly the blob's declared bits,
//! `declared`: the enclosing group's field export names indexed by handle
//! (only the fixed-handle `decode_team_economy` does not). An empty declaration
//! is an error, never a fallback to build-specific numbers.
//!
//! Members and payload types, with each build's handles as a reading aid ONLY:
//!
//! - RoundResults (`BombGameState`; 13.01: 93..=96, 13.02: 81..=84):
//!   `WinningTeam` (FName), `WinningTeamRole` and `RoundResult` (enum bytes of
//!   variable width), `EliminatedTeams` (skipped; declared at TWO consecutive
//!   handles in both builds, one name arm covering both).
//! - RoundInfos (`OwnerExclusivePlayerInfo`; 40..=44 in both builds):
//!   `RoundNumber`, `StartOfRoundMoney`, `StartOfRoundLoadoutValue`,
//!   `EndOfRoundMoney`, `EndOfRoundLoadoutValue` (all Int32).
//! - TeamEconomy (`BombGameState` through 13.01; 53..=55 in the 12.01-12.05
//!   samples, 56..=58 from 12.06): `241`, the ReplicationId declared as a
//!   hardcoded FName index (IntPacked), and `LoadoutValue`,
//!   `AverageLoadoutValue` (Int32). From 13.02 the property lives in
//!   separately replicated BaseTeamState actors. `decode_team_economy` keeps
//!   the fixed 56..=58 layout; `decode_team_economy_declared` follows the
//!   declaration.

mod framing;
mod round_infos;
mod round_results;
mod team_economy;

#[cfg(test)]
mod tests;

pub use round_infos::{PlayerRoundInfo, decode_round_infos};
pub use round_results::{AresRoundOutcome, AresTeamRole, RoundResult, decode_round_results};
pub use team_economy::{TeamEconomyUpdate, decode_team_economy, decode_team_economy_declared};

/// Errors that can occur while decoding a struct-array blob.
#[derive(Debug, Clone, thiserror::Error)]
pub enum StructBlobError {
    /// The underlying bit reader hit EOF or produced a malformed primitive.
    #[error("bit read: {0}")]
    BitIo(#[from] vrf_bitio::BitError),

    /// A primitive nested in the blob was structurally invalid.
    #[error("field decode: {0}")]
    Decode(#[from] crate::DecodeError),

    /// The declared array element count exceeds a sane maximum.
    #[error("array count {count} exceeds maximum {max}")]
    ArrayCountTooLarge { count: u32, max: u32 },

    /// An element index is out of bounds relative to the declared count.
    #[error("element index {index} >= declared count {count}")]
    IndexOutOfBounds { index: u32, count: u32 },

    /// A field payload declared more bits than remain in the stream.
    #[error("field payload {bits} bits exceeds remaining {remaining}")]
    PayloadTooLarge { bits: u32, remaining: u64 },

    /// An unexpected field handle (`decode_team_economy` only).
    #[error("unsupported field handle {handle} in {context}")]
    UnsupportedHandle { handle: u32, context: &'static str },

    /// The replay declares no name for a handle the blob carries, so there is
    /// nothing to select a member with.
    #[error("undeclared field handle {handle} in {context}")]
    UndeclaredHandle { handle: u32, context: &'static str },

    /// The handle is declared, under a name this decoder has no arm for --
    /// the shape this takes when a build renames or adds a member.
    #[error("unsupported member {name} (handle {handle}) in {context}")]
    UnsupportedMember {
        name: String,
        handle: u32,
        context: &'static str,
    },

    /// Too many fields in a single element.
    #[error("too many fields in element ({context})")]
    TooManyFields { context: &'static str },

    /// A byte enum carried a value this decoder has no variant for. Reported,
    /// because `None` means "not sent".
    #[error("{enum_name} has no variant for value {value} in {context}")]
    UnknownEnumValue {
        enum_name: &'static str,
        value: u8,
        context: &'static str,
    },

    /// An enum member declared no bits or more than a byte of payload.
    #[error("{name} enum width {bits} is invalid in {context}")]
    InvalidEnumWidth {
        name: String,
        bits: u64,
        context: &'static str,
    },

    /// Bits remain after the blob should have been fully consumed.
    #[error("not fully consumed: {remaining} bits left")]
    NotFullyConsumed { remaining: u64 },

    /// A member did not consume its field window. The parent is aligned past
    /// the window whatever the member read, so only this check sees it.
    #[error("{name} (handle {handle}) left {remaining} of its {declared} bits unread in {context}")]
    MemberNotFullyConsumed {
        name: String,
        handle: u32,
        declared: u32,
        remaining: u64,
        context: &'static str,
    },
}

pub type Result<T> = core::result::Result<T, StructBlobError>;
