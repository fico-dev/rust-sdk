//! Scenario tests, on `v1/applications/{workspace_id}/experiments/batch` with
//! `type=scenario`. The `batch` requests stay private to this file.

use std::collections::{BTreeMap, HashSet};

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::{DataUploadParams, InputSetCreateParams, ManagedInputCreateParams, Platform};
use crate::decision::id::{safe_id, safe_name_and_id};
use crate::decision::path::api_path;
use crate::decision::polling::{deleting, gone, poll, poll_change};
use crate::decision::{
    ExperimentRun, InputSet, ManagedInput, PollingOptions, Scenario, ScenarioConfiguration,
    ScenarioInputType, ScenarioTest, ScenarioTestCreated, ScenarioTestInformation,
    ScenarioTestMetadata,
};
use crate::tool::{InWorkspace, NoParams, Tool};
use crate::{Error, Result};

/// Tool arguments that address a scenario test, plus the operation's
/// parameters.
#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct ScenarioRef<P> {
    /// ID of the workspace.
    pub(crate) workspace_id: String,
    /// ID of the scenario test.
    pub(crate) scenario_id: String,
    #[serde(flatten)]
    pub(crate) params: P,
}

/// Parameters for [`Platform::create_scenario`].
///
/// `scenarios` is required in tool arguments. The `Default` value leaves it
/// empty, which [`Platform::create_scenario`] rejects, so set it.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
pub struct ScenarioCreateParams {
    /// ID of the scenario test. Generated when not set.
    #[serde(default)]
    pub scenario_id: Option<String>,
    /// Name of the scenario test. Defaults to its ID.
    #[serde(default)]
    pub name: Option<String>,
    /// Description of the scenario test.
    #[serde(default)]
    pub description: Option<String>,
    /// The scenarios to run, each an environment with an input and options
    /// to try. Needs at least one.
    pub scenarios: Vec<Scenario>,
    /// How many more times to run each run: 0, the default, runs each once,
    /// and 2 runs each three times. At most 999.
    #[serde(default)]
    #[schemars(range(max = 999))]
    pub repetitions: Option<u32>,
}

/// Parameters for [`Platform::get_scenario`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
#[serde(default)]
pub struct ScenarioGetParams {
    /// Polls until the scenario test reaches a terminal state, then returns it.
    /// Without it, returns the scenario test's current state. With the
    /// defaults, polling has no time or try limit.
    pub polling: Option<PollingOptions>,
}

/// Parameters for [`Platform::delete_scenario`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
#[serde(default)]
pub struct ScenarioDeleteParams {
    /// Polls until the scenario test is gone. Without it, returns once the
    /// platform accepts the delete, which it finishes later. With the
    /// defaults, polling has no time or try limit.
    pub polling: Option<PollingOptions>,
}

/// Parameters for [`Platform::update_scenario`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
#[serde(default)]
pub struct ScenarioUpdateParams {
    /// New name of the scenario test.
    pub name: Option<String>,
    /// New description of the scenario test.
    pub description: Option<String>,
}

/// What the platform returns when it creates a scenario test.
#[derive(Deserialize)]
struct Created {
    /// ID of the scenario test (API: `id`).
    #[serde(rename = "id")]
    scenario_id: String,
}

/// A scenario's input, checked against its type.
#[derive(Debug, PartialEq)]
enum ScenarioData {
    /// ID of an existing input set.
    InputSet(String),
    /// IDs of existing managed inputs.
    Inputs(Vec<String>),
    /// Input data to upload, one item per managed input.
    New(Vec<Value>),
}

/// A scenario that passed the checks.
#[derive(Debug, PartialEq)]
struct CheckedScenario {
    /// ID of the scenario, given or defaulted.
    scenario_id: String,
    /// ID of the environment the scenario runs.
    environment_id: String,
    /// Input of the scenario.
    data: ScenarioData,
    /// Option values of each of the scenario's option sets.
    combinations: Vec<BTreeMap<String, String>>,
}

/// A scenario whose input set is known: what its runs need.
#[derive(Debug)]
struct ResolvedScenario {
    /// ID of the scenario.
    scenario_id: String,
    /// ID of the environment the scenario runs.
    environment_id: String,
    /// Option values of each of the scenario's option sets.
    combinations: Vec<BTreeMap<String, String>>,
    /// ID of the scenario's input set.
    input_set_id: String,
    /// IDs of the inputs of the input set that the scenario runs.
    input_ids: Vec<String>,
}

impl Platform {
    /// Creates a scenario test from a list of scenarios.
    ///
    /// Each scenario runs its environment on each of its inputs with every
    /// combination of its option values, `repetitions + 1` times. For each
    /// scenario, in order, it gets the environment, then the input set: the
    /// existing one, or a new one of the scenario's managed inputs, or, for
    /// new data, a new one of managed inputs it creates by uploading each
    /// item. Then it creates the test, whose option sets are named
    /// `<scenario>_<n>`, with `<n>` from 0, and whose runs are numbered from
    /// 1.
    ///
    /// An unset or empty `scenario_id` is generated, an unset or empty
    /// `name` defaults to the ID, and a scenario with an unset or empty ID
    /// is `scenario-<n>`, with its position in the list from 1. The input
    /// sets and managed inputs it creates are named after the scenario, and
    /// stay if a later step fails.
    ///
    /// Returns the new test's ID. To wait for it, call
    /// [`Platform::get_scenario`] with `polling`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] before any request if the platform would
    /// reject the test only after the input sets exist:
    ///
    /// - `scenarios` is empty, or two scenarios have the same ID;
    /// - a scenario has no environment ID, or its `scenario_input_data` does
    ///   not have the shape its `scenario_input_type` needs, is empty, or has
    ///   more than 20 inputs;
    /// - an ID (the test's, a scenario's, or a scenario's environment or
    ///   input set) is not 2 to 128 letters, digits, `.`, `-` and `_`, with a
    ///   letter or digit at both ends;
    /// - the name, which defaults to the ID, is not 3 to 128 printable ASCII
    ///   characters, or it or the description (at most 256) starts with `-`
    ///   or `'`, has `+`, `@`, `=` or `"`, or looks like an HTML tag;
    /// - an option of a scenario's `configuration` has no values, or its name
    ///   does not start with a letter or digit followed by letters, digits,
    ///   `-`, `_`, `:` and `.`;
    /// - a scenario has more than 100 options, or a combination of its
    ///   option values is over 4 KiB;
    /// - the scenarios need more than 1,000 option sets, or `repetitions` is
    ///   over 999.
    ///
    /// Returns [`Error::Invalid`] after the input sets exist, and before the
    /// test is created, if it would have more than 1,000 runs, the most any
    /// platform allows.
    ///
    /// Also returns the [errors of every method](Platform#errors).
    pub async fn create_scenario(
        &self,
        workspace_id: &str,
        params: ScenarioCreateParams,
    ) -> Result<ScenarioTestCreated> {
        let scenarios = check_scenarios(params.scenarios)?;
        let repetitions = params.repetitions.unwrap_or(0);
        if repetitions > MAX_REPETITIONS {
            return Err(Error::Invalid {
                reason: format!(
                    "`repetitions` must be at most {MAX_REPETITIONS}: more would give each \
                     input more runs than the platform allows"
                ),
            });
        }
        let scenario_test_id = match params
            .scenario_id
            .filter(|scenario_id| !scenario_id.is_empty())
        {
            Some(scenario_id) => {
                check_id("the scenario test's ID", &scenario_id)?;
                scenario_id
            }
            None => safe_id("scenario"),
        };
        let name = params
            .name
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| scenario_test_id.clone());
        check_text("`name`", &name, 3, 128)?;
        if let Some(description) = params.description.as_deref().filter(|d| !d.is_empty()) {
            check_text("`description`", description, 0, 256)?;
        }
        let mut resolved = Vec::with_capacity(scenarios.len());
        for scenario in scenarios {
            // Checks that the environment exists: it is not used otherwise.
            self.get_environment(workspace_id, &scenario.environment_id)
                .await?;
            let input_set = self
                .scenario_input_set(
                    workspace_id,
                    &scenario_test_id,
                    &scenario.scenario_id,
                    scenario.data,
                )
                .await?;
            // As for acceptance tests: the run inputs, or else the managed
            // inputs.
            let input_ids = if input_set.input_ids.is_empty() {
                input_set
                    .inputs
                    .into_iter()
                    .map(|input| input.managed_input_id)
                    .collect()
            } else {
                input_set.input_ids
            };
            resolved.push(ResolvedScenario {
                scenario_id: scenario.scenario_id,
                environment_id: scenario.environment_id,
                combinations: scenario.combinations,
                input_set_id: input_set.input_set_id,
                input_ids,
            });
        }
        check_run_count(&resolved, repetitions)?;
        let (option_sets, runs) = batch_runs(&resolved, repetitions);
        let mut body = json!({
            "id": scenario_test_id,
            "name": name,
            "type": "scenario",
            "option_sets": option_sets,
            "runs": runs,
        });
        if let Some(description) = params.description {
            body["description"] = description.into();
        }
        let path = batch_path(workspace_id, &[])?;
        let created: Created = self.client.post(path).json(&body).send_json().await?;
        Ok(ScenarioTestCreated {
            scenario_id: created.scenario_id,
        })
    }

    /// Gets a scenario test, including its runs.
    ///
    /// The runs come from a second request, which pages through all of
    /// them. With `params.polling`, it first polls the test's metadata, which
    /// reports every status, until the test reaches a terminal state. Polling
    /// has no limit unless `max_duration` or `max_tries` is set, and a status
    /// this SDK does not know is not terminal, so if the platform adds a
    /// final status, polling waits until a limit or the caller's own timeout
    /// stops it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Wait`] if polling runs out of time or tries.
    ///
    /// Also returns the [errors of every method](Platform#errors).
    pub async fn get_scenario(
        &self,
        workspace_id: &str,
        scenario_id: &str,
        params: ScenarioGetParams,
    ) -> Result<ScenarioTest> {
        if let Some(options) = params.polling {
            poll(&options, || async {
                let metadata = self
                    .get_scenario_metadata(workspace_id, scenario_id)
                    .await?;
                let done = metadata.status.is_some_and(|status| status.is_terminal());
                Ok(done.then_some(()))
            })
            .await?;
        }
        let path = batch_path(workspace_id, &[scenario_id])?;
        let mut test: ScenarioTest = self.client.get(path).send_json().await?;
        let path = batch_path(workspace_id, &[scenario_id, "runs"])?;
        let runs = self
            .client
            .get(path)
            .paginate()
            .items_key("runs")
            .collect()
            .await?;
        test.runs = Some(runs);
        Ok(test)
    }

    /// Lists the scenario tests of a workspace.
    ///
    /// The list reports a test's status only as `started` or `completed`,
    /// and every other status as `unknown`; get a test's metadata for its
    /// status.
    ///
    /// # Errors
    ///
    /// Returns the [errors of every method](Platform#errors).
    pub async fn list_scenarios(&self, workspace_id: &str) -> Result<Vec<ScenarioTestMetadata>> {
        let path = batch_path(workspace_id, &[])?;
        self.client
            .get(path)
            .query(&[("type", "scenario")])
            .paginate()
            .collect()
            .await
    }

    /// Gets the metadata of a scenario test, without its runs.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Status`] with status 404 if the scenario test does not
    /// exist, and the [errors of every method](Platform#errors).
    pub async fn get_scenario_metadata(
        &self,
        workspace_id: &str,
        scenario_id: &str,
    ) -> Result<ScenarioTestMetadata> {
        let path = batch_path(workspace_id, &[scenario_id, "metadata"])?;
        self.client.get(path).send_json().await
    }

    /// Updates a scenario test.
    ///
    /// Only the fields that `params` sets change. The platform answers with
    /// the test's ID, name, description and timestamps only.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`], before any request, if `params` sets no
    /// field.
    ///
    /// Also returns the [errors of every method](Platform#errors).
    pub async fn update_scenario(
        &self,
        workspace_id: &str,
        scenario_id: &str,
        params: ScenarioUpdateParams,
    ) -> Result<ScenarioTestInformation> {
        if params.name.is_none() && params.description.is_none() {
            return Err(Error::Invalid {
                reason: "nothing to update: set `name` or `description`".to_owned(),
            });
        }
        let mut body = Map::new();
        if let Some(name) = params.name {
            body.insert("name".to_owned(), name.into());
        }
        if let Some(description) = params.description {
            body.insert("description".to_owned(), description.into());
        }
        let path = batch_path(workspace_id, &[scenario_id])?;
        self.client.patch(path).json(&body).send_json().await
    }

    /// Deletes a scenario test, and its runs.
    ///
    /// The platform deletes them in the background: with `params.polling`,
    /// this waits until a GET of the test is a 404.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Wait`] if polling runs out of time or tries, or if
    /// the platform reports that the delete failed.
    ///
    /// Also returns the [errors of every method](Platform#errors).
    pub async fn delete_scenario(
        &self,
        workspace_id: &str,
        scenario_id: &str,
        params: ScenarioDeleteParams,
    ) -> Result<()> {
        let path = batch_path(workspace_id, &[scenario_id])?;
        self.client.delete(&path).send().await?;
        if let Some(options) = params.polling {
            // The test itself, not its metadata, which is a 400 for a test
            // that is gone.
            poll_change(&options, || async {
                let result = self.client.get(&path).send_json::<ScenarioTest>().await;
                gone(result, |test| deleting(test.status, "scenario test"))
            })
            .await?;
        }
        Ok(())
    }

    /// The input set of the scenario `scenario_id` of the scenario test
    /// `scenario_test_id`: the existing one, or a new one of its managed
    /// inputs, or a new one of managed inputs uploaded from its data.
    async fn scenario_input_set(
        &self,
        workspace_id: &str,
        scenario_test_id: &str,
        scenario_id: &str,
        data: ScenarioData,
    ) -> Result<InputSet> {
        // The description names the scenario test, not the new resource's
        // own ID.
        let description = format!("Automatically created from scenario test: {scenario_test_id}");
        let inputs = match data {
            ScenarioData::InputSet(input_set_id) => {
                return self.get_input_set(workspace_id, &input_set_id).await;
            }
            ScenarioData::Inputs(managed_input_ids) => managed_input_ids
                .into_iter()
                .map(|managed_input_id| ManagedInput {
                    managed_input_id,
                    ..ManagedInput::default()
                })
                .collect(),
            ScenarioData::New(items) => {
                let mut inputs = Vec::with_capacity(items.len());
                for item in items {
                    let upload = self.create_upload(workspace_id).await?;
                    self.upload_data(DataUploadParams {
                        upload_url: upload.upload_url,
                        data: Some(item),
                        ..DataUploadParams::default()
                    })
                    .await?;
                    let (name, managed_input_id) = safe_name_and_id("man-input", scenario_id);
                    let input = self
                        .create_managed_input(
                            workspace_id,
                            ManagedInputCreateParams {
                                managed_input_id: Some(managed_input_id),
                                name: Some(name),
                                description: Some(description.clone()),
                                upload_id: Some(upload.upload_id),
                                ..ManagedInputCreateParams::default()
                            },
                        )
                        .await?;
                    inputs.push(input);
                }
                inputs
            }
        };
        let (name, input_set_id) = safe_name_and_id("inpset", scenario_id);
        self.create_input_set(
            workspace_id,
            InputSetCreateParams {
                input_set_id: Some(input_set_id),
                name: Some(name),
                description: Some(description),
                maximum_runs: Some(20),
                inputs: Some(inputs),
                ..InputSetCreateParams::default()
            },
        )
        .await
    }
}

/// Path of the scenario tests of `workspace_id`, followed by `segments`.
fn batch_path(workspace_id: &str, segments: &[&str]) -> Result<String> {
    let mut path = vec!["v1", "applications", workspace_id, "experiments", "batch"];
    path.extend_from_slice(segments);
    api_path(&path)
}

/// Most inputs of a scenario: they become an input set, which create makes
/// with a `maximum_runs` of 20.
const MAX_INPUTS: usize = 20;
/// Most option sets a scenario test can have: the backend's
/// `maxOptionSets`, which every deployment shares.
const MAX_OPTION_SETS: usize = 1_000;
/// Most options an option set can have: the backend's `maxBinaryOptions`.
const MAX_OPTIONS: usize = 100;
/// Most bytes of names and values an option set can have: the backend's
/// `maxMapSizeBytes`.
const MAX_OPTION_SET_BYTES: usize = 4 * 1024;
/// Most runs a scenario test can have: the largest `MaxBatchRunCount` of
/// the platform's deployments. Most allow 1,000, some fewer, and those
/// reject a smaller test only when it is created.
const MAX_RUNS: usize = 1_000;
/// Most repetitions a scenario test can have: one more would give each
/// input more than [`MAX_RUNS`] runs.
const MAX_REPETITIONS: u32 = MAX_RUNS as u32 - 1;

/// Checks `scenarios` and gives each its ID. An unset or empty ID is
/// `scenario-<n>`, with the scenario's position from 1.
///
/// This also rejects what the platform would reject only after the
/// scenarios before it created their input sets: an empty environment
/// ID, input set ID, managed input ID or list of inputs, more than 20
/// inputs, a scenario ID or
/// option name that the platform's patterns refuse, too many options or
/// option bytes in an option set, and more than [`MAX_OPTION_SETS`] option
/// sets. The option sets are counted before they are built, so a huge cross
/// product fails without using the memory for it.
fn check_scenarios(scenarios: Vec<Scenario>) -> Result<Vec<CheckedScenario>> {
    if scenarios.is_empty() {
        return Err(Error::Invalid {
            reason: "`scenarios` needs at least one scenario".to_owned(),
        });
    }
    let mut used = HashSet::new();
    let mut option_sets: usize = 0;
    let mut checked = Vec::with_capacity(scenarios.len());
    for (index, scenario) in scenarios.into_iter().enumerate() {
        let scenario_id = scenario
            .scenario_id
            .filter(|scenario_id| !scenario_id.is_empty())
            .unwrap_or_else(|| format!("scenario-{}", index + 1));
        check_id("the scenario ID", &scenario_id)?;
        if !used.insert(scenario_id.clone()) {
            return Err(Error::Invalid {
                reason: format!("more than one scenario has the ID `{scenario_id}`"),
            });
        }
        if scenario.environment_id.is_empty() {
            return Err(Error::Invalid {
                reason: format!("scenario `{scenario_id}` needs an environment ID (`instance_id`)"),
            });
        }
        check_id(
            &format!("the environment ID of scenario `{scenario_id}`"),
            &scenario.environment_id,
        )?;
        let input = scenario.scenario_input;
        let data = scenario_data(
            &scenario_id,
            input.scenario_input_type,
            input.scenario_input_data,
        )?;
        if let ScenarioData::InputSet(input_set_id) = &data {
            check_id(
                &format!("the input set ID of scenario `{scenario_id}`"),
                input_set_id,
            )?;
        }
        // The scenario's inputs become an input set of at most 20.
        let inputs = match &data {
            ScenarioData::InputSet(_) => 0,
            ScenarioData::Inputs(ids) => ids.len(),
            ScenarioData::New(items) => items.len(),
        };
        if inputs > MAX_INPUTS {
            return Err(Error::Invalid {
                reason: format!(
                    "scenario `{scenario_id}` has {inputs} inputs: its input set holds at most \
                     {MAX_INPUTS}"
                ),
            });
        }
        let configuration = scenario.configuration.unwrap_or_default();
        check_options(&scenario_id, &configuration)?;
        // One option set per combination of the scenario's option values.
        option_sets = configuration
            .iter()
            .try_fold(1usize, |count, option| {
                count.checked_mul(option.values.len())
            })
            .and_then(|count| option_sets.checked_add(count))
            .filter(|&total| total <= MAX_OPTION_SETS)
            .ok_or_else(|| Error::Invalid {
                reason: format!(
                    "the scenarios need more than {MAX_OPTION_SETS} option sets, the \
                     platform's limit: each scenario has one per combination of its option \
                     values"
                ),
            })?;
        checked.push(CheckedScenario {
            scenario_id,
            environment_id: scenario.environment_id,
            data,
            combinations: combinations(&configuration),
        });
    }
    Ok(checked)
}

/// Checks that `id`, which `what` names, has the platform's ID pattern
/// (`IDRegex`): 2 to 128 ASCII letters, digits, `.`, `-` and `_`, with a
/// letter or digit at both ends.
fn check_id(what: &str, id: &str) -> Result<()> {
    let bytes = id.as_bytes();
    let valid = (2..=128).contains(&bytes.len())
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
        && bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        && bytes.last().is_some_and(u8::is_ascii_alphanumeric);
    if valid {
        return Ok(());
    }
    Err(Error::Invalid {
        reason: format!(
            "{what} `{id}` must be 2 to 128 letters, digits, `.`, `-` and `_`, with a letter \
             or digit at both ends"
        ),
    })
}

/// Checks that `text`, a name or a description that `what` names, is what
/// the platform takes: `min` to `max` printable ASCII characters, not
/// starting with `-` or `'`, without `+`, `@`, `=` or `"`, and without
/// anything that looks like an HTML tag (`NameAndDescRegex` and `TagRegex`).
fn check_text(what: &str, text: &str, min: usize, max: usize) -> Result<()> {
    let invalid = |rule: String| {
        Err(Error::Invalid {
            reason: format!("{what} {rule}"),
        })
    };
    if !text.bytes().all(|b| (b' '..=b'~').contains(&b)) {
        return invalid("must be printable ASCII".to_owned());
    }
    if !(min..=max).contains(&text.len()) {
        return invalid(match min {
            0 => format!("must be at most {max} characters"),
            _ => format!("must be {min} to {max} characters"),
        });
    }
    let tag = text
        .find('<')
        .is_some_and(|start| text.get(start + 2..).is_some_and(|rest| rest.contains('>')));
    if text.starts_with(['-', '\'']) || text.contains(['+', '@', '=', '"']) || tag {
        return invalid(
            "must not start with `-` or `'`, or have `+`, `@`, `=`, `\"` or an HTML tag".to_owned(),
        );
    }
    Ok(())
}

/// Checks the options of the scenario `scenario_id` against what the
/// platform takes for each of its option sets: names with the option-key
/// pattern (`OptionKeyRegex`), at least one value each, at most
/// [`MAX_OPTIONS`] options and at most [`MAX_OPTION_SET_BYTES`] of names and
/// values. The bytes are counted for the largest combination, with each
/// option's longest value.
fn check_options(scenario_id: &str, configuration: &[ScenarioConfiguration]) -> Result<()> {
    // As in `combinations`, a later option with the same name replaces an
    // earlier one.
    let mut longest: BTreeMap<&str, usize> = BTreeMap::new();
    for option in configuration {
        let name = option.name.as_bytes();
        if name.is_empty() {
            return Err(Error::Invalid {
                reason: format!("an option of scenario `{scenario_id}` needs a name"),
            });
        }
        let valid = name[0].is_ascii_alphanumeric()
            && name
                .iter()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b':' | b'.'));
        if !valid {
            return Err(Error::Invalid {
                reason: format!(
                    "option `{}` of scenario `{scenario_id}` must start with a letter or digit, \
                     followed by letters, digits, `-`, `_`, `:` and `.`",
                    option.name
                ),
            });
        }
        let Some(value_len) = option.values.iter().map(String::len).max() else {
            return Err(Error::Invalid {
                reason: format!(
                    "option `{}` of scenario `{scenario_id}` needs at least one value",
                    option.name
                ),
            });
        };
        longest.insert(&option.name, value_len);
    }
    if longest.len() > MAX_OPTIONS {
        return Err(Error::Invalid {
            reason: format!(
                "scenario `{scenario_id}` has more than {MAX_OPTIONS} options, the platform's \
                 limit"
            ),
        });
    }
    let bytes: usize = longest
        .iter()
        .map(|(name, value_len)| name.len() + value_len)
        .sum();
    if bytes > MAX_OPTION_SET_BYTES {
        return Err(Error::Invalid {
            reason: format!(
                "a combination of the options of scenario `{scenario_id}` has {bytes} bytes of \
                 names and values, over the platform's limit of {MAX_OPTION_SET_BYTES}"
            ),
        });
    }
    Ok(())
}

/// Checks that `scenarios` with `repetitions` make at most [`MAX_RUNS`]
/// runs, counted without building them.
fn check_run_count(scenarios: &[ResolvedScenario], repetitions: u32) -> Result<()> {
    let runs = scenarios.iter().try_fold(0usize, |total, scenario| {
        scenario
            .combinations
            .len()
            .checked_mul(scenario.input_ids.len())?
            .checked_mul(repetitions as usize + 1)?
            .checked_add(total)
    });
    match runs {
        Some(runs) if runs <= MAX_RUNS => Ok(()),
        _ => Err(Error::Invalid {
            reason: format!(
                "the scenario test would have more than {MAX_RUNS} runs, the most the platform \
                 allows: one per scenario, option set, input and repetition"
            ),
        }),
    }
}

/// The input `data` of the scenario `scenario_id`, if it has the shape that
/// `input_type` needs: an ID for an input set, a list of IDs for managed
/// inputs, and a list of JSON objects for new data.
fn scenario_data(
    scenario_id: &str,
    input_type: ScenarioInputType,
    data: Value,
) -> Result<ScenarioData> {
    let invalid = |shape: &str| Error::Invalid {
        reason: format!(
            "the `scenario_input_data` of scenario `{scenario_id}` must be {shape} for the \
             `scenario_input_type` `{}`",
            input_type_name(input_type)
        ),
    };
    match input_type {
        ScenarioInputType::InputSet => match data {
            Value::String(input_set_id) if !input_set_id.is_empty() => {
                Ok(ScenarioData::InputSet(input_set_id))
            }
            _ => Err(invalid("the ID of an input set")),
        },
        ScenarioInputType::Input => {
            let shape = "a list of managed input IDs";
            let Value::Array(items) = data else {
                return Err(invalid(shape));
            };
            if items.is_empty() {
                return Err(invalid(shape));
            }
            items
                .into_iter()
                .map(|item| match item {
                    Value::String(managed_input_id) if !managed_input_id.is_empty() => {
                        Ok(managed_input_id)
                    }
                    _ => Err(invalid(shape)),
                })
                .collect::<Result<_>>()
                .map(ScenarioData::Inputs)
        }
        ScenarioInputType::New => match data {
            Value::Array(items) if !items.is_empty() && items.iter().all(Value::is_object) => {
                Ok(ScenarioData::New(items))
            }
            _ => Err(invalid("a list of JSON objects, the inputs")),
        },
    }
}

/// The wire name of `input_type`.
fn input_type_name(input_type: ScenarioInputType) -> &'static str {
    match input_type {
        ScenarioInputType::InputSet => "input_set",
        ScenarioInputType::Input => "input",
        ScenarioInputType::New => "new",
    }
}

/// The option values of each combination of `configuration`'s values: the
/// cross product, with the first option's values varying slowest and the
/// last option's fastest. No options give one empty combination.
fn combinations(configuration: &[ScenarioConfiguration]) -> Vec<BTreeMap<String, String>> {
    let mut combinations = vec![BTreeMap::new()];
    for option in configuration {
        combinations = combinations
            .into_iter()
            .flat_map(|combination| {
                option.values.iter().map(move |value| {
                    let mut combination = combination.clone();
                    combination.insert(option.name.clone(), value.clone());
                    combination
                })
            })
            .collect();
    }
    combinations
}

/// The option sets and runs of a scenario test of `scenarios`: for each
/// scenario, option set, input and repetition, in that order, one run,
/// numbered from 1.
/// Option sets are named `<scenario>_<n>`, with `<n>` from 0.
fn batch_runs(
    scenarios: &[ResolvedScenario],
    repetitions: u32,
) -> (
    BTreeMap<String, BTreeMap<String, String>>,
    Vec<ExperimentRun>,
) {
    let mut option_sets = BTreeMap::new();
    let mut runs = Vec::new();
    for scenario in scenarios {
        for (index, combination) in scenario.combinations.iter().enumerate() {
            let option_set = format!("{}_{index}", scenario.scenario_id);
            option_sets.insert(option_set.clone(), combination.clone());
            for input_id in &scenario.input_ids {
                for repetition in 0..=repetitions {
                    let run_number = (runs.len() + 1).to_string();
                    runs.push(ExperimentRun {
                        input_id: input_id.clone(),
                        option_set: Some(option_set.clone()),
                        environment_id: Some(scenario.environment_id.clone()),
                        input_set_id: Some(scenario.input_set_id.clone()),
                        scenario_id: Some(scenario.scenario_id.clone()),
                        repetition: Some(repetition),
                        run_number: Some(run_number),
                        ..ExperimentRun::default()
                    });
                }
            }
        }
    }
    (option_sets, runs)
}

pub(crate) fn tools() -> Vec<Tool> {
    vec![
        Tool::new(
            "decision_platform_scenario_create",
            "Creates a scenario test from a list of scenarios.",
            |cx, a: InWorkspace<ScenarioCreateParams>| async move {
                cx.platform()?
                    .create_scenario(&a.workspace_id, a.params)
                    .await
            },
        ),
        Tool::new(
            "decision_platform_scenario_get",
            "Gets a scenario test, including its runs.",
            |cx, a: ScenarioRef<ScenarioGetParams>| async move {
                cx.platform()?
                    .get_scenario(&a.workspace_id, &a.scenario_id, a.params)
                    .await
            },
        )
        .read_only(),
        Tool::new(
            "decision_platform_scenario_list",
            "Lists the scenario tests of a workspace.",
            |cx, a: InWorkspace<NoParams>| async move {
                cx.platform()?.list_scenarios(&a.workspace_id).await
            },
        )
        .read_only(),
        Tool::new(
            "decision_platform_scenario_metadata_get",
            "Gets the metadata of a scenario test, without its runs.",
            |cx, a: ScenarioRef<NoParams>| async move {
                cx.platform()?
                    .get_scenario_metadata(&a.workspace_id, &a.scenario_id)
                    .await
            },
        )
        .read_only(),
        Tool::new(
            "decision_platform_scenario_update",
            "Updates a scenario test.",
            |cx, a: ScenarioRef<ScenarioUpdateParams>| async move {
                cx.platform()?
                    .update_scenario(&a.workspace_id, &a.scenario_id, a.params)
                    .await
            },
        )
        .destructive(),
        Tool::new(
            "decision_platform_scenario_delete",
            "Deletes a scenario test.",
            |cx, a: ScenarioRef<ScenarioDeleteParams>| async move {
                cx.platform()?
                    .delete_scenario(&a.workspace_id, &a.scenario_id, a.params)
                    .await
            },
        )
        .destructive(),
    ]
}

#[cfg(test)]
mod tests {
    use crate::decision::ScenarioInput;

    use super::*;

    /// A scenario of `environment_id` on the input set `last-week`, with
    /// `configuration`.
    fn scenario(
        scenario_id: Option<&str>,
        environment_id: &str,
        configuration: Option<Vec<ScenarioConfiguration>>,
    ) -> Scenario {
        Scenario {
            scenario_id: scenario_id.map(str::to_owned),
            environment_id: environment_id.to_owned(),
            scenario_input: ScenarioInput {
                scenario_input_type: ScenarioInputType::InputSet,
                scenario_input_data: json!("last-week"),
            },
            configuration,
        }
    }

    /// An option `name` with `values`.
    fn option(name: &str, values: &[&str]) -> ScenarioConfiguration {
        ScenarioConfiguration {
            name: name.to_owned(),
            values: values.iter().map(|value| value.to_string()).collect(),
        }
    }

    /// `pairs` as a map of option names to values.
    fn options(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect()
    }

    fn reason(result: Result<Vec<CheckedScenario>>) -> String {
        match result {
            Err(Error::Invalid { reason }) => reason,
            other => panic!("expected Error::Invalid, got {other:?}"),
        }
    }

    #[test]
    fn crosses_option_values_with_the_first_option_slowest() {
        let configuration = [option("x", &["1", "2"]), option("y", &["3", "4"])];
        assert_eq!(
            combinations(&configuration),
            [
                options(&[("x", "1"), ("y", "3")]),
                options(&[("x", "1"), ("y", "4")]),
                options(&[("x", "2"), ("y", "3")]),
                options(&[("x", "2"), ("y", "4")]),
            ]
        );
        assert_eq!(combinations(&[]), [BTreeMap::new()]);
    }

    #[test]
    fn numbers_runs_for_each_scenario_option_set_input_and_repetition() {
        let scenarios = [
            ResolvedScenario {
                scenario_id: "rainy".to_owned(),
                environment_id: "production".to_owned(),
                combinations: vec![options(&[("x", "1")]), options(&[("x", "2")])],
                input_set_id: "last-week".to_owned(),
                input_ids: vec!["run-1".to_owned()],
            },
            ResolvedScenario {
                scenario_id: "sunny".to_owned(),
                environment_id: "staging".to_owned(),
                combinations: vec![BTreeMap::new()],
                input_set_id: "weekdays".to_owned(),
                input_ids: vec!["monday".to_owned(), "tuesday".to_owned()],
            },
        ];
        let (option_sets, runs) = batch_runs(&scenarios, 1);
        assert_eq!(
            json!(option_sets),
            json!({ "rainy_0": { "x": "1" }, "rainy_1": { "x": "2" }, "sunny_0": {} })
        );
        let runs: Vec<(String, String, String, u32, String)> = runs
            .into_iter()
            .map(|run| {
                (
                    run.input_id,
                    run.option_set.unwrap(),
                    run.environment_id.unwrap(),
                    run.repetition.unwrap(),
                    run.run_number.unwrap(),
                )
            })
            .collect();
        let expected = [
            ("run-1", "rainy_0", "production", 0, "1"),
            ("run-1", "rainy_0", "production", 1, "2"),
            ("run-1", "rainy_1", "production", 0, "3"),
            ("run-1", "rainy_1", "production", 1, "4"),
            ("monday", "sunny_0", "staging", 0, "5"),
            ("monday", "sunny_0", "staging", 1, "6"),
            ("tuesday", "sunny_0", "staging", 0, "7"),
            ("tuesday", "sunny_0", "staging", 1, "8"),
        ]
        .map(|(input, set, environment, repetition, number)| {
            (
                input.to_owned(),
                set.to_owned(),
                environment.to_owned(),
                repetition,
                number.to_owned(),
            )
        });
        assert_eq!(runs, expected);
    }

    #[test]
    fn defaults_scenario_ids_to_their_positions() {
        let checked = check_scenarios(vec![
            scenario(None, "production", None),
            scenario(Some(""), "production", None),
            scenario(Some("rainy"), "staging", Some(vec![option("x", &["1"])])),
        ])
        .unwrap();
        let ids: Vec<&str> = checked.iter().map(|s| s.scenario_id.as_str()).collect();
        assert_eq!(ids, ["scenario-1", "scenario-2", "rainy"]);
        assert_eq!(
            checked[2].data,
            ScenarioData::InputSet("last-week".to_owned())
        );
        assert_eq!(checked[2].combinations, [options(&[("x", "1")])]);
    }

    #[test]
    fn rejects_scenarios_that_cannot_run() {
        assert!(reason(check_scenarios(Vec::new())).contains("at least one scenario"));
        let duplicate = check_scenarios(vec![
            scenario(Some("scenario-2"), "production", None),
            scenario(None, "production", None),
        ]);
        assert!(reason(duplicate).contains("`scenario-2`"));
        let no_environment = check_scenarios(vec![scenario(None, "", None)]);
        assert!(reason(no_environment).contains("environment ID"));
        let no_values = check_scenarios(vec![scenario(
            None,
            "production",
            Some(vec![option("x", &[])]),
        )]);
        assert!(reason(no_values).contains("option `x`"));
        let no_name = check_scenarios(vec![scenario(
            None,
            "production",
            Some(vec![option("", &["1"])]),
        )]);
        assert!(reason(no_name).contains("needs a name"));
    }

    #[test]
    fn checks_ids_against_the_platforms_pattern() {
        for valid in ["ab", "rainy-day_2.0", &"a".repeat(128)] {
            assert!(check_id("ID", valid).is_ok(), "{valid}");
        }
        for invalid in [
            "a",
            "rainy day",
            "-rainy",
            "rainy_",
            "río",
            &"a".repeat(129),
        ] {
            match check_id("ID", invalid) {
                Err(Error::Invalid { reason }) => {
                    assert!(reason.contains("2 to 128"), "{invalid}: {reason}")
                }
                other => panic!("{invalid}: expected Error::Invalid, got {other:?}"),
            }
        }
        let scenario_id = check_scenarios(vec![scenario(Some("High demand"), "production", None)]);
        assert!(reason(scenario_id).contains("scenario ID `High demand`"));
        let environment = check_scenarios(vec![scenario(None, "prod env", None)]);
        assert!(reason(environment).contains("the environment ID of scenario `scenario-1`"));
        let input_set = check_scenarios(vec![Scenario {
            scenario_input: ScenarioInput {
                scenario_input_type: ScenarioInputType::InputSet,
                scenario_input_data: json!("last week"),
            },
            ..scenario(None, "production", None)
        }]);
        assert!(reason(input_set).contains("the input set ID of scenario `scenario-1`"));
    }

    #[test]
    fn checks_names_and_descriptions_as_the_platform_does() {
        let check = |text: &str| check_text("`name`", text, 3, 128);
        for valid in [
            "Weather",
            "Rain vs. sun (2026)",
            "a<>b",
            "a > b",
            &"x".repeat(128),
        ] {
            assert!(check(valid).is_ok(), "{valid}");
        }
        let cases = [
            ("ab", "3 to 128"),
            (&*"x".repeat(129), "3 to 128"),
            ("Lluvia, año", "printable ASCII"),
            ("tab\there", "printable ASCII"),
            ("-rainy", "must not start"),
            ("'rainy'", "must not start"),
            ("rain+sun", "must not start"),
            ("me@example.com", "must not start"),
            ("a=b", "must not start"),
            ("say \"hi\"", "must not start"),
            ("<b>bold</b>", "HTML tag"),
            ("a < b > c", "HTML tag"),
        ];
        for (text, message) in cases {
            match check(text) {
                Err(Error::Invalid { reason }) => {
                    assert!(reason.contains(message), "{text}: {reason}")
                }
                other => panic!("{text}: expected Error::Invalid, got {other:?}"),
            }
        }
        // A description may be short, but not over 256.
        assert!(check_text("`description`", "ok", 0, 256).is_ok());
        let long = check_text("`description`", &"x".repeat(257), 0, 256);
        assert!(matches!(long, Err(Error::Invalid { reason }) if reason.contains("at most 256")));
    }

    #[test]
    fn checks_options_against_the_platforms_limits() {
        let check = |configuration: Vec<ScenarioConfiguration>| {
            check_scenarios(vec![scenario(None, "production", Some(configuration))])
        };
        for name in ["solve.duration", "a", "x:y-z_1"] {
            assert!(check(vec![option(name, &["1"])]).is_ok(), "{name}");
        }
        for name in ["solve duration", ".hidden", "-x", "ñ"] {
            let message = reason(check(vec![option(name, &["1"])]));
            assert!(
                message.contains("must start with a letter or digit"),
                "{name}: {message}"
            );
        }
        // 100 options fit, 101 do not.
        let many = |count: usize| {
            (0..count)
                .map(|i| option(&format!("o{i}"), &["1"]))
                .collect()
        };
        assert!(check(many(100)).is_ok());
        assert!(reason(check(many(101))).contains("more than 100 options"));
        // The bytes count each option's longest value: 4 KiB fit, one more
        // byte does not.
        let value = "v".repeat(4 * 1024 - 1);
        assert!(check(vec![option("x", &["1", &value])]).is_ok());
        let value = "v".repeat(4 * 1024);
        let message = reason(check(vec![option("x", &["1", &value])]));
        assert!(message.contains("4097 bytes"), "{message}");
    }

    #[test]
    fn counts_runs_without_building_them() {
        let resolved = |combinations: usize, inputs: usize| ResolvedScenario {
            scenario_id: "rainy".to_owned(),
            environment_id: "production".to_owned(),
            combinations: vec![BTreeMap::new(); combinations],
            input_set_id: "last-week".to_owned(),
            input_ids: (0..inputs).map(|i| format!("input-{i}")).collect(),
        };
        // 2 × (10 × 25 × 2) = 1,000 runs fit; one more input does not.
        assert!(check_run_count(&[resolved(10, 25), resolved(10, 25)], 1).is_ok());
        let over = check_run_count(&[resolved(10, 25), resolved(10, 26)], 1);
        assert!(matches!(over, Err(Error::Invalid { reason }) if reason.contains("1000 runs")));
        // The largest repetitions on one input fit.
        assert!(check_run_count(&[resolved(1, 1)], MAX_REPETITIONS).is_ok());
    }

    #[test]
    fn counts_option_sets_before_building_them() {
        let ten: Vec<String> = (0..10).map(|value| value.to_string()).collect();
        let ten: Vec<&str> = ten.iter().map(String::as_str).collect();
        let options = |count: usize| -> Vec<ScenarioConfiguration> {
            (0..count)
                .map(|index| option(&format!("o{index}"), &ten))
                .collect()
        };
        // 1,000 option sets fit, across scenarios too: 9 × 100, then 90 and
        // 8, then 1 each for a scenario without options and one with a
        // single value.
        let mut scenarios: Vec<Scenario> = (0..9)
            .map(|index| scenario(Some(&format!("s{index}")), "production", Some(options(2))))
            .collect();
        let rest = [
            Some(vec![option("x", &ten), option("y", &ten[..9])]),
            Some(vec![option("x", &ten[..8])]),
            None,
            Some(vec![option("x", &["1"])]),
        ];
        for (index, configuration) in rest.into_iter().enumerate() {
            let scenario_id = format!("s{}", 9 + index);
            scenarios.push(scenario(Some(&scenario_id), "production", configuration));
        }
        let at_limit = check_scenarios(scenarios).unwrap();
        let total: usize = at_limit.iter().map(|s| s.combinations.len()).sum();
        assert_eq!(total, MAX_OPTION_SETS);
        // One more fails.
        let over = check_scenarios(vec![
            scenario(Some("rainy"), "production", Some(options(3))),
            scenario(Some("sunny"), "production", None),
        ]);
        assert!(reason(over).contains("more than 1000 option sets"));
        // 10^30 combinations overflow the count instead of being built.
        let huge = check_scenarios(vec![scenario(None, "production", Some(options(30)))]);
        assert!(reason(huge).contains("more than 1000 option sets"));
    }

    #[test]
    fn checks_the_shape_of_scenario_input_data() {
        let data = |input_type, data| scenario_data("rainy", input_type, data);
        assert_eq!(
            data(ScenarioInputType::Input, json!(["monday"])).unwrap(),
            ScenarioData::Inputs(vec!["monday".to_owned()])
        );
        assert_eq!(
            data(ScenarioInputType::New, json!([{ "stops": [] }])).unwrap(),
            ScenarioData::New(vec![json!({ "stops": [] })])
        );
        let bad = [
            (ScenarioInputType::InputSet, json!(["last-week"])),
            (ScenarioInputType::InputSet, json!("")),
            (ScenarioInputType::Input, json!("monday")),
            (ScenarioInputType::Input, json!([])),
            (ScenarioInputType::Input, json!(["monday", 7])),
            (ScenarioInputType::Input, json!([""])),
            (ScenarioInputType::New, json!({ "stops": [] })),
            (ScenarioInputType::New, json!([])),
            (ScenarioInputType::New, json!(["{}"])),
        ];
        for (input_type, value) in bad {
            match data(input_type, value.clone()) {
                Err(Error::Invalid { reason }) => assert!(
                    reason.contains(input_type_name(input_type)),
                    "{value}: {reason}"
                ),
                other => panic!("{value}: expected Error::Invalid, got {other:?}"),
            }
        }
    }
}
