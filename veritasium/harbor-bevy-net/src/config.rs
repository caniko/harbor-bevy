use std::fmt::Write as _;

use bevy::prelude::Resource;
use thespan::{GossipTopic, ThespanConfig};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transports {
    pub lan: bool,
    pub lan_port: u16,
    pub webrtc: bool,
    pub webrtc_port: u16,
    pub relay: bool,
    pub relay_port: u16,
    pub steam: bool,
    pub steam_virtual_port: i32,
}

impl Default for Transports {
    fn default() -> Self {
        Self {
            lan: true,
            lan_port: crate::discovery::PortSettings::default().port,
            webrtc: false,
            webrtc_port: 0,
            relay: false,
            relay_port: 0,
            steam: false,
            steam_virtual_port: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Resource)]
pub struct NetConfig {
    pub app_id: String,
    pub protocol_prefix: String,
    pub session_id: Option<String>,
    pub transports: Transports,
    pub mailbox_capacity: usize,
    pub ingress_capacity: usize,
    pub max_events_per_frame: usize,
    pub max_deferred_events: usize,
    pub rejoin_window_secs: u64,
    pub liveness_interval_secs: u64,
    pub liveness_stall_secs: u64,
}

impl Default for NetConfig {
    fn default() -> Self {
        Self {
            app_id: "harbor-bevy-net".into(),
            protocol_prefix: "/harbor-bevy-net".into(),
            session_id: None,
            transports: Transports::default(),
            mailbox_capacity: 4096,
            ingress_capacity: 4096,
            max_events_per_frame: 256,
            max_deferred_events: 4096,
            rejoin_window_secs: crate::DEFAULT_REJOIN_WINDOW.as_secs(),
            liveness_interval_secs: crate::LIVENESS_PING_INTERVAL.as_secs(),
            liveness_stall_secs: crate::LIVENESS_STALL_TIMEOUT.as_secs(),
        }
    }
}

impl NetConfig {
    pub fn to_thespan_config(&self) -> ThespanConfig {
        ThespanConfig::new(
            &self.app_id,
            &self.protocol_prefix,
            thespan::identity::IDENTITY_DIR_ENV,
        )
    }

    pub fn match_topic(&self, match_id: &[u8; 16]) -> GossipTopic {
        let mut topic = self.control_domain("match");
        topic.push('/');
        for byte in match_id {
            write!(topic, "{byte:02x}").expect("formatting into a string is infallible");
        }
        topic.into()
    }

    /// Explicit application scope; independent of the transport's opt-in
    /// process-wide session isolation setting.
    pub fn control_domain(&self, purpose: &str) -> String {
        let scope = self.session_id.as_deref().unwrap_or_default();
        format!(
            "{}/control/{}:{scope}/{}:{purpose}",
            self.protocol_prefix,
            scope.len(),
            purpose.len()
        )
    }
}
