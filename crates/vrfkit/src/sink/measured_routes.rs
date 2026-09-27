//! Which checksum-gated structured-array routes a replay branch admits.
//!
//! Each route in [`MeasuredArrayRoute`] expands one exact (group, parent name,
//! parent checksum) identity into additive child rows, and types a child only
//! when its declared handle, name and checksum match a measured member table.
//! Those tables are keyed by handle, so a route is only as right as the
//! build's member layout: on a build where a member moved to another handle
//! the route either refuses (and counts it) or emits children it cannot type.
//!
//! The gate is therefore per build AND per route, not per build. A route is
//! admitted for a branch only where it was observed on that build's replays
//! with every array counter at zero, and an independent decoder matched every
//! child row it emitted. A route that was never observed on a build is
//! unobserved, not verified, and stays off there. The measurement behind
//! every entry, and the reason for every route held back, is in
//! docs/LEGACY_BUILD_SUPPORT.md.

/// One measured structured-array route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MeasuredArrayRoute {
    /// `OwnerExclusivePlayerInfo.AllPlayersObfuscatedPlayerInformation`.
    AllPlayersObfuscatedPlayerInformation,
    /// `OwnerExclusivePlayerInfo.TrackedRewards`.
    TrackedRewards,
    /// `PersonalizationComponent.SelectedV2`.
    SelectedV2,
    /// `PlayerMatchStatsComponent.KillData`.
    KillData,
    /// `EffectManagerComponent.ServerActiveEffects`.
    ServerActiveEffects,
    /// `FiniteSpeedMovementComponent.RequestedIgnoreActors`.
    RequestedIgnoreActors,
    /// `BlindManagerComponent.ActiveBlinds`.
    ActiveBlinds,
    /// `MulticastSetPath.NetworkedProjectilePath` (an RPC parameter array).
    NetworkedProjectilePath,
}

impl MeasuredArrayRoute {
    /// Every route, in declaration order.
    pub(super) const ALL: [Self; 8] = [
        Self::AllPlayersObfuscatedPlayerInformation,
        Self::TrackedRewards,
        Self::SelectedV2,
        Self::KillData,
        Self::ServerActiveEffects,
        Self::RequestedIgnoreActors,
        Self::ActiveBlinds,
        Self::NetworkedProjectilePath,
    ];

    const fn bit(self) -> u8 {
        1 << (self as u8)
    }
}

/// The set of routes admitted for one replay branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) struct MeasuredArrayRoutes(u8);

impl MeasuredArrayRoutes {
    /// No route: every parent stays one raw row, and no child is emitted.
    pub(super) const NONE: Self = Self(0);

    /// Every route.
    pub(super) const ALL: Self = Self::of(&MeasuredArrayRoute::ALL);

    const fn of(routes: &[MeasuredArrayRoute]) -> Self {
        let mut bits = 0;
        let mut i = 0;
        while i < routes.len() {
            bits |= routes[i].bit();
            i += 1;
        }
        Self(bits)
    }

    /// The routes admitted for `branch`, the replay header's branch string.
    ///
    /// An unknown branch admits nothing. The table is pinned for every
    /// supported branch by `routes_are_pinned_for_every_supported_branch`,
    /// so a build cannot gain or lose a route without that test changing too.
    ///
    /// The legacy entries were measured on 2026-09-28 over all 48 11.06-12.09
    /// replays (three per build) and the single public 12.10, 12.11 and 13.00
    /// fixtures, main stream and checkpoints. What holds each route back:
    ///
    /// - `AllPlayersObfuscatedPlayerInformation` and `TrackedRewards`, before
    ///   12.04: two members inserted ahead of `TrackedRewards` move every
    ///   later `OwnerExclusivePlayerInfo` handle by +3. The handle-keyed
    ///   member tables would type nothing, so the route is not the one that
    ///   was measured.
    /// - `SelectedV2`, 11.06-11.08: a zero-width `DynamicMappings` member at
    ///   handle 13 moves the nested attachment array to handle 14. The
    ///   nested route never matches, and the walker skips the zero-width
    ///   member without a counter.
    /// - `ActiveBlinds`, every legacy build: through 12.04 `SourceID` and
    ///   `EffectID` swap handles 4 and 5; from 12.05 the handles match, but
    ///   some `SourceID` values arrive as the 9-bit hardcoded-name form
    ///   (index 0) the 297-bit width rule refuses. Either way the refusal
    ///   moves `array_leaf_decode_errors`.
    /// - A route with no child observed on a build (the path on 11.06, 12.06
    ///   and 12.07; four routes in each single-replay 12.10/12.11/13.00
    ///   fixture) is unobserved there, not verified.
    ///
    /// Measurement and per-build counts: docs/LEGACY_BUILD_SUPPORT.md.
    pub(super) fn for_branch(branch: &str) -> Self {
        use MeasuredArrayRoute::{
            AllPlayersObfuscatedPlayerInformation as PlayerInfo, KillData,
            NetworkedProjectilePath as ProjectilePath, RequestedIgnoreActors, SelectedV2,
            ServerActiveEffects, TrackedRewards,
        };
        match branch {
            "++Ares-Core+release-11.06" => {
                Self::of(&[KillData, ServerActiveEffects, RequestedIgnoreActors])
            }
            "++Ares-Core+release-11.07" | "++Ares-Core+release-11.08" => Self::of(&[
                KillData,
                ServerActiveEffects,
                RequestedIgnoreActors,
                ProjectilePath,
            ]),
            "++Ares-Core+release-11.09"
            | "++Ares-Core+release-11.10"
            | "++Ares-Core+release-11.11"
            | "++Ares-Core+release-12.00"
            | "++Ares-Core+release-12.01"
            | "++Ares-Core+release-12.02"
            | "++Ares-Core+release-12.03" => Self::of(&[
                SelectedV2,
                KillData,
                ServerActiveEffects,
                RequestedIgnoreActors,
                ProjectilePath,
            ]),
            "++Ares-Core+release-12.04"
            | "++Ares-Core+release-12.05"
            | "++Ares-Core+release-12.08"
            | "++Ares-Core+release-12.09" => Self::of(&[
                PlayerInfo,
                TrackedRewards,
                SelectedV2,
                KillData,
                ServerActiveEffects,
                RequestedIgnoreActors,
                ProjectilePath,
            ]),
            "++Ares-Core+release-12.06" | "++Ares-Core+release-12.07" => Self::of(&[
                PlayerInfo,
                TrackedRewards,
                SelectedV2,
                KillData,
                ServerActiveEffects,
                RequestedIgnoreActors,
            ]),
            "++Ares-Core+release-12.10"
            | "++Ares-Core+release-12.11"
            | "++Ares-Core+release-13.00" => {
                Self::of(&[PlayerInfo, TrackedRewards, SelectedV2, ServerActiveEffects])
            }
            // The builds every route was first measured on.
            "++Ares-Core+release-13.01"
            | "++Ares-Core+release-13.02"
            | "++Ares-Core+release-13.04"
            | "++Ares-Core+release-13.05"
            | "++Ares-Core+release-13.06" => Self::ALL,
            _ => Self::NONE,
        }
    }

    /// Whether `route` may expand its parent on this branch.
    pub(super) fn admits(self, route: MeasuredArrayRoute) -> bool {
        self.0 & route.bit() != 0
    }
}

#[cfg(test)]
mod tests {
    use super::MeasuredArrayRoute as Route;
    use super::MeasuredArrayRoutes;
    use Route::{
        ActiveBlinds, AllPlayersObfuscatedPlayerInformation as PlayerInfo, KillData,
        NetworkedProjectilePath as ProjectilePath, RequestedIgnoreActors, SelectedV2,
        ServerActiveEffects, TrackedRewards,
    };

    const ALL: &[Route] = &[
        PlayerInfo,
        TrackedRewards,
        SelectedV2,
        KillData,
        ServerActiveEffects,
        RequestedIgnoreActors,
        ActiveBlinds,
        ProjectilePath,
    ];
    /// 11.06: the path is unobserved and SelectedV2's layout differs.
    const B1106: &[Route] = &[KillData, ServerActiveEffects, RequestedIgnoreActors];
    /// 11.07-11.08: SelectedV2's nested attachment handle moved.
    const B1107: &[Route] = &[
        KillData,
        ServerActiveEffects,
        RequestedIgnoreActors,
        ProjectilePath,
    ];
    /// 11.09-12.03: the OwnerExclusivePlayerInfo handles are shifted by +3.
    const B1109: &[Route] = &[
        SelectedV2,
        KillData,
        ServerActiveEffects,
        RequestedIgnoreActors,
        ProjectilePath,
    ];
    /// 12.04-12.09: everything but ActiveBlinds.
    const B1204: &[Route] = &[
        PlayerInfo,
        TrackedRewards,
        SelectedV2,
        KillData,
        ServerActiveEffects,
        RequestedIgnoreActors,
        ProjectilePath,
    ];
    /// 12.06-12.07: no projectile path in any sample.
    const B1206: &[Route] = &[
        PlayerInfo,
        TrackedRewards,
        SelectedV2,
        KillData,
        ServerActiveEffects,
        RequestedIgnoreActors,
    ];
    /// 12.10, 12.11 and 13.00: the only four routes their one fixture holds.
    const FIXTURE: &[Route] = &[PlayerInfo, TrackedRewards, SelectedV2, ServerActiveEffects];

    /// Every supported branch and the routes it admits, in the order of
    /// `vrf_transform::ALL_VERSIONS`. A change to the gate is a change to this
    /// table, made together with the measurement that justifies it.
    const PINNED: &[(&str, &[Route])] = &[
        ("++Ares-Core+release-11.06", B1106),
        ("++Ares-Core+release-11.07", B1107),
        ("++Ares-Core+release-11.08", B1107),
        ("++Ares-Core+release-11.09", B1109),
        ("++Ares-Core+release-11.10", B1109),
        ("++Ares-Core+release-11.11", B1109),
        ("++Ares-Core+release-12.00", B1109),
        ("++Ares-Core+release-12.01", B1109),
        ("++Ares-Core+release-12.02", B1109),
        ("++Ares-Core+release-12.03", B1109),
        ("++Ares-Core+release-12.04", B1204),
        ("++Ares-Core+release-12.05", B1204),
        ("++Ares-Core+release-12.06", B1206),
        ("++Ares-Core+release-12.07", B1206),
        ("++Ares-Core+release-12.08", B1204),
        ("++Ares-Core+release-12.09", B1204),
        ("++Ares-Core+release-12.10", FIXTURE),
        ("++Ares-Core+release-12.11", FIXTURE),
        ("++Ares-Core+release-13.00", FIXTURE),
        ("++Ares-Core+release-13.01", ALL),
        ("++Ares-Core+release-13.02", ALL),
        ("++Ares-Core+release-13.04", ALL),
        ("++Ares-Core+release-13.05", ALL),
        ("++Ares-Core+release-13.06", ALL),
    ];

    #[test]
    fn active_blinds_stays_off_on_every_legacy_build() {
        // Both legacy refusal causes move array_leaf_decode_errors; see
        // `MeasuredArrayRoutes::for_branch`. Only the 13.x builds admit it.
        for (branch, routes) in PINNED {
            let measured_13x = branch.starts_with("++Ares-Core+release-13.")
                && *branch != "++Ares-Core+release-13.00";
            assert_eq!(routes.contains(&ActiveBlinds), measured_13x, "{branch}");
            assert_eq!(
                MeasuredArrayRoutes::for_branch(branch).admits(ActiveBlinds),
                measured_13x,
                "{branch}"
            );
        }
    }

    #[test]
    fn routes_are_pinned_for_every_supported_branch() {
        // One row per supported branch: a newly supported build must be
        // placed in the table deliberately, even if it admits nothing.
        let supported: Vec<&str> = vrf_transform::ALL_VERSIONS
            .iter()
            .map(|version| version.branch())
            .collect();
        let pinned: Vec<&str> = PINNED.iter().map(|(branch, _)| *branch).collect();
        assert_eq!(pinned, supported);

        for (branch, routes) in PINNED {
            let admitted = MeasuredArrayRoutes::for_branch(branch);
            for route in Route::ALL {
                assert_eq!(
                    admitted.admits(route),
                    routes.contains(&route),
                    "{branch}: {route:?}"
                );
            }
        }
    }

    #[test]
    fn unknown_branches_admit_no_route() {
        for branch in [
            "",
            "13.05",
            "++Ares-Core+release-13.07",
            "++Ares-Core+release-12.12",
            "++Ares-Core+release-13.05 ",
        ] {
            let admitted = MeasuredArrayRoutes::for_branch(branch);
            assert_eq!(admitted, MeasuredArrayRoutes::NONE, "{branch:?}");
            for route in Route::ALL {
                assert!(!admitted.admits(route), "{branch:?}: {route:?}");
            }
        }
    }

    #[test]
    fn every_route_has_its_own_bit() {
        let mut seen = 0u8;
        for route in Route::ALL {
            assert_eq!(seen & route.bit(), 0, "{route:?} shares a bit");
            seen |= route.bit();
            assert!(MeasuredArrayRoutes::ALL.admits(route));
            assert!(!MeasuredArrayRoutes::NONE.admits(route));
        }
    }
}
