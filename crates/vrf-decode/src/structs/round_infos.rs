//! `OwnerExclusivePlayerInfo.RoundInfos` -- per-player per-round credit.

use vrf_bitio::BitReader;

use super::Result;
use super::framing::{decode_elements, member_name};

/// Names this blob in error messages.
const CONTEXT: &str = "RoundInfos";

/// A single player round-info entry (per-round economy for one player).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerRoundInfo {
    /// Zero-based index within the array (round index within this update).
    pub index: u32,
    /// Round number. `None` if not present in this update.
    pub round_number: Option<i32>,
    /// Money at the start of the round.
    pub start_of_round_money: Option<i32>,
    /// Loadout value at the start of the round.
    pub start_of_round_loadout_value: Option<i32>,
    /// Money at the end of the round (the analytically critical field).
    pub end_of_round_money: Option<i32>,
    /// Loadout value at the end of the round.
    pub end_of_round_loadout_value: Option<i32>,
}

/// Decode an `OwnerExclusivePlayerInfo.RoundInfos` blob.
///
/// # Wire layout
///
/// Standard UE RepLayout dynamic-array framing (see module docs). Members are
/// selected by declared name; these five kept their 40..=44 handles through
/// 13.02 by luck, not guarantee. See docs/archive/PROJECT_STATUS.md 26-G.
///
/// # Arguments
///
/// * `reader` - A `BitReader` positioned at the start of the blob, with
///   `len_bits()` equal to the declared bit count.
/// * `declared` - The enclosing group's net field export names indexed by
///   handle. See [`super::round_results::decode_round_results`] on why an
///   empty slice is an error rather than a fallback.
pub fn decode_round_infos(
    reader: &mut BitReader<'_>,
    declared: &[Option<&str>],
) -> Result<Vec<PlayerRoundInfo>> {
    decode_elements(
        reader,
        CONTEXT,
        |handle| member_name(declared, handle, CONTEXT),
        |index| PlayerRoundInfo {
            index,
            round_number: None,
            start_of_round_money: None,
            start_of_round_loadout_value: None,
            end_of_round_money: None,
            end_of_round_loadout_value: None,
        },
        |row, name, sub| {
            let slot = match name {
                "RoundNumber" => &mut row.round_number,
                "StartOfRoundMoney" => &mut row.start_of_round_money,
                "StartOfRoundLoadoutValue" => &mut row.start_of_round_loadout_value,
                "EndOfRoundMoney" => &mut row.end_of_round_money,
                "EndOfRoundLoadoutValue" => &mut row.end_of_round_loadout_value,
                _ => return Ok(None),
            };
            // Every member here is a fixed 32-bit Int32, so its window is fully
            // spoken for. A wider one means the field is not the Int32 this
            // decoder believes it is, and the half it read is not a value.
            *slot = Some(sub.read_i32()?);
            Ok(Some(name))
        },
    )
}
