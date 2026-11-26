//! HTTP transport shared by every part of the SDK.
//!
//! [`Client`] holds the defaults (base URL, auth, time limits, retries) and
//! a connection pool. Requests are built with [`RequestBuilder`], which can
//! override any default for a single request. Auth is a fixed bearer token
//! or a [`TokenSource`], which the client asks before each request.

mod pagination;
mod request;
mod response;
mod retry;

// The proxy tests need the crate-private `ClientBuilder::proxy`, the push
// tests with a stand-in for `uv` the crate-private
// `Platform::push_workspace_with`, and the MCP tests a platform to call.
#[cfg(test)]
#[path = "../tests/mock_server/mod.rs"]
pub(crate) mod mock_server;

use std::fmt;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use reqwest::Url;
use reqwest::header::{self, HeaderMap, HeaderName, HeaderValue};
use reqwest::redirect;

use crate::error::redact_query;
use crate::tool::BoxFuture;
use crate::{Error, Result};

pub use pagination::Paginator;
pub use request::{Method, RequestBuilder};
pub use response::Response;
pub use retry::RetryPolicy;

use request::{Auth, HeaderOp, Parts};
use retry::Retries;

const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(20);
const DEFAULT_USER_AGENT: &str = concat!("fico-sdk/", env!("CARGO_PKG_VERSION"));

/// HTTP client with retries, timeouts and sensible defaults.
///
/// Cloning a `Client` is cheap: clones share the same configuration and
/// connection pool, so create one and reuse it.
///
/// # Examples
///
/// ```no_run
/// # async fn run() -> fico_sdk::Result<()> {
/// use fico_sdk::Client;
///
/// let client = Client::new("https://api.example.com")?;
/// let response = client.get("/v1/applications").send().await?;
/// println!("{}", response.status());
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
}

struct Inner {
    http: reqwest::Client,
    base_url: Url,
    headers: HeaderMap,
    bearer_token: Option<HeaderValue>,
    token_source: Option<Arc<dyn TokenSource>>,
    connect_timeout: Duration,
    read_timeout: Duration,
    timeout: Option<Duration>,
    retry: RetryPolicy,
}

/// Where a [`Client`] gets its bearer token before each request, e.g. a
/// login whose tokens expire and are refreshed. Set it with
/// [`ClientBuilder::token_source`].
///
/// The client asks it before each request that sends the client's token to
/// the base URL's origin (each page of a [`Paginator`] too, but not each
/// retry), so a long-lived client never sends an expired token. Building a
/// client never asks it.
pub trait TokenSource: Send + Sync + 'static {
    /// The token to send as `Authorization: Bearer <token>`.
    ///
    /// # Errors
    ///
    /// An error fails the request before it is sent, and is returned as is.
    fn token(&self) -> BoxFuture<'_, Result<String>>;
}

impl Client {
    /// Creates a client for `base_url` with the default configuration.
    ///
    /// Equivalent to `Client::builder(base_url).build()`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidUrl`] if `base_url` is not an absolute
    /// `http` or `https` URL, or [`Error::Init`] if the underlying HTTP
    /// client cannot be initialized.
    pub fn new(base_url: impl AsRef<str>) -> Result<Client> {
        Client::builder(base_url).build()
    }

    /// Starts configuring a client for `base_url`.
    pub fn builder(base_url: impl AsRef<str>) -> ClientBuilder {
        ClientBuilder::new(base_url)
    }

    /// A client like this one that sends `Authorization: Bearer <token>`
    /// instead of its bearer token or its [`TokenSource`]. It shares this
    /// client's connection pool, headers, timeout and retry policy.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidHeader`] if `token` is not a valid header
    /// value.
    pub fn with_bearer_token(&self, token: impl AsRef<str>) -> Result<Client> {
        let inner = &self.inner;
        Ok(Client {
            inner: Arc::new(Inner {
                http: inner.http.clone(),
                base_url: inner.base_url.clone(),
                headers: inner.headers.clone(),
                bearer_token: Some(bearer_header(token.as_ref())?),
                token_source: None,
                connect_timeout: inner.connect_timeout,
                read_timeout: inner.read_timeout,
                timeout: inner.timeout,
                retry: inner.retry.clone(),
            }),
        })
    }

    /// Starts a `GET` request. See [`Client::request`].
    pub fn get(&self, path: impl AsRef<str>) -> RequestBuilder {
        self.request(Method::Get, path)
    }

    /// Starts a `POST` request. See [`Client::request`].
    pub fn post(&self, path: impl AsRef<str>) -> RequestBuilder {
        self.request(Method::Post, path)
    }

    /// Starts a `PUT` request. See [`Client::request`].
    pub fn put(&self, path: impl AsRef<str>) -> RequestBuilder {
        self.request(Method::Put, path)
    }

    /// Starts a `PATCH` request. See [`Client::request`].
    pub fn patch(&self, path: impl AsRef<str>) -> RequestBuilder {
        self.request(Method::Patch, path)
    }

    /// Starts a `DELETE` request. See [`Client::request`].
    pub fn delete(&self, path: impl AsRef<str>) -> RequestBuilder {
        self.request(Method::Delete, path)
    }

    /// Starts a request with the given method.
    ///
    /// `path` is appended to the base URL's path, so `/v1/apps` on a client
    /// for `https://host/api` targets `https://host/api/v1/apps`. An absolute
    /// `http://` or `https://` URL is used as-is; the client's bearer token is
    /// only sent to URLs with the same origin as the base URL.
    ///
    /// `path` must already be percent-encoded: `?` and `#` start the query
    /// and fragment, so encode any user-supplied segment before putting it
    /// in the path. Dot segments (`..`, also written `%2e%2e`) are resolved,
    /// and a path that ends up outside the base URL's path is reported as
    /// [`Error::InvalidUrl`] when the request is sent.
    pub fn request(&self, method: Method, path: impl AsRef<str>) -> RequestBuilder {
        RequestBuilder::new(self.clone(), method, self.resolve(path.as_ref()))
    }

    /// Resolves a request path or absolute URL against the base URL.
    fn resolve(&self, path: &str) -> Result<Url> {
        // Only `scheme://...` is absolute, so `things:batchGet` and
        // `https:batchGet` are paths.
        if let Ok(url) = Url::parse(path)
            && url.has_authority()
            && path
                .split_once(':')
                .is_some_and(|(_, rest)| rest.starts_with("//"))
        {
            return check_http_url(url, path);
        }
        let base = self.inner.base_url.as_str().trim_end_matches('/');
        let path = path.trim_start_matches('/');
        let joined = if path.is_empty() {
            base.to_owned()
        } else {
            format!("{base}/{path}")
        };
        let url = Url::parse(&joined).map_err(|e| invalid_url(&joined, e))?;
        // Dot segments must not climb out of the base path.
        let base_path = self.inner.base_url.path().trim_end_matches('/');
        let inside = url
            .path()
            .strip_prefix(base_path)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'));
        if !inside {
            return Err(invalid_url(&joined, "path leaves the base URL's path"));
        }
        Ok(url)
    }

    /// Sends a request, retrying according to its retry policy.
    pub(crate) async fn execute(&self, parts: &Parts) -> Result<Response> {
        let mut url = parts.url.clone();
        if !parts.query.is_empty() {
            url.query_pairs_mut().extend_pairs(&parts.query);
        }
        let sourced = self.sourced_token(parts, &url).await?;
        let headers = self.headers_for(parts, &url, sourced.as_ref());
        let read = parts.read_timeout.unwrap_or(self.inner.read_timeout);
        let total = parts.timeout.or(self.inner.timeout);
        let policy = parts.retry.as_ref().unwrap_or(&self.inner.retry);
        let retryable_method = policy.allows_method(parts.method);

        let mut retries = Retries::default();
        loop {
            let outcome = self.attempt(parts, &url, &headers, read, total).await;
            let delay = match &outcome {
                Ok(response)
                    if retryable_method
                        && policy.allows_status(parts.method, response.status())
                        && policy.has_status_retries(&retries) =>
                {
                    let busy = busy_detail(response);
                    if busy.should_retry == Some(false) {
                        None
                    } else {
                        let wait = busy.retry_in.or_else(|| retry_after(response));
                        policy.retry_status(&mut retries, wait)
                    }
                }
                Err(error) if retryable_method && policy.allows_error(parts.method, error) => {
                    policy.retry_transport(&mut retries)
                }
                _ => None,
            };
            match delay {
                Some(delay) => tokio::time::sleep(delay).await,
                None => return outcome.and_then(Response::error_for_status),
            }
        }
    }

    /// Sends a single attempt and buffers the response body, within the
    /// limits of [`read_response`](Self::read_response), and at most
    /// `total`, if set, for the whole attempt.
    async fn attempt(
        &self,
        parts: &Parts,
        url: &Url,
        headers: &HeaderMap,
        read: Duration,
        total: Option<Duration>,
    ) -> Result<Response> {
        let attempt = self.read_response(parts, url, headers, read);
        match total {
            Some(total) => within(total, url, attempt).await?,
            None => attempt.await,
        }
    }

    /// Sends a request and reads its response. The head must arrive within
    /// the connect limit plus `read` of the start, plus another `read` for a
    /// request with a body, and each chunk of the body within `read` of the
    /// previous one. reqwest itself only limits connecting.
    async fn read_response(
        &self,
        parts: &Parts,
        url: &Url,
        headers: &HeaderMap,
        read: Duration,
    ) -> Result<Response> {
        let mut request = self
            .inner
            .http
            .request(parts.method.to_reqwest(), url.clone())
            .headers(headers.clone());
        if let Some(body) = &parts.body {
            request = request.body(body.clone());
        }
        let connect = self.inner.connect_timeout;
        let map_err = |e: reqwest::Error| transport_error(e, url, read);
        // `requests` gives connecting, sending the body and waiting for the
        // head a limit each. reqwest doesn't say when the body is sent, so
        // the head gets the sum.
        let sending = if parts.body.is_some() {
            read
        } else {
            Duration::ZERO
        };
        let head = connect.saturating_add(sending).saturating_add(read);
        let mut response = within(head, url, request.send()).await?.map_err(map_err)?;
        let status = response.status().as_u16();
        let final_url = response.url().to_string();
        let response_headers = response.headers().clone();
        let mut body = Vec::new();
        while let Some(chunk) = within(read, url, response.chunk())
            .await?
            .map_err(map_err)?
        {
            body.extend_from_slice(&chunk);
        }
        Ok(Response::new(status, final_url, response_headers, body))
    }

    /// The token of the client's [`TokenSource`], when the request sends the
    /// client's token to `url`.
    async fn sourced_token(&self, parts: &Parts, url: &Url) -> Result<Option<HeaderValue>> {
        let Some(source) = &self.inner.token_source else {
            return Ok(None);
        };
        let same_origin = url.origin() == self.inner.base_url.origin();
        if !matches!(parts.auth, Auth::Default) || !same_origin {
            return Ok(None);
        }
        bearer_header(&source.token().await?).map(Some)
    }

    /// Merges the client defaults with the request's header operations.
    /// `sourced` is the token of the client's [`TokenSource`], if it was
    /// asked.
    fn headers_for(&self, parts: &Parts, url: &Url, sourced: Option<&HeaderValue>) -> HeaderMap {
        let same_origin = url.origin() == self.inner.base_url.origin();
        let mut headers = if same_origin {
            self.inner.headers.clone()
        } else {
            self.inner
                .headers
                .get_all(header::USER_AGENT)
                .iter()
                .map(|value| (header::USER_AGENT, value.clone()))
                .collect()
        };
        let token = match &parts.auth {
            Auth::Default if same_origin => self.inner.bearer_token.as_ref().or(sourced),
            Auth::Default | Auth::None => None,
            Auth::Bearer(token) => Some(token),
        };
        if let Some(token) = token {
            headers.insert(header::AUTHORIZATION, token.clone());
        }
        if let Some(content_type) = &parts.content_type {
            headers.insert(header::CONTENT_TYPE, content_type.clone());
        }
        for op in &parts.headers {
            match op {
                HeaderOp::Set(name, value) => {
                    headers.insert(name.clone(), value.clone());
                }
                HeaderOp::Remove(name) => {
                    headers.remove(name);
                }
            }
        }
        headers
    }
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("base_url", &self.inner.base_url.as_str())
            .field("token_source", &self.inner.token_source.is_some())
            .field("connect_timeout", &self.inner.connect_timeout)
            .field("read_timeout", &self.inner.read_timeout)
            .field("timeout", &self.inner.timeout)
            .field("retry", &self.inner.retry)
            .finish_non_exhaustive()
    }
}

/// Configures a [`Client`]. Created with [`Client::builder`].
///
/// # Time limits
///
/// Each attempt of a request has three limits. Running out of the connect
/// limit fails the attempt with [`Error::Connect`], since the server never
/// got the request, and running out of another with [`Error::Timeout`],
/// whose `after` is the limit that ran out. The [`RetryPolicy`] decides
/// whether to retry the attempt.
///
/// * [`connect_timeout`](Self::connect_timeout), 10s by default, limits
///   opening a connection: DNS, TCP, TLS and a proxy's tunnel. A request on
///   a pooled connection doesn't connect.
/// * [`read_timeout`](Self::read_timeout), 20s by default, limits each wait
///   for the server.
///   * The response head must arrive within the connect limit plus the read
///     limit of the start of the attempt, plus another read limit for a
///     request with a body: room to connect, to send the body and to wait
///     for the answer. Python's `requests` limits these one at a time, so
///     an attempt that it completes never fails here. A server that never
///     answers fails the attempt after 30s, or 50s with a body, and `after`
///     is that sum.
///   * Each chunk of the response body must arrive within the read limit of
///     the previous one, so a large response that keeps arriving never
///     runs out of it.
/// * [`timeout`](Self::timeout), none by default, limits the whole attempt,
///   from connecting until the response body is read.
///
/// [`RequestBuilder::read_timeout`] and [`RequestBuilder::timeout`] override
/// the last two for a single request.
///
/// With the defaults, a server that keeps sending a few bytes within each
/// read limit holds the request open, and its body in memory, for as long as
/// it keeps sending. To bound how long a request can take, set
/// [`timeout`](Self::timeout), or wrap the call in [`tokio::time::timeout`];
/// dropping the future cancels the request and closes its connection.
///
/// # Proxies
///
/// Requests go through the proxies that the `HTTPS_PROXY`, `HTTP_PROXY` and
/// `ALL_PROXY` environment variables name, except for the hosts that
/// `NO_PROXY` lists; each variable may also be lowercase. The operating
/// system's proxy settings are not read, so set these variables to use the
/// proxy they configure.
///
/// The bearer token and default headers go only to the base URL's origin,
/// as without a proxy. A proxy sees them only for a plain `http://` origin:
/// for `https://`, it only forwards the encrypted connection, and sees the
/// host and port and its own credentials.
///
/// # Examples
///
/// ```no_run
/// # fn run() -> fico_sdk::Result<()> {
/// use std::time::Duration;
/// use fico_sdk::{Client, RetryPolicy};
///
/// let client = Client::builder("https://api.example.com")
///     .bearer_token("YOUR_TOKEN")
///     .read_timeout(Duration::from_secs(60))
///     .retry(RetryPolicy::default().max_retries(3))
///     .build()?;
/// # Ok(())
/// # }
/// ```
#[must_use = "a builder does nothing until `build` is called"]
pub struct ClientBuilder {
    base_url: String,
    bearer_token: Option<String>,
    token_source: Option<Arc<dyn TokenSource>>,
    headers: Vec<(String, String)>,
    user_agent: String,
    connect_timeout: Duration,
    read_timeout: Duration,
    timeout: Option<Duration>,
    retry: RetryPolicy,
    #[cfg(test)]
    proxy: Option<String>,
}

impl ClientBuilder {
    /// Starts configuring a client for `base_url`. Same as
    /// [`Client::builder`].
    pub fn new(base_url: impl AsRef<str>) -> ClientBuilder {
        ClientBuilder {
            base_url: base_url.as_ref().to_owned(),
            bearer_token: None,
            token_source: None,
            headers: Vec::new(),
            user_agent: DEFAULT_USER_AGENT.to_owned(),
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            read_timeout: DEFAULT_READ_TIMEOUT,
            timeout: None,
            retry: RetryPolicy::default(),
            #[cfg(test)]
            proxy: None,
        }
    }

    /// Sends `Authorization: Bearer <token>` with every request to the base
    /// URL's origin. Replaces a [`token_source`](Self::token_source).
    pub fn bearer_token(mut self, token: impl Into<String>) -> ClientBuilder {
        self.bearer_token = Some(token.into());
        self.token_source = None;
        self
    }

    /// Asks `source` for the bearer token before each request to the base
    /// URL's origin (see [`TokenSource`]). Replaces a
    /// [`bearer_token`](Self::bearer_token).
    pub fn token_source(mut self, source: impl TokenSource) -> ClientBuilder {
        self.token_source = Some(Arc::new(source));
        self.bearer_token = None;
        self
    }

    /// Adds a header sent with every request to the base URL's origin,
    /// replacing any previous value. Use [`RequestBuilder::header`] to send a
    /// header to another origin.
    ///
    /// Redirects from the base URL's origin to another origin are not
    /// followed, so these headers never leave it: the request fails with
    /// [`Error::Redirect`] instead.
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> ClientBuilder {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// Replaces the default `User-Agent` (`fico-sdk/<version>`).
    pub fn user_agent(mut self, user_agent: impl Into<String>) -> ClientBuilder {
        self.user_agent = user_agent.into();
        self
    }

    /// Sets the time limit for opening a connection, including TLS and a
    /// proxy's tunnel. Defaults to 10s. Running out of it fails the attempt
    /// with [`Error::Connect`]. See [Time limits](Self#time-limits).
    pub fn connect_timeout(mut self, timeout: Duration) -> ClientBuilder {
        self.connect_timeout = timeout;
        self
    }

    /// Sets the time limit for each wait for the server: for sending the
    /// request body, for the response head, and for each chunk of the
    /// response body. Defaults to 20s. Raise it for requests that upload
    /// large bodies or that the server takes long to answer. See [Time
    /// limits](Self#time-limits) for how the head's limit adds up.
    pub fn read_timeout(mut self, timeout: Duration) -> ClientBuilder {
        self.read_timeout = timeout;
        self
    }

    /// Sets a time limit for each whole request attempt, from connecting
    /// until the whole response body is read. There is none by default, so
    /// a response that keeps arriving is read however long it takes. See
    /// [Time limits](Self#time-limits).
    pub fn timeout(mut self, timeout: Duration) -> ClientBuilder {
        self.timeout = Some(timeout);
        self
    }

    /// Sets the retry policy. Defaults to [`RetryPolicy::default`].
    pub fn retry(mut self, retry: RetryPolicy) -> ClientBuilder {
        self.retry = retry;
        self
    }

    /// Validates the configuration and builds the client.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidUrl`] if the base URL is not an absolute
    /// `http` or `https` URL, [`Error::InvalidHeader`] if a header, the
    /// bearer token or the user agent is not a valid header value, or
    /// [`Error::Init`] if the underlying HTTP client cannot be initialized.
    pub fn build(self) -> Result<Client> {
        let mut base_url = Url::parse(&self.base_url)
            .map_err(|e| invalid_url(&self.base_url, e))
            .and_then(|url| check_http_url(url, &self.base_url))?;
        base_url.set_query(None);
        base_url.set_fragment(None);

        let mut headers = HeaderMap::new();
        headers.insert(
            header::USER_AGENT,
            header_value("user-agent", &self.user_agent)?,
        );
        for (name, value) in &self.headers {
            headers.insert(header_name(name)?, header_value(name, value)?);
        }
        let bearer_token = self
            .bearer_token
            .map(|token| bearer_header(&token))
            .transpose()?;

        // The other limits are `Client::attempt`'s: reqwest's read timeout
        // runs from the start of the request, not from the end of its body,
        // and can't be set per request.
        let http = reqwest::Client::builder()
            .redirect(redirect_policy(base_url.clone()))
            .connect_timeout(self.connect_timeout);
        #[cfg(test)]
        let http = match &self.proxy {
            Some(proxy) => http.proxy(reqwest::Proxy::all(proxy.as_str()).map_err(init_error)?),
            None => http,
        };
        let http = http.build().map_err(init_error)?;

        Ok(Client {
            inner: Arc::new(Inner {
                http,
                base_url,
                headers,
                bearer_token,
                token_source: self.token_source,
                connect_timeout: self.connect_timeout,
                read_timeout: self.read_timeout,
                timeout: self.timeout,
                retry: self.retry,
            }),
        })
    }

    /// Sends every request through `proxy` instead of the environment's
    /// proxies. Tests can't set environment
    /// variables: `std::env::set_var` is `unsafe` in edition 2024.
    #[cfg(test)]
    fn proxy(mut self, proxy: impl Into<String>) -> ClientBuilder {
        self.proxy = Some(proxy.into());
        self
    }
}

impl fmt::Debug for ClientBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientBuilder")
            .field("base_url", &self.base_url)
            .field("token_source", &self.token_source.is_some())
            .field("user_agent", &self.user_agent)
            .field("connect_timeout", &self.connect_timeout)
            .field("read_timeout", &self.read_timeout)
            .field("timeout", &self.timeout)
            .field("retry", &self.retry)
            .finish_non_exhaustive()
    }
}

fn init_error(error: reqwest::Error) -> Error {
    Error::Init {
        source: Box::new(error),
    }
}

/// Runs `future`, or fails with [`Error::Timeout`] after `limit`.
async fn within<T>(limit: Duration, url: &Url, future: impl Future<Output = T>) -> Result<T> {
    tokio::time::timeout(limit, future)
        .await
        .map_err(|_| Error::Timeout {
            url: redact_query(url.as_str()),
            after: limit,
        })
}

/// Follows up to 10 redirects, like reqwest's default, but stops when a
/// request to the base URL's origin would be redirected to another origin, so
/// default headers are not sent there.
fn redirect_policy(base_url: Url) -> redirect::Policy {
    let origin = base_url.origin();
    redirect::Policy::custom(move |attempt| {
        // `previous` starts with the original URL, which is not a redirect.
        if attempt.previous().len() > 10 {
            attempt.error("too many redirects")
        } else if attempt
            .previous()
            .first()
            .is_some_and(|url| url.origin() == origin)
            && attempt.url().origin() != origin
        {
            attempt.stop()
        } else {
            attempt.follow()
        }
    })
}

/// What a `429` response's body says about retrying it.
fn busy_detail(response: &Response) -> retry::BusyDetail {
    if response.status() != 429 {
        return retry::BusyDetail::default();
    }
    retry::parse_busy_detail(response.bytes())
}

/// Wait requested by a `429` or `503` response's `Retry-After` header.
fn retry_after(response: &Response) -> Option<Duration> {
    if !matches!(response.status(), 429 | 503) {
        return None;
    }
    retry::parse_retry_after(response.header("retry-after")?, SystemTime::now())
}

fn check_http_url(url: Url, raw: &str) -> Result<Url> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(Error::InvalidUrl {
            url: raw.to_owned(),
            reason: "scheme must be http or https".to_owned(),
        });
    }
    if url.host_str().is_none_or(str::is_empty) {
        return Err(Error::InvalidUrl {
            url: raw.to_owned(),
            reason: "URL has no host".to_owned(),
        });
    }
    Ok(url)
}

fn invalid_url(raw: &str, error: impl fmt::Display) -> Error {
    Error::InvalidUrl {
        url: raw.to_owned(),
        reason: error.to_string(),
    }
}

pub(crate) fn header_name(name: &str) -> Result<HeaderName> {
    HeaderName::from_bytes(name.as_bytes()).map_err(|_| Error::InvalidHeader {
        name: name.to_owned(),
    })
}

pub(crate) fn header_value(name: &str, value: &str) -> Result<HeaderValue> {
    HeaderValue::from_str(value).map_err(|_| Error::InvalidHeader {
        name: name.to_owned(),
    })
}

pub(crate) fn bearer_header(token: &str) -> Result<HeaderValue> {
    let mut value = header_value("authorization", &format!("Bearer {token}"))?;
    value.set_sensitive(true);
    Ok(value)
}

/// Maps a reqwest error. Of the client's limits, reqwest only enforces the
/// connect limit, and running out of it is a connection error, whose source
/// says it timed out: the server never got the request. Another timeout is
/// the operating system's, during a read, and is reported with the read
/// limit. The error keeps the URL with its query redacted, and reqwest's
/// error none, so neither shows a presigned URL's credentials.
fn transport_error(error: reqwest::Error, url: &Url, read: Duration) -> Error {
    let url = redact_query(url.as_str());
    let error = error.without_url();
    if error.is_connect() {
        Error::Connect {
            url,
            source: Box::new(error),
        }
    } else if error.is_timeout() {
        Error::Timeout { url, after: read }
    } else {
        Error::Request {
            url,
            source: Box::new(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::mock_server::{MockResponse, MockServer};
    use super::*;

    fn client(base_url: &str) -> Client {
        Client::new(base_url).unwrap()
    }

    fn resolve(base_url: &str, path: &str) -> String {
        client(base_url).resolve(path).unwrap().to_string()
    }

    #[test]
    fn appends_paths_to_the_base_path() {
        for base in ["https://host/api", "https://host/api/"] {
            for path in ["/v1/apps", "v1/apps", "//v1/apps"] {
                assert_eq!(
                    resolve(base, path),
                    "https://host/api/v1/apps",
                    "{base} + {path}"
                );
            }
        }
        assert_eq!(resolve("https://host", "/v1/apps"), "https://host/v1/apps");
        assert_eq!(resolve("https://host/api", ""), "https://host/api");
        assert_eq!(
            resolve("https://host/api", "/v1/apps?x=1"),
            "https://host/api/v1/apps?x=1"
        );
    }

    #[test]
    fn treats_paths_with_a_colon_as_paths() {
        assert_eq!(
            resolve("https://host/api", "things:batchGet"),
            "https://host/api/things:batchGet"
        );
        assert_eq!(
            resolve("https://host/api", "/search?next=https://other"),
            "https://host/api/search?next=https://other"
        );
        for path in ["https:batchGet", "https:evil.example/x", "http:/single"] {
            assert_eq!(
                resolve("https://host/api", path),
                format!("https://host/api/{path}")
            );
        }
    }

    #[test]
    fn rejects_paths_that_leave_the_base_path() {
        let client = client("https://host/api/v1");
        for path in [
            "..",
            "/apps/../../../admin",
            "%2e%2e/%2E%2E/admin",
            "../v1x",
        ] {
            let err = client.resolve(path).unwrap_err();
            assert!(matches!(err, Error::InvalidUrl { .. }), "{path}: {err}");
        }
        assert_eq!(
            resolve("https://host/api/v1", "/apps/.."),
            "https://host/api/v1/"
        );
        assert_eq!(
            resolve("https://host/api/v1", "../v1/apps"),
            "https://host/api/v1/apps"
        );
        assert_eq!(resolve("https://host", "/a/../b"), "https://host/b");
    }

    #[test]
    fn drops_query_and_fragment_from_the_base_url() {
        assert_eq!(
            resolve("https://host/api?x=1#top", "/v1"),
            "https://host/api/v1"
        );
    }

    #[test]
    fn uses_absolute_urls_as_is() {
        let url = "https://bucket.s3.amazonaws.com/key?X-Amz-Signature=abc";
        assert_eq!(resolve("https://host/api", url), url);
    }

    #[test]
    fn rejects_invalid_base_urls() {
        for base in ["", "host/api", "ftp://host", "mailto:a@b.c", "https://"] {
            let err = Client::new(base).unwrap_err();
            assert!(matches!(err, Error::InvalidUrl { .. }), "{base}: {err}");
        }
    }

    #[test]
    fn rejects_non_http_absolute_urls() {
        let err = client("https://host")
            .resolve("ftp://other/file")
            .unwrap_err();
        assert!(matches!(err, Error::InvalidUrl { .. }));
    }

    #[test]
    fn rejects_invalid_headers() {
        let err = Client::builder("https://host")
            .header("bad name", "v")
            .build()
            .unwrap_err();
        assert!(matches!(err, Error::InvalidHeader { name } if name == "bad name"));

        let err = Client::builder("https://host")
            .bearer_token("a\nb")
            .build()
            .unwrap_err();
        assert!(matches!(err, Error::InvalidHeader { name } if name == "authorization"));
    }

    #[test]
    fn sends_the_bearer_token_only_to_the_base_origin() {
        let client = Client::builder("https://host/api")
            .bearer_token("secret")
            .build()
            .unwrap();
        let has_auth = |path: &str| {
            let parts = client.get(path).into_parts().unwrap();
            client
                .headers_for(&parts, &parts.url, None)
                .contains_key(header::AUTHORIZATION)
        };
        assert!(has_auth("/v1/apps"));
        assert!(has_auth("https://host/other"));
        assert!(has_auth("https://host:443/other"));
        assert!(!has_auth("http://host/other"));
        assert!(!has_auth("https://host:8443/other"));
        assert!(!has_auth("https://evil.example/other"));
    }

    #[test]
    fn sends_default_headers_only_to_the_base_origin() {
        let client = Client::builder("https://host/api")
            .header("x-api-key", "secret")
            .build()
            .unwrap();
        let headers = |path: &str| {
            let parts = client.get(path).into_parts().unwrap();
            client.headers_for(&parts, &parts.url, None)
        };
        let same = headers("/v1/apps");
        assert!(same.contains_key("x-api-key"));
        assert!(same.contains_key(header::USER_AGENT));
        let other = headers("https://evil.example/other");
        assert!(!other.contains_key("x-api-key"));
        assert!(other.contains_key(header::USER_AGENT));
    }

    #[test]
    fn debug_output_hides_the_bearer_token() {
        let builder = Client::builder("https://host").bearer_token("secret");
        assert!(!format!("{builder:?}").contains("secret"));
        assert!(!format!("{:?}", builder.build().unwrap()).contains("secret"));
    }

    /// A client for `http://api.invalid` that sends every request through
    /// `proxy`. `.invalid` names never resolve (RFC 6761), so a request
    /// only succeeds through the proxy.
    fn proxied(proxy: &MockServer) -> ClientBuilder {
        Client::builder("http://api.invalid")
            .bearer_token("secret")
            .retry(RetryPolicy::none())
            .proxy(proxy.url())
    }

    #[tokio::test]
    async fn sends_requests_through_the_proxy() {
        let proxy = MockServer::start(vec![MockResponse::new(200).body("ok")]).await;
        let response = proxied(&proxy)
            .build()
            .unwrap()
            .get("/v1/apps")
            .send()
            .await;
        assert_eq!(response.unwrap().bytes(), b"ok");

        let request = &proxy.requests()[0];
        assert_eq!(request.target, "http://api.invalid/v1/apps");
        assert_eq!(request.header("authorization"), Some("Bearer secret"));
    }

    #[tokio::test]
    async fn keeps_the_bearer_token_off_presigned_urls_through_the_proxy() {
        let proxy = MockServer::start(vec![MockResponse::new(200)]).await;
        proxied(&proxy)
            .build()
            .unwrap()
            .put("http://storage.invalid/upload?signature=abc")
            .body("file contents")
            .send()
            .await
            .unwrap();

        let request = &proxy.requests()[0];
        assert_eq!(
            request.target,
            "http://storage.invalid/upload?signature=abc"
        );
        assert_eq!(request.header("authorization"), None);
        assert_eq!(request.body, b"file contents");
    }

    #[tokio::test]
    async fn keeps_the_query_out_of_connect_errors() {
        // Nothing listens on the port once the listener is dropped, so the
        // connection is refused.
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let err = Client::builder("https://host")
            .retry(RetryPolicy::none())
            .build()
            .unwrap()
            .put(format!("http://127.0.0.1:{port}/upload?signature=secret"))
            .body("file contents")
            .send()
            .await
            .unwrap_err();

        assert!(matches!(err, Error::Connect { .. }), "{err:?}");
        assert_eq!(
            err.to_string(),
            format!("could not connect to http://127.0.0.1:{port}/upload?<redacted>")
        );
        assert!(!format!("{err:?}").contains("secret"), "{err:?}");
    }

    #[tokio::test]
    async fn keeps_the_query_out_of_status_errors() {
        let server = MockServer::start(vec![MockResponse::new(403)]).await;
        let err = client(server.url())
            .get("/upload?signature=secret")
            .send()
            .await
            .unwrap_err();

        assert!(matches!(err, Error::Status { status: 403, .. }), "{err:?}");
        assert!(!format!("{err} {err:?}").contains("secret"), "{err:?}");
    }

    #[tokio::test]
    async fn limits_the_proxy_tunnel_by_the_connect_limit() {
        let proxy =
            MockServer::start(vec![MockResponse::new(200).delay(Duration::from_secs(5))]).await;
        let connect_timeout = Duration::from_millis(500);
        let err = proxied(&proxy)
            .connect_timeout(connect_timeout)
            .build()
            .unwrap()
            .get("https://api.invalid/v1/apps")
            .send()
            .await
            .unwrap_err();

        let Error::Connect { source, .. } = &err else {
            panic!("unexpected error: {err}");
        };
        let source = source.downcast_ref::<reqwest::Error>().unwrap();
        assert!(source.is_timeout(), "{source}");
        let request = &proxy.requests()[0];
        assert_eq!(request.method, "CONNECT");
        assert_eq!(request.target, "api.invalid:443");
    }

    #[tokio::test]
    async fn retries_posts_that_run_out_of_the_connect_limit() {
        let slow = || MockResponse::new(200).delay(Duration::from_secs(5));
        let proxy = MockServer::start(vec![slow(), slow(), slow()]).await;
        let retry = RetryPolicy::default()
            .backoff_factor(Duration::ZERO)
            .jitter(Duration::ZERO);
        let err = proxied(&proxy)
            .connect_timeout(Duration::from_millis(500))
            .retry(retry)
            .build()
            .unwrap()
            .post("https://api.invalid/v1/runs")
            .body("{}")
            .send()
            .await
            .unwrap_err();

        assert!(matches!(err, Error::Connect { .. }), "{err}");
        let methods: Vec<String> = proxy.requests().into_iter().map(|r| r.method).collect();
        assert_eq!(methods, ["CONNECT", "CONNECT", "CONNECT"]);
    }
}
