//! MCP server over the SDK's tools.
//!
//! [`serve_stdio`] serves local mode over stdin and stdout, and
//! [`serve_http`] serves remote mode over streamable HTTP, without the
//! local-only tools. [`SurfaceOptions`] pick the tools they advertise when
//! they start; see [`Mode`].
//!
//! Both build the [`Context`] of every call with `provider`, which allows
//! token refresh and, in remote mode, per-request auth. `provider` is async,
//! so it can await a token refresh, and takes the [`RequestInfo`] by value,
//! so its future can keep it.
//!
//! Tool errors become `isError` results that carry the error's full
//! `source()` chain. Results are structured JSON, and a result that is not
//! an object is wrapped as `{"result": …}`. No rmcp type is part of this
//! API.
//!
//! In remote mode, the curated tools link to content too large to return
//! inline, such as a run's output, as resources:
//! `fico://decision/workspaces/{workspace_id}/runs/{run_id}/{output|logs|input}`.
//! `resources/read` serves them with the context that `provider` builds for
//! the read, so a caller reads only what its own credentials reach. Local
//! mode writes such content to files in the FICO home instead, and serves no
//! resources of its own.

mod cache;
mod decision;
mod operations;
mod surface;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::error::Error as StdError;
use std::fmt;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use hyper::server::conn::http1;
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::service::TowerToHyperService;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Extensions,
    Implementation, ListResourceTemplatesResult, ListToolsResult, PaginatedRequestParams,
    ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult, Resource,
    ResourceContents, ResourceTemplate, ServerCapabilities, ServerConfig,
};
use rmcp::service::{RequestContext, RoleServer, ServerInitializeError};
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ErrorData, ServerHandler, ServiceExt};
use serde_json::{Map, Value};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
use tokio::task::JoinSet;

use self::surface::Surface;
use crate::tool::{AbortOnDrop, Context, Tool};
use crate::{Error, Result};

/// How long the HTTP server waits before it accepts connections again after
/// accepting one failed, e.g. because the process ran out of file
/// descriptors.
const ACCEPT_RETRY_DELAY: Duration = Duration::from_millis(100);

/// What the server knows about the request behind a tool call, for
/// `provider` to build its [`Context`] from.
///
/// Its `Debug` output hides the `Authorization` header.
#[derive(Clone, Default)]
#[non_exhaustive]
pub struct RequestInfo {
    /// The request's `Authorization` header, in remote mode. Always `None`
    /// in local mode.
    pub authorization: Option<String>,
}

impl RequestInfo {
    /// Request information with this `Authorization` header, as remote mode
    /// gives a provider, e.g. to test one.
    pub fn with_authorization(authorization: impl Into<String>) -> RequestInfo {
        RequestInfo {
            authorization: Some(authorization.into()),
        }
    }
}

impl fmt::Debug for RequestInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let authorization = self.authorization.as_ref().map(|_| "<redacted>");
        f.debug_struct("RequestInfo")
            .field("authorization", &authorization)
            .finish()
    }
}

/// Which tools the server advertises.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Mode {
    /// The curated workflow tools of each capability, e.g. `decision_run_create`,
    /// which starts a platform run and waits for it, plus the four
    /// `fico_operation_*` tools for everything else. 20 tools in local mode
    /// and 15 in remote mode.
    #[default]
    Default,
    /// Only the four `fico_operation_*` tools: `fico_operation_search`
    /// finds operations by keyword, and `fico_operation_read`,
    /// `fico_operation_write` and `fico_operation_destructive` call them,
    /// one tool per risk level.
    Minimal,
    /// Every operation as its own tool, e.g. `decision_platform_run_get`,
    /// for clients that search their tools.
    Granular,
}

/// The tools a server advertises, fixed when it starts: its [`Mode`],
/// whether it is read-only, and whether it serves account administration.
///
/// # Examples
///
/// ```
/// use fico_sdk::mcp::{Mode, SurfaceOptions};
///
/// let surface = SurfaceOptions::default().mode(Mode::Granular).read_only(true);
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SurfaceOptions {
    mode: Mode,
    read_only: bool,
    admin: bool,
}

impl SurfaceOptions {
    /// Sets the mode. Defaults to [`Mode::default`].
    pub fn mode(mut self, mode: Mode) -> Self {
        self.mode = mode;
        self
    }

    /// Whether to advertise only the tools that read, leaving out every
    /// tool that adds, changes or deletes data. Defaults to `false`.
    pub fn read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }

    /// Whether to serve the account, SSO and integration operations, which
    /// administer a whole account. Defaults to `false`.
    pub fn admin(mut self, admin: bool) -> Self {
        self.admin = admin;
        self
    }
}

/// Serves the tools over stdin and stdout, until stdin closes (local mode).
///
/// stdout carries the protocol, so nothing else in the process may write to
/// it. `provider` builds the context of every call; if it fails, the call
/// fails with its error.
///
/// # Errors
///
/// Returns an error if the transport fails.
///
/// # Examples
///
/// ```no_run
/// # async fn run() -> fico_sdk::Result<()> {
/// use fico_sdk::decision::local::Local;
/// use fico_sdk::mcp::{RequestInfo, SurfaceOptions, serve_stdio};
/// use fico_sdk::tool::Context;
///
/// let local = Local::new()?;
/// let provider = move |_: RequestInfo| {
///     let local = local.clone();
///     async move { Ok(Context::new().with_local(local)) }
/// };
/// serve_stdio(provider, SurfaceOptions::default()).await
/// # }
/// ```
pub async fn serve_stdio<P, Fut>(provider: P, surface: SurfaceOptions) -> Result<()>
where
    P: Fn(RequestInfo) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Context>> + Send + 'static,
{
    let (stdin, stdout) = rmcp::transport::stdio();
    serve_local(provider, &surface, stdin, stdout).await
}

/// Settings of [`serve_http`].
///
/// # Examples
///
/// ```
/// use fico_sdk::mcp::{HttpOptions, SurfaceOptions};
///
/// let options = HttpOptions::default()
///     .allowed_hosts(["mcp.example.com"])
///     .allowed_origins(["https://app.example.com:443"])
///     .surface(SurfaceOptions::default().read_only(true));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpOptions {
    allowed_hosts: Vec<String>,
    allowed_origins: Vec<String>,
    surface: SurfaceOptions,
}

impl Default for HttpOptions {
    /// Accepts the loopback hosts and no browser origin, and advertises the
    /// default surface.
    fn default() -> Self {
        // `serve_http` checks `Origin` even when the list is empty (rmcp's
        // `enforce_origin_validation`), as the MCP transport spec requires.
        HttpOptions {
            allowed_hosts: vec!["localhost".into(), "127.0.0.1".into(), "::1".into()],
            allowed_origins: Vec::new(),
            surface: SurfaceOptions::default(),
        }
    }
}

impl HttpOptions {
    /// Sets the hostnames, or `host:port` authorities, that a request's
    /// `Host` header may name. Requests for other hosts get
    /// `403 Forbidden`. An empty list turns the check off and accepts every
    /// host, which leaves a server on this machine open to DNS rebinding.
    ///
    /// Defaults to `localhost`, `127.0.0.1` and `::1`, which protects a
    /// server on this machine from DNS rebinding, so a public deployment
    /// sets its own hostnames.
    pub fn allowed_hosts(mut self, hosts: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.allowed_hosts = hosts.into_iter().map(Into::into).collect();
        self
    }

    /// Sets the browser origins that a request's `Origin` header may name,
    /// each with its scheme and port, e.g. `https://app.example.com:443`
    /// (`:*` matches any port). Requests from other origins get
    /// `403 Forbidden`. Requests without an `Origin` header, such as those
    /// of non-browser clients, always pass.
    ///
    /// Defaults to none, which rejects every request that has an `Origin`
    /// header.
    pub fn allowed_origins(mut self, origins: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.allowed_origins = origins.into_iter().map(Into::into).collect();
        self
    }

    /// Sets the tools the server advertises. Defaults to
    /// [`SurfaceOptions::default`].
    pub fn surface(mut self, surface: SurfaceOptions) -> Self {
        self.surface = surface;
        self
    }
}

/// Serves the tools over streamable HTTP on `addr` (remote mode), without
/// the local-only tools, until the returned future is dropped.
///
/// `provider` builds the context of every call from its request, e.g. from
/// its bearer token; if it fails, the call fails with its error. The server
/// drops any [`Local`](crate::decision::local::Local) from the contexts it
/// returns, so that no tool reads or writes the server's files. So it
/// advertises the tools that only write through `Local` as read-only (see
/// [`Tool::is_read_only_in_remote_mode`](crate::tool::Tool::is_read_only_in_remote_mode)).
///
/// `provider` must check that the request's token was issued for this
/// server, and must not send that token on to the platform: MCP forbids
/// [token passthrough]. It calls the platform with a token of its own
/// instead, e.g. one it gets for the user in exchange for the request's.
///
/// [token passthrough]: https://modelcontextprotocol.io/specification/2025-06-18/basic/security_best_practices#token-passthrough
///
/// # Errors
///
/// Returns an error if `addr` cannot be bound.
///
/// # Examples
///
/// ```no_run
/// # async fn run() -> fico_sdk::Result<()> {
/// use fico_sdk::Client;
/// use fico_sdk::decision::platform::Platform;
/// use fico_sdk::mcp::{HttpOptions, RequestInfo, serve_http};
/// use fico_sdk::tool::Context;
///
/// /// Checks that `authorization` holds a token issued for this server, and
/// /// returns a client that calls the platform with a platform token for the
/// /// same user, never with the request's token.
/// async fn platform_client(authorization: Option<String>) -> fico_sdk::Result<Client> {
///     // ...
/// #   let _ = authorization;
/// #   Client::new("https://api.example.com")
/// }
///
/// let addr = "127.0.0.1:8080".parse().unwrap();
/// let provider = |info: RequestInfo| async move {
///     let client = platform_client(info.authorization).await?;
///     Ok(Context::new().with_platform(Platform::new(client)))
/// };
/// serve_http(provider, addr, HttpOptions::default()).await
/// # }
/// ```
pub async fn serve_http<P, Fut>(provider: P, addr: SocketAddr, options: HttpOptions) -> Result<()>
where
    P: Fn(RequestInfo) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Context>> + Send + 'static,
{
    let listener = TcpListener::bind(addr).await.map_err(|e| Error::Serve {
        reason: format!("could not listen on {addr}"),
        source: Some(Box::new(e)),
    })?;
    match serve_remote(provider, listener, &options).await {}
}

/// Serves the tools that `surface` selects over `reader` and `writer`, in
/// local mode, until `reader` closes: [`serve_stdio`] after it takes stdin
/// and stdout.
async fn serve_local<P, Fut, R, W>(
    provider: P,
    surface: &SurfaceOptions,
    reader: R,
    writer: W,
) -> Result<()>
where
    P: Fn(RequestInfo) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Context>> + Send + 'static,
    R: AsyncRead + Send + Unpin + 'static,
    W: AsyncWrite + Send + Unpin + 'static,
{
    let server = Server::new(provider, Surface::new(surface, false), false);
    serve_io(server, reader, writer).await
}

/// Serves the tools over streamable HTTP on `listener`, in remote mode,
/// until the returned future is dropped: [`serve_http`] after it binds its
/// address.
async fn serve_remote<P, Fut>(
    provider: P,
    listener: TcpListener,
    options: &HttpOptions,
) -> Infallible
where
    P: Fn(RequestInfo) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Context>> + Send + 'static,
{
    let server = Server::new(provider, Surface::new(&options.surface, true), true);
    serve_listener(server, listener, options).await
}

/// Serves `server` over a byte stream, e.g. stdin and stdout, until
/// `reader` closes.
async fn serve_io<P, Fut, R, W>(server: Server<P>, reader: R, writer: W) -> Result<()>
where
    P: Fn(RequestInfo) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Context>> + Send + 'static,
    R: AsyncRead + Send + Unpin + 'static,
    W: AsyncWrite + Send + Unpin + 'static,
{
    let session = match server.serve((reader, writer)).await {
        Ok(session) => session,
        // The client went away before it started a session.
        Err(ServerInitializeError::ConnectionClosed(_)) => return Ok(()),
        Err(e) => {
            return Err(Error::Serve {
                reason: "could not start the session".to_owned(),
                source: Some(e.to_string().into()),
            });
        }
    };
    session.waiting().await.map(drop).map_err(|e| Error::Serve {
        reason: "the session failed".to_owned(),
        source: Some(Box::new(e)),
    })
}

/// Serves `server` over streamable HTTP on `listener`, until the returned
/// future is dropped.
async fn serve_listener<P, Fut>(
    server: Server<P>,
    listener: TcpListener,
    options: &HttpOptions,
) -> Infallible
where
    P: Fn(RequestInfo) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Context>> + Send + 'static,
{
    // Stateless: every request stands alone, so the server keeps no
    // sessions, calls `provider` with each request's own headers, and can
    // run as several instances behind a load balancer.
    let config = StreamableHttpServerConfig::default()
        .with_allowed_hosts(options.allowed_hosts.clone())
        .with_allowed_origins(options.allowed_origins.clone())
        .enforce_origin_validation()
        .with_legacy_session_mode(false)
        .with_json_response(true);
    // Stops the requests that are still running when this future is dropped.
    let _stop = config.cancellation_token.clone().drop_guard();
    let service = StreamableHttpService::new(
        move || Ok(server.clone()),
        Arc::new(NeverSessionManager::default()),
        config,
    );
    // Dropping the set, with this future, closes every connection.
    let mut connections = JoinSet::new();
    loop {
        while connections.try_join_next().is_some() {}
        let stream = match listener.accept().await {
            Ok((stream, _)) => stream,
            Err(_) => {
                tokio::time::sleep(ACCEPT_RETRY_DELAY).await;
                continue;
            }
        };
        let service = TowerToHyperService::new(service.clone());
        connections.spawn(async move {
            // A connection that fails only fails its own client.
            let _ = http1::Builder::new()
                .timer(TokioTimer::new())
                .serve_connection(TokioIo::new(stream), service)
                .await;
        });
    }
}

/// The MCP server: the advertised tools, by name, and the provider of the
/// context of every call.
struct Server<P> {
    tools: Arc<BTreeMap<&'static str, Tool>>,
    definitions: Arc<Vec<rmcp::model::Tool>>,
    info: Arc<ServerConfig>,
    provider: Arc<P>,
    /// Whether this is remote mode, which drops `Local` from every context.
    remote: bool,
}

impl<P> Clone for Server<P> {
    fn clone(&self) -> Self {
        Server {
            tools: Arc::clone(&self.tools),
            definitions: Arc::clone(&self.definitions),
            info: Arc::clone(&self.info),
            provider: Arc::clone(&self.provider),
            remote: self.remote,
        }
    }
}

impl<P, Fut> Server<P>
where
    P: Fn(RequestInfo) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Context>> + Send + 'static,
{
    fn new(provider: P, surface: Surface, remote: bool) -> Self {
        let definitions = surface
            .tools
            .iter()
            .map(|tool| surface::definition(tool, remote))
            .collect();
        let capabilities = ServerCapabilities::builder()
            .enable_tools()
            .enable_resources()
            .build();
        let mut info = ServerConfig::new(capabilities);
        info.server_info = Implementation::new("fico", crate::VERSION);
        info.server_info.title = Some("FICO".to_owned());
        info.instructions = Some(surface.instructions);
        let tools = surface.tools.into_iter().map(|tool| (tool.name(), tool));
        Server {
            tools: Arc::new(tools.collect()),
            definitions: Arc::new(definitions),
            info: Arc::new(info),
            provider: Arc::new(provider),
            remote,
        }
    }

    /// The context that `provider` builds for `info`, without local mode in
    /// remote mode. Fails with the message of the error and its causes.
    async fn context(&self, info: RequestInfo) -> Result<Context, String> {
        let provider = Arc::clone(&self.provider);
        // The provider runs in its own task, so that a panic in it fails
        // the call instead of leaving the call without an answer. Dropping
        // the call, e.g. when it is cancelled, aborts the task.
        let task = AbortOnDrop(tokio::spawn(async move { provider(info).await }));
        let cx = match task.await {
            Ok(Ok(cx)) => cx,
            Ok(Err(error)) => return Err(message(&error)),
            Err(_) => return Err("the context provider panicked".to_owned()),
        };
        Ok(match self.remote {
            true => cx.without_local(),
            false => cx,
        })
    }

    /// Calls `tool` with `args`, in the context that `provider` builds for
    /// `info`. Fails with the message of the error and its causes.
    async fn call(&self, tool: &Tool, info: RequestInfo, args: Value) -> Result<Value, String> {
        let cx = self.context(info).await?;
        tool.call(cx, args).await.map_err(|error| message(&error))
    }

    /// The content of the resource `uri`, read in the context that
    /// `provider` builds for `info`. Fails with the message of the error and
    /// its causes.
    async fn read(&self, uri: &str, info: RequestInfo) -> Result<String, String> {
        let (workspace_id, run_id, part) = cache::parse_uri(uri).map_err(|e| message(&e))?;
        let cx = self.context(info).await?;
        let platform = cx.platform().map_err(|e| message(&e))?.clone();
        // In its own task, as tool calls run, so that a panic fails the read
        // instead of the session.
        let task = AbortOnDrop(tokio::spawn(async move {
            decision::read_resource(&platform, &workspace_id, &run_id, part).await
        }));
        match task.await {
            Ok(result) => result.map_err(|error| message(&error)),
            Err(_) => Err("reading the resource panicked".to_owned()),
        }
    }
}

impl<P, Fut> ServerHandler for Server<P>
where
    P: Fn(RequestInfo) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Context>> + Send + 'static,
{
    fn get_info(&self) -> ServerConfig {
        ServerConfig::clone(&self.info)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let definitions = Vec::clone(&self.definitions);
        Ok(ListToolsResult::with_all_items(definitions))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let Some(tool) = self.tools.get(request.name.as_ref()) else {
            let message = format!("unknown tool `{}`", request.name);
            return Err(ErrorData::invalid_params(message, None));
        };
        let info = RequestInfo {
            authorization: authorization(&context.extensions),
        };
        let args = request.arguments.map_or(Value::Null, Value::Object);
        let result = tokio::select! {
            result = self.call(tool, info, args) => result,
            // Dropping the call cancels it.
            () = context.ct.cancelled() => Err("the call was cancelled".to_owned()),
        };
        let result = match result {
            Ok(Value::Object(object)) => structured(Value::Object(object)),
            Ok(value) => {
                let object = Map::from_iter([("result".to_owned(), value)]);
                structured(Value::Object(object))
            }
            Err(message) => CallToolResult::error(vec![ContentBlock::text(message)]),
        };
        Ok(result.into())
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        let templates = cache::Part::ALL
            .into_iter()
            .map(|part| {
                ResourceTemplate::new(cache::uri_template(part), format!("run {}", part.as_str()))
                    .with_title(format!("Run {}", part.as_str()))
                    .with_description(part.description())
                    .with_mime_type(part.mime_type())
            })
            .collect();
        Ok(ListResourceTemplatesResult::with_all_items(templates))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        let uri = request.uri;
        let mime_type = match cache::parse_uri(&uri) {
            Ok((_, _, part)) => part.mime_type(),
            Err(error) => return Err(ErrorData::invalid_params(message(&error), None)),
        };
        let info = RequestInfo {
            authorization: authorization(&context.extensions),
        };
        let text = tokio::select! {
            text = self.read(&uri, info) => text,
            () = context.ct.cancelled() => Err("the read was cancelled".to_owned()),
        };
        match text {
            Ok(text) => {
                let contents = ResourceContents::text(text, uri).with_mime_type(mime_type);
                Ok(ReadResourceResult::new(vec![contents]).into())
            }
            Err(message) => Err(ErrorData::resource_not_found(message, None)),
        }
    }
}

/// The result of a call that returned `value`, with a resource link block
/// for each resource that `value` links to.
pub(crate) fn structured(value: Value) -> CallToolResult {
    let links = cache::links(&value);
    let mut result = CallToolResult::structured(value);
    for (uri, name, part) in links {
        let link = Resource::new(uri, name).with_mime_type(part.mime_type());
        result.content.push(ContentBlock::resource_link(link));
    }
    result
}

/// The `Authorization` header of the HTTP request behind a call, if any.
fn authorization(extensions: &Extensions) -> Option<String> {
    let parts = extensions.get::<hyper::http::request::Parts>()?;
    let value = parts.headers.get(hyper::header::AUTHORIZATION)?;
    value.to_str().ok().map(str::to_owned)
}

/// The message of `error`, followed by those of its causes.
pub(crate) fn message(error: &dyn StdError) -> String {
    let mut message = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    message
}
