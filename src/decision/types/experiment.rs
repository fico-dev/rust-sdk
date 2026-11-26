//! Types shared by every experiment.
//!
//! The platform builds acceptance and scenario tests on batch experiments,
//! which the SDK does not expose, so [`ExperimentRun`] is crate-private.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Status of an experiment: an acceptance, scenario, shadow or switchback test.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum ExperimentStatus {
    /// The experiment started.
    Started,
    /// The experiment completed.
    Completed,
    /// The experiment failed.
    Failed,
    /// The experiment is a draft and has not started.
    Draft,
    /// The experiment was canceled.
    Canceled,
    /// The experiment is stopping.
    Stopping,
    /// The experiment is being deleted.
    Deleting,
    /// Deleting the experiment failed.
    DeleteFailed,
    /// A value this version of the SDK does not know.
    #[serde(other)]
    Unknown,
}

impl ExperimentStatus {
    /// Whether an experiment with this status has stopped changing: it
    /// completed, failed, is a draft, was canceled or failed to delete. A
    /// status this SDK does not know is not terminal.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            ExperimentStatus::Completed
                | ExperimentStatus::Failed
                | ExperimentStatus::Draft
                | ExperimentStatus::Canceled
                | ExperimentStatus::DeleteFailed
        )
    }
}

/// A run that an experiment's underlying batch requests: an input, an
/// environment and an option set. It only goes into request bodies.
///
/// Set `environment_id`, `build_id` or both.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub(crate) struct ExperimentRun {
    /// ID of the input: a managed input, or an input of `input_set_id`.
    pub(crate) input_id: String,
    /// Option set of the run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) option_set: Option<String>,
    /// ID of the environment to run (API: `instance_id`).
    #[serde(rename = "instance_id", skip_serializing_if = "Option::is_none")]
    pub(crate) environment_id: Option<String>,
    /// ID of the build to run (API: `version_id`).
    #[serde(rename = "version_id", skip_serializing_if = "Option::is_none")]
    pub(crate) build_id: Option<String>,
    /// ID of the input set the input belongs to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) input_set_id: Option<String>,
    /// ID of the scenario of the run, in a scenario test.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) scenario_id: Option<String>,
    /// Repetition number of the run, in a scenario test.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) repetition: Option<u32>,
    /// Number of the run in the batch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) run_number: Option<String>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn writes_experiment_runs_with_api_names() {
        let run = ExperimentRun {
            input_id: "input-1".to_owned(),
            option_set: Some("set-1".to_owned()),
            environment_id: Some("production".to_owned()),
            build_id: Some("v1".to_owned()),
            input_set_id: Some("inputs".to_owned()),
            scenario_id: Some("rainy".to_owned()),
            repetition: Some(0),
            run_number: Some("1".to_owned()),
        };
        assert_eq!(
            serde_json::to_value(&run).unwrap(),
            json!({
                "input_id": "input-1",
                "option_set": "set-1",
                "instance_id": "production",
                "version_id": "v1",
                "input_set_id": "inputs",
                "scenario_id": "rainy",
                "repetition": 0,
                "run_number": "1"
            })
        );
        let minimal = ExperimentRun {
            input_id: "input-1".to_owned(),
            environment_id: Some("production".to_owned()),
            ..ExperimentRun::default()
        };
        assert_eq!(
            serde_json::to_value(&minimal).unwrap(),
            json!({ "input_id": "input-1", "instance_id": "production" })
        );
    }

    #[test]
    fn says_which_experiment_statuses_are_terminal() {
        let read =
            |wire: &str| -> ExperimentStatus { serde_json::from_value(json!(wire)).unwrap() };
        for wire in ["completed", "failed", "draft", "canceled", "delete-failed"] {
            assert!(read(wire).is_terminal(), "{wire}");
        }
        for wire in ["started", "stopping", "deleting", "unknown", "paused"] {
            assert!(!read(wire).is_terminal(), "{wire}");
        }
        assert_eq!(read("paused"), ExperimentStatus::Unknown);
    }
}
