//! Tests for the platform data upload operation and its tool, against the
//! mock server.

mod mock_server;
mod support;

use std::fs;
use std::io::Read;

use fico_sdk::Error;
use fico_sdk::decision::local::Local;
use fico_sdk::decision::platform::DataUploadParams;
use fico_sdk::tool::{Context, ToolError};
use flate2::read::GzDecoder;
use mock_server::{MockResponse, MockServer, RecordedRequest};
use serde_json::json;
use support::TempDir;

/// A presigned URL on `bucket`, another origin than the API's.
fn presigned(bucket: &MockServer) -> String {
    format!("{}/uploads/0d7ab3fa?X-Amz-Signature=abc", bucket.url())
}

/// The files of the tar.gz `archive`, as (path, contents), in order.
fn untar(archive: &[u8]) -> Vec<(String, String)> {
    let mut archive = tar::Archive::new(GzDecoder::new(archive));
    archive
        .entries()
        .unwrap()
        .map(|entry| {
            let mut entry = entry.unwrap();
            let path = entry.path().unwrap().to_string_lossy().into_owned();
            let mut contents = String::new();
            entry.read_to_string(&mut contents).unwrap();
            (path, contents)
        })
        .collect()
}

/// Checks that `request` is the presigned PUT, with no token and no
/// content type.
fn assert_presigned_put(request: &RecordedRequest) {
    assert_eq!(request.method, "PUT");
    assert_eq!(request.target, "/uploads/0d7ab3fa?X-Amz-Signature=abc");
    assert_eq!(request.header("authorization"), None);
    assert_eq!(request.header("content-type"), None);
}

/// Uploads `params` to a new bucket and returns the request it got.
async fn upload(params: DataUploadParams) -> RecordedRequest {
    let api = MockServer::start(Vec::new()).await;
    let bucket = MockServer::start(vec![MockResponse::new(200)]).await;
    let params = DataUploadParams {
        upload_url: presigned(&bucket),
        ..params
    };
    support::platform(&api).upload_data(params).await.unwrap();
    assert!(api.requests().is_empty());
    let mut requests = bucket.requests();
    assert_eq!(requests.len(), 1);
    requests.remove(0)
}

#[tokio::test]
async fn uploads_json_data() {
    let request = upload(DataUploadParams {
        data: Some(json!({ "stops": [{ "id": "A" }] })),
        ..Default::default()
    })
    .await;
    assert_presigned_put(&request);
    assert_eq!(request.body, br#"{"stops":[{"id":"A"}]}"#);
}

#[tokio::test]
async fn uploads_a_file_as_it_is() {
    let dir = TempDir::new("data-upload");
    let file = dir.path().join("input.json");
    // Not compact, so a rewrite would show.
    fs::write(&file, "{ \"stops\": [] }\n").unwrap();
    let request = upload(DataUploadParams {
        data_file: Some(file),
        ..Default::default()
    })
    .await;
    assert_presigned_put(&request);
    assert_eq!(request.body, b"{ \"stops\": [] }\n");
}

#[tokio::test]
async fn uploads_a_directory_as_a_tar_gz() {
    let dir = TempDir::new("data-upload");
    fs::write(dir.path().join("orders.csv"), "id\n1\n").unwrap();
    fs::create_dir(dir.path().join("depots")).unwrap();
    fs::write(dir.path().join("depots/north.json"), "{}").unwrap();
    let request = upload(DataUploadParams {
        data_dir: Some(dir.path().to_owned()),
        ..Default::default()
    })
    .await;
    assert_presigned_put(&request);
    assert_eq!(
        untar(&request.body),
        [
            ("depots/north.json".to_owned(), "{}".to_owned()),
            ("orders.csv".to_owned(), "id\n1\n".to_owned()),
        ]
    );
}

#[tokio::test]
async fn uploads_bytes() {
    let request = upload(DataUploadParams {
        bytes: Some(b"raw".to_vec()),
        ..Default::default()
    })
    .await;
    assert_presigned_put(&request);
    assert_eq!(request.body, b"raw");
}

#[tokio::test]
async fn needs_exactly_one_source() {
    let server = MockServer::start(Vec::new()).await;
    let platform = support::platform(&server);
    let dir = TempDir::new("data-upload");
    let url = presigned(&server);
    for params in [
        DataUploadParams {
            upload_url: url.clone(),
            ..Default::default()
        },
        DataUploadParams {
            upload_url: url.clone(),
            data: Some(json!({})),
            data_dir: Some(dir.path().to_owned()),
            ..Default::default()
        },
        DataUploadParams {
            upload_url: url.clone(),
            data: Some(json!({})),
            bytes: Some(Vec::new()),
            ..Default::default()
        },
        // An empty path is unset.
        DataUploadParams {
            upload_url: url.clone(),
            data_file: Some("".into()),
            ..Default::default()
        },
    ] {
        let err = platform.upload_data(params).await.unwrap_err();
        assert!(
            matches!(&err, Error::Invalid { reason, .. } if reason.contains("exactly one")),
            "{err}"
        );
    }
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn needs_an_absolute_upload_url() {
    let server = MockServer::start(Vec::new()).await;
    let platform = support::platform(&server);
    for upload_url in [
        "",
        "/v1/applications/routing",
        "v1/applications",
        "file:///etc/passwd",
        "https:uploads",
    ] {
        let params = DataUploadParams {
            upload_url: upload_url.to_owned(),
            data: Some(json!({})),
            ..Default::default()
        };
        let err = platform.upload_data(params).await.unwrap_err();
        assert!(
            matches!(&err, Error::Invalid { reason, .. } if reason.contains("upload_url")),
            "{upload_url}: {err}"
        );
    }
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn needs_data_dir_to_be_a_directory() {
    let server = MockServer::start(Vec::new()).await;
    let dir = TempDir::new("data-upload");
    let file = dir.path().join("input.json");
    fs::write(&file, "{}").unwrap();
    let params = DataUploadParams {
        upload_url: presigned(&server),
        data_dir: Some(file),
        ..Default::default()
    };
    let err = support::platform(&server)
        .upload_data(params)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Invalid { .. }), "{err}");

    let params = DataUploadParams {
        upload_url: presigned(&server),
        data_file: Some(dir.path().join("missing.json")),
        ..Default::default()
    };
    let err = support::platform(&server)
        .upload_data(params)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Io { .. }), "{err}");
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn returns_the_error_of_a_failed_upload() {
    let api = MockServer::start(Vec::new()).await;
    let bucket = MockServer::start(vec![MockResponse::new(403).body("expired")]).await;
    let params = DataUploadParams {
        upload_url: presigned(&bucket),
        data: Some(json!({})),
        ..Default::default()
    };
    let err = support::platform(&api)
        .upload_data(params)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Status { status: 403, .. }), "{err}");
}

#[tokio::test]
async fn serves_the_upload_tool_in_local_mode() {
    let api = MockServer::start(Vec::new()).await;
    let bucket = MockServer::start(vec![MockResponse::new(200)]).await;
    let home = TempDir::new("data-upload-home");
    let cx = Context::new()
        .with_platform(support::platform(&api))
        .with_local(Local::with_home(home.path()));
    let result = support::call_tool(
        cx,
        "decision_platform_data_upload",
        json!({ "upload_url": presigned(&bucket), "data": { "stops": [] } }),
    )
    .await
    .unwrap();
    assert_eq!(result, json!(null));
    let requests = bucket.requests();
    assert_presigned_put(&requests[0]);
    assert_eq!(requests[0].body, br#"{"stops":[]}"#);
}

#[tokio::test]
async fn refuses_the_upload_tool_without_local_mode() {
    let bucket = MockServer::start(vec![MockResponse::new(200)]).await;
    let cx = Context::new().with_platform(support::platform(&bucket));
    let err = support::call_tool(
        cx,
        "decision_platform_data_upload",
        json!({ "upload_url": presigned(&bucket), "data": {} }),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(err, ToolError::Failed(Error::Invalid { .. })),
        "{err}"
    );
    assert!(bucket.requests().is_empty());
}

#[tokio::test]
async fn rejects_bytes_in_the_upload_tool() {
    let bucket = MockServer::start(Vec::new()).await;
    let home = TempDir::new("data-upload-home");
    let cx = Context::new()
        .with_platform(support::platform(&bucket))
        .with_local(Local::with_home(home.path()));
    let err = support::call_tool(
        cx,
        "decision_platform_data_upload",
        json!({ "upload_url": presigned(&bucket), "bytes": [1, 2] }),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ToolError::InvalidArguments(_)), "{err}");
    assert!(bucket.requests().is_empty());
}
