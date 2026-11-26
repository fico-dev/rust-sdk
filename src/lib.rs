//! Official Rust SDK for the FICO® platform.
//!
//! * [`decision`]: the `decision` capability. Its operations are methods of
//!   [`Platform`](decision::platform::Platform), on the platform, and of
//!   [`Local`](decision::local::Local), on this machine.
//!   [`decision::runtime`] is for a workspace's own code: it reads a run's
//!   input and options and writes its output, metrics and assets.
//! * [`config`]: the profiles in `config.yaml`, and their resolution, with
//!   the environment, into an endpoint and a credential.
//! * [`auth`]: PKCE logins and their tokens.
//! * [`cache`]: the dependency cache that push bundles Python packages from.
//! * [`uv`]: the SDK's pinned `uv`, which push and local Python runs use.
//! * [`mcp`]: the MCP server, which serves the operations as tools
//!   ([`tool`]).
//! * [`Client`]: the HTTP transport under `Platform`, with retries and time
//!   limits.
//!
//! The I/O is async, on tokio. Fallible functions return [`Result`], whose
//! [`Error`] is shared by the whole SDK; only [`Tool::call`](tool::Tool::call)
//! wraps it in a [`ToolError`](tool::ToolError).
//!
//! # Examples
//!
//! Lists the workspaces on the platform, with the default profile or the
//! `FICO_PROFILE`, `FICO_ENDPOINT` and `FICO_API_KEY` environment variables:
//!
//! ```no_run
//! # async fn run() -> fico_sdk::Result<()> {
//! use fico_sdk::config::{self, Config, Env};
//! use fico_sdk::decision::platform::Platform;
//!
//! let home = config::home_dir()?;
//! let config = Config::load(&home).await?;
//! let Some(resolved) = config.resolve(None, &Env::from_process())? else {
//!     return Ok(()); // No profile, and no `FICO_API_KEY`.
//! };
//! let platform = Platform::new(resolved.client_builder(&home).build()?);
//! for workspace in platform.list_workspaces().await? {
//!     println!("{}", workspace.workspace_id);
//! }
//! # Ok(())
//! # }
//! ```

pub mod auth;
pub mod cache;
pub mod config;
pub mod decision;
mod error;
mod http;
pub mod mcp;
mod rand;
mod time;
pub mod tool;
pub mod uv;
mod zip;

pub use error::{Error, ProfileProblem, Result};
pub use http::{
    Client, ClientBuilder, Method, Paginator, RequestBuilder, Response, RetryPolicy, TokenSource,
};

/// The version of this crate, `fico-sdk`, e.g. `0.1.0`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
