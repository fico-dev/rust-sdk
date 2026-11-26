//! Tests for the platform environment operations and their tools, against
//! the mock server.

mod mock_server;
mod support;

use std::collections::BTreeMap;

use fico_sdk::Error;
use fico_sdk::decision::platform::{EnvironmentCreateParams, EnvironmentUpdateParams};
use fico_sdk::decision::{
    ContentFormat, EnvironmentConfiguration, Format, FormatInput, FormatOutput, RunQueuing,
};
use fico_sdk::tool::{Context, ToolError};
use mock_server::{MockResponse, MockServer};
use serde_json::{Value, json};

/// An environment as the platform returns it, with a field the SDK does not
/// model (`working_directory`).
fn environment_json() -> Value {
    json!({
        "id": "prod",
        "application_id": "routing",
        "version_id": "v1",
        "name": "Production",
        "description": "Plans the daily routes.",
        "configuration": {
            "execution_class": "6c9500mb870s",
            "format": { "input": { "type": "json" } },
            "options": { "solve.duration": "10s" },
            "secrets_collection_id": "routing-secrets",
            "queuing": { "priority": 6, "disabled": false },
        },
        "locked": false,
        "created_at": "2026-10-01T12:00:00Z",
        "updated_at": "2026-10-01T12:00:00Z",
        "working_directory": "src",
    })
}

/// An environment that runs through an integration.
fn integration_json() -> Value {
    let mut environment = environment_json();
    environment["configuration"] = json!({
        "execution_class": "integration",
        "integration_id": "dbx",
        "queuing": { "priority": 6, "disabled": false },
    });
    environment
}

fn queuing(priority: Option<u32>, disabled: Option<bool>) -> Option<RunQueuing> {
    Some(RunQueuing { priority, disabled })
}

fn json_format() -> Option<Format> {
    Some(Format {
        format_input: FormatInput {
            input_type: ContentFormat::Json,
        },
        format_output: None,
    })
}

fn create_params(environment_id: &str) -> EnvironmentCreateParams {
    EnvironmentCreateParams {
        environment_id: Some(environment_id.to_owned()),
        ..Default::default()
    }
}

#[tokio::test]
async fn creates_an_environment() {
    let server = MockServer::start(vec![MockResponse::json(200, environment_json())]).await;
    let configuration = EnvironmentConfiguration {
        execution_class: Some("6c9500mb870s".to_owned()),
        format: json_format(),
        options: Some(BTreeMap::from([(
            "solve.duration".to_owned(),
            "10s".to_owned(),
        )])),
        secrets_collection_id: Some("routing-secrets".to_owned()),
        queuing: queuing(Some(6), Some(false)),
        integration_id: None,
    };
    let params = EnvironmentCreateParams {
        environment_id: Some("prod".to_owned()),
        name: Some("Production".to_owned()),
        description: Some("Plans the daily routes.".to_owned()),
        configuration: Some(configuration.clone()),
        exist_ok: false,
    };
    let environment = support::platform(&server)
        .create_environment("routing", "v1", params)
        .await
        .unwrap();

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].target, "/v1/applications/routing/instances");
    assert_eq!(
        support::body(&requests[0]),
        json!({
            "id": "prod",
            "name": "Production",
            "version_id": "v1",
            "description": "Plans the daily routes.",
            "configuration": environment_json()["configuration"],
        })
    );
    assert_eq!(environment.environment_id, "prod");
    assert_eq!(environment.workspace_id, "routing");
    assert_eq!(environment.build_id, "v1");
    assert_eq!(environment.configuration, configuration);
    assert!(!environment.locked);
}

#[tokio::test]
async fn names_a_new_environment_after_its_id() {
    let server = MockServer::start(vec![MockResponse::json(200, environment_json())]).await;
    support::platform(&server)
        .create_environment("routing", "v1", create_params("prod"))
        .await
        .unwrap();
    let request = &server.requests()[0];
    assert_eq!(
        support::body(request),
        json!({ "id": "prod", "name": "prod", "version_id": "v1" })
    );
}

#[tokio::test]
async fn generates_an_id_when_none_is_set() {
    let server = MockServer::start(vec![MockResponse::json(200, environment_json())]).await;
    support::platform(&server)
        .create_environment("routing", "v1", create_params(""))
        .await
        .unwrap();
    let body = support::body(&server.requests()[0]);
    let environment_id = body["id"].as_str().unwrap();
    let suffix = environment_id.strip_prefix("environment-").unwrap();
    assert_eq!(suffix.len(), 8, "{environment_id}");
    assert!(
        suffix
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()),
        "{environment_id}"
    );
    assert_eq!(body["name"], environment_id);
}

#[tokio::test]
async fn returns_the_existing_environment_with_exist_ok() {
    let server = MockServer::start(vec![MockResponse::json(200, environment_json())]).await;
    let params = EnvironmentCreateParams {
        exist_ok: true,
        ..create_params("prod")
    };
    let environment = support::platform(&server)
        .create_environment("routing", "v1", params)
        .await
        .unwrap();
    assert_eq!(environment.environment_id, "prod");
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "GET");
    assert_eq!(
        requests[0].target,
        "/v1/applications/routing/instances/prod"
    );
}

#[tokio::test]
async fn creates_a_missing_environment_with_exist_ok() {
    let server = MockServer::start(vec![
        MockResponse::new(404).body("app instance not found"),
        MockResponse::json(200, environment_json()),
    ])
    .await;
    let params = EnvironmentCreateParams {
        exist_ok: true,
        ..create_params("prod")
    };
    support::platform(&server)
        .create_environment("routing", "v1", params)
        .await
        .unwrap();
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[1].method, "POST");
    assert_eq!(requests[1].target, "/v1/applications/routing/instances");
    assert_eq!(
        support::body(&requests[1]),
        json!({ "id": "prod", "name": "prod", "version_id": "v1" })
    );
}

#[tokio::test]
async fn stops_on_other_errors_with_exist_ok() {
    let server = MockServer::start(vec![MockResponse::new(500).body("boom")]).await;
    let params = EnvironmentCreateParams {
        exist_ok: true,
        ..create_params("prod")
    };
    let err = support::platform(&server)
        .create_environment("routing", "v1", params)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Status { status: 500, .. }), "{err}");
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "GET");
}

#[tokio::test]
async fn needs_an_id_for_exist_ok() {
    let server = MockServer::start(Vec::new()).await;
    let params = EnvironmentCreateParams {
        exist_ok: true,
        ..Default::default()
    };
    let err = support::platform(&server)
        .create_environment("routing", "v1", params)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Invalid { .. }), "{err}");
    assert_eq!(
        err.to_string(),
        "invalid argument: `exist_ok` needs an `environment_id`"
    );
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn accepts_queuing_priorities_from_1_to_9() {
    let server = MockServer::start(vec![
        MockResponse::json(200, environment_json()),
        MockResponse::json(200, environment_json()),
    ])
    .await;
    let platform = support::platform(&server);
    for priority in [1, 9] {
        let params = EnvironmentCreateParams {
            configuration: Some(EnvironmentConfiguration {
                queuing: queuing(Some(priority), None),
                ..Default::default()
            }),
            ..create_params("prod")
        };
        platform
            .create_environment("routing", "v1", params)
            .await
            .unwrap();
    }
    let priorities: Vec<Value> = server
        .requests()
        .iter()
        .map(|request| support::body(request)["configuration"]["queuing"].clone())
        .collect();
    assert_eq!(
        priorities,
        [json!({ "priority": 1 }), json!({ "priority": 9 })]
    );
}

#[tokio::test]
async fn rejects_an_unknown_input_format() {
    let server = MockServer::start(Vec::new()).await;
    let configuration = EnvironmentConfiguration {
        format: Some(Format {
            format_input: FormatInput {
                input_type: ContentFormat::Unknown,
            },
            format_output: None,
        }),
        ..Default::default()
    };
    let params = EnvironmentCreateParams {
        configuration: Some(configuration),
        ..create_params("prod")
    };
    let err = support::platform(&server)
        .create_environment("routing", "v1", params)
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "invalid argument: the input `format` must be `json` or `multi-file`"
    );
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn rejects_queuing_priorities_outside_1_to_9() {
    let server = MockServer::start(Vec::new()).await;
    let platform = support::platform(&server);
    for priority in [0, 10] {
        let configuration = EnvironmentConfiguration {
            queuing: queuing(Some(priority), Some(true)),
            ..Default::default()
        };
        let message = format!(
            "invalid argument: the queuing `priority` must be between 1 and 9, not {priority}"
        );
        let params = EnvironmentCreateParams {
            configuration: Some(configuration.clone()),
            ..create_params("prod")
        };
        let err = platform
            .create_environment("routing", "v1", params)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Invalid { .. }), "{err}");
        assert_eq!(err.to_string(), message);
        // An update checks it before it unlocks the environment, too.
        let params = EnvironmentUpdateParams {
            configuration: Some(configuration),
            locked: Some(false),
            ..Default::default()
        };
        let err = platform
            .update_environment("routing", "prod", params)
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), message);
    }
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn rejects_other_execution_classes_with_an_integration() {
    let server = MockServer::start(Vec::new()).await;
    let params = EnvironmentCreateParams {
        configuration: Some(EnvironmentConfiguration {
            execution_class: Some("6c9500mb870s".to_owned()),
            integration_id: Some("dbx".to_owned()),
            ..Default::default()
        }),
        ..create_params("prod")
    };
    let err = support::platform(&server)
        .create_environment("routing", "v1", params)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Invalid { .. }), "{err}");
    assert_eq!(
        err.to_string(),
        "invalid argument: when `integration_id` is set, `execution_class` must be \
         `integration` or unset"
    );
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn runs_an_integration_on_the_integration_execution_class() {
    let server = MockServer::start(vec![
        MockResponse::json(200, integration_json()),
        MockResponse::json(200, integration_json()),
    ])
    .await;
    let platform = support::platform(&server);
    for execution_class in [None, Some(String::new())] {
        let params = EnvironmentCreateParams {
            configuration: Some(EnvironmentConfiguration {
                execution_class,
                format: Some(Format {
                    format_input: FormatInput {
                        input_type: ContentFormat::MultiFile,
                    },
                    format_output: Some(FormatOutput {
                        output_type: ContentFormat::MultiFile,
                    }),
                }),
                integration_id: Some("dbx".to_owned()),
                ..Default::default()
            }),
            ..create_params("prod")
        };
        platform
            .create_environment("routing", "v1", params)
            .await
            .unwrap();
    }
    // An integration keeps only the input format.
    for request in server.requests() {
        assert_eq!(
            support::body(&request)["configuration"],
            json!({
                "execution_class": "integration",
                "format": { "input": { "type": "multi-file" } },
                "integration_id": "dbx",
            })
        );
    }
}

#[tokio::test]
async fn gets_an_environment() {
    let server = MockServer::start(vec![MockResponse::json(200, environment_json())]).await;
    let environment = support::platform(&server)
        .get_environment("routing", "prod")
        .await
        .unwrap();
    let request = &server.requests()[0];
    assert_eq!(request.method, "GET");
    assert_eq!(request.target, "/v1/applications/routing/instances/prod");
    assert_eq!(environment.name, "Production");
    assert_eq!(
        environment.configuration.queuing,
        queuing(Some(6), Some(false))
    );
    // Emitted JSON uses the API names, and leaves out what the SDK does not
    // model.
    let mut expected = environment_json();
    expected
        .as_object_mut()
        .unwrap()
        .remove("working_directory");
    assert_eq!(serde_json::to_value(&environment).unwrap(), expected);
}

#[tokio::test]
async fn reads_new_content_formats_as_unknown() {
    let mut environment = environment_json();
    environment["configuration"]["format"]["input"]["type"] = json!("csv-archive");
    let server = MockServer::start(vec![MockResponse::json(200, environment)]).await;
    let environment = support::platform(&server)
        .get_environment("routing", "prod")
        .await
        .unwrap();
    let format = environment.configuration.format.unwrap();
    assert_eq!(format.format_input.input_type, ContentFormat::Unknown);
}

#[tokio::test]
async fn checks_whether_an_environment_exists() {
    let server = MockServer::start(vec![
        MockResponse::json(200, environment_json()),
        MockResponse::new(404).body("app instance not found"),
        MockResponse::new(500).body("boom"),
    ])
    .await;
    let platform = support::platform(&server);
    assert!(
        platform
            .environment_exists("routing", "prod")
            .await
            .unwrap()
    );
    assert!(!platform.environment_exists("routing", "dev").await.unwrap());
    let err = platform
        .environment_exists("routing", "prod")
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Status { status: 500, .. }), "{err}");
    let targets: Vec<String> = server.requests().into_iter().map(|r| r.target).collect();
    assert_eq!(
        targets,
        [
            "/v1/applications/routing/instances/prod",
            "/v1/applications/routing/instances/dev",
            "/v1/applications/routing/instances/prod",
        ]
    );
}

#[tokio::test]
async fn lists_environments_across_pages() {
    let mut second = environment_json();
    second["id"] = json!("dev");
    let server = MockServer::start(vec![
        MockResponse::json(
            200,
            json!({ "items": [environment_json()], "next_page_token": "page-2" }),
        ),
        MockResponse::json(200, json!({ "items": [second] })),
    ])
    .await;
    let environments = support::platform(&server)
        .list_environments("routing")
        .await
        .unwrap();
    let environment_ids: Vec<&str> = environments
        .iter()
        .map(|environment| environment.environment_id.as_str())
        .collect();
    assert_eq!(environment_ids, ["prod", "dev"]);

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert_eq!(request.method, "GET");
        assert!(
            request
                .target
                .starts_with("/v1/applications/routing/instances?")
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
    let mut updated = environment_json();
    updated["name"] = json!("Production EU");
    updated["version_id"] = json!("v2");
    let server = MockServer::start(vec![
        MockResponse::json(200, environment_json()),
        MockResponse::json(200, updated.clone()),
    ])
    .await;
    let params = EnvironmentUpdateParams {
        name: Some("Production EU".to_owned()),
        build_id: Some("v2".to_owned()),
        ..Default::default()
    };
    let environment = support::platform(&server)
        .update_environment("routing", "prod", params)
        .await
        .unwrap();

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].method, "GET");
    assert_eq!(
        requests[0].target,
        "/v1/applications/routing/instances/prod"
    );
    assert_eq!(requests[1].method, "PUT");
    assert_eq!(
        requests[1].target,
        "/v1/applications/routing/instances/prod"
    );
    // Everything else, `working_directory` included, goes back as it came.
    assert_eq!(support::body(&requests[1]), updated);
    assert_eq!(environment.name, "Production EU");
    assert_eq!(environment.build_id, "v2");
}

#[tokio::test]
async fn merges_configuration_changes_field_by_field() {
    let server = MockServer::start(vec![
        MockResponse::json(200, environment_json()),
        MockResponse::json(200, environment_json()),
    ])
    .await;
    let params = EnvironmentUpdateParams {
        configuration: Some(EnvironmentConfiguration {
            execution_class: Some("8c16gb12h".to_owned()),
            queuing: queuing(Some(2), None),
            ..Default::default()
        }),
        ..Default::default()
    };
    support::platform(&server)
        .update_environment("routing", "prod", params)
        .await
        .unwrap();
    // The options, secrets and format stay, and so does `queuing.disabled`.
    let mut expected = environment_json();
    expected["configuration"]["execution_class"] = json!("8c16gb12h");
    expected["configuration"]["queuing"] = json!({ "priority": 2, "disabled": false });
    assert_eq!(support::body(&server.requests()[1]), expected);
}

#[tokio::test]
async fn replaces_the_options_and_the_format_as_a_whole() {
    let server = MockServer::start(vec![
        MockResponse::json(200, environment_json()),
        MockResponse::json(200, environment_json()),
    ])
    .await;
    let params = EnvironmentUpdateParams {
        configuration: Some(EnvironmentConfiguration {
            format: Some(Format {
                format_input: FormatInput {
                    input_type: ContentFormat::MultiFile,
                },
                format_output: None,
            }),
            options: Some(BTreeMap::from([(
                "solve.iterations".to_owned(),
                "50".to_owned(),
            )])),
            ..Default::default()
        }),
        ..Default::default()
    };
    support::platform(&server)
        .update_environment("routing", "prod", params)
        .await
        .unwrap();
    let mut expected = environment_json();
    expected["configuration"]["format"] = json!({ "input": { "type": "multi-file" } });
    expected["configuration"]["options"] = json!({ "solve.iterations": "50" });
    assert_eq!(support::body(&server.requests()[1]), expected);
}

#[tokio::test]
async fn removes_the_integration_with_an_empty_id() {
    let server = MockServer::start(vec![
        MockResponse::json(200, integration_json()),
        MockResponse::json(200, environment_json()),
    ])
    .await;
    let params = EnvironmentUpdateParams {
        configuration: Some(EnvironmentConfiguration {
            execution_class: Some("6c9500mb870s".to_owned()),
            integration_id: Some(String::new()),
            ..Default::default()
        }),
        ..Default::default()
    };
    support::platform(&server)
        .update_environment("routing", "prod", params)
        .await
        .unwrap();
    assert_eq!(
        support::body(&server.requests()[1])["configuration"],
        json!({
            "execution_class": "6c9500mb870s",
            "queuing": { "priority": 6, "disabled": false },
        })
    );
}

#[tokio::test]
async fn leaves_the_integration_for_another_execution_class() {
    let server = MockServer::start(vec![
        MockResponse::json(200, integration_json()),
        MockResponse::json(200, environment_json()),
        MockResponse::json(200, integration_json()),
        MockResponse::json(200, integration_json()),
        MockResponse::json(200, integration_json()),
        MockResponse::json(200, integration_json()),
    ])
    .await;
    let platform = support::platform(&server);
    let changes = [
        EnvironmentConfiguration {
            execution_class: Some("6c9500mb870s".to_owned()),
            ..Default::default()
        },
        // An empty class keeps the stored one, and other fields keep the
        // integration.
        EnvironmentConfiguration {
            execution_class: Some(String::new()),
            ..Default::default()
        },
        EnvironmentConfiguration {
            secrets_collection_id: Some("routing-secrets".to_owned()),
            ..Default::default()
        },
    ];
    for configuration in changes {
        let params = EnvironmentUpdateParams {
            configuration: Some(configuration),
            ..Default::default()
        };
        platform
            .update_environment("routing", "prod", params)
            .await
            .unwrap();
    }
    let requests = server.requests();
    assert_eq!(
        support::body(&requests[1])["configuration"],
        json!({
            "execution_class": "6c9500mb870s",
            "queuing": { "priority": 6, "disabled": false },
        })
    );
    for request in [&requests[3], &requests[5]] {
        assert_eq!(
            support::body(request)["configuration"]["integration_id"],
            "dbx"
        );
    }
}

#[tokio::test]
async fn moves_an_environment_to_an_integration() {
    let server = MockServer::start(vec![
        MockResponse::json(200, environment_json()),
        MockResponse::json(200, integration_json()),
    ])
    .await;
    let params = EnvironmentUpdateParams {
        configuration: Some(EnvironmentConfiguration {
            integration_id: Some("dbx".to_owned()),
            ..Default::default()
        }),
        ..Default::default()
    };
    support::platform(&server)
        .update_environment("routing", "prod", params)
        .await
        .unwrap();
    // The stored execution class gives way to `integration`.
    let configuration = &support::body(&server.requests()[1])["configuration"];
    assert_eq!(configuration["execution_class"], "integration");
    assert_eq!(configuration["integration_id"], "dbx");
}

#[tokio::test]
async fn adds_a_configuration_when_the_stored_one_is_missing() {
    for stored in [None, Some(Value::Null)] {
        let mut environment = environment_json();
        let object = environment.as_object_mut().unwrap();
        object.remove("configuration");
        if let Some(stored) = stored {
            object.insert("configuration".to_owned(), stored);
        }
        let server = MockServer::start(vec![
            MockResponse::json(200, environment),
            MockResponse::json(200, environment_json()),
        ])
        .await;
        let params = EnvironmentUpdateParams {
            configuration: Some(EnvironmentConfiguration {
                queuing: queuing(None, Some(true)),
                ..Default::default()
            }),
            ..Default::default()
        };
        support::platform(&server)
            .update_environment("routing", "prod", params)
            .await
            .unwrap();
        assert_eq!(
            support::body(&server.requests()[1])["configuration"],
            json!({ "queuing": { "disabled": true } })
        );
    }
}

#[tokio::test]
async fn locks_an_environment_after_updating_it() {
    let mut locked = environment_json();
    locked["description"] = json!("Frozen for the audit.");
    locked["locked"] = json!(true);
    let server = MockServer::start(vec![
        MockResponse::json(200, environment_json()),
        MockResponse::json(200, environment_json()),
        MockResponse::json(200, locked),
    ])
    .await;
    let params = EnvironmentUpdateParams {
        description: Some("Frozen for the audit.".to_owned()),
        locked: Some(true),
        ..Default::default()
    };
    let environment = support::platform(&server)
        .update_environment("routing", "prod", params)
        .await
        .unwrap();
    let requests = server.requests();
    let calls: Vec<(&str, &str)> = requests
        .iter()
        .map(|r| (r.method.as_str(), r.target.as_str()))
        .collect();
    assert_eq!(
        calls,
        [
            ("GET", "/v1/applications/routing/instances/prod"),
            ("PUT", "/v1/applications/routing/instances/prod"),
            ("PUT", "/v1/applications/routing/instances/prod/lock"),
        ]
    );
    assert_eq!(
        support::body(&requests[1])["description"],
        "Frozen for the audit."
    );
    assert_eq!(support::body(&requests[2]), json!({ "locked": true }));
    // The lock's response is the final state.
    assert!(environment.locked);
    assert_eq!(environment.description, "Frozen for the audit.");
}

#[tokio::test]
async fn unlocks_an_environment_before_updating_it() {
    let mut updated = environment_json();
    updated["name"] = json!("Production EU");
    let server = MockServer::start(vec![
        MockResponse::json(200, environment_json()),
        MockResponse::json(200, environment_json()),
        MockResponse::json(200, updated),
    ])
    .await;
    let params = EnvironmentUpdateParams {
        name: Some("Production EU".to_owned()),
        locked: Some(false),
        ..Default::default()
    };
    let environment = support::platform(&server)
        .update_environment("routing", "prod", params)
        .await
        .unwrap();
    let requests = server.requests();
    let calls: Vec<(&str, &str)> = requests
        .iter()
        .map(|r| (r.method.as_str(), r.target.as_str()))
        .collect();
    assert_eq!(
        calls,
        [
            ("PUT", "/v1/applications/routing/instances/prod/lock"),
            ("GET", "/v1/applications/routing/instances/prod"),
            ("PUT", "/v1/applications/routing/instances/prod"),
        ]
    );
    assert_eq!(support::body(&requests[0]), json!({ "locked": false }));
    assert_eq!(support::body(&requests[2])["name"], "Production EU");
    assert_eq!(environment.name, "Production EU");
    assert!(!environment.locked);
}

#[tokio::test]
async fn refuses_to_update_a_locked_environment() {
    let mut stored = environment_json();
    stored["locked"] = json!(true);
    let server = MockServer::start(vec![
        MockResponse::json(200, stored.clone()),
        MockResponse::json(200, stored),
    ])
    .await;
    let platform = support::platform(&server);
    // Locking it after the change doesn't unlock it for the change.
    for locked in [None, Some(true)] {
        let params = EnvironmentUpdateParams {
            name: Some("Production EU".to_owned()),
            locked,
            ..Default::default()
        };
        let err = platform
            .update_environment("routing", "prod", params)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Invalid { .. }), "{err}");
        assert_eq!(
            err.to_string(),
            "invalid argument: the environment is locked: set `locked` to false to update it"
        );
    }
    // Only the GETs went out, no PUT.
    let methods: Vec<String> = server.requests().into_iter().map(|r| r.method).collect();
    assert_eq!(methods, ["GET", "GET"]);
}

#[tokio::test]
async fn only_locks_when_nothing_else_changes() {
    let mut locked = environment_json();
    locked["locked"] = json!(true);
    let server = MockServer::start(vec![
        MockResponse::json(200, locked.clone()),
        MockResponse::json(200, locked),
    ])
    .await;
    let platform = support::platform(&server);
    // A configuration that sets no field changes nothing either.
    for configuration in [None, Some(EnvironmentConfiguration::default())] {
        let params = EnvironmentUpdateParams {
            configuration,
            locked: Some(true),
            ..Default::default()
        };
        let environment = platform
            .update_environment("routing", "prod", params)
            .await
            .unwrap();
        assert!(environment.locked);
    }
    for request in server.requests() {
        assert_eq!(request.method, "PUT");
        assert_eq!(
            request.target,
            "/v1/applications/routing/instances/prod/lock"
        );
        assert_eq!(support::body(&request), json!({ "locked": true }));
    }
}

#[tokio::test]
async fn rejects_the_reserved_id_default() {
    let server = MockServer::start(Vec::new()).await;
    let platform = support::platform(&server);
    let create = EnvironmentCreateParams {
        environment_id: Some("default".to_owned()),
        exist_ok: true,
        ..Default::default()
    };
    let err = platform
        .create_environment("routing", "v1", create)
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "invalid argument: `default` is reserved: it names the workspace's default environment"
    );
    let update = EnvironmentUpdateParams {
        name: Some("Default".to_owned()),
        ..Default::default()
    };
    let err = platform
        .update_environment("routing", "default", update)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Invalid { .. }), "{err}");
    assert_eq!(
        err.to_string(),
        "invalid argument: `default` is reserved: it names the workspace's default environment"
    );
    // Before any request.
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn rejects_an_update_that_changes_nothing() {
    let server = MockServer::start(Vec::new()).await;
    let platform = support::platform(&server);
    let empty = [
        None,
        Some(EnvironmentConfiguration::default()),
        Some(EnvironmentConfiguration {
            queuing: queuing(None, None),
            ..Default::default()
        }),
    ];
    for configuration in empty {
        let params = EnvironmentUpdateParams {
            configuration,
            ..Default::default()
        };
        let err = platform
            .update_environment("routing", "prod", params)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Invalid { .. }), "{err}");
        assert_eq!(
            err.to_string(),
            "invalid argument: the update changes nothing: set at least one field"
        );
    }
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn deletes_an_environment() {
    let server = MockServer::start(vec![MockResponse::new(204)]).await;
    support::platform(&server)
        .delete_environment("routing", "prod")
        .await
        .unwrap();
    let request = &server.requests()[0];
    assert_eq!(request.method, "DELETE");
    assert_eq!(request.target, "/v1/applications/routing/instances/prod");
}

#[tokio::test]
async fn encodes_environment_ids_in_paths() {
    let server = MockServer::start(vec![MockResponse::json(200, environment_json())]).await;
    let platform = support::platform(&server);
    platform
        .get_environment("my workspace", "prod?eu")
        .await
        .unwrap();
    assert_eq!(
        server.requests()[0].target,
        "/v1/applications/my%20workspace/instances/prod%3Feu"
    );
    for (workspace_id, environment_id) in [
        ("routing", ".."),
        ("..", "prod"),
        ("routing", ""),
        ("routing", "prod/eu"),
    ] {
        let err = platform
            .delete_environment(workspace_id, environment_id)
            .await
            .unwrap_err();
        assert!(
            matches!(err, Error::Invalid { .. }),
            "{environment_id:?}: {err}"
        );
        let params = EnvironmentUpdateParams {
            locked: Some(false),
            ..Default::default()
        };
        let err = platform
            .update_environment(workspace_id, environment_id, params)
            .await
            .unwrap_err();
        assert!(
            matches!(err, Error::Invalid { .. }),
            "{environment_id:?}: {err}"
        );
    }
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn serves_the_create_tool() {
    let (result, requests) = support::serve_tool_ok(
        vec![MockResponse::json(200, environment_json())],
        "decision_platform_environment_create",
        json!({
            "workspace_id": "routing",
            "build_id": "v1",
            "environment_id": "prod",
            "configuration": { "execution_class": "6c9500mb870s", "queuing": { "priority": 3 } },
        }),
    )
    .await;
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].target, "/v1/applications/routing/instances");
    assert_eq!(
        support::body(&requests[0]),
        json!({
            "id": "prod",
            "name": "prod",
            "version_id": "v1",
            "configuration": { "execution_class": "6c9500mb870s", "queuing": { "priority": 3 } },
        })
    );
    assert_eq!(result["application_id"], "routing");
    assert_eq!(result["version_id"], "v1");
}

#[tokio::test]
async fn the_create_tool_needs_a_build_id() {
    let server = MockServer::start(Vec::new()).await;
    let cx = Context::new().with_platform(support::platform(&server));
    let err = support::call_tool(
        cx,
        "decision_platform_environment_create",
        json!({ "workspace_id": "routing", "environment_id": "prod" }),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ToolError::InvalidArguments(_)), "{err}");
    assert!(err.to_string().contains("build_id"), "{err}");
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn serves_the_get_tool() {
    let (result, requests) = support::serve_tool_ok(
        vec![MockResponse::json(200, environment_json())],
        "decision_platform_environment_get",
        json!({ "workspace_id": "routing", "environment_id": "prod" }),
    )
    .await;
    assert_eq!(
        requests[0].target,
        "/v1/applications/routing/instances/prod"
    );
    assert_eq!(result["configuration"]["queuing"]["priority"], 6);
}

#[tokio::test]
async fn serves_the_exists_tool() {
    let (result, requests) = support::serve_tool_ok(
        vec![MockResponse::new(404).body("app instance not found")],
        "decision_platform_environment_exists",
        json!({ "workspace_id": "routing", "environment_id": "prod" }),
    )
    .await;
    assert_eq!(
        requests[0].target,
        "/v1/applications/routing/instances/prod"
    );
    assert_eq!(result, json!(false));
}

#[tokio::test]
async fn serves_the_list_tool() {
    let (result, requests) = support::serve_tool_ok(
        vec![MockResponse::json(
            200,
            json!({ "items": [environment_json()] }),
        )],
        "decision_platform_environment_list",
        json!({ "workspace_id": "routing" }),
    )
    .await;
    assert_eq!(
        requests[0].target,
        "/v1/applications/routing/instances?pagereturn=true"
    );
    assert_eq!(result[0]["id"], "prod");
}

#[tokio::test]
async fn serves_the_update_tool() {
    let mut locked = environment_json();
    locked["configuration"]["options"] = json!({ "solve.duration": "30s" });
    locked["locked"] = json!(true);
    let (result, requests) = support::serve_tool_ok(
        vec![
            MockResponse::json(200, environment_json()),
            MockResponse::json(200, environment_json()),
            MockResponse::json(200, locked),
        ],
        "decision_platform_environment_update",
        json!({
            "workspace_id": "routing",
            "environment_id": "prod",
            "configuration": { "options": { "solve.duration": "30s" } },
            "locked": true,
        }),
    )
    .await;
    assert_eq!(requests[1].method, "PUT");
    assert_eq!(
        support::body(&requests[1])["configuration"]["options"],
        json!({ "solve.duration": "30s" })
    );
    assert_eq!(
        requests[2].target,
        "/v1/applications/routing/instances/prod/lock"
    );
    assert_eq!(result["locked"], true);
}

#[tokio::test]
async fn serves_the_delete_tool() {
    let (result, requests) = support::serve_tool_ok(
        vec![MockResponse::new(204)],
        "decision_platform_environment_delete",
        json!({ "workspace_id": "routing", "environment_id": "prod" }),
    )
    .await;
    assert_eq!(requests[0].method, "DELETE");
    assert_eq!(
        requests[0].target,
        "/v1/applications/routing/instances/prod"
    );
    assert_eq!(result, Value::Null);
}
