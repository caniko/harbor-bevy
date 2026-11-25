use bevy::prelude::Resource;
use std::{net::SocketAddr, time::Duration};
use thespan::{Multiaddr, PeerId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum GameStatus {
    Waiting = 0,
    InProgress = 1,
    Full = 2,
}

impl TryFrom<u8> for GameStatus {
    type Error = u8;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Waiting),
            1 => Ok(Self::InProgress),
            2 => Ok(Self::Full),
            _ => Err(value),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum GameBackend {
    Lan = 0,
    Steam = 1,
    Rendezvous = 2,
    WebRtc = 3,
}

impl TryFrom<u8> for GameBackend {
    type Error = u8;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Lan),
            1 => Ok(Self::Steam),
            2 => Ok(Self::Rendezvous),
            3 => Ok(Self::WebRtc),
            _ => Err(value),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionInfo {
    Lan(SocketAddr),
    Steam(u64),
    Rendezvous(Multiaddr),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowsedGame {
    pub name: String,
    pub peer: PeerId,
    pub backend: GameBackend,
    pub status: GameStatus,
    pub connection: ConnectionInfo,
    pub players: u16,
    pub capacity: u16,
}

impl BrowsedGame {
    pub fn is_joinable(&self) -> bool {
        self.status == GameStatus::Waiting && self.players < self.capacity
    }

    pub fn matches_quick_match(&self, backend: Option<GameBackend>) -> bool {
        self.is_joinable() && backend.is_none_or(|backend| backend == self.backend)
    }
}

#[derive(Debug, Default, Resource)]
pub struct GameBrowserList(pub Vec<BrowsedGame>);

#[derive(Debug, Clone, Resource)]
pub struct HostedGameInfo(pub BrowsedGame);

#[derive(Debug, Clone, Resource)]
pub struct HostingState(pub HostedGameInfo);

#[derive(Debug, Clone, Resource)]
pub struct JoiningState(pub BrowsedGame);

#[derive(Debug, Clone, Copy, Resource)]
pub struct LobbyCountdown(pub Duration);

#[derive(Debug, Clone, Copy, Resource)]
pub struct PortSettings {
    pub port: u16,
}

impl Default for PortSettings {
    fn default() -> Self {
        Self { port: 27100 }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Resource)]
pub enum QuickMatchState {
    #[default]
    Idle,
    Searching {
        backend: Option<GameBackend>,
    },
    Joining,
}
