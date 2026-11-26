//! Tests for the platform ensemble definition operations and their tools,
//! against the mock server.

mod mock_server;
mod support;

use std::collections::BTreeMap;

use fico_sdk::Error;
use fico_sdk::decision::platform::{EnsembleCreateParams, EnsembleUpdateParams};
use fico_sdk::decision::{
    EvaluationRule, RuleObjective, RuleTolerance, RuleToleranceType, RunGroup,
};
use fico_sdk::tool::ToolError;
use mock_server::{MockResponse, MockServer};
use serde_json::{Value, json};

/// An ensemble definition as the platform returns it. Run groups always
/// carry `repetitions` and leave out `options` when they have none, and the
/// timestamps are Go's `RFC3339Nano`, which drops trailing zeros.
fn ensemble_json() -> Value {
    json!({
        "id": "fast-or-cheap",
        "application_id": "routing",
        "name": "Fast or cheap",
        "description": "Picks the cheapest plan, then the fastest.",
        "run_groups": [
            {
                "id": "quick",
                "instance_id": "quick-env",
                "options": { "solve.duration": "5s" },
                "repetitions": 0,
            },
            { "id": "thorough", "instance_id": "thorough-env", "repetitions": 2 },
        ],
        "rules": [
            {
                "id": "cost",
                "statistics_path": "$.result.value",
                "tolerance": { "value": 0.05, "type": "relative" },
                "objective": "minimize",
                "index": 0,
            },
            {
                "id": "duration",
                "statistics_path": "$.result.duration",
                "tolerance": { "value": 1.5, "type": "absolute" },
                "objective": "minimize",
                "index": 1,
            },
        ],
        "created_at": "2026-10-01T12:00:00.123456Z",
        "updated_at": "2026-10-01T12:00:00.123456Z",
    })
}

fn run_groups() -> Vec<RunGroup> {
    vec![
        RunGroup {
            group_id: "quick".to_owned(),
            environment_id: "quick-env".to_owned(),
            options: Some(BTreeMap::from([(
                "solve.duration".to_owned(),
                "5s".to_owned(),
            )])),
            repetitions: None,
        },
        RunGroup {
            group_id: "thorough".to_owned(),
            environment_id: "thorough-env".to_owned(),
            options: None,
            repetitions: Some(2),
        },
    ]
}

fn rules() -> Vec<EvaluationRule> {
    vec![EvaluationRule {
        rule_id: "cost".to_owned(),
        metrics_path: "$.result.value".to_owned(),
        objective: RuleObjective::Minimize,
        tolerance: RuleTolerance {
            value: 0.05,
            tolerance_type: RuleToleranceType::Relative,
        },
        index: 0,
    }]
}

/// The run groups and rules of `run_groups()` and `rules()`, as the
/// request body carries them.
fn wire_groups_and_rules() -> (Value, Value) {
    let groups = json!([
        {
            "id": "quick",
            "instance_id": "quick-env",
            "options": { "solve.duration": "5s" },
        },
        { "id": "thorough", "instance_id": "thorough-env", "repetitions": 2 },
    ]);
    let rules = json!([{
        "id": "cost",
        "statistics_path": "$.result.value",
        "objective": "minimize",
        "tolerance": { "value": 0.05, "type": "relative" },
        "index": 0,
    }]);
    (groups, rules)
}

#[tokio::test]
async fn creates_an_ensemble() {
    let server = MockServer::start(vec![MockResponse::json(200, ensemble_json())]).await;
    let params = EnsembleCreateParams {
        ensemble_id: Some("fast-or-cheap".to_owned()),
        name: Some("Fast or cheap".to_owned()),
        description: Some("Picks the cheapest plan, then the fastest.".to_owned()),
        run_groups: run_groups(),
        rules: rules(),
    };
    let ensemble = support::platform(&server)
        .create_ensemble("routing", params)
        .await
        .unwrap();

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].target, "/v1/applications/routing/ensembles");
    let (groups, rules) = wire_groups_and_rules();
    assert_eq!(
        support::body(&requests[0]),
        json!({
            "id": "fast-or-cheap",
            "run_groups": groups,
            "rules": rules,
            "name": "Fast or cheap",
            "description": "Picks the cheapest plan, then the fastest.",
        })
    );
    assert_eq!(ensemble.ensemble_id, "fast-or-cheap");
    assert_eq!(ensemble.workspace_id, "routing");
    assert_eq!(ensemble.run_groups[0].group_id, "quick");
    assert_eq!(ensemble.run_groups[0].environment_id, "quick-env");
    assert_eq!(ensemble.run_groups[1].repetitions, Some(2));
    let rule = &ensemble.rules[0];
    assert_eq!(rule.rule_id, "cost");
    assert_eq!(rule.metrics_path, "$.result.value");
    assert_eq!(rule.objective, RuleObjective::Minimize);
    assert_eq!(rule.tolerance.tolerance_type, RuleToleranceType::Relative);
    assert_eq!(rule.tolerance.value, 0.05);
}

#[tokio::test]
async fn names_a_new_ensemble_after_its_id() {
    let server = MockServer::start(vec![MockResponse::json(200, ensemble_json())]).await;
    let params = EnsembleCreateParams {
        ensemble_id: Some("fast-or-cheap".to_owned()),
        run_groups: run_groups(),
        rules: rules(),
        ..Default::default()
    };
    support::platform(&server)
        .create_ensemble("routing", params)
        .await
        .unwrap();
    let body = support::body(&server.requests()[0]);
    assert_eq!(body["name"], "fast-or-cheap");
    assert_eq!(body["description"], "fast-or-cheap");
}

#[tokio::test]
async fn describes_a_new_ensemble_with_its_name() {
    let server = MockServer::start(vec![MockResponse::json(200, ensemble_json())]).await;
    let params = EnsembleCreateParams {
        ensemble_id: Some("fast-or-cheap".to_owned()),
        name: Some("Fast or cheap".to_owned()),
        description: Some(String::new()),
        run_groups: run_groups(),
        rules: rules(),
    };
    support::platform(&server)
        .create_ensemble("routing", params)
        .await
        .unwrap();
    let body = support::body(&server.requests()[0]);
    assert_eq!(body["name"], "Fast or cheap");
    assert_eq!(body["description"], "Fast or cheap");
}

#[tokio::test]
async fn generates_an_id_when_none_is_set() {
    let server = MockServer::start(vec![MockResponse::json(200, ensemble_json())]).await;
    let params = EnsembleCreateParams {
        ensemble_id: Some(String::new()),
        name: Some(String::new()),
        run_groups: run_groups(),
        rules: rules(),
        ..Default::default()
    };
    support::platform(&server)
        .create_ensemble("routing", params)
        .await
        .unwrap();
    let body = support::body(&server.requests()[0]);
    let ensemble_id = body["id"].as_str().unwrap();
    let suffix = ensemble_id.strip_prefix("ensemble-").unwrap();
    assert_eq!(suffix.len(), 8, "{ensemble_id}");
    assert!(
        suffix
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()),
        "{ensemble_id}"
    );
    assert_eq!(body["name"], ensemble_id);
    assert_eq!(body["description"], ensemble_id);
}

#[tokio::test]
async fn needs_run_groups_and_rules() {
    let server = MockServer::start(Vec::new()).await;
    let platform = support::platform(&server);
    let cases = [
        (
            EnsembleCreateParams {
                rules: rules(),
                ..Default::default()
            },
            "invalid argument: `run_groups` needs at least one run group",
        ),
        (
            EnsembleCreateParams {
                run_groups: run_groups(),
                ..Default::default()
            },
            "invalid argument: `rules` needs at least one rule",
        ),
    ];
    for (params, message) in cases {
        let err = platform
            .create_ensemble("routing", params)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Invalid { .. }), "{err}");
        assert_eq!(err.to_string(), message);
    }
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn rejects_bad_run_groups_and_rules_before_any_request() {
    let server = MockServer::start(Vec::new()).await;
    let platform = support::platform(&server);
    let mut no_group_id = run_groups();
    no_group_id[1].group_id = String::new();
    let mut no_environment = run_groups();
    no_environment[0].environment_id = String::new();
    let mut no_rule_id = rules();
    no_rule_id[0].rule_id = String::new();
    let mut no_path = rules();
    no_path[0].metrics_path = String::new();
    let mut unknown_objective = rules();
    unknown_objective[0].objective = RuleObjective::Unknown;
    let mut unknown_tolerance = rules();
    unknown_tolerance[0].tolerance.tolerance_type = RuleToleranceType::Unknown;
    let mut same_id = rules();
    same_id.push(EvaluationRule {
        index: 1,
        ..same_id[0].clone()
    });
    let cases = [
        (
            no_group_id,
            rules(),
            "`run_groups[1]` needs a `group_id` (`id`)",
        ),
        (
            no_environment,
            rules(),
            "`run_groups[0]` needs an `environment_id` (`instance_id`)",
        ),
        (
            run_groups(),
            no_rule_id,
            "`rules[0]` needs a `rule_id` (`id`)",
        ),
        (
            run_groups(),
            no_path,
            "`rules[0]` needs a `metrics_path` (`statistics_path`)",
        ),
        (
            run_groups(),
            unknown_objective,
            "`rules[0].objective` must be `maximize` or `minimize`",
        ),
        (
            run_groups(),
            unknown_tolerance,
            "`rules[0].tolerance.type` must be `absolute` or `relative`",
        ),
        (
            run_groups(),
            same_id,
            "more than one rule has the ID `cost`",
        ),
    ];
    for (run_groups, rules, message) in cases {
        let params = EnsembleCreateParams {
            run_groups,
            rules,
            ..Default::default()
        };
        let err = platform
            .create_ensemble("routing", params)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Invalid { .. }), "{err}");
        assert_eq!(err.to_string(), format!("invalid argument: {message}"));
    }
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn gets_an_ensemble() {
    let server = MockServer::start(vec![MockResponse::json(200, ensemble_json())]).await;
    let ensemble = support::platform(&server)
        .get_ensemble("routing", "fast-or-cheap")
        .await
        .unwrap();
    let request = &server.requests()[0];
    assert_eq!(request.method, "GET");
    assert_eq!(
        request.target,
        "/v1/applications/routing/ensembles/fast-or-cheap"
    );
    assert_eq!(ensemble.name, "Fast or cheap");
    assert_eq!(ensemble.run_groups[0].repetitions, Some(0));
    assert_eq!(ensemble.run_groups[1].options, None);
    assert_eq!(ensemble.rules[1].index, 1);
    assert_eq!(ensemble.created_at, "2026-10-01T12:00:00.123456Z");
    // Emitted JSON uses the API names.
    assert_eq!(serde_json::to_value(&ensemble).unwrap(), ensemble_json());
}

#[tokio::test]
async fn reads_new_rule_values_as_unknown() {
    let mut response = ensemble_json();
    response["rules"][0]["objective"] = json!("balance");
    response["rules"][0]["tolerance"]["type"] = json!("percentile");
    let server = MockServer::start(vec![MockResponse::json(200, response)]).await;
    let ensemble = support::platform(&server)
        .get_ensemble("routing", "fast-or-cheap")
        .await
        .unwrap();
    let rule = &ensemble.rules[0];
    assert_eq!(rule.objective, RuleObjective::Unknown);
    assert_eq!(rule.tolerance.tolerance_type, RuleToleranceType::Unknown);
}

#[tokio::test]
async fn lists_ensembles_across_pages() {
    let mut second = ensemble_json();
    second["id"] = json!("cheap-only");
    let server = MockServer::start(vec![
        MockResponse::json(
            200,
            json!({ "items": [ensemble_json()], "next_page_token": "page-2" }),
        ),
        MockResponse::json(200, json!({ "items": [second] })),
    ])
    .await;
    let ensembles = support::platform(&server)
        .list_ensembles("routing")
        .await
        .unwrap();
    let ensemble_ids: Vec<&str> = ensembles.iter().map(|e| e.ensemble_id.as_str()).collect();
    assert_eq!(ensemble_ids, ["fast-or-cheap", "cheap-only"]);

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert_eq!(request.method, "GET");
        assert!(
            request
                .target
                .starts_with("/v1/applications/routing/ensembles?")
        );
    }
    assert_eq!(
        requests[0].query(),
        support::pairs(&[("pagereturn", "true")])
    );
    assert_eq!(
        requests[1].query(),
        support::pairs(&[("pagereturn", "true"), ("pagetoken", "page-2")])
    );
}

#[tokio::test]
async fn updates_only_the_fields_that_are_set() {
    let mut updated = ensemble_json();
    updated["description"] = json!("Cheapest plan wins.");
    let server = MockServer::start(vec![MockResponse::json(200, updated)]).await;
    let params = EnsembleUpdateParams {
        description: Some("Cheapest plan wins.".to_owned()),
        ..Default::default()
    };
    let ensemble = support::platform(&server)
        .update_ensemble("routing", "fast-or-cheap", params)
        .await
        .unwrap();

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "PATCH");
    assert_eq!(
        requests[0].target,
        "/v1/applications/routing/ensembles/fast-or-cheap"
    );
    assert_eq!(
        support::body(&requests[0]),
        json!({ "description": "Cheapest plan wins." })
    );
    assert_eq!(ensemble.description, "Cheapest plan wins.");
    assert_eq!(ensemble.name, "Fast or cheap");
}

#[tokio::test]
async fn sends_an_empty_name_on_update() {
    let server = MockServer::start(vec![MockResponse::json(200, ensemble_json())]).await;
    let params = EnsembleUpdateParams {
        name: Some(String::new()),
        description: Some("Cheapest plan wins.".to_owned()),
    };
    support::platform(&server)
        .update_ensemble("routing", "fast-or-cheap", params)
        .await
        .unwrap();
    assert_eq!(
        support::body(&server.requests()[0]),
        json!({ "name": "", "description": "Cheapest plan wins." })
    );
}

#[tokio::test]
async fn needs_a_change_to_update() {
    let server = MockServer::start(Vec::new()).await;
    let err = support::platform(&server)
        .update_ensemble("routing", "fast-or-cheap", EnsembleUpdateParams::default())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Invalid { .. }), "{err}");
    assert_eq!(
        err.to_string(),
        "invalid argument: an update needs `name` or `description`"
    );
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn deletes_an_ensemble() {
    let server = MockServer::start(vec![MockResponse::new(204)]).await;
    support::platform(&server)
        .delete_ensemble("routing", "fast-or-cheap")
        .await
        .unwrap();
    let request = &server.requests()[0];
    assert_eq!(request.method, "DELETE");
    assert_eq!(
        request.target,
        "/v1/applications/routing/ensembles/fast-or-cheap"
    );
}

#[tokio::test]
async fn encodes_ensemble_ids_in_paths() {
    let server = MockServer::start(vec![MockResponse::json(200, ensemble_json())]).await;
    let platform = support::platform(&server);
    platform
        .get_ensemble("my workspace", "fast?cheap")
        .await
        .unwrap();
    assert_eq!(
        server.requests()[0].target,
        "/v1/applications/my%20workspace/ensembles/fast%3Fcheap"
    );
    for (workspace_id, ensemble_id) in [
        ("routing", ".."),
        ("..", "fast-or-cheap"),
        ("routing", ""),
        ("routing", "fast/cheap"),
    ] {
        let err = platform
            .delete_ensemble(workspace_id, ensemble_id)
            .await
            .unwrap_err();
        assert!(
            matches!(err, Error::Invalid { .. }),
            "{ensemble_id:?}: {err}"
        );
    }
    assert_eq!(server.requests().len(), 1);
}

/// Arguments of `decision_platform_ensemble_create`, with the run groups and
/// rules of `wire_groups_and_rules()`.
fn create_args() -> Value {
    let (groups, rules) = wire_groups_and_rules();
    json!({
        "workspace_id": "routing",
        "ensemble_id": "fast-or-cheap",
        "run_groups": groups,
        "rules": rules,
    })
}

#[tokio::test]
async fn serves_the_create_tool() {
    let (result, requests) = support::serve_tool(
        vec![MockResponse::json(200, ensemble_json())],
        "decision_platform_ensemble_create",
        create_args(),
    )
    .await;
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].target, "/v1/applications/routing/ensembles");
    let (groups, rules) = wire_groups_and_rules();
    assert_eq!(
        support::body(&requests[0]),
        json!({
            "id": "fast-or-cheap",
            "run_groups": groups,
            "rules": rules,
            "name": "fast-or-cheap",
            "description": "fast-or-cheap",
        })
    );
    assert_eq!(result.unwrap()["application_id"], "routing");
}

#[test]
fn requires_run_groups_and_rules_in_the_create_tool() {
    let tool = support::tool("decision_platform_ensemble_create");
    assert_eq!(
        tool.input_schema()["required"],
        json!(["workspace_id", "run_groups", "rules"])
    );
}

#[tokio::test]
async fn rejects_bad_create_tool_arguments() {
    let mut without_rules = create_args();
    without_rules.as_object_mut().unwrap().remove("rules");
    let mut unknown_objective = create_args();
    unknown_objective["rules"][0]["objective"] = json!("balance");
    let mut numeric_option = create_args();
    numeric_option["run_groups"][0]["options"] = json!({ "solve.duration": 5 });
    let mut without_environment = create_args();
    without_environment["run_groups"][1]
        .as_object_mut()
        .unwrap()
        .remove("instance_id");
    for args in [
        without_rules,
        unknown_objective,
        numeric_option,
        without_environment,
    ] {
        let (result, requests) =
            support::serve_tool(Vec::new(), "decision_platform_ensemble_create", args).await;
        assert!(
            matches!(result, Err(ToolError::InvalidArguments(_))),
            "{result:?}"
        );
        assert!(requests.is_empty());
    }
}

#[tokio::test]
async fn serves_the_get_tool() {
    let (result, requests) = support::serve_tool(
        vec![MockResponse::json(200, ensemble_json())],
        "decision_platform_ensemble_get",
        json!({ "workspace_id": "routing", "ensemble_id": "fast-or-cheap" }),
    )
    .await;
    assert_eq!(
        requests[0].target,
        "/v1/applications/routing/ensembles/fast-or-cheap"
    );
    assert_eq!(result.unwrap()["rules"][0]["tolerance"]["type"], "relative");
}

#[tokio::test]
async fn serves_the_list_tool() {
    let (result, requests) = support::serve_tool(
        vec![MockResponse::json(
            200,
            json!({ "items": [ensemble_json()] }),
        )],
        "decision_platform_ensemble_list",
        json!({ "workspace_id": "routing" }),
    )
    .await;
    assert_eq!(
        requests[0].target,
        "/v1/applications/routing/ensembles?pagereturn=true"
    );
    assert_eq!(result.unwrap()[0]["id"], "fast-or-cheap");
}

#[tokio::test]
async fn serves_the_update_tool() {
    let mut updated = ensemble_json();
    updated["name"] = json!("Cheapest first");
    let (result, requests) = support::serve_tool(
        vec![MockResponse::json(200, updated)],
        "decision_platform_ensemble_update",
        json!({
            "workspace_id": "routing",
            "ensemble_id": "fast-or-cheap",
            "name": "Cheapest first",
        }),
    )
    .await;
    assert_eq!(requests[0].method, "PATCH");
    assert_eq!(
        requests[0].target,
        "/v1/applications/routing/ensembles/fast-or-cheap"
    );
    assert_eq!(
        support::body(&requests[0]),
        json!({ "name": "Cheapest first" })
    );
    assert_eq!(result.unwrap()["name"], "Cheapest first");
}

#[tokio::test]
async fn serves_the_delete_tool() {
    let (result, requests) = support::serve_tool(
        vec![MockResponse::new(204)],
        "decision_platform_ensemble_delete",
        json!({ "workspace_id": "routing", "ensemble_id": "fast-or-cheap" }),
    )
    .await;
    assert_eq!(requests[0].method, "DELETE");
    assert_eq!(
        requests[0].target,
        "/v1/applications/routing/ensembles/fast-or-cheap"
    );
    assert_eq!(result.unwrap(), Value::Null);
}
