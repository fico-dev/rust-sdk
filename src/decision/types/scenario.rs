//! Scenario test types.
//!
//! On the wire a scenario test is a batch experiment with `type=scenario`;
//! the SDK only exposes it as a scenario test. Fields use the API's names,
//! except for the decision nouns (`scenario_id`, `workspace_id`,
//! `environment_id`). On the wire they keep the API's names.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::experiment::ExperimentStatus;
use super::run::Run;

/// A scenario test, including its runs.
///
/// A scenario test runs every scenario, an environment with an input and
/// option values, so their results can be compared.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct ScenarioTest {
    /// ID of the scenario test (API: `id`).
    #[serde(rename = "id")]
    pub scenario_id: String,
    /// Name of the scenario test.
    pub name: String,
    /// Description of the scenario test.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// When the scenario test was created, in RFC 3339.
    pub created_at: String,
    /// When the scenario test was last updated, in RFC 3339.
    pub updated_at: String,
    /// Status of the scenario test. The platform reports a test that is
    /// stopping, being deleted or failed to delete as `started`; its
    /// metadata has the exact status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<ExperimentStatus>,
    /// Number of runs the scenario test requested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number_of_requested_runs: Option<u64>,
    /// Number of runs of the scenario test.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number_of_runs: Option<u64>,
    /// Number of runs of the scenario test that completed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number_of_completed_runs: Option<u64>,
    /// The option sets of the scenario test, by key (`<scenario>_<n>`):
    /// each maps option names to values.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub option_sets: Option<BTreeMap<String, BTreeMap<String, String>>>,
    /// Kind of experiment (API: `type`): `scenario`.
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub experiment_type: Option<String>,
    /// ID of the scenario test's input set. Empty when each run names its
    /// own, as in the scenario tests this SDK creates.
    pub input_set_id: String,
    /// IDs of the environments the scenario test runs (API:
    /// `instance_ids`).
    #[serde(
        rename = "instance_ids",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub environment_ids: Option<Vec<String>>,
    /// Distributions of the runs' metrics, grouped by scenario, option set
    /// and environment. The platform leaves them out of a failed test.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grouped_distributional_summaries: Option<Vec<Map<String, Value>>>,
    /// Runs of the scenario test. Only
    /// [`Platform::get_scenario`](crate::decision::platform::Platform::get_scenario)
    /// fills them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runs: Option<Vec<Run>>,
}

/// Metadata of a scenario test, without its runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct ScenarioTestMetadata {
    /// ID of the scenario test (API: `id`).
    #[serde(rename = "id")]
    pub scenario_id: String,
    /// Name of the scenario test.
    pub name: String,
    /// Description of the scenario test.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// ID of the workspace the scenario test belongs to (API: `app_id`).
    /// Only the metadata of a single test has it, not a list.
    #[serde(rename = "app_id", default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    /// When the scenario test was created, in RFC 3339.
    pub created_at: String,
    /// When the scenario test was last updated, in RFC 3339.
    pub updated_at: String,
    /// Status of the scenario test. A list reports only `started` and
    /// `completed`, and every other status as `unknown`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<ExperimentStatus>,
    /// Number of runs the scenario test requested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number_of_requested_runs: Option<u64>,
    /// Number of runs of the scenario test.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number_of_runs: Option<u64>,
    /// Number of runs of the scenario test that completed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number_of_completed_runs: Option<u64>,
    /// The option sets of the scenario test, by key (`<scenario>_<n>`):
    /// each maps option names to values. Only a list has them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub option_sets: Option<BTreeMap<String, BTreeMap<String, String>>>,
    /// Kind of experiment (API: `type`): `scenario`.
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub experiment_type: Option<String>,
    /// ID of the scenario test's input set. Only a list has it, and it is
    /// empty when each run names its own, as in the scenario tests this SDK
    /// creates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_set_id: Option<String>,
    /// IDs of the environments the scenario test runs (API:
    /// `instance_ids`). Only a list has them.
    #[serde(
        rename = "instance_ids",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub environment_ids: Option<Vec<String>>,
}

/// Information of a scenario test, as an update returns it.
///
/// The platform answers an update with the ID, name, description and
/// timestamps only, so the other fields are `None`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct ScenarioTestInformation {
    /// ID of the scenario test (API: `id`).
    #[serde(rename = "id")]
    pub scenario_id: String,
    /// Name of the scenario test.
    pub name: String,
    /// Description of the scenario test.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// When the scenario test was created, in RFC 3339.
    pub created_at: String,
    /// When the scenario test was last updated, in RFC 3339.
    pub updated_at: String,
    /// Status of the scenario test.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<ExperimentStatus>,
    /// Number of runs the scenario test requested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number_of_requested_runs: Option<u64>,
    /// Number of runs of the scenario test.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number_of_runs: Option<u64>,
    /// Number of runs of the scenario test that completed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number_of_completed_runs: Option<u64>,
    /// The option sets of the scenario test, by key (`<scenario>_<n>`):
    /// each maps option names to values.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub option_sets: Option<BTreeMap<String, BTreeMap<String, String>>>,
}

/// ID of a scenario test that was just created.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct ScenarioTestCreated {
    /// ID of the scenario test.
    pub scenario_id: String,
}

/// An option of a scenario, and the values to try for it.
///
/// A scenario runs every combination of its options' values: options `x`
/// with values `1` and `2` and `y` with `3` and `4` give four runs per
/// input.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ScenarioConfiguration {
    /// Name of the option.
    pub name: String,
    /// Values to try for the option. Needs at least one.
    pub values: Vec<String>,
}

/// Where a scenario's input comes from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ScenarioInputType {
    /// An existing input set.
    #[default]
    InputSet,
    /// Existing managed inputs.
    Input,
    /// New input data, uploaded when the test is created.
    New,
}

/// Input of a scenario.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ScenarioInput {
    /// Where the input comes from.
    pub scenario_input_type: ScenarioInputType,
    /// The input, whose shape depends on `scenario_input_type`: for
    /// `input_set`, the ID of an input set, a string; for `input`, the IDs
    /// of managed inputs, a list of strings; for `new`, the input data to
    /// upload, a list of JSON objects. For `input` and `new`, creating the
    /// test creates an input set of them.
    pub scenario_input_data: Value,
}

/// A scenario of a scenario test: an environment, an input and option values.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Scenario {
    /// ID of the scenario, unique in the test. Defaults to `scenario-<n>`,
    /// where `<n>` is its position in the list, from 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scenario_id: Option<String>,
    /// ID of the environment the scenario runs (API: `instance_id`).
    #[serde(rename = "instance_id")]
    pub environment_id: String,
    /// Input of the scenario.
    pub scenario_input: ScenarioInput,
    /// Options to try, each with its values. The scenario runs every
    /// combination of them. Without it, the scenario runs once per input,
    /// with no options.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub configuration: Option<Vec<ScenarioConfiguration>>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn reads_and_writes_scenarios_with_api_names() {
        let wire = json!({
            "scenario_id": "rainy",
            "instance_id": "production",
            "scenario_input": {
                "scenario_input_type": "input",
                "scenario_input_data": ["monday", "tuesday"],
            },
            "configuration": [{ "name": "solve.duration", "values": ["5s", "10s"] }],
        });
        let scenario: Scenario = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            scenario,
            Scenario {
                scenario_id: Some("rainy".to_owned()),
                environment_id: "production".to_owned(),
                scenario_input: ScenarioInput {
                    scenario_input_type: ScenarioInputType::Input,
                    scenario_input_data: json!(["monday", "tuesday"]),
                },
                configuration: Some(vec![ScenarioConfiguration {
                    name: "solve.duration".to_owned(),
                    values: vec!["5s".to_owned(), "10s".to_owned()],
                }]),
            }
        );
        assert_eq!(serde_json::to_value(&scenario).unwrap(), wire);
    }

    #[test]
    fn reads_scenario_tests_as_the_platform_sends_them() {
        let test: ScenarioTest = serde_json::from_value(json!({
            "id": "scenario-1",
            "name": "Weather",
            "description": "",
            "status": "completed",
            "created_at": "2026-10-20T14:00:00Z",
            "updated_at": "2026-10-20T14:05:00Z",
            "input_set_id": "",
            "instance_ids": null,
            "type": "scenario",
            "grouped_distributional_summaries": null,
        }))
        .unwrap();
        assert_eq!(test.scenario_id, "scenario-1");
        assert_eq!(test.status, Some(ExperimentStatus::Completed));
        assert_eq!(test.environment_ids, None);
        assert_eq!(test.grouped_distributional_summaries, None);
        assert_eq!(test.option_sets, None);
        let created = ScenarioTestCreated {
            scenario_id: "scenario-1".to_owned(),
        };
        assert_eq!(
            serde_json::to_value(&created).unwrap(),
            json!({ "scenario_id": "scenario-1" })
        );
    }
}
