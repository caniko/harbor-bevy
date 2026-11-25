//! Generic counters and histogram summaries.

use serde_json::{Map, Value, json};
use std::{collections::BTreeMap, time::Instant};

#[cfg_attr(feature = "bevy", derive(bevy::prelude::Resource))]
pub struct SimulationMetrics {
    pub instance_id: String,
    pub start_time: Instant,
    pub counters: BTreeMap<&'static str, u64>,
    pub histograms: BTreeMap<&'static str, HistogramBucket>,
    pub anomalies: Vec<(u64, String)>,
    pub final_tick: u64,
}

/// A summary of all samples for one metric.
#[derive(Clone, Debug, Default)]
pub struct HistogramBucket {
    pub min: u64,
    pub max: u64,
    pub sum: u128,
    pub count: u64,
}

impl HistogramBucket {
    pub fn record(&mut self, value: u64) {
        if self.count == u64::MAX {
            return;
        }
        if self.count == 0 {
            self.min = value;
            self.max = value;
        } else {
            self.min = self.min.min(value);
            self.max = self.max.max(value);
        }
        self.sum = self.sum.saturating_add(u128::from(value));
        self.count += 1;
    }

    pub fn mean(&self) -> f64 {
        if self.count == 0 {
            0.0
        } else {
            self.sum as f64 / self.count as f64
        }
    }
}

pub mod counters {
    pub const EVENT_FLOODS_DETECTED: &str = "event_floods_detected";
}

pub mod histograms {
    pub const FRAME_TIME_US: &str = "frame_time_us";
}

impl SimulationMetrics {
    pub fn new(instance_id: impl Into<String>) -> Self {
        Self {
            instance_id: instance_id.into(),
            start_time: Instant::now(),
            counters: BTreeMap::new(),
            histograms: BTreeMap::new(),
            anomalies: Vec::new(),
            final_tick: 0,
        }
    }

    pub fn inc(&mut self, name: &'static str) {
        self.add(name, 1);
    }

    pub fn add(&mut self, name: &'static str, amount: u64) {
        let count = self.counters.entry(name).or_default();
        *count = count.saturating_add(amount);
    }

    pub fn record_histogram(&mut self, name: &'static str, value: u64) {
        self.histograms.entry(name).or_default().record(value);
    }

    pub fn to_json(&self) -> String {
        self.to_json_with_extra(Value::Null)
    }

    /// Keep caller-defined metadata under `extra`, separate from built-in fields.
    pub fn to_json_with_extra(&self, extra: Value) -> String {
        let histograms: Map<_, _> = self
            .histograms
            .iter()
            .map(|(&name, bucket)| {
                (
                    name.to_owned(),
                    json!({
                        "count": bucket.count,
                        "min": bucket.min,
                        "max": bucket.max,
                        "mean": bucket.mean(),
                    }),
                )
            })
            .collect();
        let anomalies: Vec<_> = self
            .anomalies
            .iter()
            .map(|(tick, description)| json!({ "tick": tick, "description": description }))
            .collect();
        json!({
            "instance_id": self.instance_id,
            "elapsed_secs": self.start_time.elapsed().as_secs_f64(),
            "final_tick": self.final_tick,
            "counters": self.counters,
            "histograms": histograms,
            "anomalies": anomalies,
            "extra": extra,
        })
        .to_string()
    }
}

#[cfg(feature = "bevy")]
pub fn record_frame_time(
    time: bevy::prelude::Res<bevy::time::Time<bevy::time::Real>>,
    mut metrics: bevy::prelude::ResMut<SimulationMetrics>,
) {
    let micros = u64::try_from(time.delta().as_micros()).unwrap_or(u64::MAX);
    metrics.record_histogram(histograms::FRAME_TIME_US, micros);
}
