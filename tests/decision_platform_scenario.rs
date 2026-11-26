//! Tests for the platform scenario test operations and their tools, against
//! the mock server.

mod mock_server;
mod support;

use fico_sdk::Error;
use fico_sdk::decision::platform::{
    ScenarioCreateParams, ScenarioDeleteParams, ScenarioGetParams, ScenarioUpdateParams,
};
use fico_sdk::decision::{
    ExperimentStatus, PollingOptions, Scenario, ScenarioConfiguration, ScenarioInput,
    ScenarioInputType,
};
use fico_sdk::tool::ToolError;
use mock_server::{MockResponse, MockServer, RecordedRequest};
use serde_json::{Value, json};

/// Path of the workspace `routing`.
const WORKSPACE: &str = "/v1/applications/routing";
/// Path of the experiments of the `routing` workspace.
const EXPERIMENTS: &str = "/v1/applications/routing/experiments";
/// Path of the scenario tests of the `routing` workspace.
const TESTS: &str = "/v1/applications/routing/experiments/batch";
/// Path of the scenario test `scenario-1`.
const TEST: &str = "/v1/applications/routing/experiments/batch/scenario-1";

/// An environment as the platform returns it.
fn environment_json(environment_id: &str) -> Value {
    json!({
        "id": environment_id,
        "application_id": "routing",
        "version_id": "v1",
        "name": environment_id,
        "description": "",
        "configuration": { "execution_class": "6c9500mb870s" },
        "locked": false,
        "created_at": "2026-10-01T12:00:00Z",
        "updated_at": "2026-10-01T12:00:00Z",
    })
}

/// An input set of run inputs, as the platform returns it.
fn input_set_json() -> Value {
    json!({
        "id": "last-week",
        "name": "Last week",
        "description": "Runs of the production environment.",
        "app_id": "routing",
        "created_at": "2026-10-01T12:00:00Z",
        "input_ids": ["run-1", "run-2"],
        "updated_at": "2026-10-01T12:00:00Z",
        "inputs": [],
    })
}

/// An input set of the managed inputs `managed_input_ids`, as the platform
/// returns it after creating it.
fn managed_input_set_json(input_set_id: &str, managed_input_ids: &[&str]) -> Value {
    let inputs: Vec<Value> = managed_input_ids
        .iter()
        .map(|id| json!({ "id": id, "name": id, "description": "" }))
        .collect();
    json!({
        "id": input_set_id,
        "name": input_set_id,
        "description": "Automatically created from scenario test: scenario-1",
        "app_id": "routing",
        "created_at": "2026-10-20T14:00:00Z",
        "input_ids": [],
        "updated_at": "2026-10-20T14:00:00Z",
        "inputs": inputs,
    })
}

/// A managed input as the platform returns it after creating it.
fn managed_input_json(managed_input_id: &str) -> Value {
    json!({
        "id": managed_input_id,
        "name": managed_input_id,
        "description": "Automatically created from scenario test: scenario-1",
        "app_id": "routing",
        "upload_id": "0d7ab3fa",
        "format": { "input": { "type": "json" }, "output": { "type": "json" } },
        "created_at": "2026-10-20T14:00:00Z",
        "updated_at": "2026-10-20T14:00:00Z",
    })
}

/// The answer of `POST A/runs/uploadurl`, with an upload URL on `bucket`.
fn upload_json(bucket: &MockServer, upload_id: &str) -> Value {
    json!({
        "upload_id": upload_id,
        "upload_url": format!("{}/uploads/{upload_id}?X-Amz-Signature=abc", bucket.url()),
    })
}

/// A scenario test as `GET X/batch/{id}` returns it, with `status`.
fn test_json(status: &str) -> Value {
    json!({
        "id": "scenario-1",
        "name": "Weather",
        "description": "Rain against sun.",
        "status": status,
        "created_at": "2026-10-20T14:00:00.123456Z",
        "updated_at": "2026-10-20T14:05:00.123456Z",
        "input_set_id": "",
        "instance_ids": ["production", "staging"],
        "option_sets": {
            "rainy_0": { "solve.duration": "5s" },
            "rainy_1": { "solve.duration": "10s" },
        },
        "number_of_requested_runs": 4,
        "number_of_runs": 4,
        "number_of_completed_runs": 4,
        "type": "scenario",
        "grouped_distributional_summaries": [{
            "group_keys": ["scenarioID", "optionSetID"],
            "group_values": ["rainy", "rainy_0"],
            "indicator_keys": ["cost"],
            "indicator_distributions": {},
            "number_of_runs_total": 2,
        }],
    })
}

/// Scenario test metadata as `GET X/batch/{id}/metadata` returns it.
fn metadata_json(status: &str) -> Value {
    json!({
        "id": "scenario-1",
        "name": "Weather",
        "description": "Rain against sun.",
        "app_id": "routing",
        "created_at": "2026-10-20T14:00:00Z",
        "updated_at": "2026-10-20T14:05:00Z",
        "status": status,
        "number_of_requested_runs": 4,
        "number_of_runs": 4,
        "type": "scenario",
    })
}

/// A run of the scenario test as `GET …/runs` lists it.
fn run_json(run_id: &str) -> Value {
    json!({
        "id": run_id,
        "user_email": "dev@example.com",
        "name": "",
        "description": "",
        "created_at": "2026-10-20T14:01:00Z",
        "application_id": "routing",
        "application_instance_id": "production",
        "application_version_id": "v1",
        "run_type": { "type": "standard", "definition_id": "", "reference_id": "" },
        "execution_class": "6c9500mb870s",
        "runtime": "ghcr.io/nextmv-io/runtime/default:latest",
        "status": "succeeded",
        "status_v2": "succeeded",
        "experiment_id": "scenario-1",
        "scenario_id": "rainy",
        "repetition": 0,
        "input_set_id": "last-week",
    })
}

/// What the platform's update returns.
fn updated_json() -> Value {
    json!({
        "id": "scenario-1",
        "name": "Weather",
        "description": "Rain against sun, again.",
        "created_at": "2026-10-20T14:00:00Z",
        "updated_at": "2026-10-20T15:00:00Z",
    })
}

/// A scenario `scenario_id` of the environment `environment_id`, whose
/// input is `data` of `input_type`.
fn scenario(
    scenario_id: Option<&str>,
    environment_id: &str,
    input_type: ScenarioInputType,
    data: Value,
) -> Scenario {
    Scenario {
        scenario_id: scenario_id.map(str::to_owned),
        environment_id: environment_id.to_owned(),
        scenario_input: ScenarioInput {
            scenario_input_type: input_type,
            scenario_input_data: data,
        },
        configuration: None,
    }
}

/// An option `name` with `values`.
fn option(name: &str, values: &[&str]) -> ScenarioConfiguration {
    ScenarioConfiguration {
        name: name.to_owned(),
        values: values.iter().map(|value| value.to_string()).collect(),
    }
}

/// Create parameters for the test `scenario-1` of `scenarios`.
fn params(scenarios: Vec<Scenario>) -> ScenarioCreateParams {
    ScenarioCreateParams {
        scenario_id: Some("scenario-1".to_owned()),
        scenarios,
        ..ScenarioCreateParams::default()
    }
}

/// Each request as `METHOD target`.
fn sent(requests: &[RecordedRequest]) -> Vec<String> {
    let line = |r: &RecordedRequest| format!("{} {}", r.method, r.target);
    requests.iter().map(line).collect()
}

/// A run of the batch body, as create sends it.
fn run(
    input_id: &str,
    option_set: &str,
    environment_id: &str,
    input_set_id: &str,
    repetition: u32,
    run_number: u32,
) -> Value {
    let scenario_id = option_set.rsplit_once('_').unwrap().0;
    json!({
        "input_id": input_id,
        "option_set": option_set,
        "instance_id": environment_id,
        "input_set_id": input_set_id,
        "scenario_id": scenario_id,
        "repetition": repetition,
        "run_number": run_number.to_string(),
    })
}

/// Asserts that `id` is `<prefix>-<words>-<8 random characters>`, as
/// `safe_name_and_id` makes it, and that `name` is it in Start Case.
fn assert_safe_name_and_id(name: &Value, id: &Value, prefix_and_words: &str) {
    let id = id.as_str().unwrap();
    let suffix = id
        .strip_prefix(&format!("{prefix_and_words}-"))
        .unwrap_or_else(|| panic!("{id}"));
    assert_eq!(suffix.len(), 8, "{id}");
    let start_case: Vec<String> = id
        .split('-')
        .map(|word| {
            let (first, rest) = word.split_at(1);
            format!("{}{rest}", first.to_uppercase())
        })
        .collect();
    assert_eq!(name, &json!(start_case.join(" ")));
}

#[tokio::test]
async fn creates_a_scenario_test_on_existing_input_sets() {
    let server = MockServer::start(vec![
        MockResponse::json(200, environment_json("production")),
        MockResponse::json(200, input_set_json()),
        MockResponse::json(200, environment_json("staging")),
        MockResponse::json(200, input_set_json()),
        MockResponse::json(202, json!({ "id": "scenario-1" })),
    ])
    .await;
    let rainy = Scenario {
        configuration: Some(vec![
            option("solve.duration", &["5s", "10s"]),
            option("solve.iterations", &["10"]),
        ]),
        ..scenario(
            Some("rainy"),
            "production",
            ScenarioInputType::InputSet,
            json!("last-week"),
        )
    };
    let sunny = scenario(
        None,
        "staging",
        ScenarioInputType::InputSet,
        json!("last-week"),
    );
    let params = ScenarioCreateParams {
        name: Some("Weather".to_owned()),
        description: Some("Rain against sun.".to_owned()),
        repetitions: Some(1),
        ..params(vec![rainy, sunny])
    };
    let created = support::platform(&server)
        .create_scenario("routing", params)
        .await
        .unwrap();
    assert_eq!(created.scenario_id, "scenario-1");

    let requests = server.requests();
    assert_eq!(
        sent(&requests),
        [
            format!("GET {WORKSPACE}/instances/production"),
            format!("GET {EXPERIMENTS}/inputsets/last-week"),
            format!("GET {WORKSPACE}/instances/staging"),
            format!("GET {EXPERIMENTS}/inputsets/last-week"),
            format!("POST {TESTS}"),
        ]
    );
    for request in &requests {
        assert_eq!(request.header("authorization"), Some("Bearer secret"));
    }
    assert_eq!(
        support::body(&requests[4]),
        json!({
            "id": "scenario-1",
            "name": "Weather",
            "description": "Rain against sun.",
            "type": "scenario",
            "option_sets": {
                "rainy_0": { "solve.duration": "5s", "solve.iterations": "10" },
                "rainy_1": { "solve.duration": "10s", "solve.iterations": "10" },
                "scenario-2_0": {},
            },
            "runs": [
                run("run-1", "rainy_0", "production", "last-week", 0, 1),
                run("run-1", "rainy_0", "production", "last-week", 1, 2),
                run("run-2", "rainy_0", "production", "last-week", 0, 3),
                run("run-2", "rainy_0", "production", "last-week", 1, 4),
                run("run-1", "rainy_1", "production", "last-week", 0, 5),
                run("run-1", "rainy_1", "production", "last-week", 1, 6),
                run("run-2", "rainy_1", "production", "last-week", 0, 7),
                run("run-2", "rainy_1", "production", "last-week", 1, 8),
                run("run-1", "scenario-2_0", "staging", "last-week", 0, 9),
                run("run-1", "scenario-2_0", "staging", "last-week", 1, 10),
                run("run-2", "scenario-2_0", "staging", "last-week", 0, 11),
                run("run-2", "scenario-2_0", "staging", "last-week", 1, 12),
            ],
        })
    );
}

#[tokio::test]
async fn creates_an_input_set_from_a_scenarios_managed_inputs() {
    let server = MockServer::start(vec![
        MockResponse::json(200, environment_json("production")),
        MockResponse::json(
            200,
            managed_input_set_json("inpset-rainy-3k9x0q2a", &["monday", "tuesday"]),
        ),
        MockResponse::json(202, json!({ "id": "scenario-1" })),
    ])
    .await;
    let rainy = scenario(
        Some("rainy"),
        "production",
        ScenarioInputType::Input,
        json!(["monday", "tuesday"]),
    );
    support::platform(&server)
        .create_scenario("routing", params(vec![rainy]))
        .await
        .unwrap();

    let requests = server.requests();
    assert_eq!(
        sent(&requests),
        [
            format!("GET {WORKSPACE}/instances/production"),
            format!("POST {EXPERIMENTS}/inputsets"),
            format!("POST {TESTS}"),
        ]
    );
    let input_set = support::body(&requests[1]);
    assert_safe_name_and_id(&input_set["name"], &input_set["id"], "inpset-rainy");
    assert_eq!(
        input_set["description"],
        "Automatically created from scenario test: scenario-1"
    );
    assert_eq!(input_set["maximum_runs"], 20);
    assert_eq!(
        input_set["inputs"],
        json!([{ "id": "monday" }, { "id": "tuesday" }])
    );
    // The runs name the input set that the platform created.
    let batch = support::body(&requests[2]);
    assert_eq!(batch["option_sets"], json!({ "rainy_0": {} }));
    assert_eq!(
        batch["runs"],
        json!([
            run(
                "monday",
                "rainy_0",
                "production",
                "inpset-rainy-3k9x0q2a",
                0,
                1
            ),
            run(
                "tuesday",
                "rainy_0",
                "production",
                "inpset-rainy-3k9x0q2a",
                0,
                2
            ),
        ])
    );
}

#[tokio::test]
async fn uploads_new_data_as_managed_inputs_of_a_new_input_set() {
    let bucket = MockServer::start(vec![MockResponse::new(200), MockResponse::new(200)]).await;
    let server = MockServer::start(vec![
        MockResponse::json(200, environment_json("production")),
        MockResponse::json(200, upload_json(&bucket, "upload-1")),
        MockResponse::json(200, managed_input_json("man-input-rainy-aaaaaaaa")),
        MockResponse::json(200, upload_json(&bucket, "upload-2")),
        MockResponse::json(200, managed_input_json("man-input-rainy-bbbbbbbb")),
        MockResponse::json(
            200,
            managed_input_set_json(
                "inpset-rainy-cccccccc",
                &["man-input-rainy-aaaaaaaa", "man-input-rainy-bbbbbbbb"],
            ),
        ),
        MockResponse::json(202, json!({ "id": "scenario-1" })),
    ])
    .await;
    let rainy = scenario(
        Some("rainy"),
        "production",
        ScenarioInputType::New,
        json!([{ "stops": ["A"] }, { "stops": ["B", "C"] }]),
    );
    support::platform(&server)
        .create_scenario("routing", params(vec![rainy]))
        .await
        .unwrap();

    let requests = server.requests();
    assert_eq!(
        sent(&requests),
        [
            format!("GET {WORKSPACE}/instances/production"),
            format!("POST {WORKSPACE}/runs/uploadurl"),
            format!("POST {WORKSPACE}/inputs"),
            format!("POST {WORKSPACE}/runs/uploadurl"),
            format!("POST {WORKSPACE}/inputs"),
            format!("POST {EXPERIMENTS}/inputsets"),
            format!("POST {TESTS}"),
        ]
    );
    // Each item goes to its own presigned URL, as compact JSON, with no
    // token.
    let uploads = bucket.requests();
    assert_eq!(
        sent(&uploads),
        [
            "PUT /uploads/upload-1?X-Amz-Signature=abc",
            "PUT /uploads/upload-2?X-Amz-Signature=abc",
        ]
    );
    assert_eq!(uploads[0].body, br#"{"stops":["A"]}"#);
    assert_eq!(uploads[1].body, br#"{"stops":["B","C"]}"#);
    for upload in &uploads {
        assert_eq!(upload.header("authorization"), None);
    }
    for (request, upload_id) in [(&requests[2], "upload-1"), (&requests[4], "upload-2")] {
        let managed_input = support::body(request);
        assert_safe_name_and_id(
            &managed_input["name"],
            &managed_input["id"],
            "man-input-rainy",
        );
        assert_eq!(managed_input["upload_id"], upload_id);
        assert_eq!(
            managed_input["description"],
            "Automatically created from scenario test: scenario-1"
        );
        assert_eq!(
            managed_input["format"],
            json!({ "input": { "type": "json" }, "output": { "type": "json" } })
        );
    }
    let input_set = support::body(&requests[5]);
    assert_safe_name_and_id(&input_set["name"], &input_set["id"], "inpset-rainy");
    assert_eq!(input_set["maximum_runs"], 20);
    let inputs: Vec<&str> = input_set["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|input| input["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        inputs,
        ["man-input-rainy-aaaaaaaa", "man-input-rainy-bbbbbbbb"]
    );
    assert_eq!(
        support::body(&requests[6])["runs"],
        json!([
            run(
                "man-input-rainy-aaaaaaaa",
                "rainy_0",
                "production",
                "inpset-rainy-cccccccc",
                0,
                1
            ),
            run(
                "man-input-rainy-bbbbbbbb",
                "rainy_0",
                "production",
                "inpset-rainy-cccccccc",
                0,
                2
            ),
        ])
    );
}

#[tokio::test]
async fn generates_the_id_and_name_when_unset_or_empty() {
    let empty = ScenarioCreateParams {
        scenario_id: Some(String::new()),
        name: Some(String::new()),
        ..ScenarioCreateParams::default()
    };
    for params in [ScenarioCreateParams::default(), empty] {
        let params = ScenarioCreateParams {
            scenarios: vec![scenario(
                None,
                "production",
                ScenarioInputType::InputSet,
                json!("last-week"),
            )],
            ..params
        };
        let server = MockServer::start(vec![
            MockResponse::json(200, environment_json("production")),
            MockResponse::json(200, input_set_json()),
            MockResponse::json(202, json!({ "id": "scenario-x" })),
        ])
        .await;
        let created = support::platform(&server)
            .create_scenario("routing", params)
            .await
            .unwrap();
        // The ID is the one the platform answers with.
        assert_eq!(created.scenario_id, "scenario-x");

        let batch = support::body(&server.requests()[2]);
        let scenario_id = batch["id"].as_str().unwrap();
        let suffix = scenario_id.strip_prefix("scenario-").unwrap();
        assert_eq!(suffix.len(), 8, "{scenario_id}");
        assert_eq!(batch["name"], scenario_id);
        assert_eq!(batch.get("description"), None);
    }
}

#[tokio::test]
async fn rejects_a_create_that_cannot_run_before_any_request() {
    let good = || {
        scenario(
            None,
            "production",
            ScenarioInputType::InputSet,
            json!("last-week"),
        )
    };
    let no_values = Scenario {
        configuration: Some(vec![option("solve.duration", &[])]),
        ..good()
    };
    let no_name = Scenario {
        configuration: Some(vec![option("", &["5s"])]),
        ..good()
    };
    // 10 × 10 × 10 option sets fit; a second scenario's one does not.
    let ten = ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"];
    let thousand = Scenario {
        configuration: Some(vec![
            option("x", &ten),
            option("y", &ten),
            option("z", &ten),
        ]),
        ..good()
    };
    let cases = [
        (Vec::new(), "`scenarios` needs at least one scenario"),
        (
            vec![
                Scenario {
                    scenario_id: Some("scenario-2".to_owned()),
                    ..good()
                },
                good(),
            ],
            "more than one scenario has the ID `scenario-2`",
        ),
        (
            vec![
                good(),
                Scenario {
                    environment_id: String::new(),
                    ..good()
                },
            ],
            "scenario `scenario-2` needs an environment ID (`instance_id`)",
        ),
        (
            vec![scenario(
                None,
                "production",
                ScenarioInputType::InputSet,
                json!(["last-week"]),
            )],
            "the `scenario_input_data` of scenario `scenario-1` must be the ID of an input set \
             for the `scenario_input_type` `input_set`",
        ),
        (
            vec![scenario(
                None,
                "production",
                ScenarioInputType::Input,
                json!("monday"),
            )],
            "the `scenario_input_data` of scenario `scenario-1` must be a list of managed input \
             IDs for the `scenario_input_type` `input`",
        ),
        (
            vec![scenario(
                None,
                "production",
                ScenarioInputType::New,
                json!([]),
            )],
            "the `scenario_input_data` of scenario `scenario-1` must be a list of JSON objects, \
             the inputs for the `scenario_input_type` `new`",
        ),
        (
            vec![scenario(
                None,
                "production",
                ScenarioInputType::New,
                json!(vec![json!({}); 21]),
            )],
            "scenario `scenario-1` has 21 inputs: its input set holds at most 20",
        ),
        (
            vec![scenario(
                None,
                "production",
                ScenarioInputType::Input,
                json!((0..21).map(|i| format!("input-{i}")).collect::<Vec<_>>()),
            )],
            "scenario `scenario-1` has 21 inputs: its input set holds at most 20",
        ),
        (
            vec![no_values],
            "option `solve.duration` of scenario `scenario-1` needs at least one value",
        ),
        (
            vec![no_name],
            "an option of scenario `scenario-1` needs a name",
        ),
        (
            vec![thousand, good()],
            "the scenarios need more than 1000 option sets, the platform's limit: each \
             scenario has one per combination of its option values",
        ),
        (
            vec![Scenario {
                scenario_id: Some("High demand".to_owned()),
                ..good()
            }],
            "the scenario ID `High demand` must be 2 to 128 letters, digits, `.`, \
             `-` and `_`, with a letter or digit at both ends",
        ),
        (
            vec![Scenario {
                configuration: Some(vec![option("solve duration", &["5s"])]),
                ..good()
            }],
            "option `solve duration` of scenario `scenario-1` must start with a letter or \
             digit, followed by letters, digits, `-`, `_`, `:` and `.`",
        ),
    ];
    for (scenarios, message) in cases {
        let server = MockServer::start(Vec::new()).await;
        let err = support::platform(&server)
            .create_scenario("routing", params(scenarios))
            .await
            .unwrap_err();
        let Error::Invalid { reason, .. } = &err else {
            panic!("{err:?}");
        };
        assert_eq!(reason, message);
        assert!(server.requests().is_empty());
    }
    // The test's own ID, name, description and repetitions.
    let test_cases = [
        (
            ScenarioCreateParams {
                scenario_id: Some("a".to_owned()),
                ..params(vec![good()])
            },
            "the scenario test's ID `a` must be 2 to 128 letters, digits, `.`, `-` and `_`, \
             with a letter or digit at both ends",
        ),
        (
            ScenarioCreateParams {
                name: Some("Rain+Sun".to_owned()),
                ..params(vec![good()])
            },
            "`name` must not start with `-` or `'`, or have `+`, `@`, `=`, `\"` or an HTML tag",
        ),
        (
            ScenarioCreateParams {
                scenario_id: Some("ab".to_owned()),
                ..params(vec![good()])
            },
            "`name` must be 3 to 128 characters",
        ),
        (
            ScenarioCreateParams {
                description: Some("x".repeat(257)),
                ..params(vec![good()])
            },
            "`description` must be at most 256 characters",
        ),
        (
            ScenarioCreateParams {
                repetitions: Some(1000),
                ..params(vec![good()])
            },
            "`repetitions` must be at most 999: more would give each input more runs than the \
             platform allows",
        ),
    ];
    for (params, message) in test_cases {
        let server = MockServer::start(Vec::new()).await;
        let err = support::platform(&server)
            .create_scenario("routing", params)
            .await
            .unwrap_err();
        let Error::Invalid { reason, .. } = &err else {
            panic!("{err:?}");
        };
        assert_eq!(reason, message);
        assert!(server.requests().is_empty());
    }
}

#[tokio::test]
async fn rejects_more_than_1000_runs_before_creating_the_test() {
    let server = MockServer::start(vec![
        MockResponse::json(200, environment_json("production")),
        MockResponse::json(200, input_set_json()),
    ])
    .await;
    // 2 inputs × 500 repetitions = 1,000 runs, then 2 option sets double it.
    let rainy = Scenario {
        configuration: Some(vec![option("solve.duration", &["5s", "10s"])]),
        ..scenario(
            None,
            "production",
            ScenarioInputType::InputSet,
            json!("last-week"),
        )
    };
    let params = ScenarioCreateParams {
        repetitions: Some(499),
        ..params(vec![rainy])
    };
    let err = support::platform(&server)
        .create_scenario("routing", params)
        .await
        .unwrap_err();
    let Error::Invalid { reason, .. } = &err else {
        panic!("{err:?}");
    };
    assert_eq!(
        reason,
        "the scenario test would have more than 1000 runs, the most the platform allows: one \
         per scenario, option set, input and repetition"
    );
    // The input set is read, and the test is never created.
    assert_eq!(
        sent(&server.requests()),
        [
            format!("GET {WORKSPACE}/instances/production"),
            format!("GET {EXPERIMENTS}/inputsets/last-week"),
        ]
    );
}

#[tokio::test]
async fn stops_at_a_missing_environment() {
    let server = MockServer::start(vec![MockResponse::json(
        404,
        json!({ "error": "instance not found" }),
    )])
    .await;
    let rainy = scenario(
        None,
        "gone",
        ScenarioInputType::InputSet,
        json!("last-week"),
    );
    let err = support::platform(&server)
        .create_scenario("routing", params(vec![rainy]))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Status { status: 404, .. }), "{err:?}");
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn gets_a_scenario_test_with_every_page_of_its_runs() {
    let server = MockServer::start(vec![
        MockResponse::json(200, test_json("completed")),
        MockResponse::json(
            200,
            json!({ "runs": [run_json("run-a"), run_json("run-b")], "next_page_token": "page-2" }),
        ),
        MockResponse::json(200, json!({ "runs": [run_json("run-c")] })),
    ])
    .await;
    let test = support::platform(&server)
        .get_scenario("routing", "scenario-1", ScenarioGetParams::default())
        .await
        .unwrap();

    let requests = server.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(sent(&requests[..1]), [format!("GET {TEST}")]);
    for request in &requests[1..] {
        assert_eq!(request.method, "GET");
        assert!(request.target.starts_with(&format!("{TEST}/runs?")));
    }
    assert_eq!(
        requests[1].query(),
        support::pairs(&[("pagereturn", "true")])
    );
    assert_eq!(
        requests[2].query(),
        support::pairs(&[("pagereturn", "true"), ("pagetoken", "page-2")])
    );
    assert_eq!(test.scenario_id, "scenario-1");
    assert_eq!(test.status, Some(ExperimentStatus::Completed));
    assert_eq!(test.description.as_deref(), Some("Rain against sun."));
    assert_eq!(test.input_set_id, "");
    assert_eq!(
        test.environment_ids,
        Some(vec!["production".to_owned(), "staging".to_owned()])
    );
    let option_sets = test.option_sets.unwrap();
    assert_eq!(option_sets["rainy_1"]["solve.duration"], "10s");
    assert_eq!(test.number_of_completed_runs, Some(4));
    let summaries = test.grouped_distributional_summaries.unwrap();
    assert_eq!(summaries[0]["group_values"], json!(["rainy", "rainy_0"]));
    let runs: Vec<String> = test.runs.unwrap().into_iter().map(|run| run.id).collect();
    assert_eq!(runs, ["run-a", "run-b", "run-c"]);
}

#[tokio::test]
async fn polls_the_metadata_until_the_scenario_test_completes() {
    let server = MockServer::start(vec![
        MockResponse::json(200, metadata_json("started")),
        MockResponse::json(200, metadata_json("stopping")),
        MockResponse::json(200, metadata_json("completed")),
        MockResponse::json(200, test_json("completed")),
        MockResponse::json(200, json!({ "runs": [run_json("run-a")] })),
    ])
    .await;
    let params = ScenarioGetParams {
        polling: Some(support::fast_polling()),
    };
    let test = support::platform(&server)
        .get_scenario("routing", "scenario-1", params)
        .await
        .unwrap();

    let requests = server.requests();
    assert_eq!(
        sent(&requests),
        [
            format!("GET {TEST}/metadata"),
            format!("GET {TEST}/metadata"),
            format!("GET {TEST}/metadata"),
            format!("GET {TEST}"),
            format!("GET {TEST}/runs?pagereturn=true"),
        ]
    );
    assert_eq!(test.status, Some(ExperimentStatus::Completed));
    assert_eq!(test.runs.unwrap().len(), 1);
}

#[tokio::test]
async fn stops_polling_after_max_tries() {
    let server = MockServer::start(vec![
        MockResponse::json(200, metadata_json("started")),
        MockResponse::json(200, metadata_json("started")),
    ])
    .await;
    let params = ScenarioGetParams {
        polling: Some(PollingOptions {
            max_tries: Some(2),
            ..support::fast_polling()
        }),
    };
    let err = support::platform(&server)
        .get_scenario("routing", "scenario-1", params)
        .await
        .unwrap_err();
    let Error::Wait { reason, .. } = &err else {
        panic!("{err:?}");
    };
    assert_eq!(
        reason,
        "no terminal state after 2 tries, the `max_tries` limit"
    );
    assert_eq!(server.requests().len(), 2);
}

#[tokio::test]
async fn lists_scenario_tests_across_pages() {
    let mut first = metadata_json("completed");
    first.as_object_mut().unwrap().remove("app_id");
    first["option_sets"] = json!({ "rainy_0": {} });
    first["input_set_id"] = json!("inputs-1");
    first["instance_ids"] = json!(["production"]);
    let mut second = metadata_json("unknown");
    second["id"] = json!("scenario-2");
    let server = MockServer::start(vec![
        MockResponse::json(
            200,
            json!({ "items": [first], "next_page_token": "page-2" }),
        ),
        MockResponse::json(200, json!({ "items": [second] })),
    ])
    .await;
    let tests = support::platform(&server)
        .list_scenarios("routing")
        .await
        .unwrap();
    let ids: Vec<&str> = tests.iter().map(|test| test.scenario_id.as_str()).collect();
    assert_eq!(ids, ["scenario-1", "scenario-2"]);
    assert_eq!(tests[0].workspace_id, None);
    assert!(tests[0].option_sets.as_ref().unwrap()["rainy_0"].is_empty());
    assert_eq!(tests[0].experiment_type.as_deref(), Some("scenario"));
    assert_eq!(tests[0].input_set_id.as_deref(), Some("inputs-1"));
    assert_eq!(
        tests[0].environment_ids.as_deref(),
        Some(&["production".to_owned()][..])
    );
    assert_eq!(tests[1].status, Some(ExperimentStatus::Unknown));
    assert_eq!(tests[1].input_set_id, None);
    let written = serde_json::to_value(&tests[0]).unwrap();
    assert_eq!(written["type"], "scenario");
    assert_eq!(written["instance_ids"], json!(["production"]));

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert_eq!(request.method, "GET");
        assert!(request.target.starts_with(&format!("{TESTS}?")));
    }
    assert_eq!(
        requests[0].query(),
        support::pairs(&[("type", "scenario"), ("pagereturn", "true")])
    );
    assert_eq!(
        requests[1].query(),
        support::pairs(&[
            ("type", "scenario"),
            ("pagereturn", "true"),
            ("pagetoken", "page-2"),
        ])
    );
}

#[tokio::test]
async fn gets_the_metadata_of_a_scenario_test() {
    let server = MockServer::start(vec![MockResponse::json(
        200,
        metadata_json("delete-failed"),
    )])
    .await;
    let metadata = support::platform(&server)
        .get_scenario_metadata("routing", "scenario-1")
        .await
        .unwrap();

    assert_eq!(sent(&server.requests()), [format!("GET {TEST}/metadata")]);
    assert_eq!(metadata.scenario_id, "scenario-1");
    assert_eq!(metadata.workspace_id.as_deref(), Some("routing"));
    assert_eq!(metadata.status, Some(ExperimentStatus::DeleteFailed));
    assert_eq!(metadata.number_of_requested_runs, Some(4));
    assert_eq!(metadata.number_of_completed_runs, None);
}

#[tokio::test]
async fn updates_only_the_fields_set() {
    let cases = [
        (
            ScenarioUpdateParams {
                description: Some("Rain against sun, again.".to_owned()),
                ..ScenarioUpdateParams::default()
            },
            json!({ "description": "Rain against sun, again." }),
        ),
        (
            ScenarioUpdateParams {
                name: Some("Weather".to_owned()),
                description: Some(String::new()),
            },
            json!({ "name": "Weather", "description": "" }),
        ),
    ];
    for (params, expected) in cases {
        let server = MockServer::start(vec![MockResponse::json(200, updated_json())]).await;
        let information = support::platform(&server)
            .update_scenario("routing", "scenario-1", params)
            .await
            .unwrap();

        let requests = server.requests();
        assert_eq!(sent(&requests), [format!("PATCH {TEST}")]);
        assert_eq!(support::body(&requests[0]), expected);
        assert_eq!(information.scenario_id, "scenario-1");
        assert_eq!(
            information.description.as_deref(),
            Some("Rain against sun, again.")
        );
        assert_eq!(information.updated_at, "2026-10-20T15:00:00Z");
        assert_eq!(information.status, None);
    }
}

#[tokio::test]
async fn rejects_an_update_that_sets_nothing() {
    let server = MockServer::start(Vec::new()).await;
    let err = support::platform(&server)
        .update_scenario("routing", "scenario-1", ScenarioUpdateParams::default())
        .await
        .unwrap_err();
    let Error::Invalid { reason, .. } = &err else {
        panic!("{err:?}");
    };
    assert_eq!(reason, "nothing to update: set `name` or `description`");
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn polls_a_delete_until_the_scenario_test_is_gone() {
    let server = MockServer::start(vec![
        MockResponse::new(204),
        MockResponse::json(200, test_json("started")),
        MockResponse::new(404).body("batch experiment not found"),
    ])
    .await;
    let params = ScenarioDeleteParams {
        polling: Some(support::fast_polling()),
    };
    support::platform(&server)
        .delete_scenario("routing", "scenario-1", params)
        .await
        .unwrap();
    let calls: Vec<_> = server
        .requests()
        .into_iter()
        .map(|r| format!("{} {}", r.method, r.target))
        .collect();
    // The test's own GET: its metadata is a 400 for a test that is gone.
    assert_eq!(
        calls,
        [
            format!("DELETE {TEST}"),
            format!("GET {TEST}"),
            format!("GET {TEST}"),
        ]
    );
}

#[tokio::test]
async fn ends_the_polling_of_a_failed_scenario_delete_with_an_error() {
    let server = MockServer::start(vec![
        MockResponse::new(204),
        MockResponse::json(200, test_json("delete-failed")),
    ])
    .await;
    let params = ScenarioDeleteParams {
        polling: Some(support::fast_polling()),
    };
    let err = support::platform(&server)
        .delete_scenario("routing", "scenario-1", params)
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "stopped waiting: the platform could not delete the scenario test"
    );
}

#[tokio::test]
async fn deletes_a_scenario_test() {
    let server = MockServer::start(vec![MockResponse::new(204)]).await;
    support::platform(&server)
        .delete_scenario("routing", "scenario-1", Default::default())
        .await
        .unwrap();
    assert_eq!(sent(&server.requests()), [format!("DELETE {TEST}")]);
}

/// Arguments of the create tool, with one scenario on the input set
/// `last-week`.
fn create_args() -> Value {
    json!({
        "workspace_id": "routing",
        "scenario_id": "scenario-1",
        "repetitions": 0,
        "scenarios": [{
            "scenario_id": "rainy",
            "instance_id": "production",
            "scenario_input": {
                "scenario_input_type": "input_set",
                "scenario_input_data": "last-week",
            },
            "configuration": [{ "name": "solve.duration", "values": ["5s", "10s"] }],
        }],
    })
}

#[tokio::test]
async fn serves_the_create_tool() {
    let (result, requests) = support::serve_tool(
        vec![
            MockResponse::json(200, environment_json("production")),
            MockResponse::json(200, input_set_json()),
            MockResponse::json(202, json!({ "id": "scenario-1" })),
        ],
        "decision_platform_scenario_create",
        create_args(),
    )
    .await;
    assert_eq!(requests.len(), 3);
    let batch = support::body(&requests[2]);
    assert_eq!(batch["type"], "scenario");
    assert_eq!(
        batch["option_sets"],
        json!({ "rainy_0": { "solve.duration": "5s" }, "rainy_1": { "solve.duration": "10s" } })
    );
    assert_eq!(batch["runs"].as_array().unwrap().len(), 4);
    assert_eq!(result.unwrap(), json!({ "scenario_id": "scenario-1" }));
}

#[tokio::test]
async fn rejects_bad_arguments_in_the_create_tool() {
    let mut no_scenarios = create_args();
    no_scenarios.as_object_mut().unwrap().remove("scenarios");
    let mut unknown_type = create_args();
    unknown_type["scenarios"][0]["scenario_input"]["scenario_input_type"] = json!("run");
    let mut unknown_field = create_args();
    unknown_field["scenarios"][0]["environment_id"] = json!("production");
    let mut content_type = create_args();
    content_type["content_type"] = json!("multi-file");
    let cases = [
        (no_scenarios, "missing field `scenarios`"),
        (unknown_type, "unknown variant `run`"),
        (unknown_field, "unknown field `scenarios[0].environment_id`"),
        (content_type, "unknown field `content_type`"),
    ];
    for (args, message) in cases {
        let (result, requests) =
            support::serve_tool(Vec::new(), "decision_platform_scenario_create", args).await;
        let err = result.unwrap_err();
        assert!(
            matches!(&err, ToolError::InvalidArguments(m) if m.contains(message)),
            "{err}"
        );
        assert!(requests.is_empty());
    }
}

#[tokio::test]
async fn serves_the_get_tool_with_default_polling() {
    let (result, requests) = support::serve_tool(
        vec![
            MockResponse::json(200, metadata_json("completed")),
            MockResponse::json(200, test_json("completed")),
            MockResponse::json(200, json!({ "runs": [run_json("run-a")] })),
        ],
        "decision_platform_scenario_get",
        json!({
            "workspace_id": "routing",
            "scenario_id": "scenario-1",
            "polling": { "initial_delay": 0, "jitter": 0 },
        }),
    )
    .await;
    assert_eq!(
        sent(&requests),
        [
            format!("GET {TEST}/metadata"),
            format!("GET {TEST}"),
            format!("GET {TEST}/runs?pagereturn=true"),
        ]
    );
    let result = result.unwrap();
    assert_eq!(result["id"], "scenario-1");
    assert_eq!(result["instance_ids"], json!(["production", "staging"]));
    assert_eq!(result["runs"][0]["id"], "run-a");
    assert_eq!(result["type"], "scenario");
}

#[tokio::test]
async fn serves_the_list_tool() {
    let (result, requests) = support::serve_tool(
        vec![MockResponse::json(
            200,
            json!({ "items": [metadata_json("completed")] }),
        )],
        "decision_platform_scenario_list",
        json!({ "workspace_id": "routing" }),
    )
    .await;
    assert_eq!(
        sent(&requests),
        [format!("GET {TESTS}?type=scenario&pagereturn=true")]
    );
    assert_eq!(result.unwrap()[0]["id"], "scenario-1");
}

#[tokio::test]
async fn serves_the_metadata_get_tool() {
    let (result, requests) = support::serve_tool(
        vec![MockResponse::json(200, metadata_json("started"))],
        "decision_platform_scenario_metadata_get",
        json!({ "workspace_id": "routing", "scenario_id": "scenario-1" }),
    )
    .await;
    assert_eq!(sent(&requests), [format!("GET {TEST}/metadata")]);
    let result = result.unwrap();
    assert_eq!(result["status"], "started");
    assert_eq!(result["app_id"], "routing");
}

#[tokio::test]
async fn serves_the_update_tool() {
    let (result, requests) = support::serve_tool(
        vec![MockResponse::json(200, updated_json())],
        "decision_platform_scenario_update",
        json!({
            "workspace_id": "routing",
            "scenario_id": "scenario-1",
            "name": "Weather",
        }),
    )
    .await;
    assert_eq!(sent(&requests), [format!("PATCH {TEST}")]);
    assert_eq!(support::body(&requests[0]), json!({ "name": "Weather" }));
    assert_eq!(result.unwrap()["updated_at"], "2026-10-20T15:00:00Z");
}

#[tokio::test]
async fn serves_the_delete_tool() {
    let (result, requests) = support::serve_tool(
        vec![MockResponse::new(204)],
        "decision_platform_scenario_delete",
        json!({ "workspace_id": "routing", "scenario_id": "scenario-1" }),
    )
    .await;
    assert_eq!(sent(&requests), [format!("DELETE {TEST}")]);
    assert_eq!(result.unwrap(), Value::Null);
}
