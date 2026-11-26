//! Tests for the profiles of `config.yaml` (`fico_sdk::config`): the file,
//! setting and deleting profiles, the OIDC settings, masking, resolution
//! and the client it builds.

mod mock_server;
mod support;

use std::collections::BTreeMap;
use std::path::Path;

use fico_sdk::config::{
    self, ACCOUNT_HEADER, Config, Credential, EndpointOidc, Env, EnvOverride, Profile,
    ProfileParams,
};
use fico_sdk::{Error, ProfileProblem};
use serde_json::json;

use crate::mock_server::{MockResponse, MockServer};
use crate::support::TempDir;

/// An API key long enough to show up in any output that leaks it.
const KEY: &str = "fico_key_8c1f2e9d7b3a";

/// An example `config.yaml`: a default PKCE profile, an API-key profile and
/// the OIDC settings of an endpoint.
const EXAMPLE: &str = r#"default_profile: default
profiles:
  default:
    endpoint: https://api.example.com
    auth_type: pkce
    auth_session: work
    team_id: 9f3c1d2e-team
    sso_domain: example.com
  ci:
    endpoint: https://api.example.com
    auth_type: api_key
    api_key: fico_key_8c1f2e9d7b3a
endpoints:
  api.staging.example.com:
    oidc_discovery_url: https://idp.example.com/.well-known/openid-configuration
    client_id: "client-1"
"#;

fn api_key(endpoint: &str, api_key: &str) -> Profile {
    Profile::ApiKey {
        endpoint: Some(endpoint.to_owned()),
        api_key: api_key.to_owned(),
    }
}

fn pkce(endpoint: &str, session: Option<&str>) -> Profile {
    Profile::Pkce {
        endpoint: Some(endpoint.to_owned()),
        auth_session: session.map(str::to_owned),
        team_id: None,
        sso_domain: None,
    }
}

fn oidc(url: &str, client_id: &str) -> EndpointOidc {
    EndpointOidc {
        oidc_discovery_url: url.to_owned(),
        client_id: client_id.to_owned(),
        callback_port: None,
    }
}

/// A configuration with the API-key profiles `names`, the first one the
/// default.
fn config_with(names: &[&str]) -> Config {
    let mut config = Config::default();
    for name in names {
        config
            .set_profile(name, api_key("api.example.com", KEY), false)
            .unwrap();
    }
    config
}

fn write_config(home: &Path, yaml: &str) {
    std::fs::write(home.join("config.yaml"), yaml).unwrap();
}

/// Asserts that `err` is `Error::Invalid` and quotes no API key.
fn assert_invalid(err: &Error) {
    assert!(matches!(err, Error::Invalid { .. }), "{err}");
    assert!(!err.to_string().contains(KEY), "{err}");
}

fn assert_not_found(err: &Error, name: &str) {
    assert!(
        matches!(err, Error::NotFound { kind: "profile", id, .. } if id == name),
        "{err}"
    );
}

// The file.

#[tokio::test]
async fn round_trips_the_example_file() {
    let home = TempDir::new("config");
    write_config(home.path(), EXAMPLE);
    let config = Config::load(home.path()).await.unwrap();

    assert_eq!(config.default_profile.as_deref(), Some("default"));
    assert_eq!(
        config.profiles["default"],
        Profile::Pkce {
            endpoint: Some("https://api.example.com".to_owned()),
            auth_session: Some("work".to_owned()),
            team_id: Some("9f3c1d2e-team".to_owned()),
            sso_domain: Some("example.com".to_owned()),
        }
    );
    assert_eq!(
        config.profiles["ci"],
        api_key("https://api.example.com", KEY)
    );
    assert_eq!(
        config.endpoints["api.staging.example.com"],
        oidc(
            "https://idp.example.com/.well-known/openid-configuration",
            "client-1"
        )
    );

    config.save(home.path()).await.unwrap();
    assert_eq!(Config::load(home.path()).await.unwrap(), config);
}

#[tokio::test]
async fn writes_keys_sorted_and_only_the_known_ones() {
    let home = TempDir::new("config");
    write_config(
        home.path(),
        "system_certs: true\napikey: old\nprofiles:\n  b:\n    auth_type: api_key\n    \
         endpoint: https://b.example.com\n    api_key: kb\n    system_certs: true\n  a:\n    \
         auth_type: api_key\n    endpoint: https://a.example.com\n    api_key: ka\n",
    );
    let config = Config::load(home.path()).await.unwrap();
    config.save(home.path()).await.unwrap();

    let text = std::fs::read_to_string(home.path().join("config.yaml")).unwrap();
    assert!(
        !text.contains("system_certs") && !text.contains("apikey"),
        "{text}"
    );
    assert!(
        text.find("  a:").unwrap() < text.find("  b:").unwrap(),
        "{text}"
    );
}

#[tokio::test]
async fn keeps_api_keys_that_look_like_other_yaml_values() {
    let home = TempDir::new("config");
    let mut config = Config::default();
    for (i, key) in ["12345", "true", "null", "~", "0x1A", "#key", "a: b", " x"]
        .iter()
        .enumerate()
    {
        let mut profile = api_key("api.example.com", key);
        if let Profile::ApiKey { api_key, .. } = &mut profile {
            // `set_profile` trims keys, so put the raw key in place.
            *api_key = (*key).to_owned();
        }
        config.profiles.insert(format!("p{i}"), profile);
    }
    config.save(home.path()).await.unwrap();
    assert_eq!(Config::load(home.path()).await.unwrap(), config);
}

#[tokio::test]
async fn loads_a_missing_or_empty_file_as_empty() {
    let home = TempDir::new("config");
    assert_eq!(Config::load(home.path()).await.unwrap(), Config::default());
    let missing = home.path().join("missing");
    assert_eq!(Config::load(&missing).await.unwrap(), Config::default());
    for empty in [
        "",
        "\n  \n",
        "# nothing yet\n",
        "~\n",
        "profiles:\nendpoints:\n",
    ] {
        write_config(home.path(), empty);
        assert_eq!(
            Config::load(home.path()).await.unwrap(),
            Config::default(),
            "{empty:?}"
        );
    }
}

#[tokio::test]
async fn skips_endpoints_without_both_values() {
    let home = TempDir::new("config");
    write_config(
        home.path(),
        "endpoints:\n  a.example.com:\n    oidc_discovery_url: https://idp\n  \
         b.example.com:\n    oidc_discovery_url: https://idp\n    client_id: \"\"\n  \
         c.example.com:\n    oidc_discovery_url: https://idp\n    client_id: c\n",
    );
    let config = Config::load(home.path()).await.unwrap();
    assert_eq!(
        config.endpoints.keys().collect::<Vec<_>>(),
        ["c.example.com"]
    );
}

#[tokio::test]
async fn reads_and_writes_the_callback_port() {
    let home = TempDir::new("config");
    write_config(
        home.path(),
        "endpoints:\n  a.example.com:\n    oidc_discovery_url: https://idp\n    \
         client_id: c\n    callback_port: 0\n  b.example.com:\n    \
         oidc_discovery_url: https://idp\n    client_id: c\n",
    );
    let config = Config::load(home.path()).await.unwrap();
    assert_eq!(config.endpoints["a.example.com"].callback_port, Some(0));
    assert_eq!(config.endpoints["b.example.com"].callback_port, None);
    config.save(home.path()).await.unwrap();
    let yaml = std::fs::read_to_string(Config::path(home.path())).unwrap();
    assert_eq!(yaml.matches("callback_port").count(), 1, "{yaml}");
    assert_eq!(Config::load(home.path()).await.unwrap(), config);

    for port in ["70000", "-1", "\"x\"", "1.5"] {
        write_config(
            home.path(),
            &format!(
                "endpoints:\n  a.example.com:\n    oidc_discovery_url: https://idp\n    \
                 client_id: c\n    callback_port: {port}\n"
            ),
        );
        let err = Config::load(home.path()).await.unwrap_err();
        assert_invalid(&err);
        assert!(err.to_string().contains("callback_port"), "{err}");
    }
}

#[tokio::test]
async fn reports_invalid_files_without_their_values() {
    let home = TempDir::new("config");
    for yaml in [
        format!("profiles:\n  ci:\n    auth_type: api_key\n    api_key: \"{KEY}\n"),
        format!("profiles: [{KEY}\n"),
        format!("- {KEY}\n"),
        format!("{KEY}\n"),
        format!("profiles:\n  ci: {KEY}\n"),
        format!("profiles:\n  ci:\n    auth_type: oauth\n    api_key: {KEY}\n"),
        format!("profiles:\n  ci:\n    api_key: {KEY}\n"),
        "profiles:\n  ci:\n    auth_type: api_key\n    endpoint: https://h\n".to_owned(),
        "profiles:\n  ci:\n    auth_type: api_key\n    api_key: 987654321\n".to_owned(),
        "profiles:\n  ci:\n    auth_type: api_key\n    api_key: [987654321]\n".to_owned(),
        "default_profile: [987654321]\n".to_owned(),
        "endpoints: 987654321\n".to_owned(),
        // Numbers too large for a float.
        "profiles:\n  ci:\n    auth_type: api_key\n    api_key: 987654321e999\n".to_owned(),
        "profiles:\n  ci:\n    <<: {auth_type: api_key, api_key: 987654321e999}\n".to_owned(),
        "profiles:\n  ci:\n    <<: [{api_key: -987654321e999}]\n".to_owned(),
        "profiles:\n  ci:\n    auth_type: 987654321e999\n".to_owned(),
        "profiles:\n  ci:\n    auth_type: KEY987654321\n    api_key: x\n".to_owned(),
        "profiles:\n  ci: !!map KEY987654321\n".to_owned(),
    ] {
        write_config(home.path(), &yaml);
        let err = Config::load(home.path()).await.unwrap_err();
        assert_invalid(&err);
        let message = err.to_string();
        assert!(message.contains("config.yaml"), "{message}");
        assert!(!message.contains("987654321"), "{message}");
    }

    write_config(
        home.path(),
        "profiles:\n  ci:\n    auth_type: api_key\n    api_key: 987654321e999\n",
    );
    let err = Config::load(home.path()).await.unwrap_err();
    assert!(
        err.to_string()
            .contains("the value at line 4, column 14 is not a finite number: quote it"),
        "{err}"
    );
    write_config(
        home.path(),
        "profiles:\n  ci:\n    <<: {auth_type: api_key, api_key: 987654321e999}\n",
    );
    let err = Config::load(home.path()).await.unwrap_err();
    assert!(
        err.to_string().contains("is not a finite number: quote it"),
        "{err}"
    );
    write_config(
        home.path(),
        "profiles:\n  ci:\n    auth_type: oauth\n    api_key: x\n",
    );
    let err = Config::load(home.path()).await.unwrap_err();
    assert!(
        err.to_string()
            .contains("`auth_type` of profile `ci` must be `api_key` or `pkce`"),
        "{err}"
    );

    write_config(
        home.path(),
        "profiles:\n  ci:\n    auth_type: api_key\n    api_key: 987\n",
    );
    let err = Config::load(home.path()).await.unwrap_err();
    assert!(
        err.to_string().contains("`api_key` of profile `ci`"),
        "{err}"
    );
}

#[tokio::test]
async fn reports_an_unreadable_file_as_io() {
    let home = TempDir::new("config");
    std::fs::create_dir(home.path().join("config.yaml")).unwrap();
    let err = Config::load(home.path()).await.unwrap_err();
    assert!(
        matches!(&err, Error::Io { path, .. } if path.ends_with("config.yaml")),
        "{err}"
    );
}

#[tokio::test]
async fn saves_atomically_into_a_new_directory() {
    let root = TempDir::new("config");
    let home = root.path().join("nested").join(".fico");
    config_with(&["a"]).save(&home).await.unwrap();
    config_with(&["b"]).save(&home).await.unwrap();

    let config = Config::load(&home).await.unwrap();
    assert_eq!(config.profiles.keys().collect::<Vec<_>>(), ["b"]);
    let files: Vec<_> = std::fs::read_dir(&home)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(files, ["config.yaml"]);
    assert_eq!(Config::path(&home), home.join("config.yaml"));
}

#[cfg(unix)]
#[tokio::test]
async fn saves_the_file_with_mode_0600() {
    use std::os::unix::fs::PermissionsExt;

    let home = TempDir::new("config");
    config_with(&["a"]).save(home.path()).await.unwrap();
    let mode = std::fs::metadata(home.path().join("config.yaml"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);

    // A file that was readable by others is replaced by a private one.
    let path = home.path().join("config.yaml");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    config_with(&["b"]).save(home.path()).await.unwrap();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
}

// Profiles.

#[test]
fn makes_the_first_profile_the_default() {
    let mut config = Config::default();
    let set = config
        .set_profile(" ci ", api_key("api.example.com", KEY), false)
        .unwrap();
    assert_eq!(set.name, "ci");
    assert_eq!(set.stale_session, None);
    assert_eq!(config.default_profile.as_deref(), Some("ci"));

    config
        .set_profile("dev", api_key("api.example.com", KEY), false)
        .unwrap();
    assert_eq!(config.default_profile.as_deref(), Some("ci"));
    config
        .set_profile("prod", api_key("api.example.com", KEY), true)
        .unwrap();
    assert_eq!(config.default_profile.as_deref(), Some("prod"));
    config
        .set_profile("dev", api_key("api.example.com", "other"), true)
        .unwrap();
    assert_eq!(config.default_profile.as_deref(), Some("dev"));

    // A default that names no profile is replaced.
    let mut config = Config {
        default_profile: Some("gone".to_owned()),
        ..Config::default()
    };
    config
        .set_profile("ci", api_key("api.example.com", KEY), false)
        .unwrap();
    assert_eq!(config.default_profile.as_deref(), Some("ci"));
}

#[test]
fn normalizes_profiles() {
    let mut config = Config::default();
    config
        .set_profile(
            "a",
            api_key(" https://API.example.com/ ", &format!(" {KEY}\n")),
            false,
        )
        .unwrap();
    assert_eq!(
        config.profiles["a"],
        api_key("https://api.example.com", KEY)
    );

    config
        .set_profile(
            "b",
            Profile::Pkce {
                endpoint: Some("http://127.0.0.1:8080/".to_owned()),
                auth_session: Some(" work ".to_owned()),
                team_id: Some(" team-1 ".to_owned()),
                sso_domain: Some(" Ada@corp.example.com ".to_owned()),
            },
            false,
        )
        .unwrap();
    assert_eq!(
        config.profiles["b"],
        Profile::Pkce {
            endpoint: Some("http://127.0.0.1:8080".to_owned()),
            auth_session: Some("work".to_owned()),
            team_id: Some("team-1".to_owned()),
            sso_domain: Some("corp.example.com".to_owned()),
        }
    );

    config
        .set_profile(
            "c",
            Profile::Pkce {
                endpoint: Some("api.other.example.com".to_owned()),
                auth_session: Some("other".to_owned()),
                team_id: Some("  ".to_owned()),
                sso_domain: Some("@".to_owned()),
            },
            false,
        )
        .unwrap();
    assert_eq!(
        config.profiles["c"],
        pkce("https://api.other.example.com", Some("other"))
    );
}

#[test]
fn normalizes_endpoints() {
    for (endpoint, normal) in [
        ("api.example.com", "https://api.example.com"),
        ("api.example.com/", "https://api.example.com"),
        ("https://api.example.com", "https://api.example.com"),
        ("https://api.example.com/v0/", "https://api.example.com/v0"),
        ("http://localhost:8080", "http://localhost:8080"),
    ] {
        let mut config = Config::default();
        config
            .set_profile("p", api_key(endpoint, KEY), false)
            .unwrap();
        assert_eq!(config.profiles["p"], api_key(normal, KEY), "{endpoint}");
    }
    for endpoint in ["ftp://api.example.com", "https://", "https://h?x=1"] {
        let err = Config::default()
            .set_profile("p", api_key(endpoint, KEY), false)
            .unwrap_err();
        assert_invalid(&err);
    }
}

#[test]
fn never_quotes_a_url_that_may_hold_a_password() {
    // Built from parts, so that secret scanners don't read them as
    // credentials.
    let userinfo = ["ada", "pa55word"].join(":");
    for endpoint in [
        format!("https://{userinfo}@api.example.com"),
        format!("https://{userinfo}/x@api.example.com"),
        format!("{userinfo}@api.example.com"),
        format!("ftp://{userinfo}@api.example.com"),
    ] {
        let err = Config::default()
            .set_profile("p", api_key(&endpoint, KEY), false)
            .unwrap_err();
        assert_invalid(&err);
        assert!(!err.to_string().contains("pa55word"), "{err}");
    }
    let err = Config::default()
        .set_endpoint_oidc(
            "api.example.com",
            oidc(&format!("https://{userinfo}/x@idp.example.com"), "client-1"),
        )
        .unwrap_err();
    assert_invalid(&err);
    assert!(!err.to_string().contains("pa55word"), "{err}");

    // Without an `@`, the message shows the URL.
    let err = Config::default()
        .set_profile("p", api_key("https://api.example.com?x=1", KEY), false)
        .unwrap_err();
    assert!(
        err.to_string().contains("`https://api.example.com?x=1`"),
        "{err}"
    );
}

#[test]
fn switching_the_type_drops_the_other_types_keys() {
    let mut config = Config::default();
    config
        .set_profile(
            "p",
            Profile::Pkce {
                endpoint: Some("api.example.com".to_owned()),
                auth_session: Some("work".to_owned()),
                team_id: Some("team-1".to_owned()),
                sso_domain: Some("example.com".to_owned()),
            },
            false,
        )
        .unwrap();
    config
        .set_profile("p", api_key("api.example.com", KEY), false)
        .unwrap();
    let yaml = serde_json::to_value(&config).unwrap();
    assert_eq!(
        yaml["profiles"]["p"],
        json!({"auth_type": "api_key", "endpoint": "https://api.example.com", "api_key": KEY})
    );

    config
        .set_profile("p", pkce("api.example.com", Some("work")), false)
        .unwrap();
    let yaml = serde_json::to_value(&config).unwrap();
    assert_eq!(
        yaml["profiles"]["p"],
        json!({"auth_type": "pkce", "endpoint": "https://api.example.com", "auth_session": "work"})
    );
}

#[test]
fn rejects_empty_names_keys_and_missing_endpoints() {
    let mut config = Config::default();
    for name in ["", "  "] {
        let err = config
            .set_profile(name, api_key("api.example.com", KEY), false)
            .unwrap_err();
        assert_invalid(&err);
    }
    for key in ["", " \n", "a\nb"] {
        let err = config
            .set_profile("p", api_key("api.example.com", key), false)
            .unwrap_err();
        assert_invalid(&err);
    }
    assert_eq!(config, Config::default());
}

#[test]
fn needs_a_session_off_the_default_endpoint() {
    let mut config = Config::default();
    for session in [None, Some(""), Some(" "), Some("default"), Some("DEFAULT")] {
        let err = config
            .set_profile("p", pkce("api.example.com", session), false)
            .unwrap_err();
        assert!(
            matches!(
                &err,
                Error::Profile { profile, problem: ProfileProblem::NoSession { endpoint, .. }, .. }
                    if profile == "p" && endpoint == "https://api.example.com"
            ),
            "{err}"
        );
        assert!(err.to_string().contains("session"), "{err}");
    }
    assert!(config.profiles.is_empty());
}

#[test]
fn rejects_session_names_that_are_not_one_safe_path_segment() {
    let mut config = Config::default();
    for session in ["..", ".", "a/b", "a\\b", "a\0b", "../x", "con", "x."] {
        let err = config
            .set_profile("p", pkce("api.example.com", Some(session)), false)
            .unwrap_err();
        assert_invalid(&err);
    }
    assert!(config.profiles.is_empty());
}

#[test]
fn keeps_each_session_on_one_endpoint() {
    let mut config = Config::default();
    config
        .set_profile("a", pkce("api.example.com", Some("work")), false)
        .unwrap();
    config
        .set_profile("b", pkce("https://api.example.com/", Some("work")), false)
        .unwrap();
    for session in ["work", "Work"] {
        let err = config
            .set_profile("c", pkce("api.staging.example.com", Some(session)), false)
            .unwrap_err();
        assert_invalid(&err);
        assert!(err.to_string().contains("profile `a`"), "{err}");
    }
    // A profile can move its own session to another endpoint once it is
    // the only one using it.
    config.delete_profile("b", None).unwrap();
    config
        .set_profile("a", pkce("api.staging.example.com", Some("work")), false)
        .unwrap();
    // API-key profiles have no session.
    config
        .set_profile("k", api_key("api.example.com", KEY), false)
        .unwrap();
}

#[test]
fn keeps_the_domain_of_an_email() {
    for email in [
        "ada@example.com",
        "@example.com",
        "example.com",
        " Ada@x@example.com ",
    ] {
        assert_eq!(
            config::email_domain(email).as_deref(),
            Some("example.com"),
            "{email}"
        );
    }
    for email in ["", " ", "@", "ada@"] {
        assert_eq!(config::email_domain(email), None, "{email:?}");
    }
}

// OIDC settings.

#[test]
fn stores_oidc_settings_with_the_discovery_suffix() {
    let mut config = Config::default();
    let replaced = config
        .set_endpoint_oidc(
            "https://api.staging.example.com/",
            oidc("https://idp.example.com/pool/ ", " client-1 "),
        )
        .unwrap();
    assert!(!replaced);
    let expected = oidc(
        "https://idp.example.com/pool/.well-known/openid-configuration",
        "client-1",
    );
    assert_eq!(config.endpoints["api.staging.example.com"], expected);
    for endpoint in [
        "api.staging.example.com",
        "https://API.staging.example.com/",
    ] {
        assert_eq!(config.endpoint_oidc(endpoint), Some(expected.clone()));
    }
    assert_eq!(config.endpoint_oidc("api.example.com"), None);
    assert_eq!(config.endpoint_oidc("ftp://x"), None);

    // A stored URL without the suffix gets it on read.
    let mut endpoints = BTreeMap::new();
    endpoints.insert(
        "api.dev.example.com".to_owned(),
        oidc("https://idp/pool", "c"),
    );
    let config = Config {
        endpoints,
        ..Config::default()
    };
    assert_eq!(
        config.endpoint_oidc("api.dev.example.com"),
        Some(oidc(
            "https://idp/pool/.well-known/openid-configuration",
            "c"
        ))
    );
}

#[test]
fn reports_an_override_only_when_the_settings_differ() {
    let mut config = Config::default();
    let url = "https://idp.example.com/.well-known/openid-configuration";
    assert!(
        !config
            .set_endpoint_oidc("api.x.com", oidc(url, "a"))
            .unwrap()
    );
    assert!(
        !config
            .set_endpoint_oidc("https://api.x.com", oidc("https://idp.example.com", "a"))
            .unwrap()
    );
    assert!(
        config
            .set_endpoint_oidc("api.x.com", oidc(url, "b"))
            .unwrap()
    );
    assert!(
        config
            .set_endpoint_oidc("api.x.com", oidc("https://other.example.com", "b"))
            .unwrap()
    );
    assert_eq!(config.endpoints.len(), 1);

    // Settings stored under another spelling of the endpoint are replaced.
    let mut config = Config::default();
    config
        .endpoints
        .insert("API.x.com/".to_owned(), oidc(url, "a"));
    assert!(
        config
            .set_endpoint_oidc("api.x.com", oidc(url, "b"))
            .unwrap()
    );
    assert_eq!(config.endpoints.keys().collect::<Vec<_>>(), ["api.x.com"]);
}

#[test]
fn rejects_invalid_oidc_settings() {
    let mut config = Config::default();
    for (endpoint, settings) in [
        ("api.x.com", oidc("https://idp", " ")),
        ("api.x.com", oidc("not a url", "c")),
        ("api.x.com", oidc("ftp://idp", "c")),
        ("", oidc("https://idp", "c")),
    ] {
        let err = config.set_endpoint_oidc(endpoint, settings).unwrap_err();
        assert_invalid(&err);
    }
    assert!(config.endpoints.is_empty());
}

// Deleting profiles.

#[test]
fn deletes_profiles() {
    let mut config = config_with(&["a", "b", "c"]);
    let err = config.delete_profile("x", None).unwrap_err();
    assert_not_found(&err, "x");

    let deleted = config.delete_profile("b", None).unwrap();
    assert_eq!(deleted.new_default.as_deref(), Some("a"));
    assert_eq!(deleted.unused_session, None);
    assert_eq!(config.profiles.keys().collect::<Vec<_>>(), ["a", "c"]);
}

#[test]
fn deleting_the_default_profile_needs_a_new_one() {
    let mut config = config_with(&["a", "b", "c"]);
    let before = config.clone();
    let err = config.delete_profile("a", None).unwrap_err();
    assert!(
        matches!(
            &err,
            Error::Profile { profile, problem: ProfileProblem::NewDefaultNeeded { profiles, .. }, .. }
                if profile == "a" && profiles == &["b", "c"]
        ),
        "{err}"
    );
    let err = config.delete_profile("a", Some("x")).unwrap_err();
    assert_not_found(&err, "x");
    let err = config.delete_profile("a", Some("a")).unwrap_err();
    assert_invalid(&err);
    assert_eq!(config, before);

    let deleted = config.delete_profile("a", Some("c")).unwrap();
    assert_eq!(deleted.new_default.as_deref(), Some("c"));
    assert_eq!(config.default_profile.as_deref(), Some("c"));

    let deleted = config.delete_profile("c", Some("b")).unwrap();
    assert_eq!(deleted.new_default.as_deref(), Some("b"));

    let deleted = config.delete_profile("b", None).unwrap();
    assert_eq!(deleted.new_default, None);
    assert_eq!(config.default_profile, None);
    assert!(config.profiles.is_empty());
}

#[test]
fn moves_the_default_when_another_profile_is_deleted() {
    let mut config = config_with(&["a", "b", "c"]);
    let deleted = config.delete_profile("b", Some("c")).unwrap();
    assert_eq!(deleted.new_default.as_deref(), Some("c"));
    assert_eq!(config.default_profile.as_deref(), Some("c"));
    assert_eq!(config.profiles.keys().collect::<Vec<_>>(), ["a", "c"]);
}

#[test]
fn reports_a_session_that_no_profile_uses_anymore() {
    let mut config = Config::default();
    config
        .set_profile("a", pkce("api.example.com", Some("work")), false)
        .unwrap();
    config
        .set_profile("b", pkce("api.example.com", Some("Work")), false)
        .unwrap();
    config
        .set_profile("c", pkce("api.example.com", Some("solo")), false)
        .unwrap();
    config
        .set_profile("k", api_key("api.example.com", KEY), false)
        .unwrap();

    let deleted = config.delete_profile("b", None).unwrap();
    assert_eq!(deleted.unused_session, None);
    let deleted = config.delete_profile("c", None).unwrap();
    assert_eq!(deleted.unused_session.as_deref(), Some("solo"));
    let deleted = config.delete_profile("k", None).unwrap();
    assert_eq!(deleted.unused_session, None);
    let deleted = config.delete_profile("a", None).unwrap();
    assert_eq!(deleted.unused_session.as_deref(), Some("work"));
}

#[test]
fn reports_the_session_that_a_change_leaves_stale() {
    let work = || pkce("api.example.com", Some("work"));
    let stale = |config: &mut Config, profile: Profile| {
        config
            .set_profile("a", profile, false)
            .unwrap()
            .stale_session
    };

    // Moving the only profile of a session to another endpoint, switching
    // it to an API key or to another session leaves the session stale.
    for profile in [
        pkce("api.staging.example.com", Some("work")),
        api_key("api.example.com", KEY),
        pkce("api.example.com", Some("other")),
    ] {
        let mut config = Config::default();
        assert_eq!(stale(&mut config, work()), None);
        assert_eq!(stale(&mut config, profile).as_deref(), Some("work"));
    }

    // Changing only the team, spelling the session in another case, or the
    // endpoint in another form, leaves it as it was.
    let mut config = Config::default();
    stale(&mut config, work());
    let mut team = work();
    if let Profile::Pkce { team_id, .. } = &mut team {
        *team_id = Some("team-1".to_owned());
    }
    assert_eq!(stale(&mut config, team), None);
    assert_eq!(
        stale(&mut config, pkce("https://API.example.com/", Some("Work"))),
        None
    );

    // A session that another profile still uses there is not stale.
    let mut config = Config::default();
    stale(&mut config, work());
    config.set_profile("b", work(), false).unwrap();
    assert_eq!(stale(&mut config, api_key("api.example.com", KEY)), None);
}

#[tokio::test]
async fn never_reports_an_invalid_session_as_stale() {
    let home = TempDir::new("config");
    write_config(
        home.path(),
        "profiles:\n  a:\n    auth_type: pkce\n    endpoint: https://api.example.com\n    \
         auth_session: ../../.ssh\n",
    );
    let mut config = Config::load(home.path()).await.unwrap();
    let set = config
        .set_profile("a", api_key("api.example.com", KEY), false)
        .unwrap();
    assert_eq!(set.stale_session, None);
}

#[test]
fn stores_each_session_in_lowercase() {
    let mut config = Config::default();
    config
        .set_profile("a", pkce("api.example.com", Some(" Work ")), false)
        .unwrap();
    assert_eq!(
        config.profiles["a"],
        pkce("https://api.example.com", Some("work"))
    );
    let resolved = config.resolve(Some("a"), &Env::default()).unwrap().unwrap();
    assert_eq!(resolved.credential, Credential::Session("work".to_owned()));
}

#[tokio::test]
async fn reports_sessions_of_hand_edited_files_by_one_valid_name() {
    let home = TempDir::new("config");
    let profile = |name: &str, session: &str| {
        format!(
            "  {name}:\n    auth_type: pkce\n    endpoint: https://api.example.com\n    \
             auth_session: {session}\n"
        )
    };

    // Spellings that differ only in case are one session.
    write_config(
        home.path(),
        &format!(
            "profiles:\n{}{}",
            profile("a", "work"),
            profile("b", "Work")
        ),
    );
    let mut config = Config::load(home.path()).await.unwrap();
    for name in ["a", "b"] {
        let resolved = config
            .resolve(Some(name), &Env::default())
            .unwrap()
            .unwrap();
        assert_eq!(resolved.credential, Credential::Session("work".to_owned()));
    }
    let deleted = config.delete_profile("a", None).unwrap();
    assert_eq!(deleted.unused_session, None);
    let deleted = config.delete_profile("b", None).unwrap();
    assert_eq!(deleted.unused_session.as_deref(), Some("work"));

    // A name that is not a valid session is never returned.
    for session in ["../../.ssh", "a/b", "con", "\"..\""] {
        write_config(
            home.path(),
            &format!("profiles:\n{}", profile("p", session)),
        );
        let mut config = Config::load(home.path()).await.unwrap();
        let deleted = config.delete_profile("p", None).unwrap();
        assert_eq!(deleted.unused_session, None, "{session}");
    }
}

// Masking and secrets.

#[test]
fn masks_api_keys() {
    assert_eq!(config::mask_api_key("abcdefgh"), "ab****gh");
    assert_eq!(config::mask_api_key("abcde"), "ab****de");
    assert_eq!(config::mask_api_key("abcd"), "****");
    assert_eq!(config::mask_api_key("ab"), "**");
    assert_eq!(config::mask_api_key(""), "");
    assert_eq!(config::mask_api_key("ébcdéf"), "éb****éf");

    let config = config_with(&["a"]);
    let masked = config.masked();
    let masked_key = config::mask_api_key(KEY);
    assert_eq!(masked_key, "fi****3a");
    assert_eq!(
        masked.profiles["a"],
        api_key("https://api.example.com", &masked_key)
    );
    let listed = serde_json::to_string(&masked).unwrap();
    assert!(!listed.contains(KEY), "{listed}");
    assert_eq!(
        serde_json::to_value(&masked).unwrap(),
        json!({
            "default_profile": "a",
            "profiles": {"a": {
                "auth_type": "api_key",
                "endpoint": "https://api.example.com",
                "api_key": masked_key,
            }},
            "endpoints": {},
        })
    );
}

#[test]
fn never_shows_an_api_key_in_debug_output() {
    let config = config_with(&["a"]);
    let env = Env {
        api_key: Some(KEY.to_owned()),
        ..Env::default()
    };
    let resolved = config.resolve(None, &env).unwrap().unwrap();
    for debug in [
        format!("{config:?}"),
        format!("{:?}", config.profiles["a"]),
        format!("{env:?}"),
        format!("{resolved:?}"),
        format!("{:?}", resolved.credential),
        format!("{:?}", resolved.client_builder(Path::new("/nowhere"))),
    ] {
        assert!(!debug.contains(KEY), "{debug}");
    }
    assert!(format!("{env:?}").contains("<redacted>"));
}

#[test]
fn never_shows_an_api_key_in_profile_params_debug_output() {
    let params = ProfileParams::ApiKey {
        endpoint: Some("api.example.com".to_owned()),
        api_key: KEY.to_owned(),
    };
    let debug = format!("{params:?}");
    assert!(!debug.contains(KEY), "{debug}");
    assert!(debug.contains("<redacted>"), "{debug}");
    assert!(debug.contains("api.example.com"), "{debug}");
}

// Resolution.

/// A configuration with an API-key profile `ci` (the default) and a PKCE
/// profile `work` with a team.
fn two_profiles() -> Config {
    let mut config = config_with(&["ci"]);
    config
        .set_profile(
            "work",
            Profile::Pkce {
                endpoint: Some("api.work.example.com".to_owned()),
                auth_session: Some("work".to_owned()),
                team_id: Some("team-1".to_owned()),
                sso_domain: None,
            },
            false,
        )
        .unwrap();
    config
}

fn env(profile: Option<&str>, endpoint: Option<&str>, api_key: Option<&str>) -> Env {
    Env {
        profile: profile.map(str::to_owned),
        endpoint: endpoint.map(str::to_owned),
        api_key: api_key.map(str::to_owned),
    }
}

#[test]
fn picks_the_env_var_then_the_flag_then_the_default_profile() {
    let config = two_profiles();
    let profile = |flag: Option<&str>, env_profile: Option<&str>| {
        let resolved = config.resolve(flag, &env(env_profile, None, None));
        resolved.unwrap().unwrap().profile.unwrap()
    };
    assert_eq!(profile(None, None), "ci");
    assert_eq!(profile(None, Some("work")), "work");
    assert_eq!(profile(Some("ci"), Some("work")), "work");
    assert_eq!(profile(Some("work"), Some(" ")), "work");
    assert_eq!(profile(Some(" work "), None), "work");
    assert_eq!(profile(Some(""), Some(" ")), "ci");

    for (flag, env_profile) in [(Some("ci"), Some("nope")), (Some("nope"), None)] {
        let err = config
            .resolve(flag, &env(env_profile, None, None))
            .unwrap_err();
        assert_not_found(&err, "nope");
    }
}

#[test]
fn reports_an_env_var_profile_that_replaced_the_flag() {
    let config = two_profiles();
    let overrides = |flag: Option<&str>, env_profile: Option<&str>| {
        let resolved = config.resolve(flag, &env(env_profile, None, None));
        resolved.unwrap().unwrap().overrides
    };
    assert_eq!(
        overrides(Some("ci"), Some("work")),
        [EnvOverride::Profile {
            profile: "work".to_owned(),
            given: "ci".to_owned(),
        }]
    );
    // The same profile, or only one of them, replaces nothing.
    assert_eq!(overrides(Some("work"), Some(" work ")), []);
    assert_eq!(overrides(None, Some("work")), []);
    assert_eq!(overrides(Some("work"), None), []);
}

#[test]
fn resolves_the_endpoint_and_the_credential_of_a_profile() {
    let config = two_profiles();
    let resolved = config.resolve(None, &Env::default()).unwrap().unwrap();
    assert_eq!(resolved.endpoint, "https://api.example.com");
    assert_eq!(resolved.credential, Credential::ApiKey(KEY.to_owned()));
    assert_eq!(resolved.team_id, None);
    assert_eq!(resolved.overrides, []);

    let resolved = config
        .resolve(Some("work"), &Env::default())
        .unwrap()
        .unwrap();
    assert_eq!(resolved.endpoint, "https://api.work.example.com");
    assert_eq!(resolved.credential, Credential::Session("work".to_owned()));
    assert_eq!(resolved.team_id.as_deref(), Some("team-1"));
    assert_eq!(resolved.overrides, []);
}

#[test]
fn lets_the_environment_override_the_profile_and_says_so() {
    let config = two_profiles();
    let resolved = config
        .resolve(
            None,
            &env(None, Some("http://localhost:9000/"), Some("other-key")),
        )
        .unwrap()
        .unwrap();
    assert_eq!(resolved.endpoint, "http://localhost:9000");
    assert_eq!(
        resolved.credential,
        Credential::ApiKey("other-key".to_owned())
    );
    assert_eq!(
        resolved.overrides,
        [
            EnvOverride::Endpoint {
                profile: "ci".to_owned()
            },
            EnvOverride::ApiKey {
                profile: "ci".to_owned()
            },
        ]
    );

    // `FICO_API_KEY` replaces a PKCE session, and the team goes with it.
    let resolved = config
        .resolve(Some("work"), &env(None, None, Some(KEY)))
        .unwrap()
        .unwrap();
    assert_eq!(resolved.credential, Credential::ApiKey(KEY.to_owned()));
    assert_eq!(resolved.team_id, None);
    assert_eq!(
        resolved.overrides,
        [EnvOverride::ApiKey {
            profile: "work".to_owned()
        }]
    );
}

#[test]
fn reports_no_override_when_the_values_match() {
    let config = two_profiles();
    let resolved = config
        .resolve(None, &env(None, Some("API.example.com/"), Some(KEY)))
        .unwrap()
        .unwrap();
    assert_eq!(resolved.endpoint, "https://api.example.com");
    assert_eq!(resolved.overrides, []);
}

#[test]
fn works_from_the_environment_alone() {
    let config = Config::default();
    assert_eq!(config.resolve(None, &Env::default()).unwrap(), None);
    assert_eq!(
        config
            .resolve(None, &env(None, Some("api.example.com"), None))
            .unwrap(),
        None
    );

    let resolved = config
        .resolve(None, &env(None, Some("api.example.com"), Some(KEY)))
        .unwrap()
        .unwrap();
    assert_eq!(resolved.profile, None);
    assert_eq!(resolved.endpoint, "https://api.example.com");
    assert_eq!(resolved.credential, Credential::ApiKey(KEY.to_owned()));
    assert_eq!(resolved.overrides, []);

    let err = config
        .resolve(None, &env(None, Some("ftp://x"), Some(KEY)))
        .unwrap_err();
    assert_invalid(&err);
    let err = config
        .resolve(Some("ci"), &env(None, None, Some(KEY)))
        .unwrap_err();
    assert_not_found(&err, "ci");
}

#[tokio::test]
async fn checks_profiles_that_were_edited_by_hand() {
    let home = TempDir::new("config");
    for profile in [
        "    auth_type: api_key\n    endpoint: https://h.example.com\n    api_key: \" \"\n",
        "    auth_type: pkce\n    endpoint: https://h.example.com\n    auth_session: ../x\n",
    ] {
        write_config(home.path(), &format!("profiles:\n  p:\n{profile}"));
        let config = Config::load(home.path()).await.unwrap();
        let err = config.resolve(Some("p"), &Env::default()).unwrap_err();
        assert_invalid(&err);
    }
    // Without a session, a PKCE profile on another endpoint than the default
    // has none to use.
    write_config(
        home.path(),
        "profiles:\n  p:\n    auth_type: pkce\n    endpoint: https://h.example.com\n",
    );
    let config = Config::load(home.path()).await.unwrap();
    let err = config.resolve(Some("p"), &Env::default()).unwrap_err();
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

    // A profile written by hand without an endpoint uses the default one.
    write_config(
        home.path(),
        "profiles:\n  p:\n    auth_type: api_key\n    api_key: k\n",
    );
    let config = Config::load(home.path()).await.unwrap();
    let resolved = config.resolve(Some("p"), &Env::default()).unwrap().unwrap();
    assert_eq!(resolved.endpoint, default());

    // A stored endpoint written by hand is normalized when it is used.
    write_config(
        home.path(),
        "profiles:\n  p:\n    auth_type: api_key\n    endpoint: API.example.com/\n    api_key: k\n",
    );
    let config = Config::load(home.path()).await.unwrap();
    let resolved = config.resolve(Some("p"), &Env::default()).unwrap().unwrap();
    assert_eq!(resolved.endpoint, "https://api.example.com");
}

// The client.

#[tokio::test]
async fn builds_a_client_that_sends_the_api_key() {
    let server = MockServer::start(vec![MockResponse::json(200, json!({"items": []}))]).await;
    let mut config = Config::default();
    config
        .set_profile("ci", api_key(server.url(), KEY), false)
        .unwrap();
    let resolved = config.resolve(None, &Env::default()).unwrap().unwrap();
    let client = resolved
        .client_builder(Path::new("/nowhere"))
        .build()
        .unwrap();
    client.get("/v1/applications").send().await.unwrap();

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    let expected = format!("Bearer {KEY}");
    assert_eq!(requests[0].header("authorization"), Some(expected.as_str()));
    assert_eq!(requests[0].header(ACCOUNT_HEADER), None);
}

#[tokio::test]
async fn builds_a_client_that_reads_the_tokens_of_a_pkce_session() {
    let server = MockServer::start(vec![MockResponse::json(200, json!({}))]).await;
    let home = TempDir::new("config");
    let mut config = Config::default();
    config
        .set_profile(
            "work",
            Profile::Pkce {
                endpoint: Some(server.url().to_owned()),
                auth_session: Some("work".to_owned()),
                team_id: Some("team-1".to_owned()),
                sso_domain: None,
            },
            false,
        )
        .unwrap();
    // Building makes no request; the first request needs a login.
    let resolved = config.resolve(None, &Env::default()).unwrap().unwrap();
    let client = resolved.client_builder(home.path()).build().unwrap();
    let err = client.get("/v1/applications").send().await.unwrap_err();
    assert!(
        matches!(&err, Error::NotLoggedIn { session, .. } if session == "work"),
        "{err}"
    );
    assert!(server.requests().is_empty());

    // With `FICO_API_KEY`, the key is the bearer and no team is sent.
    let resolved = config
        .resolve(None, &env(None, None, Some(KEY)))
        .unwrap()
        .unwrap();
    resolved
        .client_builder(home.path())
        .build()
        .unwrap()
        .get("/v1/applications")
        .send()
        .await
        .unwrap();
    let requests = server.requests();
    assert_eq!(requests[0].header(ACCOUNT_HEADER), None);
    let expected = format!("Bearer {KEY}");
    assert_eq!(requests[0].header("authorization"), Some(expected.as_str()));
}

#[test]
fn resolves_the_oidc_settings_of_a_pkce_session() {
    let mut config = Config::default();
    let settings = oidc("https://idp.example.com", "client-1");
    config
        .set_endpoint_oidc("api.example.com", settings)
        .unwrap();
    config
        .set_profile("work", pkce("api.example.com", Some("work")), false)
        .unwrap();
    let resolved = config.resolve(None, &Env::default()).unwrap().unwrap();
    assert_eq!(
        resolved.oidc.map(|oidc| oidc.client_id).as_deref(),
        Some("client-1")
    );
    // An API key needs none.
    let resolved = config
        .resolve(None, &env(None, None, Some(KEY)))
        .unwrap()
        .unwrap();
    assert_eq!(resolved.oidc, None);
}

fn default() -> &'static str {
    config::DEFAULT_ENDPOINT.expect("a default endpoint")
}

#[test]
fn gives_a_profile_without_an_endpoint_the_default_one() {
    let mut config = Config::default();
    for (name, endpoint) in [("a", None), ("b", Some(" ".to_owned()))] {
        let profile = Profile::ApiKey {
            endpoint,
            api_key: KEY.to_owned(),
        };
        config.set_profile(name, profile, false).unwrap();
        // It stores none, and resolves to the default.
        assert!(matches!(
            &config.profiles[name],
            Profile::ApiKey { endpoint: None, .. }
        ));
        let resolved = config
            .resolve(Some(name), &Env::default())
            .unwrap()
            .unwrap();
        assert_eq!(resolved.endpoint, default());
    }
}

#[test]
fn resolves_the_environment_alone_to_the_default_endpoint() {
    let config = Config::default();
    let resolved = config
        .resolve(None, &env(None, None, Some(KEY)))
        .unwrap()
        .unwrap();
    assert_eq!(resolved.profile, None);
    assert_eq!(resolved.endpoint, default());
    assert_eq!(resolved.credential, Credential::ApiKey(KEY.to_owned()));
}

#[test]
fn takes_the_default_session_and_built_in_oidc_on_the_default_endpoint() {
    let mut config = Config::default();
    // The same endpoint without its scheme, in capitals and with a slash.
    let shouting = format!(
        "{}/",
        default().trim_start_matches("https://").to_uppercase()
    );
    // The endpoint may be left out or spelled out, and so may the session.
    for (name, endpoint, session) in [
        ("a", None, None),
        ("b", Some(default()), Some("default")),
        ("c", Some(shouting.as_str()), Some("")),
    ] {
        let profile = Profile::Pkce {
            endpoint: endpoint.map(str::to_owned),
            auth_session: session.map(str::to_owned),
            team_id: None,
            sso_domain: None,
        };
        config.set_profile(name, profile, false).unwrap();
        let resolved = config
            .resolve(Some(name), &Env::default())
            .unwrap()
            .unwrap();
        assert_eq!(resolved.endpoint, default(), "{name}");
        assert_eq!(
            resolved.credential,
            Credential::Session("default".to_owned())
        );
        let oidc = resolved.oidc.expect("built-in OIDC settings");
        assert!(
            oidc.oidc_discovery_url
                .ends_with("/.well-known/openid-configuration")
        );
        assert!(!oidc.client_id.is_empty());
        assert_eq!(oidc.callback_port, None);
    }
    // Spelled-out ones are stored as the default, a session as none.
    assert!(matches!(
        &config.profiles["b"],
        Profile::Pkce {
            auth_session: None,
            ..
        }
    ));
    // Another endpoint still needs a session of its own, and has no built-in
    // OIDC settings.
    let err = config
        .set_profile("d", pkce("api.example.com", None), false)
        .unwrap_err();
    assert!(
        matches!(
            err,
            Error::Profile {
                problem: ProfileProblem::NoSession { .. },
                ..
            }
        ),
        "{err}"
    );
    assert_eq!(config.endpoint_oidc("api.example.com"), None);
}

#[test]
fn prefers_stored_oidc_settings_over_the_built_in_ones() {
    let mut config = Config::default();
    let built_in = config.endpoint_oidc(default()).unwrap();
    let replaced = config
        .set_endpoint_oidc(default(), oidc("https://idp.example.com", "client-1"))
        .unwrap();
    assert!(replaced);
    let stored = config.endpoint_oidc(default()).unwrap();
    assert_eq!(stored.client_id, "client-1");
    assert_ne!(stored, built_in);
    config
        .set_profile("a", pkce(default(), None), false)
        .unwrap();
    let resolved = config.resolve(Some("a"), &Env::default()).unwrap().unwrap();
    assert_eq!(resolved.oidc, Some(stored));
}
