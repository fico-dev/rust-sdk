//! Tests for the platform run asset operations and their tools, against the
//! mock server.

mod mock_server;
mod support;

use std::fs;

use fico_sdk::Error;
use fico_sdk::decision::local::Local;
use fico_sdk::decision::platform::RunAssetGetParams;
use fico_sdk::tool::{Context, ToolError};
use mock_server::{MockResponse, MockServer, RecordedRequest};
use serde_json::{Value, json};
use support::TempDir;

/// Path of the assets of `run-1` in the `routing` workspace.
const ASSETS: &str = "/v1/applications/routing/runs/run-1/assets";
/// ID of the asset of `asset_json`.
const ASSET_ID: &str = "9161b136-f0e7-4b20-ba62-f5a064b9d5c2";

/// A Plotly asset as the platform's list returns it, with a field the SDK
/// does not model (`owner`).
fn asset_json() -> Value {
    json!({
        "id": ASSET_ID,
        "name": "Plotly example",
        "content_type": "json",
        "visual": { "schema": "plotly", "type": "custom-tab", "label": "Charts", "tab_order": 1 },
        "created_at": "2026-10-29T14:35:10Z",
        "size": 7008,
        "owner": "wrangle",
    })
}

/// [`asset_json`] as the platform's get returns it, with `download_url`.
fn with_url(bucket: &MockServer) -> Value {
    let mut asset = asset_json();
    asset["download_url"] = json!(format!(
        "{}/assets/plotly.json?X-Amz-Signature=abc",
        bucket.url()
    ));
    asset
}

/// The method and target of each request, e.g. `GET /v1/apps?x=1`.
fn sent(requests: &[RecordedRequest]) -> Vec<String> {
    requests
        .iter()
        .map(|r| format!("{} {}", r.method, r.target))
        .collect()
}

#[tokio::test]
async fn lists_the_assets_of_a_run() {
    let mut failed = json!({ "id": "a2", "created_at": "2026-10-29T14:35:10Z" });
    failed["error"] = json!({ "code": "ERROR_TOO_LARGE" });
    let server = MockServer::start(vec![MockResponse::json(
        200,
        json!({ "items": [asset_json(), failed] }),
    )])
    .await;
    let assets = support::platform(&server)
        .list_run_assets("routing", "run-1")
        .await
        .unwrap();
    assert_eq!(sent(&server.requests()), [format!("GET {ASSETS}")]);
    assert_eq!(assets.len(), 2);
    assert_eq!(assets[0].asset_id, ASSET_ID);
    let visual = assets[0].visual.as_ref().unwrap();
    assert_eq!(visual.schema, "plotly");
    assert_eq!(visual.visual_type, "custom-tab");
    assert_eq!(visual.tab_order, Some(1));
    assert_eq!(assets[0].size, Some(7008));
    assert_eq!(assets[1].error.as_ref().unwrap().code, "ERROR_TOO_LARGE");
    assert_eq!(assets[1].name, None);
    // Emitted JSON uses the API names, and leaves out what the SDK does not
    // model.
    let mut expected = asset_json();
    expected.as_object_mut().unwrap().remove("owner");
    assert_eq!(serde_json::to_value(&assets[0]).unwrap(), expected);
}

#[tokio::test]
async fn lists_no_assets_of_a_run_without_any() {
    let server = MockServer::start(vec![MockResponse::json(200, json!({}))]).await;
    let assets = support::platform(&server)
        .list_run_assets("routing", "run-1")
        .await
        .unwrap();
    assert!(assets.is_empty());
}

#[tokio::test]
async fn gets_an_asset_with_its_content_from_its_download_url() {
    let content = json!([{ "data": [], "layout": {} }]);
    let bucket = MockServer::start(vec![MockResponse::json(200, content.clone())]).await;
    let server = MockServer::start(vec![MockResponse::json(200, with_url(&bucket))]).await;
    let asset = support::platform(&server)
        .get_run_asset("routing", "run-1", ASSET_ID, RunAssetGetParams::default())
        .await
        .unwrap();
    assert_eq!(
        sent(&server.requests()),
        [format!("GET {ASSETS}/{ASSET_ID}")]
    );
    let downloads = bucket.requests();
    assert_eq!(
        sent(&downloads),
        ["GET /assets/plotly.json?X-Amz-Signature=abc"]
    );
    // The presigned URL gets no token.
    assert_eq!(downloads[0].header("authorization"), None);
    assert_eq!(asset.asset.name.as_deref(), Some("Plotly example"));
    assert_eq!(asset.content, Some(content));
}

#[tokio::test]
async fn writes_an_asset_to_output_file() {
    let bucket = MockServer::start(vec![MockResponse::new(200).body(r#"[{"data":[]}]"#)]).await;
    let server = MockServer::start(vec![MockResponse::json(200, with_url(&bucket))]).await;
    let dir = TempDir::new("asset");
    let file = dir.path().join("visuals").join("plotly.json");
    let params = RunAssetGetParams {
        output_file: Some(file.clone()),
    };
    let asset = support::platform(&server)
        .get_run_asset("routing", "run-1", ASSET_ID, params)
        .await
        .unwrap();
    assert_eq!(asset.content, None);
    assert_eq!(fs::read_to_string(file).unwrap(), r#"[{"data":[]}]"#);
}

#[tokio::test]
async fn needs_output_file_for_an_asset_that_is_not_json() {
    let bucket = MockServer::start(Vec::new()).await;
    let mut asset = with_url(&bucket);
    asset["content_type"] = json!("csv");
    let server = MockServer::start(vec![MockResponse::json(200, asset)]).await;
    let err = support::platform(&server)
        .get_run_asset("routing", "run-1", ASSET_ID, RunAssetGetParams::default())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Invalid { .. }), "{err}");
    assert!(bucket.requests().is_empty());
}

#[tokio::test]
async fn rejects_an_asset_without_a_download_url() {
    let server = MockServer::start(vec![MockResponse::json(200, asset_json())]).await;
    let err = support::platform(&server)
        .get_run_asset("routing", "run-1", ASSET_ID, RunAssetGetParams::default())
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        format!("invalid argument: the asset `{ASSET_ID}` has no content to download")
    );
}

#[tokio::test]
async fn deletes_an_asset() {
    let server = MockServer::start(vec![MockResponse::new(204)]).await;
    support::platform(&server)
        .delete_run_asset("routing", "run-1", ASSET_ID)
        .await
        .unwrap();
    assert_eq!(
        sent(&server.requests()),
        [format!("DELETE {ASSETS}/{ASSET_ID}")]
    );
}

#[tokio::test]
async fn rejects_ids_that_change_the_path() {
    let server = MockServer::start(Vec::new()).await;
    let platform = support::platform(&server);
    for (run_id, asset_id) in [("..", ASSET_ID), ("run-1", ""), ("run-1", "a/b")] {
        let err = platform
            .delete_run_asset("routing", run_id, asset_id)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Invalid { .. }), "{asset_id:?}: {err}");
    }
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn serves_the_list_tool() {
    let (result, requests) = support::serve_tool_ok(
        vec![MockResponse::json(200, json!({ "items": [asset_json()] }))],
        "decision_platform_run_asset_list",
        json!({ "workspace_id": "routing", "run_id": "run-1" }),
    )
    .await;
    assert_eq!(sent(&requests), [format!("GET {ASSETS}")]);
    assert_eq!(result[0]["id"], ASSET_ID);
}

#[tokio::test]
async fn serves_the_get_tool() {
    let bucket = MockServer::start(vec![MockResponse::json(200, json!({ "ok": true }))]).await;
    let (result, requests) = support::serve_tool_ok(
        vec![MockResponse::json(200, with_url(&bucket))],
        "decision_platform_run_asset_get",
        json!({ "workspace_id": "routing", "run_id": "run-1", "asset_id": ASSET_ID }),
    )
    .await;
    assert_eq!(sent(&requests), [format!("GET {ASSETS}/{ASSET_ID}")]);
    assert_eq!(result["content"], json!({ "ok": true }));
    assert_eq!(result["asset"]["name"], "Plotly example");
}

#[tokio::test]
async fn needs_local_mode_for_output_file_in_the_get_tool() {
    let server = MockServer::start(Vec::new()).await;
    let remote = Context::new().with_platform(support::platform(&server));
    let args = json!({
        "workspace_id": "routing",
        "run_id": "run-1",
        "asset_id": ASSET_ID,
        "output_file": "/srv/plotly.json",
    });
    let err = support::call_tool(remote, "decision_platform_run_asset_get", args)
        .await
        .unwrap_err();
    assert!(
        matches!(err, ToolError::Failed(Error::Invalid { .. })),
        "{err}"
    );
    assert!(server.requests().is_empty());
    // In local mode, the file is written.
    let bucket = MockServer::start(vec![MockResponse::json(200, json!([]))]).await;
    let server = MockServer::start(vec![MockResponse::json(200, with_url(&bucket))]).await;
    let dir = TempDir::new("asset-tool");
    let file = dir.path().join("plotly.json");
    let local = Context::new()
        .with_platform(support::platform(&server))
        .with_local(Local::with_home(dir.path().join(".fico")));
    let args = json!({
        "workspace_id": "routing",
        "run_id": "run-1",
        "asset_id": ASSET_ID,
        "output_file": file,
    });
    support::call_tool(local, "decision_platform_run_asset_get", args)
        .await
        .unwrap();
    assert_eq!(fs::read_to_string(file).unwrap(), "[]");
}

#[tokio::test]
async fn serves_the_delete_tool() {
    let (_, requests) = support::serve_tool_ok(
        vec![MockResponse::new(204)],
        "decision_platform_run_asset_delete",
        json!({ "workspace_id": "routing", "run_id": "run-1", "asset_id": ASSET_ID }),
    )
    .await;
    assert_eq!(sent(&requests), [format!("DELETE {ASSETS}/{ASSET_ID}")]);
}
