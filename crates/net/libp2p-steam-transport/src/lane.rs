use steamworks::networking_sockets::{NetConnection, NetworkingSockets};

use crate::error::SteamTransportError;

/// Configuration for Steam connection lanes.
///
/// Lanes provide head-of-line blocking prevention, priority ordering,
/// and bandwidth partitioning between message categories on a single connection.
///
/// Higher numeric priority values are processed first. Lanes with the same
/// priority share bandwidth according to their weights.
#[derive(Debug, Clone)]
pub struct LaneConfig {
    /// Number of lanes (minimum 2 for lanes to be useful).
    pub num_lanes: i32,
    /// Priority per lane. Higher values = higher priority.
    /// Length must equal `num_lanes`.
    pub priorities: Vec<i32>,
    /// Bandwidth weight per lane (relative, same-priority only).
    /// Length must equal `num_lanes`.
    pub weights: Vec<u16>,
}

/// Default number of lanes for the muxer's substream pool.
pub const DEFAULT_INITIAL_LANES: i32 = 8;

impl Default for LaneConfig {
    /// 8 lanes, all same priority (substreams don't need priority ordering
    /// between each other — only ordering *within* a lane matters).
    /// Equal weights since all substreams are peers.
    fn default() -> Self {
        let n = DEFAULT_INITIAL_LANES;
        Self {
            num_lanes: n,
            priorities: vec![0; n as usize],
            weights: vec![1; n as usize],
        }
    }
}

/// Configure lanes on an established connection.
pub(crate) fn configure_lanes(
    sockets: &NetworkingSockets,
    connection: &NetConnection,
    config: &LaneConfig,
) -> Result<(), SteamTransportError> {
    sockets
        .configure_connection_lanes(
            connection,
            config.num_lanes,
            &config.priorities,
            &config.weights,
        )
        .map_err(|e| SteamTransportError::Steam(format!("configure_connection_lanes failed: {e}")))
}
