//! Tests for the platform upload URL operation and its tool, against the
//! mock server.

mod mock_server;
mod support;

use fico_sdk::tool::Context;
use mock_server::{MockResponse, MockServer};
use serde_json::{Value, json};

/// An upload URL as `POST A/runs/uploadurl` returns it.
fn upload_json() -> Value {
    json!({
        "upload_id": "0d7ab3fa-6c2d-4b8e-9a51-2f3c4d5e6f70",
        "upload_url": "https://bucket.example.com/uploads/0d7ab3fa?X-Amz-Signature=abc",
    })
}

#[tokio::test]
async fn creates_an_upload_url() {
    let server = MockServer::start(vec![MockResponse::json(200, upload_json())]).await;
    let upload = support::platform(&server)
        .create_upload("routing")
        .await
        .unwrap();

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "POST");
    assert_eq!(
        requests[0].target,
        "/v1/applications/routing/runs/uploadurl"
    );
    assert!(requests[0].body.is_empty());
    assert_eq!(requests[0].header("authorization"), Some("Bearer secret"));
    assert_eq!(upload.upload_id, "0d7ab3fa-6c2d-4b8e-9a51-2f3c4d5e6f70");
    assert_eq!(
        upload.upload_url,
        "https://bucket.example.com/uploads/0d7ab3fa?X-Amz-Signature=abc"
    );
}

#[tokio::test]
async fn encodes_the_workspace_id_in_the_path() {
    let server = MockServer::start(vec![MockResponse::json(200, upload_json())]).await;
    support::platform(&server)
        .create_upload("my routing")
        .await
        .unwrap();
    assert_eq!(
        server.requests()[0].target,
        "/v1/applications/my%20routing/runs/uploadurl"
    );
}

#[tokio::test]
async fn serves_the_create_tool() {
    let server = MockServer::start(vec![MockResponse::json(200, upload_json())]).await;
    let cx = Context::new().with_platform(support::platform(&server));
    let result = support::call_tool(
        cx,
        "decision_platform_upload_create",
        json!({ "workspace_id": "routing" }),
    )
    .await
    .unwrap();
    assert_eq!(result, upload_json());
    let requests = server.requests();
    assert_eq!(requests[0].method, "POST");
    assert_eq!(
        requests[0].target,
        "/v1/applications/routing/runs/uploadurl"
    );
}
