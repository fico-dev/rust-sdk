//! Token-based pagination.

use std::collections::VecDeque;
use std::fmt;
use std::marker::PhantomData;

use serde::de::DeserializeOwned;
use serde_json::Value;

use super::Client;
use super::request::Parts;
use crate::error::redact_query;
use crate::{Error, Result};

const DEFAULT_MAX_PAGES: usize = 500;
const DEFAULT_ITEMS_KEY: &str = "items";

/// Iterates over items of a paginated endpoint. Created with
/// [`RequestBuilder::paginate`](crate::RequestBuilder::paginate).
///
/// Each page is requested with `pagereturn=true`. Items are read from the
/// `items_key` field (default `"items"`) and the next page is requested by
/// sending the response's `next_page_token` back as `pagetoken`, until no
/// token is returned.
///
/// # Examples
///
/// ```no_run
/// # async fn run() -> fico_sdk::Result<()> {
/// use fico_sdk::Client;
///
/// let client = Client::new("https://api.example.com")?;
///
/// // All at once.
/// let apps: Vec<serde_json::Value> = client.get("/v1/applications").paginate().collect().await?;
///
/// // One at a time, fetching pages as needed.
/// let mut runs = client.get("/v1/runs").paginate::<serde_json::Value>().max_pages(10);
/// while let Some(run) = runs.next().await? {
///     println!("{run}");
/// }
/// # Ok(())
/// # }
/// ```
#[must_use = "a paginator does nothing until `next` or `collect` is called"]
pub struct Paginator<T> {
    client: Client,
    parts: Parts,
    error: Option<Error>,
    max_pages: usize,
    items_key: String,
    pages: usize,
    next_token: Option<String>,
    done: bool,
    failed: bool,
    buffer: VecDeque<T>,
    _items: PhantomData<fn() -> T>,
}

impl<T: DeserializeOwned> Paginator<T> {
    pub(crate) fn new(client: Client, parts: Parts, error: Option<Error>) -> Self {
        Paginator {
            client,
            parts,
            error,
            max_pages: DEFAULT_MAX_PAGES,
            items_key: DEFAULT_ITEMS_KEY.to_owned(),
            pages: 0,
            next_token: None,
            done: false,
            failed: false,
            buffer: VecDeque::new(),
            _items: PhantomData,
        }
    }

    /// Sets the maximum number of pages to fetch. Needing more pages is an
    /// error, which guards against pagination loops. Defaults to 500.
    pub fn max_pages(mut self, max_pages: usize) -> Self {
        self.max_pages = max_pages;
        self
    }

    /// Sets the response field holding the page's items. Defaults to
    /// `"items"`.
    pub fn items_key(mut self, items_key: impl Into<String>) -> Self {
        self.items_key = items_key.into();
        self
    }

    /// Returns the next item, fetching the next page when needed, or `None`
    /// when there are no more items.
    ///
    /// A page is only returned if all its items decode, and after an error
    /// every later call fails too, so `None` always means the list is
    /// exhausted.
    ///
    /// # Errors
    ///
    /// Returns any error from sending the request, [`Error::Decode`] if an
    /// item cannot be deserialized, or [`Error::Pagination`] if a response
    /// is malformed, more than `max_pages` pages are needed, or an earlier
    /// call failed.
    pub async fn next(&mut self) -> Result<Option<T>> {
        loop {
            if let Some(item) = self.buffer.pop_front() {
                return Ok(Some(item));
            }
            if self.failed {
                return Err(Error::Pagination {
                    reason: "an earlier page failed. Start a new paginator to retry".to_owned(),
                });
            }
            if self.done {
                return Ok(None);
            }
            let result = match self.error.take() {
                Some(error) => Err(error),
                None => self.fetch_page().await,
            };
            if let Err(error) = result {
                self.failed = true;
                return Err(error);
            }
        }
    }

    /// Fetches all remaining items.
    ///
    /// # Errors
    ///
    /// Same as [`next`](Self::next).
    pub async fn collect(mut self) -> Result<Vec<T>> {
        let mut items = Vec::new();
        while let Some(item) = self.next().await? {
            items.push(item);
        }
        Ok(items)
    }

    async fn fetch_page(&mut self) -> Result<()> {
        if self.pages >= self.max_pages {
            return Err(Error::Pagination {
                reason: format!(
                    "exceeded the maximum number of pages ({}). Increase `max_pages` if you \
                     expect more pages, or check for a pagination loop",
                    self.max_pages
                ),
            });
        }

        let mut parts = self.parts.clone();
        parts
            .query
            .push(("pagereturn".to_owned(), "true".to_owned()));
        if let Some(token) = &self.next_token {
            parts.query.push(("pagetoken".to_owned(), token.clone()));
        }
        let response = self.client.execute(&parts).await?;
        self.pages += 1;

        let malformed = |reason: String| Error::Pagination {
            reason: format!("response from {} {reason}", redact_query(response.url())),
        };
        let Value::Object(mut page) = response.json::<Value>()? else {
            return Err(malformed("is not a JSON object".to_owned()));
        };
        let items = match page.remove(&self.items_key) {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(items)) => items
                .into_iter()
                .map(serde_json::from_value)
                .collect::<Result<Vec<T>, _>>()
                .map_err(|e| response.decode_error(e))?,
            Some(_) => {
                return Err(malformed(format!(
                    "has a non-list `{}` field",
                    self.items_key
                )));
            }
        };
        let next_token = match page.remove("next_page_token") {
            None | Some(Value::Null) => None,
            Some(Value::String(token)) if token.is_empty() => None,
            Some(Value::String(token)) => Some(token),
            Some(_) => return Err(malformed("has a non-string `next_page_token`".to_owned())),
        };
        self.buffer.extend(items);
        self.done = next_token.is_none();
        self.next_token = next_token;
        Ok(())
    }
}

impl<T> fmt::Debug for Paginator<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Paginator")
            .field("url", &self.parts.url.as_str())
            .field("max_pages", &self.max_pages)
            .field("items_key", &self.items_key)
            .field("pages", &self.pages)
            .finish_non_exhaustive()
    }
}
