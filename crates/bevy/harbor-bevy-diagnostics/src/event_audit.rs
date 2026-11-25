//! Explicit event counting and sustained flood detection.

use serde::Serialize;
use std::collections::BTreeMap;
use std::fmt::Write as _;

#[derive(Clone, Debug)]
#[cfg_attr(feature = "bevy", derive(bevy::prelude::Resource))]
pub struct EventAuditConfig {
    /// Counts strictly greater than these thresholds trigger evaluation.
    pub per_type_threshold: u32,
    pub total_threshold: u32,
    /// Consecutive frames required for a type alert. Zero behaves like one.
    pub sustained_frames: u32,
}

impl Default for EventAuditConfig {
    fn default() -> Self {
        Self {
            per_type_threshold: 50,
            total_threshold: 200,
            sustained_frames: 3,
        }
    }
}

#[derive(Default)]
struct EventTypeTracker {
    current_count: u32,
    consecutive_over: u32,
    alerted: bool,
    lifetime_total: u64,
}

#[derive(Default)]
#[cfg_attr(feature = "bevy", derive(bevy::prelude::Resource))]
pub struct EventAuditState {
    trackers: BTreeMap<&'static str, EventTypeTracker>,
    frame_total: u32,
    #[cfg(feature = "bevy")]
    tick: u64,
    pub alerts: Vec<EventAuditAlert>,
}

#[derive(Clone, Debug, Serialize)]
pub struct EventAuditAlert {
    pub tick: u64,
    pub event_type: &'static str,
    pub count: u32,
    pub description: String,
}

impl EventAuditState {
    /// Count a caller-instrumented event. Events are not observed automatically.
    pub fn record(&mut self, event_type: &'static str) {
        self.record_batch(event_type, 1);
    }

    pub fn record_batch(&mut self, event_type: &'static str, count: u32) {
        let tracker = self.trackers.entry(event_type).or_default();
        tracker.current_count = tracker.current_count.saturating_add(count);
        tracker.lifetime_total = tracker.lifetime_total.saturating_add(u64::from(count));
        self.frame_total = self.frame_total.saturating_add(count);
    }

    /// Emit alerts, reset frame counters, and return the number of new alerts.
    pub fn finish_frame(&mut self, config: &EventAuditConfig, tick: u64) -> u64 {
        let first = self.alerts.len();
        if self.frame_total > config.total_threshold {
            tracing::warn!(
                tick,
                count = self.frame_total,
                threshold = config.total_threshold,
                "total event flood"
            );
            self.alerts.push(EventAuditAlert {
                tick,
                event_type: "_total_",
                count: self.frame_total,
                description: format!("Total exceeds threshold {}", config.total_threshold),
            });
        }
        self.frame_total = 0;

        for (&event_type, tracker) in &mut self.trackers {
            let count = std::mem::take(&mut tracker.current_count);
            if count <= config.per_type_threshold {
                tracker.consecutive_over = 0;
                tracker.alerted = false;
                continue;
            }
            tracker.consecutive_over = tracker.consecutive_over.saturating_add(1);
            if !tracker.alerted && tracker.consecutive_over >= config.sustained_frames {
                tracker.alerted = true;
                tracing::error!(
                    tick,
                    event_type,
                    count,
                    frames = tracker.consecutive_over,
                    "sustained event flood"
                );
                self.alerts.push(EventAuditAlert {
                    tick,
                    event_type,
                    count,
                    description: format!(
                        "Exceeded threshold {} for {} frames",
                        config.per_type_threshold, tracker.consecutive_over
                    ),
                });
            }
        }
        (self.alerts.len() - first) as u64
    }

    pub fn summary(&self) -> String {
        let mut out = format!(
            "=== Event Audit Summary ===\nAlerts: {}\n",
            self.alerts.len()
        );
        for (name, tracker) in &self.trackers {
            let _ = writeln!(out, "{name}: {}", tracker.lifetime_total);
        }
        out
    }

    pub fn to_json(&self) -> String {
        let event_types: Vec<_> = self
            .trackers
            .iter()
            .map(|(&name, tracker)| {
                serde_json::json!({
                    "name": name,
                    "current_count": tracker.current_count,
                    "consecutive_over": tracker.consecutive_over,
                    "alerted": tracker.alerted,
                    "lifetime_total": tracker.lifetime_total,
                })
            })
            .collect();
        serde_json::json!({
            "frame_total": self.frame_total,
            "event_types": event_types,
            "alerts": self.alerts,
        })
        .to_string()
    }
}

/// Run once per frame, after the event producers.
#[cfg(feature = "bevy")]
pub fn audit_events_end_frame(
    config: bevy::prelude::Res<EventAuditConfig>,
    mut state: bevy::prelude::ResMut<EventAuditState>,
    metrics: Option<bevy::prelude::ResMut<crate::metrics::SimulationMetrics>>,
) {
    state.tick = state.tick.saturating_add(1);
    let tick = state.tick;
    let alerts = state.finish_frame(&config, tick);
    if let Some(mut metrics) = metrics {
        metrics.final_tick = tick;
        if alerts != 0 {
            metrics.add(crate::metrics::counters::EVENT_FLOODS_DETECTED, alerts);
        }
    }
}
