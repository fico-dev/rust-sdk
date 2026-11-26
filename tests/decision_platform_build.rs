//! Tests for the platform build operations and their tools, against the mock
//! server.

mod mock_server;
mod support;

use fico_sdk::Error;
use fico_sdk::decision::platform::{BuildCreateParams, BuildUpdateParams};
use fico_sdk::decision::{AccountMemberRole, ContentFormat, ControlType, ManifestOptionType};
use fico_sdk::tool::{Context, ToolError};
use mock_server::{MockResponse, MockServer};
use serde_json::{Value, json};

/// A build as the platform returns it, with a field the SDK does not model
/// (`statistics_path` in the multi-file output).
fn build_json() -> Value {
    json!({
        "id": "v1",
        "application_id": "routing",
        "name": "v1",
        "description": "First build.",
        "executable": {
            "id": "v1",
            "user_email": "dev@example.com",
            "uploaded_at": "2026-10-01T11:58:00Z",
            "requirements": {
                "executable_type": "go",
                "runtime": "ghcr.io/nextmv-io/runtime/default:latest",
                "entrypoint": "main",
                "working_directory": "bin",
                "options": {
                    "strict": false,
                    "validation": { "enforce": "all" },
                    "format": { "template": ["--{{name}}={{value}}"] },
                    "items": [
                        {
                            "name": "duration",
                            "description": "Max solver runtime in seconds",
                            "option_type": "int",
                            "required": false,
                            "default": 30,
                            "additional_attributes": { "max": 300, "min": 1, "step": 10 },
                            "ui": { "control_type": "slider", "display_name": "Duration" },
                        },
                        {
                            "name": "details",
                            "description": "",
                            "option_type": "bool",
                            "required": true,
                            "ui": { "control_type": "toggle", "hidden_from": ["operator"] },
                        },
                    ],
                },
                "io_configuration": {
                    "format": "multi-file",
                    "multi_file": {
                        "input_path": "inputs",
                        "output_configuration": {
                            "statistics_path": "outputs/statistics.json",
                            "metrics_path": "outputs/metrics.json",
                            "assets_path": "outputs/assets.json",
                            "solutions_path": "outputs/solutions",
                        },
                    },
                },
            },
        },
        "created_at": "2026-10-01T12:00:00Z",
        "updated_at": "2026-10-01T12:00:00Z",
    })
}

#[tokio::test]
async fn creates_a_build() {
    let server = MockServer::start(vec![MockResponse::json(200, build_json())]).await;
    let params = BuildCreateParams {
        build_id: Some("v1".to_owned()),
        name: Some("v1".to_owned()),
        description: Some("First build.".to_owned()),
        exist_ok: false,
    };
    let build = support::platform(&server)
        .create_build("routing", params)
        .await
        .unwrap();

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].target, "/v1/applications/routing/versions");
    assert_eq!(
        support::body(&requests[0]),
        json!({ "id": "v1", "name": "v1", "description": "First build." })
    );
    assert_eq!(build.build_id, "v1");
    assert_eq!(build.workspace_id, "routing");
    let executable = build.executable.unwrap();
    assert_eq!(executable.executable_id, "v1");
    assert_eq!(executable.user_email, "dev@example.com");
    assert_eq!(executable.requirements.executable_type, "go");
    assert_eq!(
        executable.requirements.runtime,
        "ghcr.io/nextmv-io/runtime/default:latest"
    );
}

#[tokio::test]
async fn names_a_new_build_after_its_id() {
    let server = MockServer::start(vec![MockResponse::json(200, build_json())]).await;
    let params = BuildCreateParams {
        build_id: Some("v1".to_owned()),
        ..Default::default()
    };
    support::platform(&server)
        .create_build("routing", params)
        .await
        .unwrap();
    let request = &server.requests()[0];
    assert_eq!(support::body(request), json!({ "id": "v1", "name": "v1" }));
}

#[tokio::test]
async fn generates_an_id_when_none_is_set() {
    let server = MockServer::start(vec![MockResponse::json(200, build_json())]).await;
    let params = BuildCreateParams {
        build_id: Some(String::new()),
        ..Default::default()
    };
    support::platform(&server)
        .create_build("routing", params)
        .await
        .unwrap();
    let body = support::body(&server.requests()[0]);
    let build_id = body["id"].as_str().unwrap();
    let suffix = build_id.strip_prefix("build-").unwrap();
    assert_eq!(suffix.len(), 8, "{build_id}");
    assert!(
        suffix
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()),
        "{build_id}"
    );
    assert_eq!(body["name"], build_id);
}

#[tokio::test]
async fn returns_the_existing_build_with_exist_ok() {
    let server = MockServer::start(vec![MockResponse::json(200, build_json())]).await;
    let params = BuildCreateParams {
        build_id: Some("v1".to_owned()),
        exist_ok: true,
        ..Default::default()
    };
    let build = support::platform(&server)
        .create_build("routing", params)
        .await
        .unwrap();
    assert_eq!(build.build_id, "v1");
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[0].target, "/v1/applications/routing/versions/v1");
}

#[tokio::test]
async fn creates_a_missing_build_with_exist_ok() {
    let server = MockServer::start(vec![
        MockResponse::new(404).body("version not found"),
        MockResponse::json(200, build_json()),
    ])
    .await;
    let params = BuildCreateParams {
        build_id: Some("v1".to_owned()),
        exist_ok: true,
        ..Default::default()
    };
    support::platform(&server)
        .create_build("routing", params)
        .await
        .unwrap();
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[1].method, "POST");
    assert_eq!(requests[1].target, "/v1/applications/routing/versions");
    assert_eq!(
        support::body(&requests[1]),
        json!({ "id": "v1", "name": "v1" })
    );
}

#[tokio::test]
async fn stops_on_other_errors_with_exist_ok() {
    let server = MockServer::start(vec![MockResponse::new(500).body("boom")]).await;
    let params = BuildCreateParams {
        build_id: Some("v1".to_owned()),
        exist_ok: true,
        ..Default::default()
    };
    let err = support::platform(&server)
        .create_build("routing", params)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Status { status: 500, .. }), "{err}");
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[0].target, "/v1/applications/routing/versions/v1");
}

#[tokio::test]
async fn needs_an_id_for_exist_ok() {
    let server = MockServer::start(Vec::new()).await;
    let params = BuildCreateParams {
        exist_ok: true,
        ..Default::default()
    };
    let err = support::platform(&server)
        .create_build("routing", params)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Invalid { .. }), "{err}");
    assert_eq!(
        err.to_string(),
        "invalid argument: `exist_ok` needs a `build_id`"
    );
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn gets_a_build() {
    let server = MockServer::start(vec![MockResponse::json(200, build_json())]).await;
    let build = support::platform(&server)
        .get_build("routing", "v1")
        .await
        .unwrap();
    let request = &server.requests()[0];
    assert_eq!(request.method, "GET");
    assert_eq!(request.target, "/v1/applications/routing/versions/v1");
    assert_eq!(build.description, "First build.");
    let requirements = &build.executable.as_ref().unwrap().requirements;
    assert_eq!(requirements.entrypoint.as_deref(), Some("main"));
    assert_eq!(requirements.working_directory.as_deref(), Some("bin"));
    let options = requirements.options.as_ref().unwrap();
    assert_eq!(options.items[0].option_type, ManifestOptionType::Int);
    assert_eq!(
        options.items[0].ui.as_ref().unwrap().control_type,
        Some(ControlType::Slider)
    );
    assert_eq!(
        options.format.as_ref().unwrap().template,
        ["--{{name}}={{value}}"]
    );
    let io = requirements.io_configuration.as_ref().unwrap();
    assert_eq!(io.format, ContentFormat::MultiFile);
    // Emitted JSON uses the API names, and leaves out what the SDK does not
    // model.
    let mut expected = build_json();
    let output = expected["executable"]["requirements"]["io_configuration"]["multi_file"]
        ["output_configuration"]
        .as_object_mut()
        .unwrap();
    output.remove("statistics_path");
    assert_eq!(serde_json::to_value(&build).unwrap(), expected);
}

#[tokio::test]
async fn decodes_a_build_with_values_the_sdk_does_not_know() {
    let mut build = build_json();
    let requirements = &mut build["executable"]["requirements"];
    requirements["io_configuration"] = json!({ "format": "csv-archive" });
    let item = &mut requirements["options"]["items"][0];
    item["option_type"] = json!("date");
    item["ui"] = json!({ "control_type": "calendar", "hidden_from": ["auditor"] });
    let server = MockServer::start(vec![MockResponse::json(200, build)]).await;
    let build = support::platform(&server)
        .get_build("routing", "v1")
        .await
        .unwrap();
    let requirements = build.executable.unwrap().requirements;
    let item = &requirements.options.unwrap().items[0];
    assert_eq!(item.option_type, ManifestOptionType::Unknown);
    let ui = item.ui.as_ref().unwrap();
    assert_eq!(ui.control_type, Some(ControlType::Unknown));
    assert_eq!(ui.hidden_from, Some(vec![AccountMemberRole::Unknown]));
    assert_eq!(
        requirements.io_configuration.unwrap().format,
        ContentFormat::Unknown
    );
}

#[tokio::test]
async fn decodes_a_build_with_only_its_type_and_runtime() {
    let mut build = build_json();
    build["executable"]["requirements"] = json!({
        "executable_type": "python",
        "runtime": "ghcr.io/nextmv-io/runtime/python:3.11",
    });
    let server = MockServer::start(vec![MockResponse::json(200, build)]).await;
    let build = support::platform(&server)
        .get_build("routing", "v1")
        .await
        .unwrap();
    let requirements = build.executable.unwrap().requirements;
    assert_eq!(requirements.options, None);
    assert_eq!(requirements.io_configuration, None);
    assert_eq!(requirements.entrypoint, None);
}

#[tokio::test]
async fn decodes_a_build_without_an_executable() {
    let mut without = build_json();
    without.as_object_mut().unwrap().remove("executable");
    let server = MockServer::start(vec![MockResponse::json(200, without)]).await;
    let build = support::platform(&server)
        .get_build("routing", "v1")
        .await
        .unwrap();
    assert_eq!(build.executable, None);
}

#[tokio::test]
async fn checks_whether_a_build_exists() {
    let server = MockServer::start(vec![
        MockResponse::json(200, build_json()),
        MockResponse::new(404).body("version not found"),
        MockResponse::new(500).body("boom"),
    ])
    .await;
    let platform = support::platform(&server);
    assert!(platform.build_exists("routing", "v1").await.unwrap());
    assert!(!platform.build_exists("routing", "v9").await.unwrap());
    let err = platform.build_exists("routing", "v1").await.unwrap_err();
    assert!(matches!(err, Error::Status { status: 500, .. }), "{err}");
    let targets: Vec<String> = server.requests().into_iter().map(|r| r.target).collect();
    assert_eq!(
        targets,
        [
            "/v1/applications/routing/versions/v1",
            "/v1/applications/routing/versions/v9",
            "/v1/applications/routing/versions/v1",
        ]
    );
}

#[tokio::test]
async fn lists_builds_across_pages() {
    let mut second = build_json();
    second["id"] = json!("v2");
    let server = MockServer::start(vec![
        MockResponse::json(
            200,
            json!({ "items": [build_json()], "next_page_token": "page-2" }),
        ),
        MockResponse::json(200, json!({ "items": [second] })),
    ])
    .await;
    let builds = support::platform(&server)
        .list_builds("routing")
        .await
        .unwrap();
    let build_ids: Vec<&str> = builds.iter().map(|b| b.build_id.as_str()).collect();
    assert_eq!(build_ids, ["v1", "v2"]);

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert_eq!(request.method, "GET");
        assert!(
            request
                .target
                .starts_with("/v1/applications/routing/versions?")
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
    let mut updated = build_json();
    updated["description"] = json!("Faster routes.");
    let server = MockServer::start(vec![
        MockResponse::json(200, build_json()),
        MockResponse::json(200, updated.clone()),
    ])
    .await;
    let params = BuildUpdateParams {
        description: Some("Faster routes.".to_owned()),
        ..Default::default()
    };
    let build = support::platform(&server)
        .update_build("routing", "v1", params)
        .await
        .unwrap();

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[0].target, "/v1/applications/routing/versions/v1");
    assert_eq!(requests[1].method, "PUT");
    assert_eq!(requests[1].target, "/v1/applications/routing/versions/v1");
    // Everything else, `entrypoint` included, goes back as it came.
    assert_eq!(support::body(&requests[1]), updated);
    assert_eq!(build.description, "Faster routes.");
    assert_eq!(build.name, "v1");
}

#[tokio::test]
async fn rejects_an_empty_update() {
    let server = MockServer::start(Vec::new()).await;
    let err = support::platform(&server)
        .update_build("routing", "v1", BuildUpdateParams::default())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Invalid { .. }), "{err}");
    assert_eq!(
        err.to_string(),
        "invalid argument: nothing to update: set `name` or `description`"
    );
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn sends_an_empty_string_as_a_change() {
    let mut updated = build_json();
    updated["description"] = json!("");
    let server = MockServer::start(vec![
        MockResponse::json(200, build_json()),
        MockResponse::json(200, updated.clone()),
    ])
    .await;
    let params = BuildUpdateParams {
        description: Some(String::new()),
        ..Default::default()
    };
    let build = support::platform(&server)
        .update_build("routing", "v1", params)
        .await
        .unwrap();

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].method, "PUT");
    assert_eq!(support::body(&requests[1]), updated);
    assert_eq!(build.description, "");
}

#[tokio::test]
async fn deletes_a_build() {
    let server = MockServer::start(vec![MockResponse::new(204)]).await;
    support::platform(&server)
        .delete_build("routing", "v1")
        .await
        .unwrap();
    let request = &server.requests()[0];
    assert_eq!(request.method, "DELETE");
    assert_eq!(request.target, "/v1/applications/routing/versions/v1");
}

#[tokio::test]
async fn encodes_build_ids_in_paths() {
    let server = MockServer::start(vec![MockResponse::json(200, build_json())]).await;
    let platform = support::platform(&server);
    platform.get_build("my workspace", "v1?beta").await.unwrap();
    assert_eq!(
        server.requests()[0].target,
        "/v1/applications/my%20workspace/versions/v1%3Fbeta"
    );
    for (workspace_id, build_id) in [
        ("routing", ".."),
        ("..", "v1"),
        ("routing", ""),
        ("routing", "v1/beta"),
    ] {
        let err = platform
            .delete_build(workspace_id, build_id)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Invalid { .. }), "{build_id:?}: {err}");
    }
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn serves_the_create_tool() {
    let (result, requests) = support::serve_tool_ok(
        vec![MockResponse::json(200, build_json())],
        "decision_platform_build_create",
        json!({ "workspace_id": "routing", "build_id": "v1", "description": "First build." }),
    )
    .await;
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].target, "/v1/applications/routing/versions");
    // The `build_id` argument is the API's `id`, in the body and in the
    // result.
    assert_eq!(
        support::body(&requests[0]),
        json!({ "id": "v1", "name": "v1", "description": "First build." })
    );
    assert_eq!(result["id"], "v1");
    assert_eq!(result["application_id"], "routing");
}

#[tokio::test]
async fn rejects_an_id_argument_in_the_create_tool() {
    let server = MockServer::start(Vec::new()).await;
    let cx = Context::new().with_platform(support::platform(&server));
    let args = json!({ "workspace_id": "routing", "id": "v1" });
    let err = support::call_tool(cx, "decision_platform_build_create", args)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ToolError::InvalidArguments(m) if m.starts_with("unknown field `id`")),
        "{err}"
    );
    assert!(server.requests().is_empty());
}

#[test]
fn does_not_read_id_as_the_build_id() {
    // `Tool::call` checks arguments against the schema, which leaves out
    // `#[serde(alias)]`, so only serde shows that `id` is no alias.
    let params: BuildCreateParams = serde_json::from_value(json!({ "id": "v1" })).unwrap();
    assert_eq!(params.build_id, None);
}

#[tokio::test]
async fn serves_the_get_tool() {
    let (result, requests) = support::serve_tool_ok(
        vec![MockResponse::json(200, build_json())],
        "decision_platform_build_get",
        json!({ "workspace_id": "routing", "build_id": "v1" }),
    )
    .await;
    assert_eq!(requests[0].target, "/v1/applications/routing/versions/v1");
    assert_eq!(
        result["executable"]["requirements"]["executable_type"],
        "go"
    );
}

#[tokio::test]
async fn serves_the_exists_tool() {
    let (result, requests) = support::serve_tool_ok(
        vec![MockResponse::json(200, build_json())],
        "decision_platform_build_exists",
        json!({ "workspace_id": "routing", "build_id": "v1" }),
    )
    .await;
    assert_eq!(requests[0].target, "/v1/applications/routing/versions/v1");
    assert_eq!(result, json!(true));
}

#[tokio::test]
async fn serves_the_list_tool() {
    let (result, requests) = support::serve_tool_ok(
        vec![MockResponse::json(200, json!({ "items": [build_json()] }))],
        "decision_platform_build_list",
        json!({ "workspace_id": "routing" }),
    )
    .await;
    assert_eq!(
        requests[0].target,
        "/v1/applications/routing/versions?pagereturn=true"
    );
    assert_eq!(result[0]["id"], "v1");
}

#[tokio::test]
async fn serves_the_update_tool() {
    let mut updated = build_json();
    updated["name"] = json!("Version 1");
    let (result, requests) = support::serve_tool_ok(
        vec![
            MockResponse::json(200, build_json()),
            MockResponse::json(200, updated.clone()),
        ],
        "decision_platform_build_update",
        json!({ "workspace_id": "routing", "build_id": "v1", "name": "Version 1" }),
    )
    .await;
    assert_eq!(requests[1].method, "PUT");
    assert_eq!(requests[1].target, "/v1/applications/routing/versions/v1");
    assert_eq!(support::body(&requests[1]), updated);
    assert_eq!(result["name"], "Version 1");
}

#[tokio::test]
async fn rejects_an_empty_update_in_the_update_tool() {
    let server = MockServer::start(Vec::new()).await;
    let cx = Context::new().with_platform(support::platform(&server));
    let args = json!({ "workspace_id": "routing", "build_id": "v1" });
    let err = support::call_tool(cx, "decision_platform_build_update", args)
        .await
        .unwrap_err();
    assert!(
        matches!(err, ToolError::Failed(Error::Invalid { .. })),
        "{err}"
    );
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn serves_the_delete_tool() {
    let (result, requests) = support::serve_tool_ok(
        vec![MockResponse::new(204)],
        "decision_platform_build_delete",
        json!({ "workspace_id": "routing", "build_id": "v1" }),
    )
    .await;
    assert_eq!(requests[0].method, "DELETE");
    assert_eq!(requests[0].target, "/v1/applications/routing/versions/v1");
    assert_eq!(result, Value::Null);
}
