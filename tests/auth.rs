//! Tests for `fico_sdk::auth`: the PKCE login, the token store, refresh,
//! sessions, teams, the SSO lookup, the client's token source and the MCP
//! provider. `tests/mock_server` plays the identity provider and the
//! platform, and reqwest plays the browser.

mod mock_server;
mod support;

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use fico_sdk::auth::{self, LoginOptions, PendingLogin, Session};
use fico_sdk::config::{ACCOUNT_HEADER, Config, EndpointOidc, Env, Profile};
use fico_sdk::mcp::RequestInfo;
use fico_sdk::{Client, Error, ProfileProblem};
use serde_json::{Value, json};

use crate::mock_server::{MockResponse, MockServer, RecordedRequest};
use crate::support::{TempDir, jwt, rfc3339, unix_now};

const CLIENT_ID: &str = "client-1";

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

/// A token response with `id_token`, expiring in an hour.
fn token_response(id_token: &str) -> MockResponse {
    MockResponse::json(
        200,
        json!({
            "access_token": "access-1",
            "id_token": id_token,
            "refresh_token": "refresh-1",
            "token_type": "Bearer",
            "expires_in": 3600,
        }),
    )
}

/// The OIDC settings of an identity provider at `url`, with a free
/// callback port.
fn oidc(url: &str) -> EndpointOidc {
    EndpointOidc {
        oidc_discovery_url: format!("{url}/.well-known/openid-configuration"),
        client_id: CLIENT_ID.to_owned(),
        callback_port: Some(0),
    }
}

fn api_key(endpoint: &str, api_key: &str) -> Profile {
    Profile::ApiKey {
        endpoint: Some(endpoint.to_owned()),
        api_key: api_key.to_owned(),
    }
}

fn pkce(endpoint: &str, session: &str, team_id: Option<&str>) -> Profile {
    Profile::Pkce {
        endpoint: Some(endpoint.to_owned()),
        auth_session: Some(session.to_owned()),
        team_id: team_id.map(str::to_owned),
        sso_domain: None,
    }
}

/// A configuration whose profile `work` logs in to session `work` on
/// `server`, which is also its identity provider.
fn config(server: &MockServer, team_id: Option<&str>) -> Config {
    let mut config = Config::default();
    config
        .set_endpoint_oidc(server.url(), oidc(server.url()))
        .unwrap();
    config
        .set_profile("work", pkce(server.url(), "work", team_id), false)
        .unwrap();
    config
}

/// The only session of `config`.
fn session(config: &Config) -> Session {
    let mut sessions = auth::sessions(config, None).unwrap();
    assert_eq!(sessions.len(), 1);
    sessions.remove(0)
}

/// The query parameters of `url`.
fn query(url: &str) -> HashMap<String, String> {
    reqwest::Url::parse(url)
        .unwrap()
        .query_pairs()
        .into_owned()
        .collect()
}

/// The form fields of `request`'s body.
fn form(request: &RecordedRequest) -> HashMap<String, String> {
    let body = String::from_utf8(request.body.clone()).unwrap();
    query(&format!("http://x/?{body}"))
}

/// What the browser does after the login: the identity provider sends it
/// to the callback with `params`.
async fn callback(login: &PendingLogin, params: &[(&str, &str)]) -> reqwest::Response {
    let url = reqwest::Url::parse_with_params(login.redirect_uri(), params).unwrap();
    reqwest::get(url).await.unwrap()
}

/// Stores tokens for session `work` of `config` as a login would, with an
/// access token that expires in `expires_in` seconds (in the past when
/// negative). The ID token stays valid for an hour longer.
fn logged_in(home: &Path, config: &Config, expires_in: i64) {
    let token_dir = home.join("auth").join("work");
    std::fs::create_dir_all(&token_dir).unwrap();
    let tokens = json!({
        "endpoint": session(config).endpoint,
        "token_type": "Bearer",
        "access_token": format!("access-{}", unix_now() + expires_in),
        "id_token": id_token(expires_in.max(1) + 3600),
        "refresh_token": "refresh-0",
        "expires_at": rfc3339(unix_now() + expires_in),
    });
    std::fs::write(token_dir.join("tokens.json"), tokens.to_string()).unwrap();
}

fn stored(home: &Path) -> Option<Value> {
    let path = home.join("auth").join("work").join("tokens.json");
    let text = std::fs::read_to_string(path).ok()?;
    Some(serde_json::from_str(&text).unwrap())
}

fn assert_not_logged_in(err: &Error) {
    assert!(
        matches!(err, Error::NotLoggedIn { session, .. } if session == "work"),
        "{err}"
    );
}

// The login.

#[tokio::test]
async fn logs_in_through_the_browser() {
    let token = id_token(3600);
    let server = MockServer::start_with(|url| vec![discovery(url), token_response(&token)]).await;
    let home = TempDir::new("auth");
    let config = config(&server, None);
    let session = session(&config);

    let login = auth::login(home.path(), &session, LoginOptions::new())
        .await
        .unwrap();
    let url = login.authorization_url().to_owned();
    assert!(
        url.starts_with(&format!("{}/oauth2/authorize?", server.url())),
        "{url}"
    );
    let params = query(&url);
    assert_eq!(params["response_type"], "code");
    assert_eq!(params["client_id"], CLIENT_ID);
    assert_eq!(params["scope"], "email openid profile");
    assert_eq!(params["code_challenge_method"], "S256");
    assert_eq!(params["code_challenge"].len(), 43);
    assert!(params["state"].len() >= 22);
    assert_eq!(params["redirect_uri"], login.redirect_uri());
    assert!(login.redirect_uri().starts_with("http://127.0.0.1:"));
    assert!(!params.contains_key("identity_provider"));
    assert!(!login.sso_lookup_failed());

    // A path other than `/` is not the callback.
    let other = format!("{}/favicon.ico", login.redirect_uri());
    assert_eq!(reqwest::get(other).await.unwrap().status(), 404);
    let state = params["state"].clone();
    let page = callback(&login, &[("code", "code-1"), ("state", &state)]).await;
    assert_eq!(page.status(), 200);
    let logged_in = login.complete().await.unwrap();
    assert_eq!(logged_in.session, "work");
    assert_eq!(logged_in.endpoint, server.url());
    assert_eq!(logged_in.profiles, ["work"]);

    // The exchange sends the verifier of the challenge.
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].target, "/.well-known/openid-configuration");
    assert_eq!(requests[1].target, "/oauth2/token");
    assert_eq!(
        requests[1].header("content-type"),
        Some("application/x-www-form-urlencoded")
    );
    let exchange = form(&requests[1]);
    assert_eq!(exchange["grant_type"], "authorization_code");
    assert_eq!(exchange["client_id"], CLIENT_ID);
    assert_eq!(exchange["code"], "code-1");
    assert_eq!(exchange["redirect_uri"], logged_in_redirect(&url));
    let verifier = &exchange["code_verifier"];
    assert_eq!(verifier.len(), 86);
    let digest = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, verifier.as_bytes());
    assert_eq!(
        support::base64url(digest.as_ref()),
        params["code_challenge"]
    );
    assert!(!url.contains(verifier.as_str()));

    let tokens = stored(home.path()).unwrap();
    assert_eq!(tokens["id_token"], token);
    assert_eq!(tokens["refresh_token"], "refresh-1");
    assert_eq!(tokens["endpoint"], server.url());
    assert_eq!(tokens["expires_at"], logged_in.expires_at);
}

fn logged_in_redirect(url: &str) -> String {
    query(url)["redirect_uri"].clone()
}

#[tokio::test]
async fn rejects_bad_callbacks() {
    for (params, why) in [
        (
            vec![("error", "access_denied"), ("error_description", "no")],
            "access_denied",
        ),
        (vec![("code", "code-1"), ("state", "forged")], "state"),
        (vec![("state", "<state>")], "code"),
        (vec![("code", ""), ("state", "<state>")], "code"),
    ] {
        let server = MockServer::start_with(|url| vec![discovery(url)]).await;
        let home = TempDir::new("auth");
        let session = session(&config(&server, None));
        let login = auth::login(home.path(), &session, LoginOptions::new())
            .await
            .unwrap();
        let state = query(login.authorization_url())["state"].clone();
        let params: Vec<(&str, &str)> = params
            .iter()
            .map(|&(k, v)| (k, if v == "<state>" { state.as_str() } else { v }))
            .collect();
        callback(&login, &params).await;
        let err = login.complete().await.unwrap_err();
        assert!(
            matches!(&err, Error::Login { reason, .. } if reason.contains(why)),
            "{why}: {err}"
        );
        // Nothing was exchanged or stored.
        assert_eq!(server.requests().len(), 1);
        assert_eq!(stored(home.path()), None);
    }
}

#[tokio::test]
async fn rejects_tokens_that_fail_the_checks() {
    let tokens = |token_type: &str, id_token: String| {
        MockResponse::json(
            200,
            json!({"access_token": "a", "id_token": id_token, "token_type": token_type}),
        )
    };
    for (response, why) in [
        (tokens("mac", id_token(600)), "token_type"),
        (
            tokens(
                "Bearer",
                jwt(json!({"aud": "other", "exp": unix_now() + 600})),
            ),
            "aud",
        ),
        (tokens("Bearer", id_token(-10)), "exp"),
        (
            MockResponse::json(200, json!({"access_token": "a", "token_type": "Bearer"})),
            "ID token",
        ),
    ] {
        let server = MockServer::start_with(|url| vec![discovery(url), response]).await;
        let home = TempDir::new("auth");
        let session = session(&config(&server, None));
        let login = auth::login(home.path(), &session, LoginOptions::new())
            .await
            .unwrap();
        let state = query(login.authorization_url())["state"].clone();
        callback(&login, &[("code", "c"), ("state", &state)]).await;
        let err = login.complete().await.unwrap_err();
        assert!(
            matches!(&err, Error::Login { reason, .. } if reason.contains(why)),
            "{why}: {err}"
        );
        assert_eq!(stored(home.path()), None);
    }
}

#[tokio::test]
async fn reports_a_refused_exchange_without_echoing_it() {
    let refusal = MockResponse::json(
        400,
        json!({"error": "invalid_grant", "error_description": "bad code", "echo": "code-1"}),
    );
    let server = MockServer::start_with(|url| vec![discovery(url), refusal]).await;
    let home = TempDir::new("auth");
    let session = session(&config(&server, None));
    let login = auth::login(home.path(), &session, LoginOptions::new())
        .await
        .unwrap();
    let state = query(login.authorization_url())["state"].clone();
    callback(&login, &[("code", "code-1"), ("state", &state)]).await;
    let err = login.complete().await.unwrap_err();
    let message = err.to_string();
    assert!(
        matches!(err, Error::Status { status: 400, .. }),
        "{message}"
    );
    assert!(message.contains("bad code"), "{message}");
    assert!(!message.contains("code-1"), "{message}");
}

#[tokio::test]
async fn gives_up_without_a_callback() {
    let server = MockServer::start_with(|url| vec![discovery(url)]).await;
    let home = TempDir::new("auth");
    let session = session(&config(&server, None));
    let login = auth::login(home.path(), &session, LoginOptions::new())
        .await
        .unwrap()
        .with_timeout(Duration::from_millis(50));
    let err = login.complete().await.unwrap_err();
    assert!(matches!(err, Error::Wait { .. }), "{err}");
}

#[tokio::test]
async fn forces_a_fresh_session_through_the_logout_endpoint() {
    let server = MockServer::start_with(|url| vec![discovery(url)]).await;
    let home = TempDir::new("auth");
    let session = session(&config(&server, None));
    let login = auth::login(home.path(), &session, LoginOptions::new().force(true))
        .await
        .unwrap();
    let url = login.authorization_url();
    assert!(
        url.starts_with(&format!("{}/logout?", server.url())),
        "{url}"
    );
    let params = query(url);
    assert_eq!(params["client_id"], CLIENT_ID);
    assert_eq!(params["response_type"], "code");
    assert!(params.contains_key("code_challenge"));
}

#[tokio::test]
async fn logs_in_through_the_sso_provider_of_the_domain() {
    let server = MockServer::start_with(|url| {
        vec![
            discovery(url),
            MockResponse::json(
                200,
                json!({"enabled": true, "domain_identifier": "AcmeOkta"}),
            ),
        ]
    })
    .await;
    let home = TempDir::new("auth");
    let mut config = config(&server, None);
    let Some(Profile::Pkce { sso_domain, .. }) = config.profiles.get_mut("work") else {
        panic!("not a PKCE profile");
    };
    *sso_domain = Some("acme.com".to_owned());
    let session = session(&config);
    assert_eq!(session.sso_domain.as_deref(), Some("acme.com"));
    let login = auth::login(home.path(), &session, LoginOptions::new())
        .await
        .unwrap();
    assert_eq!(
        query(login.authorization_url())["identity_provider"],
        "AcmeOkta"
    );
    assert!(!login.sso_lookup_failed());
    let lookup = &server.requests()[1];
    assert_eq!(lookup.target, "/v1/enterprise/sso/domain?domain=acme.com");
    assert_eq!(lookup.header("authorization"), None);
}

#[tokio::test]
async fn falls_back_to_the_standard_login_when_the_sso_lookup_fails() {
    let server = MockServer::start_with(|url| vec![discovery(url), MockResponse::new(404)]).await;
    let home = TempDir::new("auth");
    let mut config = config(&server, None);
    if let Some(Profile::Pkce { sso_domain, .. }) = config.profiles.get_mut("work") {
        *sso_domain = Some("acme.com".to_owned());
    }
    let login = auth::login(home.path(), &session(&config), LoginOptions::new())
        .await
        .unwrap();
    assert!(login.sso_lookup_failed());
    assert!(!query(login.authorization_url()).contains_key("identity_provider"));
}

#[tokio::test]
async fn reads_the_sso_provider_of_a_domain() {
    let server = MockServer::start(vec![
        MockResponse::json(
            200,
            json!({"enabled": true, "domain_identifier": "AcmeOkta"}),
        ),
        MockResponse::json(
            200,
            json!({"enabled": false, "domain_identifier": "AcmeOkta"}),
        ),
        MockResponse::json(200, json!({})),
    ])
    .await;
    assert_eq!(
        auth::sso_provider(server.url(), " acme.com ")
            .await
            .unwrap()
            .as_deref(),
        Some("AcmeOkta")
    );
    assert_eq!(
        auth::sso_provider(server.url(), "acme.com").await.unwrap(),
        None
    );
    assert_eq!(
        auth::sso_provider(server.url(), "acme.com").await.unwrap(),
        None
    );
    let err = auth::sso_provider("http://api.example.com", "acme.com")
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Invalid { .. }), "{err}");
}

#[tokio::test]
async fn reports_a_busy_callback_port() {
    let server = MockServer::start_with(|url| vec![discovery(url)]).await;
    let busy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = busy.local_addr().unwrap().port();
    let mut config = config(&server, None);
    let mut settings = oidc(server.url());
    settings.callback_port = Some(port);
    config.set_endpoint_oidc(server.url(), settings).unwrap();
    let home = TempDir::new("auth");
    let err = auth::login(home.path(), &session(&config), LoginOptions::new())
        .await
        .unwrap_err();
    let message = err.to_string();
    assert!(matches!(err, Error::Login { .. }), "{message}");
    assert!(message.contains(&format!("127.0.0.1:{port}")), "{message}");
}

#[tokio::test]
async fn needs_the_oidc_settings_of_the_endpoint() {
    let mut config = Config::default();
    config
        .set_profile("work", pkce("https://api.example.com", "work", None), false)
        .unwrap();
    let session = session(&config);
    assert_eq!(session.oidc, None);
    let home = TempDir::new("auth");
    let err = auth::login(home.path(), &session, LoginOptions::new())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Invalid { .. }), "{err}");
}

// Sessions.

#[test]
fn groups_profiles_by_session_with_the_default_first() {
    let mut config = Config::default();
    let a = "https://a.example.com";
    let b = "https://b.example.com";
    config.set_endpoint_oidc(a, oidc("https://idp")).unwrap();
    for (name, endpoint, session) in [
        ("zeta", a, "shared"),
        ("alpha", b, "other"),
        ("main", a, "shared"),
        ("beta", a, "shared"),
    ] {
        config
            .set_profile(name, pkce(endpoint, session, None), false)
            .unwrap();
    }
    config
        .set_profile("ci", api_key(a, "ci-key"), false)
        .unwrap();
    config.default_profile = Some("main".to_owned());

    let sessions = auth::sessions(&config, None).unwrap();
    let names: Vec<_> = sessions
        .iter()
        .map(|s| (s.session.as_str(), s.endpoint.as_str(), s.profiles.clone()))
        .collect();
    assert_eq!(
        names,
        [
            (
                "shared",
                a,
                vec!["main".to_owned(), "beta".to_owned(), "zeta".to_owned()]
            ),
            ("other", b, vec!["alpha".to_owned()]),
        ]
    );
    // An endpoint without OIDC settings is reported as data.
    assert!(sessions[0].oidc.is_some());
    assert_eq!(sessions[1].oidc, None);
    let json = serde_json::to_value(&sessions[1]).unwrap();
    assert_eq!(
        json,
        json!({"session": "other", "endpoint": b, "profiles": ["alpha"]})
    );

    let one = auth::sessions(&config, Some("beta")).unwrap();
    assert_eq!(one.len(), 1);
    assert_eq!(one[0].profiles, ["beta"]);
}

#[test]
fn checks_the_profile_to_log_in() {
    let mut config = Config::default();
    config
        .set_profile("ci", api_key("https://api.example.com", "ci-key"), false)
        .unwrap();
    let err = auth::sessions(&config, Some("missing")).unwrap_err();
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
    let err = auth::sessions(&config, Some("ci")).unwrap_err();
    assert!(
        matches!(
            &err,
            Error::Profile { profile, problem: ProfileProblem::NotPkce { auth_type: "api_key", .. }, .. }
                if profile == "ci"
        ),
        "{err}"
    );
    assert_eq!(auth::sessions(&config, None).unwrap(), []);
}

#[test]
fn rejects_profile_names_that_resolve_another_profile() {
    let a = "https://a.example.com";
    // Only a hand-edited `config.yaml` has these names: `set_profile` trims
    // names and rejects blank ones, and `resolve` reads them the same way,
    // so they would resolve the default profile, or none.
    for (name, default) in [("", None), ("", Some("main")), (" main ", Some("main"))] {
        let mut config = Config::default();
        config
            .set_profile("main", pkce(a, "main", None), false)
            .unwrap();
        config.default_profile = default.map(str::to_owned);
        config
            .profiles
            .insert(name.to_owned(), pkce(a, "other", None));

        let err = auth::sessions(&config, None).unwrap_err();
        assert!(
            matches!(&err, Error::Invalid { reason, .. } if reason.contains("blank or untrimmed")),
            "{name:?}, {default:?}: {err}"
        );
    }
    let mut config = Config::default();
    config.profiles.insert(String::new(), pkce(a, "main", None));
    let err = auth::sessions(&config, Some("")).unwrap_err();
    assert!(
        matches!(&err, Error::Invalid { reason, .. } if reason.contains("blank or untrimmed")),
        "{err}"
    );
}

// The store.

#[tokio::test]
async fn stores_tokens_privately_and_logs_out() {
    let token = id_token(3600);
    let server = MockServer::start_with(|url| vec![discovery(url), token_response(&token)]).await;
    let home = TempDir::new("auth");
    let session = session(&config(&server, None));
    let login = auth::login(home.path(), &session, LoginOptions::new())
        .await
        .unwrap();
    let state = query(login.authorization_url())["state"].clone();
    callback(&login, &[("code", "c"), ("state", &state)]).await;
    login.complete().await.unwrap();

    let dir = home.path().join("auth").join("work");
    let file = dir.join("tokens.json");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&file), 0o600);
    }
    // Only the token file is left: the temporary file was renamed.
    let names: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(names, ["tokens.json"]);
    assert_eq!(
        auth::ensure_token(home.path(), &session).await.unwrap(),
        token
    );

    assert!(auth::logout(home.path(), "work").await.unwrap());
    assert!(!file.exists());
    assert!(!auth::logout(home.path(), "WORK").await.unwrap());
    let err = auth::ensure_token(home.path(), &session).await.unwrap_err();
    assert_not_logged_in(&err);
}

#[tokio::test]
async fn keeps_session_names_inside_the_auth_directory() {
    let home = TempDir::new("auth");
    for session in ["..", "../config", "a/b", ".hidden", "", "con"] {
        let err = auth::logout(home.path(), session).await.unwrap_err();
        assert!(matches!(err, Error::Invalid { .. }), "{session:?}: {err}");
    }
}

#[tokio::test]
async fn never_sends_tokens_to_another_endpoint() {
    let server = MockServer::start(vec![]).await;
    let other = MockServer::start(vec![]).await;
    let home = TempDir::new("auth");
    let config = config(&server, None);
    logged_in(home.path(), &config, 3600);
    // The same session on another endpoint, e.g. through `FICO_ENDPOINT`.
    let env = Env {
        endpoint: Some(other.url().to_owned()),
        ..Env::default()
    };
    let resolved = config.resolve(None, &env).unwrap().unwrap();
    let client = resolved.client_builder(home.path()).build().unwrap();
    let err = client.get("/v1/applications").send().await.unwrap_err();
    assert_not_logged_in(&err);
    assert!(other.requests().is_empty());
}

// Requests and refresh.

#[tokio::test]
async fn sends_the_bearer_and_the_team_of_a_pkce_profile() {
    let server = MockServer::start(vec![
        MockResponse::json(200, json!({})),
        MockResponse::json(200, json!({})),
    ])
    .await;
    let home = TempDir::new("auth");
    let config = config(&server, Some("team-1"));
    logged_in(home.path(), &config, 3600);
    let token = stored(home.path()).unwrap()["id_token"]
        .as_str()
        .unwrap()
        .to_owned();
    let resolved = config.resolve(None, &Env::default()).unwrap().unwrap();
    let client = resolved.client_builder(home.path()).build().unwrap();
    client.get("/v1/applications").send().await.unwrap();
    client.get("/v1/applications").send().await.unwrap();

    let requests = server.requests();
    assert_eq!(requests.len(), 2, "no refresh while the token is fresh");
    for request in &requests {
        let bearer = format!("Bearer {token}");
        assert_eq!(request.header("authorization"), Some(bearer.as_str()));
        assert_eq!(request.header(ACCOUNT_HEADER), Some("team-1"));
    }
}

#[tokio::test]
async fn refreshes_tokens_that_expire_soon() {
    // Cognito's refresh returns no refresh token, and here no ID token.
    let refreshed = MockResponse::json(
        200,
        json!({"access_token": "access-2", "token_type": "Bearer", "expires_in": 3600}),
    );
    let server = MockServer::start_with(|url| {
        vec![
            discovery(url),
            refreshed,
            MockResponse::json(200, json!({})),
        ]
    })
    .await;
    let home = TempDir::new("auth");
    let config = config(&server, None);
    // Expires within the 30 seconds of margin.
    logged_in(home.path(), &config, 20);
    let old = stored(home.path()).unwrap();
    let resolved = config.resolve(None, &Env::default()).unwrap().unwrap();
    let client = resolved.client_builder(home.path()).build().unwrap();
    client.get("/v1/applications").send().await.unwrap();

    let requests = server.requests();
    assert_eq!(requests[0].target, "/.well-known/openid-configuration");
    // The token endpoint, not the logout endpoint.
    assert_eq!(requests[1].target, "/oauth2/token");
    let refresh = form(&requests[1]);
    assert_eq!(refresh["grant_type"], "refresh_token");
    assert_eq!(refresh["refresh_token"], "refresh-0");
    assert_eq!(refresh["client_id"], CLIENT_ID);
    assert_eq!(requests[2].target, "/v1/applications");
    let bearer = format!("Bearer {}", old["id_token"].as_str().unwrap());
    assert_eq!(requests[2].header("authorization"), Some(bearer.as_str()));

    // The refresh and ID tokens the response left out are kept.
    let new = stored(home.path()).unwrap();
    assert_eq!(new["access_token"], "access-2");
    assert_eq!(new["refresh_token"], "refresh-0");
    assert_eq!(new["id_token"], old["id_token"]);
    assert_ne!(new["expires_at"], old["expires_at"]);
}

#[tokio::test]
async fn deletes_the_tokens_on_invalid_grant() {
    let server = MockServer::start_with(|url| {
        vec![
            discovery(url),
            MockResponse::json(400, json!({"error": "invalid_grant"})),
        ]
    })
    .await;
    let home = TempDir::new("auth");
    let config = config(&server, None);
    logged_in(home.path(), &config, -60);
    let resolved = config.resolve(None, &Env::default()).unwrap().unwrap();
    let client = resolved.client_builder(home.path()).build().unwrap();
    let err = client.get("/v1/applications").send().await.unwrap_err();
    assert_not_logged_in(&err);
    assert_eq!(stored(home.path()), None);
    assert_eq!(server.requests().len(), 2, "the platform was never called");
}

#[tokio::test]
async fn needs_a_login_without_tokens_or_a_refresh_token() {
    let server = MockServer::start(vec![]).await;
    let home = TempDir::new("auth");
    let config = config(&server, None);
    let resolved = config.resolve(None, &Env::default()).unwrap().unwrap();
    let client = resolved.client_builder(home.path()).build().unwrap();
    assert_not_logged_in(&client.get("/x").send().await.unwrap_err());

    logged_in(home.path(), &config, -60);
    let mut tokens = stored(home.path()).unwrap();
    tokens.as_object_mut().unwrap().remove("refresh_token");
    let file = home.path().join("auth").join("work").join("tokens.json");
    std::fs::write(file, tokens.to_string()).unwrap();
    assert_not_logged_in(&client.get("/x").send().await.unwrap_err());
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn never_shows_a_token_in_debug_output() {
    let server = MockServer::start(vec![]).await;
    let home = TempDir::new("auth");
    let config = config(&server, None);
    logged_in(home.path(), &config, 3600);
    let tokens = stored(home.path()).unwrap();
    let resolved = config.resolve(None, &Env::default()).unwrap().unwrap();
    let builder = resolved.client_builder(home.path());
    let provider = auth::ProfileProvider::new(home.path(), &resolved, |b| b).unwrap();
    for debug in [
        format!("{builder:?}"),
        format!("{:?}", builder.build().unwrap()),
        format!("{provider:?}"),
        format!("{:?}", session(&config)),
    ] {
        for secret in ["id_token", "access_token", "refresh_token"] {
            let value = tokens[secret].as_str().unwrap();
            assert!(!debug.contains(value), "{secret}: {debug}");
        }
    }
}

// Teams.

#[tokio::test]
async fn lists_and_resolves_teams() {
    let server = MockServer::start(vec![MockResponse::json(
        200,
        json!([
            {"id": "t-1", "name": "Routing", "role": "admin", "pending_invite": false},
            {"id": "t-2", "name": "Scheduling", "role": "member", "pending_invite": true},
        ]),
    )])
    .await;
    let client = Client::builder(server.url())
        .bearer_token("token-1")
        .build()
        .unwrap();
    let teams = auth::list_teams(&client).await.unwrap();
    let request = &server.requests()[0];
    assert_eq!(request.target, "/v1/internal/me/organization");
    assert_eq!(request.header("authorization"), Some("Bearer token-1"));
    assert_eq!(teams.len(), 2);
    assert!(teams[1].pending_invite);

    assert_eq!(
        auth::resolve_team(&teams, " routing ").unwrap().team_id,
        "t-1"
    );
    assert_eq!(
        auth::resolve_team(&teams, "SCHEDULING").unwrap().team_id,
        "t-2"
    );
    let err = auth::resolve_team(&teams, "billing").unwrap_err();
    let message = err.to_string();
    assert!(matches!(err, Error::Invalid { .. }), "{message}");
    assert!(message.contains("`Routing`, `Scheduling`"), "{message}");
}

// The MCP provider.

#[tokio::test]
async fn gives_each_mcp_call_a_fresh_token() {
    let refreshed = id_token(3600);
    let server = MockServer::start_with(|url| {
        vec![
            MockResponse::json(200, json!({"items": []})),
            discovery(url),
            token_response(&refreshed),
            MockResponse::json(200, json!({"items": []})),
        ]
    })
    .await;
    let home = TempDir::new("auth");
    let config = config(&server, Some("team-1"));
    logged_in(home.path(), &config, 3600);
    let first = stored(home.path()).unwrap()["id_token"]
        .as_str()
        .unwrap()
        .to_owned();
    let resolved = config.resolve(None, &Env::default()).unwrap().unwrap();
    let provider = auth::ProfileProvider::new(home.path(), &resolved, |b| b).unwrap();

    // The request's own token is never sent on.
    let info = RequestInfo::with_authorization("Bearer client-token");
    let call = |info: RequestInfo| async {
        let cx = provider.context(info).await.unwrap();
        cx.platform().unwrap().list_workspaces().await.unwrap();
    };
    call(info.clone()).await;

    // The session's tokens are about to expire: the next call refreshes.
    logged_in(home.path(), &config, 10);
    let expiring = stored(home.path()).unwrap();
    let provider = auth::ProfileProvider::new(home.path(), &resolved, |b| b).unwrap();
    let cx = provider.context(info).await.unwrap();
    cx.platform().unwrap().list_workspaces().await.unwrap();

    let requests = server.requests();
    assert_eq!(requests.len(), 4);
    let bearer = |i: usize| requests[i].header("authorization").map(str::to_owned);
    assert_eq!(bearer(0), Some(format!("Bearer {first}")));
    assert_eq!(requests[2].target, "/oauth2/token");
    assert_eq!(bearer(3), Some(format!("Bearer {refreshed}")));
    for i in [0, 3] {
        assert_ne!(bearer(i).as_deref(), Some("Bearer client-token"));
        assert_eq!(requests[i].header(ACCOUNT_HEADER), Some("team-1"));
    }
    assert_ne!(
        stored(home.path()).unwrap()["access_token"],
        expiring["access_token"]
    );
}

#[tokio::test]
async fn fails_mcp_calls_of_a_session_that_is_not_logged_in() {
    let server = MockServer::start(vec![]).await;
    let home = TempDir::new("auth");
    let config = config(&server, None);
    config.save(home.path()).await.unwrap();
    let provider = auth::ProfileProvider::from_profile(home.path(), Some("work"), &Env::default())
        .await
        .unwrap();
    let err = provider.context(RequestInfo::default()).await.unwrap_err();
    assert_not_logged_in(&err);

    let empty = TempDir::new("auth");
    let err = auth::ProfileProvider::from_profile(empty.path(), None, &Env::default())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Invalid { .. }), "{err}");
}

#[tokio::test]
async fn serves_an_api_key_profile_as_a_fixed_bearer() {
    let server = MockServer::start(vec![MockResponse::json(200, json!({"items": []}))]).await;
    let home = TempDir::new("auth");
    let env = Env {
        endpoint: Some(server.url().to_owned()),
        api_key: Some("key-1".to_owned()),
        ..Env::default()
    };
    let provider = auth::ProfileProvider::from_profile(home.path(), None, &env)
        .await
        .unwrap();
    let cx = provider.context(RequestInfo::default()).await.unwrap();
    cx.platform().unwrap().list_workspaces().await.unwrap();
    assert_eq!(
        server.requests()[0].header("authorization"),
        Some("Bearer key-1")
    );
}

#[tokio::test]
async fn refreshes_tokens_whose_expiry_is_unreadable() {
    let refreshed = id_token(3600);
    let server = MockServer::start_with(|url| {
        vec![
            discovery(url),
            token_response(&refreshed),
            MockResponse::json(200, json!({})),
        ]
    })
    .await;
    let home = TempDir::new("auth");
    let config = config(&server, None);
    logged_in(home.path(), &config, 3600);
    let mut tokens = stored(home.path()).unwrap();
    tokens["expires_at"] = json!("2026-10-20 12:00:00");
    let file = home.path().join("auth").join("work").join("tokens.json");
    std::fs::write(file, tokens.to_string()).unwrap();

    let resolved = config.resolve(None, &Env::default()).unwrap().unwrap();
    let client = resolved.client_builder(home.path()).build().unwrap();
    client.get("/v1/applications").send().await.unwrap();
    let requests = server.requests();
    assert_eq!(requests[1].target, "/oauth2/token");
    let bearer = format!("Bearer {refreshed}");
    assert_eq!(requests[2].header("authorization"), Some(bearer.as_str()));
}

#[tokio::test]
async fn needs_a_login_when_the_kept_id_token_has_expired() {
    // Cognito's refresh returns no ID token, so the stored one is kept.
    let refreshed = MockResponse::json(
        200,
        json!({"access_token": "access-2", "token_type": "Bearer", "expires_in": 3600}),
    );
    let server = MockServer::start_with(|url| vec![discovery(url), refreshed]).await;
    let home = TempDir::new("auth");
    let config = config(&server, None);
    logged_in(home.path(), &config, -60);
    let mut tokens = stored(home.path()).unwrap();
    tokens["id_token"] = json!(id_token(-60));
    let file = home.path().join("auth").join("work").join("tokens.json");
    std::fs::write(&file, tokens.to_string()).unwrap();

    let resolved = config.resolve(None, &Env::default()).unwrap().unwrap();
    let client = resolved.client_builder(home.path()).build().unwrap();
    for _ in 0..3 {
        assert_not_logged_in(&client.get("/x").send().await.unwrap_err());
    }
    // The refreshed tokens can't be used, so they weren't stored, and the
    // identity provider was asked once: later requests fail without it.
    assert_eq!(stored(home.path()).unwrap(), tokens);
    let targets: Vec<_> = server.requests().iter().map(|r| r.target.clone()).collect();
    assert_eq!(
        targets,
        ["/.well-known/openid-configuration", "/oauth2/token"]
    );

    // A new login stores another bearer, which the client then sends.
    logged_in(home.path(), &config, 3600);
    let fresh = stored(home.path()).unwrap();
    let server_requests = server.requests().len();
    let err = client.get("/x").send().await.unwrap_err();
    // The mock has no scripted response left, so the request reached it.
    assert!(matches!(err, Error::Status { status: 500, .. }), "{err}");
    let requests = server.requests();
    assert_eq!(requests.len(), server_requests + 1);
    let bearer = format!("Bearer {}", fresh["id_token"].as_str().unwrap());
    assert_eq!(
        requests.last().unwrap().header("authorization"),
        Some(bearer.as_str())
    );
}
