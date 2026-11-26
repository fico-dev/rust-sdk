//! Buffered HTTP responses.

use std::fmt;

use reqwest::Url;
use reqwest::header::HeaderMap;
use serde::de::DeserializeOwned;

use crate::error::redact_query;
use crate::{Error, Result};

/// A response whose body has been read into memory.
///
/// The whole body is held in memory, so this is meant for API responses,
/// not large file downloads.
pub struct Response {
    status: u16,
    url: String,
    headers: HeaderMap,
    body: Vec<u8>,
}

impl Response {
    pub(crate) fn new(status: u16, url: String, headers: HeaderMap, body: Vec<u8>) -> Self {
        Response {
            status,
            url,
            headers,
            body,
        }
    }

    /// HTTP status code.
    pub fn status(&self) -> u16 {
        self.status
    }

    /// Final URL of the response, after any redirects.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Value of the first header named `name` (case-insensitive), if present
    /// and valid UTF-8.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name)?.to_str().ok()
    }

    /// Raw response body.
    pub fn bytes(&self) -> &[u8] {
        &self.body
    }

    /// Takes ownership of the raw response body.
    pub fn into_bytes(self) -> Vec<u8> {
        self.body
    }

    /// Response body as UTF-8 text.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Decode`] if the body is not valid UTF-8.
    pub fn text(&self) -> Result<&str> {
        std::str::from_utf8(&self.body).map_err(|e| self.decode_error(e))
    }

    /// Deserializes the JSON response body. An empty body (e.g. from a
    /// `204 No Content`) is read as `null`, so it decodes into `()` or
    /// `Option<T>`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Decode`] if the body is not valid JSON for `T`.
    pub fn json<T: DeserializeOwned>(&self) -> Result<T> {
        let body: &[u8] = if self.body.is_empty() {
            b"null"
        } else {
            &self.body
        };
        serde_json::from_slice(body).map_err(|e| self.decode_error(e))
    }

    /// Turns redirects that were not followed into [`Error::Redirect`], and
    /// 4xx and 5xx responses into [`Error::Status`].
    pub(crate) fn error_for_status(self) -> Result<Response> {
        if (300..400).contains(&self.status)
            && let Some(location) = self.location()
        {
            return Err(Error::Redirect {
                status: self.status,
                url: redact_query(&self.url),
                location,
            });
        }
        if self.status < 400 {
            return Ok(self);
        }
        Err(Error::Status {
            status: self.status,
            body: String::from_utf8_lossy(&self.body).into_owned(),
            url: redact_query(&self.url),
        })
    }

    /// The `Location` header, resolved against the response URL.
    fn location(&self) -> Option<String> {
        let location = self.header("location")?;
        let resolved = Url::parse(&self.url).and_then(|url| url.join(location));
        Some(resolved.map_or_else(|_| location.to_owned(), String::from))
    }

    /// An [`Error::Decode`] for this response, with `error` as the source.
    pub(crate) fn decode_error(
        &self,
        error: impl std::error::Error + Send + Sync + 'static,
    ) -> Error {
        Error::Decode {
            url: redact_query(&self.url),
            source: Box::new(error),
        }
    }
}

impl fmt::Debug for Response {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Response")
            .field("status", &self.status)
            .field("url", &redact_query(&self.url))
            .field("body_len", &self.body.len())
            .finish_non_exhaustive()
    }
}
