//! Tests of the MCP server over its transports: stdio over in-memory pipes,
//! and streamable HTTP on a listener bound to a free port.

use std::collections::BTreeSet;
use std::io;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::json;
use tokio::io::{
    AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines, ReadHalf, WriteHalf,
};
use tokio::task::JoinHandle;

use super::*;
use crate::Client as HttpClient;
use crate::RetryPolicy;
use crate::decision::local::Local;
use crate::decision::platform::Platform;

/// The protocol version the test clients speak.
const PROTOCOL_VERSION: &str = "2025-06-18";

/// How long the test clients wait for a response, so that a server that
/// stops answering fails its test instead of hanging it.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);

/// A context with the platform, at an address where nothing listens, and
/// local mode, in a home directory that does not exist. The platform's
/// client does not retry, so a call that reaches it fails at once.
pub(crate) fn full_context() -> Context {
    let client = HttpClient::builder("http://127.0.0.1:9")
        .retry(RetryPolicy::none())
        .build()
        .unwrap();
    Context::new()
        .with_platform(Platform::new(client))
        .with_local(Local::with_home("/nonexistent/fico-sdk-tests"))
}

/// A provider that always returns [`full_context`].
pub(crate) async fn provider(_: RequestInfo) -> Result<Context> {
    Ok(full_context())
}

/// An MCP client of a test server, over stdio or HTTP. Dropping it stops
/// the server.
pub(crate) enum McpClient {
    Stdio {
        lines: Lines<BufReader<ReadHalf<DuplexStream>>>,
        writer: WriteHalf<DuplexStream>,
        next_id: u64,
        server: JoinHandle<Result<()>>,
    },
    Http {
        client: reqwest::Client,
        url: String,
        next_id: u64,
        server: JoinHandle<Infallible>,
    },
}

impl McpClient {
    /// A client of the server that `serve` runs on the other end of
    /// in-memory pipes, after it initialized a session.
    async fn over_pipes<S, F>(serve: S) -> McpClient
    where
        S: FnOnce(ReadHalf<DuplexStream>, WriteHalf<DuplexStream>) -> F,
        F: Future<Output = Result<()>> + Send + 'static,
    {
        let (client, server_end) = tokio::io::duplex(1 << 20);
        let (server_reader, server_writer) = tokio::io::split(server_end);
        let server = tokio::spawn(serve(server_reader, server_writer));
        let (reader, writer) = tokio::io::split(client);
        let mut client = McpClient::Stdio {
            lines: BufReader::new(reader).lines(),
            writer,
            next_id: 0,
            server,
        };
        client.initialize().await;
        client
    }

    /// A client of the server that `serve` runs on a listener bound to a
    /// free port, after it initialized.
    async fn over_http<S, F>(serve: S) -> McpClient
    where
        S: FnOnce(TcpListener) -> F,
        F: Future<Output = Infallible> + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let server = tokio::spawn(serve(listener));
        let mut client = McpClient::Http {
            client: http_client(),
            url,
            next_id: 0,
            server,
        };
        client.initialize().await;
        client
    }

    /// A client of the test `server`, over stdio.
    pub(crate) async fn stdio<P, Fut>(server: Server<P>) -> McpClient
    where
        P: Fn(RequestInfo) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Context>> + Send + 'static,
    {
        McpClient::over_pipes(|reader, writer| serve_io(server, reader, writer)).await
    }

    /// A client of the test `server`, over HTTP with `options`.
    pub(crate) async fn http<P, Fut>(server: Server<P>, options: HttpOptions) -> McpClient
    where
        P: Fn(RequestInfo) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Context>> + Send + 'static,
    {
        let serve = |listener| async move { serve_listener(server, listener, &options).await };
        McpClient::over_http(serve).await
    }

    async fn initialize(&mut self) -> Value {
        let params = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": "fico-sdk-tests", "version": "0" },
        });
        let response = self.request("initialize", params).await;
        assert_eq!(
            response["result"]["serverInfo"]["name"], "fico",
            "{response}"
        );
        self.notify("notifications/initialized").await;
        response
    }

    /// Sends a request and returns the whole response: its `result` or its
    /// `error`.
    pub(crate) async fn request(&mut self, method: &str, params: Value) -> Value {
        match self {
            McpClient::Stdio {
                lines,
                writer,
                next_id,
                ..
            } => {
                *next_id += 1;
                let id = *next_id;
                let message =
                    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
                let line = format!("{message}\n");
                writer.write_all(line.as_bytes()).await.unwrap();
                loop {
                    let line = tokio::time::timeout(RESPONSE_TIMEOUT, lines.next_line())
                        .await
                        .unwrap_or_else(|_| panic!("no response to `{method}` in time"))
                        .unwrap();
                    let response: Value =
                        serde_json::from_str(&line.expect("no response")).unwrap();
                    if response["id"] == id {
                        return response;
                    }
                }
            }
            McpClient::Http {
                client,
                url,
                next_id,
                ..
            } => {
                *next_id += 1;
                let message =
                    json!({ "jsonrpc": "2.0", "id": *next_id, "method": method, "params": params });
                let response = post(client, url, &message, &[]).await;
                assert_eq!(response.status(), 200);
                serde_json::from_slice(&response.bytes().await.unwrap()).unwrap()
            }
        }
    }

    async fn notify(&mut self, method: &str) {
        let message = json!({ "jsonrpc": "2.0", "method": method });
        match self {
            McpClient::Stdio { writer, .. } => {
                let line = format!("{message}\n");
                writer.write_all(line.as_bytes()).await.unwrap();
            }
            McpClient::Http { client, url, .. } => {
                post(client, url, &message, &[]).await;
            }
        }
    }

    /// The advertised tools.
    pub(crate) async fn tools(&mut self) -> Vec<Value> {
        let response = self.request("tools/list", json!({})).await;
        response["result"]["tools"].as_array().unwrap().clone()
    }

    /// The server's instructions.
    pub(crate) async fn instructions(&mut self) -> String {
        let response = self.initialize().await;
        response["result"]["instructions"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    /// Calls the tool `name` with `args` and returns its result.
    pub(crate) async fn call(&mut self, name: &str, args: Value) -> Value {
        let params = json!({ "name": name, "arguments": args });
        let response = self.request("tools/call", params).await;
        assert!(response.get("error").is_none(), "{response}");
        response["result"].clone()
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        match self {
            McpClient::Stdio { server, .. } => server.abort(),
            McpClient::Http { server, .. } => server.abort(),
        }
    }
}

/// An HTTP client that waits at most [`RESPONSE_TIMEOUT`] for a response.
fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(RESPONSE_TIMEOUT)
        .build()
        .unwrap()
}

/// Posts a JSON-RPC message the way MCP clients do, with `headers` added.
pub(crate) async fn post(
    client: &reqwest::Client,
    url: &str,
    message: &Value,
    headers: &[(&str, &str)],
) -> reqwest::Response {
    let mut request = client
        .post(url)
        .header("accept", "application/json, text/event-stream")
        .header("content-type", "application/json")
        .header("mcp-protocol-version", PROTOCOL_VERSION)
        .body(message.to_string());
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    request.send().await.unwrap()
}

/// The text of a tool result's first content block.
pub(crate) fn text(result: &Value) -> &str {
    result["content"][0]["text"].as_str().unwrap_or_default()
}

/// Asserts that `result` is an error whose text contains `expected`.
#[track_caller]
pub(crate) fn assert_error(result: &Value, expected: &str) {
    assert_eq!(result["isError"], true, "{result}");
    assert!(
        text(result).contains(expected),
        "{expected:?} not in {result}"
    );
}

/// Every combination of surface options.
pub(crate) fn every_surface() -> Vec<SurfaceOptions> {
    let mut surfaces = Vec::new();
    for mode in [Mode::Default, Mode::Minimal, Mode::Granular] {
        for read_only in [false, true] {
            for admin in [false, true] {
                let options = SurfaceOptions::default()
                    .mode(mode)
                    .read_only(read_only)
                    .admin(admin);
                surfaces.push(options);
            }
        }
    }
    surfaces
}

/// A client of the server that `serve_http` (`remote`) or `serve_stdio`
/// serves with `options`, after they bind their address or take stdin and
/// stdout.
pub(crate) async fn client(options: &SurfaceOptions, remote: bool) -> McpClient {
    let options = options.clone();
    match remote {
        true => {
            let options = HttpOptions::default().surface(options);
            let serve = |listener| async move { serve_remote(provider, listener, &options).await };
            McpClient::over_http(serve).await
        }
        false => {
            let serve = |reader, writer| async move {
                serve_local(provider, &options, reader, writer).await
            };
            McpClient::over_pipes(serve).await
        }
    }
}

pub(crate) fn names(tools: &[Value]) -> BTreeSet<String> {
    tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_owned())
        .collect()
}

/// How many tools the surface of `options` has, in remote mode or not,
/// before read-only drops any.
fn expected_count(options: &SurfaceOptions, remote: bool) -> usize {
    match (options.mode, remote, options.admin) {
        (Mode::Default, false, _) => 20,
        (Mode::Default, true, _) => 15,
        (Mode::Minimal, _, _) => 4,
        (Mode::Granular, false, false) => 101,
        (Mode::Granular, false, true) => 119,
        (Mode::Granular, true, false) => 81,
        (Mode::Granular, true, true) => 99,
    }
}

#[tokio::test]
async fn lists_the_tools_of_every_surface_over_both_transports() {
    for remote in [false, true] {
        for options in every_surface() {
            let tools = client(&options, remote).await.tools().await;
            let label = format!("{options:?}, remote: {remote}");
            let unique = names(&tools);
            assert_eq!(unique.len(), tools.len(), "{label}");
            if !options.read_only {
                assert_eq!(tools.len(), expected_count(&options, remote), "{label}");
                continue;
            }
            // Read-only keeps exactly the read-only tools of the same surface.
            let full = client(&options.clone().read_only(false), remote)
                .await
                .tools()
                .await;
            let reads: Vec<Value> = full
                .into_iter()
                .filter(|tool| tool["annotations"]["readOnlyHint"] == true)
                .collect();
            assert!(!reads.is_empty(), "{label}");
            assert_eq!(unique, names(&reads), "{label}");
        }
    }
}

#[tokio::test]
async fn leaves_out_local_only_tools_over_http_and_admin_tools_unless_asked() {
    let granular = SurfaceOptions::default().mode(Mode::Granular);
    let remote = names(&client(&granular, true).await.tools().await);
    assert!(
        !remote
            .iter()
            .any(|name| name.starts_with("decision_local_"))
    );
    assert!(!remote.contains("decision_platform_workspace_push"));
    assert!(!remote.iter().any(|name| surface::is_admin(name)));
    let local = names(&client(&granular, false).await.tools().await);
    assert!(local.contains("decision_local_run_get"));
    assert!(!local.contains("decision_platform_sso_enable"));
    let admin = client(&granular.clone().admin(true), false)
        .await
        .tools()
        .await;
    assert!(names(&admin).contains("decision_platform_sso_enable"));
}

/// The number of sentences in `text`, not counting the dots in code spans
/// and in `e.g.` and `i.e.`.
pub(crate) fn sentences(text: &str) -> usize {
    let chars: Vec<char> = text.chars().collect();
    let mut count = 0;
    let mut in_code = false;
    for (i, c) in chars.iter().enumerate() {
        match c {
            '`' => in_code = !in_code,
            '.' | '?' | '!' if !in_code => {
                let ends = chars.get(i + 1).is_none_or(|next| next.is_whitespace());
                let before: String = chars[i.saturating_sub(3)..=i].iter().collect();
                if ends && before != "e.g." && before != "i.e." {
                    count += 1;
                }
            }
            _ => {}
        }
    }
    count
}

#[test]
fn counts_sentences() {
    assert_eq!(sentences("One. Two, e.g. `workspace.yaml`. Three?"), 3);
    assert_eq!(sentences("Lists `a.b` and `c`, i.e. two."), 1);
}

#[tokio::test]
async fn gives_every_tool_a_title_one_risk_level_and_a_short_description() {
    let read = json!([true, false, true]);
    let write = json!([false, false, null]);
    let destructive = json!([false, true, null]);
    for remote in [false, true] {
        for options in every_surface() {
            for tool in client(&options, remote).await.tools().await {
                let name = tool["name"].as_str().unwrap();
                let title = tool["title"].as_str().unwrap_or_default();
                assert!(!title.is_empty(), "{name}");
                let hints = &tool["annotations"];
                assert_eq!(hints["title"], title, "{name}");
                let risk = json!([
                    hints["readOnlyHint"],
                    hints["destructiveHint"],
                    hints["idempotentHint"],
                ]);
                assert!(
                    [&read, &write, &destructive].contains(&&risk),
                    "{name}: {hints}"
                );
                let description = tool["description"].as_str().unwrap();
                assert!(description.len() <= 2048, "{name}");
                // Granular mode advertises only catalog rows, which have one
                // sentence. The other modes advertise only tools written for
                // MCP, which say what they do, when to use them and what
                // instead.
                let least = match options.mode {
                    Mode::Granular => 1,
                    _ => 3,
                };
                assert!(sentences(description) >= least, "{name}: {description}");
            }
        }
    }
}

/// The tool names in the code spans of `text`.
fn named_tools(text: &str) -> impl Iterator<Item = &str> {
    let spans = text.split('`').skip(1).step_by(2);
    spans.filter(|span| span.starts_with("decision_") || span.starts_with("fico_"))
}

/// A model calls the tools that the instructions and the descriptions name,
/// so they only name tools that the surface has.
#[tokio::test]
async fn names_only_advertised_tools_in_instructions_and_descriptions() {
    for remote in [false, true] {
        for options in every_surface() {
            let mut client = client(&options, remote).await;
            let instructions = client.instructions().await;
            let label = format!("{options:?}, remote: {remote}");
            assert!(instructions.len() <= 2048, "{label}: {instructions}");
            let tools = client.tools().await;
            let advertised = names(&tools);
            for tool in named_tools(&instructions) {
                assert!(
                    advertised.contains(tool),
                    "{label}: the instructions name `{tool}`"
                );
            }
            for definition in &tools {
                let name = &definition["name"];
                let description = definition["description"].as_str().unwrap();
                for tool in named_tools(description) {
                    assert!(advertised.contains(tool), "{label}: {name} names `{tool}`");
                }
            }
        }
    }
}

#[tokio::test]
async fn reads_only_over_http_what_only_writes_through_local() {
    let granular = SurfaceOptions::default().mode(Mode::Granular);
    for (remote, read_only, destructive) in [(false, false, true), (true, true, false)] {
        let tools = client(&granular, remote).await.tools().await;
        let run_get = tools
            .iter()
            .find(|tool| tool["name"] == "decision_platform_run_get")
            .unwrap();
        let hints = &run_get["annotations"];
        assert_eq!(hints["readOnlyHint"], read_only, "remote: {remote}");
        assert_eq!(hints["destructiveHint"], destructive, "remote: {remote}");
    }
}

#[tokio::test]
async fn names_the_bad_field_in_the_error() {
    let granular = SurfaceOptions::default().mode(Mode::Granular);
    for remote in [false, true] {
        let mut client = client(&granular, remote).await;
        let args = json!({ "workspace_id": "routing" });
        let result = client.call("decision_platform_run_get", args).await;
        assert_error(&result, "missing field `run_id`");
        let args = json!({ "workspace_id": "routing", "run_id": "r", "nmae": "x" });
        let result = client.call("decision_platform_run_get", args).await;
        assert_error(&result, "unknown field `nmae`");
    }
}

#[tokio::test]
async fn rejects_unknown_tools_with_a_protocol_error() {
    for remote in [false, true] {
        let mut client = client(&SurfaceOptions::default(), remote).await;
        let params = json!({ "name": "decision_nothing", "arguments": {} });
        let response = client.request("tools/call", params).await;
        assert_eq!(response["error"]["code"], -32602, "{response}");
    }
}

/// Arguments of the test tools: none.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct Nothing {}

/// A test tool that returns `value`.
fn returns(name: &'static str, value: Value) -> Tool {
    Tool::new(name, "Returns a value.", move |_cx, _: Nothing| {
        let value = value.clone();
        async move { Ok(value) }
    })
}

/// A local-mode server of the test `tools`, with `provider`.
fn test_server<P, Fut>(provider: P, tools: Vec<Tool>) -> Server<P>
where
    P: Fn(RequestInfo) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Context>> + Send + 'static,
{
    let surface = Surface {
        tools,
        instructions: String::new(),
    };
    Server::new(provider, surface, false)
}

#[tokio::test]
async fn returns_structured_results_and_wraps_the_others() {
    let tools = vec![
        returns("object", json!({ "id": "a" })),
        returns("number", json!(42)),
        returns("nothing", Value::Null),
    ];
    let mut client = McpClient::stdio(test_server(provider, tools)).await;
    let cases = [
        ("object", json!({ "id": "a" })),
        ("number", json!({ "result": 42 })),
        ("nothing", json!({ "result": null })),
    ];
    for (name, expected) in cases {
        let result = client.call(name, Value::Null).await;
        assert_eq!(result["isError"], false, "{result}");
        assert_eq!(result["structuredContent"], expected);
        let content: Value = serde_json::from_str(text(&result)).unwrap();
        assert_eq!(content, expected);
    }
}

#[tokio::test]
async fn reports_failures_with_their_causes() {
    let fails = Tool::new("fails", "Fails.", |_cx, _: Nothing| async {
        Err::<(), _>(Error::Io {
            path: "workspace.yaml".into(),
            source: io::Error::other("disk full"),
        })
    });
    let panics = Tool::new("panics", "Panics.", |_cx, _: Nothing| async {
        if true {
            panic!("boom");
        }
        Ok(())
    });
    let mut client = McpClient::stdio(test_server(provider, vec![fails, panics])).await;
    let result = client.call("fails", Value::Null).await;
    assert_error(&result, "could not access `workspace.yaml`: disk full");
    let result = client.call("panics", Value::Null).await;
    assert_error(&result, "the tool panicked: boom");
}

#[tokio::test]
async fn fails_the_call_when_the_provider_fails() {
    let failing = |_: RequestInfo| async {
        Err(Error::Invalid {
            reason: "no token".to_owned(),
        })
    };
    let tools = vec![returns("object", json!({}))];
    let mut client = McpClient::stdio(test_server(failing, tools)).await;
    let result = client.call("object", Value::Null).await;
    assert_error(&result, "invalid argument: no token");
    let panicking = |_: RequestInfo| async {
        if true {
            panic!("provider bug");
        }
        Ok(Context::new())
    };
    let tools = vec![returns("object", json!({}))];
    let mut client = McpClient::stdio(test_server(panicking, tools)).await;
    let result = client.call("object", Value::Null).await;
    assert_error(&result, "the context provider panicked");
}

#[tokio::test]
async fn passes_the_authorization_header_to_the_provider_over_http() {
    for remote in [false, true] {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        let recording = move |info: RequestInfo| {
            record.lock().unwrap().push(info.authorization);
            async { Ok(Context::new()) }
        };
        let server = Server {
            remote,
            ..test_server(recording, vec![returns("object", json!({}))])
        };
        let message = json!({
            "jsonrpc": "2.0", "id": 9, "method": "tools/call",
            "params": { "name": "object", "arguments": {} },
        });
        let headers = [("authorization", "Bearer abc")];
        match remote {
            true => {
                let client = McpClient::http(server, HttpOptions::default()).await;
                let McpClient::Http {
                    client: http, url, ..
                } = &client
                else {
                    unreachable!()
                };
                let response = post(http, url, &message, &headers).await;
                assert_eq!(response.status(), 200);
            }
            false => {
                McpClient::stdio(server)
                    .await
                    .call("object", Value::Null)
                    .await;
            }
        }
        let expected = remote.then(|| "Bearer abc".to_owned());
        assert_eq!(*seen.lock().unwrap(), vec![expected], "remote: {remote}");
    }
}

#[tokio::test]
async fn drops_local_mode_over_http() {
    let granular = SurfaceOptions::default().mode(Mode::Granular);
    let args = json!({ "workspace_id": "routing", "run_id": "r", "output_dir": "/srv/out" });
    let mut remote = client(&granular, true).await;
    let result = remote.call("decision_platform_run_get", args.clone()).await;
    assert_error(&result, "only available in local mode");
    // Over stdio, the same context keeps local mode.
    let mut local = client(&granular, false).await;
    let result = local.call("decision_platform_run_get", args).await;
    assert!(
        !text(&result).contains("only available in local mode"),
        "{result}"
    );
}

#[tokio::test]
async fn rejects_hosts_and_origins_that_are_not_allowed() {
    let message = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {} });
    let defaults = HttpOptions::default();
    let cases = [
        (defaults.clone(), Some("https://evil.example"), 403),
        (defaults.clone(), None, 200),
        (
            defaults
                .clone()
                .allowed_origins(["https://app.example.com:443"]),
            Some("https://app.example.com"),
            200,
        ),
        (
            defaults.clone().allowed_hosts(["mcp.example.com"]),
            None,
            403,
        ),
        (defaults.allowed_hosts(Vec::<String>::new()), None, 200),
    ];
    for (options, origin, status) in cases {
        let label = format!("{options:?}, origin: {origin:?}");
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { serve_remote(provider, listener, &options).await });
        let headers: Vec<(&str, &str)> = origin
            .map(|origin| ("origin", origin))
            .into_iter()
            .collect();
        let response = post(&http_client(), &url, &message, &headers).await;
        assert_eq!(response.status(), status, "{label}");
        task.abort();
    }
}

#[tokio::test]
async fn fails_to_serve_http_on_an_address_in_use() {
    let taken = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = taken.local_addr().unwrap();
    let error = serve_http(provider, addr, HttpOptions::default())
        .await
        .unwrap_err();
    assert!(matches!(error, Error::Serve { .. }), "{error}");
    let expected = format!("MCP server failed: could not listen on {addr}");
    assert_eq!(error.to_string(), expected);
    assert!(error.source().is_some());
}

#[tokio::test]
async fn stops_serving_stdio_when_the_client_closes_it() {
    let options = SurfaceOptions::default();
    // Before the session starts.
    let (client_end, server_end) = tokio::io::duplex(1024);
    drop(client_end);
    let (reader, writer) = tokio::io::split(server_end);
    serve_local(provider, &options, reader, writer)
        .await
        .unwrap();
    // After it started.
    let mut client = client(&options, false).await;
    let McpClient::Stdio { writer, server, .. } = &mut client else {
        unreachable!()
    };
    writer.shutdown().await.unwrap();
    let stopped = tokio::time::timeout(Duration::from_secs(5), server).await;
    stopped.unwrap().unwrap().unwrap();
}

#[test]
fn debug_output_hides_the_authorization_header() {
    let info = RequestInfo {
        authorization: Some("Bearer secret".to_owned()),
    };
    let debug = format!("{info:?}");
    assert!(!debug.contains("secret"), "{debug}");
    assert_eq!(
        debug,
        r#"RequestInfo { authorization: Some("<redacted>") }"#
    );
    let none = format!("{:?}", RequestInfo::default());
    assert_eq!(none, "RequestInfo { authorization: None }");
}

#[tokio::test]
async fn finds_and_calls_operations_through_the_operation_tools() {
    let minimal = SurfaceOptions::default().mode(Mode::Minimal);
    for remote in [false, true] {
        let mut client = client(&minimal, remote).await;
        let args = json!({ "query": "local run list", "limit": 20 });
        let result = client.call("fico_operation_search", args).await;
        assert_eq!(result["isError"], false, "{result}");
        let found = result["structuredContent"]["operations"]
            .as_array()
            .unwrap();
        let local = found
            .iter()
            .any(|operation| operation["name"] == "decision_local_run_list");
        assert_eq!(local, !remote, "remote: {remote}");
        let args = json!({
            "name": "decision_local_run_list",
            "arguments": { "src": "/srv/workspace" },
        });
        let result = client.call("fico_operation_read", args).await;
        match remote {
            true => assert_error(&result, "only available in local mode"),
            false => assert!(!text(&result).contains("local mode"), "{result}"),
        }
    }
}

/// Most characters that the definitions of the default surface may take in
/// a `tools/list` result: the first measurement, 25,148, plus 10%.
const DEFAULT_SURFACE_BUDGET: usize = 27_700;

#[tokio::test]
async fn keeps_the_default_surface_within_its_budget() {
    let tools = client(&SurfaceOptions::default(), false)
        .await
        .tools()
        .await;
    assert!(tools.len() <= 20, "{}", tools.len());
    let size = Value::Array(tools).to_string().len();
    assert!(size <= DEFAULT_SURFACE_BUDGET, "{size} characters");
}

#[tokio::test]
async fn leaves_operations_to_the_curated_tools_of_the_same_name() {
    let mut client = client(&SurfaceOptions::default(), false).await;
    let args = json!({ "query": "init manifest local run list sync", "limit": 20 });
    let result = client.call("fico_operation_search", args).await;
    let found = names(
        result["structuredContent"]["operations"]
            .as_array()
            .unwrap(),
    );
    let advertised = names(&client.tools().await);
    assert!(!found.is_empty());
    assert!(found.is_disjoint(&advertised), "{found:?}");
    let args = json!({ "name": "decision_manifest_validate", "arguments": {} });
    let result = client.call("fico_operation_read", args).await;
    assert_error(&result, "call the `decision_manifest_validate` tool");
}

#[tokio::test]
async fn stops_the_provider_when_the_call_is_cancelled() {
    /// Sets its flag when dropped.
    struct SetOnDrop(Arc<AtomicBool>);

    impl Drop for SetOnDrop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    /// Waits until `flag` is set, for at most 5 seconds.
    async fn until(flag: &AtomicBool) -> bool {
        for _ in 0..500 {
            if flag.load(Ordering::SeqCst) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        false
    }

    let started = Arc::new(AtomicBool::new(false));
    let dropped = Arc::new(AtomicBool::new(false));
    let (start, stop) = (Arc::clone(&started), Arc::clone(&dropped));
    let hanging = move |_: RequestInfo| {
        let (start, guard) = (Arc::clone(&start), SetOnDrop(Arc::clone(&stop)));
        async move {
            let _guard = guard;
            start.store(true, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_secs(60)).await;
            Ok(Context::new())
        }
    };
    let tools = vec![returns("object", json!({}))];
    let mut client = McpClient::stdio(test_server(hanging, tools)).await;
    let McpClient::Stdio { writer, .. } = &mut client else {
        unreachable!()
    };
    let call = json!({
        "jsonrpc": "2.0", "id": 7, "method": "tools/call",
        "params": { "name": "object", "arguments": {} },
    });
    writer
        .write_all(format!("{call}\n").as_bytes())
        .await
        .unwrap();
    assert!(until(&started).await, "the provider did not start");
    let cancel = json!({
        "jsonrpc": "2.0", "method": "notifications/cancelled",
        "params": { "requestId": 7 },
    });
    writer
        .write_all(format!("{cancel}\n").as_bytes())
        .await
        .unwrap();
    assert!(
        until(&dropped).await,
        "the provider kept running after the call was cancelled"
    );
}
