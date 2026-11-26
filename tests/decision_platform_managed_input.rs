//! Tests for the platform managed input operations and their tools, against
//! the mock server.

mod mock_server;
mod support;

use fico_sdk::Error;
use fico_sdk::decision::ContentFormat;
use fico_sdk::decision::platform::{ManagedInputCreateParams, ManagedInputUpdateParams};
use fico_sdk::tool::{Context, ToolError};
use mock_server::{MockResponse, MockServer};
use serde_json::{Value, json};

/// A managed input from an upload, as the platform returns it, with a field
/// the SDK does not model (`app_id`).
fn managed_input_json() -> Value {
    json!({
        "id": "monday",
        "name": "Monday",
        "description": "Orders of a Monday.",
        "app_id": "routing",
        "run_id": "",
        "upload_id": "0d7ab3fa-6c2d-4b8e-9a51-2f3c4d5e6f70",
        "format": { "input": { "type": "json" } },
        "created_at": "2026-10-01T12:00:00Z",
        "updated_at": "2026-10-01T12:00:00Z",
    })
}

/// A managed input from a run's multi-file input, as the platform returns it.
fn run_managed_input_json() -> Value {
    json!({
        "id": "tuesday",
        "name": "tuesday",
        "description": "",
        "app_id": "",
        "run_id": "run-1",
        "upload_id": "",
        "format": { "input": { "type": "multi-file" } },
        "created_at": "2026-10-02T12:00:00Z",
        "updated_at": "2026-10-02T12:00:00Z",
    })
}

#[tokio::test]
async fn creates_a_managed_input_from_an_upload() {
    let server = MockServer::start(vec![MockResponse::json(201, managed_input_json())]).await;
    let params = ManagedInputCreateParams {
        managed_input_id: Some("monday".to_owned()),
        name: Some("Monday".to_owned()),
        description: Some("Orders of a Monday.".to_owned()),
        upload_id: Some("0d7ab3fa-6c2d-4b8e-9a51-2f3c4d5e6f70".to_owned()),
        ..Default::default()
    };
    let managed_input = support::platform(&server)
        .create_managed_input("routing", params)
        .await
        .unwrap();

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].target, "/v1/applications/routing/inputs");
    // The format defaults to JSON, for the input and the output.
    assert_eq!(
        support::body(&requests[0]),
        json!({
            "id": "monday",
            "name": "Monday",
            "description": "Orders of a Monday.",
            "upload_id": "0d7ab3fa-6c2d-4b8e-9a51-2f3c4d5e6f70",
            "format": { "input": { "type": "json" }, "output": { "type": "json" } },
        })
    );
    assert_eq!(managed_input.managed_input_id, "monday");
    assert_eq!(
        managed_input.upload_id.as_deref(),
        Some("0d7ab3fa-6c2d-4b8e-9a51-2f3c4d5e6f70")
    );
    let format = managed_input.format.unwrap();
    assert_eq!(format.format_input.input_type, ContentFormat::Json);
    assert_eq!(format.format_output, None);
}

#[tokio::test]
async fn creates_a_managed_input_from_a_run() {
    let server = MockServer::start(vec![MockResponse::json(201, run_managed_input_json())]).await;
    let params = ManagedInputCreateParams {
        managed_input_id: Some("tuesday".to_owned()),
        run_id: Some("run-1".to_owned()),
        content_format: Some(ContentFormat::MultiFile),
        ..Default::default()
    };
    let managed_input = support::platform(&server)
        .create_managed_input("routing", params)
        .await
        .unwrap();
    assert_eq!(
        support::body(&server.requests()[0]),
        json!({
            "id": "tuesday",
            "name": "tuesday",
            "run_id": "run-1",
            "format": { "input": { "type": "multi-file" }, "output": { "type": "multi-file" } },
        })
    );
    assert_eq!(managed_input.run_id.as_deref(), Some("run-1"));
    assert_eq!(
        managed_input.format.unwrap().format_input.input_type,
        ContentFormat::MultiFile
    );
}

#[tokio::test]
async fn generates_an_id_when_none_is_set() {
    let server = MockServer::start(vec![MockResponse::json(201, managed_input_json())]).await;
    let params = ManagedInputCreateParams {
        name: Some(String::new()),
        upload_id: Some("0d7ab3fa-6c2d-4b8e-9a51-2f3c4d5e6f70".to_owned()),
        ..Default::default()
    };
    support::platform(&server)
        .create_managed_input("routing", params)
        .await
        .unwrap();
    let body = support::body(&server.requests()[0]);
    let managed_input_id = body["id"].as_str().unwrap();
    let suffix = managed_input_id.strip_prefix("managed-input-").unwrap();
    assert_eq!(suffix.len(), 8, "{managed_input_id}");
    assert!(
        suffix
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()),
        "{managed_input_id}"
    );
    assert_eq!(body["name"], managed_input_id);
}

#[tokio::test]
async fn rejects_an_unknown_content_format() {
    let server = MockServer::start(Vec::new()).await;
    let params = ManagedInputCreateParams {
        upload_id: Some("0d7ab3fa-6c2d-4b8e-9a51-2f3c4d5e6f70".to_owned()),
        content_format: Some(ContentFormat::Unknown),
        ..Default::default()
    };
    let err = support::platform(&server)
        .create_managed_input("routing", params)
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "invalid argument: `content_format` must be `json` or `multi-file`"
    );
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn needs_exactly_one_source() {
    let server = MockServer::start(Vec::new()).await;
    let platform = support::platform(&server);
    let source = |upload_id: Option<&str>, run_id: Option<&str>| ManagedInputCreateParams {
        managed_input_id: Some("monday".to_owned()),
        upload_id: upload_id.map(str::to_owned),
        run_id: run_id.map(str::to_owned),
        ..Default::default()
    };
    for params in [
        source(None, None),
        source(Some(""), Some("")),
        source(Some("0d7ab3fa-6c2d-4b8e-9a51-2f3c4d5e6f70"), Some("run-1")),
    ] {
        let err = platform
            .create_managed_input("routing", params)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Invalid { .. }), "{err}");
        assert_eq!(
            err.to_string(),
            "invalid argument: a managed input needs exactly one of `upload_id` and `run_id`"
        );
    }
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn leaves_out_an_empty_source() {
    let server = MockServer::start(vec![MockResponse::json(201, run_managed_input_json())]).await;
    let params = ManagedInputCreateParams {
        managed_input_id: Some("tuesday".to_owned()),
        upload_id: Some(String::new()),
        run_id: Some("run-1".to_owned()),
        ..Default::default()
    };
    support::platform(&server)
        .create_managed_input("routing", params)
        .await
        .unwrap();
    let body = support::body(&server.requests()[0]);
    assert_eq!(body["run_id"], "run-1");
    assert!(body.get("upload_id").is_none(), "{body}");
}

#[tokio::test]
async fn gets_a_managed_input() {
    let server = MockServer::start(vec![MockResponse::json(200, managed_input_json())]).await;
    let managed_input = support::platform(&server)
        .get_managed_input("routing", "monday")
        .await
        .unwrap();
    let request = &server.requests()[0];
    assert_eq!(request.method, "GET");
    assert_eq!(request.target, "/v1/applications/routing/inputs/monday");
    assert_eq!(managed_input.name.as_deref(), Some("Monday"));
    assert_eq!(managed_input.run_id.as_deref(), Some(""));
    assert_eq!(
        managed_input.created_at.as_deref(),
        Some("2026-10-01T12:00:00Z")
    );
    // Emitted JSON uses the API names, and leaves out what the SDK does not
    // model.
    let mut expected = managed_input_json();
    expected.as_object_mut().unwrap().remove("app_id");
    assert_eq!(serde_json::to_value(&managed_input).unwrap(), expected);
}

#[tokio::test]
async fn lists_managed_inputs_across_pages() {
    let server = MockServer::start(vec![
        MockResponse::json(
            200,
            json!({ "items": [managed_input_json()], "next_page_token": "page-2" }),
        ),
        MockResponse::json(200, json!({ "items": [run_managed_input_json()] })),
    ])
    .await;
    let managed_inputs = support::platform(&server)
        .list_managed_inputs("routing")
        .await
        .unwrap();
    let ids: Vec<&str> = managed_inputs
        .iter()
        .map(|m| m.managed_input_id.as_str())
        .collect();
    assert_eq!(ids, ["monday", "tuesday"]);

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert_eq!(request.method, "GET");
        assert!(
            request
                .target
                .starts_with("/v1/applications/routing/inputs?")
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
    let mut updated = managed_input_json();
    updated["name"] = json!("First Monday");
    let server = MockServer::start(vec![
        MockResponse::json(200, managed_input_json()),
        MockResponse::json(200, updated.clone()),
    ])
    .await;
    let params = ManagedInputUpdateParams {
        name: Some("First Monday".to_owned()),
        ..Default::default()
    };
    let managed_input = support::platform(&server)
        .update_managed_input("routing", "monday", params)
        .await
        .unwrap();

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[0].target, "/v1/applications/routing/inputs/monday");
    assert_eq!(requests[1].method, "PUT");
    assert_eq!(requests[1].target, "/v1/applications/routing/inputs/monday");
    // Everything else, `app_id` included, goes back as it came.
    assert_eq!(support::body(&requests[1]), updated);
    assert_eq!(managed_input.name.as_deref(), Some("First Monday"));
    assert_eq!(
        managed_input.description.as_deref(),
        Some("Orders of a Monday.")
    );
}

#[tokio::test]
async fn rejects_an_empty_update() {
    let server = MockServer::start(Vec::new()).await;
    let err = support::platform(&server)
        .update_managed_input("routing", "monday", ManagedInputUpdateParams::default())
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
async fn deletes_a_managed_input() {
    let server = MockServer::start(vec![MockResponse::new(204)]).await;
    support::platform(&server)
        .delete_managed_input("routing", "monday")
        .await
        .unwrap();
    let request = &server.requests()[0];
    assert_eq!(request.method, "DELETE");
    assert_eq!(request.target, "/v1/applications/routing/inputs/monday");
}

#[tokio::test]
async fn encodes_managed_input_ids_in_paths() {
    let server = MockServer::start(vec![MockResponse::json(200, managed_input_json())]).await;
    let platform = support::platform(&server);
    platform
        .get_managed_input("my workspace", "mon?day")
        .await
        .unwrap();
    assert_eq!(
        server.requests()[0].target,
        "/v1/applications/my%20workspace/inputs/mon%3Fday"
    );
    for (workspace_id, managed_input_id) in [
        ("routing", ".."),
        ("..", "monday"),
        ("routing", ""),
        ("routing", "mon/day"),
    ] {
        let err = platform
            .delete_managed_input(workspace_id, managed_input_id)
            .await
            .unwrap_err();
        assert!(
            matches!(err, Error::Invalid { .. }),
            "{managed_input_id:?}: {err}"
        );
    }
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn serves_the_create_tool() {
    let (result, requests) = support::serve_tool_ok(
        vec![MockResponse::json(201, run_managed_input_json())],
        "decision_platform_managed_input_create",
        json!({
            "workspace_id": "routing",
            "managed_input_id": "tuesday",
            "run_id": "run-1",
            "content_format": "multi-file",
        }),
    )
    .await;
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].target, "/v1/applications/routing/inputs");
    assert_eq!(
        support::body(&requests[0]),
        json!({
            "id": "tuesday",
            "name": "tuesday",
            "run_id": "run-1",
            "format": { "input": { "type": "multi-file" }, "output": { "type": "multi-file" } },
        })
    );
    assert_eq!(result["format"]["input"]["type"], "multi-file");
}

#[tokio::test]
async fn rejects_unknown_content_formats_in_the_create_tool() {
    let server = MockServer::start(Vec::new()).await;
    let cx = Context::new().with_platform(support::platform(&server));
    let args = json!({ "workspace_id": "routing", "run_id": "run-1", "content_format": "csv" });
    let err = support::call_tool(cx, "decision_platform_managed_input_create", args)
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::InvalidArguments(_)), "{err}");
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn serves_the_get_tool() {
    let (result, requests) = support::serve_tool_ok(
        vec![MockResponse::json(200, managed_input_json())],
        "decision_platform_managed_input_get",
        json!({ "workspace_id": "routing", "managed_input_id": "monday" }),
    )
    .await;
    assert_eq!(requests[0].target, "/v1/applications/routing/inputs/monday");
    assert_eq!(result["upload_id"], "0d7ab3fa-6c2d-4b8e-9a51-2f3c4d5e6f70");
}

#[tokio::test]
async fn serves_the_list_tool() {
    let (result, requests) = support::serve_tool_ok(
        vec![MockResponse::json(
            200,
            json!({ "items": [managed_input_json()] }),
        )],
        "decision_platform_managed_input_list",
        json!({ "workspace_id": "routing" }),
    )
    .await;
    assert_eq!(
        requests[0].target,
        "/v1/applications/routing/inputs?pagereturn=true"
    );
    assert_eq!(result[0]["id"], "monday");
}

#[tokio::test]
async fn serves_the_update_tool() {
    let mut updated = managed_input_json();
    updated["description"] = json!("");
    let (result, requests) = support::serve_tool_ok(
        vec![
            MockResponse::json(200, managed_input_json()),
            MockResponse::json(200, updated.clone()),
        ],
        "decision_platform_managed_input_update",
        json!({ "workspace_id": "routing", "managed_input_id": "monday", "description": "" }),
    )
    .await;
    assert_eq!(requests[1].method, "PUT");
    assert_eq!(requests[1].target, "/v1/applications/routing/inputs/monday");
    assert_eq!(support::body(&requests[1]), updated);
    assert_eq!(result["description"], "");
}

#[tokio::test]
async fn serves_the_delete_tool() {
    let (result, requests) = support::serve_tool_ok(
        vec![MockResponse::new(204)],
        "decision_platform_managed_input_delete",
        json!({ "workspace_id": "routing", "managed_input_id": "monday" }),
    )
    .await;
    assert_eq!(requests[0].method, "DELETE");
    assert_eq!(requests[0].target, "/v1/applications/routing/inputs/monday");
    assert_eq!(result, Value::Null);
}
