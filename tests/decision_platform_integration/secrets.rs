//! Secrets collection operations.

use fico_sdk::decision::platform::{RunCreateParams, SecretsCreateParams, SecretsUpdateParams};
use fico_sdk::decision::{RunConfiguration, Secret, SecretType};

use crate::suite::Suite;

pub async fn run(suite: &Suite) {
    let platform = &suite.platform;
    let workspace_id = &suite.workspace_id;
    let location = "BURROW_ENTRANCE";
    let value = "Make 2 lefts and a hop forward";

    // We can create a secrets collection.
    let params = SecretsCreateParams {
        collection_id: None,
        name: None,
        description: None,
        secrets: vec![Secret {
            secret_type: SecretType::Env,
            location: location.to_owned(),
            value: Some(value.to_owned()),
        }],
    };
    let summary = platform.create_secrets(workspace_id, params).await.unwrap();
    let collection_id = summary.collection_id;

    // We can get a secrets collection. The platform masks the values of its
    // secrets: it never returns them.
    let collection = platform
        .get_secrets(workspace_id, &collection_id)
        .await
        .unwrap();
    assert_eq!(collection.secrets.len(), 1);
    assert_eq!(collection.secrets[0].location, location);
    let masked = collection.secrets[0].value.as_deref().unwrap_or_default();
    assert_ne!(masked, value);
    assert!(
        !masked.is_empty() && masked.chars().all(|c| c == '*'),
        "{masked}"
    );

    // We can list secrets collections.
    let collections = platform.list_secrets(workspace_id).await.unwrap();
    assert!(!collections.is_empty());
    assert!(collections.iter().any(|c| c.collection_id == collection_id));

    // We can update a secrets collection.
    let name = "Bunny Secrets";
    let description = "Secrets for bunny application";
    let params = SecretsUpdateParams {
        name: Some(name.to_owned()),
        description: Some(description.to_owned()),
        ..Default::default()
    };
    let summary = platform
        .update_secrets(workspace_id, &collection_id, params)
        .await
        .unwrap();
    assert_eq!(summary.name, name);
    assert_eq!(summary.description, description);

    // We can replace the secrets of a collection: the existing secret is
    // updated, a new one is created, and the masked values that the platform
    // returns never go back.
    let replaced = vec![
        Secret {
            secret_type: SecretType::Env,
            location: location.to_owned(),
            value: Some(value.to_owned()),
        },
        Secret {
            secret_type: SecretType::Env,
            location: "BURROW_EXIT".to_owned(),
            value: Some("Hop twice to the right".to_owned()),
        },
    ];
    let params = SecretsUpdateParams {
        secrets: Some(replaced),
        ..Default::default()
    };
    platform
        .update_secrets(workspace_id, &collection_id, params)
        .await
        .unwrap();
    let collection = platform
        .get_secrets(workspace_id, &collection_id)
        .await
        .unwrap();
    let mut locations: Vec<&str> = collection
        .secrets
        .iter()
        .map(|secret| secret.location.as_str())
        .collect();
    locations.sort_unstable();
    assert_eq!(locations, ["BURROW_ENTRANCE", "BURROW_EXIT"]);

    // We can start a run using the secrets collection.
    let params = RunCreateParams {
        configuration: Some(RunConfiguration {
            secrets_collection_id: Some(collection_id.clone()),
            ..Default::default()
        }),
        ..suite.input().create()
    };
    let created = platform.create_run(workspace_id, params).await.unwrap();
    suite.assert_succeeded(&created.run_id).await;

    // We can delete a secrets collection.
    platform
        .delete_secrets(workspace_id, &collection_id)
        .await
        .unwrap();
}
