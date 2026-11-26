//! Tests for the platform account operations and their tools, against the
//! mock server.

mod mock_server;
mod support;

use fico_sdk::Error;
use fico_sdk::decision::AccountMemberRole;
use fico_sdk::decision::platform::{AccountCreateParams, AccountUpdateParams};
use fico_sdk::tool::{Context, ToolError};
use mock_server::{MockResponse, MockServer};
use serde_json::{Value, json};

const ACCOUNT_ID: &str = "3f1c9a52-7d4e-4b8a-9c61-2e5d8f0a7b13";

/// An account as the platform returns it, with a field the SDK does not
/// model (`root_org_info`).
fn account_json() -> Value {
    json!({
        "id": ACCOUNT_ID,
        "name": "Bunny Logistics",
        "root_org_info": { "id": "root-org", "name": "Carrot Express", "admin": true },
        "members": [
            { "email": "peter.rabbit@carrotexpress.com", "role": "root", "pending_invite": false },
            { "email": "flopsy@carrotexpress.com", "role": "admin", "pending_invite": true },
        ],
    })
}

#[tokio::test]
async fn creates_an_account() {
    let mut created = account_json();
    created.as_object_mut().unwrap().remove("root_org_info");
    let server = MockServer::start(vec![MockResponse::json(201, created.clone())]).await;
    let params = AccountCreateParams {
        name: "Bunny Logistics".to_owned(),
        admins: vec!["flopsy@carrotexpress.com".to_owned()],
    };
    let account = support::platform(&server)
        .create_account(params)
        .await
        .unwrap();

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].target, "/v1/organization");
    assert_eq!(
        support::body(&requests[0]),
        json!({ "name": "Bunny Logistics", "admins": ["flopsy@carrotexpress.com"] })
    );
    assert_eq!(account.account_id.as_deref(), Some(ACCOUNT_ID));
    assert_eq!(account.name.as_deref(), Some("Bunny Logistics"));
    let members = account.members.as_deref().unwrap();
    assert_eq!(members.len(), 2);
    assert_eq!(members[0].role, Some(AccountMemberRole::Root));
    assert_eq!(
        members[1].email.as_deref(),
        Some("flopsy@carrotexpress.com")
    );
    assert_eq!(members[1].role, Some(AccountMemberRole::Admin));
    assert_eq!(members[1].pending_invite, Some(true));
    assert_eq!(serde_json::to_value(&account).unwrap(), created);
}

#[tokio::test]
async fn needs_admins_to_create_an_account() {
    let server = MockServer::start(Vec::new()).await;
    let platform = support::platform(&server);
    let cases: [(&[&str], &str); 3] = [
        (&[], "invalid argument: `admins` needs at least one email"),
        (
            &["flopsy@carrotexpress.com", ""],
            "invalid argument: `admins` has a blank email",
        ),
        (&[" "], "invalid argument: `admins` has a blank email"),
    ];
    for (admins, message) in cases {
        let params = AccountCreateParams {
            name: "Bunny Logistics".to_owned(),
            admins: admins.iter().map(|admin| admin.to_string()).collect(),
        };
        let err = platform.create_account(params).await.unwrap_err();
        assert!(matches!(err, Error::Invalid { .. }), "{admins:?}: {err}");
        assert_eq!(err.to_string(), message);
    }
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn gets_an_account() {
    let server = MockServer::start(vec![MockResponse::json(200, account_json())]).await;
    let account = support::platform(&server)
        .get_account(ACCOUNT_ID)
        .await
        .unwrap();
    let request = &server.requests()[0];
    assert_eq!(request.method, "GET");
    assert_eq!(request.target, format!("/v1/organization/{ACCOUNT_ID}"));
    assert_eq!(request.header("authorization"), Some("Bearer secret"));
    assert_eq!(account.account_id.as_deref(), Some(ACCOUNT_ID));
    // Emitted JSON uses the API names, and leaves out what the SDK does not
    // model.
    let mut expected = account_json();
    expected.as_object_mut().unwrap().remove("root_org_info");
    assert_eq!(serde_json::to_value(&account).unwrap(), expected);
}

#[tokio::test]
async fn decodes_a_minimal_account() {
    let server = MockServer::start(vec![MockResponse::json(
        200,
        json!({ "id": ACCOUNT_ID, "members": [{ "email": "new@carrotexpress.com", "role": "unassigned" }] }),
    )])
    .await;
    let account = support::platform(&server)
        .get_account(ACCOUNT_ID)
        .await
        .unwrap();
    assert_eq!(account.name, None);
    let member = &account.members.unwrap()[0];
    assert_eq!(member.role, Some(AccountMemberRole::Unknown));
    assert_eq!(member.pending_invite, None);
}

#[tokio::test]
async fn updates_the_name_and_sends_the_whole_account_back() {
    let server = MockServer::start(vec![
        MockResponse::json(200, account_json()),
        MockResponse::json(200, json!({ "name": "Bunny Express" })),
    ])
    .await;
    let params = AccountUpdateParams {
        name: Some("Bunny Express".to_owned()),
    };
    let account = support::platform(&server)
        .update_account(ACCOUNT_ID, params)
        .await
        .unwrap();

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[0].target, format!("/v1/organization/{ACCOUNT_ID}"));
    assert_eq!(requests[1].method, "PUT");
    assert_eq!(requests[1].target, format!("/v1/organization/{ACCOUNT_ID}"));
    // Everything else, members and `root_org_info` included, goes back as
    // it came.
    let mut updated = account_json();
    updated["name"] = json!("Bunny Express");
    assert_eq!(support::body(&requests[1]), updated);
    // The platform answers with the name only, and the result is the whole
    // account.
    assert_eq!(account.account_id.as_deref(), Some(ACCOUNT_ID));
    assert_eq!(account.name.as_deref(), Some("Bunny Express"));
    assert_eq!(account.members.unwrap().len(), 2);
}

#[tokio::test]
async fn takes_the_platforms_values_after_an_update() {
    let server = MockServer::start(vec![
        MockResponse::json(200, account_json()),
        MockResponse::json(200, json!({ "name": "Bunny Express (2)" })),
    ])
    .await;
    let params = AccountUpdateParams {
        name: Some("Bunny Express".to_owned()),
    };
    let account = support::platform(&server)
        .update_account(ACCOUNT_ID, params)
        .await
        .unwrap();
    assert_eq!(account.name.as_deref(), Some("Bunny Express (2)"));
}

#[tokio::test]
async fn rejects_an_empty_update() {
    let server = MockServer::start(Vec::new()).await;
    for name in [None, Some(String::new())] {
        let err = support::platform(&server)
            .update_account(ACCOUNT_ID, AccountUpdateParams { name })
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Invalid { .. }), "{err}");
        assert_eq!(
            err.to_string(),
            "invalid argument: nothing to update: set `name`"
        );
    }
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn reports_an_account_that_is_not_an_object_on_update() {
    let server = MockServer::start(vec![MockResponse::json(200, json!([ACCOUNT_ID]))]).await;
    let params = AccountUpdateParams {
        name: Some("Bunny Express".to_owned()),
    };
    let err = support::platform(&server)
        .update_account(ACCOUNT_ID, params)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Decode { .. }), "{err}");
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn deletes_an_account() {
    let server = MockServer::start(vec![MockResponse::new(204)]).await;
    support::platform(&server)
        .delete_account(ACCOUNT_ID)
        .await
        .unwrap();
    let request = &server.requests()[0];
    assert_eq!(request.method, "DELETE");
    assert_eq!(request.target, format!("/v1/organization/{ACCOUNT_ID}"));
}

#[tokio::test]
async fn encodes_account_ids_in_paths() {
    let server = MockServer::start(vec![MockResponse::json(200, account_json())]).await;
    let platform = support::platform(&server);
    platform.get_account("a#b c?d").await.unwrap();
    assert_eq!(
        server.requests()[0].target,
        "/v1/organization/a%23b%20c%3Fd"
    );
    for id in ["", ".", "..", "a/b"] {
        let err = platform.delete_account(id).await.unwrap_err();
        assert!(matches!(err, Error::Invalid { .. }), "{id:?}: {err}");
    }
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn serves_the_create_tool() {
    let (result, requests) = support::serve_tool_ok(
        vec![MockResponse::json(201, account_json())],
        "decision_platform_account_create",
        json!({ "name": "Bunny Logistics", "admins": ["flopsy@carrotexpress.com"] }),
    )
    .await;
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].target, "/v1/organization");
    assert_eq!(
        support::body(&requests[0]),
        json!({ "name": "Bunny Logistics", "admins": ["flopsy@carrotexpress.com"] })
    );
    assert_eq!(result["id"], ACCOUNT_ID);
    assert_eq!(result["members"][1]["role"], "admin");
}

#[tokio::test]
async fn needs_a_name_and_admins_in_the_create_tool() {
    let server = MockServer::start(Vec::new()).await;
    let cx = Context::new().with_platform(support::platform(&server));
    let required =
        support::tool("decision_platform_account_create").input_schema()["required"].clone();
    assert_eq!(required, json!(["name", "admins"]));
    let err = support::call_tool(
        cx,
        "decision_platform_account_create",
        json!({ "name": "Bunny Logistics" }),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ToolError::InvalidArguments(_)), "{err}");
    assert_eq!(err.to_string(), "invalid arguments: missing field `admins`");
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn serves_the_get_tool() {
    let (result, requests) = support::serve_tool_ok(
        vec![MockResponse::json(200, account_json())],
        "decision_platform_account_get",
        json!({ "account_id": ACCOUNT_ID }),
    )
    .await;
    assert_eq!(requests[0].target, format!("/v1/organization/{ACCOUNT_ID}"));
    assert_eq!(result["name"], "Bunny Logistics");
}

#[tokio::test]
async fn serves_the_update_tool() {
    let (result, requests) = support::serve_tool_ok(
        vec![
            MockResponse::json(200, account_json()),
            MockResponse::json(200, json!({ "name": "Bunny Express" })),
        ],
        "decision_platform_account_update",
        json!({ "account_id": ACCOUNT_ID, "name": "Bunny Express" }),
    )
    .await;
    assert_eq!(requests[1].method, "PUT");
    assert_eq!(requests[1].target, format!("/v1/organization/{ACCOUNT_ID}"));
    assert_eq!(support::body(&requests[1])["name"], "Bunny Express");
    assert_eq!(result["id"], ACCOUNT_ID);
    assert_eq!(result["name"], "Bunny Express");
}

#[tokio::test]
async fn serves_the_delete_tool() {
    let (result, requests) = support::serve_tool_ok(
        vec![MockResponse::new(204)],
        "decision_platform_account_delete",
        json!({ "account_id": ACCOUNT_ID }),
    )
    .await;
    assert_eq!(requests[0].method, "DELETE");
    assert_eq!(requests[0].target, format!("/v1/organization/{ACCOUNT_ID}"));
    assert_eq!(result, Value::Null);
}
