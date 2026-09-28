//! Channel lifecycle: open, close, and the two bunch-level GUID preambles.
//!
//! An open bunch carries the actor GUID (and, for a dynamic actor, the spawn
//! block in [`super::spawn`]) ahead of its content blocks; a close carries only
//! the header flag. This runs at most a few thousand times per replay; the
//! per-block hot path is [`super::framing`].

use vrf_bitio::BitReader;

use crate::bunch::RawBunchHeader;
use crate::error::{NetError, Result};
use crate::net_guid;
use crate::stats::NetStats;
use crate::types::{MAX_GUID_COUNT, NetworkGuid};

use super::spawn;
use super::{ActorChannelState, ChannelTable, PLAYER_CONTROLLER_LEAF, ReplicationSink};

/// Whether `path` names the replay controller, in any of the spellings the
/// same asset arrives under:
///
/// | source | string |
/// |---|---|
/// | net field export group path | `/Game/Characters/_Core/BaseReplayController.BaseReplayController_C` |
/// | NetGUID path (package-map export) | `/Game/Characters/_Core/BaseReplayController` |
/// | archetype GUID path (class default object) | `Default__BaseReplayController_C` |
/// | `/_Core/` elided alias | `/Game/Characters/BaseReplayController` |
///
/// The 12.01--12.06 samples name the role BaseJanusController, whose opening
/// bunch also carries the net-player-index byte; that exact leaf is accepted
/// too.
///
/// Getting this wrong is silent: the index byte is not consumed, every content
/// block after it in the bunch shifts by 8 bits, and that surfaces only as one
/// malformed block and a few hundred skipped bits.
pub(super) fn is_player_controller_path(path: &str) -> bool {
    let segment = path.rsplit('/').next().unwrap_or(path);
    // `Asset.Class_C` -> `Class_C`; a bare segment is unchanged.
    let class = segment.rsplit('.').next().unwrap_or(segment);
    let class = class.strip_prefix("Default__").unwrap_or(class);
    let class = class.strip_suffix("_C").unwrap_or(class);
    matches!(class, PLAYER_CONTROLLER_LEAF | "BaseJanusController")
}

/// Whether this channel's actor or archetype resolves to the replay
/// controller, which decides the net-player-index byte.
///
/// Unreal writes that 1-byte player index between the spawn data and the first
/// content block only for a dynamic PlayerController (`ReadNetPlayerIndexStage.cs`:
/// `OpenedDynamicActor && IsPlayerController(archetype/class/actor path)`).
/// Paths come from the sink's cache (`GuidPathSink::path_for_guid` says why).
/// A missed byte does not desync visibly: with the spawn-velocity bit in
/// [`super::spawn`] the misframed header re-synchronises a few bits later
/// (docs/archive/PROJECT_STATUS.md 17-A has the mechanism and measurements).
pub(super) fn is_player_controller_channel(
    actor_net_guid: NetworkGuid,
    archetype_net_guid: NetworkGuid,
    sink: &dyn ReplicationSink,
) -> bool {
    [archetype_net_guid.0, actor_net_guid.0]
        .iter()
        .filter_map(|&g| sink.path_for_guid(g))
        .any(is_player_controller_path)
}

/// Read a package-map export bunch: a run of GUID declarations with paths.
///
/// Both skip paths drop the whole bunch, differently. A RepLayout export is a
/// *limitation* (this parser does not implement that variant): counted on its
/// own line, `Ok`. An impossible GUID count is a *failure*: every path
/// declaration after it is lost, so it is an `Err`, which the caller counts
/// and whose abandoned bits it tallies.
pub(super) fn read_package_map_exports(
    payload: &mut BitReader<'_>,
    stats: &mut NetStats,
    sink: &mut dyn ReplicationSink,
) -> Result<()> {
    let has_rep_layout_export = payload.read_bit()?;
    if has_rep_layout_export {
        // Unsupported variant: skip the bunch, but say so.
        stats.rep_layout_export_bunches += 1;
        payload.skip_remaining();
        return Ok(());
    }

    let num_guids = payload.read_i32()?;
    if num_guids < 0 || num_guids as u32 > MAX_GUID_COUNT {
        return Err(NetError::InvalidGuidCount {
            count: num_guids,
            max: MAX_GUID_COUNT,
        });
    }

    for _ in 0..num_guids {
        let _ = net_guid::internal_load_object(payload, true, 0, sink)?;
        stats.exported_guids += 1;
    }
    Ok(())
}

/// Consume the must-be-mapped GUID list that precedes the content blocks.
pub(super) fn read_must_be_mapped_guids(
    payload: &mut BitReader<'_>,
    stats: &mut NetStats,
) -> Result<()> {
    let count = payload.read_u16()?;
    for _ in 0..count {
        let _guid = payload.read_int_packed()?;
        stats.must_be_mapped_guids += 1;
    }
    Ok(())
}

/// Read the actor GUID (and spawn block, when dynamic) that opens a channel.
pub(super) fn handle_channel_open(
    header: &RawBunchHeader,
    payload: &mut BitReader<'_>,
    channels: &mut ChannelTable,
    stats: &mut NetStats,
    sink: &mut dyn ReplicationSink,
) -> Result<()> {
    let ch_index = header.ch_index;

    let actor_net_guid = net_guid::internal_load_object(payload, false, 0, sink)?;

    let mut state = ActorChannelState {
        channel_index: ch_index,
        is_open: true,
        is_dormant: false,
        actor_net_guid,
        archetype_net_guid: NetworkGuid(0),
        level_guid: NetworkGuid(0),
        spawn_location: None,
        spawn_rotation: None,
        spawn_scale: None,
        spawn_velocity: None,
        open_packet_id: header.packet_id,
    };

    // A dynamic actor's spawn block is mandatory (`NewActorSerializer.cs`
    // reads it unconditionally), so a payload that ends here fails the read.
    // The shape is counted so a corpus run can say whether it ever occurs.
    if actor_net_guid.is_dynamic() {
        if payload.at_end() {
            stats.actor_opens_missing_spawn += 1;
        }
        spawn::read_dynamic_spawn_data(payload, &mut state, sink)?;
    }

    stats.actor_opens += 1;
    sink.on_actor_open(&state);
    // The row already exists (its bunch counter was bumped): `entry`, since an
    // `insert` would reset it.
    let slot = channels.entry(ch_index).or_default();
    // A reopen over a live actor stands and is counted, with no close
    // fabricated (see `NetStats::channel_reopens_while_open`).
    if slot.state.as_ref().is_some_and(|s| s.is_open) {
        stats.channel_reopens_while_open += 1;
    }
    slot.state = Some(state);
    Ok(())
}

/// Mark a channel closed and notify the sink. A channel that is already closed
/// is left alone so a repeated close does not double-count.
pub(super) fn handle_channel_close(
    header: &RawBunchHeader,
    channels: &mut ChannelTable,
    stats: &mut NetStats,
    sink: &mut dyn ReplicationSink,
) {
    let Some(ch) = channels
        .get_mut(&header.ch_index)
        .and_then(|s| s.state.as_mut())
    else {
        return;
    };
    if !ch.is_open {
        return;
    }
    ch.is_open = false;
    ch.is_dormant = header.b_dormant;
    stats.actor_closes += 1;
    sink.on_actor_close(header.ch_index, ch.actor_net_guid, header.b_dormant);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every spelling of the controller asset in the corpus must normalise to
    /// the same leaf, and nothing else may.
    #[test]
    fn controller_path_spellings_all_normalise() {
        for path in [
            "/Game/Characters/_Core/BaseReplayController.BaseReplayController_C",
            "/Game/Characters/_Core/BaseReplayController",
            "Default__BaseReplayController_C",
            "/Game/Characters/BaseReplayController",
            "BaseReplayController",
            "/Game/Characters/_Core/BaseJanusController.BaseJanusController_C",
            "/Game/Characters/_Core/BaseJanusController",
            "Default__BaseJanusController_C",
            "BaseJanusController",
        ] {
            assert!(is_player_controller_path(path), "{path}");
        }
        for path in [
            "",
            "/Game/Characters/_Core/BaseReplayControllerExtra",
            "/Game/Characters/_Core/PlayerController.PlayerController_C",
            "Default__BaseReplayController_D",
            "Default__BaseJanusController_D",
            "BaseJanusControllerExtra",
        ] {
            assert!(!is_player_controller_path(path), "{path}");
        }
    }
}
