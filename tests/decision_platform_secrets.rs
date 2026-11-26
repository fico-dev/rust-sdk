//! Tests for the platform secrets operations and their tools, against the
//! mock server.

mod mock_server;
mod support;

use fico_sdk::Error;
use fico_sdk::decision::platform::{SecretsCreateParams, SecretsUpdateParams};
use fico_sdk::decision::{Secret, SecretType, SecretsCollection};
use fico_sdk::tool::ToolError;
use mock_server::{MockResponse, MockServer};
use serde_json::{Value, json};

/// A secrets collection summary as the platform returns it.
fn summary_json() -> Value {
    json!({
        "id": "keys",
        "application_id": "routing",
        "name": "Keys",
        "description": "Keys of the map service.",
        "created_at": "2026-10-01T12:00:00Z",
        "updated_at": "2026-10-02T08:30:00Z",
    })
}

/// The secrets of [`collection_json`], with their values.
fn secrets_json() -> Value {
    json!([
        { "type": "env", "location": "MAP_API_KEY", "value": "key-123" },
        { "type": "file", "location": "licenses/solver.lic", "value": "license-456" },
    ])
}

/// A secrets collection as the platform returns it, with its values.
fn collection_json() -> Value {
    let mut collection = summary_json();
    collection["secrets"] = secrets_json();
    collection
}

/// The secrets of [`secrets_json`].
fn secrets() -> Vec<Secret> {
    vec![
        Secret {
            secret_type: SecretType::Env,
            location: "MAP_API_KEY".to_owned(),
            value: Some("key-123".to_owned()),
        },
        Secret {
            secret_type: SecretType::File,
            location: "licenses/solver.lic".to_owned(),
            value: Some("license-456".to_owned()),
        },
    ]
}

fn create_params() -> SecretsCreateParams {
    SecretsCreateParams {
        collection_id: Some("keys".to_owned()),
        name: Some("Keys".to_owned()),
        description: Some("Keys of the map service.".to_owned()),
        secrets: secrets(),
    }
}

#[tokio::test]
async fn creates_a_secrets_collection() {
    let server = MockServer::start(vec![MockResponse::json(200, summary_json())]).await;
    let summary = support::platform(&server)
        .create_secrets("routing", create_params())
        .await
        .unwrap();

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].target, "/v1/applications/routing/secrets");
    assert_eq!(
        support::body(&requests[0]),
        json!({
            "id": "keys",
            "name": "Keys",
            "description": "Keys of the map service.",
            "secrets": secrets_json(),
        })
    );
    assert_eq!(summary.collection_id, "keys");
    assert_eq!(summary.workspace_id, "routing");
    assert_eq!(summary.name, "Keys");
    assert_eq!(summary.created_at, "2026-10-01T12:00:00Z");
    assert_eq!(serde_json::to_value(&summary).unwrap(), summary_json());
}

#[tokio::test]
async fn names_a_new_collection_after_its_id() {
    let server = MockServer::start(vec![MockResponse::json(200, summary_json())]).await;
    let params = SecretsCreateParams {
        name: None,
        description: None,
        ..create_params()
    };
    support::platform(&server)
        .create_secrets("routing", params)
        .await
        .unwrap();
    let request = &server.requests()[0];
    assert_eq!(
        support::body(request),
        json!({ "id": "keys", "name": "keys", "secrets": secrets_json() })
    );
}

#[tokio::test]
async fn generates_an_id_when_none_is_set() {
    let server = MockServer::start(vec![MockResponse::json(200, summary_json())]).await;
    let params = SecretsCreateParams {
        collection_id: Some(String::new()),
        name: Some(String::new()),
        ..create_params()
    };
    support::platform(&server)
        .create_secrets("routing", params)
        .await
        .unwrap();
    let body = support::body(&server.requests()[0]);
    let id = body["id"].as_str().unwrap();
    let suffix = id.strip_prefix("secrets-").unwrap();
    assert_eq!(suffix.len(), 8, "{id}");
    assert!(
        suffix
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()),
        "{id}"
    );
    assert_eq!(body["name"], id);
}

#[tokio::test]
async fn needs_at_least_one_secret_to_create() {
    let server = MockServer::start(Vec::new()).await;
    let params = SecretsCreateParams {
        secrets: Vec::new(),
        ..create_params()
    };
    let err = support::platform(&server)
        .create_secrets("routing", params)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Invalid { .. }), "{err}");
    assert_eq!(
        err.to_string(),
        "invalid argument: `secrets` needs at least one secret"
    );
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn rejects_an_unknown_secret_type() {
    let server = MockServer::start(Vec::new()).await;
    let mut unknown = secrets();
    unknown[0].secret_type = SecretType::Unknown;
    let params = SecretsCreateParams {
        secrets: unknown,
        ..create_params()
    };
    let err = support::platform(&server)
        .create_secrets("routing", params)
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "invalid argument: `secrets[0].type` must be `env` or `file`"
    );
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn needs_a_value_for_every_secret() {
    let server = MockServer::start(Vec::new()).await;
    let platform = support::platform(&server);
    let mut stripped = secrets();
    stripped[1].value = None;
    let params = SecretsCreateParams {
        secrets: stripped.clone(),
        ..create_params()
    };
    let err = platform
        .create_secrets("routing", params)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Invalid { .. }), "{err}");
    assert_eq!(
        err.to_string(),
        "invalid argument: `secrets[1]` has no `value`"
    );
    let params = SecretsUpdateParams {
        secrets: Some(stripped),
        ..Default::default()
    };
    let err = platform
        .update_secrets("routing", "keys", params)
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "invalid argument: `secrets[1]` has no `value`"
    );
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn gets_a_secrets_collection() {
    let server = MockServer::start(vec![MockResponse::json(200, collection_json())]).await;
    let collection = support::platform(&server)
        .get_secrets("routing", "keys")
        .await
        .unwrap();
    let request = &server.requests()[0];
    assert_eq!(request.method, "GET");
    assert_eq!(request.target, "/v1/applications/routing/secrets/keys");
    assert_eq!(collection.summary.collection_id, "keys");
    assert_eq!(collection.summary.description, "Keys of the map service.");
    assert_eq!(collection.secrets, secrets());
    // Emitted JSON is the API's object, values included.
    assert_eq!(
        serde_json::to_value(&collection).unwrap(),
        collection_json()
    );
}

#[tokio::test]
async fn reads_new_secret_types_as_unknown() {
    let mut vault = collection_json();
    vault["secrets"][0]["type"] = json!("vault");
    let server = MockServer::start(vec![MockResponse::json(200, vault)]).await;
    let collection = support::platform(&server)
        .get_secrets("routing", "keys")
        .await
        .unwrap();
    assert_eq!(collection.secrets[0].secret_type, SecretType::Unknown);
    assert_eq!(collection.secrets[1].secret_type, SecretType::File);
}

#[test]
fn removes_secret_values() {
    let collection: SecretsCollection = serde_json::from_value(collection_json()).unwrap();
    let summary = collection.summary.clone();
    let stripped = collection.without_values();
    assert_eq!(stripped.summary, summary);
    assert!(stripped.secrets.iter().all(|s| s.value.is_none()));
    let mut expected = summary_json();
    expected["secrets"] = json!([
        { "type": "env", "location": "MAP_API_KEY" },
        { "type": "file", "location": "licenses/solver.lic" },
    ]);
    assert_eq!(serde_json::to_value(&stripped).unwrap(), expected);
    // What it emits reads back.
    let read: SecretsCollection = serde_json::from_value(expected).unwrap();
    assert_eq!(read, stripped);
}

#[test]
fn hides_secret_values_in_debug_output() {
    let collection: SecretsCollection = serde_json::from_value(collection_json()).unwrap();
    let update = SecretsUpdateParams {
        secrets: Some(secrets()),
        ..Default::default()
    };
    for debug in [
        format!("{:?}", create_params()),
        format!("{update:?}"),
        format!("{collection:?}"),
    ] {
        assert!(!debug.contains("key-123"), "{debug}");
        assert!(!debug.contains("license-456"), "{debug}");
        assert!(debug.contains("MAP_API_KEY"), "{debug}");
    }
    assert_eq!(
        format!("{:?}", secrets()[0]),
        r#"Secret { secret_type: Env, location: "MAP_API_KEY", value: Some("<redacted>") }"#
    );
    assert_eq!(
        format!("{:?}", collection.without_values().secrets[0]),
        r#"Secret { secret_type: Env, location: "MAP_API_KEY", value: None }"#
    );
}

#[tokio::test]
async fn lists_secrets_collections_across_pages() {
    let mut second = summary_json();
    second["id"] = json!("tokens");
    let server = MockServer::start(vec![
        MockResponse::json(
            200,
            json!({ "items": [summary_json()], "next_page_token": "page-2" }),
        ),
        MockResponse::json(200, json!({ "items": [second] })),
    ])
    .await;
    let collections = support::platform(&server)
        .list_secrets("routing")
        .await
        .unwrap();
    let ids: Vec<&str> = collections
        .iter()
        .map(|c| c.collection_id.as_str())
        .collect();
    assert_eq!(ids, ["keys", "tokens"]);

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert_eq!(request.method, "GET");
        assert!(
            request
                .target
                .starts_with("/v1/applications/routing/secrets?")
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
    let mut updated = summary_json();
    updated["description"] = json!("Keys of the map and traffic services.");
    let server = MockServer::start(vec![MockResponse::json(200, updated)]).await;
    let params = SecretsUpdateParams {
        description: Some("Keys of the map and traffic services.".to_owned()),
        ..Default::default()
    };
    let summary = support::platform(&server)
        .update_secrets("routing", "keys", params)
        .await
        .unwrap();

    // One request, without the secrets: the platform masks their values,
    // and rejects masked values sent back.
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "PATCH");
    assert_eq!(requests[0].target, "/v1/applications/routing/secrets/keys");
    assert_eq!(
        support::body(&requests[0]),
        json!({ "description": "Keys of the map and traffic services." })
    );
    assert_eq!(summary.description, "Keys of the map and traffic services.");
    assert_eq!(summary.name, "Keys");
}

#[tokio::test]
async fn replaces_every_secret_on_update() {
    // The platform returns masked values.
    let mut fetched = collection_json();
    fetched["secrets"][0]["value"] = json!("***************");
    fetched["secrets"][1]["value"] = json!("***************");
    let server = MockServer::start(vec![
        MockResponse::json(200, fetched),
        MockResponse::json(200, summary_json()),
    ])
    .await;
    let key = Secret {
        secret_type: SecretType::Env,
        location: "MAP_API_KEY".to_owned(),
        value: Some("key-789".to_owned()),
    };
    let token = Secret {
        secret_type: SecretType::Env,
        location: "TRAFFIC_TOKEN".to_owned(),
        value: Some("token-789".to_owned()),
    };
    let params = SecretsUpdateParams {
        name: Some("Tokens".to_owned()),
        secrets: Some(vec![key, token]),
        ..Default::default()
    };
    support::platform(&server)
        .update_secrets("routing", "keys", params)
        .await
        .unwrap();

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[0].target, "/v1/applications/routing/secrets/keys");
    assert_eq!(requests[1].method, "PATCH");
    assert_eq!(requests[1].target, "/v1/applications/routing/secrets/keys");
    // An existing key is updated, a new one created, and the rest deleted,
    // by type and location: no masked value goes back.
    assert_eq!(
        support::body(&requests[1]),
        json!({
            "name": "Tokens",
            "operations": [
                { "op": "update", "type": "env", "location": "MAP_API_KEY", "value": "key-789" },
                { "op": "create", "type": "env", "location": "TRAFFIC_TOKEN", "value": "token-789" },
                { "op": "delete", "type": "file", "location": "licenses/solver.lic" },
            ],
        })
    );
}

#[tokio::test]
async fn keeps_the_secrets_left_out_with_keep_existing() {
    let server = MockServer::start(vec![
        MockResponse::json(200, collection_json()),
        MockResponse::json(200, summary_json()),
    ])
    .await;
    let token = Secret {
        secret_type: SecretType::Env,
        location: "TRAFFIC_TOKEN".to_owned(),
        value: Some("token-789".to_owned()),
    };
    let params = SecretsUpdateParams {
        secrets: Some(vec![token]),
        keep_existing: true,
        ..Default::default()
    };
    support::platform(&server)
        .update_secrets("routing", "keys", params)
        .await
        .unwrap();
    assert_eq!(
        support::body(&server.requests()[1]),
        json!({
            "operations": [
                { "op": "create", "type": "env", "location": "TRAFFIC_TOKEN", "value": "token-789" },
            ],
        })
    );
}

#[tokio::test]
async fn serves_the_update_tool_with_keep_existing() {
    let (_, requests) = support::serve_tool_ok(
        vec![
            MockResponse::json(200, collection_json()),
            MockResponse::json(200, summary_json()),
        ],
        "decision_platform_secrets_update",
        json!({
            "workspace_id": "routing",
            "collection_id": "keys",
            "secrets": [{ "type": "env", "location": "MAP_API_KEY", "value": "key-789" }],
            "keep_existing": true,
        }),
    )
    .await;
    assert_eq!(
        support::body(&requests[1])["operations"],
        json!([{ "op": "update", "type": "env", "location": "MAP_API_KEY", "value": "key-789" }])
    );
}

#[tokio::test]
async fn deletes_secrets_of_types_the_sdk_does_not_know_by_their_name() {
    let mut fetched = collection_json();
    fetched["secrets"][1]["type"] = json!("vault");
    let server = MockServer::start(vec![
        MockResponse::json(200, fetched),
        MockResponse::json(200, summary_json()),
    ])
    .await;
    let params = SecretsUpdateParams {
        secrets: Some(secrets()[..1].to_vec()),
        ..Default::default()
    };
    support::platform(&server)
        .update_secrets("routing", "keys", params)
        .await
        .unwrap();
    assert_eq!(
        support::body(&server.requests()[1])["operations"],
        json!([
            { "op": "update", "type": "env", "location": "MAP_API_KEY", "value": "key-123" },
            { "op": "delete", "type": "vault", "location": "licenses/solver.lic" },
        ])
    );
}

#[tokio::test]
async fn rejects_the_same_secret_twice_in_an_update() {
    let server = MockServer::start(Vec::new()).await;
    let twice = vec![secrets()[0].clone(), secrets()[0].clone()];
    let params = SecretsUpdateParams {
        secrets: Some(twice),
        ..Default::default()
    };
    let err = support::platform(&server)
        .update_secrets("routing", "keys", params)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Invalid { .. }), "{err}");
    assert_eq!(
        err.to_string(),
        "invalid argument: `secrets` has the env secret `MAP_API_KEY` more than once"
    );
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn rejects_an_empty_update() {
    let server = MockServer::start(Vec::new()).await;
    let err = support::platform(&server)
        .update_secrets("routing", "keys", SecretsUpdateParams::default())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Invalid { .. }), "{err}");
    assert_eq!(
        err.to_string(),
        "invalid argument: set at least one of `name`, `description` and `secrets`"
    );
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn needs_at_least_one_secret_to_update() {
    let server = MockServer::start(Vec::new()).await;
    let params = SecretsUpdateParams {
        secrets: Some(Vec::new()),
        ..Default::default()
    };
    let err = support::platform(&server)
        .update_secrets("routing", "keys", params)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Invalid { .. }), "{err}");
    assert_eq!(
        err.to_string(),
        "invalid argument: `secrets` needs at least one secret"
    );
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn deletes_a_secrets_collection() {
    let server = MockServer::start(vec![MockResponse::new(204)]).await;
    support::platform(&server)
        .delete_secrets("routing", "keys")
        .await
        .unwrap();
    let request = &server.requests()[0];
    assert_eq!(request.method, "DELETE");
    assert_eq!(request.target, "/v1/applications/routing/secrets/keys");
}

#[tokio::test]
async fn encodes_collection_ids_in_paths() {
    let server = MockServer::start(vec![MockResponse::json(200, collection_json())]).await;
    let platform = support::platform(&server);
    platform
        .get_secrets("my workspace", "keys?v2")
        .await
        .unwrap();
    assert_eq!(
        server.requests()[0].target,
        "/v1/applications/my%20workspace/secrets/keys%3Fv2"
    );
    for (workspace_id, collection_id) in [
        ("routing", ".."),
        ("..", "keys"),
        ("routing", ""),
        ("routing", "keys/v2"),
    ] {
        let err = platform
            .delete_secrets(workspace_id, collection_id)
            .await
            .unwrap_err();
        assert!(
            matches!(err, Error::Invalid { .. }),
            "{collection_id:?}: {err}"
        );
    }
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn requires_every_field_of_a_secret_in_tool_schemas() {
    let create = support::tool("decision_platform_secrets_create");
    let schema = create.input_schema();
    assert!(
        schema["required"]
            .as_array()
            .unwrap()
            .contains(&json!("secrets"))
    );
    let update = support::tool("decision_platform_secrets_update");
    for (name, schema) in [("create", schema), ("update", update.input_schema())] {
        let secret = &schema["properties"]["secrets"]["items"];
        assert_eq!(
            secret["required"],
            json!(["type", "location", "value"]),
            "{name}"
        );
        assert_eq!(secret["properties"]["value"]["type"], "string", "{name}");
    }
}

#[tokio::test]
async fn serves_the_create_tool() {
    let (result, requests) = support::serve_tool(
        vec![MockResponse::json(200, summary_json())],
        "decision_platform_secrets_create",
        json!({ "workspace_id": "routing", "collection_id": "keys", "secrets": secrets_json() }),
    )
    .await;
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].target, "/v1/applications/routing/secrets");
    assert_eq!(
        support::body(&requests[0]),
        json!({ "id": "keys", "name": "keys", "secrets": secrets_json() })
    );
    assert_eq!(result.unwrap(), summary_json());
}

#[tokio::test]
async fn rejects_bad_secrets_in_tool_arguments() {
    let mut vault = secrets_json();
    vault[0]["type"] = json!("vault");
    let (result, requests) = support::serve_tool(
        Vec::new(),
        "decision_platform_secrets_create",
        json!({ "workspace_id": "routing", "secrets": vault }),
    )
    .await;
    let err = result.unwrap_err();
    assert!(matches!(err, ToolError::InvalidArguments(_)), "{err}");
    assert_eq!(
        err.to_string(),
        "invalid arguments: unknown variant `vault`, expected `env` or `file`"
    );
    assert!(requests.is_empty());

    let mut stripped = secrets_json();
    stripped[0].as_object_mut().unwrap().remove("value");
    let (result, requests) = support::serve_tool(
        Vec::new(),
        "decision_platform_secrets_create",
        json!({ "workspace_id": "routing", "secrets": stripped }),
    )
    .await;
    let err = result.unwrap_err();
    assert!(
        matches!(err, ToolError::Failed(Error::Invalid { .. })),
        "{err}"
    );
    assert!(requests.is_empty());
}

#[tokio::test]
async fn serves_the_get_tool_without_values() {
    let (result, requests) = support::serve_tool(
        vec![MockResponse::json(200, collection_json())],
        "decision_platform_secrets_get",
        json!({ "workspace_id": "routing", "collection_id": "keys" }),
    )
    .await;
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[0].target, "/v1/applications/routing/secrets/keys");
    let result = result.unwrap();
    assert_eq!(result["id"], "keys");
    assert_eq!(
        result["secrets"],
        json!([
            { "type": "env", "location": "MAP_API_KEY" },
            { "type": "file", "location": "licenses/solver.lic" },
        ])
    );
}

#[tokio::test]
async fn serves_the_list_tool() {
    let (result, requests) = support::serve_tool(
        vec![MockResponse::json(
            200,
            json!({ "items": [summary_json()] }),
        )],
        "decision_platform_secrets_list",
        json!({ "workspace_id": "routing" }),
    )
    .await;
    assert_eq!(
        requests[0].target,
        "/v1/applications/routing/secrets?pagereturn=true"
    );
    assert_eq!(result.unwrap(), json!([summary_json()]));
}

#[tokio::test]
async fn serves_the_update_tool() {
    let mut updated = summary_json();
    updated["name"] = json!("Map keys");
    let (result, requests) = support::serve_tool(
        vec![MockResponse::json(200, updated.clone())],
        "decision_platform_secrets_update",
        json!({ "workspace_id": "routing", "collection_id": "keys", "name": "Map keys" }),
    )
    .await;
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "PATCH");
    assert_eq!(requests[0].target, "/v1/applications/routing/secrets/keys");
    assert_eq!(support::body(&requests[0]), json!({ "name": "Map keys" }));
    assert_eq!(result.unwrap(), updated);
}

#[tokio::test]
async fn serves_the_delete_tool() {
    let (result, requests) = support::serve_tool(
        vec![MockResponse::new(204)],
        "decision_platform_secrets_delete",
        json!({ "workspace_id": "routing", "collection_id": "keys" }),
    )
    .await;
    assert_eq!(requests[0].method, "DELETE");
    assert_eq!(requests[0].target, "/v1/applications/routing/secrets/keys");
    assert_eq!(result.unwrap(), Value::Null);
}
