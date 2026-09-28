//! Pinned rows from replay `02d4d478`, with the values the C# reference
//! produced for the same bytes.

use super::*;
use crate::test_bits::{BitWriter, hex};
use vrf_bitio::BitReader;

// -- Declarations ---------------------------------------------------------
//
// Every test supplies the declaration its bytes were recorded under: the same
// members sit at different handles on 13.01 and 13.02.

/// `BombGameState_C` on build 13.01: RoundResults members at 93..=96.
fn bomb_game_state_1301() -> Vec<Option<&'static str>> {
    let mut d = vec![None; 98];
    d[92] = Some("RoundResults");
    d[93] = Some("WinningTeam");
    d[94] = Some("WinningTeamRole");
    d[95] = Some("RoundResult");
    d[96] = Some("EliminatedTeams");
    d[97] = Some("EliminatedTeams");
    // Handle 50 is the match-winner scalar, NOT a RoundResults member; it
    // shares the name, so a search by name could land here.
    d[50] = Some("WinningTeam");
    d
}

/// `BombGameState_C` on build 13.02: the same members, eight handles lower.
///
/// 13.02 dropped `TeamEconomy` and `TeamComponents` and added `TeamStates`.
fn bomb_game_state_1302() -> Vec<Option<&'static str>> {
    let mut d = vec![None; 86];
    d[80] = Some("RoundResults");
    d[81] = Some("WinningTeam");
    d[82] = Some("WinningTeamRole");
    d[83] = Some("RoundResult");
    d[84] = Some("EliminatedTeams");
    d[85] = Some("EliminatedTeams");
    d[50] = Some("WinningTeam");
    d[52] = Some("TeamStates");
    d[53] = Some("TeamStates");
    d
}

/// `OwnerExclusivePlayerInfo`, identical on both builds measured.
fn owner_exclusive_player_info() -> Vec<Option<&'static str>> {
    let mut d = vec![None; 45];
    d[39] = Some("RoundInfos");
    d[40] = Some("RoundNumber");
    d[41] = Some("StartOfRoundMoney");
    d[42] = Some("StartOfRoundLoadoutValue");
    d[43] = Some("EndOfRoundMoney");
    d[44] = Some("EndOfRoundLoadoutValue");
    d
}

// -- RoundResults tests ---------------------------------------------------

/// Row 0 from replay 02d4d478, t=84942ms.
/// C# output: [{RoundNumber:0, WinningTeam:"Red", WinningTeamRole:attacker, RoundResult:elimination}]
#[test]
fn round_results_row0_red_attacker_elimination() {
    let data = hex("0202bcc208000000a4cac800000000007c0d028c00c2800202c420250400000000");
    let mut r = BitReader::with_bit_len(&data, 264).unwrap();
    let results = decode_round_results(&mut r, &bomb_game_state_1301()).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].round_number, 0);
    assert_eq!(results[0].winning_team.as_deref(), Some("Red"));
    assert_eq!(results[0].winning_team_role, Some(AresTeamRole::Attacker));
    assert_eq!(results[0].round_result, Some(AresRoundOutcome::Elimination));
}

/// Row 4 from replay 02d4d478, t=580448ms.
/// C# output: [{RoundNumber:4, WinningTeam:"Blue", WinningTeamRole:defender, RoundResult:time_expired}]
#[test]
fn round_results_row4_blue_defender_time_expired() {
    let data = hex("0a0abcd20a00000084d8eaca00000000007c0d048c300000");
    let mut r = BitReader::with_bit_len(&data, 192).unwrap();
    let results = decode_round_results(&mut r, &bomb_game_state_1301()).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].round_number, 4);
    assert_eq!(results[0].winning_team.as_deref(), Some("Blue"));
    assert_eq!(results[0].winning_team_role, Some(AresTeamRole::Defender));
    assert_eq!(results[0].round_result, Some(AresRoundOutcome::TimeExpired));
}

/// Row 6 from replay 02d4d478, t=796414ms.
/// C# output: [{RoundNumber:6, WinningTeam:"Blue", WinningTeamRole:defender, RoundResult:defuse}]
#[test]
fn round_results_row6_blue_defender_defuse() {
    let data = hex("0e0ebcd20a00000084d8eaca00000000007c0d048c100000");
    let mut r = BitReader::with_bit_len(&data, 192).unwrap();
    let results = decode_round_results(&mut r, &bomb_game_state_1301()).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].round_number, 6);
    assert_eq!(results[0].winning_team.as_deref(), Some("Blue"));
    assert_eq!(results[0].winning_team_role, Some(AresTeamRole::Defender));
    assert_eq!(results[0].round_result, Some(AresRoundOutcome::Defuse));
}

/// A 0-bit blob is an error for all three decoders: not even the element
/// count is there. (The export path never hands a decoder zero bits.)
#[test]
fn a_zero_bit_blob_is_an_error_for_every_decoder() {
    let eof = |err: StructBlobError| {
        matches!(
            err,
            StructBlobError::BitIo(vrf_bitio::BitError::Eof {
                position: 0,
                length: 0,
                requested: 8
            })
        )
    };
    let blob = || BitReader::with_bit_len(&[], 0).unwrap();
    let err = decode_round_results(&mut blob(), &bomb_game_state_1301()).unwrap_err();
    assert!(eof(err.clone()), "RoundResults: {err:?}");
    let err = decode_round_infos(&mut blob(), &owner_exclusive_player_info()).unwrap_err();
    assert!(eof(err.clone()), "RoundInfos: {err:?}");
    let err = decode_team_economy(&mut blob()).unwrap_err();
    assert!(eof(err.clone()), "TeamEconomy: {err:?}");
}

// -- RoundResults on build 13.02 ------------------------------------------

/// Round 0 from replay `f1110ea5`, build `++Ares-Core+release-13.02`,
/// t=72684ms. Members sit at 81..=84 here; the 13.01 decoder read nothing.
#[test]
fn round_results_1302_round0() {
    let data = hex("0202a4d20a00000084d8eaca00000000004c0d848a00aa800202ac20f50200000000");
    let mut r = BitReader::with_bit_len(&data, 272).unwrap();
    let results = decode_round_results(&mut r, &bomb_game_state_1302()).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].round_number, 0);
    assert_eq!(results[0].winning_team.as_deref(), Some("Blue"));
    assert_eq!(results[0].winning_team_role, Some(AresTeamRole::Defender));
    assert_eq!(results[0].round_result, Some(AresRoundOutcome::Elimination));
}

/// 13.02 bytes under the 13.01 declaration must FAIL by name, not return an
/// empty vector: that is how a whole build's missing match scores looked like
/// a clean export.
#[test]
fn round_results_1302_bytes_under_1301_declaration_is_an_error() {
    let data = hex("0202a4d20a00000084d8eaca00000000004c0d848a00aa800202ac20f50200000000");
    let mut r = BitReader::with_bit_len(&data, 272).unwrap();
    let err = decode_round_results(&mut r, &bomb_game_state_1301()).unwrap_err();
    assert!(
        matches!(
            err,
            StructBlobError::UndeclaredHandle {
                handle: 81,
                context: "RoundResults"
            }
        ),
        "expected an undeclared-handle error naming handle 81, got {err:?}"
    );
}

/// The mirror: 13.01 bytes under the 13.02 declaration fail at handle 93, so
/// the decoder is keyed on the declaration, not on either set of numbers.
#[test]
fn round_results_1301_bytes_under_1302_declaration_is_an_error() {
    let data = hex("0202bcc208000000a4cac800000000007c0d028c00c2800202c420250400000000");
    let mut r = BitReader::with_bit_len(&data, 264).unwrap();
    let err = decode_round_results(&mut r, &bomb_game_state_1302()).unwrap_err();
    assert!(
        matches!(err, StructBlobError::UndeclaredHandle { handle: 93, .. }),
        "expected an undeclared-handle error naming handle 93, got {err:?}"
    );
}

/// An empty declaration is an error, not an empty result. There is no safe
/// fallback set of handle numbers to guess with.
#[test]
fn round_results_with_no_declaration_is_an_error() {
    let data = hex("0202bcc208000000a4cac800000000007c0d028c00c2800202c420250400000000");
    let mut r = BitReader::with_bit_len(&data, 264).unwrap();
    assert!(decode_round_results(&mut r, &[]).is_err());
}

/// A handle that IS declared, under a name with no arm, names itself in the
/// error. This is the shape a renamed or added member takes.
#[test]
fn round_results_unknown_member_name_is_reported_by_name() {
    let data = hex("0202bcc208000000a4cac800000000007c0d028c00c2800202c420250400000000");
    let mut r = BitReader::with_bit_len(&data, 264).unwrap();
    let mut declared = bomb_game_state_1301();
    declared[93] = Some("WinningTeamV2");
    let err = decode_round_results(&mut r, &declared).unwrap_err();
    match err {
        StructBlobError::UnsupportedMember { name, handle, .. } => {
            assert_eq!(name, "WinningTeamV2");
            assert_eq!(handle, 93);
        }
        other => panic!("expected UnsupportedMember, got {other:?}"),
    }
}

// -- TeamEconomy tests ----------------------------------------------------

/// Row 0 from replay 02d4d478, t=7ms. Initial spawn with ReplicationIds.
/// C# output: [{Index:0, LV:0, ALV:0, RepId:272}, {Index:1, LV:0, ALV:0, RepId:274}]
#[test]
fn team_economy_row0_initial_spawn() {
    let data = hex("0402722021047440000000007640000000000004722025047440000000007640000000000000");
    let mut r = BitReader::with_bit_len(&data, 304).unwrap();
    let results = decode_team_economy(&mut r).unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].index, 0);
    assert_eq!(results[0].replication_id, Some(272));
    assert_eq!(results[0].loadout_value, Some(0));
    assert_eq!(results[0].average_loadout_value, Some(0));
    assert_eq!(results[1].index, 1);
    assert_eq!(results[1].replication_id, Some(274));
    assert_eq!(results[1].loadout_value, Some(0));
    assert_eq!(results[1].average_loadout_value, Some(0));
}

/// Row 1 from replay 02d4d478, t=62ms.
/// C# output: [{Index:0, LV:4350, ALV:870, RepId:null}, {Index:1, LV:4150, ALV:830, RepId:null}]
#[test]
fn team_economy_row1_round_start() {
    let data = hex("04027440fe100000764066030000000474403610000076403e0300000000");
    let mut r = BitReader::with_bit_len(&data, 240).unwrap();
    let results = decode_team_economy(&mut r).unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].index, 0);
    assert_eq!(results[0].replication_id, None);
    assert_eq!(results[0].loadout_value, Some(4350));
    assert_eq!(results[0].average_loadout_value, Some(870));
    assert_eq!(results[1].index, 1);
    assert_eq!(results[1].replication_id, None);
    assert_eq!(results[1].loadout_value, Some(4150));
    assert_eq!(results[1].average_loadout_value, Some(830));
}

/// Row 2 from replay 02d4d478, t=92033ms.
/// C# output: [{Index:0, LV:21200, ALV:4240}, {Index:1, LV:11600, ALV:2320}]
#[test]
fn team_economy_row2_midgame() {
    let data = hex("04027440d052000076409010000000047440502d00007640100900000000");
    let mut r = BitReader::with_bit_len(&data, 240).unwrap();
    let results = decode_team_economy(&mut r).unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].index, 0);
    assert_eq!(results[0].loadout_value, Some(21200));
    assert_eq!(results[0].average_loadout_value, Some(4240));
    assert_eq!(results[1].index, 1);
    assert_eq!(results[1].loadout_value, Some(11600));
    assert_eq!(results[1].average_loadout_value, Some(2320));
}

// -- RoundInfos tests -----------------------------------------------------

/// First RoundInfos row from replay 02d4d478, t=91927ms, actor 196.
/// C# base64: "AgJSQAAAAABUQAAAAABWQAAAAABYQGwHAABaQAAAAAAAAA=="
/// Decoded: [{Index:0, RN:0, SM:0, SL:0, EM:1900, EL:0}]
#[test]
fn round_infos_row0_end_of_round1() {
    let data = hex("020252400000000054400000000056400000000058406c0700005a40000000000000");
    let mut r = BitReader::with_bit_len(&data, 272).unwrap();
    let results = decode_round_infos(&mut r, &owner_exclusive_player_info()).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].index, 0);
    assert_eq!(results[0].round_number, Some(0));
    assert_eq!(results[0].start_of_round_money, Some(0));
    assert_eq!(results[0].start_of_round_loadout_value, Some(0));
    assert_eq!(results[0].end_of_round_money, Some(1900));
    assert_eq!(results[0].end_of_round_loadout_value, Some(0));
}

/// Second RoundInfos row from replay 02d4d478, t=91927ms, actor 184.
/// C# base64: "AgJSQAAAAABUQAAAAABWQAAAAABYQNAHAABaQMgAAAAAAA=="
/// Decoded: [{Index:0, RN:0, SM:0, SL:0, EM:2000, EL:200}]
#[test]
fn round_infos_row1_different_player() {
    let data = hex("02025240000000005440000000005640000000005840d00700005a40c80000000000");
    let mut r = BitReader::with_bit_len(&data, 272).unwrap();
    let results = decode_round_infos(&mut r, &owner_exclusive_player_info()).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].index, 0);
    assert_eq!(results[0].round_number, Some(0));
    assert_eq!(results[0].start_of_round_money, Some(0));
    assert_eq!(results[0].start_of_round_loadout_value, Some(0));
    assert_eq!(results[0].end_of_round_money, Some(2000));
    assert_eq!(results[0].end_of_round_loadout_value, Some(200));
}

/// Third RoundInfos row from replay 02d4d478, t=91927ms, actor 240.
/// C# base64: "AgJSQAAAAABUQAAAAABWQAAAAABYQDQIAABaQFgCAAAAAA=="
/// Decoded: [{Index:0, RN:0, SM:0, SL:0, EM:2100, EL:600}]
#[test]
fn round_infos_row2_another_player() {
    let data = hex("02025240000000005440000000005640000000005840340800005a40580200000000");
    let mut r = BitReader::with_bit_len(&data, 272).unwrap();
    let results = decode_round_infos(&mut r, &owner_exclusive_player_info()).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].index, 0);
    assert_eq!(results[0].end_of_round_money, Some(2100));
    assert_eq!(results[0].end_of_round_loadout_value, Some(600));
}

// -- TooManyFields boundary (B12) -----------------------------------------

/// The framing module's `MAX_FIELDS_PER_ELEMENT`, repeated rather than
/// imported on purpose: a changed cap must fail the boundary tests.
const MAX_FIELDS_PER_ELEMENT: u32 = 8;

/// A `RoundInfos` blob whose single, correctly terminated element carries `n`
/// zero `RoundNumber` fields (handle 40), each `handle(1) bitcount(1)
/// payload(4)` bytes; a one-byte IntPacked `v < 128` is the byte `v << 1`.
fn round_infos_with_n_fields(n: u32) -> (Vec<u8>, u64) {
    /// One-byte IntPacked for values that fit in 7 bits (no continuation).
    const fn ip1(v: u8) -> u8 {
        v << 1
    }
    let mut bytes = Vec::new();
    bytes.push(ip1(1)); // array count = 1
    bytes.push(ip1(1)); // element encoded_index = 1 (index 0)
    for _ in 0..n {
        bytes.push(ip1(41)); // encoded_handle = 41 (handle 40 = "RoundNumber")
        bytes.push(ip1(32)); // bit_count = 32
        bytes.extend_from_slice(&[0u8; 4]); // 32-bit zero payload
    }
    bytes.push(0); // field terminator (encoded_handle = 0)
    bytes.push(0); // element terminator (encoded_index = 0)
    let bits = u64::try_from(bytes.len() * 8).unwrap();
    (bytes, bits)
}

/// MAX fields in one element must parse: the guard rejects only the MAX+1-th.
#[test]
fn round_infos_accepts_max_fields_per_element() {
    let (bytes, bits) = round_infos_with_n_fields(MAX_FIELDS_PER_ELEMENT);
    let mut r = BitReader::with_bit_len(&bytes, bits).unwrap();
    let results = decode_round_infos(&mut r, &owner_exclusive_player_info()).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].round_number, Some(0));
}

/// The MAX+1-th field is rejected as `TooManyFields`.
#[test]
fn round_infos_rejects_one_more_than_max_fields() {
    let (bytes, bits) = round_infos_with_n_fields(MAX_FIELDS_PER_ELEMENT + 1);
    let mut r = BitReader::with_bit_len(&bytes, bits).unwrap();
    let err = decode_round_infos(&mut r, &owner_exclusive_player_info()).unwrap_err();
    assert!(
        matches!(
            err,
            StructBlobError::TooManyFields {
                context: "RoundInfos"
            }
        ),
        "expected TooManyFields, got {err:?}"
    );
}

// -- Unknown enum values --------------------------------------------------

/// One element carrying a single member at `handle`, whose payload window is
/// `width` bits holding `value` (zero past its 32 bits).
fn one_member(handle: u32, value: u32, width: u32) -> (Vec<u8>, u64) {
    let mut bits = BitWriter::new();
    bits.int_packed(1); // element count
    bits.int_packed(1); // encoded index -> element 0
    bits.int_packed(handle + 1);
    bits.int_packed(width);
    bits.bits(u64::from(value), width);
    bits.int_packed(0); // end of element
    bits.int_packed(0); // end of array
    let (data, bit_len) = bits.finish();
    (data, u64::from(bit_len))
}

/// An `AresTeamRole` value outside the declared variants is `UnknownEnumValue`,
/// not an absent field.
#[test]
fn round_results_unknown_team_role_is_an_error_not_an_absent_field() {
    // Handle 94 is WinningTeamRole on 13.01. Role 7 is past RoleCount (5).
    let (data, bit_len) = one_member(94, 7, 3);
    let mut r = BitReader::with_bit_len(&data, bit_len).unwrap();
    let err = decode_round_results(&mut r, &bomb_game_state_1301())
        .expect_err("an unknown role must not decode as an absent field");
    assert!(
        matches!(
            err,
            StructBlobError::UnknownEnumValue {
                enum_name: "AresTeamRole",
                value: 7,
                ..
            }
        ),
        "got {err:?}"
    );
}

/// The same for `AresRoundOutcome`, whose declared range is 0..=7, so an
/// unknown value needs a four-bit payload.
#[test]
fn round_results_unknown_outcome_is_an_error_not_an_absent_field() {
    // Handle 95 is RoundResult on 13.01. Outcome 8 is past Invalid (7).
    let (data, bit_len) = one_member(95, 8, 4);
    let mut r = BitReader::with_bit_len(&data, bit_len).unwrap();
    let err = decode_round_results(&mut r, &bomb_game_state_1301())
        .expect_err("an unknown outcome must not decode as an absent field");
    assert!(
        matches!(
            err,
            StructBlobError::UnknownEnumValue {
                enum_name: "AresRoundOutcome",
                value: 8,
                ..
            }
        ),
        "got {err:?}"
    );
}

/// A value INSIDE the declared range still decodes, so the guard above cannot
/// be satisfied by rejecting everything.
#[test]
fn round_results_known_enum_values_still_decode() {
    let (data, bit_len) = one_member(94, 2, 3);
    let mut r = BitReader::with_bit_len(&data, bit_len).unwrap();
    let results = decode_round_results(&mut r, &bomb_game_state_1301()).unwrap();
    assert_eq!(results[0].winning_team_role, Some(AresTeamRole::Defender));
}

#[test]
fn round_results_zero_width_enum_is_an_error_not_an_absent_field() {
    let (data, bit_len) = one_member(94, 0, 0);
    let mut r = BitReader::with_bit_len(&data, bit_len).unwrap();
    let err = decode_round_results(&mut r, &bomb_game_state_1301())
        .expect_err("a declared enum field with no payload cannot become absent");
    assert!(
        matches!(
            err,
            StructBlobError::InvalidEnumWidth {
                bits: 0,
                context: "RoundResults",
                ..
            }
        ),
        "got {err:?}"
    );
}

#[test]
fn struct_fname_rejects_a_negative_instance_number() {
    // Inline (not hardcoded) "Blue" with instance number -1.
    let mut fname = BitWriter::new();
    fname.bits(0, 1).i32(5);
    for byte in b"Blue\0" {
        fname.bits(u64::from(*byte), 8);
    }
    fname.i32(-1);
    // One element whose WinningTeam (handle 93 on 13.01) is that FName.
    let mut bits = BitWriter::new();
    bits.int_packed(1); // element count
    bits.int_packed(1); // encoded index -> round 0
    bits.int_packed(94);
    bits.int_packed(fname.bit_len());
    bits.append(&fname);
    bits.int_packed(0); // end of element
    bits.int_packed(0); // end of array
    let (data, bit_len) = bits.finish();
    let mut reader = BitReader::with_bit_len(&data, u64::from(bit_len)).unwrap();
    let err = decode_round_results(&mut reader, &bomb_game_state_1301())
        .expect_err("the struct decoder must propagate invalid FName numbers");
    assert!(
        matches!(
            err,
            StructBlobError::Decode(crate::DecodeError::InvalidFNameNumber { number: -1 })
        ),
        "got {err:?}"
    );
}

// -- Field windows the member did not consume -----------------------------

/// A member that reads less than its declared window fails
/// (`MemberNotFullyConsumed`) instead of exporting the part it read: the
/// 64-bit `EndOfRoundMoney` whose first 32 bits say 1900.
#[test]
fn round_infos_member_that_underreads_its_window_is_an_error() {
    // Handle 43 is EndOfRoundMoney; 64 declared bits against a 32-bit Int32.
    let (data, bit_len) = one_member(43, 1900, 64);
    let mut r = BitReader::with_bit_len(&data, bit_len).unwrap();
    let err = decode_round_infos(&mut r, &owner_exclusive_player_info())
        .expect_err("half-read money must not export as a value");
    match &err {
        StructBlobError::MemberNotFullyConsumed {
            name,
            declared,
            remaining,
            ..
        } => {
            assert_eq!(name, "EndOfRoundMoney");
            assert_eq!(*declared, 64);
            assert_eq!(*remaining, 32);
        }
        other => panic!("expected MemberNotFullyConsumed, got {other:?}"),
    }
}

/// The exact-width case still decodes, so the guard above cannot be satisfied
/// by rejecting every member.
#[test]
fn round_infos_member_with_an_exact_window_still_decodes() {
    let (data, bit_len) = one_member(43, 1900, 32);
    let mut r = BitReader::with_bit_len(&data, bit_len).unwrap();
    let results = decode_round_infos(&mut r, &owner_exclusive_player_info()).unwrap();
    assert_eq!(results[0].end_of_round_money, Some(1900));
}
