//! Deterministic scenario matrices with explicit missing coverage.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScenarioOutcome {
    Passed,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScenarioResult {
    pub name: String,
    pub outcome: ScenarioOutcome,
    pub message: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AxisReport {
    pub missing_reason: Option<String>,
    pub scenarios: Vec<ScenarioResult>,
}

impl AxisReport {
    pub fn new(scenarios: Vec<ScenarioResult>) -> Self {
        Self {
            missing_reason: scenarios.is_empty().then(|| "no scenarios ran".into()),
            scenarios,
        }
    }

    pub fn missing(reason: impl Into<String>) -> Self {
        Self {
            missing_reason: Some(reason.into()),
            scenarios: Vec::new(),
        }
    }

    /// Empty scenario lists are missing even when read from an older snapshot
    /// that did not record a missing reason.
    pub fn is_missing(&self) -> bool {
        self.missing_reason.is_some() || self.scenarios.is_empty()
    }

    pub fn passed(&self) -> bool {
        !self.is_missing()
            && self
                .scenarios
                .iter()
                .all(|s| s.outcome == ScenarioOutcome::Passed)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatrixReport {
    axes: BTreeMap<String, AxisReport>,
}

impl MatrixReport {
    /// Register all expected axes as missing until their reports are supplied.
    pub fn new(expected_axes: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            axes: expected_axes
                .into_iter()
                .map(|name| (name.into(), AxisReport::missing("axis has not run")))
                .collect(),
        }
    }

    pub fn axes(&self) -> &BTreeMap<String, AxisReport> {
        &self.axes
    }

    pub fn set_axis(&mut self, axis: impl Into<String>, report: AxisReport) {
        self.axes.insert(axis.into(), report);
    }

    /// Evaluate named cases in input order, recording failures and continuing.
    pub fn run_axis<T>(
        &mut self,
        axis: impl Into<String>,
        scenarios: impl IntoIterator<Item = (impl Into<String>, T)>,
        mut evaluate: impl FnMut(T) -> anyhow::Result<()>,
    ) {
        let scenarios = scenarios
            .into_iter()
            .map(|(name, case)| {
                let (outcome, message) = match evaluate(case) {
                    Ok(()) => (ScenarioOutcome::Passed, None),
                    Err(error) => (ScenarioOutcome::Failed, Some(format!("{error:#}"))),
                };
                ScenarioResult {
                    name: name.into(),
                    outcome,
                    message,
                }
            })
            .collect();
        self.set_axis(axis, AxisReport::new(scenarios));
    }

    pub fn all_passed(&self) -> bool {
        !self.axes.is_empty() && self.axes.values().all(AxisReport::passed)
    }

    /// Serialize axes in sorted name order and scenarios in execution order.
    pub fn snapshot_json(&self) -> serde_json::Result<String> {
        serde_json::to_string_pretty(self)
    }

    pub fn from_snapshot_json(json: &str) -> serde_json::Result<Self> {
        serde_json::from_str(json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_axes_and_empty_matrices_are_not_green() {
        assert!(AxisReport::default().is_missing());
        assert!(!MatrixReport::default().all_passed());
        let mut matrix = MatrixReport::new(["test"]);
        matrix.run_axis("test", [] as [(&str, ()); 0], |()| Ok(()));
        assert!(matrix.axes()["test"].is_missing());
        assert!(!matrix.all_passed());
        matrix.run_axis("test", [("first", false), ("second", true)], |ok| {
            anyhow::ensure!(ok, "not ready");
            Ok(())
        });
        let cases = &matrix.axes()["test"].scenarios;
        assert_eq!(cases[0].message.as_deref(), Some("not ready"));
        assert_eq!(cases[1].outcome, ScenarioOutcome::Passed);
        assert!(!matrix.all_passed());
    }
}
