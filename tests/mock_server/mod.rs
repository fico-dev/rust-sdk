//! In-house mock HTTP/1.1 server for integration tests.
//!
//! Serves scripted responses in order, one per request, and records every
//! request it receives. Each connection handles a single request and is then
//! closed.
#![allow(dead_code, reason = "each test crate uses a subset")]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

/// A request received by the mock server.
#[derive(Debug, Clone)]
pub struct RecordedRequest {
    pub method: String,
    /// Path and query, e.g. `/v1/apps?limit=5`.
    pub target: String,
    /// Header names are lowercase.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl RecordedRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    /// Decoded query parameters, in order.
    pub fn query(&self) -> Vec<(String, String)> {
        let Some((_, query)) = self.target.split_once('?') else {
            return Vec::new();
        };
        reqwest::Url::parse(&format!("http://x/?{query}"))
            .unwrap()
            .query_pairs()
            .into_owned()
            .collect()
    }
}

/// A scripted response.
#[derive(Debug, Clone)]
pub struct MockResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    delay: Duration,
    trickle: Option<Duration>,
    read_body_over: Option<Duration>,
}

impl MockResponse {
    pub fn new(status: u16) -> Self {
        MockResponse {
            status,
            headers: Vec::new(),
            body: Vec::new(),
            delay: Duration::ZERO,
            trickle: None,
            read_body_over: None,
        }
    }

    pub fn json(status: u16, body: serde_json::Value) -> Self {
        MockResponse::new(status)
            .header("content-type", "application/json")
            .body(body.to_string())
    }

    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }

    pub fn body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = body.into();
        self
    }

    /// Waits before sending the response.
    pub fn delay(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }

    /// Sends the body one byte at a time, waiting `gap` before each byte.
    pub fn trickle(mut self, gap: Duration) -> Self {
        self.trickle = Some(gap);
        self
    }

    /// Reads the request body at a steady pace, over `duration`, before
    /// answering.
    pub fn read_body_slowly(mut self, duration: Duration) -> Self {
        self.read_body_over = Some(duration);
        self
    }
}

#[derive(Default)]
struct State {
    responses: VecDeque<MockResponse>,
    requests: Vec<RecordedRequest>,
}

pub struct MockServer {
    url: String,
    state: Arc<Mutex<State>>,
}

impl MockServer {
    /// Starts a server that answers requests with `responses`, in order.
    /// Requests beyond the script get a `500`.
    pub async fn start(responses: Vec<MockResponse>) -> MockServer {
        MockServer::start_with(|_| responses).await
    }

    /// Starts a server that answers requests with the responses that
    /// `responses` makes from its URL, for responses that link back to the
    /// server, such as an OIDC discovery document.
    pub async fn start_with(responses: impl FnOnce(&str) -> Vec<MockResponse>) -> MockServer {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let responses = responses(&url);
        let state = Arc::new(Mutex::new(State {
            responses: responses.into(),
            requests: Vec::new(),
        }));
        let server_state = Arc::clone(&state);
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(handle(stream, Arc::clone(&server_state)));
            }
        });
        MockServer { url, state }
    }

    /// Base URL of the server, e.g. `http://127.0.0.1:54321`.
    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.state.lock().unwrap().requests.clone()
    }
}

/// A URL on which nothing is listening.
pub async fn unused_url() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    format!("http://{}", listener.local_addr().unwrap())
}

async fn handle(stream: TcpStream, state: Arc<Mutex<State>>) {
    let mut stream = BufReader::new(stream);
    let Some((mut request, length)) = read_head(&mut stream).await else {
        return;
    };
    let response = state.lock().unwrap().responses.pop_front();
    let response =
        response.unwrap_or_else(|| MockResponse::new(500).body("no more scripted responses"));
    let Some(body) = read_body(&mut stream, length, response.read_body_over).await else {
        return;
    };
    request.body = body;
    state.lock().unwrap().requests.push(request);
    tokio::time::sleep(response.delay).await;

    let mut head = format!("HTTP/1.1 {} Mock\r\n", response.status);
    for (name, value) in &response.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str(&format!(
        "content-length: {}\r\nconnection: close\r\n\r\n",
        response.body.len()
    ));
    let stream = stream.get_mut();
    let _ = stream.write_all(head.as_bytes()).await;
    match response.trickle {
        Some(gap) => {
            for byte in &response.body {
                tokio::time::sleep(gap).await;
                if stream.write_all(&[*byte]).await.is_err() {
                    return;
                }
            }
        }
        None => {
            let _ = stream.write_all(&response.body).await;
        }
    }
    let _ = stream.shutdown().await;
}

/// Reads the request line and headers, and returns the request without its
/// body, and the body's length.
async fn read_head(stream: &mut BufReader<TcpStream>) -> Option<(RecordedRequest, usize)> {
    let mut line = String::new();
    stream.read_line(&mut line).await.ok()?;
    let mut request_line = line.split_whitespace();
    let method = request_line.next()?.to_owned();
    let target = request_line.next()?.to_owned();

    let mut headers = Vec::new();
    loop {
        line.clear();
        stream.read_line(&mut line).await.ok()?;
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        let (name, value) = line.split_once(':')?;
        headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
    }

    let length = headers
        .iter()
        .find(|(name, _)| name == "content-length")
        .and_then(|(_, value)| value.parse().ok())
        .unwrap_or(0);
    let request = RecordedRequest {
        method,
        target,
        headers,
        body: Vec::new(),
    };
    Some((request, length))
}

/// Reads `length` bytes of body, all at once or, with `over`, in small
/// reads paced to take that long.
async fn read_body(
    stream: &mut BufReader<TcpStream>,
    length: usize,
    over: Option<Duration>,
) -> Option<Vec<u8>> {
    let mut body = vec![0; length];
    let Some(over) = over else {
        stream.read_exact(&mut body).await.ok()?;
        return Some(body);
    };
    let started = tokio::time::Instant::now();
    let mut read = 0;
    while read < length {
        let end = (read + 16 * 1024).min(length);
        stream.read_exact(&mut body[read..end]).await.ok()?;
        read = end;
        let due = over.mul_f64(read as f64 / length as f64);
        tokio::time::sleep_until(started + due).await;
    }
    Some(body)
}
