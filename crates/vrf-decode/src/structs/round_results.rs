//! `BombGameState.RoundResults` -- per-round winning team and outcome.

use vrf_bitio::BitReader;

use super::framing::{decode_elements, member_name, read_narrow_byte};
use super::{Result, StructBlobError};
use crate::decode::scalar::read_fname;

/// Names this blob in error messages.
const CONTEXT: &str = "RoundResults";

/// The role a team played during the round.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum AresTeamRole {
    None = 0,
    Attacker = 1,
    Defender = 2,
    FreeForAll = 3,
    Any = 4,
    RoleCount = 5,
}

impl AresTeamRole {
    fn from_byte(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::None),
            1 => Some(Self::Attacker),
            2 => Some(Self::Defender),
            3 => Some(Self::FreeForAll),
            4 => Some(Self::Any),
            5 => Some(Self::RoleCount),
            _ => Option::None,
        }
    }

    /// The snake_case spelling exporters write; here, with the enum, so a new
    /// variant fails to compile where it is added.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Attacker => "attacker",
            Self::Defender => "defender",
            Self::FreeForAll => "free_for_all",
            Self::Any => "any",
            Self::RoleCount => "role_count",
        }
    }
}

/// How the round ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum AresRoundOutcome {
    Elimination = 0,
    Defuse = 1,
    Detonate = 2,
    TimeExpired = 3,
    Cheat = 4,
    Surrendered = 5,
    RoundOutcomeCount = 6,
    Invalid = 7,
}

impl AresRoundOutcome {
    fn from_byte(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Elimination),
            1 => Some(Self::Defuse),
            2 => Some(Self::Detonate),
            3 => Some(Self::TimeExpired),
            4 => Some(Self::Cheat),
            5 => Some(Self::Surrendered),
            6 => Some(Self::RoundOutcomeCount),
            7 => Some(Self::Invalid),
            _ => Option::None,
        }
    }

    /// The snake_case spelling exporters write. See [`AresTeamRole::as_str`].
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Elimination => "elimination",
            Self::Defuse => "defuse",
            Self::Detonate => "detonate",
            Self::TimeExpired => "time_expired",
            Self::Cheat => "cheat",
            Self::Surrendered => "surrendered",
            Self::RoundOutcomeCount => "round_outcome_count",
            Self::Invalid => "invalid",
        }
    }
}

/// A single round result entry.
#[derive(Debug, Clone, PartialEq)]
pub struct RoundResult {
    /// Zero-based round number.
    pub round_number: u32,
    /// The team name that won (e.g. "Blue", "Red"). `None` if not present.
    pub winning_team: Option<String>,
    /// The role of the winning team. `None` if not present in this update.
    pub winning_team_role: Option<AresTeamRole>,
    /// How the round ended. `None` if not present in this update.
    pub round_result: Option<AresRoundOutcome>,
}

/// Decode a `BombGameState.RoundResults` blob; see the module docs for
/// `reader`, `declared` and why members are selected by declared name.
pub fn decode_round_results(
    reader: &mut BitReader<'_>,
    declared: &[Option<&str>],
) -> Result<Vec<RoundResult>> {
    decode_elements(
        reader,
        CONTEXT,
        |handle| member_name(declared, handle, CONTEXT),
        |round_number| RoundResult {
            round_number,
            winning_team: None,
            winning_team_role: None,
            round_result: None,
        },
        |row, name, sub| {
            match name {
                "WinningTeam" => row.winning_team = Some(read_fname::<StructBlobError>(sub, 1024)?),
                // An unusable width or an unknown value is an error, never
                // `None`, which means "not sent in this update".
                "WinningTeamRole" => {
                    let v = read_narrow_byte(sub, name, CONTEXT)?;
                    row.winning_team_role = Some(AresTeamRole::from_byte(v).ok_or(
                        StructBlobError::UnknownEnumValue {
                            enum_name: "AresTeamRole",
                            value: v,
                            context: CONTEXT,
                        },
                    )?);
                }
                "RoundResult" => {
                    let v = read_narrow_byte(sub, name, CONTEXT)?;
                    row.round_result = Some(AresRoundOutcome::from_byte(v).ok_or(
                        StructBlobError::UnknownEnumValue {
                            enum_name: "AresRoundOutcome",
                            value: v,
                            context: CONTEXT,
                        },
                    )?);
                }
                // Opaque nested array at two handles, skipped EXPLICITLY: the
                // consumption check would read a fall-through as a member that
                // left its window unread, not one deliberately uninterpreted.
                "EliminatedTeams" => sub.skip_remaining(),
                _ => return Ok(None),
            }
            // Interpreted members consume exactly (`read_fname` is
            // self-delimiting, `read_narrow_byte` takes the whole window).
            Ok(Some(name))
        },
    )
}
