//! Workspace operations, on `v1/applications`.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::{fmt, io};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

use super::Platform;
use super::data::{UPLOAD_TIMEOUT, upload_timeout};
use super::environment::check_not_reserved;
use crate::cache::Limits;
use crate::decision::bundle::{self, Bundler, Uv};
use crate::decision::id::safe_id;
use crate::decision::manifest::{self, MANIFEST_FILE};
use crate::decision::path::api_path;
use crate::decision::polling::{Change, gone, poll_change};
use crate::decision::{
    Manifest, ManifestPipRequirements, ManifestType, PollingOptions, Workspace, WorkspaceSettings,
    archive,
};
use crate::tool::{AbortOnDrop, InWorkspace, NoParams, Tool};
use crate::uv::UV_VERSION;
use crate::{Error, Result, config};

/// Receives the progress events of [`Platform::push_workspace`]. Cheap to
/// clone: clones share the callback.
///
/// The callback is called synchronously, on the task that does the work,
/// once per event and in order. It must be quick and must not block, since
/// the work waits for it; a sink that does slow I/O should hand the events
/// to a channel instead.
#[derive(Clone)]
pub struct PushProgress(Arc<dyn Fn(&PushEvent) + Send + Sync>);

impl PushProgress {
    /// Creates a sink that calls `f` with every event.
    pub fn new(f: impl Fn(&PushEvent) + Send + Sync + 'static) -> PushProgress {
        PushProgress(Arc::new(f))
    }

    /// Reports `event` to the sink.
    fn emit(&self, event: PushEvent) {
        (self.0)(&event);
    }
}

impl fmt::Debug for PushProgress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PushProgress")
    }
}

/// Reports `event` to `progress`, if there is a sink.
fn emit(progress: Option<&PushProgress>, event: PushEvent) {
    if let Some(progress) = progress {
        progress.emit(event);
    }
}

/// A step of [`Platform::push_workspace`], reported to a [`PushProgress`]
/// sink.
///
/// Every variant is `#[non_exhaustive]`, so fields can be added later:
/// match them with `..`. Serialized, an event is an object whose `event`
/// key is the variant's name in snake case, e.g.
/// `{"event": "packaged", "files": 3, "bytes": 1024, "with_dependencies": false}`.
///
/// Events never carry an API key, a token or an environment variable's
/// value. [`CommandOutput`](PushEvent::CommandOutput) is what the user's
/// own build or pre-push command printed.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
#[non_exhaustive]
pub enum PushEvent {
    /// A push of a local workspace to the platform started.
    #[non_exhaustive]
    PushStarted {
        /// The workspace pushed to.
        workspace_id: String,
    },
    /// Push is running the manifest's build command.
    #[non_exhaustive]
    BuildCommand {
        /// The command, as the manifest writes it.
        command: String,
    },
    /// Push is running the manifest's pre-push command.
    #[non_exhaustive]
    PrePushCommand {
        /// The command, as the manifest writes it.
        command: String,
    },
    /// A line that the build or pre-push command wrote to stdout, without
    /// its line break. Push captures that output: it never reaches the
    /// host's stdout.
    #[non_exhaustive]
    CommandOutput {
        /// The line, with invalid UTF-8 replaced.
        text: String,
    },
    /// Push is collecting the files that the manifest lists.
    #[non_exhaustive]
    CopyingFiles {},
    /// Push is compressing the files into a tar.gz bundle.
    #[non_exhaustive]
    Compressing {},
    /// Push packaged the workspace.
    #[non_exhaustive]
    Packaged {
        /// The number of the workspace's files in the bundle, the manifest
        /// included, without the files of its Python dependencies.
        files: u64,
        /// The size of the compressed bundle, in bytes.
        bytes: u64,
        /// Whether the bundle holds the workspace's Python dependencies.
        with_dependencies: bool,
    },
    /// Push is uploading the bundle to the platform.
    #[non_exhaustive]
    Uploading {
        /// The workspace uploaded to.
        workspace_id: String,
    },
    /// Push uploaded the bundle and set the workspace's requirements.
    #[non_exhaustive]
    Pushed {
        /// The workspace pushed to.
        workspace_id: String,
    },
    /// Push is downloading the SDK's `uv`, which bundles Python
    /// dependencies, as it is not installed yet ([`crate::uv::install`]).
    /// Only the first push that needs it does, in each FICO home.
    #[non_exhaustive]
    InstallingUv {
        /// The version of `uv` installed.
        version: String,
    },
    /// Push started bundling the workspace's Python dependencies.
    #[non_exhaustive]
    BundlingStarted {
        /// Whether the dependency cache is used.
        cache: bool,
    },
    /// Some of the Python dependencies were found in the cache.
    #[non_exhaustive]
    DependenciesCached {
        /// The number found in the cache.
        cached: u64,
        /// The number of dependencies.
        total: u64,
    },
    /// Push is downloading and compressing Python dependencies from the
    /// package index.
    #[non_exhaustive]
    DependenciesDownloading {
        /// The number of dependencies downloaded.
        count: u64,
    },
    /// Push is building and compressing local or direct Python requirements
    /// from source.
    #[non_exhaustive]
    DirectRequirementsBuilding {
        /// The number of requirements built.
        count: u64,
    },
}

/// `CREATE_NEW_PROCESS_GROUP`, from the Windows API.
#[cfg(windows)]
pub(crate) const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

/// Parameters for [`Platform::create_workspace`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
#[serde(default)]
pub struct WorkspaceCreateParams {
    /// ID of the workspace. Generated when not set.
    pub workspace_id: Option<String>,
    /// Name of the workspace. Defaults to its ID.
    pub name: Option<String>,
    /// Description of the workspace.
    pub description: Option<String>,
    /// Whether the workspace is for a decision workflow.
    pub is_workflow: Option<bool>,
    /// ID of the environment that runs use by default. Can't be `default`,
    /// which names this environment.
    pub default_environment_id: Option<String>,
    /// ID of the environment that experiments use by default. Can't be
    /// `default`.
    pub default_experiment_environment: Option<String>,
    /// Whether to return the workspace with this ID if it exists, instead of
    /// creating one. Needs `workspace_id`.
    pub exist_ok: bool,
}

/// Parameters for [`Platform::update_workspace`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
#[serde(default)]
pub struct WorkspaceUpdateParams {
    /// New name of the workspace.
    pub name: Option<String>,
    /// New description of the workspace.
    pub description: Option<String>,
    /// ID of the new environment that runs use by default. Can't be
    /// `default`, which names this environment.
    pub default_environment_id: Option<String>,
    /// ID of the new environment that experiments use by default. Can't be
    /// `default`.
    pub default_experiment_environment: Option<String>,
    /// New settings of the workspace. They replace the current ones: an
    /// empty list of links removes them all.
    pub settings: Option<WorkspaceSettings>,
    /// Whether to lock (true) or unlock (false) the workspace. Unset leaves
    /// it as it is.
    pub locked: Option<bool>,
}

/// The `deletion_status` of a workspace that the platform could not delete.
const DELETION_FAILED: &str = "App deletion failed";

/// Parameters for [`Platform::delete_workspace`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
#[serde(default)]
pub struct WorkspaceDeleteParams {
    /// Polls until the workspace is gone. Without it, returns once the
    /// platform accepts the delete, which it finishes later. With the
    /// defaults, polling has no time or try limit.
    pub polling: Option<PollingOptions>,
}

/// Parameters for [`Platform::push_workspace`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
#[serde(default)]
pub struct WorkspacePushParams {
    /// Directory of the workspace to push. Its build and pre-push commands
    /// run in it, and the manifest's `files` are relative to it. A leading
    /// `~` stands for the home directory. Defaults to the current directory,
    /// also when empty.
    pub src: Option<PathBuf>,
    /// Manifest file to push the workspace with, bundled as `workspace.yaml`.
    /// A leading `~` stands for the home directory. Defaults to the
    /// `workspace.yaml` in `src`, also when empty.
    pub manifest: Option<PathBuf>,
    /// Whether to bundle a Python workspace's dependencies without the
    /// dependency cache, which stays unchanged: every package is downloaded
    /// again.
    pub no_cache: bool,
}

impl Platform {
    /// Creates a workspace.
    ///
    /// An unset or empty `workspace_id` is generated, and an unset or empty
    /// `name` defaults to the ID. With `exist_ok`, an existing workspace with
    /// the same ID is returned instead.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`], before any request, if `exist_ok` is set
    /// without a `workspace_id`, or if a default environment is
    /// [`DEFAULT_ENVIRONMENT_ID`](super::DEFAULT_ENVIRONMENT_ID), which is
    /// reserved.
    ///
    /// Also returns the [errors of every method](Platform#errors).
    pub async fn create_workspace(&self, params: WorkspaceCreateParams) -> Result<Workspace> {
        check_not_reserved(params.default_environment_id.as_deref())?;
        check_not_reserved(params.default_experiment_environment.as_deref())?;
        let workspace_id = match params
            .workspace_id
            .filter(|workspace_id| !workspace_id.is_empty())
        {
            Some(workspace_id) => workspace_id,
            None if params.exist_ok => {
                return Err(Error::Invalid {
                    reason: "`exist_ok` needs a `workspace_id`".to_owned(),
                });
            }
            None => safe_id("workspace"),
        };
        if params.exist_ok {
            match self.get_workspace(&workspace_id).await {
                Err(Error::Status { status: 404, .. }) => {}
                found => return found,
            }
        }
        let name = match params.name.filter(|name| !name.is_empty()) {
            Some(name) => name,
            None => workspace_id.clone(),
        };
        let mut body = json!({ "id": workspace_id, "name": name });
        if let Some(description) = params.description {
            body["description"] = description.into();
        }
        if let Some(is_workflow) = params.is_workflow {
            body["is_pipeline"] = is_workflow.into();
        }
        if let Some(environment_id) = params.default_environment_id {
            body["default_instance"] = environment_id.into();
        }
        if let Some(environment_id) = params.default_experiment_environment {
            body["default_experiment_instance"] = environment_id.into();
        }
        let path = api_path(&["v1", "applications"])?;
        self.client.post(path).json(&body).send_json().await
    }

    /// Gets a workspace.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Status`] with status 404 if the workspace does not
    /// exist, and the [errors of every method](Platform#errors).
    pub async fn get_workspace(&self, workspace_id: &str) -> Result<Workspace> {
        let path = api_path(&["v1", "applications", workspace_id])?;
        self.client.get(path).send_json().await
    }

    /// Checks whether a workspace exists.
    ///
    /// Returns `false` when the platform answers `404 Not Found`.
    ///
    /// # Errors
    ///
    /// Returns the [errors of every method](Platform#errors), except a 404,
    /// which is `false`.
    pub async fn workspace_exists(&self, workspace_id: &str) -> Result<bool> {
        let path = api_path(&["v1", "applications", workspace_id])?;
        match self.client.get(path).send().await {
            Ok(_) => Ok(true),
            Err(Error::Status { status: 404, .. }) => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// Lists the workspaces.
    ///
    /// # Errors
    ///
    /// Returns the [errors of every method](Platform#errors).
    pub async fn list_workspaces(&self) -> Result<Vec<Workspace>> {
        let path = api_path(&["v1", "applications"])?;
        self.client.get(path).paginate().collect().await
    }

    /// Updates a workspace.
    ///
    /// Fields that `params` leaves unset keep their values. An empty string
    /// counts as set. `locked` alone only locks or unlocks the workspace.
    ///
    /// The platform unlocks a locked workspace whenever it updates it, so a
    /// workspace that was locked is locked again after the other changes,
    /// unless `locked` is `false`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`], before any request, if `params` sets no
    /// field, or if a default environment is
    /// [`DEFAULT_ENVIRONMENT_ID`](super::DEFAULT_ENVIRONMENT_ID), which is
    /// reserved.
    ///
    /// If the update is applied but the request that locks the workspace
    /// again fails, returns that request's error, and the workspace stays
    /// unlocked.
    ///
    /// Also returns the [errors of every method](Platform#errors).
    pub async fn update_workspace(
        &self,
        workspace_id: &str,
        params: WorkspaceUpdateParams,
    ) -> Result<Workspace> {
        let WorkspaceUpdateParams {
            name,
            description,
            default_environment_id,
            default_experiment_environment,
            settings,
            locked,
        } = params;
        check_not_reserved(default_environment_id.as_deref())?;
        check_not_reserved(default_experiment_environment.as_deref())?;
        let changes = name.is_some()
            || description.is_some()
            || default_environment_id.is_some()
            || default_experiment_environment.is_some()
            || settings.is_some();
        if !changes {
            return match locked {
                Some(locked) => self.lock_workspace(workspace_id, locked).await,
                None => Err(Error::Invalid {
                    reason: "nothing to update: set `name`, `description`, \
                             `default_environment_id`, `default_experiment_environment`, \
                             `settings` or `locked`"
                        .to_owned(),
                }),
            };
        }
        let path = api_path(&["v1", "applications", workspace_id])?;
        // The whole workspace goes back, so fields this SDK does not model
        // keep their values too.
        let mut workspace: Map<String, Value> = self.client.get(&path).send_json().await?;
        let was_locked = workspace.get("locked") == Some(&Value::Bool(true));
        if let Some(name) = name {
            workspace.insert("name".to_owned(), name.into());
        }
        if let Some(description) = description {
            workspace.insert("description".to_owned(), description.into());
        }
        if let Some(environment_id) = default_environment_id {
            workspace.insert("default_instance".to_owned(), environment_id.into());
        }
        if let Some(environment_id) = default_experiment_environment {
            workspace.insert(
                "default_experiment_instance".to_owned(),
                environment_id.into(),
            );
        }
        if let Some(settings) = settings {
            workspace.insert("settings".to_owned(), json!(settings));
        }
        let updated: Workspace = self.client.put(path).json(&workspace).send_json().await?;
        match locked {
            Some(locked) if locked != updated.locked => {
                self.lock_workspace(workspace_id, locked).await
            }
            None if was_locked && !updated.locked => self.lock_workspace(workspace_id, true).await,
            _ => Ok(updated),
        }
    }

    /// Locks or unlocks a workspace, and returns it.
    async fn lock_workspace(&self, workspace_id: &str, locked: bool) -> Result<Workspace> {
        let path = api_path(&["v1", "applications", workspace_id, "lock"])?;
        self.client
            .put(path)
            .json(&json!({ "locked": locked }))
            .send_json()
            .await
    }

    /// Deletes a workspace, and everything in it.
    ///
    /// The platform deletes it in the background: with `params.polling`,
    /// this waits until a GET of it is a 404.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Wait`] if polling runs out of time or tries, or if
    /// the platform reports that the delete failed (its `deletion_status`).
    ///
    /// Also returns the [errors of every method](Platform#errors).
    pub async fn delete_workspace(
        &self,
        workspace_id: &str,
        params: WorkspaceDeleteParams,
    ) -> Result<()> {
        let path = api_path(&["v1", "applications", workspace_id])?;
        self.client.delete(path).send().await?;
        if let Some(options) = params.polling {
            poll_change(&options, || async {
                let result = self.get_workspace(workspace_id).await;
                gone(result, |workspace| {
                    match workspace.deletion_status.as_deref() {
                        Some(DELETION_FAILED) => Change::Failed(format!(
                            "the platform could not delete the workspace: {DELETION_FAILED}"
                        )),
                        _ => Change::Pending,
                    }
                })
            })
            .await?;
        }
        Ok(())
    }

    /// Pushes a local workspace to the platform: builds it and uploads its binary.
    ///
    /// Reads the manifest and runs its build and pre-push commands in
    /// `src`. Then it bundles the files the manifest lists, with the
    /// manifest as `workspace.yaml`, into a tar.gz, uploads it, and sets the
    /// workspace's requirements (type, runtime, content format, options and
    /// entry point) from the manifest. Making a build of it is a separate
    /// step, see [`Platform::create_build`].
    ///
    /// The commands start in a new process group, so a terminal's Ctrl-C
    /// does not reach them. Dropping the returned future kills them and every
    /// process they started; a caller that exits without dropping it, e.g.
    /// through `std::process::exit` or a signal it does not handle, leaves
    /// them running. They never read stdin, and their stdout is captured,
    /// because the MCP stdio transport uses both: each line of it is
    /// reported as [`PushEvent::CommandOutput`].
    ///
    /// # Python dependencies
    ///
    /// For a Python workspace whose manifest sets `python.pip-requirements`,
    /// push also bundles the packages it lists, with `uv`: the one set with
    /// [`Platform::with_uv`], else `FICO_UV`, else the SDK's own, which the
    /// first such push downloads into the FICO home ([`crate::uv`]), so no
    /// `uv` needs to be installed. `python.pip-requirements` is a list of packages, or a
    /// `requirements.txt` or `pyproject.toml` (its
    /// `[project].dependencies`) in `src`. `uv pip compile` pins them for
    /// `python.version` (3.10 or later, `3.11` when unset) and the Linux
    /// platform of `python.arch` (`arm64` when unset), and `uv pip install`
    /// installs each pinned package from a wheel. The installed files go
    /// under `.nextmv/python/deps/` in the bundle, where the platform's
    /// runtime reads them. Requirements that `uv` can't pin, such as local
    /// paths, URLs and VCS references, are built from source on every push.
    ///
    /// Pinned packages are kept in the dependency cache of the platform's
    /// FICO home ([`Platform::with_home`], else [`config::home_dir`]), in
    /// `<home>/cache/deps`, which [`cache::info`](crate::cache::info) and
    /// [`cache::clear`](crate::cache::clear) read and empty. With
    /// `no_cache`, push neither reads nor writes it. `uv` runs in `src`
    /// like the commands, also in a new process group with its output
    /// captured, and with `UV_SYSTEM_CERTS=true` unless it is set, so that
    /// it trusts the system's certificates.
    ///
    /// Uploading the bundle, and the platform's check of it, use a read
    /// limit of 10 minutes, whatever the client's. The upload's is longer
    /// for a bundle that takes more than that to send at 128 KiB/s: one
    /// more minute per 7.5 MiB. With the transport's limit on the response head (see
    /// [`ClientBuilder`'s time limits](crate::ClientBuilder#time-limits)),
    /// which also covers sending the bundle, each of the two requests can
    /// take about twice its read limit per attempt, and attempts are retried
    /// as the client's [`RetryPolicy`](crate::RetryPolicy) says. A
    /// whole-attempt limit set on the client
    /// ([`ClientBuilder::timeout`](crate::ClientBuilder::timeout)) still
    /// applies. The upload URL expires 10 minutes after the platform issues
    /// it. Storage checks that when an upload starts, so an upload that
    /// started in time completes, but a retry after it fails with
    /// [`Error::Status`] (403).
    ///
    /// Each step is reported to the platform's progress sink
    /// ([`Platform::with_push_progress`]), in this order: `PushStarted`,
    /// `InstallingUv` (only when it downloads the SDK's `uv`),
    /// `BuildCommand` and the command's output, `PrePushCommand` and its
    /// output, `BundlingStarted`, `DependenciesCached`,
    /// `DependenciesDownloading` and `DirectRequirementsBuilding` for Python
    /// dependencies, `CopyingFiles`, `Compressing`, `Packaged`, `Uploading`
    /// and `Pushed` ([`PushEvent`]).
    ///
    /// The SDK's `uv` is installed before the build and pre-push commands
    /// run, so that a failed download stops the push before anything else.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] if `workspace_id` is not a valid ID, a
    /// `pyproject.toml` has no list of dependencies, or the FICO home is
    /// needed but cannot be found, or the SDK's `uv` can't be installed for
    /// this platform or doesn't match its pinned SHA-256, [`Error::Io`] if
    /// `src` is not a directory, the manifest or a file cannot be read, the
    /// dependency cache or `uv` cannot be written, or a command or `uv`
    /// cannot be started, the transport's errors if `uv` can't be
    /// downloaded,
    /// [`Error::Manifest`] if the manifest is invalid, e.g. its Python
    /// version is too old to bundle dependencies, or the files it lists are
    /// missing, and [`Error::Command`] if a command or `uv` fails.
    ///
    /// Also returns the [errors of every method](Platform#errors).
    pub async fn push_workspace(
        &self,
        workspace_id: &str,
        params: WorkspacePushParams,
    ) -> Result<()> {
        let source = crate::uv::Source::default();
        self.push_workspace_with(workspace_id, params, &source, Limits::default())
            .await
    }

    /// [`Platform::push_workspace`], installing the SDK's `uv` from
    /// `uv_source`, and with `limits` as the dependency cache's.
    async fn push_workspace_with(
        &self,
        workspace_id: &str,
        params: WorkspacePushParams,
        uv_source: &crate::uv::Source,
        limits: Limits,
    ) -> Result<()> {
        let path = api_path(&["v1", "applications", workspace_id, "binary"])?;
        let progress = self.push_progress.as_ref();
        emit(
            progress,
            PushEvent::PushStarted {
                workspace_id: workspace_id.to_owned(),
            },
        );
        // An empty `src` or `manifest` means unset.
        let set = |path: &PathBuf| !path.as_os_str().is_empty();
        let src = manifest::workspace_dir(params.src.filter(set));
        let manifest_path = match params.manifest.filter(set) {
            Some(manifest_path) => manifest::expand_home(manifest_path),
            None => src.join(MANIFEST_FILE),
        };
        // Otherwise a missing `src` would fail as the commands' program.
        match tokio::fs::metadata(&src).await {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => {
                return Err(Error::Io {
                    path: src,
                    source: io::ErrorKind::NotADirectory.into(),
                });
            }
            Err(source) => return Err(Error::Io { path: src, source }),
        }
        let manifest = manifest::read(manifest_path.clone()).await?;
        // Checked before any command runs.
        let python = match dependencies(&manifest) {
            Some(requirements) => {
                let target = bundle::target(manifest.python.as_ref(), &manifest_path)?;
                let on_install = || {
                    let version = UV_VERSION.to_owned();
                    emit(progress, PushEvent::InstallingUv { version });
                };
                let home = || self.home();
                let explicit = self.uv.as_deref();
                let program =
                    crate::uv::resolve_from(explicit, home, on_install, uv_source).await?;
                let uv = Uv::binary(program);
                let cache = match params.no_cache {
                    true => None,
                    false => Some((self.home()?, limits)),
                };
                let bundler = Bundler {
                    uv,
                    cache,
                    temp_root: std::env::temp_dir(),
                };
                Some((requirements, target, bundler))
            }
            None => None,
        };

        let build = manifest.build.as_ref();
        if let Some(command) = build.and_then(|build| build.command.as_deref()) {
            // Run directly, not through a shell.
            let mut words = command.split_whitespace();
            if let Some(program) = words.next() {
                let args: Vec<&str> = words.collect();
                let environment = build.and_then(|build| build.environment.as_ref());
                emit(
                    progress,
                    PushEvent::BuildCommand {
                        command: command.to_owned(),
                    },
                );
                let program = OsStr::new(program);
                run_command(command, program, &args, &src, environment, progress).await?;
            }
        }
        if let Some(command) = manifest.pre_push.as_deref().filter(|c| !c.is_empty()) {
            let (shell, flag) = shell();
            emit(
                progress,
                PushEvent::PrePushCommand {
                    command: command.to_owned(),
                },
            );
            let args = [flag, command];
            run_command(command, shell.as_os_str(), &args, &src, None, progress).await?;
        }

        // The workspace's own files come last, and end the archive.
        let mut bundle = Vec::new();
        let with_dependencies = python.is_some();
        if let Some((requirements, target, bundler)) = python {
            emit(
                progress,
                PushEvent::BundlingStarted {
                    cache: bundler.cache.is_some(),
                },
            );
            let report = |event| emit(progress, event);
            bundle = bundler
                .bundle(&src, &manifest_path, requirements, &target, report)
                .await?;
        }
        emit(progress, PushEvent::CopyingFiles {});
        let files = bundle_files(src, &manifest, manifest_path).await?;
        let file_count = files.len() as u64;
        emit(progress, PushEvent::Compressing {});
        bundle.extend(archive::pack_files(files).await?);
        emit(
            progress,
            PushEvent::Packaged {
                files: file_count,
                bytes: bundle.len() as u64,
                with_dependencies,
            },
        );
        emit(
            progress,
            PushEvent::Uploading {
                workspace_id: workspace_id.to_owned(),
            },
        );

        /// The response of `GET A/binary`.
        #[derive(Deserialize)]
        struct BinaryUpload {
            /// Presigned URL to PUT the bundle to.
            upload_url: String,
        }
        let upload: BinaryUpload = self.client.get(&path).send_json().await?;
        // The URL is presigned: it takes no token.
        self.client
            .put(&upload.upload_url)
            .no_auth()
            .header("content-type", "application/octet-stream")
            .read_timeout(upload_timeout(bundle.len()))
            .body(bundle)
            .send()
            .await?;
        let body = json!({ "requirements": requirements(&manifest) });
        // The platform reads the whole bundle before it answers.
        self.client
            .put(path)
            .json(&body)
            .read_timeout(UPLOAD_TIMEOUT)
            .send()
            .await?;
        emit(
            progress,
            PushEvent::Pushed {
                workspace_id: workspace_id.to_owned(),
            },
        );
        Ok(())
    }
}

impl Platform {
    /// The FICO home whose dependency cache and `uv` push uses: the one set
    /// with [`Platform::with_home`], else [`config::home_dir`].
    fn home(&self) -> Result<PathBuf> {
        match &self.home {
            Some(home) => Ok(home.clone()),
            None => config::home_dir(),
        }
    }
}

/// The Python packages that push bundles for `manifest`, if any: its
/// `python.pip-requirements`, for a Python workspace, unless it is an empty
/// file name or list.
fn dependencies(manifest: &Manifest) -> Option<&ManifestPipRequirements> {
    if manifest.manifest_type != ManifestType::Python {
        return None;
    }
    let requirements = manifest.python.as_ref()?.pip_requirements.as_ref()?;
    match requirements {
        ManifestPipRequirements::File(file) if file.is_empty() => None,
        ManifestPipRequirements::Packages(packages) if packages.is_empty() => None,
        requirements => Some(requirements),
    }
}

/// Runs `program` with `args` in `dir`, with `environment` added to its
/// own, and waits for it. It starts in a new process group, its stdin is
/// empty, each line of its stdout is reported to `progress` as it comes,
/// and if the push is dropped, it is killed with every process it started.
/// `command` is the manifest's command, which a failure names.
async fn run_command(
    command: &str,
    program: &OsStr,
    args: &[&str],
    dir: &Path,
    environment: Option<&BTreeMap<String, String>>,
    progress: Option<&PushProgress>,
) -> Result<()> {
    let mut process = tokio::process::Command::new(program);
    process
        .args(args)
        .current_dir(dir)
        .envs(environment.into_iter().flatten())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    process.process_group(0);
    #[cfg(windows)]
    process.creation_flags(CREATE_NEW_PROCESS_GROUP);
    let io_error = |source| Error::Io {
        path: PathBuf::from(program),
        source,
    };
    let mut child = process.spawn().map_err(io_error)?;
    // `kill_on_drop` only kills the child, not what a shell starts. Declared
    // after `child`, the guard drops first, so it finds the group or tree
    // while the child still runs.
    let mut group = KillGroupOnDrop(child.id());
    // Both pipes are read before waiting, stderr in a task of its own, so
    // that a command that fills one pipe while the other is read never
    // blocks.
    let stderr = child.stderr.take().map(|mut pipe| {
        AbortOnDrop(tokio::spawn(async move {
            let mut stderr = Vec::new();
            pipe.read_to_end(&mut stderr).await.map(|_| stderr)
        }))
    });
    if let Some(pipe) = child.stdout.take() {
        let mut pipe = BufReader::new(pipe);
        let mut line = Vec::new();
        while pipe.read_until(b'\n', &mut line).await.map_err(io_error)? > 0 {
            let text = line.strip_suffix(b"\n").unwrap_or(&line);
            let text = text.strip_suffix(b"\r").unwrap_or(text);
            emit(
                progress,
                PushEvent::CommandOutput {
                    text: String::from_utf8_lossy(text).into_owned(),
                },
            );
            line.clear();
        }
    }
    let stderr = match stderr {
        Some(task) => match task.await {
            Ok(read) => read.map_err(io_error)?,
            Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
            Err(error) => return Err(io_error(io::Error::other(error))),
        },
        None => Vec::new(),
    };
    let status = child.wait().await.map_err(io_error)?;
    group.0 = None;
    if status.success() {
        return Ok(());
    }
    Err(Error::Command {
        program: command.to_owned(),
        status: status.code(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
}

/// When dropped, kills the process group of the process with this ID,
/// which leads it, unless the ID is `None`.
pub(crate) struct KillGroupOnDrop(pub(crate) Option<u32>);

impl Drop for KillGroupOnDrop {
    fn drop(&mut self) {
        let Some(id) = self.0 else {
            return;
        };
        // In-house, as std has no call for a group: `kill` on a negative
        // ID kills the group, and `taskkill /T` the process tree.
        let mut kill = if cfg!(windows) {
            let mut kill = std::process::Command::new("taskkill");
            kill.args(["/T", "/F", "/PID", &id.to_string()]);
            kill
        } else {
            let mut kill = std::process::Command::new("kill");
            kill.args(["-KILL", "--", &format!("-{id}")]);
            kill
        };
        // The group may be gone already, and the child is killed anyway.
        let _ = kill
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// The shell that runs the pre-push command, and its flag: `bash -c` when
/// `SHELL` is set and `bash` is on the `PATH`, else `cmd /c` on Windows and
/// `sh -c` elsewhere.
fn shell() -> (PathBuf, &'static str) {
    if std::env::var_os("SHELL").is_some()
        && let Some(bash) = find_program("bash")
    {
        return (bash, "-c");
    }
    if cfg!(windows) {
        (PathBuf::from("cmd"), "/c")
    } else {
        (PathBuf::from("sh"), "-c")
    }
}

/// The first executable file named `name` (`name.exe` on Windows) in the
/// directories of the `PATH`, as Python's `shutil.which` finds it.
fn find_program(name: &str) -> Option<PathBuf> {
    let file = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    };
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(&file))
        .find(|candidate| is_executable(candidate))
}

/// Whether `path` is a file this process may execute.
fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        metadata.is_file()
    }
}

/// The files to bundle, as `(name, path)` pairs for
/// [`archive::pack_files`]: the files that the manifest's `files` match in
/// `src`, then the manifest at `manifest_path` as `workspace.yaml`, unless
/// `files` matches a `workspace.yaml` already.
///
/// # Errors
///
/// Returns [`Error::Manifest`] if the files do not make a bundle, see
/// [`find_files`].
async fn bundle_files(
    src: PathBuf,
    manifest: &Manifest,
    manifest_path: PathBuf,
) -> Result<Vec<(PathBuf, PathBuf)>> {
    let patterns = manifest.files.clone();
    let entry_point = match manifest
        .execution
        .as_ref()
        .and_then(|e| e.entrypoint.as_deref())
    {
        Some(entry_point) => normalize(Path::new(entry_point)),
        None => PathBuf::from(match manifest.manifest_type {
            ManifestType::Python => "main.py",
            ManifestType::Go | ManifestType::Binary => "main",
            ManifestType::Java => "main.jar",
        }),
    };
    let dir = src.clone();
    // Walking the workspace blocks.
    let found = match tokio::task::spawn_blocking(move || find_files(&dir, &patterns, &entry_point))
        .await
    {
        Ok(found) => found.map_err(|reason| Error::Manifest {
            path: manifest_path.clone(),
            reason,
        })?,
        Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
        Err(error) => {
            return Err(Error::Io {
                path: src,
                source: io::Error::other(error),
            });
        }
    };
    let mut files = Vec::with_capacity(found.len() + 1);
    if !found.contains(Path::new(MANIFEST_FILE)) {
        files.push((PathBuf::from(MANIFEST_FILE), manifest_path));
    }
    files.extend(found.into_iter().map(|name| {
        let path = src.join(&name);
        (name, path)
    }));
    Ok(files)
}

/// The files that `patterns`, a manifest's `files`, match in `src`, relative
/// to it, or why they do not make a bundle:
///
/// - a pattern is a glob: `*`, `?` and `[…]` match within a name, as in
///   [`fnmatch`], and a `**` component matches any number of directories.
///   Names that start with `.` only match a pattern that does too.
/// - a trailing `/` stands for the files directly in a directory;
/// - a leading `!` removes what it matches from the files that the patterns
///   before it found;
/// - only files are kept, and a link to a file counts as one.
///
/// It fails if the files leave out `entry_point`, then if a pattern without
/// `!` matches nothing, and also if a pattern is absolute or matches a file
/// outside `src`.
fn find_files(
    src: &Path,
    patterns: &[String],
    entry_point: &Path,
) -> Result<BTreeSet<PathBuf>, String> {
    let mut found = BTreeSet::new();
    let mut missing = Vec::new();
    for entry in patterns {
        let mut pattern = entry.clone();
        if pattern.ends_with('/') {
            pattern.push('*');
        }
        let (negated, pattern) = match pattern.strip_prefix('!') {
            Some(rest) => (true, rest),
            None => (false, pattern.as_str()),
        };
        // With `!` too, as `glob` would read `/main.py` as `main.py`.
        if matches!(
            Path::new(pattern).components().next(),
            Some(Component::Prefix(_) | Component::RootDir)
        ) {
            return Err(format!(
                "the `files` entry `{entry}` is an absolute path, but `files` are relative to \
                 the workspace"
            ));
        }
        if negated {
            let matches: BTreeSet<PathBuf> =
                glob(src, pattern).iter().map(|p| normalize(p)).collect();
            found.retain(|file| !matches.contains(file));
            continue;
        }
        let matches = glob(src, pattern);
        if matches.is_empty() {
            missing.push(format!("`{entry}`"));
            continue;
        }
        for file in matches.iter().map(|p| normalize(p)) {
            if !src.join(&file).is_file() {
                continue;
            }
            if file.starts_with("..") {
                return Err(format!(
                    "the `files` entry `{entry}` matches `{}`, which is outside the workspace",
                    file.display()
                ));
            }
            found.insert(file);
        }
    }
    if !found.contains(entry_point) {
        return Err(format!(
            "`files` must include the workspace's entry point, `{}`",
            entry_point.display()
        ));
    }
    if !missing.is_empty() {
        return Err(format!(
            "nothing matches the `files` entries {}",
            missing.join(", ")
        ));
    }
    Ok(found)
}

/// The paths that the glob `pattern` matches under `src`, relative to it, as
/// Python's `glob.glob(pattern, recursive=True)` finds them: files and
/// directories, at paths built from the pattern, e.g. `./main.py`.
///
/// Unlike Python, `**` does not walk into a directory it is already in
/// through a link, so a link cycle can't make it loop.
fn glob(src: &Path, pattern: &str) -> Vec<PathBuf> {
    let parts: Vec<&str> = pattern
        .split(std::path::is_separator)
        .filter(|part| !part.is_empty())
        .collect();
    let mut paths = vec![PathBuf::new()];
    for (i, part) in parts.iter().enumerate() {
        // Only the last part may match a file.
        let dirs_only = i + 1 < parts.len();
        let mut next = Vec::new();
        for path in &paths {
            if *part == "**" {
                if path.as_os_str().is_empty() || src.join(path).is_dir() {
                    next.push(path.clone());
                }
                descend(src, path, dirs_only, &mut Vec::new(), &mut next);
            } else if part.contains(['*', '?', '[']) {
                for name in list(src, path, dirs_only) {
                    let text = name.to_string_lossy();
                    if (part.starts_with('.') || !text.starts_with('.')) && fnmatch(part, &text) {
                        next.push(path.join(name));
                    }
                }
            } else {
                // A literal name: the path must exist, which the parts after
                // it check if there are any.
                let joined = path.join(part);
                if dirs_only || src.join(&joined).symlink_metadata().is_ok() {
                    next.push(joined);
                }
            }
        }
        paths = next;
    }
    // A pattern of only `**` matches the directory itself, which Python
    // leaves out.
    paths.retain(|path| !path.as_os_str().is_empty());
    paths
}

/// Appends to `out` what is under `dir`, relative to `src`, at any depth, as
/// Python's `**` finds it: only directories if `dirs_only`, and nothing
/// whose name starts with `.`, or under it. `walking` holds the directories
/// being walked, so a link back into one of them is not walked again.
fn descend(
    src: &Path,
    dir: &Path,
    dirs_only: bool,
    walking: &mut Vec<PathBuf>,
    out: &mut Vec<PathBuf>,
) {
    let Ok(real) = src.join(dir).canonicalize() else {
        return;
    };
    if walking.contains(&real) {
        return;
    }
    walking.push(real);
    for name in list(src, dir, dirs_only) {
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        let path = dir.join(name);
        out.push(path.clone());
        if src.join(&path).is_dir() {
            descend(src, &path, dirs_only, walking, out);
        }
    }
    walking.pop();
}

/// The names in the directory `dir`, relative to `src`, or only its
/// directories (links to directories included) if `dirs_only`. A directory
/// that cannot be read has none, as in Python's `glob`.
fn list(src: &Path, dir: &Path, dirs_only: bool) -> Vec<OsString> {
    let Ok(entries) = std::fs::read_dir(src.join(dir)) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| !dirs_only || entry.path().is_dir())
        .map(|entry| entry.file_name())
        .collect()
}

/// `path` without `.` components, and with each `..` canceling the name
/// before it, as Python's `os.path.normpath` does. A leading `..` stays.
fn normalize(path: &Path) -> PathBuf {
    let mut parts: Vec<Component> = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir if matches!(parts.last(), Some(Component::Normal(_))) => {
                parts.pop();
            }
            component => parts.push(component),
        }
    }
    parts.iter().collect()
}

/// Whether `name` matches the shell pattern `pattern`, as Python's
/// `fnmatch` decides: `*` matches any text, `?` any one character, and
/// `[…]` one character of a set, such as `[ab]` or `[a-z]`, or one outside
/// it with `[!…]`. A `]` right after `[` or `[!` is in the set, and a `[`
/// with no closing `]` matches itself. Case only matters off Windows.
fn fnmatch(pattern: &str, name: &str) -> bool {
    let (pattern, name) = if cfg!(windows) {
        (pattern.to_lowercase(), name.to_lowercase())
    } else {
        (pattern.to_owned(), name.to_owned())
    };
    let tokens = tokens(&pattern);
    let name: Vec<char> = name.chars().collect();
    // Match one token per character, and on a mismatch let the last `*`
    // take one more character.
    let (mut t, mut n) = (0, 0);
    let mut star = None;
    while n < name.len() {
        match tokens.get(t) {
            Some(Token::Star) => {
                star = Some((t, n));
                t += 1;
            }
            Some(token) if token.matches(name[n]) => {
                t += 1;
                n += 1;
            }
            _ => match star {
                Some((star_t, star_n)) => {
                    t = star_t + 1;
                    n = star_n + 1;
                    star = Some((star_t, star_n + 1));
                }
                None => return false,
            },
        }
    }
    tokens[t..].iter().all(|token| matches!(token, Token::Star))
}

/// A part of an [`fnmatch`] pattern.
enum Token {
    /// `*`: any text.
    Star,
    /// `?`: any one character.
    Any,
    /// A character that matches itself.
    Char(char),
    /// `[…]`: one character in `ranges`, or outside them if `negated`.
    Set {
        negated: bool,
        ranges: Vec<(char, char)>,
    },
}

impl Token {
    /// Whether this token matches the one character `c`.
    fn matches(&self, c: char) -> bool {
        match self {
            Token::Star | Token::Any => true,
            Token::Char(own) => *own == c,
            Token::Set { negated, ranges } => {
                ranges.iter().any(|&(low, high)| low <= c && c <= high) != *negated
            }
        }
    }
}

/// The tokens of an [`fnmatch`] pattern.
fn tokens(pattern: &str) -> Vec<Token> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        match c {
            '*' if matches!(tokens.last(), Some(Token::Star)) => {}
            '*' => tokens.push(Token::Star),
            '?' => tokens.push(Token::Any),
            '[' => match set(&chars[i..]) {
                Some((set, length)) => {
                    tokens.push(set);
                    i += length;
                }
                None => tokens.push(Token::Char('[')),
            },
            c => tokens.push(Token::Char(c)),
        }
    }
    tokens
}

/// The set that `chars`, which follow a `[`, describe, and how many of them
/// it takes with its closing `]`, or `None` if there is no closing `]`. A
/// range whose ends are reversed, such as `z-a`, matches nothing.
fn set(chars: &[char]) -> Option<(Token, usize)> {
    let negated = chars.first() == Some(&'!');
    let start = usize::from(negated);
    let mut end = start + usize::from(chars.get(start) == Some(&']'));
    while *chars.get(end)? != ']' {
        end += 1;
    }
    let members = &chars[start..end];
    let mut ranges = Vec::new();
    let mut k = 0;
    while k < members.len() {
        if k + 2 < members.len() && members[k + 1] == '-' {
            ranges.push((members[k], members[k + 2]));
            k += 3;
        } else {
            ranges.push((members[k], members[k]));
            k += 1;
        }
    }
    Some((Token::Set { negated, ranges }, end + 1))
}

/// The requirements that `PUT A/binary` takes, from `manifest`, without a
/// statistics path: FICO has no run statistics.
fn requirements(manifest: &Manifest) -> Value {
    let content = &manifest.configuration.content;
    let mut io = json!({ "format": content.format });
    if let Some(multi_file) = &content.multi_file {
        let output = &multi_file.output;
        let mut paths = Map::new();
        for (key, path) in [
            ("metrics_path", &output.metrics),
            ("assets_path", &output.assets),
            ("solutions_path", &output.solutions),
        ] {
            if !path.is_empty() {
                paths.insert(key.to_owned(), path.as_str().into());
            }
        }
        io["multi_file"] = json!({
            "input_path": multi_file.input.path,
            "output_configuration": paths,
        });
    }
    let mut requirements = json!({
        "executable_type": manifest.manifest_type,
        "runtime": manifest.runtime,
        "io_configuration": io,
    });
    if let Some(options) = &manifest.configuration.options {
        let mut options = json!(options);
        // Local-only options are not meant for the platform.
        if let Some(Value::Array(items)) = options.get_mut("items") {
            items.retain(|item| item["local_only"] != true);
        }
        // The platform takes the manifest's list of entries as `template`.
        if let Some(format) = options.get_mut("format") {
            *format = json!({ "template": format.take() });
        }
        requirements["options"] = options;
    }
    if let Some(execution) = &manifest.execution {
        if let Some(entry_point) = execution.entrypoint.as_deref().filter(|e| !e.is_empty()) {
            requirements["entrypoint"] = entry_point.into();
        }
        if let Some(cwd) = execution.cwd.as_deref().filter(|cwd| !cwd.is_empty()) {
            requirements["working_directory"] = cwd.into();
        }
    }
    requirements
}

pub(crate) fn tools() -> Vec<Tool> {
    vec![
        Tool::new(
            "decision_platform_workspace_create",
            "Creates a workspace.",
            |cx, a: WorkspaceCreateParams| async move { cx.platform()?.create_workspace(a).await },
        ),
        Tool::new(
            "decision_platform_workspace_get",
            "Gets a workspace.",
            |cx, a: InWorkspace<NoParams>| async move {
                cx.platform()?.get_workspace(&a.workspace_id).await
            },
        )
        .read_only(),
        Tool::new(
            "decision_platform_workspace_exists",
            "Checks whether a workspace exists.",
            |cx, a: InWorkspace<NoParams>| async move {
                cx.platform()?.workspace_exists(&a.workspace_id).await
            },
        )
        .read_only(),
        Tool::new(
            "decision_platform_workspace_list",
            "Lists the workspaces.",
            |cx, _: NoParams| async move { cx.platform()?.list_workspaces().await },
        )
        .read_only(),
        Tool::new(
            "decision_platform_workspace_update",
            "Updates a workspace.",
            |cx, a: InWorkspace<WorkspaceUpdateParams>| async move {
                cx.platform()?
                    .update_workspace(&a.workspace_id, a.params)
                    .await
            },
        )
        .destructive(),
        Tool::new(
            "decision_platform_workspace_delete",
            "Deletes a workspace.",
            |cx, a: InWorkspace<WorkspaceDeleteParams>| async move {
                cx.platform()?
                    .delete_workspace(&a.workspace_id, a.params)
                    .await
            },
        )
        .destructive(),
        Tool::new(
            "decision_platform_workspace_push",
            "Pushes a local workspace to the platform: builds it and uploads its binary.",
            |cx, a: InWorkspace<WorkspacePushParams>| async move {
                cx.local()?;
                cx.platform()?
                    .push_workspace(&a.workspace_id, a.params)
                    .await
            },
        )
        .local_only()
        .destructive(),
    ]
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    /// A new directory under the system's temp directory, removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> TempDir {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("fico-sdk-push-{}-{n}", std::process::id()));
            // Left over from an earlier test process with the same ID.
            let _ = fs::remove_dir_all(&path);
            fs::create_dir(&path).unwrap();
            TempDir(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// A temp directory holding `ws/`, the workspace of the glob tests, and
    /// `outside.txt` next to it.
    fn workspace() -> (TempDir, PathBuf) {
        let root = TempDir::new();
        let src = root.0.join("ws");
        for file in [
            "main.py",
            "src/a.py",
            "src/sub/b.py",
            "src/.dot.py",
            ".env",
            ".hidden/h.py",
            "data/x.json",
            "data/y.json",
            "[x].txt",
        ] {
            let path = src.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, file).unwrap();
        }
        fs::write(root.0.join("outside.txt"), "outside").unwrap();
        (root, src)
    }

    fn sorted(paths: Vec<PathBuf>) -> Vec<String> {
        let paths: BTreeSet<String> = paths
            .iter()
            .map(|path| normalize(path).display().to_string())
            .collect();
        paths.into_iter().collect()
    }

    fn strings(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    #[test]
    fn globs_as_python_does() {
        let (_root, src) = workspace();
        // Each result is what Python's `glob.glob(pattern, recursive=True)`
        // gives in the same tree, normalized.
        let cases: [(&str, &[&str]); 20] = [
            ("src/*", &["src/a.py", "src/sub"]),
            ("src/**", &["src", "src/a.py", "src/sub", "src/sub/b.py"]),
            ("**/*.py", &["main.py", "src/a.py", "src/sub/b.py"]),
            ("*", &["[x].txt", "data", "main.py", "src"]),
            (".*", &[".env", ".hidden"]),
            ("./main.py", &["main.py"]),
            ("src//a.py", &["src/a.py"]),
            ("nope/../main.py", &[]),
            ("[ms]*", &["main.py", "src"]),
            ("src/?.py", &["src/a.py"]),
            ("**/.*", &[".env", ".hidden", "src/.dot.py"]),
            ("src/.*", &["src/.dot.py"]),
            ("[[]x].txt", &["[x].txt"]),
            ("[!m]*", &["[x].txt", "data", "src"]),
            ("*/**/*.py", &["src/a.py", "src/sub/b.py"]),
            ("../outside.txt", &["../outside.txt"]),
            (
                "**",
                &[
                    "[x].txt",
                    "data",
                    "data/x.json",
                    "data/y.json",
                    "main.py",
                    "src",
                    "src/a.py",
                    "src/sub",
                    "src/sub/b.py",
                ],
            ),
            ("data/[x-y].json", &["data/x.json", "data/y.json"]),
            ("data/[!x].json", &["data/y.json"]),
            ("src/sub", &["src/sub"]),
        ];
        for (pattern, expected) in cases {
            assert_eq!(sorted(glob(&src, pattern)), strings(expected), "{pattern}");
        }
    }

    #[test]
    fn matches_names_as_fnmatch_does() {
        let cases = [
            ("*", "", true),
            ("*", ".env", true),
            ("*.py", "main.py", true),
            ("*.py", "main.pyc", false),
            ("m*n*.py", "main.py", true),
            ("a*b*c", "abxbc", true),
            ("a*b*c", "abxbd", false),
            ("?", "a", true),
            ("?", "", false),
            ("??", "a", false),
            ("[abc]", "b", true),
            ("[abc]", "d", false),
            ("[!abc]", "d", true),
            ("[a-c]x", "bx", true),
            ("[a-]", "-", true),
            ("[-a]", "-", true),
            ("[]a]", "]", true),
            ("[!]a]", "]", false),
            ("[z-a]", "m", false),
            ("[!z-a]", "m", true),
            ("[x", "[x", true),
            ("[!", "[!", true),
            ("a\\b", "a\\b", true),
        ];
        for (pattern, name, expected) in cases {
            assert_eq!(fnmatch(pattern, name), expected, "{pattern} {name}");
        }
        assert_eq!(fnmatch("Main.PY", "main.py"), cfg!(windows));
    }

    #[test]
    fn normalizes_paths_as_normpath_does() {
        for (path, expected) in [
            ("./main.py", "main.py"),
            ("src//a.py", "src/a.py"),
            ("src/../main.py", "main.py"),
            ("src/./sub/", "src/sub"),
            ("../x", "../x"),
            ("a/../../x", "../x"),
            ("", ""),
        ] {
            assert_eq!(normalize(Path::new(path)), Path::new(expected), "{path}");
        }
    }

    fn find(src: &Path, patterns: &[&str]) -> Result<Vec<String>, String> {
        let patterns = strings(patterns);
        let found = find_files(src, &patterns, Path::new("main.py"))?;
        Ok(found
            .iter()
            .map(|path| path.display().to_string())
            .collect())
    }

    #[test]
    fn finds_the_files_the_patterns_match() {
        let (_root, src) = workspace();
        let found = find(&src, &["main.py", "src/", "data/*.json", "!data/y.json"]);
        assert_eq!(
            found.unwrap(),
            strings(&["data/x.json", "main.py", "src/a.py"])
        );
        // `!` only removes what earlier patterns found.
        let found = find(&src, &["data/", "!data/", "data/x.json", "./main.py"]);
        assert_eq!(found.unwrap(), strings(&["data/x.json", "main.py"]));
        // Directories are left out, but a pattern that only matches one is
        // not missing.
        let found = find(&src, &["main.py", "src", "src/**"]);
        assert_eq!(
            found.unwrap(),
            strings(&["main.py", "src/a.py", "src/sub/b.py"])
        );
    }

    #[test]
    fn needs_the_entry_point_then_every_pattern() {
        let (_root, src) = workspace();
        let err = find(&src, &["src/", "missing.txt"]).unwrap_err();
        assert_eq!(
            err,
            "`files` must include the workspace's entry point, `main.py`"
        );
        let err = find(&src, &["main.py", "missing.txt", "nope/*", "!gone"]).unwrap_err();
        assert_eq!(
            err,
            "nothing matches the `files` entries `missing.txt`, `nope/*`"
        );
    }

    #[test]
    fn rejects_files_outside_the_workspace() {
        let (_root, src) = workspace();
        let err = find(&src, &["main.py", "../outside.txt"]).unwrap_err();
        assert_eq!(
            err,
            "the `files` entry `../outside.txt` matches `../outside.txt`, which is outside the \
             workspace"
        );
        let absolute = src.join("main.py").display().to_string();
        let err = find(&src, &[&absolute]).unwrap_err();
        assert!(err.contains("is an absolute path"), "{err}");
        // With `!` too: it would otherwise leave out `main.py`.
        let err = find(&src, &["main.py", "!/main.py"]).unwrap_err();
        assert_eq!(
            err,
            "the `files` entry `!/main.py` is an absolute path, but `files` are relative to the \
             workspace"
        );
        // Leaving out what is outside is harmless.
        let found = find(&src, &["main.py", "!../outside.txt"]).unwrap();
        assert_eq!(found, strings(&["main.py"]));
    }

    #[cfg(unix)]
    #[test]
    fn follows_links_without_looping() {
        use std::os::unix::fs::symlink;

        let (root, src) = workspace();
        let shared = root.0.join("shared");
        fs::create_dir(&shared).unwrap();
        fs::write(shared.join("lib.py"), "").unwrap();
        symlink(&shared, src.join("shared")).unwrap();
        symlink(&src, src.join("src/loop")).unwrap();

        let found = find(&src, &["main.py", "**/*.py"]).unwrap();
        assert_eq!(
            found,
            strings(&[
                "main.py",
                "shared/lib.py",
                "src/a.py",
                "src/loop/main.py",
                "src/sub/b.py"
            ])
        );
    }

    /// A sink that collects every event, and the events.
    fn collector() -> (PushProgress, Arc<std::sync::Mutex<Vec<PushEvent>>>) {
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = Arc::clone(&events);
        let progress = PushProgress::new(move |event: &PushEvent| {
            seen.lock().unwrap().push(event.clone());
        });
        (progress, events)
    }

    /// Pushes the Python workspace in `src` to mock servers, with the fake
    /// `uv` in `uv_dir`, through `with_uv`, and the FICO home `home`, and
    /// returns the events and the bundle that the storage received.
    async fn push_python(
        src: &Path,
        uv_dir: &Path,
        home: &Path,
        no_cache: bool,
    ) -> (Vec<PushEvent>, Vec<u8>) {
        let (uv, _) = crate::decision::bundle::tests::fake_uv_script(
            uv_dir,
            "--index-url https://pypi.org/simple\nnumpy==2.1.0\n./local\n",
        );
        let platform = |platform: Platform| platform.with_uv(&uv);
        push_python_with(src, home, no_cache, platform, &crate::uv::Source::default()).await
    }

    /// Pushes the Python workspace in `src` to mock servers with the FICO
    /// home `home` and the platform that `configure` makes, installing the
    /// SDK's `uv` from `uv_source`, and returns the events and the bundle.
    async fn push_python_with(
        src: &Path,
        home: &Path,
        no_cache: bool,
        configure: impl FnOnce(Platform) -> Platform,
        uv_source: &crate::uv::Source,
    ) -> (Vec<PushEvent>, Vec<u8>) {
        use crate::http::mock_server::{MockResponse, MockServer};

        let storage = MockServer::start(vec![MockResponse::new(200)]).await;
        let upload_url = format!("{}/bundle", storage.url());
        let platform_server = MockServer::start(vec![
            MockResponse::json(200, json!({ "upload_url": upload_url })),
            MockResponse::new(204),
        ])
        .await;
        let (progress, events) = collector();
        let client = crate::Client::builder(platform_server.url())
            .build()
            .unwrap();
        let platform = configure(
            Platform::new(client)
                .with_push_progress(progress)
                .with_home(home),
        );
        let params = WorkspacePushParams {
            src: Some(src.to_owned()),
            no_cache,
            ..Default::default()
        };
        platform
            .push_workspace_with("routing", params, uv_source, Limits::default())
            .await
            .unwrap();
        let events = std::mem::take(&mut *events.lock().unwrap());
        let bundle = storage.requests().remove(0).body;
        (events, bundle)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bundles_python_dependencies_before_the_workspaces_files() {
        let root = TempDir::new();
        let src = root.0.join("ws");
        fs::create_dir(&src).unwrap();
        let yaml = "type: python\nfiles: [main.py]\npython:\n  pip-requirements: [numpy]\n";
        fs::write(src.join("workspace.yaml"), yaml).unwrap();
        fs::write(src.join("main.py"), "print()").unwrap();
        let (uv_dir, home) = (root.0.join("uv"), root.0.join("home"));

        let (events, bundle) = push_python(&src, &uv_dir, &home, false).await;
        let size = bundle.len() as u64;
        assert_eq!(
            events,
            [
                PushEvent::PushStarted {
                    workspace_id: "routing".to_owned()
                },
                PushEvent::BundlingStarted { cache: true },
                PushEvent::DependenciesDownloading { count: 1 },
                PushEvent::DirectRequirementsBuilding { count: 1 },
                PushEvent::CopyingFiles {},
                PushEvent::Compressing {},
                PushEvent::Packaged {
                    files: 2,
                    bytes: size,
                    with_dependencies: true
                },
                PushEvent::Uploading {
                    workspace_id: "routing".to_owned()
                },
                PushEvent::Pushed {
                    workspace_id: "routing".to_owned()
                },
            ]
        );
        let out = root.0.join("out");
        archive::unpack(bundle, out.clone()).await.unwrap();
        let deps = out.join(".nextmv/python/deps");
        assert!(deps.join("numpy__2_1_0/__init__.py").is_file());
        assert!(deps.join("__local/__init__.py").is_file());
        assert_eq!(fs::read_to_string(out.join("main.py")).unwrap(), "print()");
        assert_eq!(
            fs::read_to_string(out.join("workspace.yaml")).unwrap(),
            yaml
        );
        let info = crate::cache::info(&home).await.unwrap();
        assert_eq!(info.num_dependencies, 1);

        // The second push takes numpy from the platform's home.
        let (events, _) = push_python(&src, &uv_dir, &home, false).await;
        assert!(
            events.contains(&PushEvent::DependenciesCached {
                cached: 1,
                total: 1
            }),
            "{events:?}"
        );

        // Without the cache, it downloads numpy again.
        let (events, _) = push_python(&src, &uv_dir, &home, true).await;
        assert_eq!(events[1], PushEvent::BundlingStarted { cache: false });
        assert_eq!(events[2], PushEvent::DependenciesDownloading { count: 1 });
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn installs_the_sdks_uv_on_the_first_push_only() {
        use crate::http::mock_server::{MockResponse, MockServer};
        use crate::uv::tests::{asset, tar_gz};

        let root = TempDir::new();
        let src = root.0.join("ws");
        fs::create_dir(&src).unwrap();
        let yaml = "type: python\nfiles: [main.py]\npython:\n  pip-requirements: [numpy]\n";
        fs::write(src.join("workspace.yaml"), yaml).unwrap();
        fs::write(src.join("main.py"), "print()").unwrap();
        let home = root.0.join("home");
        // The fake `uv`, served as `uv`'s release archive. It keeps its
        // lockfile next to wherever it is installed.
        let (_, script) = crate::decision::bundle::tests::fake_uv_script(
            &root.0.join("built"),
            "--index-url https://pypi.org/simple\nnumpy==2.1.0\n",
        );
        let target = "aarch64-apple-darwin";
        let archive = tar_gz(target, &script);
        let releases = MockServer::start(vec![MockResponse::new(200).body(archive.clone())]).await;
        let source = crate::uv::Source {
            base_url: releases.url().to_owned(),
            asset: Some(asset(target, "tar.gz", &archive)),
        };
        let installed = home.join("bin/uv-0.13.5/uv");
        let place_lockfile = || {
            let dir = installed.parent().unwrap();
            fs::create_dir_all(dir).unwrap();
            fs::write(
                dir.join("lockfile"),
                "--index-url https://pypi.org/simple\nnumpy==2.1.0\n",
            )
            .unwrap();
        };
        place_lockfile();

        let (events, _) = push_python_with(&src, &home, false, |p| p, &source).await;
        let installing = PushEvent::InstallingUv {
            version: "0.13.5".to_owned(),
        };
        assert_eq!(events[1], installing, "{events:?}");
        assert_eq!(events[2], PushEvent::BundlingStarted { cache: true });
        assert!(installed.is_file());
        assert_eq!(releases.requests().len(), 1);

        // The second push runs the installed `uv`, with no download.
        let (events, _) = push_python_with(&src, &home, false, |p| p, &source).await;
        assert!(!events.contains(&installing), "{events:?}");
        assert_eq!(releases.requests().len(), 1);
    }

    #[test]
    fn converts_the_manifest_into_requirements() {
        let manifest: Manifest = serde_json::from_value(json!({
            "type": "go",
            "runtime": "ghcr.io/nextmv-io/runtime/default:latest",
            "files": ["main"],
        }))
        .unwrap();
        assert_eq!(
            requirements(&manifest),
            json!({
                "executable_type": "go",
                "runtime": "ghcr.io/nextmv-io/runtime/default:latest",
                "io_configuration": { "format": "json" },
            })
        );
    }
}
