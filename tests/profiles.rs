//! Tests for the whole-call profile functions of `fico_sdk::config` and
//! `fico_sdk::auth`: create, get, list and delete a profile, and log
//! profiles in and out. `tests/mock_server` plays the identity provider and
//! the platform, and reqwest plays the browser.

mod mock_server;
mod support;

use std::path::Path;

use fico_sdk::config::{
    self, Config, EndpointOidc, Env, PkceParams, Profile, ProfileCreated, ProfileParams,
};
use fico_sdk::{Error, ProfileProblem};
use serde_json::{Value, json};

use crate::mock_server::{MockResponse, MockServer};
use crate::support::{TempDir, jwt, rfc3339, unix_now};

const CLIENT_ID: &str = "client-1";

/// A fake API key, built from parts so that secret scanners don't read it
/// as a credential.
fn key() -> String {
    ["sk", "test", "1234abcd"].join("-")
}

fn api_key(endpoint: &str) -> Profile {
    Profile::ApiKey {
        endpoint: Some(endpoint.to_owned()),
        api_key: key(),
    }
}

fn pkce(endpoint: &str, session: &str) -> Profile {
    Profile::Pkce {
        endpoint: Some(endpoint.to_owned()),
        auth_session: Some(session.to_owned()),
        team_id: Some("t-1".to_owned()),
        sso_domain: None,
    }
}

/// Writes the configuration with `profiles`, in order; the first is the
/// default. Also writes stored tokens for each PKCE session.
async fn write(home: &Path, profiles: &[(&str, Profile)]) {
    let mut config = Config::default();
    for (name, profile) in profiles {
        config.set_profile(name, profile.clone(), false).unwrap();
        if let Profile::Pkce { auth_session, .. } = profile {
            let dir = home.join("auth").join(auth_session.as_deref().unwrap());
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("tokens.json"), "{}").unwrap();
        }
    }
    config.save(home).await.unwrap();
}

fn has_tokens(home: &Path, session: &str) -> bool {
    home.join("auth").join(session).join("tokens.json").exists()
}

// The config file.

#[tokio::test]
async fn creates_the_home_and_the_file_on_the_first_write() {
    let parent = TempDir::new("profiles");
    let home = parent.path().join("not").join("yet");
    assert!(!home.exists());
    let mut config = Config::default();
    config
        .set_profile("ci", api_key("https://api.example.com"), false)
        .unwrap();
    config.save(&home).await.unwrap();
    assert!(Config::path(&home).is_file());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(Config::path(&home))
            .unwrap()
            .permissions();
        assert_eq!(mode.mode() & 0o777, 0o600);
    }
}

// Get and list.

#[tokio::test]
async fn gets_a_profile_with_its_key_masked() {
    let home = TempDir::new("profiles");
    write(
        home.path(),
        &[
            ("ci", api_key("https://api.example.com")),
            ("work", pkce("https://api.example.com", "work")),
        ],
    )
    .await;

    let ci = config::get_profile(home.path(), " ci ").await.unwrap();
    assert_eq!(ci.name, "ci");
    assert!(ci.default);
    let Profile::ApiKey { api_key, .. } = &ci.profile else {
        panic!("{ci:?}");
    };
    assert_eq!(api_key, "sk****cd");
    assert!(!format!("{ci:?}").contains(&key()));

    let work = config::get_profile(home.path(), "work").await.unwrap();
    assert!(!work.default);

    let err = config::get_profile(home.path(), "missing")
        .await
        .unwrap_err();
    assert!(
        matches!(&err, Error::NotFound { kind: "profile", id, .. } if id == "missing"),
        "{err}"
    );
}

#[tokio::test]
async fn lists_the_default_profile_first_with_keys_masked() {
    let home = TempDir::new("profiles");
    write(
        home.path(),
        &[
            ("a", api_key("https://api.example.com")),
            ("b", api_key("https://api.example.com")),
            ("c", api_key("https://api.example.com")),
        ],
    )
    .await;
    let mut config = Config::load(home.path()).await.unwrap();
    config.default_profile = Some("b".to_owned());
    config.save(home.path()).await.unwrap();

    let profiles = config::list_profiles(home.path()).await.unwrap();
    let names: Vec<&str> = profiles.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["b", "a", "c"]);
    assert_eq!(
        profiles.iter().map(|p| p.default).collect::<Vec<_>>(),
        [true, false, false]
    );
    assert!(!format!("{profiles:?}").contains(&key()));
    let json = serde_json::to_string(&profiles).unwrap();
    assert!(
        json.contains("sk****cd") && !json.contains(&key()),
        "{json}"
    );
}

#[tokio::test]
async fn reads_a_missing_config_file_as_no_profiles() {
    let parent = TempDir::new("profiles");
    let home = parent.path().join("none");
    assert!(config::list_profiles(&home).await.unwrap().is_empty());
    let err = config::get_profile(&home, "ci").await.unwrap_err();
    assert!(matches!(err, Error::NotFound { .. }), "{err}");
    assert!(!home.exists(), "reading must not create the home");
}

// Delete.

#[tokio::test]
async fn deletes_the_tokens_of_a_session_that_no_other_profile_uses() {
    let home = TempDir::new("profiles");
    write(
        home.path(),
        &[
            ("ci", api_key("https://api.example.com")),
            ("work", pkce("https://api.example.com", "work")),
            ("home", pkce("https://api.example.com", "shared")),
            ("also", pkce("https://api.example.com", "shared")),
        ],
    )
    .await;

    // A session that another profile shares keeps its tokens.
    let removed = config::delete_profile(home.path(), "also", None)
        .await
        .unwrap();
    assert_eq!(removed.name, "also");
    assert!(removed.unused_session.is_none());
    assert!(has_tokens(home.path(), "shared"));

    // The last profile of a session takes its tokens along.
    let removed = config::delete_profile(home.path(), "home", None)
        .await
        .unwrap();
    let unused = removed.unused_session.unwrap();
    assert_eq!(unused.session, "shared");
    assert!(matches!(unused.deleted, Ok(true)));
    assert!(!has_tokens(home.path(), "shared"));
    assert!(has_tokens(home.path(), "work"));

    // A session of one profile.
    let removed = config::delete_profile(home.path(), "work", None)
        .await
        .unwrap();
    assert!(removed.unused_session.is_some());
    assert!(!has_tokens(home.path(), "work"));

    // An API-key profile has no session.
    write(home.path(), &[("ci", api_key("https://api.example.com"))]).await;
    let removed = config::delete_profile(home.path(), "ci", None)
        .await
        .unwrap();
    assert!(removed.unused_session.is_none());
    assert_eq!(removed.new_default, None);
    write(home.path(), &[("ci", api_key("https://api.example.com"))]).await;
    let config = Config::load(home.path()).await.unwrap();
    assert_eq!(config.profiles.keys().collect::<Vec<_>>(), ["ci"]);
}

#[tokio::test]
async fn deleting_the_default_profile_names_the_others_or_a_new_default() {
    let home = TempDir::new("profiles");
    write(
        home.path(),
        &[
            ("a", api_key("https://api.example.com")),
            ("c", api_key("https://api.example.com")),
            ("b", api_key("https://api.example.com")),
        ],
    )
    .await;
    let before = std::fs::read(Config::path(home.path())).unwrap();

    let err = config::delete_profile(home.path(), "a", None)
        .await
        .unwrap_err();
    assert!(
        matches!(
            &err,
            Error::Profile { profile, problem: ProfileProblem::NewDefaultNeeded { profiles, .. }, .. }
                if profile == "a" && profiles == &["b", "c"]
        ),
        "{err}"
    );
    let err = config::delete_profile(home.path(), "zzz", None)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::NotFound { .. }), "{err}");
    assert_eq!(std::fs::read(Config::path(home.path())).unwrap(), before);

    let removed = config::delete_profile(home.path(), "a", Some("c"))
        .await
        .unwrap();
    assert_eq!(removed.new_default.as_deref(), Some("c"));
    assert_eq!(
        Config::load(home.path())
            .await
            .unwrap()
            .default_profile
            .as_deref(),
        Some("c")
    );
}

// Create.

/// The ID token an identity provider issues for `CLIENT_ID`, expiring in
/// `expires_in` seconds.
fn id_token(expires_in: i64) -> String {
    jwt(json!({"aud": CLIENT_ID, "exp": unix_now() + expires_in, "sub": "ada"}))
}

/// The discovery document of an identity provider at `url`.
fn discovery(url: &str) -> MockResponse {
    MockResponse::json(
        200,
        json!({
            "issuer": url,
            "authorization_endpoint": format!("{url}/oauth2/authorize"),
            "token_endpoint": format!("{url}/oauth2/token"),
            "end_session_endpoint": format!("{url}/logout"),
        }),
    )
}

/// A token response with an ID token that lasts an hour.
fn token_response() -> MockResponse {
    MockResponse::json(
        200,
        json!({
            "access_token": "access-1",
            "id_token": id_token(3600),
            "refresh_token": "refresh-1",
            "token_type": "Bearer",
            "expires_in": 3600,
        }),
    )
}

/// The user's teams, in no order.
fn teams() -> MockResponse {
    MockResponse::json(
        200,
        json!([
            {"id": "t-2", "name": "Owl", "role": "admin"},
            {"id": "t-1", "name": "Hare"},
        ]),
    )
}

/// Stores valid tokens for `session`, as a login to `endpoint` would.
fn store_tokens(home: &Path, session: &str, endpoint: &str) {
    let dir = home.join("auth").join(session);
    std::fs::create_dir_all(&dir).unwrap();
    let tokens = json!({
        "endpoint": endpoint,
        "token_type": "Bearer",
        "access_token": "access-0",
        "id_token": id_token(7200),
        "refresh_token": "refresh-0",
        "expires_at": rfc3339(unix_now() + 3600),
    });
    std::fs::write(dir.join("tokens.json"), tokens.to_string()).unwrap();
}

fn oidc(url: &str) -> EndpointOidc {
    EndpointOidc {
        oidc_discovery_url: format!("{url}/.well-known/openid-configuration"),
        client_id: CLIENT_ID.to_owned(),
        callback_port: Some(0),
    }
}

/// The parameters of a PKCE profile `work` on `server`, which is also its
/// identity provider, with the OIDC settings given.
fn params(server: &MockServer, team: Option<&str>) -> PkceParams {
    PkceParams {
        endpoint: Some(server.url().to_owned()),
        auth_session: Some("work".to_owned()),
        oidc_discovery_url: Some(format!("{}/.well-known/openid-configuration", server.url())),
        client_id: Some(CLIENT_ID.to_owned()),
        team: team.map(str::to_owned),
        ..PkceParams::default()
    }
}

/// Stores the endpoint's OIDC settings, with a free callback port.
async fn store_oidc(home: &Path, server: &MockServer) {
    let mut config = Config::load(home).await.unwrap();
    config
        .set_endpoint_oidc(server.url(), oidc(server.url()))
        .unwrap();
    config.save(home).await.unwrap();
}

/// Creates the profile `work` and plays the browser: it sends the callback
/// of a successful sign-in when `on_login` is called. Returns the result and
/// how many logins started.
async fn create(home: &Path, params: PkceParams) -> (fico_sdk::Result<ProfileCreated>, usize) {
    let mut logins = 0;
    let result =
        config::create_profile(home, "work", ProfileParams::Pkce(params), false, |login| {
            logins += 1;
            let state = reqwest::Url::parse(login.authorization_url())
                .unwrap()
                .query_pairs()
                .find(|(key, _)| key == "state")
                .unwrap()
                .1
                .into_owned();
            let url = reqwest::Url::parse_with_params(
                login.redirect_uri(),
                [("code", "code-1"), ("state", state.as_str())],
            )
            .unwrap();
            tokio::spawn(reqwest::get(url));
        })
        .await;
    (result, logins)
}

fn stored_tokens(home: &Path, session: &str) -> Option<Value> {
    let text = std::fs::read_to_string(home.join("auth").join(session).join("tokens.json")).ok()?;
    Some(serde_json::from_str(&text).unwrap())
}

#[tokio::test]
async fn creates_a_pkce_profile_with_its_login_on_the_first_write() {
    let server =
        MockServer::start_with(|url| vec![discovery(url), token_response(), teams()]).await;
    let parent = TempDir::new("profiles");
    let home = parent.path().join("fresh");
    // A free callback port, so that parallel tests don't share one.
    store_oidc(&home, &server).await;
    let mut given = params(&server, Some(" hare "));
    given.oidc_discovery_url = None;
    given.client_id = None;

    let (created, logins) = create(&home, given).await;
    let created = created.unwrap();
    assert_eq!(logins, 1);
    assert_eq!(created.profile.name, "work");
    assert!(created.profile.default, "the first profile is the default");
    assert!(!created.oidc_overridden);
    let session = created.session.unwrap();
    assert_eq!(session.session, "work");
    assert!(session.logged_in);
    let team = created.team.unwrap();
    assert_eq!((team.team_id.as_str(), team.name.as_str()), ("t-1", "Hare"));
    assert!(created.stale_session.is_none());

    // The home, the profile, the OIDC settings and the tokens are saved.
    let config = Config::load(&home).await.unwrap();
    let Profile::Pkce {
        team_id,
        auth_session,
        ..
    } = &config.profiles["work"]
    else {
        panic!("{config:?}");
    };
    assert_eq!(team_id.as_deref(), Some("t-1"));
    assert_eq!(auth_session.as_deref(), Some("work"));
    assert_eq!(
        config.endpoint_oidc(server.url()).unwrap().client_id,
        CLIENT_ID
    );
    assert!(stored_tokens(&home, "work").is_some());
    assert_eq!(server.requests().len(), 3);
    assert_eq!(server.requests()[2].target, "/v1/internal/me/organization");
}

#[tokio::test]
async fn uses_valid_tokens_instead_of_logging_in() {
    let server = MockServer::start(vec![teams()]).await;
    let home = TempDir::new("profiles");
    store_oidc(home.path(), &server).await;
    store_tokens(home.path(), "work", server.url());

    let mut given = params(&server, Some("Owl"));
    given.oidc_discovery_url = None;
    given.client_id = None;
    let (created, logins) = create(home.path(), given).await;
    let created = created.unwrap();
    assert_eq!(logins, 0);
    assert!(!created.session.unwrap().logged_in);
    assert_eq!(created.team.unwrap().team_id, "t-2");
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn merges_given_oidc_settings_over_the_stored_ones() {
    let server = MockServer::start(vec![teams()]).await;
    let home = TempDir::new("profiles");
    let mut stored = Config::default();
    stored
        .set_endpoint_oidc(
            server.url(),
            EndpointOidc {
                callback_port: Some(0),
                ..oidc(server.url())
            },
        )
        .unwrap();
    stored.save(home.path()).await.unwrap();
    store_tokens(home.path(), "work", server.url());

    // Only the discovery URL is given: the client ID and the callback port
    // stay.
    let mut given = params(&server, Some("Hare"));
    given.client_id = None;
    given.oidc_discovery_url = Some(format!("{}/other", server.url()));
    let created = create(home.path(), given).await.0.unwrap();
    assert!(created.oidc_overridden);
    let after = Config::load(home.path()).await.unwrap();
    let oidc = after.endpoint_oidc(server.url()).unwrap();
    assert_eq!(
        oidc.oidc_discovery_url,
        format!("{}/other/.well-known/openid-configuration", server.url())
    );
    assert_eq!(oidc.client_id, CLIENT_ID);
    assert_eq!(oidc.callback_port, Some(0));
}

#[tokio::test]
async fn asks_for_the_oidc_settings_before_any_request() {
    let server = MockServer::start(vec![]).await;
    let home = TempDir::new("profiles");
    for (url, client_id) in [(false, false), (true, false), (false, true)] {
        let mut given = params(&server, Some("Hare"));
        if !url {
            given.oidc_discovery_url = None;
        }
        if !client_id {
            given.client_id = None;
        }
        let (result, logins) = create(home.path(), given).await;
        let err = result.unwrap_err();
        assert!(
            matches!(
                &err,
                Error::Profile { profile, problem: ProfileProblem::NoOidc { endpoint }, .. }
                    if profile == "work" && endpoint == server.url()
            ),
            "{err}"
        );
        assert_eq!(logins, 0);
    }
    assert!(server.requests().is_empty());
    assert!(Config::load(home.path()).await.unwrap().profiles.is_empty());
}

#[tokio::test]
async fn checks_the_profile_before_any_login() {
    let server = MockServer::start(vec![]).await;
    let home = TempDir::new("profiles");
    let mut given = params(&server, Some("Hare"));
    given.auth_session = None;
    let err = create(home.path(), given).await.0.unwrap_err();
    assert!(
        matches!(
            &err,
            Error::Profile {
                problem: ProfileProblem::NoSession { .. },
                ..
            }
        ),
        "{err}"
    );
    let mut given = params(&server, Some("Hare"));
    given.endpoint = Some("ftp://nope".to_owned());
    let err = create(home.path(), given).await.0.unwrap_err();
    assert!(matches!(err, Error::Invalid { .. }), "{err}");
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn lists_the_teams_when_none_is_given_and_keeps_the_login() {
    let server =
        MockServer::start_with(|url| vec![discovery(url), token_response(), teams(), teams()])
            .await;
    let home = TempDir::new("profiles");
    store_oidc(home.path(), &server).await;
    let mut given = params(&server, None);
    given.oidc_discovery_url = None;
    given.client_id = None;

    let (result, logins) = create(home.path(), given.clone()).await;
    let err = result.unwrap_err();
    assert_eq!(logins, 1, "{err}");
    assert!(
        matches!(
            &err,
            Error::Profile { problem: ProfileProblem::Team { team: None, teams, .. }, .. }
                if teams == &["Hare", "Owl"]
        ),
        "{err}"
    );
    // The profile is not saved, but the login is kept.
    assert!(Config::load(home.path()).await.unwrap().profiles.is_empty());
    assert!(stored_tokens(home.path(), "work").is_some());

    // The second call, with a team, logs in no more.
    given.team = Some("Owl".to_owned());
    let (result, logins) = create(home.path(), given).await;
    assert_eq!(logins, 0);
    assert!(!result.unwrap().session.unwrap().logged_in);
    assert_eq!(server.requests().len(), 4);
}

#[tokio::test]
async fn rejects_an_unknown_team_and_a_failed_team_lookup() {
    let server = MockServer::start(vec![
        teams(),
        MockResponse::json(403, json!({"message": "no"})),
    ])
    .await;
    let home = TempDir::new("profiles");
    store_oidc(home.path(), &server).await;
    store_tokens(home.path(), "work", server.url());
    let before = std::fs::read(Config::path(home.path())).unwrap();

    let mut given = params(&server, Some("Fox"));
    given.oidc_discovery_url = None;
    given.client_id = None;
    let err = create(home.path(), given.clone()).await.0.unwrap_err();
    assert!(
        matches!(
            &err,
            Error::Profile { problem: ProfileProblem::Team { team: Some(team), .. }, .. }
                if team == "Fox"
        ),
        "{err}"
    );
    given.team = Some("Hare".to_owned());
    let err = create(home.path(), given).await.0.unwrap_err();
    assert!(matches!(err, Error::Status { status: 403, .. }), "{err}");
    assert_eq!(std::fs::read(Config::path(home.path())).unwrap(), before);
}

#[tokio::test]
async fn deletes_the_tokens_of_the_session_a_change_leaves_unused() {
    let server = MockServer::start(vec![teams()]).await;
    let home = TempDir::new("profiles");
    store_oidc(home.path(), &server).await;
    let mut config = Config::load(home.path()).await.unwrap();
    config
        .set_profile("work", pkce(server.url(), "old"), false)
        .unwrap();
    config.save(home.path()).await.unwrap();
    store_tokens(home.path(), "old", server.url());
    store_tokens(home.path(), "work", server.url());

    // To another session.
    let mut given = params(&server, Some("Hare"));
    given.oidc_discovery_url = None;
    given.client_id = None;
    let created = create(home.path(), given).await.0.unwrap();
    let stale = created.stale_session.unwrap();
    assert_eq!(stale.session, "old");
    assert!(matches!(stale.deleted, Ok(true)));
    assert!(stored_tokens(home.path(), "old").is_none());
    assert!(stored_tokens(home.path(), "work").is_some());

    // Switching to an API key leaves the session unused.
    let created = config::create_profile(
        home.path(),
        "work",
        ProfileParams::ApiKey {
            endpoint: Some(server.url().to_owned()),
            api_key: key(),
        },
        false,
        |_| {},
    )
    .await
    .unwrap();
    assert_eq!(created.stale_session.unwrap().session, "work");
    assert!(stored_tokens(home.path(), "work").is_none());
    assert!(created.session.is_none() && created.team.is_none());
    let Profile::ApiKey { api_key, .. } = created.profile.profile else {
        panic!();
    };
    assert_eq!(api_key, "sk****cd");
}

#[tokio::test]
async fn creates_an_api_key_profile_on_the_first_write() {
    let parent = TempDir::new("profiles");
    let home = parent.path().join("fresh");
    let created = config::create_profile(
        &home,
        " ci ",
        ProfileParams::ApiKey {
            endpoint: Some("api.example.com".to_owned()),
            api_key: key(),
        },
        true,
        |_| panic!("an API key needs no login"),
    )
    .await
    .unwrap();
    assert_eq!(created.profile.name, "ci");
    assert!(created.profile.default);
    assert!(!format!("{created:?}").contains(&key()));
    let config = Config::load(&home).await.unwrap();
    assert!(matches!(&config.profiles["ci"], Profile::ApiKey { api_key, .. } if *api_key == key()));

    // Without an endpoint, the profile is saved on the default one.
    let created = config::create_profile(
        &home,
        "plain",
        ProfileParams::ApiKey {
            endpoint: None,
            api_key: key(),
        },
        false,
        |_| {},
    )
    .await
    .unwrap();
    assert!(matches!(
        created.profile.profile,
        Profile::ApiKey { endpoint: None, .. }
    ));
    let config = Config::load(&home).await.unwrap();
    let resolved = config
        .resolve(Some("plain"), &Env::default())
        .unwrap()
        .unwrap();
    assert_eq!(
        resolved.endpoint,
        config::DEFAULT_ENDPOINT.expect("a default endpoint")
    );
    assert!(!format!("{created:?}").contains(&key()));
}

// Login and logout over several profiles.

/// A configuration with three PKCE profiles, `alpha` (the default) logging
/// in at `good`, `beta` on an endpoint without OIDC settings, and `gamma`
/// at `bad`, whose identity provider fails; `delta` shares `alpha`'s
/// session, and `ci` has an API key.
async fn several(home: &Path, good: &MockServer, bad: &MockServer) {
    let mut config = Config::default();
    config
        .set_endpoint_oidc(good.url(), oidc(good.url()))
        .unwrap();
    config
        .set_endpoint_oidc(bad.url(), oidc(bad.url()))
        .unwrap();
    for (name, endpoint, session) in [
        ("alpha", good.url(), "zeta"),
        ("beta", "https://nowhere.example.com", "beta"),
        ("gamma", bad.url(), "gamma"),
        ("delta", good.url(), "zeta"),
    ] {
        config
            .set_profile(name, pkce(endpoint, session), false)
            .unwrap();
    }
    config
        .set_profile("ci", api_key(good.url()), false)
        .unwrap();
    config.default_profile = Some("alpha".to_owned());
    config.save(home).await.unwrap();
}

/// What the browser does for a login that succeeds.
fn sign_in(login: &fico_sdk::auth::PendingLogin) {
    let state = reqwest::Url::parse(login.authorization_url())
        .unwrap()
        .query_pairs()
        .find(|(key, _)| key == "state")
        .unwrap()
        .1
        .into_owned();
    let url = reqwest::Url::parse_with_params(
        login.redirect_uri(),
        [("code", "code-1"), ("state", state.as_str())],
    )
    .unwrap();
    tokio::spawn(reqwest::get(url));
}

#[tokio::test]
async fn logs_in_every_pkce_session_once_and_reports_each() {
    use fico_sdk::auth::{self, LoginOptions, SessionLogin};
    let good = MockServer::start_with(|url| vec![discovery(url), token_response()]).await;
    let bad = MockServer::start(vec![MockResponse::new(404)]).await;
    let home = TempDir::new("profiles");
    several(home.path(), &good, &bad).await;

    let mut shown = 0;
    let results = auth::login_profiles(home.path(), None, LoginOptions::new(), |login| {
        shown += 1;
        sign_in(login);
    })
    .await
    .unwrap();
    assert_eq!(shown, 1);

    // The default profile's session first, then the others by profile name.
    assert_eq!(results.len(), 3, "{results:?}");
    let SessionLogin::LoggedIn(zeta) = &results[0] else {
        panic!("{results:?}");
    };
    assert_eq!(zeta.session, "zeta");
    assert_eq!(zeta.profiles, ["alpha", "delta"]);
    assert!(
        matches!(&results[1], SessionLogin::Skipped { session, .. } if session.session == "beta"),
        "{results:?}"
    );
    assert!(
        matches!(&results[2], SessionLogin::Failed { session, .. } if session.session == "gamma"),
        "{results:?}"
    );
    assert!(stored_tokens(home.path(), "zeta").is_some());
    assert!(stored_tokens(home.path(), "gamma").is_none());
    assert!(!format!("{results:?}").contains("code-1"));
}

#[tokio::test]
async fn logs_in_one_profile_and_checks_it_first() {
    use fico_sdk::auth::{self, LoginOptions, SessionLogin};
    let good = MockServer::start_with(|url| vec![discovery(url), token_response()]).await;
    let bad = MockServer::start(vec![]).await;
    let home = TempDir::new("profiles");
    several(home.path(), &good, &bad).await;

    let results = auth::login_profiles(home.path(), Some(" delta "), LoginOptions::new(), sign_in)
        .await
        .unwrap();
    assert!(matches!(results.as_slice(), [SessionLogin::LoggedIn(l)] if l.profiles == ["delta"]));

    let err = auth::login_profiles(home.path(), Some("ci"), LoginOptions::new(), |_| {})
        .await
        .unwrap_err();
    assert!(
        matches!(
            &err,
            Error::Profile { profile, problem: ProfileProblem::NotPkce { auth_type: "api_key", .. }, .. }
                if profile == "ci"
        ),
        "{err}"
    );
    let err = auth::login_profiles(home.path(), Some("nope"), LoginOptions::new(), |_| {})
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            Error::NotFound {
                kind: "profile",
                ..
            }
        ),
        "{err}"
    );
}

#[tokio::test]
async fn logs_out_every_session_sorted_and_reports_missing_tokens() {
    use fico_sdk::auth::{self, SessionLogout};
    let good = MockServer::start(vec![]).await;
    let bad = MockServer::start(vec![]).await;
    let home = TempDir::new("profiles");
    several(home.path(), &good, &bad).await;
    store_tokens(home.path(), "zeta", good.url());
    store_tokens(home.path(), "gamma", bad.url());

    let results = auth::logout_profiles(home.path(), None).await.unwrap();
    let seen: Vec<(&str, bool)> = results
        .iter()
        .map(|result| match result {
            SessionLogout::LoggedOut {
                session,
                had_tokens,
                ..
            } => (session.session.as_str(), *had_tokens),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(seen, [("beta", false), ("gamma", true), ("zeta", true)]);
    assert!(stored_tokens(home.path(), "zeta").is_none());
    assert!(stored_tokens(home.path(), "gamma").is_none());

    let err = auth::logout_profiles(home.path(), Some("ci"))
        .await
        .unwrap_err();
    assert!(
        matches!(
            &err,
            Error::Profile {
                problem: ProfileProblem::NotPkce { .. },
                ..
            }
        ),
        "{err}"
    );
    let err = auth::logout_profiles(home.path(), Some("nope"))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::NotFound { .. }), "{err}");
}

#[tokio::test]
async fn logs_out_a_failing_session_without_stopping_the_others() {
    use fico_sdk::auth::{self, SessionLogout};
    let good = MockServer::start(vec![]).await;
    let bad = MockServer::start(vec![]).await;
    let home = TempDir::new("profiles");
    several(home.path(), &good, &bad).await;
    store_tokens(home.path(), "zeta", good.url());
    // A directory where the token file belongs can't be deleted as a file.
    std::fs::create_dir_all(home.path().join("auth").join("gamma").join("tokens.json")).unwrap();

    let results = auth::logout_profiles(home.path(), None).await.unwrap();
    assert!(
        matches!(&results[1], SessionLogout::Failed { session, .. } if session.session == "gamma"),
        "{results:?}"
    );
    assert!(matches!(
        &results[2],
        SessionLogout::LoggedOut {
            had_tokens: true,
            ..
        }
    ));
    assert!(stored_tokens(home.path(), "zeta").is_none());
}

#[tokio::test]
async fn has_nothing_to_do_without_pkce_profiles() {
    use fico_sdk::auth::{self, LoginOptions};
    let parent = TempDir::new("profiles");
    let home = parent.path().join("none");
    assert!(auth::logout_profiles(&home, None).await.unwrap().is_empty());
    let results = auth::login_profiles(&home, None, LoginOptions::new(), |_| {})
        .await
        .unwrap();
    assert!(results.is_empty());
}
