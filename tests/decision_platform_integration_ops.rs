//! Tests for the platform integration operations and their tools, against
//! the mock server.

mod mock_server;
mod support;

use fico_sdk::Error;
use fico_sdk::decision::platform::{IntegrationCreateParams, IntegrationUpdateParams};
use fico_sdk::decision::{
    ExecutableType, IntegrationHealthStatus, IntegrationProvider, IntegrationType,
};
use fico_sdk::tool::ToolError;
use mock_server::{MockResponse, MockServer, RecordedRequest};
use serde_json::{Value, json};

const INTEGRATIONS: &str = "/v1/integrations";
const INTEGRATION: &str = "/v1/integrations/dbx-1";

/// A Databricks configuration that is only well formed.
fn config_json() -> Value {
    json!({
        "job_id": 123,
        "client_id": "client",
        "client_secret": "not-a-secret", // pragma: allowlist secret
        "workspace_url": "https://dbc.example.com",
    })
}

/// An integration as the platform's get returns it, with a field the SDK
/// does not model (`owner`).
fn integration_json() -> Value {
    json!({
        "id": "dbx-1",
        "name": "Databricks",
        "description": "Runs on Databricks.",
        "global": false,
        "application_ids": ["routing"],
        "type": "compute",
        "exec_types": ["python"],
        "provider": "dbx",
        "provider_config": config_json(),
        "created_at": "2026-10-30T10:00:00Z",
        "updated_at": "2026-10-30T10:00:00Z",
        "owner": "ana@example.com",
    })
}

/// [`integration_json`] as a list has it: without its `provider_config`.
fn summary_json() -> Value {
    let mut summary = integration_json();
    summary.as_object_mut().unwrap().remove("provider_config");
    summary
}

fn create_params() -> IntegrationCreateParams {
    IntegrationCreateParams {
        integration_id: Some("dbx-1".to_owned()),
        name: Some("Databricks".to_owned()),
        description: Some("Runs on Databricks.".to_owned()),
        workspace_ids: vec!["routing".to_owned()],
        integration_type: IntegrationType::Compute,
        executable_types: vec![ExecutableType::Python],
        provider: IntegrationProvider::Dbx,
        provider_config: config_json(),
        ..Default::default()
    }
}

/// The method and target of each request.
fn sent(requests: &[RecordedRequest]) -> Vec<String> {
    requests
        .iter()
        .map(|r| format!("{} {}", r.method, r.target))
        .collect()
}

#[tokio::test]
async fn creates_an_integration() {
    let server = MockServer::start(vec![MockResponse::json(201, integration_json())]).await;
    let integration = support::platform(&server)
        .create_integration(create_params())
        .await
        .unwrap();
    let requests = server.requests();
    assert_eq!(sent(&requests), [format!("POST {INTEGRATIONS}")]);
    assert_eq!(
        support::body(&requests[0]),
        json!({
            "id": "dbx-1",
            "name": "Databricks",
            "description": "Runs on Databricks.",
            "global": false,
            "application_ids": ["routing"],
            "type": "compute",
            "exec_types": ["python"],
            "provider": "dbx",
            "provider_config": config_json(),
        })
    );
    assert_eq!(integration.integration_id, "dbx-1");
    assert_eq!(integration.integration_type, IntegrationType::Compute);
    assert_eq!(integration.provider, IntegrationProvider::Dbx);
    assert_eq!(integration.executable_types, [ExecutableType::Python]);
    assert_eq!(
        integration.workspace_ids.as_deref(),
        Some(&["routing".to_owned()][..])
    );
    assert_eq!(integration.provider_config, Some(config_json()));
}

#[tokio::test]
async fn generates_an_id_and_names_it_after_it() {
    let server = MockServer::start(vec![MockResponse::json(201, integration_json())]).await;
    let params = IntegrationCreateParams {
        integration_id: None,
        name: Some(String::new()),
        ..create_params()
    };
    support::platform(&server)
        .create_integration(params)
        .await
        .unwrap();
    let body = support::body(&server.requests()[0]);
    let id = body["id"].as_str().unwrap();
    assert!(id.starts_with("integration-"), "{id}");
    assert_eq!(body["name"], id);
}

#[tokio::test]
async fn returns_the_existing_integration_with_exist_ok() {
    let server = MockServer::start(vec![MockResponse::json(200, integration_json())]).await;
    let params = IntegrationCreateParams {
        exist_ok: true,
        ..create_params()
    };
    support::platform(&server)
        .create_integration(params)
        .await
        .unwrap();
    assert_eq!(sent(&server.requests()), [format!("GET {INTEGRATION}")]);
}

#[tokio::test]
async fn creates_a_missing_integration_with_exist_ok() {
    let server = MockServer::start(vec![
        MockResponse::new(404).body("integration not found"),
        MockResponse::json(201, integration_json()),
    ])
    .await;
    let params = IntegrationCreateParams {
        exist_ok: true,
        ..create_params()
    };
    support::platform(&server)
        .create_integration(params)
        .await
        .unwrap();
    assert_eq!(
        sent(&server.requests()),
        [format!("GET {INTEGRATION}"), format!("POST {INTEGRATIONS}")]
    );
}

#[tokio::test]
async fn rejects_a_create_that_the_platform_would_refuse() {
    let server = MockServer::start(Vec::new()).await;
    let platform = support::platform(&server);
    let cases = [
        (
            IntegrationCreateParams {
                integration_id: None,
                exist_ok: true,
                ..create_params()
            },
            "`exist_ok` needs an `integration_id`",
        ),
        (
            IntegrationCreateParams {
                is_global: true,
                ..create_params()
            },
            "a global integration has no `workspace_ids`: every workspace may use it",
        ),
        (
            IntegrationCreateParams {
                workspace_ids: Vec::new(),
                ..create_params()
            },
            "an integration that is not global needs `workspace_ids`",
        ),
        (
            IntegrationCreateParams {
                executable_types: Vec::new(),
                ..create_params()
            },
            "`executable_types` needs at least one type",
        ),
        (
            IntegrationCreateParams {
                executable_types: vec![ExecutableType::Unknown],
                ..create_params()
            },
            "`executable_types` must be known types, such as `python`",
        ),
        (
            IntegrationCreateParams {
                integration_type: IntegrationType::Unknown,
                ..create_params()
            },
            "`integration_type` must be `compute` or `data`",
        ),
        (
            IntegrationCreateParams {
                provider: IntegrationProvider::Unknown,
                ..create_params()
            },
            "`provider` must be `dbx` or `aws-batch`",
        ),
    ];
    for (params, message) in cases {
        let err = platform.create_integration(params).await.unwrap_err();
        assert!(matches!(err, Error::Invalid { .. }), "{err}");
        assert_eq!(err.to_string(), format!("invalid argument: {message}"));
    }
    // A global integration with no workspaces is fine.
    let server = MockServer::start(vec![MockResponse::json(201, integration_json())]).await;
    let params = IntegrationCreateParams {
        is_global: true,
        workspace_ids: Vec::new(),
        ..create_params()
    };
    support::platform(&server)
        .create_integration(params)
        .await
        .unwrap();
}

#[tokio::test]
async fn gets_an_integration() {
    let server = MockServer::start(vec![MockResponse::json(200, integration_json())]).await;
    let integration = support::platform(&server)
        .get_integration("dbx-1")
        .await
        .unwrap();
    assert_eq!(sent(&server.requests()), [format!("GET {INTEGRATION}")]);
    assert_eq!(integration.name, "Databricks");
    let mut expected = integration_json();
    expected.as_object_mut().unwrap().remove("owner");
    assert_eq!(serde_json::to_value(&integration).unwrap(), expected);
}

#[tokio::test]
async fn reads_values_it_does_not_know_as_unknown() {
    let mut integration = integration_json();
    integration["type"] = json!("storage");
    integration["provider"] = json!("snowflake");
    integration["exec_types"] = json!(["python", "rust"]);
    let server = MockServer::start(vec![MockResponse::json(200, integration)]).await;
    let integration = support::platform(&server)
        .get_integration("dbx-1")
        .await
        .unwrap();
    assert_eq!(integration.integration_type, IntegrationType::Unknown);
    assert_eq!(integration.provider, IntegrationProvider::Unknown);
    assert_eq!(
        integration.executable_types,
        [ExecutableType::Python, ExecutableType::Unknown]
    );
}

#[tokio::test]
async fn checks_whether_an_integration_exists() {
    let server = MockServer::start(vec![
        MockResponse::json(200, integration_json()),
        MockResponse::new(404).body("integration not found"),
        MockResponse::new(500).body("boom"),
    ])
    .await;
    let platform = support::platform(&server);
    assert!(platform.integration_exists("dbx-1").await.unwrap());
    assert!(!platform.integration_exists("dbx-2").await.unwrap());
    let err = platform.integration_exists("dbx-1").await.unwrap_err();
    assert!(matches!(err, Error::Status { status: 500, .. }), "{err}");
}

#[tokio::test]
async fn lists_integrations_across_pages() {
    let mut second = summary_json();
    second["id"] = json!("dbx-2");
    let server = MockServer::start(vec![
        MockResponse::json(
            200,
            json!({ "items": [summary_json()], "next_page_token": "page-2" }),
        ),
        MockResponse::json(200, json!({ "items": [second] })),
    ])
    .await;
    let integrations = support::platform(&server)
        .list_integrations()
        .await
        .unwrap();
    let ids: Vec<&str> = integrations
        .iter()
        .map(|i| i.integration_id.as_str())
        .collect();
    assert_eq!(ids, ["dbx-1", "dbx-2"]);
    assert_eq!(integrations[0].provider_config, None);
    let requests = server.requests();
    assert_eq!(
        requests[1].query(),
        support::pairs(&[("pagereturn", "true"), ("pagetoken", "page-2")])
    );
}

#[tokio::test]
async fn updates_only_the_fields_that_are_set_and_sends_the_rest_back() {
    let mut updated = integration_json();
    updated["name"] = json!("Databricks prod");
    updated["application_ids"] = json!(["routing", "scheduling"]);
    let server = MockServer::start(vec![
        MockResponse::json(200, integration_json()),
        MockResponse::json(200, updated.clone()),
    ])
    .await;
    let params = IntegrationUpdateParams {
        name: Some("Databricks prod".to_owned()),
        workspace_ids: Some(vec!["routing".to_owned(), "scheduling".to_owned()]),
        ..Default::default()
    };
    let integration = support::platform(&server)
        .update_integration("dbx-1", params)
        .await
        .unwrap();
    let requests = server.requests();
    assert_eq!(
        sent(&requests),
        [format!("GET {INTEGRATION}"), format!("PUT {INTEGRATION}")]
    );
    // Everything else, `provider_config` and `owner` included, goes back as
    // it came.
    assert_eq!(support::body(&requests[1]), updated);
    assert_eq!(integration.name, "Databricks prod");
}

#[tokio::test]
async fn checks_the_merged_integration_on_update() {
    let server = MockServer::start(vec![MockResponse::json(200, integration_json())]).await;
    let params = IntegrationUpdateParams {
        is_global: Some(true),
        ..Default::default()
    };
    let err = support::platform(&server)
        .update_integration("dbx-1", params)
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "invalid argument: a global integration has no `workspace_ids`: every workspace may use it"
    );
    // The GET only: no PUT.
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn rejects_an_empty_update() {
    let server = MockServer::start(Vec::new()).await;
    let platform = support::platform(&server);
    let err = platform
        .update_integration("dbx-1", IntegrationUpdateParams::default())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Invalid { .. }), "{err}");
    let params = IntegrationUpdateParams {
        executable_types: Some(Vec::new()),
        ..Default::default()
    };
    let err = platform
        .update_integration("dbx-1", params)
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "invalid argument: `executable_types` needs at least one type"
    );
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn deletes_an_integration() {
    let server = MockServer::start(vec![MockResponse::new(204)]).await;
    support::platform(&server)
        .delete_integration("dbx-1")
        .await
        .unwrap();
    assert_eq!(sent(&server.requests()), [format!("DELETE {INTEGRATION}")]);
}

#[tokio::test]
async fn gets_the_health_of_an_integration() {
    let server = MockServer::start(vec![MockResponse::json(
        200,
        json!({ "status": "unavailable", "last_updated": "2026-10-30T10:00:00Z" }),
    )])
    .await;
    let health = support::platform(&server)
        .get_integration_health("dbx-1")
        .await
        .unwrap();
    assert_eq!(
        sent(&server.requests()),
        [format!("GET {INTEGRATION}/health")]
    );
    assert_eq!(health.status, IntegrationHealthStatus::Unavailable);
    assert_eq!(health.message, None);
}

#[tokio::test]
async fn rejects_ids_that_change_the_path() {
    let server = MockServer::start(Vec::new()).await;
    let platform = support::platform(&server);
    for id in ["", "..", "a/b"] {
        let err = platform.delete_integration(id).await.unwrap_err();
        assert!(matches!(err, Error::Invalid { .. }), "{id:?}: {err}");
    }
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn serves_the_create_tool_without_the_provider_config() {
    let (result, requests) = support::serve_tool_ok(
        vec![MockResponse::json(201, integration_json())],
        "decision_platform_integration_create",
        json!({
            "integration_id": "dbx-1",
            "workspace_ids": ["routing"],
            "integration_type": "compute",
            "executable_types": ["python"],
            "provider": "dbx",
            "provider_config": config_json(),
        }),
    )
    .await;
    assert_eq!(
        support::body(&requests[0])["provider_config"],
        config_json()
    );
    assert_eq!(result["id"], "dbx-1");
    assert_eq!(result.get("provider_config"), None);
}

#[tokio::test]
async fn rejects_bad_create_tool_arguments() {
    for (args, message) in [
        (
            json!({ "integration_type": "compute", "executable_types": ["python"], "provider": "dbx" }),
            "missing field `provider_config`",
        ),
        (
            json!({
                "integration_type": "compute", "executable_types": ["python"],
                "provider": "acme", "provider_config": {},
            }),
            "unknown variant `acme`",
        ),
    ] {
        let (result, requests) =
            support::serve_tool(Vec::new(), "decision_platform_integration_create", args).await;
        let err = result.unwrap_err();
        assert!(
            matches!(&err, ToolError::InvalidArguments(m) if m.contains(message)),
            "{err}"
        );
        assert!(requests.is_empty());
    }
}

#[tokio::test]
async fn serves_the_get_tool_without_the_provider_config() {
    let (result, requests) = support::serve_tool_ok(
        vec![MockResponse::json(200, integration_json())],
        "decision_platform_integration_get",
        json!({ "integration_id": "dbx-1" }),
    )
    .await;
    assert_eq!(sent(&requests), [format!("GET {INTEGRATION}")]);
    assert_eq!(result["name"], "Databricks");
    // The config holds credentials: the tool never returns it.
    assert_eq!(result.get("provider_config"), None);
    assert!(!result.to_string().contains("not-a-secret"));
}

#[tokio::test]
async fn serves_the_exists_tool() {
    let (result, _) = support::serve_tool_ok(
        vec![MockResponse::new(404).body("integration not found")],
        "decision_platform_integration_exists",
        json!({ "integration_id": "dbx-1" }),
    )
    .await;
    assert_eq!(result, json!(false));
}

#[tokio::test]
async fn serves_the_list_tool() {
    let (result, requests) = support::serve_tool_ok(
        vec![MockResponse::json(
            200,
            json!({ "items": [summary_json()] }),
        )],
        "decision_platform_integration_list",
        json!({}),
    )
    .await;
    assert_eq!(
        sent(&requests),
        [format!("GET {INTEGRATIONS}?pagereturn=true")]
    );
    assert_eq!(result[0]["id"], "dbx-1");
}

#[tokio::test]
async fn serves_the_update_tool_without_the_provider_config() {
    let mut updated = integration_json();
    updated["description"] = json!("");
    let (result, requests) = support::serve_tool_ok(
        vec![
            MockResponse::json(200, integration_json()),
            MockResponse::json(200, updated.clone()),
        ],
        "decision_platform_integration_update",
        json!({ "integration_id": "dbx-1", "description": "" }),
    )
    .await;
    assert_eq!(support::body(&requests[1]), updated);
    assert_eq!(result["description"], "");
    assert_eq!(result.get("provider_config"), None);
}

#[tokio::test]
async fn serves_the_delete_tool() {
    let (result, requests) = support::serve_tool_ok(
        vec![MockResponse::new(204)],
        "decision_platform_integration_delete",
        json!({ "integration_id": "dbx-1" }),
    )
    .await;
    assert_eq!(sent(&requests), [format!("DELETE {INTEGRATION}")]);
    assert_eq!(result, Value::Null);
}

#[tokio::test]
async fn serves_the_health_tool() {
    let (result, _) = support::serve_tool_ok(
        vec![MockResponse::json(
            200,
            json!({ "status": "healthy", "message": "ok", "last_updated": "2026-10-30T10:00:00Z" }),
        )],
        "decision_platform_integration_health_get",
        json!({ "integration_id": "dbx-1" }),
    )
    .await;
    assert_eq!(result["status"], "healthy");
}
