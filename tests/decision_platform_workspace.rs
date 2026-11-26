//! Tests for the platform workspace operations and their tools, against the
//! mock server.

mod mock_server;
mod support;

use fico_sdk::Error;
use fico_sdk::decision::platform::{
    WorkspaceCreateParams, WorkspaceDeleteParams, WorkspaceUpdateParams,
};
use fico_sdk::decision::{WorkspaceLink, WorkspaceSettings, WorkspaceType};
use fico_sdk::tool::{Context, ToolError};
use mock_server::{MockResponse, MockServer};
use serde_json::{Value, json};

/// A workspace as the platform returns it, with a field the SDK does not
/// model (`dev_uri`).
fn workspace_json() -> Value {
    json!({
        "id": "routing",
        "name": "Routing",
        "description": "Routes vehicles.",
        "type": "custom",
        "default_instance": "prod",
        "default_experiment_instance": "staging",
        "subscription_id": "",
        "locked": false,
        "active_executable": {
            "id": "routing",
            "user_email": "dev@example.com",
            "uploaded_at": "2026-10-01T11:58:00Z",
            "requirements": {
                "executable_type": "python",
                "runtime": "ghcr.io/nextmv-io/runtime/python:3.11",
                "io_configuration": { "format": "json" },
            },
        },
        "created_at": "2026-10-01T12:00:00Z",
        "updated_at": "2026-10-02T08:30:00Z",
        "settings": {
            "links": [{
                "label": "Docs",
                "url": "https://example.com",
                "description": "",
                "tags": ["docs"],
            }],
        },
        "dev_uri": "",
    })
}

/// [`workspace_json`], locked or unlocked.
fn locked_json(locked: bool) -> Value {
    let mut workspace = workspace_json();
    workspace["locked"] = json!(locked);
    workspace
}

#[tokio::test]
async fn creates_a_workspace() {
    let server = MockServer::start(vec![MockResponse::json(200, workspace_json())]).await;
    let params = WorkspaceCreateParams {
        workspace_id: Some("routing".to_owned()),
        name: Some("Routing".to_owned()),
        description: Some("Routes vehicles.".to_owned()),
        is_workflow: Some(false),
        default_environment_id: Some("prod".to_owned()),
        default_experiment_environment: Some("staging".to_owned()),
        exist_ok: false,
    };
    let workspace = support::platform(&server)
        .create_workspace(params)
        .await
        .unwrap();

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].target, "/v1/applications");
    assert_eq!(
        support::body(&requests[0]),
        json!({
            "id": "routing",
            "name": "Routing",
            "description": "Routes vehicles.",
            "is_pipeline": false,
            "default_instance": "prod",
            "default_experiment_instance": "staging",
        })
    );
    assert_eq!(workspace.workspace_id, "routing");
    assert_eq!(workspace.name.as_deref(), Some("Routing"));
    assert_eq!(workspace.workspace_type, Some(WorkspaceType::Custom));
    assert_eq!(workspace.default_environment_id.as_deref(), Some("prod"));
    assert_eq!(
        workspace.default_experiment_environment.as_deref(),
        Some("staging")
    );
    assert!(!workspace.locked);
    assert_eq!(
        workspace.created_at.as_deref(),
        Some("2026-10-01T12:00:00Z")
    );
}

#[tokio::test]
async fn names_a_new_workspace_after_its_id() {
    let server = MockServer::start(vec![MockResponse::json(200, workspace_json())]).await;
    let params = WorkspaceCreateParams {
        workspace_id: Some("routing".to_owned()),
        name: Some(String::new()),
        ..Default::default()
    };
    support::platform(&server)
        .create_workspace(params)
        .await
        .unwrap();
    let request = &server.requests()[0];
    assert_eq!(
        support::body(request),
        json!({ "id": "routing", "name": "routing" })
    );
}

#[tokio::test]
async fn generates_an_id_when_none_is_set() {
    let server = MockServer::start(vec![MockResponse::json(200, workspace_json())]).await;
    let platform = support::platform(&server);
    platform
        .create_workspace(WorkspaceCreateParams::default())
        .await
        .unwrap();
    let body = support::body(&server.requests()[0]);
    let workspace_id = body["id"].as_str().unwrap();
    let suffix = workspace_id.strip_prefix("workspace-").unwrap();
    assert_eq!(suffix.len(), 8, "{workspace_id}");
    assert!(
        suffix
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()),
        "{workspace_id}"
    );
    assert_eq!(body["name"], workspace_id);
}

#[tokio::test]
async fn returns_the_existing_workspace_with_exist_ok() {
    let server = MockServer::start(vec![MockResponse::json(200, workspace_json())]).await;
    let params = WorkspaceCreateParams {
        workspace_id: Some("routing".to_owned()),
        exist_ok: true,
        ..Default::default()
    };
    let workspace = support::platform(&server)
        .create_workspace(params)
        .await
        .unwrap();
    assert_eq!(workspace.workspace_id, "routing");
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[0].target, "/v1/applications/routing");
}

#[tokio::test]
async fn creates_a_missing_workspace_with_exist_ok() {
    let server = MockServer::start(vec![
        MockResponse::new(404).body("workspace not found"),
        MockResponse::json(200, workspace_json()),
    ])
    .await;
    let params = WorkspaceCreateParams {
        workspace_id: Some("routing".to_owned()),
        exist_ok: true,
        ..Default::default()
    };
    support::platform(&server)
        .create_workspace(params)
        .await
        .unwrap();
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[1].method, "POST");
    assert_eq!(requests[1].target, "/v1/applications");
    assert_eq!(
        support::body(&requests[1]),
        json!({ "id": "routing", "name": "routing" })
    );
}

#[tokio::test]
async fn stops_on_other_errors_with_exist_ok() {
    let server = MockServer::start(vec![MockResponse::new(500).body("boom")]).await;
    let params = WorkspaceCreateParams {
        workspace_id: Some("routing".to_owned()),
        exist_ok: true,
        ..Default::default()
    };
    let err = support::platform(&server)
        .create_workspace(params)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Status { status: 500, .. }), "{err}");
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[0].target, "/v1/applications/routing");
}

#[tokio::test]
async fn needs_an_id_for_exist_ok() {
    let server = MockServer::start(Vec::new()).await;
    for workspace_id in [None, Some(String::new())] {
        let params = WorkspaceCreateParams {
            workspace_id,
            exist_ok: true,
            ..Default::default()
        };
        let err = support::platform(&server)
            .create_workspace(params)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Invalid { .. }), "{err}");
        assert_eq!(
            err.to_string(),
            "invalid argument: `exist_ok` needs a `workspace_id`"
        );
    }
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn gets_a_workspace() {
    let server = MockServer::start(vec![MockResponse::json(200, workspace_json())]).await;
    let workspace = support::platform(&server)
        .get_workspace("routing")
        .await
        .unwrap();
    let request = &server.requests()[0];
    assert_eq!(request.method, "GET");
    assert_eq!(request.target, "/v1/applications/routing");
    assert_eq!(request.header("authorization"), Some("Bearer secret"));
    assert_eq!(workspace.description.as_deref(), Some("Routes vehicles."));
    assert_eq!(workspace.subscription_id.as_deref(), Some(""));
    let executable = workspace.active_executable.as_ref().unwrap();
    assert_eq!(executable.requirements.executable_type, "python");
    let links = &workspace.settings.as_ref().unwrap().links;
    assert_eq!(links[0].label, "Docs");
    assert_eq!(links[0].tags, ["docs"]);
    assert_eq!(workspace.deletion_status, None);
    // Emitted JSON uses the API names, and leaves out what the SDK does not
    // model.
    let mut expected = workspace_json();
    expected.as_object_mut().unwrap().remove("dev_uri");
    assert_eq!(serde_json::to_value(&workspace).unwrap(), expected);
}

#[tokio::test]
async fn reads_the_deletion_status() {
    let mut deleting = workspace_json();
    deleting["deletion_status"] = json!("App deletion in progress");
    let server = MockServer::start(vec![MockResponse::json(200, deleting)]).await;
    let workspace = support::platform(&server)
        .get_workspace("routing")
        .await
        .unwrap();
    assert_eq!(
        workspace.deletion_status.as_deref(),
        Some("App deletion in progress")
    );
}

#[tokio::test]
async fn decodes_a_minimal_workspace() {
    let server = MockServer::start(vec![MockResponse::json(
        200,
        json!({ "id": "routing", "type": "new-kind" }),
    )])
    .await;
    let workspace = support::platform(&server)
        .get_workspace("routing")
        .await
        .unwrap();
    assert_eq!(workspace.workspace_type, Some(WorkspaceType::Unknown));
    assert_eq!(workspace.name, None);
    assert!(!workspace.locked);
}

#[tokio::test]
async fn checks_whether_a_workspace_exists() {
    let server = MockServer::start(vec![
        MockResponse::json(200, workspace_json()),
        MockResponse::new(404).body("workspace not found"),
        MockResponse::new(500).body("boom"),
    ])
    .await;
    let platform = support::platform(&server);
    assert!(platform.workspace_exists("routing").await.unwrap());
    assert!(!platform.workspace_exists("missing").await.unwrap());
    let err = platform.workspace_exists("routing").await.unwrap_err();
    assert!(matches!(err, Error::Status { status: 500, .. }), "{err}");
    let targets: Vec<String> = server.requests().into_iter().map(|r| r.target).collect();
    assert_eq!(
        targets,
        [
            "/v1/applications/routing",
            "/v1/applications/missing",
            "/v1/applications/routing",
        ]
    );
}

#[tokio::test]
async fn lists_workspaces_across_pages() {
    let mut second = workspace_json();
    second["id"] = json!("scheduling");
    let server = MockServer::start(vec![
        MockResponse::json(
            200,
            json!({ "items": [workspace_json()], "next_page_token": "page-2" }),
        ),
        MockResponse::json(200, json!({ "items": [second] })),
    ])
    .await;
    let workspaces = support::platform(&server).list_workspaces().await.unwrap();
    let workspace_ids: Vec<&str> = workspaces.iter().map(|w| w.workspace_id.as_str()).collect();
    assert_eq!(workspace_ids, ["routing", "scheduling"]);

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert_eq!(request.method, "GET");
        assert!(request.target.starts_with("/v1/applications?"));
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
    let mut updated = workspace_json();
    updated["name"] = json!("Fleet Routing");
    updated["default_instance"] = json!("canary");
    let server = MockServer::start(vec![
        MockResponse::json(200, workspace_json()),
        MockResponse::json(200, updated.clone()),
    ])
    .await;
    let params = WorkspaceUpdateParams {
        name: Some("Fleet Routing".to_owned()),
        default_environment_id: Some("canary".to_owned()),
        ..Default::default()
    };
    let workspace = support::platform(&server)
        .update_workspace("routing", params)
        .await
        .unwrap();

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[0].target, "/v1/applications/routing");
    assert_eq!(requests[1].method, "PUT");
    assert_eq!(requests[1].target, "/v1/applications/routing");
    // Everything else, `settings` included, goes back as it came.
    assert_eq!(support::body(&requests[1]), updated);
    assert_eq!(workspace.name.as_deref(), Some("Fleet Routing"));
    assert_eq!(workspace.default_environment_id.as_deref(), Some("canary"));
}

#[tokio::test]
async fn replaces_the_settings() {
    let mut updated = workspace_json();
    updated["settings"] = json!({
        "links": [{ "label": "Runbook", "url": "https://example.com/runbook", "description": "On call" }],
    });
    let mut cleared = workspace_json();
    cleared["settings"] = json!({ "links": [] });
    let server = MockServer::start(vec![
        MockResponse::json(200, workspace_json()),
        MockResponse::json(200, updated.clone()),
        MockResponse::json(200, workspace_json()),
        MockResponse::json(200, cleared.clone()),
    ])
    .await;
    let platform = support::platform(&server);
    let settings = WorkspaceSettings {
        links: vec![WorkspaceLink {
            label: "Runbook".to_owned(),
            url: "https://example.com/runbook".to_owned(),
            description: "On call".to_owned(),
            ..Default::default()
        }],
    };
    let params = WorkspaceUpdateParams {
        settings: Some(settings),
        ..Default::default()
    };
    let workspace = platform.update_workspace("routing", params).await.unwrap();
    assert_eq!(workspace.settings.unwrap().links[0].label, "Runbook");
    // An empty list of links removes them all.
    let params = WorkspaceUpdateParams {
        settings: Some(WorkspaceSettings::default()),
        ..Default::default()
    };
    platform.update_workspace("routing", params).await.unwrap();

    let requests = server.requests();
    assert_eq!(requests.len(), 4);
    assert_eq!(support::body(&requests[1]), updated);
    assert_eq!(support::body(&requests[3]), cleared);
}

#[tokio::test]
async fn locks_and_unlocks_without_other_changes() {
    let server = MockServer::start(vec![
        MockResponse::json(200, locked_json(true)),
        MockResponse::json(200, locked_json(false)),
    ])
    .await;
    let platform = support::platform(&server);
    for locked in [true, false] {
        let params = WorkspaceUpdateParams {
            locked: Some(locked),
            ..Default::default()
        };
        let workspace = platform.update_workspace("routing", params).await.unwrap();
        assert_eq!(workspace.locked, locked);
    }
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    for (request, locked) in requests.iter().zip([true, false]) {
        assert_eq!(request.method, "PUT");
        assert_eq!(request.target, "/v1/applications/routing/lock");
        assert_eq!(support::body(request), json!({ "locked": locked }));
    }
}

#[tokio::test]
async fn locks_a_workspace_again_after_an_update() {
    // The platform unlocks a workspace that it updates.
    let mut updated = locked_json(false);
    updated["name"] = json!("Fleet Routing");
    let mut relocked = updated.clone();
    relocked["locked"] = json!(true);
    let server = MockServer::start(vec![
        MockResponse::json(200, locked_json(true)),
        MockResponse::json(200, updated),
        MockResponse::json(200, relocked),
    ])
    .await;
    let params = WorkspaceUpdateParams {
        name: Some("Fleet Routing".to_owned()),
        ..Default::default()
    };
    let workspace = support::platform(&server)
        .update_workspace("routing", params)
        .await
        .unwrap();
    assert!(workspace.locked);
    assert_eq!(workspace.name.as_deref(), Some("Fleet Routing"));
    let requests = server.requests();
    let calls: Vec<_> = requests
        .iter()
        .map(|r| (r.method.as_str(), r.target.as_str()))
        .collect();
    assert_eq!(
        calls,
        [
            ("GET", "/v1/applications/routing"),
            ("PUT", "/v1/applications/routing"),
            ("PUT", "/v1/applications/routing/lock"),
        ]
    );
    assert_eq!(support::body(&requests[2]), json!({ "locked": true }));
}

#[tokio::test]
async fn returns_the_lock_error_after_an_update() {
    let server = MockServer::start(vec![
        MockResponse::json(200, locked_json(true)),
        MockResponse::json(200, locked_json(false)),
        MockResponse::new(500).body("lock failed"),
    ])
    .await;
    let params = WorkspaceUpdateParams {
        name: Some("Fleet Routing".to_owned()),
        ..Default::default()
    };
    let err = support::platform(&server)
        .update_workspace("routing", params)
        .await
        .unwrap_err();
    // The PUT applied the update; only the lock failed.
    assert!(matches!(err, Error::Status { status: 500, .. }), "{err}");
    assert!(
        err.to_string().contains("/v1/applications/routing/lock"),
        "{err}"
    );
    assert_eq!(server.requests().len(), 3);
}

#[tokio::test]
async fn unlocks_with_an_update() {
    let mut updated = locked_json(false);
    updated["name"] = json!("Fleet Routing");
    let server = MockServer::start(vec![
        MockResponse::json(200, locked_json(true)),
        MockResponse::json(200, updated),
    ])
    .await;
    let params = WorkspaceUpdateParams {
        name: Some("Fleet Routing".to_owned()),
        locked: Some(false),
        ..Default::default()
    };
    let workspace = support::platform(&server)
        .update_workspace("routing", params)
        .await
        .unwrap();
    assert!(!workspace.locked);
    // The PUT already unlocked it: no lock request.
    assert_eq!(server.requests().len(), 2);
}

#[tokio::test]
async fn locks_with_an_update() {
    let mut updated = workspace_json();
    updated["name"] = json!("Fleet Routing");
    let mut locked = updated.clone();
    locked["locked"] = json!(true);
    let server = MockServer::start(vec![
        MockResponse::json(200, workspace_json()),
        MockResponse::json(200, updated),
        MockResponse::json(200, locked),
    ])
    .await;
    let params = WorkspaceUpdateParams {
        name: Some("Fleet Routing".to_owned()),
        locked: Some(true),
        ..Default::default()
    };
    let workspace = support::platform(&server)
        .update_workspace("routing", params)
        .await
        .unwrap();
    assert!(workspace.locked);
    let requests = server.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[2].target, "/v1/applications/routing/lock");
    assert_eq!(support::body(&requests[2]), json!({ "locked": true }));
}

#[tokio::test]
async fn reports_a_workspace_that_is_not_an_object_on_update() {
    let server = MockServer::start(vec![MockResponse::json(200, json!(["routing"]))]).await;
    let params = WorkspaceUpdateParams {
        name: Some("Fleet Routing".to_owned()),
        ..Default::default()
    };
    let err = support::platform(&server)
        .update_workspace("routing", params)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Decode { .. }), "{err}");
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn rejects_default_as_a_default_environment() {
    let server = MockServer::start(Vec::new()).await;
    let platform = support::platform(&server);
    let creates = [
        WorkspaceCreateParams {
            default_environment_id: Some("default".to_owned()),
            ..Default::default()
        },
        WorkspaceCreateParams {
            default_experiment_environment: Some("default".to_owned()),
            ..Default::default()
        },
    ];
    for params in creates {
        let err = platform.create_workspace(params).await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "invalid argument: `default` is reserved: it names the workspace's default environment"
        );
    }
    let updates = [
        WorkspaceUpdateParams {
            default_environment_id: Some("default".to_owned()),
            ..Default::default()
        },
        WorkspaceUpdateParams {
            default_experiment_environment: Some("default".to_owned()),
            ..Default::default()
        },
    ];
    for params in updates {
        let err = platform
            .update_workspace("routing", params)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Invalid { .. }), "{err}");
        assert_eq!(
            err.to_string(),
            "invalid argument: `default` is reserved: it names the workspace's default environment"
        );
    }
    // Before any request.
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn rejects_an_empty_update() {
    let server = MockServer::start(Vec::new()).await;
    let err = support::platform(&server)
        .update_workspace("routing", WorkspaceUpdateParams::default())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Invalid { .. }), "{err}");
    assert_eq!(
        err.to_string(),
        "invalid argument: nothing to update: set `name`, `description`, \
         `default_environment_id`, `default_experiment_environment`, `settings` or `locked`"
    );
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn sends_an_empty_string_as_a_change() {
    let mut updated = workspace_json();
    updated["description"] = json!("");
    let server = MockServer::start(vec![
        MockResponse::json(200, workspace_json()),
        MockResponse::json(200, updated.clone()),
    ])
    .await;
    let params = WorkspaceUpdateParams {
        description: Some(String::new()),
        ..Default::default()
    };
    let workspace = support::platform(&server)
        .update_workspace("routing", params)
        .await
        .unwrap();

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].method, "PUT");
    assert_eq!(support::body(&requests[1]), updated);
    assert_eq!(workspace.description.as_deref(), Some(""));
}

#[tokio::test]
async fn deletes_a_workspace() {
    let server = MockServer::start(vec![MockResponse::new(204)]).await;
    support::platform(&server)
        .delete_workspace("routing", Default::default())
        .await
        .unwrap();
    let request = &server.requests()[0];
    assert_eq!(request.method, "DELETE");
    assert_eq!(request.target, "/v1/applications/routing");
}

#[tokio::test]
async fn polls_a_delete_until_the_workspace_is_gone() {
    let mut deleting = workspace_json();
    deleting["deletion_status"] = json!("App deletion in progress");
    let server = MockServer::start(vec![
        MockResponse::new(204),
        MockResponse::json(200, deleting),
        MockResponse::new(404).body("app not found"),
    ])
    .await;
    let params = WorkspaceDeleteParams {
        polling: Some(support::fast_polling()),
    };
    support::platform(&server)
        .delete_workspace("routing", params)
        .await
        .unwrap();
    let calls: Vec<_> = server
        .requests()
        .into_iter()
        .map(|r| format!("{} {}", r.method, r.target))
        .collect();
    assert_eq!(
        calls,
        [
            "DELETE /v1/applications/routing",
            "GET /v1/applications/routing",
            "GET /v1/applications/routing",
        ]
    );
}

#[tokio::test]
async fn ends_the_polling_of_a_failed_workspace_delete_with_an_error() {
    let mut failed = workspace_json();
    failed["deletion_status"] = json!("App deletion failed");
    let server = MockServer::start(vec![
        MockResponse::new(204),
        MockResponse::json(200, failed),
    ])
    .await;
    let params = WorkspaceDeleteParams {
        polling: Some(support::fast_polling()),
    };
    let err = support::platform(&server)
        .delete_workspace("routing", params)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Wait { .. }), "{err}");
    assert_eq!(
        err.to_string(),
        "stopped waiting: the platform could not delete the workspace: App deletion failed"
    );
}

#[tokio::test]
async fn serves_the_delete_tool_with_polling() {
    let (_, requests) = support::serve_tool_ok(
        vec![
            MockResponse::new(204),
            MockResponse::new(404).body("app not found"),
        ],
        "decision_platform_workspace_delete",
        json!({
            "workspace_id": "routing",
            "polling": { "initial_delay": 0, "jitter": 0 },
        }),
    )
    .await;
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].method, "GET");
}

#[tokio::test]
async fn encodes_workspace_ids_in_paths() {
    let server = MockServer::start(vec![MockResponse::json(200, workspace_json())]).await;
    let platform = support::platform(&server);
    platform.get_workspace("a#b c?d").await.unwrap();
    assert_eq!(
        server.requests()[0].target,
        "/v1/applications/a%23b%20c%3Fd"
    );
    for workspace_id in ["", ".", "..", "a/b"] {
        let err = platform
            .delete_workspace(workspace_id, Default::default())
            .await
            .unwrap_err();
        assert!(
            matches!(err, Error::Invalid { .. }),
            "{workspace_id:?}: {err}"
        );
    }
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn serves_the_create_tool() {
    let (result, requests) = support::serve_tool_ok(
        vec![MockResponse::json(200, workspace_json())],
        "decision_platform_workspace_create",
        json!({ "workspace_id": "routing", "name": "Routing", "is_workflow": true }),
    )
    .await;
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].target, "/v1/applications");
    // The `workspace_id` argument is the API's `id`, in the body and in the
    // result.
    assert_eq!(
        support::body(&requests[0]),
        json!({ "id": "routing", "name": "Routing", "is_pipeline": true })
    );
    assert_eq!(result["id"], "routing");
    assert_eq!(result["default_instance"], "prod");
}

#[tokio::test]
async fn rejects_an_id_argument_in_the_create_tool() {
    let server = MockServer::start(Vec::new()).await;
    let cx = Context::new().with_platform(support::platform(&server));
    let args = json!({ "id": "routing" });
    let err = support::call_tool(cx, "decision_platform_workspace_create", args)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ToolError::InvalidArguments(m) if m.starts_with("unknown field `id`")),
        "{err}"
    );
    assert!(server.requests().is_empty());
}

#[test]
fn does_not_read_id_as_the_workspace_id() {
    // `Tool::call` checks arguments against the schema, which leaves out
    // `#[serde(alias)]`, so only serde shows that `id` is no alias.
    let params: WorkspaceCreateParams = serde_json::from_value(json!({ "id": "routing" })).unwrap();
    assert_eq!(params.workspace_id, None);
}

#[tokio::test]
async fn serves_the_get_tool() {
    let (result, requests) = support::serve_tool_ok(
        vec![MockResponse::json(200, workspace_json())],
        "decision_platform_workspace_get",
        json!({ "workspace_id": "routing" }),
    )
    .await;
    assert_eq!(requests[0].target, "/v1/applications/routing");
    assert_eq!(result["type"], "custom");
}

#[tokio::test]
async fn serves_the_exists_tool() {
    let (result, requests) = support::serve_tool_ok(
        vec![MockResponse::new(404)],
        "decision_platform_workspace_exists",
        json!({ "workspace_id": "missing" }),
    )
    .await;
    assert_eq!(requests[0].target, "/v1/applications/missing");
    assert_eq!(result, json!(false));
}

#[tokio::test]
async fn serves_the_list_tool() {
    let (result, requests) = support::serve_tool_ok(
        vec![MockResponse::json(
            200,
            json!({ "items": [workspace_json()] }),
        )],
        "decision_platform_workspace_list",
        json!({}),
    )
    .await;
    assert_eq!(requests[0].target, "/v1/applications?pagereturn=true");
    assert_eq!(result[0]["id"], "routing");
}

#[tokio::test]
async fn serves_the_update_tool() {
    let mut updated = workspace_json();
    updated["description"] = json!("Routes trucks.");
    let (result, requests) = support::serve_tool_ok(
        vec![
            MockResponse::json(200, workspace_json()),
            MockResponse::json(200, updated.clone()),
        ],
        "decision_platform_workspace_update",
        json!({ "workspace_id": "routing", "description": "Routes trucks." }),
    )
    .await;
    assert_eq!(requests[1].method, "PUT");
    assert_eq!(requests[1].target, "/v1/applications/routing");
    assert_eq!(support::body(&requests[1]), updated);
    assert_eq!(result["description"], "Routes trucks.");
}

#[tokio::test]
async fn serves_the_update_tool_with_settings_and_locked() {
    let mut updated = workspace_json();
    updated["settings"] = json!({ "links": [{ "label": "Docs", "url": "https://example.com/docs", "description": "" }] });
    let mut locked = updated.clone();
    locked["locked"] = json!(true);
    let (result, requests) = support::serve_tool_ok(
        vec![
            MockResponse::json(200, workspace_json()),
            MockResponse::json(200, updated.clone()),
            MockResponse::json(200, locked),
        ],
        "decision_platform_workspace_update",
        json!({
            "workspace_id": "routing",
            "settings": { "links": [{ "label": "Docs", "url": "https://example.com/docs" }] },
            "locked": true,
        }),
    )
    .await;
    assert_eq!(support::body(&requests[1]), updated);
    assert_eq!(requests[2].target, "/v1/applications/routing/lock");
    assert_eq!(result["locked"], true);
    assert_eq!(
        result["settings"]["links"][0]["url"],
        "https://example.com/docs"
    );
}

#[tokio::test]
async fn rejects_an_empty_update_in_the_update_tool() {
    let server = MockServer::start(Vec::new()).await;
    let cx = Context::new().with_platform(support::platform(&server));
    let args = json!({ "workspace_id": "routing" });
    let err = support::call_tool(cx, "decision_platform_workspace_update", args)
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
        "decision_platform_workspace_delete",
        json!({ "workspace_id": "routing" }),
    )
    .await;
    assert_eq!(requests[0].method, "DELETE");
    assert_eq!(requests[0].target, "/v1/applications/routing");
    assert_eq!(result, Value::Null);
}
