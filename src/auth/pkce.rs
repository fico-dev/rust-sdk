//! The PKCE login (RFC 7636): the verifier, its S256 challenge and the
//! state, the authorization URL, the loopback callback and the code
//! exchange.

use std::collections::HashMap;
use std::convert::Infallible;
use std::fmt;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aws_lc_rs::digest::{SHA256, digest};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use reqwest::Url;
use serde::Serialize;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinSet;

use super::oidc::{self, Endpoints};
use super::store;
use super::{Session, base64, sso_provider};
use crate::{Error, Result};

/// The callback port when the endpoint's OIDC settings set none.
pub const DEFAULT_CALLBACK_PORT: u16 = 56_734;
/// The scopes a login asks for.
pub const SCOPES: &str = "email openid profile";
/// How long a login waits for the browser.
pub const BROWSER_TIMEOUT: Duration = Duration::from_secs(300);

/// Options of a [`login`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct LoginOptions {
    /// Goes through the identity provider's logout first, so the browser
    /// starts a fresh session instead of reusing a signed-in one.
    pub force: bool,
}

impl LoginOptions {
    /// The default options: no `force`.
    pub fn new() -> LoginOptions {
        LoginOptions::default()
    }

    /// Sets [`LoginOptions::force`].
    pub fn force(mut self, force: bool) -> LoginOptions {
        self.force = force;
        self
    }
}

/// A login waiting for the browser. Open [`PendingLogin::authorization_url`]
/// in a browser (and show it, for machines without one), then await
/// [`PendingLogin::complete`]. Dropping it stops listening.
pub struct PendingLogin {
    authorization_url: String,
    sso_lookup_failed: bool,
    home: PathBuf,
    session: Session,
    client_id: String,
    token_endpoint: String,
    redirect_uri: String,
    state: String,
    verifier: String,
    timeout: Duration,
    callback: oneshot::Receiver<HashMap<String, String>>,
    // Serves the callback until the login is dropped.
    _server: JoinSet<()>,
}

/// A session that logged in, which `auth login` reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct LoggedIn {
    /// The session's name.
    pub session: String,
    /// The endpoint its tokens are for.
    pub endpoint: String,
    /// The profiles that use it.
    pub profiles: Vec<String>,
    /// When its access token expires, as an RFC 3339 timestamp in UTC. It
    /// is refreshed on use.
    pub expires_at: String,
}

/// Starts the PKCE login of `session`, whose tokens go to the FICO home
/// `home`.
///
/// It reads the endpoint's OIDC discovery document, makes a random verifier,
/// its S256 challenge and a random state, and listens on
/// `127.0.0.1:<port>`, the port of the endpoint's OIDC settings (56734 by
/// default). With an [`Session::sso_domain`], it first looks up the
/// domain's SSO provider, whose login the URL then goes through; when the
/// lookup fails, the login falls back to the standard one and
/// [`PendingLogin::sso_lookup_failed`] says so. With
/// [`LoginOptions::force`], the URL goes through the identity provider's
/// logout first.
///
/// Opening the browser is the caller's job: nothing here prints.
///
/// # Errors
///
/// Returns [`Error::Invalid`] if the session's endpoint has no OIDC
/// settings ([`Session::oidc`]), [`Error::Login`] if the discovery document
/// lacks an endpoint (or `force` needs a logout endpoint it lacks) or the
/// callback port can't be bound, e.g. because another login holds it, and
/// the transport's errors for the discovery document.
pub async fn login(home: &Path, session: &Session, options: LoginOptions) -> Result<PendingLogin> {
    let oidc = session.oidc.as_ref().ok_or_else(|| Error::Invalid {
        reason: format!(
            "endpoint `{}` of session `{}` has no OIDC settings",
            session.endpoint, session.session
        ),
    })?;
    // Checked before the browser is involved.
    store::path(home, &session.session)?;
    let endpoints = oidc::discover(oidc).await?;
    let (identity_provider, sso_lookup_failed) = match &session.sso_domain {
        Some(domain) => match sso_provider(&session.endpoint, domain).await {
            Ok(provider) => (provider, false),
            Err(_) => (None, true),
        },
        None => (None, false),
    };

    let port = oidc.callback_port.unwrap_or(DEFAULT_CALLBACK_PORT);
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let listener = TcpListener::bind(address)
        .await
        .map_err(|source| Error::Login {
            reason: format!(
                "could not listen for the login's callback on {address} ({source}): is another \
                 login running?"
            ),
        })?;
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(port);
    let redirect_uri = format!("http://127.0.0.1:{port}");

    let verifier = random_text(64)?;
    let state = random_text(16)?;
    let challenge = base64::encode_url(digest(&SHA256, verifier.as_bytes()).as_ref());
    let authorization_url = authorization_url(
        &endpoints,
        &AuthorizationParams {
            client_id: &oidc.client_id,
            redirect_uri: &redirect_uri,
            challenge: &challenge,
            state: &state,
            identity_provider: identity_provider.as_deref(),
            force: options.force,
        },
    )?;

    let (sender, callback) = oneshot::channel();
    let mut server = JoinSet::new();
    server.spawn(serve_callback(listener, sender));
    Ok(PendingLogin {
        authorization_url,
        sso_lookup_failed,
        home: home.to_owned(),
        session: session.clone(),
        client_id: oidc.client_id.clone(),
        token_endpoint: endpoints.token,
        redirect_uri,
        state,
        verifier,
        timeout: BROWSER_TIMEOUT,
        callback,
        _server: server,
    })
}

impl PendingLogin {
    /// The URL to open in a browser. It holds the S256 challenge and the
    /// state, never the verifier.
    pub fn authorization_url(&self) -> &str {
        &self.authorization_url
    }

    /// The redirect URI the identity provider sends the browser back to,
    /// `http://127.0.0.1:<port>`.
    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    /// Whether the SSO lookup of the session's `sso_domain` failed, so the
    /// login fell back to the standard one. The caller warns about it.
    pub fn sso_lookup_failed(&self) -> bool {
        self.sso_lookup_failed
    }

    /// The session that logs in.
    pub fn session(&self) -> &Session {
        &self.session
    }

    /// Waits at most `timeout` for the browser instead of 5 minutes.
    pub fn with_timeout(mut self, timeout: Duration) -> PendingLogin {
        self.timeout = timeout;
        self
    }

    /// Waits for the identity provider's callback, then exchanges its code
    /// for tokens and stores them.
    ///
    /// The callback is checked in this order: an `error` fails the login,
    /// then a `state` that is not this login's, then a missing `code`. The
    /// tokens are checked as the refresh checks them: a `token_type` of
    /// `bearer`, and an ID token issued for the client (`aud`) that has not
    /// expired (`exp`), read without checking its signature.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Wait`] if no callback comes in time,
    /// [`Error::Login`] if the callback or the tokens fail the checks, or
    /// the tokens have no ID token, [`Error::Status`] with only the OAuth2
    /// error if the exchange is refused, the transport's errors, and
    /// [`Error::Io`] if the tokens can't be stored.
    pub async fn complete(self) -> Result<LoggedIn> {
        let login = |reason: String| Error::Login { reason };
        let params = tokio::time::timeout(self.timeout, self.callback)
            .await
            .map_err(|_| Error::Wait {
                reason: format!(
                    "no login callback within {} seconds",
                    self.timeout.as_secs()
                ),
            })?
            .map_err(|_| login("the login's callback listener stopped".to_owned()))?;
        if let Some(error) = params.get("error") {
            let description = params.get("error_description").unwrap_or(error);
            return Err(login(format!(
                "the identity provider reported `{error}`: {description}"
            )));
        }
        if params.get("state") != Some(&self.state) {
            return Err(login(
                "the callback's state does not match this login's, which may be a login CSRF \
                 attempt: log in again"
                    .to_owned(),
            ));
        }
        let code = params
            .get("code")
            .filter(|code| !code.is_empty())
            .ok_or_else(|| login("the callback has no authorization code".to_owned()))?;

        let form = [
            ("grant_type", "authorization_code"),
            ("client_id", self.client_id.as_str()),
            ("code", code.as_str()),
            ("redirect_uri", self.redirect_uri.as_str()),
            ("code_verifier", self.verifier.as_str()),
        ];
        let tokens = oidc::request_tokens(
            &self.token_endpoint,
            &form,
            &self.client_id,
            &self.session.endpoint,
        )
        .await?;
        if super::token::bearer(&tokens).is_none() {
            return Err(login(
                "the identity provider issued no ID token: does the login ask for the `openid` \
                 scope?"
                    .to_owned(),
            ));
        }
        store::save(&self.home, &self.session.session, &tokens).await?;
        Ok(LoggedIn {
            session: self.session.session,
            endpoint: self.session.endpoint,
            profiles: self.session.profiles,
            expires_at: tokens.expires_at,
        })
    }
}

impl fmt::Debug for PendingLogin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PendingLogin")
            .field("session", &self.session.session)
            .field("redirect_uri", &self.redirect_uri)
            .field("sso_lookup_failed", &self.sso_lookup_failed)
            .finish_non_exhaustive()
    }
}

/// What the authorization URL carries.
struct AuthorizationParams<'a> {
    client_id: &'a str,
    redirect_uri: &'a str,
    challenge: &'a str,
    state: &'a str,
    identity_provider: Option<&'a str>,
    force: bool,
}

/// The URL that starts the login in the browser: the authorization
/// endpoint with the login's parameters, or, with `force`, the logout
/// endpoint with the same parameters, after which Cognito goes on to the
/// login.
fn authorization_url(endpoints: &Endpoints, params: &AuthorizationParams<'_>) -> Result<String> {
    let base = match params.force {
        true => endpoints
            .end_session
            .as_deref()
            .ok_or_else(|| Error::Login {
                reason: "the identity provider has no logout endpoint (`end_session_endpoint`), \
                     which a forced login needs"
                    .to_owned(),
            })?,
        false => &endpoints.authorization,
    };
    let mut url = Url::parse(base).map_err(|_| Error::Login {
        reason: "the identity provider's login URL is not a URL".to_owned(),
    })?;
    {
        let mut query = url.query_pairs_mut();
        query
            .append_pair("response_type", "code")
            .append_pair("client_id", params.client_id)
            .append_pair("redirect_uri", params.redirect_uri)
            .append_pair("scope", SCOPES)
            .append_pair("code_challenge_method", "S256")
            .append_pair("code_challenge", params.challenge)
            .append_pair("state", params.state);
        if let Some(provider) = params.identity_provider {
            query.append_pair("identity_provider", provider);
        }
    }
    Ok(url.into())
}

/// `len` random bytes from the operating system's CSPRNG, as base64url
/// without padding: 64 bytes give an 86-character verifier, within RFC
/// 7636's 43 to 128.
fn random_text(len: usize) -> Result<String> {
    let mut bytes = vec![0; len];
    aws_lc_rs::rand::fill(&mut bytes).map_err(|_| Error::Login {
        reason: "the operating system's random number generator failed".to_owned(),
    })?;
    Ok(base64::encode_url(&bytes))
}

/// The page the browser shows after the callback.
const DONE_PAGE: &str = "<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"utf-8\">\
    <title>FICO login</title></head><body><p>You can close this tab and return to your \
    terminal.</p></body></html>";

/// Serves the callback on `listener`: the first request to `/` hands its
/// query parameters to `sender`; other paths get a 404, and the wait goes
/// on. Requests after the first one get the same page and are ignored.
async fn serve_callback(listener: TcpListener, sender: oneshot::Sender<HashMap<String, String>>) {
    let sender = Arc::new(Mutex::new(Some(sender)));
    let mut connections = JoinSet::new();
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            // E.g. out of file descriptors: wait instead of spinning.
            tokio::time::sleep(Duration::from_millis(100)).await;
            continue;
        };
        let sender = Arc::clone(&sender);
        let service = service_fn(move |request: Request<Incoming>| {
            let sender = Arc::clone(&sender);
            async move { Ok::<_, Infallible>(callback_response(&request, &sender)) }
        });
        connections.spawn(async move {
            let _ = http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service)
                .await;
        });
        while connections.try_join_next().is_some() {}
    }
}

/// The response to one request to the callback listener.
fn callback_response(
    request: &Request<Incoming>,
    sender: &Mutex<Option<oneshot::Sender<HashMap<String, String>>>>,
) -> Response<String> {
    let page = |status: StatusCode, body: &str| {
        let mut response = Response::new(body.to_owned());
        *response.status_mut() = status;
        response.headers_mut().insert(
            hyper::header::CONTENT_TYPE,
            hyper::header::HeaderValue::from_static("text/html; charset=utf-8"),
        );
        response
    };
    if request.uri().path() != "/" {
        return page(StatusCode::NOT_FOUND, "Not found.");
    }
    let query = request.uri().query().unwrap_or_default();
    let params: HashMap<String, String> = Url::parse(&format!("http://callback/?{query}"))
        .map(|url| url.query_pairs().into_owned().collect())
        .unwrap_or_default();
    if let Some(sender) = sender.lock().ok().and_then(|mut sender| sender.take()) {
        let _ = sender.send(params);
    }
    page(StatusCode::OK, DONE_PAGE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn makes_urlsafe_random_text_of_the_rfc_7636_length() {
        let verifier = random_text(64).unwrap();
        assert_eq!(verifier.len(), 86);
        assert!(
            verifier
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        );
        assert_ne!(verifier, random_text(64).unwrap());
    }

    #[test]
    fn builds_the_authorization_and_logout_urls() {
        let endpoints = Endpoints {
            authorization: "https://idp/oauth2/authorize".to_owned(),
            token: "https://idp/oauth2/token".to_owned(),
            end_session: Some("https://idp/logout?x=1".to_owned()),
        };
        let mut params = AuthorizationParams {
            client_id: "client-1",
            redirect_uri: "http://127.0.0.1:56734",
            challenge: "abc",
            state: "st",
            identity_provider: Some("Okta Corp"),
            force: false,
        };
        assert_eq!(
            authorization_url(&endpoints, &params).unwrap(),
            "https://idp/oauth2/authorize?response_type=code&client_id=client-1\
             &redirect_uri=http%3A%2F%2F127.0.0.1%3A56734&scope=email+openid+profile\
             &code_challenge_method=S256&code_challenge=abc&state=st&identity_provider=Okta+Corp"
        );
        params.force = true;
        params.identity_provider = None;
        let url = authorization_url(&endpoints, &params).unwrap();
        assert!(url.starts_with("https://idp/logout?x=1&response_type=code&client_id=client-1"));
        let without_logout = Endpoints {
            end_session: None,
            ..endpoints
        };
        let err = authorization_url(&without_logout, &params).unwrap_err();
        assert!(matches!(err, Error::Login { .. }), "{err}");
    }
}
