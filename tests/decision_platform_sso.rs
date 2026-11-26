//! Tests for the platform SSO operations and their tools, against the mock
//! server.

mod mock_server;
mod support;

use fico_sdk::Error;
use fico_sdk::decision::platform::{SsoCreateParams, SsoUpdateParams};
use mock_server::{MockResponse, MockServer};
use serde_json::{Value, json};

const METADATA_URL: &str = "https://sso.carrotexpress.com/saml/metadata";
const METADATA_DOCUMENT: &str = r#"<EntityDescriptor xmlns="urn:oasis:names:tc:SAML:2.0:metadata" entityID="https://sso.carrotexpress.com"></EntityDescriptor>"#;

/// An SSO configuration as the platform returns it, plus a field the SDK
/// does not model (`domain`), as a newer platform could send.
fn sso_json() -> Value {
    json!({
        "enabled": true,
        "metadata_url": METADATA_URL,
        "allow_non_domain_users": false,
        "mapped_domains": ["carrotexpress.io"],
        "domain": "carrotexpress.com",
    })
}

#[tokio::test]
async fn creates_an_sso_configuration_from_a_url() {
    let server = MockServer::start(vec![MockResponse::new(204)]).await;
    let params = SsoCreateParams {
        metadata_url: Some(METADATA_URL.to_owned()),
        ..Default::default()
    };
    support::platform(&server).create_sso(params).await.unwrap();

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].target, "/v1/enterprise/sso");
    // The platform requires `enabled`, so it is always sent.
    assert_eq!(
        support::body(&requests[0]),
        json!({
            "allow_non_domain_users": false,
            "enabled": false,
            "metadata_url": METADATA_URL,
        })
    );
}

#[tokio::test]
async fn creates_an_sso_configuration_from_a_document() {
    let server = MockServer::start(vec![MockResponse::new(204)]).await;
    let params = SsoCreateParams {
        allow_non_domain_users: true,
        enabled: true,
        metadata_url: Some(String::new()),
        metadata_document: Some(METADATA_DOCUMENT.to_owned()),
    };
    support::platform(&server).create_sso(params).await.unwrap();
    assert_eq!(
        support::body(&server.requests()[0]),
        json!({
            "allow_non_domain_users": true,
            "enabled": true,
            "metadata_document": METADATA_DOCUMENT,
        })
    );
}

/// Every pair of metadata sources that is not exactly one, and the message
/// it gets.
fn not_one_source() -> Vec<(Option<String>, Option<String>, &'static str)> {
    let neither = "invalid argument: set `metadata_url` or `metadata_document`";
    let both = "invalid argument: set only one of `metadata_url` and `metadata_document`";
    vec![
        (None, None, neither),
        (Some(String::new()), Some(String::new()), neither),
        (
            Some(METADATA_URL.to_owned()),
            Some(METADATA_DOCUMENT.to_owned()),
            both,
        ),
    ]
}

#[tokio::test]
async fn needs_exactly_one_metadata_source_to_create() {
    let server = MockServer::start(Vec::new()).await;
    let platform = support::platform(&server);
    for (metadata_url, metadata_document, message) in not_one_source() {
        let params = SsoCreateParams {
            metadata_url,
            metadata_document,
            ..Default::default()
        };
        let err = platform.create_sso(params).await.unwrap_err();
        assert!(matches!(err, Error::Invalid { .. }), "{err}");
        assert_eq!(err.to_string(), message);
    }
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn gets_the_sso_configuration() {
    let server = MockServer::start(vec![MockResponse::json(200, sso_json())]).await;
    let config = support::platform(&server).get_sso().await.unwrap();
    let request = &server.requests()[0];
    assert_eq!(request.method, "GET");
    assert_eq!(request.target, "/v1/enterprise/sso");
    assert_eq!(request.header("authorization"), Some("Bearer secret"));
    assert_eq!(config.enabled, Some(true));
    assert_eq!(config.metadata_url.as_deref(), Some(METADATA_URL));
    assert_eq!(config.metadata_document, None);
    assert!(!config.allow_non_domain_users);
    assert_eq!(
        config.mapped_domains.as_deref(),
        Some(&["carrotexpress.io".to_owned()][..])
    );
    // Emitted JSON uses the API names, and leaves out what the SDK does not
    // model.
    let mut expected = sso_json();
    expected.as_object_mut().unwrap().remove("domain");
    assert_eq!(serde_json::to_value(&config).unwrap(), expected);
}

#[tokio::test]
async fn decodes_a_minimal_sso_configuration() {
    let server = MockServer::start(vec![MockResponse::json(
        200,
        json!({ "metadata_document": METADATA_DOCUMENT }),
    )])
    .await;
    let config = support::platform(&server).get_sso().await.unwrap();
    assert_eq!(config.metadata_document.as_deref(), Some(METADATA_DOCUMENT));
    assert!(!config.allow_non_domain_users);
    assert_eq!(config.enabled, None);
    assert_eq!(config.mapped_domains, None);
}

#[tokio::test]
async fn enables_and_disables_sso() {
    let server = MockServer::start(vec![MockResponse::new(204), MockResponse::new(204)]).await;
    let platform = support::platform(&server);
    platform.enable_sso().await.unwrap();
    platform.disable_sso().await.unwrap();
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].method, "PUT");
    assert_eq!(requests[0].target, "/v1/enterprise/sso/enable");
    assert!(requests[0].body.is_empty());
    assert_eq!(requests[1].method, "PUT");
    assert_eq!(requests[1].target, "/v1/enterprise/sso/disable");
    assert!(requests[1].body.is_empty());
}

#[tokio::test]
async fn updates_the_metadata_url_and_sends_the_rest_back() {
    let server = MockServer::start(vec![
        MockResponse::json(200, sso_json()),
        MockResponse::new(204),
    ])
    .await;
    let params = SsoUpdateParams {
        metadata_url: Some("https://sso.carrotexpress.com/saml/metadata/v2".to_owned()),
        ..Default::default()
    };
    support::platform(&server).update_sso(params).await.unwrap();

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[0].target, "/v1/enterprise/sso");
    assert_eq!(requests[1].method, "PUT");
    assert_eq!(requests[1].target, "/v1/enterprise/sso");
    // Everything else, `domain` included, goes back as it came.
    let mut updated = sso_json();
    updated["metadata_url"] = json!("https://sso.carrotexpress.com/saml/metadata/v2");
    assert_eq!(support::body(&requests[1]), updated);
}

#[tokio::test]
async fn switches_from_a_url_to_a_document() {
    let server = MockServer::start(vec![
        MockResponse::json(200, sso_json()),
        MockResponse::new(204),
    ])
    .await;
    let params = SsoUpdateParams {
        metadata_document: Some(METADATA_DOCUMENT.to_owned()),
        ..Default::default()
    };
    support::platform(&server).update_sso(params).await.unwrap();
    // The platform rejects both sources at once, so the URL goes.
    let mut updated = sso_json();
    updated.as_object_mut().unwrap().remove("metadata_url");
    updated["metadata_document"] = json!(METADATA_DOCUMENT);
    assert_eq!(support::body(&server.requests()[1]), updated);
}

#[tokio::test]
async fn needs_at_most_one_metadata_source_and_a_change_to_update() {
    let server = MockServer::start(Vec::new()).await;
    let platform = support::platform(&server);
    let nothing = "invalid argument: nothing to update: set `metadata_url`, \
                   `metadata_document`, `enabled` or `allow_non_domain_users`";
    let both = "invalid argument: set only one of `metadata_url` and `metadata_document`";
    let cases = [
        (None, None, nothing),
        (Some(String::new()), Some(String::new()), nothing),
        (
            Some(METADATA_URL.to_owned()),
            Some(METADATA_DOCUMENT.to_owned()),
            both,
        ),
    ];
    for (metadata_url, metadata_document, message) in cases {
        let params = SsoUpdateParams {
            metadata_url,
            metadata_document,
            ..Default::default()
        };
        let err = platform.update_sso(params).await.unwrap_err();
        assert!(matches!(err, Error::Invalid { .. }), "{err}");
        assert_eq!(err.to_string(), message);
    }
    // Both sources stay wrong next to another change.
    let params = SsoUpdateParams {
        metadata_url: Some(METADATA_URL.to_owned()),
        metadata_document: Some(METADATA_DOCUMENT.to_owned()),
        enabled: Some(false),
        ..Default::default()
    };
    let err = platform.update_sso(params).await.unwrap_err();
    assert_eq!(err.to_string(), both);
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn updates_enabled_and_allow_non_domain_users_and_keeps_the_source() {
    let server = MockServer::start(vec![
        MockResponse::json(200, sso_json()),
        MockResponse::new(204),
    ])
    .await;
    let params = SsoUpdateParams {
        enabled: Some(false),
        allow_non_domain_users: Some(true),
        ..Default::default()
    };
    support::platform(&server).update_sso(params).await.unwrap();
    let mut updated = sso_json();
    updated["enabled"] = json!(false);
    updated["allow_non_domain_users"] = json!(true);
    assert_eq!(support::body(&server.requests()[1]), updated);
}

#[tokio::test]
async fn reports_a_configuration_that_is_not_an_object_on_update() {
    let server = MockServer::start(vec![MockResponse::json(200, json!([METADATA_URL]))]).await;
    let params = SsoUpdateParams {
        metadata_url: Some(METADATA_URL.to_owned()),
        ..Default::default()
    };
    let err = support::platform(&server)
        .update_sso(params)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Decode { .. }), "{err}");
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn deletes_the_sso_configuration() {
    let server = MockServer::start(vec![MockResponse::new(204)]).await;
    support::platform(&server).delete_sso().await.unwrap();
    let request = &server.requests()[0];
    assert_eq!(request.method, "DELETE");
    assert_eq!(request.target, "/v1/enterprise/sso");
}

#[tokio::test]
async fn deletes_a_domain() {
    let server = MockServer::start(vec![MockResponse::new(204)]).await;
    support::platform(&server)
        .delete_sso_domain("carrotexpress.io")
        .await
        .unwrap();
    let request = &server.requests()[0];
    assert_eq!(request.method, "DELETE");
    assert_eq!(
        request.target,
        "/v1/enterprise/sso/domains/carrotexpress.io"
    );
}

#[tokio::test]
async fn encodes_domains_in_paths() {
    let server = MockServer::start(vec![MockResponse::new(204)]).await;
    let platform = support::platform(&server);
    platform.delete_sso_domain("a#b c?d").await.unwrap();
    assert_eq!(
        server.requests()[0].target,
        "/v1/enterprise/sso/domains/a%23b%20c%3Fd"
    );
    for domain in ["", ".", "..", "a/b"] {
        let err = platform.delete_sso_domain(domain).await.unwrap_err();
        assert!(matches!(err, Error::Invalid { .. }), "{domain:?}: {err}");
    }
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn serves_the_create_tool() {
    let (result, requests) = support::serve_tool_ok(
        vec![MockResponse::new(204)],
        "decision_platform_sso_create",
        json!({ "enabled": true, "metadata_document": METADATA_DOCUMENT }),
    )
    .await;
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].target, "/v1/enterprise/sso");
    assert_eq!(
        support::body(&requests[0]),
        json!({
            "allow_non_domain_users": false,
            "enabled": true,
            "metadata_document": METADATA_DOCUMENT,
        })
    );
    assert_eq!(result, Value::Null);
}

#[tokio::test]
async fn serves_the_get_tool() {
    let (result, requests) = support::serve_tool_ok(
        vec![MockResponse::json(200, sso_json())],
        "decision_platform_sso_get",
        json!({}),
    )
    .await;
    assert_eq!(requests[0].target, "/v1/enterprise/sso");
    assert_eq!(result["metadata_url"], METADATA_URL);
    assert_eq!(result["mapped_domains"], json!(["carrotexpress.io"]));
}

#[tokio::test]
async fn serves_the_enable_tool() {
    let (result, requests) = support::serve_tool_ok(
        vec![MockResponse::new(204)],
        "decision_platform_sso_enable",
        json!({}),
    )
    .await;
    assert_eq!(requests[0].method, "PUT");
    assert_eq!(requests[0].target, "/v1/enterprise/sso/enable");
    assert_eq!(result, Value::Null);
}

#[tokio::test]
async fn serves_the_disable_tool() {
    let (result, requests) = support::serve_tool_ok(
        vec![MockResponse::new(204)],
        "decision_platform_sso_disable",
        json!({}),
    )
    .await;
    assert_eq!(requests[0].method, "PUT");
    assert_eq!(requests[0].target, "/v1/enterprise/sso/disable");
    assert_eq!(result, Value::Null);
}

#[tokio::test]
async fn serves_the_update_tool() {
    let (result, requests) = support::serve_tool_ok(
        vec![MockResponse::json(200, sso_json()), MockResponse::new(204)],
        "decision_platform_sso_update",
        json!({ "metadata_document": METADATA_DOCUMENT }),
    )
    .await;
    assert_eq!(requests[1].method, "PUT");
    assert_eq!(requests[1].target, "/v1/enterprise/sso");
    let sent = support::body(&requests[1]);
    assert_eq!(sent["metadata_document"], METADATA_DOCUMENT);
    assert_eq!(sent.get("metadata_url"), None);
    assert_eq!(result, Value::Null);
}

#[tokio::test]
async fn serves_the_update_tool_with_enabled() {
    let (_, requests) = support::serve_tool_ok(
        vec![MockResponse::json(200, sso_json()), MockResponse::new(204)],
        "decision_platform_sso_update",
        json!({ "enabled": false }),
    )
    .await;
    let sent = support::body(&requests[1]);
    assert_eq!(sent["enabled"], false);
    assert_eq!(sent["metadata_url"], METADATA_URL);
}

#[tokio::test]
async fn serves_the_delete_tool() {
    let (result, requests) = support::serve_tool_ok(
        vec![MockResponse::new(204)],
        "decision_platform_sso_delete",
        json!({}),
    )
    .await;
    assert_eq!(requests[0].method, "DELETE");
    assert_eq!(requests[0].target, "/v1/enterprise/sso");
    assert_eq!(result, Value::Null);
}

#[tokio::test]
async fn serves_the_domain_delete_tool() {
    let (result, requests) = support::serve_tool_ok(
        vec![MockResponse::new(204)],
        "decision_platform_sso_domain_delete",
        json!({ "domain": "carrotexpress.io" }),
    )
    .await;
    assert_eq!(requests[0].method, "DELETE");
    assert_eq!(
        requests[0].target,
        "/v1/enterprise/sso/domains/carrotexpress.io"
    );
    assert_eq!(result, Value::Null);
}
