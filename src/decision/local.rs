//! The `decision local` capability: workspaces and runs on this machine.
//!
//! [`Local`] is the only type with behavior. Every operation is a method on
//! it, defined in the file of its noun, and returns plain data from
//! [`decision`](super). The parameters of each operation are re-exported
//! here.

mod executor;
mod registry;
mod run;
pub(crate) mod run_dir;
mod run_logs;
mod visuals;
mod workspace;

pub use self::run::*;
pub use self::run_logs::*;
pub use self::workspace::*;

use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::Deserialize;

use crate::decision::LocalWorkspaceRef;
use crate::tool::Tool;
use crate::{Result, config};

/// Entry point for the `decision local` capability. Build one with
/// [`Local::new`] or [`Local::with_home`].
///
/// It works in a FICO home directory, `FICO_HOME` or `$HOME/.fico` by
/// default, which holds the local registry. Local runs live in each
/// workspace's own directory.
#[derive(Debug, Clone)]
pub struct Local {
    home: PathBuf,
    /// The program that executes local runs in their own process, if the
    /// host registered one with [`Local::with_executor`].
    executor: Option<executor::Executor>,
    /// The `uv` binary that runs Python workspaces, if the host set one
    /// with [`Local::with_uv`].
    uv: Option<PathBuf>,
}

impl Local {
    /// Creates the local root over the FICO home: `FICO_HOME` when it is
    /// set and not blank, else `$HOME/.fico` ([`config::home_dir`]). Does no
    /// I/O beyond resolving it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`](crate::Error::Invalid) if `FICO_HOME` is
    /// unset and the home directory cannot be resolved. Use
    /// [`Local::with_home`] then.
    pub fn new() -> Result<Local> {
        Ok(Local::with_home(config::home_dir()?))
    }

    /// Creates the local root over `home`, which takes the place of the
    /// FICO home. Does no I/O.
    pub fn with_home(home: impl Into<PathBuf>) -> Local {
        Local {
            home: home.into(),
            executor: None,
            uv: None,
        }
    }

    /// The FICO home this works in, e.g. for the MCP server's cache of
    /// platform runs.
    pub(crate) fn home(&self) -> &Path {
        &self.home
    }

    /// Runs Python workspaces with the `uv` binary at `uv`, instead of
    /// `FICO_UV` or the SDK's own, which it installs in the FICO home
    /// ([`crate::uv`]). Does no I/O.
    pub fn with_uv(mut self, uv: impl Into<PathBuf>) -> Local {
        self.uv = Some(uv.into());
        self
    }

    /// The `uv` binary that runs Python workspaces: the one set with
    /// [`Local::with_uv`], else `FICO_UV`, else the SDK's own in the FICO
    /// home, installed now if needed.
    pub(crate) async fn uv(&self) -> Result<std::ffi::OsString> {
        crate::uv::resolve(self.uv.as_deref(), || Ok(self.home.clone()), || {}).await
    }
}

/// Tool arguments that address a local workspace, plus the operation's
/// parameters.
#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct InLocalWorkspace<P> {
    #[serde(flatten)]
    pub(crate) workspace: LocalWorkspaceRef,
    #[serde(flatten)]
    pub(crate) params: P,
}

/// Every `decision_local_*` tool.
pub(crate) fn tools() -> Vec<Tool> {
    [run::tools(), workspace::tools()]
        .into_iter()
        .flatten()
        .collect()
}
