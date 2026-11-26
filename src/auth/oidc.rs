//! The identity provider: OIDC discovery, the token endpoint and the checks
//! of what it issues, which the login and the refresh share.

use std::net::IpAddr;
use std::time::Duration;

use reqwest::Url;
use reqwest::header::CONTENT_TYPE;
use serde::Deserialize;
use serde_json::Value;

use super::base64;
use super::store::Tokens;
use crate::config::EndpointOidc;
use crate::time::{now_unix, unix_rfc3339};
use crate::{Client, Error, Result, RetryPolicy};

/// How long the discovery document may take.
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a token request may take.
const TOKEN_TIMEOUT: Duration = Duration::from_secs(30);
/// The token lifetime when a response gives none.
const DEFAULT_EXPIRES_IN: i64 = 3_600;

/// The endpoints of an identity provider, from its discovery document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Endpoints {
    /// Where the browser logs in.
    pub(crate) authorization: String,
    /// Where codes and refresh tokens are exchanged for tokens.
    pub(crate) token: String,
    /// Where the browser logs out, if the provider has one.
    pub(crate) end_session: Option<String>,
}

/// A client for the identity provider's URLs, which are absolute. It sends
/// no bearer token and has no base path of its own.
pub(crate) fn client(url: &str) -> Result<Client> {
    let url = checked_url(url, "the OIDC discovery URL")?;
    Client::builder(url.as_str())
        .retry(RetryPolicy::default().max_retries(2))
        .build()
}

/// Reads the discovery document of `oidc`.
///
/// # Errors
///
/// Returns the transport's errors, [`Error::Decode`] if the document is not
/// JSON, and [`Error::Login`] if it lacks the authorization or the token
/// endpoint, or names one that is not `https` (or `http` on a loopback
/// host).
pub(crate) async fn discover(oidc: &EndpointOidc) -> Result<Endpoints> {
    #[derive(Deserialize)]
    struct Document {
        authorization_endpoint: Option<String>,
        token_endpoint: Option<String>,
        end_session_endpoint: Option<String>,
    }
    let client = client(&oidc.oidc_discovery_url)?;
    let document: Document = client
        .get(&oidc.oidc_discovery_url)
        .timeout(DISCOVERY_TIMEOUT)
        .send_json()
        .await?;
    let required = |url: Option<String>, name: &str| {
        let url = url.ok_or_else(|| Error::Login {
            reason: format!("the OIDC discovery document has no `{name}`"),
        })?;
        checked_url(&url, name).map(String::from)
    };
    Ok(Endpoints {
        authorization: required(document.authorization_endpoint, "authorization_endpoint")?,
        token: required(document.token_endpoint, "token_endpoint")?,
        end_session: document
            .end_session_endpoint
            .map(|url| checked_url(&url, "end_session_endpoint").map(String::from))
            .transpose()?,
    })
}

/// `url`, which tokens or codes go to, if it is `https`, or `http` on a
/// loopback host (a local identity provider or a test).
///
/// # Errors
///
/// Returns [`Error::Login`] otherwise, naming `what`.
pub(crate) fn checked_url(url: &str, what: &str) -> Result<Url> {
    let parsed = Url::parse(url).map_err(|_| Error::Login {
        reason: format!("{what} is not a URL"),
    })?;
    match parsed.scheme() {
        "https" => Ok(parsed),
        "http" if is_loopback(&parsed) => Ok(parsed),
        _ => Err(Error::Login {
            reason: format!("{what} must be an `https` URL, so tokens never travel in the clear"),
        }),
    }
}

/// Whether `url`'s host is this machine.
pub(crate) fn is_loopback(url: &Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    match host.parse::<IpAddr>() {
        Ok(ip) => ip.is_loopback(),
        Err(_) => host.eq_ignore_ascii_case("localhost"),
    }
}

/// Posts `form` to the token endpoint `url` and checks the tokens it
/// issues for `client_id` (see [`check`]). The tokens are for `endpoint`.
///
/// # Errors
///
/// Returns [`Error::Status`] for an error response (whose body names the
/// OAuth2 error, see [`is_invalid_grant`]), the transport's errors, and
/// [`Error::Login`] if the tokens fail the checks.
pub(crate) async fn request_tokens(
    url: &str,
    form: &[(&str, &str)],
    client_id: &str,
    endpoint: &str,
) -> Result<Tokens> {
    let url = checked_url(url, "the token endpoint")?;
    let body = form_encode(form);
    let response: Value = client(url.as_str())?
        .post(url.as_str())
        .header(CONTENT_TYPE.as_str(), "application/x-www-form-urlencoded")
        .body(body)
        .timeout(TOKEN_TIMEOUT)
        .send_json()
        .await
        .map_err(oauth_error)?;
    check(&response, client_id, endpoint)
}

/// The tokens in the token response `response`, after these checks: an
/// `access_token`, a `token_type` of `bearer` in any case, and, when there
/// is an `id_token`, its `aud` is `client_id` and its `exp` is in the
/// future. The ID token's signature is not checked: the TLS-protected token
/// response is the trust anchor.
///
/// # Errors
///
/// Returns [`Error::Login`] if a check fails. The reason never quotes a
/// token.
pub(crate) fn check(response: &Value, client_id: &str, endpoint: &str) -> Result<Tokens> {
    let login = |reason: &str| Error::Login {
        reason: reason.to_owned(),
    };
    let text = |key: &str| response.get(key).and_then(Value::as_str).map(str::to_owned);
    let access_token = text("access_token")
        .filter(|token| !token.is_empty())
        .ok_or_else(|| login("the token response has no `access_token`"))?;
    let token_type =
        text("token_type").ok_or_else(|| login("the token response has no `token_type`"))?;
    if !token_type.eq_ignore_ascii_case("bearer") {
        return Err(login("the token response's `token_type` is not `bearer`"));
    }
    let id_token = text("id_token");
    if let Some(id_token) = &id_token {
        check_id_token(id_token, client_id)?;
    }
    let expires_in = response
        .get("expires_in")
        .and_then(Value::as_i64)
        .unwrap_or(DEFAULT_EXPIRES_IN);
    Ok(Tokens {
        endpoint: endpoint.to_owned(),
        token_type,
        access_token,
        id_token,
        refresh_token: text("refresh_token").filter(|token| !token.is_empty()),
        expires_at: unix_rfc3339(now_unix().saturating_add(expires_in)),
    })
}

/// Checks the claims of the ID token `id_token`: `aud` is `client_id` (or a
/// list that holds it), and `exp` is in the future. Returns `exp`.
///
/// # Errors
///
/// Returns [`Error::Login`] if it is not a JWT, or a claim fails.
pub(crate) fn check_id_token(id_token: &str, client_id: &str) -> Result<i64> {
    let login = |reason: &str| Error::Login {
        reason: reason.to_owned(),
    };
    let claims = id_token
        .split('.')
        .nth(1)
        .and_then(base64::decode_url)
        .and_then(|payload| serde_json::from_slice::<Value>(&payload).ok())
        .ok_or_else(|| login("the ID token is not a JWT"))?;
    let audience_matches = match claims.get("aud") {
        Some(Value::String(aud)) => aud == client_id,
        Some(Value::Array(auds)) => auds.iter().any(|aud| aud.as_str() == Some(client_id)),
        _ => false,
    };
    if !audience_matches {
        return Err(login("the ID token was not issued for this client (`aud`)"));
    }
    let exp = claims
        .get("exp")
        .and_then(Value::as_f64)
        .ok_or_else(|| login("the ID token has no `exp`"))?;
    let exp = exp as i64;
    if exp < now_unix() {
        return Err(login("the ID token has expired (`exp`)"));
    }
    Ok(exp)
}

/// Whether `error` is the token endpoint's `invalid_grant` (RFC 6749,
/// section 5.2): a 400 whose body's `error` is `invalid_grant`. The refresh
/// token was revoked or expired.
pub(crate) fn is_invalid_grant(error: &Error) -> bool {
    match error {
        Error::Status {
            status: 400, body, ..
        } => serde_json::from_str::<Value>(body)
            .ok()
            .is_some_and(|body| body.get("error").and_then(Value::as_str) == Some("invalid_grant")),
        _ => false,
    }
}

/// A failed token request's error with only the OAuth2 `error` and
/// `error_description` of its body (RFC 6749, section 5.2), so it never
/// shows anything else the identity provider echoed.
pub(crate) fn oauth_error(error: Error) -> Error {
    let Error::Status { status, url, body } = error else {
        return error;
    };
    let body = serde_json::from_str::<Value>(&body).ok();
    let field = |key: &str| {
        let value = body.as_ref()?.get(key)?.as_str()?;
        Some((key.to_owned(), Value::String(value.to_owned())))
    };
    let fields: serde_json::Map<String, Value> = [field("error"), field("error_description")]
        .into_iter()
        .flatten()
        .collect();
    Error::Status {
        status,
        url,
        body: if fields.is_empty() {
            String::new()
        } else {
            Value::Object(fields).to_string()
        },
    }
}

/// `pairs` as `application/x-www-form-urlencoded`.
pub(crate) fn form_encode(pairs: &[(&str, &str)]) -> String {
    let mut url = Url::parse("http://form.invalid/").expect("a valid URL");
    url.query_pairs_mut().extend_pairs(pairs);
    url.query().unwrap_or_default().to_owned()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// A JWT whose payload is `claims`, with no real signature.
    fn jwt(claims: Value) -> String {
        let encode = super::base64::encode_url;
        format!(
            "{}.{}.sig",
            encode(b"{\"alg\":\"none\"}"),
            encode(claims.to_string().as_bytes())
        )
    }

    fn response(id_token: &str) -> Value {
        json!({
            "access_token": "access",
            "token_type": "Bearer",
            "id_token": id_token,
            "refresh_token": "refresh",
            "expires_in": 60,
        })
    }

    #[test]
    fn keeps_tokens_that_pass_the_checks() {
        let id_token = jwt(json!({"aud": "client-1", "exp": now_unix() + 600}));
        let tokens = check(&response(&id_token), "client-1", "https://api").unwrap();
        assert_eq!(tokens.endpoint, "https://api");
        assert_eq!(tokens.id_token.as_deref(), Some(id_token.as_str()));
        assert_eq!(tokens.refresh_token.as_deref(), Some("refresh"));
        let expires_at = crate::time::parse_unix(&tokens.expires_at).unwrap();
        assert!((expires_at - now_unix() - 60).abs() <= 1);
        // A list of audiences, and a lowercase type.
        let id_token = jwt(json!({"aud": ["x", "client-1"], "exp": now_unix() + 600}));
        let mut body = response(&id_token);
        body["token_type"] = json!("bearer");
        check(&body, "client-1", "https://api").unwrap();
    }

    #[test]
    fn rejects_tokens_that_fail_the_checks() {
        let now = now_unix();
        let good = jwt(json!({"aud": "client-1", "exp": now + 600}));
        let mut cases = vec![
            (
                response(&jwt(json!({"aud": "other", "exp": now + 600}))),
                "aud",
            ),
            (
                response(&jwt(json!({"aud": "client-1", "exp": now - 1}))),
                "expired",
            ),
            (response(&jwt(json!({"aud": "client-1"}))), "exp"),
            (response("not-a-jwt"), "JWT"),
        ];
        let mut mac = response(&good);
        mac["token_type"] = json!("mac");
        cases.push((mac, "token_type"));
        let mut untyped = response(&good);
        untyped.as_object_mut().unwrap().remove("token_type");
        cases.push((untyped, "token_type"));
        let mut no_access = response(&good);
        no_access.as_object_mut().unwrap().remove("access_token");
        cases.push((no_access, "access_token"));
        for (body, why) in cases {
            let err = check(&body, "client-1", "https://api").unwrap_err();
            assert!(
                matches!(&err, Error::Login { reason } if reason.contains(why)),
                "{why}: {err}"
            );
            assert!(!err.to_string().contains("access\""), "{err}");
        }
    }

    #[test]
    fn recognizes_invalid_grant() {
        let status = |status, body: &str| Error::Status {
            status,
            url: "https://idp/token".to_owned(),
            body: body.to_owned(),
        };
        assert!(is_invalid_grant(&status(
            400,
            r#"{"error":"invalid_grant"}"#
        )));
        assert!(!is_invalid_grant(&status(
            400,
            r#"{"error":"invalid_client"}"#
        )));
        assert!(!is_invalid_grant(&status(
            401,
            r#"{"error":"invalid_grant"}"#
        )));
        assert!(!is_invalid_grant(&status(400, "invalid_grant")));
    }

    #[test]
    fn keeps_only_the_oauth_error_of_a_body() {
        let error = Error::Status {
            status: 400,
            url: "https://idp/token".to_owned(),
            body: r#"{"error":"invalid_grant","error_description":"expired","refresh_token":"r"}"#
                .to_owned(),
        };
        let Error::Status { body, .. } = oauth_error(error) else {
            panic!("not a status error");
        };
        assert_eq!(
            body,
            r#"{"error":"invalid_grant","error_description":"expired"}"#
        );
        let error = Error::Status {
            status: 500,
            url: "https://idp/token".to_owned(),
            body: "code=secret".to_owned(),
        };
        let Error::Status { body, .. } = oauth_error(error) else {
            panic!("not a status error");
        };
        assert_eq!(body, "");
    }

    #[test]
    fn sends_tokens_only_over_https_or_to_this_machine() {
        for url in [
            "https://idp.example.com/token",
            "http://127.0.0.1:8080/token",
            "http://localhost/token",
            "http://[::1]:9/token",
        ] {
            assert!(checked_url(url, "x").is_ok(), "{url}");
        }
        for url in ["http://idp.example.com/token", "ftp://idp/x", "not a url"] {
            let err = checked_url(url, "the token endpoint").unwrap_err();
            assert!(matches!(err, Error::Login { .. }), "{url}: {err}");
        }
    }

    #[test]
    fn encodes_forms() {
        assert_eq!(
            form_encode(&[("grant_type", "refresh_token"), ("x", "a b&c=d/é")]),
            "grant_type=refresh_token&x=a+b%26c%3Dd%2F%C3%A9"
        );
    }
}
