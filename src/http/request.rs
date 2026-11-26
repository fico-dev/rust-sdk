//! Request building.

use std::fmt;
use std::time::Duration;

use reqwest::Url;
use reqwest::header::{HeaderName, HeaderValue};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::{Client, Paginator, Response, RetryPolicy, bearer_header, header_name, header_value};
use crate::{Error, Result};

/// HTTP request method.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Method {
    /// `GET`
    Get,
    /// `POST`
    Post,
    /// `PUT`
    Put,
    /// `PATCH`
    Patch,
    /// `DELETE`
    Delete,
}

impl Method {
    pub(crate) fn to_reqwest(self) -> reqwest::Method {
        match self {
            Method::Get => reqwest::Method::GET,
            Method::Post => reqwest::Method::POST,
            Method::Put => reqwest::Method::PUT,
            Method::Patch => reqwest::Method::PATCH,
            Method::Delete => reqwest::Method::DELETE,
        }
    }
}

impl fmt::Display for Method {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self.to_reqwest().as_str(), f)
    }
}

/// A request being configured. Created with [`Client::get`],
/// [`Client::post`], etc.
///
/// Every setting starts from the client's defaults and can be overridden
/// here. Invalid input (e.g. a malformed header) is reported when the
/// request is sent.
///
/// # Examples
///
/// ```no_run
/// # async fn run() -> fico_sdk::Result<()> {
/// use std::time::Duration;
/// use fico_sdk::Client;
///
/// let client = Client::new("https://api.example.com")?;
/// let run: serde_json::Value = client
///     .post("/v1/runs")
///     .query(&[("applicationId", "my-app")])
///     .header("x-request-id", "abc-123")
///     .json(&serde_json::json!({ "input": { "value": 10 } }))
///     .read_timeout(Duration::from_secs(60))
///     .send_json()
///     .await?;
/// # Ok(())
/// # }
/// ```
#[must_use = "a request does nothing until it is sent"]
pub struct RequestBuilder {
    client: Client,
    parts: Parts,
    error: Option<Error>,
}

/// Everything needed to send a request; cloned for every page when
/// paginating.
#[derive(Clone)]
pub(crate) struct Parts {
    pub(crate) method: Method,
    pub(crate) url: Url,
    pub(crate) query: Vec<(String, String)>,
    pub(crate) headers: Vec<HeaderOp>,
    pub(crate) auth: Auth,
    pub(crate) content_type: Option<HeaderValue>,
    pub(crate) body: Option<Vec<u8>>,
    pub(crate) read_timeout: Option<Duration>,
    pub(crate) timeout: Option<Duration>,
    pub(crate) retry: Option<RetryPolicy>,
}

/// Header changes applied, in order, on top of the client defaults.
#[derive(Clone)]
pub(crate) enum HeaderOp {
    Set(HeaderName, HeaderValue),
    Remove(HeaderName),
}

/// Which bearer token a request sends.
#[derive(Clone)]
pub(crate) enum Auth {
    /// The client's token, if the URL has the base URL's origin.
    Default,
    /// This token, regardless of the URL.
    Bearer(HeaderValue),
    /// No token.
    None,
}

impl RequestBuilder {
    pub(crate) fn new(client: Client, method: Method, url: Result<Url>) -> Self {
        let (url, error) = match url {
            Ok(url) => (url, None),
            Err(error) => (client.inner.base_url.clone(), Some(error)),
        };
        RequestBuilder {
            client,
            parts: Parts {
                method,
                url,
                query: Vec::new(),
                headers: Vec::new(),
                auth: Auth::Default,
                content_type: None,
                body: None,
                read_timeout: None,
                timeout: None,
                retry: None,
            },
            error,
        }
    }

    /// Appends query parameters, serialized from a struct, map or list of
    /// pairs. Can be called repeatedly.
    ///
    /// Values must be strings, numbers, booleans or lists of those; lists
    /// repeat the key and `None`/`null` values are skipped.
    pub fn query<T: Serialize + ?Sized>(mut self, query: &T) -> Self {
        match encode_query(query) {
            Ok(pairs) => self.parts.query.extend(pairs),
            Err(error) => self.fail(error),
        }
        self
    }

    /// Sets a header, replacing the client default or an earlier value.
    pub fn header(mut self, name: impl AsRef<str>, value: impl AsRef<str>) -> Self {
        let (name, value) = (name.as_ref(), value.as_ref());
        match header_name(name).and_then(|n| Ok((n, header_value(name, value)?))) {
            Ok((name, value)) => self.parts.headers.push(HeaderOp::Set(name, value)),
            Err(error) => self.fail(error),
        }
        self
    }

    /// Removes a header, including client defaults such as `User-Agent`.
    pub fn remove_header(mut self, name: impl AsRef<str>) -> Self {
        match header_name(name.as_ref()) {
            Ok(name) => self.parts.headers.push(HeaderOp::Remove(name)),
            Err(error) => self.fail(error),
        }
        self
    }

    /// Sends `Authorization: Bearer <token>` instead of the client's token,
    /// to any URL.
    pub fn bearer_token(mut self, token: impl AsRef<str>) -> Self {
        match bearer_header(token.as_ref()) {
            Ok(value) => self.parts.auth = Auth::Bearer(value),
            Err(error) => self.fail(error),
        }
        self
    }

    /// Sends no bearer token.
    pub fn no_auth(mut self) -> Self {
        self.parts.auth = Auth::None;
        self
    }

    /// Sets a JSON body and `Content-Type: application/json`.
    pub fn json<T: Serialize + ?Sized>(mut self, body: &T) -> Self {
        match serde_json::to_vec(body) {
            Ok(bytes) => {
                self.parts.body = Some(bytes);
                self.parts.content_type = Some(HeaderValue::from_static("application/json"));
            }
            Err(error) => self.fail(Error::Encode {
                reason: "JSON body is not serializable".to_owned(),
                source: Some(Box::new(error)),
            }),
        }
        self
    }

    /// Sets a raw body. Set `Content-Type` with [`header`](Self::header) if
    /// needed.
    pub fn body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.parts.body = Some(body.into());
        self.parts.content_type = None;
        self
    }

    /// Overrides the client's limit on each wait for the server, for each
    /// attempt of this request: for sending the request body, for the
    /// response head, and for each chunk of the response body. Raise it for
    /// large uploads. See [`ClientBuilder`'s time
    /// limits](crate::ClientBuilder#time-limits).
    pub fn read_timeout(mut self, timeout: Duration) -> Self {
        self.parts.read_timeout = Some(timeout);
        self
    }

    /// Sets a time limit for each whole attempt of this request, from
    /// connecting until the whole response body is read, instead of the
    /// client's. See [`ClientBuilder`'s time
    /// limits](crate::ClientBuilder#time-limits).
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.parts.timeout = Some(timeout);
        self
    }

    /// Overrides the client's retry policy for this request.
    pub fn retry(mut self, retry: RetryPolicy) -> Self {
        self.parts.retry = Some(retry);
        self
    }

    /// Disables retries for this request.
    pub fn no_retry(self) -> Self {
        self.retry(RetryPolicy::none())
    }

    /// Sends the request and buffers the response.
    ///
    /// # Errors
    ///
    /// Returns an error if the request is invalid, if it fails after all
    /// retries, if the response is a redirect that was not followed
    /// ([`Error::Redirect`]), or if the response status is 4xx or 5xx
    /// ([`Error::Status`]).
    pub async fn send(self) -> Result<Response> {
        let parts = self.into_parts()?;
        parts.client.execute(&parts.parts).await
    }

    /// Sends the request and deserializes the JSON response body.
    ///
    /// # Errors
    ///
    /// Same as [`send`](Self::send), plus [`Error::Decode`] if the body is
    /// not valid JSON for `T`. An empty body is read as `null`, see
    /// [`Response::json`].
    pub async fn send_json<T: DeserializeOwned>(self) -> Result<T> {
        self.send().await?.json()
    }

    /// Turns the request into a [`Paginator`] that follows
    /// `next_page_token` across pages.
    pub fn paginate<T: DeserializeOwned>(self) -> Paginator<T> {
        Paginator::new(self.client, self.parts, self.error)
    }

    pub(crate) fn into_parts(self) -> Result<ClientParts> {
        match self.error {
            Some(error) => Err(error),
            None => Ok(ClientParts {
                client: self.client,
                parts: self.parts,
            }),
        }
    }

    /// Records the first error, to be returned when the request is sent.
    fn fail(&mut self, error: Error) {
        self.error.get_or_insert(error);
    }
}

/// A validated request together with its client.
pub(crate) struct ClientParts {
    pub(crate) client: Client,
    pub(crate) parts: Parts,
}

impl std::ops::Deref for ClientParts {
    type Target = Parts;

    fn deref(&self) -> &Parts {
        &self.parts
    }
}

impl fmt::Debug for RequestBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RequestBuilder")
            .field("method", &self.parts.method)
            .field("url", &self.parts.url.as_str())
            .finish_non_exhaustive()
    }
}

/// Serializes `query` into key/value pairs.
fn encode_query<T: Serialize + ?Sized>(query: &T) -> Result<Vec<(String, String)>> {
    let encode_error = |reason: &str| Error::Encode {
        reason: format!("query parameters {reason}"),
        source: None,
    };
    let value = serde_json::to_value(query).map_err(|e| Error::Encode {
        reason: "query parameters are not serializable".to_owned(),
        source: Some(Box::new(e)),
    })?;
    let entries: Vec<(String, Value)> = match value {
        Value::Object(map) => map.into_iter().collect(),
        Value::Array(items) => items
            .into_iter()
            .map(|item| match item {
                Value::Array(pair) => match <[Value; 2]>::try_from(pair) {
                    Ok([Value::String(key), value]) => Ok((key, value)),
                    _ => Err(encode_error("must be (key, value) pairs")),
                },
                _ => Err(encode_error("must be (key, value) pairs")),
            })
            .collect::<Result<_, _>>()?,
        Value::Null => Vec::new(),
        _ => return Err(encode_error("must be a struct, map or list of pairs")),
    };

    let mut pairs = Vec::new();
    for (key, value) in entries {
        let values = match value {
            Value::Array(values) => values,
            value => vec![value],
        };
        for value in values {
            let value = match value {
                Value::Null => continue,
                Value::String(s) => s,
                Value::Bool(b) => b.to_string(),
                Value::Number(n) => n.to_string(),
                Value::Array(_) | Value::Object(_) => {
                    return Err(encode_error("cannot contain nested values"));
                }
            };
            pairs.push((key.clone(), value));
        }
    }
    Ok(pairs)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::error::Error as _;

    use serde_json::json;

    use super::*;

    fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn encodes_lists_of_pairs() {
        let query = [("a", "1"), ("b", "two"), ("a", "3")];
        assert_eq!(encode_query(&query).unwrap(), pairs(&query));
    }

    #[test]
    fn encodes_maps_and_scalars() {
        let mut map = BTreeMap::new();
        map.insert("limit", Value::from(5));
        map.insert("active", Value::from(true));
        map.insert("tags", Value::from(vec!["x", "y"]));
        map.insert("cursor", Value::Null);
        assert_eq!(
            encode_query(&map).unwrap(),
            pairs(&[
                ("active", "true"),
                ("limit", "5"),
                ("tags", "x"),
                ("tags", "y")
            ])
        );
        assert_eq!(encode_query(&()).unwrap(), Vec::new());
    }

    #[test]
    fn rejects_unencodable_queries() {
        for (query, reason) in [
            (json!("plain"), "must be a struct, map or list of pairs"),
            (json!([1, 2]), "must be (key, value) pairs"),
            (
                json!({ "nested": { "a": 1 } }),
                "cannot contain nested values",
            ),
            (json!({ "nested": [[1]] }), "cannot contain nested values"),
        ] {
            let err = encode_query(&query).unwrap_err();
            assert!(matches!(err, Error::Encode { source: None, .. }), "{query}");
            assert_eq!(
                err.to_string(),
                format!("could not encode request: query parameters {reason}")
            );
        }
    }

    #[test]
    fn rejects_unserializable_json_bodies() {
        let client = Client::new("https://host").unwrap();
        let body = BTreeMap::from([((1, 2), 3)]);
        let err = client.post("/x").json(&body).into_parts().err().unwrap();
        assert_eq!(
            err.to_string(),
            "could not encode request: JSON body is not serializable"
        );
        assert_eq!(err.source().unwrap().to_string(), "key must be a string");
    }

    #[test]
    fn keeps_the_first_error() {
        let client = Client::new("https://host").unwrap();
        let err = client
            .get("/x")
            .header("bad one", "v")
            .header("bad two", "v")
            .into_parts()
            .err()
            .unwrap();
        assert!(matches!(err, Error::InvalidHeader { name } if name == "bad one"));
    }
}
