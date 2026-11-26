//! The `decision` capability: workspaces, builds, environments, runs and
//! experiments, on the FICO® platform and on this machine.
//!
//! [`Platform`](platform::Platform) works on the platform and
//! [`Local`](local::Local) on this machine. They are the only types with
//! behavior: every operation is a method on one of them and returns plain
//! data defined in this module. [`tools`] serves every operation as an MCP
//! tool. [`runtime`] is for a workspace's own code, which runs inside a run:
//! it reads the run's input and options and writes its output.
//!
//! Rust names use the decision nouns, and JSON uses the platform API's
//! names: a workspace is an application, a build is a version and an
//! environment is an instance.
//!
//! # Examples
//!
//! ```no_run
//! # async fn run() -> fico_sdk::Result<()> {
//! use fico_sdk::Client;
//! use fico_sdk::decision::platform::{BuildCreateParams, Platform, RunListParams};
//!
//! let client = Client::builder("https://api.example.com").bearer_token("TOKEN").build()?;
//! let platform = Platform::new(client); // No I/O.
//! let workspace = platform.get_workspace("routing").await?; // Workspace
//! let build = platform.create_build("routing", BuildCreateParams::default()).await?; // Build
//! let runs = platform.list_runs("routing", RunListParams::default()).await?; // Vec<Run>
//! # Ok(())
//! # }
//! ```

pub mod local;
pub mod manifest;
pub mod platform;
pub mod runtime;

mod archive;
mod bundle;
mod clone;
pub(crate) mod compare;
mod fs;
mod id;
mod path;
pub(crate) mod polling;
mod time;
mod types;

pub use self::types::acceptance::*;
pub use self::types::account::*;
pub use self::types::build::*;
pub use self::types::ensemble::*;
pub use self::types::environment::*;
pub use self::types::experiment::*;
pub use self::types::input_set::*;
pub use self::types::integration::*;
pub use self::types::local_workspace::*;
pub use self::types::manifest::*;
pub use self::types::polling::*;
pub use self::types::run::*;
pub use self::types::scenario::*;
pub use self::types::secrets::*;
pub use self::types::shadow::*;
pub use self::types::sso::*;
pub use self::types::switchback::*;
pub use self::types::upload::*;
pub use self::types::workspace::*;

use crate::tool::Tool;

/// Every `decision_*` tool: one per operation, named
/// `decision_<root>_<noun>_<verb>`, e.g. `decision_platform_run_get`.
pub fn tools() -> Vec<Tool> {
    [manifest::tools(), platform::tools(), local::tools()]
        .into_iter()
        .flatten()
        .collect()
}
