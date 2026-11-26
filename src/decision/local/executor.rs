//! Running a local workspace and finalizing its run.
//!
//! A run is attached by default: [`start`] copies the files the manifest
//! lists into a temporary directory, moves the run to `running` and starts
//! the workspace there, and a task on the caller's tokio runtime waits for
//! it, collects what it wrote and [`finalize`]s the run. A host that exits
//! before its runs end registers an executor with [`Local::with_executor`]:
//! [`start`] then starts the executor, detached, with the run on its stdin,
//! and the executor calls [`Local::execute_run`], which does the same in
//! its own process.
//!
//! Every child starts in a new process group and never inherits stdout,
//! which is the MCP stdio channel. With the `json` content format, the
//! workspace's stdout is its output, in `outputs/solutions/solution.json`;
//! with `multi-file`, it goes to the logs. Its stderr always goes to
//! `logs/logs.log`. The run directory follows `run_dir.rs`, plus
//! `exit.json`, where the executor records the workspace's exit.

use std::collections::{BTreeMap, HashSet};
use std::ffi::OsString;
use std::fs;
use std::io::{self, ErrorKind, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::process::{Child, Command};

use super::{Local, run_dir, visuals};
use crate::decision::fs::{files_under, inside, write_atomically};
use crate::decision::manifest::{self, MANIFEST_FILE};
use crate::decision::{
    ContentFormat, Manifest, ManifestPipRequirements, ManifestType, RunStatus, id, time,
};
use crate::time::parse_rfc3339;
use crate::{Error, Result};

/// A program that executes local runs in its own process: the host's entry
/// point, registered with [`Local::with_executor`].
#[derive(Debug, Clone)]
pub(crate) struct Executor {
    /// The program to start.
    program: PathBuf,
    /// Its arguments.
    args: Vec<OsString>,
}

impl Local {
    /// Executes local runs in their own process, `program` with `args`, so
    /// that they go on after this process exits. Does no I/O beyond reading
    /// the current directory.
    ///
    /// A relative `program` with a directory, such as `./fico`, is taken
    /// from this process's current directory now, since the executor starts
    /// in the workspace's directory. A bare name, such as `fico`, is looked
    /// up in `PATH`. A host that runs itself passes
    /// [`std::env::current_exe`].
    ///
    /// Without an executor, a local run is attached: a task on the caller's
    /// tokio runtime waits for the workspace's process and finalizes the
    /// run, so this process and its runtime must stay alive until the run
    /// ends, or the run stays `running`. A host that exits sooner, such as
    /// a CLI, registers an entry point of its own here, e.g. a hidden
    /// subcommand. [`Local::create_run`] then starts `program` with `args`
    /// in a new process group, writes the run to its stdin and returns.
    ///
    /// The entry point must pass all of its stdin to [`Local::execute_run`].
    /// It gets no stdout, and its stderr goes to the run's logs. Its
    /// environment is this process's, with `UV_SYSTEM_CERTS=true` unless it
    /// is set, `PYTHONUNBUFFERED=1` and `PYTHONIOENCODING=utf-8`.
    pub fn with_executor<I, S>(mut self, program: impl Into<PathBuf>, args: I) -> Local
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        let program: PathBuf = program.into();
        let has_dir = program
            .parent()
            .is_some_and(|dir| !dir.as_os_str().is_empty());
        let program = match has_dir && program.is_relative() {
            true => std::path::absolute(&program).unwrap_or(program),
            false => program,
        };
        self.executor = Some(Executor {
            program,
            args: args.into_iter().map(Into::into).collect(),
        });
        self
    }

    /// Executes the local run that [`Local::create_run`] passed to the
    /// executor registered with [`Local::with_executor`]: runs the workspace,
    /// waits for it and finalizes the run.
    ///
    /// `payload` is everything the executor read on its stdin.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] if `payload` is not a local run, or the run
    /// is not `queued`, or the workspace's files are not as its manifest
    /// says; [`Error::Io`] if the run directory cannot be read or written,
    /// or the workspace cannot be started; and [`Error::Command`] if the
    /// workspace exits with a non-zero code. The run records the outcome
    /// either way, with the reason for a failure in its logs.
    pub async fn execute_run(&self, payload: &str) -> Result<()> {
        let payload: Payload = serde_json::from_str(payload).map_err(|e| Error::Invalid {
            reason: format!("the executor's input is not a local run: {e}"),
        })?;
        if !payload.src.is_absolute() {
            return Err(Error::Invalid {
                reason: format!(
                    "the executor's input has a relative workspace directory, `{}`",
                    payload.src.display()
                ),
            });
        }
        let run = Run::open(&payload.src, &payload.run_id).await?;
        let uv = match (payload.uv, payload.manifest.manifest_type) {
            (Some(uv), _) => Some(uv.into_os_string()),
            (None, ManifestType::Python) => Some(run.or_fail(self.uv().await).await?),
            (None, _) => None,
        };
        let launched = run
            .launch(&payload.manifest, &payload.options, uv.as_deref())
            .await;
        let running = run.or_fail(launched).await?;
        supervise(running).await
    }
}

/// Starts the local run `run_id` of the workspace in `src`, whose directory
/// and input the caller already wrote, with the workspace's `manifest` and
/// the run's `options`.
///
/// Attached, the default, it copies the workspace, moves the run's status
/// to `running`, starts the workspace and returns, and a task on the
/// current tokio runtime waits for the workspace and finalizes the run.
/// With an executor registered on `local` ([`Local::with_executor`]), it
/// starts the executor and returns, and the executor does all of that in
/// its own process.
///
/// Once polled, it goes on in a task of its own even if the caller stops
/// waiting, e.g. for a cancelled MCP call, so the run is either started or
/// recorded as `failed`.
///
/// # Errors
///
/// Returns [`Error::Invalid`] if the run is not `queued`, or the workspace's
/// files are not as the manifest says, and [`Error::Io`] if the run
/// directory cannot be read or written, or a program cannot be started. A
/// run that fails to start is recorded as `failed`, with the reason in its
/// logs.
pub(crate) async fn start(
    local: &Local,
    src: &Path,
    run_id: &str,
    manifest: &Manifest,
    options: &BTreeMap<String, String>,
) -> Result<()> {
    let (src, run_id) = (src.to_path_buf(), run_id.to_owned());
    let executor = local.executor.clone();
    let (manifest, options) = (manifest.clone(), options.clone());
    let path = src.clone();
    let local = local.clone();
    // Dropping the handle detaches the task instead of aborting it.
    let starting = tokio::spawn(async move {
        let run = Run::open(&src, &run_id).await?;
        // Only Python workspaces need `uv`, which may be installed now.
        let uv = match manifest.manifest_type {
            ManifestType::Python => Some(run.or_fail(local.uv().await).await?),
            _ => None,
        };
        if let Some(executor) = &executor {
            let uv = uv.map(PathBuf::from);
            let started = spawn_executor(executor, &run, &manifest, &options, uv).await;
            return run.or_fail(started).await;
        }
        let launched = run.launch(&manifest, &options, uv.as_deref()).await;
        let running = run.or_fail(launched).await?;
        tokio::spawn(async move {
            // The run records the outcome: there is no caller to return it to.
            let _ = supervise(running).await;
        });
        Ok(())
    });
    match starting.await {
        Ok(result) => result,
        Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
        Err(error) => Err(Error::Io {
            path,
            source: io::Error::other(error),
        }),
    }
}

/// Finalizes the local run `run_id` of the workspace in `src` once its
/// workspace has exited: writes its output, metrics and assets, its
/// visuals, its output size and durations, and then its status, `succeeded`
/// or `failed`.
///
/// Visuals are best effort: a run whose assets cannot be rendered still
/// ends, and [`Local::get_run_visuals`] reports the error.
///
/// It works only from the run directory, where the executor records the
/// workspace's exit, so it can run any number of times, on the first read
/// of a run too, with the same result. A run whose workspace has not
/// exited, or that is already final, is left as it is.
///
/// # Errors
///
/// Returns [`Error::Invalid`] if `run_id` is not a local run ID or its
/// files are malformed, and [`Error::Io`] if the run directory cannot be
/// read or written.
pub(crate) async fn finalize(src: &Path, run_id: &str) -> Result<()> {
    let dir = run_dir::run_dir(src, run_id)?;
    let info_file = run_dir::information_file(&dir, run_id);
    let mut info = read_information(&info_file).await?;
    if status(&info).is_terminal() {
        return Ok(());
    }
    let Some(exit) = read_exit(&dir).await? else {
        return Ok(());
    };
    if output_format(&info) == ContentFormat::Json {
        read_stdout_output(&dir).await?;
    }
    // Before the status, so that a final run has its visuals.
    let _ = visuals::render(src, run_id).await;
    let solutions = run_dir::solutions_dir(&dir);
    let output_size = blocking(solutions.clone(), move || files_size(&solutions)).await?;
    let metadata = metadata_of(&mut info, &info_file)?;
    let created_at = metadata.get("created_at").and_then(Value::as_str);
    let duration = created_at
        .and_then(|created_at| duration_ms(created_at, &exit.ended_at))
        .unwrap_or_default();
    let (status, error) = match exit.exit_code {
        Some(0) => (RunStatus::Succeeded, ""),
        _ => (RunStatus::Failed, FAILED),
    };
    metadata.insert("duration".to_owned(), json!(duration));
    metadata.insert("execution_duration".to_owned(), json!(duration));
    metadata.insert("output_size".to_owned(), json!(output_size));
    metadata.insert("error".to_owned(), json!(error));
    metadata.insert("status_v2".to_owned(), json!(status));
    write_information(&info_file, &info).await
}

/// File, in a run's directory, where the executor records how the
/// workspace exited. [`finalize`] reads it, not the process, so that it can
/// run again, or on a read, with the same result.
const EXIT_FILE: &str = "exit.json";

/// File, in a run's `outputs/solutions/`, that holds the output of a
/// workspace with the `json` content format: what it wrote to stdout.
const SOLUTION_FILE: &str = "solution.json";

/// The error of a failed run.
const FAILED: &str = "Run failed, please check logs for details.";

/// How a run's workspace exited, in [`EXIT_FILE`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Exit {
    /// Its exit code, or `None` if a signal ended it.
    exit_code: Option<i32>,
    /// When it exited, in RFC 3339.
    ended_at: String,
}

/// The [`Exit`] recorded in the run directory `dir`, or `None` if its
/// workspace has not exited.
async fn read_exit(dir: &Path) -> Result<Option<Exit>> {
    let path = dir.join(EXIT_FILE);
    let bytes = match tokio::fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(source) if source.kind() == ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(Error::Io { path, source }),
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|e| Error::Invalid {
            reason: format!("`{}` is not a workspace's exit: {e}", path.display()),
        })
}

/// The local run that [`start`] passes to the executor, on its stdin.
#[derive(Debug, Serialize, Deserialize)]
struct Payload {
    /// The workspace's directory, absolute.
    src: PathBuf,
    /// The run's ID.
    run_id: String,
    /// The workspace's manifest.
    manifest: Manifest,
    /// The run's options.
    options: BTreeMap<String, String>,
    /// The `uv` binary that runs a Python workspace, as the host resolved
    /// it, so that the executor neither looks for it nor installs it again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    uv: Option<PathBuf>,
}

/// A local run that the executor works on.
#[derive(Debug, Clone)]
struct Run {
    /// The workspace's directory, absolute.
    src: PathBuf,
    /// The run's ID.
    id: String,
    /// The run's directory.
    dir: PathBuf,
}

/// A run whose workspace is running.
struct Running {
    run: Run,
    /// The workspace's process.
    child: Child,
    /// The program that runs the workspace, for errors.
    program: String,
    /// The temporary directory that holds the copy of the workspace.
    temp: PathBuf,
    /// The copy of the workspace, where it runs.
    copy: PathBuf,
    /// The workspace's manifest.
    manifest: Manifest,
    /// The workspace's files that the copy holds, relative to it.
    files: Vec<PathBuf>,
}

impl Run {
    /// The local run `run_id` of the workspace in `src`, which must be
    /// `queued`.
    async fn open(src: &Path, run_id: &str) -> Result<Run> {
        let src = std::path::absolute(src).map_err(io_error(src))?;
        let dir = run_dir::run_dir(&src, run_id)?;
        let run = Run {
            src,
            id: run_id.to_owned(),
            dir,
        };
        let status = status(&read_information(&run.info_file()).await?);
        if status != RunStatus::Queued {
            return Err(Error::Invalid {
                reason: format!(
                    "local run `{run_id}` is `{}`, not `queued`",
                    json!(status).as_str().unwrap_or_default()
                ),
            });
        }
        Ok(run)
    }

    fn info_file(&self) -> PathBuf {
        run_dir::information_file(&self.dir, &self.id)
    }

    fn logs_file(&self) -> PathBuf {
        run_dir::logs_file(&self.dir)
    }

    /// `result`, after recording the run as failed if it is an error.
    async fn or_fail<T>(&self, result: Result<T>) -> Result<T> {
        if let Err(error) = &result {
            self.fail(error).await;
        }
        result
    }

    /// Records that the run failed with `error` before it was finalized:
    /// adds the error to the run's logs and sets its status to `failed`. A
    /// run that is already final, e.g. finalized by a read in the meantime,
    /// is left as it is. Errors on the way are ignored, as there is nowhere
    /// left to report them.
    async fn fail(&self, error: &Error) {
        let path = self.info_file();
        let info = read_information(&path).await;
        if info.as_ref().is_ok_and(|info| status(info).is_terminal()) {
            return;
        }
        let mut message = format!("\nError during run execution: {error}");
        let mut source = std::error::Error::source(error);
        while let Some(cause) = source {
            message.push_str(&format!(": {cause}"));
            source = cause.source();
        }
        message.push('\n');
        if let Ok(mut logs) = open_logs(&self.logs_file()).await {
            let _ = logs.write_all(message.as_bytes());
        }
        let Ok(mut info) = info else {
            return;
        };
        let Ok(metadata) = metadata_of(&mut info, &path) else {
            return;
        };
        metadata.insert("status_v2".to_owned(), json!(RunStatus::Failed));
        metadata.insert("error".to_owned(), json!(FAILED));
        let _ = write_information(&path, &info).await;
    }

    /// Copies the workspace into a new temporary directory, moves the run
    /// to `running` and starts the workspace in the copy, with `options`.
    /// The temporary directory is removed if that fails.
    async fn launch(
        &self,
        manifest: &Manifest,
        options: &BTreeMap<String, String>,
        uv: Option<&std::ffi::OsStr>,
    ) -> Result<Running> {
        let temp = std::env::temp_dir().join(id::safe_id(&format!("fico-{}", self.id)));
        // Only this user can read the copy, even in a shared `/tmp`, and a
        // directory that already exists is an error, so a run only ever
        // removes what it created.
        let mut dir = tokio::fs::DirBuilder::new();
        #[cfg(unix)]
        dir.mode(0o700);
        dir.create(&temp).await.map_err(io_error(&temp))?;
        let running = self.launch_in(&temp, manifest, options, uv).await;
        if running.is_err() {
            let _ = tokio::fs::remove_dir_all(&temp).await;
        }
        running
    }

    /// [`Run::launch`], with the copy in `temp`.
    async fn launch_in(
        &self,
        temp: &Path,
        manifest: &Manifest,
        options: &BTreeMap<String, String>,
        uv: Option<&std::ffi::OsStr>,
    ) -> Result<Running> {
        let format = manifest.configuration.content.format;
        if format == ContentFormat::Unknown {
            return Err(Error::Invalid {
                reason: "a manifest's content format must be `json` or `multi-file`".to_owned(),
            });
        }
        let info_file = self.info_file();
        let mut info = read_information(&info_file).await?;
        let input_format = input_format(&info, &info_file)?;

        let (files, missing) = manifest::find_files(&self.src, &manifest.files).await?;
        manifest::check_mandatory_files(manifest, &files)?;
        if !missing.is_empty() {
            return Err(Error::Invalid {
                reason: format!(
                    "could not find the files that the manifest lists: {}",
                    missing
                        .iter()
                        .map(|file| format!("`{file}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            });
        }
        let yaml = serde_saphyr::to_string(manifest).map_err(|e| Error::Invalid {
            reason: format!("could not write the manifest: {e}"),
        })?;
        let copy = temp.join("src");
        let (src, to, listed) = (self.src.clone(), copy.clone(), files.clone());
        blocking(copy.clone(), move || {
            copy_workspace(&src, &to, &yaml, &listed)
        })
        .await?;

        let stdin = match input_format {
            ContentFormat::MultiFile => {
                let (from, to) = (run_dir::inputs_dir(&self.dir), input_dir(manifest, &copy)?);
                blocking(to.clone(), move || copy_dir(&from, &to)).await?;
                Stdio::null()
            }
            _ => {
                let input = run_dir::input_json_file(&self.dir);
                let file = tokio::fs::File::open(&input)
                    .await
                    .map_err(io_error(&input))?;
                Stdio::from(file.into_std().await)
            }
        };
        let cwd = working_dir(manifest, &copy);
        if !cwd.is_dir() {
            let execution = manifest.execution.as_ref();
            let set = execution.and_then(|execution| execution.cwd.as_deref());
            return Err(Error::Invalid {
                reason: format!(
                    "the manifest's `execution.cwd`, `{}`, is not a directory in the copy of the \
                     workspace, which only holds the files the manifest lists",
                    set.unwrap_or_default()
                ),
            });
        }
        let command = command(manifest, &self.src, &copy, options, uv).await?;
        let logs = open_logs(&self.logs_file()).await?;
        // The manifest's format decides what stdout is, whatever the input's:
        // the output of a `json` workspace, logs otherwise.
        let stdout = match format {
            ContentFormat::MultiFile => logs.try_clone().map_err(io_error(&self.logs_file()))?,
            _ => {
                let path = run_dir::solutions_dir(&self.dir).join(SOLUTION_FILE);
                tokio::fs::create_dir_all(run_dir::solutions_dir(&self.dir))
                    .await
                    .map_err(io_error(&path))?;
                let file = tokio::fs::File::create(&path)
                    .await
                    .map_err(io_error(&path))?;
                file.into_std().await
            }
        };

        let metadata = metadata_of(&mut info, &info_file)?;
        metadata.insert("status_v2".to_owned(), json!(RunStatus::Running));
        let Value::Object(formats) = metadata.entry("format").or_insert_with(|| json!({})) else {
            return Err(not_information(&info_file));
        };
        formats.insert("output".to_owned(), json!({ "type": format }));
        write_information(&info_file, &info).await?;

        // The command is never empty: it ends with the entrypoint.
        let mut command = command.into_iter();
        let program = command.next().unwrap_or_default();
        let mut child = Command::new(&program);
        child
            .args(command)
            .current_dir(cwd)
            .stdin(stdin)
            .stdout(stdout)
            .stderr(logs);
        child_env(&mut child);
        new_process_group(&mut child, false);
        let child = child.spawn().map_err(io_error(Path::new(&program)))?;
        Ok(Running {
            run: self.clone(),
            child,
            program: program.to_string_lossy().into_owned(),
            temp: temp.to_path_buf(),
            copy,
            manifest: manifest.clone(),
            files,
        })
    }
}

/// Starts `executor` for `run`, detached, with the run on its stdin, and
/// returns without waiting for it.
async fn spawn_executor(
    executor: &Executor,
    run: &Run,
    manifest: &Manifest,
    options: &BTreeMap<String, String>,
    uv: Option<PathBuf>,
) -> Result<()> {
    let payload = Payload {
        src: run.src.clone(),
        run_id: run.id.clone(),
        manifest: manifest.clone(),
        options: options.clone(),
        uv,
    };
    let payload = serde_json::to_vec(&payload).map_err(|e| Error::Invalid {
        reason: format!("could not write the run for the executor: {e}"),
    })?;
    let logs = open_logs(&run.logs_file()).await?;
    let mut command = Command::new(&executor.program);
    command
        .args(&executor.args)
        .current_dir(&run.src)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(logs);
    child_env(&mut command);
    new_process_group(&mut command, true);
    let mut child = command.spawn().map_err(io_error(&executor.program))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(&payload)
            .await
            .map_err(io_error(&executor.program))?;
    }
    // Dropping `child` neither waits for the executor nor kills it: tokio
    // reaps it once it exits.
    Ok(())
}

/// Waits for the workspace, collects what it wrote, removes the copy of the
/// workspace, records its exit and finalizes the run. A failure on the way
/// records the run as failed.
///
/// # Errors
///
/// Returns [`Error::Command`], with the end of the run's logs, if the
/// workspace exits with a non-zero code, and the error that made the run
/// fail otherwise.
async fn supervise(mut running: Running) -> Result<()> {
    let finished = finish(&mut running).await;
    // In case `finish` failed before it removed the copy.
    let _ = tokio::fs::remove_dir_all(&running.temp).await;
    let status = running.run.or_fail(finished).await?;
    if status.success() {
        return Ok(());
    }
    Err(Error::Command {
        program: running.program,
        status: status.code(),
        stderr: logs_tail(&running.run.logs_file()).await,
    })
}

/// [`supervise`], up to finalizing the run.
async fn finish(running: &mut Running) -> Result<ExitStatus> {
    let program = Path::new(&running.program).to_path_buf();
    let status = running.child.wait().await.map_err(io_error(&program))?;
    let ended_at = time::now_rfc3339();
    let dir = running.run.dir.clone();
    let (copy, manifest, files) = (
        running.copy.clone(),
        running.manifest.clone(),
        running.files.clone(),
    );
    blocking(dir.clone(), move || collect(&dir, &copy, &manifest, &files)).await?;
    // The copy is not needed any more, and is gone once the run is final.
    let _ = tokio::fs::remove_dir_all(&running.temp).await;
    let exit = Exit {
        exit_code: status.code(),
        ended_at,
    };
    write_atomically(&running.run.dir.join(EXIT_FILE), &pretty(&exit)).await?;
    finalize(&running.run.src, &running.run.id).await?;
    Ok(status)
}

/// The command line that runs the copy of the workspace in `copy` with
/// `options`: the workspace's entrypoint, behind `uv run` and its
/// requirements for Python, with the `uv` binary `uv`, or `java -jar` for
/// Java, and the options in the manifest's format. Requirements files are
/// read from the workspace in `src`.
async fn command(
    manifest: &Manifest,
    src: &Path,
    copy: &Path,
    options: &BTreeMap<String, String>,
    uv: Option<&std::ffi::OsStr>,
) -> Result<Vec<OsString>> {
    let mut command: Vec<OsString> = match manifest.manifest_type {
        ManifestType::Python => {
            let uv = uv.unwrap_or("uv".as_ref()).to_owned();
            let mut command = vec![uv, "run".into()];
            command.extend(requirements(manifest, src).await?);
            command
        }
        ManifestType::Java => ["java", "-XX:MaxRAMPercentage=90", "-jar"]
            .map(OsString::from)
            .into(),
        ManifestType::Go | ManifestType::Binary => Vec::new(),
    };
    command.push(copy.join(manifest::entrypoint(manifest)).into());
    let options = manifest::option_args(manifest, options);
    command.extend(options.into_iter().map(OsString::from));
    Ok(command)
}

/// The `uv run` arguments for a Python workspace's requirements: `--with`
/// for each package of a list or of a `pyproject.toml`, or
/// `--with-requirements` and the path of another requirements file, in the
/// workspace in `src`.
async fn requirements(manifest: &Manifest, src: &Path) -> Result<Vec<OsString>> {
    let python = manifest.python.as_ref();
    let packages = match python.and_then(|python| python.pip_requirements.as_ref()) {
        None => return Ok(Vec::new()),
        Some(ManifestPipRequirements::Packages(packages)) => packages.clone(),
        Some(ManifestPipRequirements::File(file)) if file.trim().is_empty() => {
            return Ok(Vec::new());
        }
        Some(ManifestPipRequirements::File(file)) => {
            let path = src.join(file);
            if path.file_name() != Some("pyproject.toml".as_ref()) {
                return Ok(vec!["--with-requirements".into(), path.into()]);
            }
            let pyproject = tokio::fs::read_to_string(&path)
                .await
                .map_err(io_error(&path))?;
            manifest::pyproject_dependencies(&pyproject).map_err(|reason| Error::Invalid {
                reason: format!("{reason} in `{}`", path.display()),
            })?
        }
    };
    let with = packages
        .into_iter()
        .flat_map(|package| ["--with".into(), package.into()]);
    Ok(with.collect())
}

/// The directory the workspace runs in: the manifest's `execution.cwd`,
/// taken from the copy of the workspace in `copy` if it is relative, or the
/// copy itself. A relative one never resolves against the executor's
/// current directory.
fn working_dir(manifest: &Manifest, copy: &Path) -> PathBuf {
    let cwd = manifest
        .execution
        .as_ref()
        .and_then(|execution| execution.cwd.as_deref());
    match cwd {
        Some(cwd) => copy.join(cwd),
        None => copy.to_path_buf(),
    }
}

/// Where the copy of the workspace in `copy` reads a `multi-file` input: the
/// manifest's `multi-file` input path, or `inputs`.
fn input_dir(manifest: &Manifest, copy: &Path) -> Result<PathBuf> {
    let content = &manifest.configuration.content;
    let path = match (content.format, &content.multi_file) {
        (ContentFormat::MultiFile, Some(multi_file)) => multi_file.input.path.as_str(),
        _ => "inputs",
    };
    inside(copy, path)
}

/// The format of the input of the run whose information, read from `path`,
/// is `info`: JSON unless it says otherwise.
fn input_format(info: &Map<String, Value>, path: &Path) -> Result<ContentFormat> {
    let format = info
        .get("metadata")
        .and_then(|metadata| metadata.pointer("/format/input/type"));
    let Some(format) = format else {
        return Ok(ContentFormat::Json);
    };
    ContentFormat::deserialize(format).map_err(|_| Error::Invalid {
        reason: format!(
            "`{}` has the input format {format}, not `json` or `multi-file`",
            path.display()
        ),
    })
}

/// Writes the manifest as `workspace.yaml` into `copy`, then copies `files`,
/// relative to `src`, into it, so that a `workspace.yaml` among `files`
/// replaces the written one.
fn copy_workspace(src: &Path, copy: &Path, yaml: &str, files: &[PathBuf]) -> Result<()> {
    fs::create_dir_all(copy).map_err(io_error(copy))?;
    let manifest = copy.join(MANIFEST_FILE);
    fs::write(&manifest, yaml).map_err(io_error(&manifest))?;
    for file in files {
        copy_file(&src.join(file), &copy.join(file))?;
    }
    Ok(())
}

/// Copies the files under `from` into `to`, keeping their paths.
fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to).map_err(io_error(to))?;
    for file in files_under(from)? {
        copy_file(&from.join(&file), &to.join(&file))?;
    }
    Ok(())
}

/// Copies the file `from` to `to`, creating `to`'s directory. A link is
/// copied as the file it points to, and the copy keeps the file's
/// permissions, so scripts stay executable.
fn copy_file(from: &Path, to: &Path) -> Result<()> {
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent).map_err(io_error(parent))?;
    }
    fs::copy(from, to).map(drop).map_err(io_error(from))
}

/// Copies what the workspace wrote in its copy, `copy`, into the run
/// directory `dir`.
///
/// - Metrics: the manifest's `multi-file` metrics path for that format,
///   else `outputs/metrics.json` or `outputs/metrics/metrics.json`, to
///   `outputs/metrics.json`. Assets alike.
/// - Solutions, for the `multi-file` format: the files under the manifest's
///   solutions path that the workspace wrote, to `outputs/solutions/`.
fn collect(dir: &Path, copy: &Path, manifest: &Manifest, files: &[PathBuf]) -> Result<()> {
    let content = &manifest.configuration.content;
    let multi_file = match content.format {
        ContentFormat::MultiFile => content.multi_file.as_ref(),
        _ => None,
    };
    let outputs = copy.join("outputs");
    let kinds = [
        (
            "metrics",
            multi_file.map(|multi_file| multi_file.output.metrics.as_str()),
            run_dir::metrics_file(dir),
        ),
        (
            "assets",
            multi_file.map(|multi_file| multi_file.output.assets.as_str()),
            run_dir::assets_file(dir),
        ),
    ];
    for (kind, path, to) in kinds {
        let mut candidates = Vec::new();
        if let Some(path) = path {
            candidates.push(inside(copy, path)?);
        }
        candidates.push(outputs.join(format!("{kind}.json")));
        candidates.push(outputs.join(kind).join(format!("{kind}.json")));
        if let Some(from) = candidates.into_iter().find(|candidate| candidate.is_file()) {
            copy_file(&from, &to)?;
        }
    }
    if content.format != ContentFormat::MultiFile {
        return Ok(());
    }
    let solutions = match multi_file {
        Some(multi_file) => inside(copy, &multi_file.output.solutions)?,
        None => outputs.join("solutions"),
    };
    let outputs = run_dir::outputs_dir(dir);
    let excluded = [
        outputs.join("metrics"),
        outputs.join("assets"),
        run_dir::inputs_dir(dir),
    ];
    copy_new_files(&solutions, &run_dir::solutions_dir(dir), files, &excluded)
}

/// Copies the files under `from` into `to` that the workspace wrote: not
/// one of its own `files`, nor one with the path of a file in an `excluded`
/// directory, such as the run's input, nor `workspace.yaml`, a Python cache
/// or a `.pyc` file. It only creates the directories of the files it
/// copies.
fn copy_new_files(from: &Path, to: &Path, files: &[PathBuf], excluded: &[PathBuf]) -> Result<()> {
    let mut skipped: HashSet<PathBuf> = files.iter().cloned().collect();
    for dir in excluded {
        skipped.extend(files_under(dir)?);
    }
    fs::create_dir_all(to).map_err(io_error(to))?;
    for file in files_under(from)? {
        let cache = file
            .components()
            .any(|component| component.as_os_str() == "__pycache__");
        let ignored = cache
            || file.extension() == Some("pyc".as_ref())
            || file.file_name() == Some(MANIFEST_FILE.as_ref());
        if !ignored && !skipped.contains(&file) {
            copy_file(&from.join(&file), &to.join(&file))?;
        }
    }
    Ok(())
}

/// Sets the environment of the executor and the workspace, on top of this
/// process's: `UV_SYSTEM_CERTS=true` unless it is set, so that `uv` trusts
/// the system's certificates, and unbuffered UTF-8 output for Python.
fn child_env(command: &mut Command) {
    if std::env::var_os("UV_SYSTEM_CERTS").is_none() {
        command.env("UV_SYSTEM_CERTS", "true");
    }
    command
        .env("PYTHONUNBUFFERED", "1")
        .env("PYTHONIOENCODING", "utf-8");
}

/// Starts `command` in a new process group, so that signals for this
/// process's group, such as Ctrl-C in a terminal, don't reach it. On
/// Windows, a `detached` executor gets no console, and a workspace no
/// window of its own.
fn new_process_group(command: &mut Command, detached: bool) {
    #[cfg(unix)]
    {
        let _ = detached;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let console = if detached {
            DETACHED_PROCESS
        } else {
            CREATE_NO_WINDOW
        };
        command.creation_flags(CREATE_NEW_PROCESS_GROUP | console);
    }
    #[cfg(not(any(unix, windows)))]
    let _ = (command, detached);
}

/// Opens the run's logs at `path` to add to them, creating them and their
/// directory.
async fn open_logs(path: &Path) -> Result<fs::File> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(io_error(parent))?;
    }
    let file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await
        .map_err(io_error(path))?;
    Ok(file.into_std().await)
}

/// Longest end of a run's logs, in bytes, that [`supervise`] puts in
/// [`Error::Command`].
const LOGS_TAIL: u64 = 1024;

/// The last [`LOGS_TAIL`] bytes of the run's logs at `path`, or nothing if
/// they cannot be read.
async fn logs_tail(path: &Path) -> String {
    let mut tail = Vec::new();
    if let Ok(mut file) = tokio::fs::File::open(path).await {
        let len = file.metadata().await.map(|meta| meta.len()).unwrap_or(0);
        let start = SeekFrom::Start(len.saturating_sub(LOGS_TAIL));
        if file.seek(start).await.is_ok() {
            let _ = file.read_to_end(&mut tail).await;
        }
    }
    String::from_utf8_lossy(&tail).into_owned()
}

/// Reads the output of a run with the `json` content format, which is what
/// the workspace wrote to stdout: an empty output is no output, and an
/// object's `metrics` and `assets` go to their files, unless the workspace
/// wrote those files itself.
async fn read_stdout_output(dir: &Path) -> Result<()> {
    let path = run_dir::solutions_dir(dir).join(SOLUTION_FILE);
    let stdout = match tokio::fs::read(&path).await {
        Ok(stdout) => stdout,
        Err(source) if source.kind() == ErrorKind::NotFound => return Ok(()),
        Err(source) => return Err(Error::Io { path, source }),
    };
    if stdout.trim_ascii().is_empty() {
        // A finalize running at the same time may have removed it already.
        return match tokio::fs::remove_file(&path).await {
            Err(source) if source.kind() != ErrorKind::NotFound => Err(Error::Io { path, source }),
            _ => Ok(()),
        };
    }
    let Ok(Value::Object(output)) = serde_json::from_slice(&stdout) else {
        return Ok(());
    };
    let files = [
        ("metrics", run_dir::metrics_file(dir)),
        ("assets", run_dir::assets_file(dir)),
    ];
    for (key, file) in files {
        let Some(value) = output.get(key) else {
            continue;
        };
        if tokio::fs::try_exists(&file)
            .await
            .map_err(io_error(&file))?
        {
            continue;
        }
        write_atomically(&file, &pretty(&json!({ key: value }))).await?;
    }
    Ok(())
}

/// The total size, in bytes, of the files under `dir`, not counting links.
/// A missing `dir` has none.
fn files_size(dir: &Path) -> Result<u64> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(source) if source.kind() == ErrorKind::NotFound => return Ok(0),
        Err(source) => return Err(io_error(dir)(source)),
    };
    let mut size = 0;
    for entry in entries {
        let entry = entry.map_err(io_error(dir))?;
        let path = entry.path();
        let kind = entry.file_type().map_err(io_error(&path))?;
        if kind.is_dir() {
            size += files_size(&path)?;
        } else if kind.is_file() {
            size += entry.metadata().map_err(io_error(&path))?.len();
        }
    }
    Ok(size)
}

/// Milliseconds from `created_at` to `ended_at`, both RFC 3339, to a tenth
/// of a millisecond. `None` if either is not RFC 3339.
fn duration_ms(created_at: &str, ended_at: &str) -> Option<f64> {
    let micros = parse_rfc3339(ended_at)? - parse_rfc3339(created_at)?;
    Some((micros.max(0) as f64 / 100.0).round() / 10.0)
}

/// The run information file at `path`, as a JSON object, so that writing
/// it back keeps every key.
async fn read_information(path: &Path) -> Result<Map<String, Value>> {
    let bytes = tokio::fs::read(path).await.map_err(io_error(path))?;
    match serde_json::from_slice(&bytes) {
        Ok(Value::Object(info)) => Ok(info),
        _ => Err(not_information(path)),
    }
}

/// Writes the run information `info` to `path`, atomically.
async fn write_information(path: &Path, info: &Map<String, Value>) -> Result<()> {
    write_atomically(path, &pretty(info)).await
}

/// The status of the run whose information is `info`.
fn status(info: &Map<String, Value>) -> RunStatus {
    let status = info
        .get("metadata")
        .and_then(|metadata| metadata.get("status_v2"));
    status
        .and_then(|status| RunStatus::deserialize(status).ok())
        .unwrap_or(RunStatus::Unknown)
}

/// The format of the output of the run whose information is `info`: JSON
/// unless it says otherwise.
fn output_format(info: &Map<String, Value>) -> ContentFormat {
    let format = info
        .get("metadata")
        .and_then(|metadata| metadata.pointer("/format/output/type"));
    format
        .and_then(|format| ContentFormat::deserialize(format).ok())
        .unwrap_or_default()
}

/// The `metadata` object of the run information `info`, read from `path`.
fn metadata_of<'a>(
    info: &'a mut Map<String, Value>,
    path: &Path,
) -> Result<&'a mut Map<String, Value>> {
    match info.get_mut("metadata") {
        Some(Value::Object(metadata)) => Ok(metadata),
        _ => Err(not_information(path)),
    }
}

/// The error for a run information file at `path` that is malformed.
fn not_information(path: &Path) -> Error {
    Error::Invalid {
        reason: format!("`{}` is not the information of a local run", path.display()),
    }
}

/// `value` as pretty JSON, with 2-space indents, for the run's files.
fn pretty(value: &impl Serialize) -> Vec<u8> {
    // Maps with string keys and JSON values always serialize.
    serde_json::to_vec_pretty(value).unwrap_or_default()
}

/// Runs `f` on tokio's blocking pool. A panic in `f` resumes here. `path`
/// names the error if the runtime shuts down before `f` runs.
async fn blocking<T, F>(path: PathBuf, f: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(result) => result,
        Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
        Err(error) => Err(Error::Io {
            path,
            source: io::Error::other(error),
        }),
    }
}

/// Turns an I/O error on `path` into [`Error::Io`].
fn io_error(path: &Path) -> impl FnOnce(io::Error) -> Error {
    let path = path.to_path_buf();
    move |source| Error::Io { path, source }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    /// A new directory under the system's temp directory, removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> TempDir {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("fico-sdk-executor-{}-{n}", std::process::id()));
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

    const RUN_ID: &str = "local-3k9x0q2a";

    /// The ID `local-<name>-<process ID>`, for a test that looks for the
    /// copies of its run in the shared temporary directory: the same test
    /// running at the same time in another checkout never has the same ID.
    fn own_run_id(name: &str) -> String {
        format!("local-{name}-{}", std::process::id())
    }

    /// Writes `contents` to `path`, creating its directory.
    fn write(path: &Path, contents: impl AsRef<[u8]>) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn read_json(path: &Path) -> Value {
        serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
    }

    fn dir_of(src: &Path) -> PathBuf {
        run_dir::run_dir(src, RUN_ID).unwrap()
    }

    fn info_of(src: &Path) -> Value {
        read_json(&run_dir::information_file(&dir_of(src), RUN_ID))
    }

    /// The information of the run `run_id`, as local runs write it, with
    /// `status`, the input and output `format` and `created_at`.
    fn information(run_id: &str, status: &str, format: &str, created_at: &str) -> Value {
        json!({
            "description": format!("Local run created at {created_at}"),
            "id": run_id,
            "metadata": {
                "application_id": "routing",
                "application_instance_id": "",
                "application_version_id": "",
                "created_at": created_at,
                "duration": 0.0,
                "error": "",
                "execution_class": "local",
                "execution_duration": 0.0,
                "format": { "input": { "type": format }, "output": { "type": format } },
                "input_size": 0.0,
                "output_size": 0.0,
                "status_v2": status,
                "kept": { "unknown": true },
            },
            "name": format!("local run {run_id}"),
            "user_email": "",
        })
    }

    /// Writes the information of the run [`RUN_ID`] of the workspace in
    /// `src`, with `status` and the input and output `format`.
    fn write_information_file(src: &Path, status: &str, format: &str) {
        let created_at = "2026-10-07T14:19:02.000000Z";
        let info = information(RUN_ID, status, format, created_at);
        write(
            &run_dir::information_file(&dir_of(src), RUN_ID),
            pretty(&info),
        );
    }

    fn write_exit(src: &Path, exit_code: Option<i32>) {
        let exit = Exit {
            exit_code,
            ended_at: "2026-10-07T14:19:03.234567Z".to_owned(),
        };
        write(&dir_of(src).join(EXIT_FILE), pretty(&exit));
    }

    fn solution_file(src: &Path) -> PathBuf {
        run_dir::solutions_dir(&dir_of(src)).join(SOLUTION_FILE)
    }

    /// Every file under `dir`, with its contents, sorted.
    fn snapshot(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut files = Vec::new();
        let mut dirs = vec![dir.to_path_buf()];
        while let Some(next) = dirs.pop() {
            for entry in fs::read_dir(next).unwrap() {
                let path = entry.unwrap().path();
                match path.is_dir() {
                    true => dirs.push(path),
                    false => files.push((path.clone(), fs::read(path).unwrap())),
                }
            }
        }
        files.sort();
        files
    }

    #[tokio::test]
    async fn finalizes_a_json_run_from_its_stdout() {
        let src = TempDir::new();
        write_information_file(&src.0, "running", "json");
        let output = r#"{"solution": [1, 2], "metrics": {"value": 42}, "assets": []}"#;
        write(&solution_file(&src.0), output);
        write_exit(&src.0, Some(0));
        finalize(&src.0, RUN_ID).await.unwrap();

        let info = info_of(&src.0);
        let metadata = &info["metadata"];
        assert_eq!(metadata["status_v2"], "succeeded");
        assert_eq!(metadata["error"], "");
        assert_eq!(metadata["duration"], 1234.6);
        assert_eq!(metadata["execution_duration"], 1234.6);
        assert_eq!(metadata["output_size"], output.len());
        assert_eq!(metadata["kept"], json!({ "unknown": true }));
        assert_eq!(fs::read_to_string(solution_file(&src.0)).unwrap(), output);
        let dir = dir_of(&src.0);
        let metrics = read_json(&run_dir::metrics_file(&dir));
        assert_eq!(metrics, json!({ "metrics": { "value": 42 } }));
        let assets = read_json(&run_dir::assets_file(&dir));
        assert_eq!(assets, json!({ "assets": [] }));
    }

    #[tokio::test]
    async fn finalizes_a_run_with_its_visuals() {
        let src = TempDir::new();
        write_information_file(&src.0, "running", "json");
        let visual = json!({ "schema": "plotly", "label": "cost", "type": "custom-tab" });
        let content = json!({ "data": [] });
        let output =
            json!({ "assets": [{ "name": "cost", "visual": visual, "content": content }] });
        write(&solution_file(&src.0), output.to_string());
        write_exit(&src.0, Some(0));
        finalize(&src.0, RUN_ID).await.unwrap();
        let visuals = run_dir::visuals_dir(&dir_of(&src.0));
        assert!(visuals.join("cost.html").is_file());
        assert_eq!(info_of(&src.0)["metadata"]["status_v2"], "succeeded");
    }

    #[tokio::test]
    async fn finalizes_a_run_whose_visuals_fail() {
        let src = TempDir::new();
        write_information_file(&src.0, "running", "multi-file");
        let dir = dir_of(&src.0);
        write(&run_dir::assets_file(&dir), "{ not json");
        write_exit(&src.0, Some(0));
        finalize(&src.0, RUN_ID).await.unwrap();
        assert_eq!(info_of(&src.0)["metadata"]["status_v2"], "succeeded");
        assert!(!run_dir::visuals_dir(&dir).exists());
    }

    #[tokio::test]
    async fn finalizes_the_same_way_every_time() {
        let src = TempDir::new();
        write_information_file(&src.0, "running", "json");
        write(&solution_file(&src.0), r#"{"metrics": {"value": 1}}"#);
        write_exit(&src.0, Some(1));
        finalize(&src.0, RUN_ID).await.unwrap();
        let first = snapshot(&src.0);
        assert_eq!(info_of(&src.0)["metadata"]["status_v2"], "failed");
        assert_eq!(info_of(&src.0)["metadata"]["error"], FAILED);

        finalize(&src.0, RUN_ID).await.unwrap();
        assert_eq!(snapshot(&src.0), first);
        // A final run is left as it is, whatever its exit says.
        write_exit(&src.0, Some(0));
        finalize(&src.0, RUN_ID).await.unwrap();
        assert_eq!(info_of(&src.0)["metadata"]["status_v2"], "failed");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn finalizes_at_the_same_time_as_a_read() {
        for _ in 0..100 {
            let src = TempDir::new();
            write_information_file(&src.0, "running", "json");
            // Empty stdout, as when a workspace crashes, which finalizing
            // removes.
            write(&solution_file(&src.0), "");
            write_exit(&src.0, Some(1));
            let finalizing = [src.0.clone(), src.0.clone()]
                .map(|src| tokio::spawn(async move { finalize(&src, RUN_ID).await }));
            for finalized in finalizing {
                finalized.await.unwrap().unwrap();
            }
            assert_eq!(info_of(&src.0)["metadata"]["status_v2"], "failed");
            assert!(!solution_file(&src.0).exists());
        }
    }

    #[tokio::test]
    async fn leaves_final_runs_when_recording_a_failure() {
        let src = TempDir::new();
        write_information_file(&src.0, "succeeded", "json");
        let before = snapshot(&src.0);
        let run = Run {
            src: src.0.clone(),
            id: RUN_ID.to_owned(),
            dir: dir_of(&src.0),
        };
        let error = Error::Invalid {
            reason: "a failure after the run was finalized".to_owned(),
        };
        run.fail(&error).await;
        assert_eq!(snapshot(&src.0), before);
    }

    #[tokio::test]
    async fn leaves_runs_whose_workspace_has_not_exited() {
        let src = TempDir::new();
        for status in ["queued", "running", "something-new"] {
            write_information_file(&src.0, status, "json");
            write(&solution_file(&src.0), "{}");
            let before = snapshot(&src.0);
            finalize(&src.0, RUN_ID).await.unwrap();
            assert_eq!(snapshot(&src.0), before, "{status}");
        }
    }

    #[tokio::test]
    async fn keeps_metrics_files_the_workspace_wrote() {
        let src = TempDir::new();
        write_information_file(&src.0, "running", "json");
        write(&solution_file(&src.0), r#"{"metrics": {"from": "stdout"}}"#);
        let metrics = run_dir::metrics_file(&dir_of(&src.0));
        write(&metrics, r#"{"metrics": {"from": "file"}}"#);
        write_exit(&src.0, Some(0));
        finalize(&src.0, RUN_ID).await.unwrap();
        assert_eq!(read_json(&metrics)["metrics"]["from"], "file");
        assert!(!run_dir::assets_file(&dir_of(&src.0)).exists());
    }

    #[tokio::test]
    async fn drops_empty_stdout_and_keeps_non_json_stdout() {
        let src = TempDir::new();
        write_information_file(&src.0, "running", "json");
        write(&solution_file(&src.0), " \n");
        write_exit(&src.0, Some(0));
        finalize(&src.0, RUN_ID).await.unwrap();
        assert!(!solution_file(&src.0).exists());
        assert_eq!(info_of(&src.0)["metadata"]["output_size"], 0);

        write_information_file(&src.0, "running", "json");
        write(&solution_file(&src.0), "not json, metrics");
        finalize(&src.0, RUN_ID).await.unwrap();
        let text = fs::read_to_string(solution_file(&src.0)).unwrap();
        assert_eq!(text, "not json, metrics");
        assert!(!run_dir::metrics_file(&dir_of(&src.0)).exists());
    }

    #[tokio::test]
    async fn measures_multi_file_output() {
        let src = TempDir::new();
        write_information_file(&src.0, "running", "multi-file");
        let solutions = run_dir::solutions_dir(&dir_of(&src.0));
        write(&solutions.join("a.csv"), "12345");
        write(&solutions.join("nested/b.csv"), "123");
        // Stdout is not the output of a multi-file run.
        write(&solutions.join(SOLUTION_FILE), " ");
        write_exit(&src.0, None);
        finalize(&src.0, RUN_ID).await.unwrap();
        let metadata = &info_of(&src.0)["metadata"];
        assert_eq!(metadata["output_size"], 9);
        assert_eq!(metadata["status_v2"], "failed");
    }

    #[tokio::test]
    async fn rejects_malformed_run_files() {
        let src = TempDir::new();
        assert!(matches!(
            finalize(&src.0, "../x").await.unwrap_err(),
            Error::Invalid { .. }
        ));
        assert!(matches!(
            finalize(&src.0, RUN_ID).await.unwrap_err(),
            Error::Io { .. }
        ));
        write(&run_dir::information_file(&dir_of(&src.0), RUN_ID), "[]");
        assert!(matches!(
            finalize(&src.0, RUN_ID).await.unwrap_err(),
            Error::Invalid { .. }
        ));
        write_information_file(&src.0, "running", "json");
        write(&dir_of(&src.0).join(EXIT_FILE), "{}");
        assert!(matches!(
            finalize(&src.0, RUN_ID).await.unwrap_err(),
            Error::Invalid { .. }
        ));
    }

    #[test]
    fn measures_run_durations() {
        assert_eq!(
            duration_ms("2026-10-07T14:19:02.000000Z", "2026-10-07T14:19:03.234567Z"),
            Some(1234.6)
        );
        assert_eq!(
            duration_ms("2026-10-07T14:19:03Z", "2026-10-07T14:19:02Z"),
            Some(0.0)
        );
        assert_eq!(duration_ms("now", "2026-10-07T14:19:02Z"), None);
    }

    /// A local root for the tests, which never reads its home.
    fn local() -> Local {
        Local::with_home("/nonexistent/fico-home")
    }

    fn options(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        let pairs = pairs
            .iter()
            .map(|&(name, value)| (name.to_owned(), value.to_owned()));
        pairs.collect()
    }

    /// Queues the run `run_id` of the workspace in `src`, as
    /// `Local::create_run` does: its information, with the `format` of its
    /// input, and its input files, `input.json` for JSON.
    fn queue(src: &Path, run_id: &str, format: &str, inputs: &[(&str, &str)]) {
        let dir = run_dir::run_dir(src, run_id).unwrap();
        let info = information(run_id, "queued", format, &time::now_rfc3339());
        write(&run_dir::information_file(&dir, run_id), pretty(&info));
        for (name, contents) in inputs {
            write(&run_dir::inputs_dir(&dir).join(name), contents);
        }
    }

    /// The manifest of a `binary` workspace with the content `format` and
    /// `files`, whose entrypoint is `main`.
    fn binary(format: ContentFormat, files: &[&str]) -> Manifest {
        let mut manifest = Manifest {
            manifest_type: ManifestType::Binary,
            files: files.iter().map(|&file| file.to_owned()).collect(),
            ..Default::default()
        };
        manifest.configuration.content.format = format;
        if format == ContentFormat::MultiFile {
            manifest.configuration.content.multi_file = Some(Default::default());
        }
        manifest
    }

    /// Writes `main`, an executable shell script with `body`, into `src`.
    #[cfg(unix)]
    fn script(src: &Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;

        let path = src.join("main");
        write(&path, format!("#!/bin/sh\n{body}\n"));
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// What the executor reads on its stdin to execute the run `run_id` of
    /// the workspace in `src`.
    fn payload(src: &Path, run_id: &str, manifest: &Manifest) -> String {
        let payload = Payload {
            src: src.to_path_buf(),
            run_id: run_id.to_owned(),
            manifest: manifest.clone(),
            options: BTreeMap::new(),
            uv: None,
        };
        serde_json::to_string(&payload).unwrap()
    }

    /// Waits until the run `run_id` of the workspace in `src` is final, and
    /// returns its information.
    async fn final_information(src: &Path, run_id: &str) -> Value {
        let dir = run_dir::run_dir(src, run_id).unwrap();
        let path = run_dir::information_file(&dir, run_id);
        for _ in 0..300 {
            let info = read_json(&path);
            if status(info.as_object().unwrap()).is_terminal() {
                return info;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        panic!("the run is not final: {}", read_json(&path));
    }

    /// Waits until the file at `path` exists, and returns its contents.
    async fn wait_for(path: &Path) -> String {
        for _ in 0..300 {
            if let Ok(contents) = fs::read_to_string(path) {
                return contents;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        panic!("`{}` was not written", path.display());
    }

    /// The run's logs.
    fn logs(src: &Path, run_id: &str) -> String {
        let dir = run_dir::run_dir(src, run_id).unwrap();
        fs::read_to_string(run_dir::logs_file(&dir)).unwrap_or_default()
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn runs_a_json_workspace_attached() {
        let src = TempDir::new();
        let run_id = &*own_run_id("json");
        script(
            &src.0,
            r#"input=$(cat)
printf '{"echo": %s, "args": "%s", "metrics": {"value": 42}}' "$input" "$*"
echo solving >&2"#,
        );
        queue(
            &src.0,
            run_id,
            "json",
            &[("input.json", r#"{"stops": [1, 2]}"#)],
        );
        let manifest = binary(ContentFormat::Json, &["main"]);
        let options = options(&[("duration", "10s")]);
        start(&local(), &src.0, run_id, &manifest, &options)
            .await
            .unwrap();

        let info = final_information(&src.0, run_id).await;
        let metadata = &info["metadata"];
        assert_eq!(metadata["status_v2"], "succeeded", "{info}");
        assert_eq!(metadata["error"], "");
        assert_eq!(metadata["format"]["output"]["type"], "json");
        assert!(metadata["duration"].as_f64().unwrap() > 0.0, "{info}");
        let dir = run_dir::run_dir(&src.0, run_id).unwrap();
        let solution = run_dir::solutions_dir(&dir).join(SOLUTION_FILE);
        let output = read_json(&solution);
        assert_eq!(output["echo"], json!({ "stops": [1, 2] }));
        assert_eq!(output["args"], "-duration 10s");
        assert_eq!(
            metadata["output_size"],
            fs::metadata(&solution).unwrap().len()
        );
        let metrics = read_json(&run_dir::metrics_file(&dir));
        assert_eq!(metrics, json!({ "metrics": { "value": 42 } }));
        assert_eq!(logs(&src.0, run_id), "solving\n");
        assert_eq!(read_json(&dir.join(EXIT_FILE))["exit_code"], 0);
        let copies = fs::read_dir(std::env::temp_dir())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                let name = entry.file_name();
                name.to_string_lossy()
                    .starts_with(&format!("fico-{run_id}-"))
            });
        assert_eq!(copies.count(), 0, "the copy of the workspace is left");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn runs_a_multi_file_workspace() {
        let src = TempDir::new();
        let run_id = &*own_run_id("multi");
        write(&src.0.join("data/ref.csv"), "ref\n");
        script(
            &src.0,
            r#"test -f workspace.yaml || exit 8
test -f inputs/in.csv || exit 9
mkdir -p outputs/solutions/nested outputs/solutions/data outputs/solutions/__pycache__
echo result > outputs/solutions/result.csv
echo nested > outputs/solutions/nested/n.csv
cp inputs/in.csv outputs/solutions/in.csv
cp data/ref.csv outputs/solutions/data/ref.csv
cp main outputs/solutions/main
echo cache > outputs/solutions/__pycache__/m.pyc
echo manifest > outputs/solutions/workspace.yaml
printf '{"metrics": {"m": 1}}' > outputs/metrics.json
echo on stdout
echo on stderr >&2"#,
        );
        queue(&src.0, run_id, "multi-file", &[("in.csv", "a,b\n")]);
        let manifest = binary(ContentFormat::MultiFile, &["main", "data/"]);
        start(&local(), &src.0, run_id, &manifest, &BTreeMap::new())
            .await
            .unwrap();

        let info = final_information(&src.0, run_id).await;
        let metadata = &info["metadata"];
        assert_eq!(metadata["status_v2"], "succeeded", "{info}");
        assert_eq!(metadata["format"]["output"]["type"], "multi-file");
        let dir = run_dir::run_dir(&src.0, run_id).unwrap();
        let solutions = run_dir::solutions_dir(&dir);
        // Only what the workspace wrote: not its own files, the input,
        // caches or the manifest.
        let files = files_under(&solutions).unwrap();
        let expected = [Path::new("nested/n.csv"), Path::new("result.csv")];
        assert_eq!(files, expected);
        assert_eq!(metadata["output_size"], 14);
        let metrics = read_json(&run_dir::metrics_file(&dir));
        assert_eq!(metrics, json!({ "metrics": { "m": 1 } }));
        let logs = logs(&src.0, run_id);
        assert!(
            logs.contains("on stdout\n") && logs.contains("on stderr\n"),
            "{logs}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn keeps_the_output_of_a_json_workspace_with_multi_file_input() {
        let src = TempDir::new();
        let run_id = &*own_run_id("mixed");
        script(
            &src.0,
            r#"test -f inputs/in.csv || exit 9
echo '{"solution": 1}'"#,
        );
        queue(&src.0, run_id, "multi-file", &[("in.csv", "a,b\n")]);
        let manifest = binary(ContentFormat::Json, &["main"]);
        start(&local(), &src.0, run_id, &manifest, &BTreeMap::new())
            .await
            .unwrap();

        let info = final_information(&src.0, run_id).await;
        assert_eq!(info["metadata"]["status_v2"], "succeeded", "{info}");
        assert_eq!(info["metadata"]["format"]["output"]["type"], "json");
        let dir = run_dir::run_dir(&src.0, run_id).unwrap();
        let solution = run_dir::solutions_dir(&dir).join(SOLUTION_FILE);
        assert_eq!(read_json(&solution), json!({ "solution": 1 }));
        assert_eq!(logs(&src.0, run_id), "");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn starts_runs_whose_caller_stops_waiting() {
        let src = TempDir::new();
        let run_id = &*own_run_id("cancel");
        script(&src.0, "cat > /dev/null; echo '{}'");
        queue(&src.0, run_id, "json", &[("input.json", "{}")]);
        let manifest = binary(ContentFormat::Json, &["main"]);
        let none = BTreeMap::new();
        // The caller polls `start` once and drops it, as a cancelled MCP
        // call does.
        let local = local();
        let started = start(&local, &src.0, run_id, &manifest, &none);
        let stopped = tokio::time::timeout(std::time::Duration::ZERO, started).await;
        assert!(stopped.is_err());

        let info = final_information(&src.0, run_id).await;
        assert_eq!(info["metadata"]["status_v2"], "succeeded", "{info}");
        for _ in 0..100 {
            let copies = fs::read_dir(std::env::temp_dir())
                .unwrap()
                .filter_map(|entry| entry.ok())
                .filter(|entry| {
                    let name = entry.file_name();
                    name.to_string_lossy()
                        .starts_with(&format!("fico-{run_id}-"))
                })
                .count();
            if copies == 0 {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        panic!("the copy of the workspace is left");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn keeps_the_copy_of_the_workspace_private() {
        use std::os::unix::fs::PermissionsExt;

        let src = TempDir::new();
        script(&src.0, "exit 0");
        queue(&src.0, RUN_ID, "json", &[("input.json", "{}")]);
        let run = Run::open(&src.0, RUN_ID).await.unwrap();
        let manifest = binary(ContentFormat::Json, &["main"]);
        let running = run.launch(&manifest, &BTreeMap::new(), None).await.unwrap();
        let temp = running.temp.clone();
        let mode = fs::metadata(&temp).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "{mode:o}");
        supervise(running).await.unwrap();
        assert!(!temp.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn records_a_failing_workspace() {
        let src = TempDir::new();
        let run_id = &*own_run_id("fail");
        script(&src.0, "echo boom >&2\nexit 3");
        queue(&src.0, run_id, "json", &[("input.json", "{}")]);
        let manifest = binary(ContentFormat::Json, &["main"]);
        let err = local()
            .execute_run(&payload(&src.0, run_id, &manifest))
            .await
            .unwrap_err();
        let Error::Command {
            program,
            status,
            stderr,
        } = err
        else {
            panic!("{err}");
        };
        assert!(program.ends_with("main"), "{program}");
        assert_eq!(status, Some(3));
        assert_eq!(stderr, "boom\n");

        let info = final_information(&src.0, run_id).await;
        assert_eq!(info["metadata"]["status_v2"], "failed");
        assert_eq!(info["metadata"]["error"], FAILED);
        assert_eq!(logs(&src.0, run_id), "boom\n");
        let dir = run_dir::run_dir(&src.0, run_id).unwrap();
        assert!(!run_dir::solutions_dir(&dir).join(SOLUTION_FILE).exists());
    }

    /// Asserts that the run [`RUN_ID`] of the workspace in `src` failed, with
    /// `reason` in its logs.
    fn assert_failed(src: &Path, reason: &str) {
        let metadata = &info_of(src)["metadata"];
        assert_eq!(metadata["status_v2"], "failed");
        assert_eq!(metadata["error"], FAILED);
        let logs = logs(src, RUN_ID);
        assert!(logs.contains("Error during run execution: "), "{logs}");
        assert!(logs.contains(reason), "{logs}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fails_runs_that_cannot_start() {
        let json = binary(ContentFormat::Json, &["main"]);
        let none = BTreeMap::new();

        // A file the manifest lists is missing.
        let src = TempDir::new();
        script(&src.0, "exit 0");
        queue(&src.0, RUN_ID, "json", &[("input.json", "{}")]);
        let manifest = binary(ContentFormat::Json, &["main", "missing.csv"]);
        let err = start(&local(), &src.0, RUN_ID, &manifest, &none)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Invalid { .. }), "{err}");
        assert_failed(&src.0, "`missing.csv`");

        // The entrypoint is not a program.
        let src = TempDir::new();
        write(&src.0.join("main"), "not a program");
        queue(&src.0, RUN_ID, "json", &[("input.json", "{}")]);
        let err = start(&local(), &src.0, RUN_ID, &json, &none)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Io { .. }), "{err}");
        assert_failed(&src.0, "main");

        // The input is missing.
        let src = TempDir::new();
        script(&src.0, "exit 0");
        queue(&src.0, RUN_ID, "json", &[]);
        let err = start(&local(), &src.0, RUN_ID, &json, &none)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Io { .. }), "{err}");
        assert_failed(&src.0, "input.json");

        // The manifest's input path leaves the workspace.
        let src = TempDir::new();
        script(&src.0, "exit 0");
        queue(&src.0, RUN_ID, "multi-file", &[("in.csv", "")]);
        let mut manifest = binary(ContentFormat::MultiFile, &["main"]);
        if let Some(multi_file) = &mut manifest.configuration.content.multi_file {
            multi_file.input.path = "../inputs".to_owned();
        }
        let err = start(&local(), &src.0, RUN_ID, &manifest, &none)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Invalid { .. }), "{err}");
        assert_failed(&src.0, "`../inputs`");

        // The working directory is not in the copy of the workspace.
        let src = TempDir::new();
        script(&src.0, "exit 0");
        queue(&src.0, RUN_ID, "json", &[("input.json", "{}")]);
        let mut manifest = binary(ContentFormat::Json, &["main"]);
        manifest.execution = Some(crate::decision::ManifestExecution {
            entrypoint: None,
            cwd: Some("bin".to_owned()),
        });
        let err = start(&local(), &src.0, RUN_ID, &manifest, &none)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Invalid { .. }), "{err}");
        assert_failed(&src.0, "`execution.cwd`, `bin`");

        // A run that is not queued is left as it is.
        let src = TempDir::new();
        script(&src.0, "exit 0");
        write_information_file(&src.0, "running", "json");
        let before = snapshot(&src.0);
        let err = start(&local(), &src.0, RUN_ID, &json, &none)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("is `running`, not `queued`"),
            "{err}"
        );
        assert_eq!(snapshot(&src.0), before);
    }

    /// A Python workspace in `src` whose `main.py` the fake `uv` at the
    /// returned path "runs": the script records its arguments and prints
    /// the output, so a run only succeeds if it starts this `uv`.
    #[cfg(unix)]
    fn python_with_fake_uv(src: &Path) -> (Manifest, PathBuf) {
        use std::os::unix::fs::PermissionsExt;

        write(&src.join("main.py"), "print('never run')\n");
        let uv = src.join("fake-uv");
        let body = "printf '%s\\n' \"$@\" > \"$0.args\"; echo '{\"by\": \"fake uv\"}'";
        write(&uv, format!("#!/bin/sh\n{body}\n"));
        fs::set_permissions(&uv, fs::Permissions::from_mode(0o755)).unwrap();
        let mut manifest = binary(ContentFormat::Json, &["main.py"]);
        manifest.manifest_type = ManifestType::Python;
        (manifest, uv)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn runs_python_with_the_uv_the_host_set() {
        let src = TempDir::new();
        let run_id = "local-pyuv001";
        let (manifest, uv) = python_with_fake_uv(&src.0);
        queue(&src.0, run_id, "json", &[("input.json", "{}")]);
        // The home doesn't exist: the SDK's own `uv` can't be the one.
        let local = local().with_uv(&uv);
        start(&local, &src.0, run_id, &manifest, &BTreeMap::new())
            .await
            .unwrap();
        let info = final_information(&src.0, run_id).await;
        assert_eq!(info["metadata"]["status_v2"], "succeeded", "{info}");
        let args = fs::read_to_string(uv.with_extension("args")).unwrap();
        assert_eq!(args.lines().next(), Some("run"), "{args}");
        assert!(args.contains("main.py"), "{args}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn hands_the_resolved_uv_to_a_registered_executor() {
        let src = TempDir::new();
        let run_id = "local-pyuv002";
        let (manifest, uv) = python_with_fake_uv(&src.0);
        queue(&src.0, run_id, "json", &[("input.json", "{}")]);
        let record = src.0.join("payload.json");
        let recorder = r#"cat > "$0.part"; mv "$0.part" "$0""#;
        let host = local()
            .with_uv(&uv)
            .with_executor("/bin/sh", ["-c", recorder, record.to_str().unwrap()]);
        start(&host, &src.0, run_id, &manifest, &BTreeMap::new())
            .await
            .unwrap();
        let sent = wait_for(&record).await;
        let payload: Payload = serde_json::from_str(&sent).unwrap();
        assert_eq!(payload.uv.as_deref(), Some(uv.as_path()));

        // The executor's entry point runs it with that `uv`, from a `Local`
        // that has none set.
        local().execute_run(&sent).await.unwrap();
        let info = final_information(&src.0, run_id).await;
        assert_eq!(info["metadata"]["status_v2"], "succeeded", "{info}");
        assert!(uv.with_extension("args").is_file());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn starts_a_registered_executor_detached() {
        let src = TempDir::new();
        let run_id = &*own_run_id("detach");
        script(&src.0, r#"cat > /dev/null; echo '{"done": true}'"#);
        queue(&src.0, run_id, "json", &[("input.json", "{}")]);
        let record = src.0.join("payload.json");
        let recorder = r#"cat > "$0.part"; env > "$0.env"; echo from the executor >&2
mv "$0.part" "$0""#;
        let local = local().with_executor("/bin/sh", ["-c", recorder, record.to_str().unwrap()]);
        let manifest = binary(ContentFormat::Json, &["main"]);
        let options = options(&[("duration", "10s")]);
        start(&local, &src.0, run_id, &manifest, &options)
            .await
            .unwrap();

        // The executor got the run, which nothing else started.
        let sent = wait_for(&record).await;
        let dir = run_dir::run_dir(&src.0, run_id).unwrap();
        let info = read_json(&run_dir::information_file(&dir, run_id));
        assert_eq!(info["metadata"]["status_v2"], "queued");
        let payload: Payload = serde_json::from_str(&sent).unwrap();
        assert_eq!(payload.src, src.0);
        assert_eq!(payload.run_id, run_id);
        assert_eq!(payload.manifest, manifest);
        assert_eq!(payload.options, options);
        let env = fs::read_to_string(src.0.join("payload.json.env")).unwrap();
        for var in [
            "PYTHONUNBUFFERED=1",
            "PYTHONIOENCODING=utf-8",
            "UV_SYSTEM_CERTS=",
        ] {
            assert!(env.lines().any(|line| line.starts_with(var)), "{var}");
        }
        assert_eq!(logs(&src.0, run_id), "from the executor\n");

        // The executor's entry point runs it.
        local.execute_run(&sent).await.unwrap();
        let info = final_information(&src.0, run_id).await;
        assert_eq!(info["metadata"]["status_v2"], "succeeded", "{info}");
        let solution = run_dir::solutions_dir(&dir).join(SOLUTION_FILE);
        assert_eq!(read_json(&solution), json!({ "done": true }));
    }

    #[cfg(unix)]
    #[test]
    fn takes_a_relative_executor_from_the_current_directory() {
        let program = |program: &str| {
            let local = local().with_executor(program, ["decision"]);
            local.executor.map(|executor| executor.program).unwrap()
        };
        let here = std::env::current_dir().unwrap();
        assert_eq!(program("./target/fico"), here.join("./target/fico"));
        assert_eq!(program("target/fico"), here.join("target/fico"));
        assert_eq!(program("fico"), Path::new("fico"));
        assert_eq!(program("/usr/bin/fico"), Path::new("/usr/bin/fico"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fails_runs_whose_executor_cannot_start() {
        let src = TempDir::new();
        queue(&src.0, RUN_ID, "json", &[("input.json", "{}")]);
        let local = local().with_executor("/nonexistent/fico", ["decision"]);
        let manifest = binary(ContentFormat::Json, &["main"]);
        let err = start(&local, &src.0, RUN_ID, &manifest, &BTreeMap::new())
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Io { .. }), "{err}");
        assert_failed(&src.0, "/nonexistent/fico");
    }

    #[tokio::test]
    async fn rejects_payloads_that_are_not_runs() {
        let src = TempDir::new();
        let manifest = binary(ContentFormat::Json, &["main"]);
        let cases = [
            "not json".to_owned(),
            payload(Path::new("relative"), RUN_ID, &manifest),
            payload(&src.0, "../x", &manifest),
        ];
        for case in cases {
            let err = local().execute_run(&case).await.unwrap_err();
            assert!(matches!(err, Error::Invalid { .. }), "{case}: {err}");
        }
        let missing = payload(&src.0, RUN_ID, &manifest);
        let err = local().execute_run(&missing).await.unwrap_err();
        assert!(matches!(err, Error::Io { .. }), "{err}");
    }

    /// What the workspaces and the executor of [`never_writes_to_stdout`]
    /// write on stdout and stderr.
    const MARKER: &str = "executor-stdout-marker";

    /// Runs the test binary again, for this test only, so that it can see
    /// the process's real stdout: the workspaces and the executor of
    /// [`run_workspaces_that_print`] write [`MARKER`] on theirs.
    #[cfg(unix)]
    #[test]
    fn never_writes_to_stdout() {
        const CHILD: &str = "FICO_SDK_TEST_EXECUTOR_STDOUT";
        if std::env::var_os(CHILD).is_some() {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            return runtime.block_on(run_workspaces_that_print());
        }
        let test = "decision::local::executor::tests::never_writes_to_stdout";
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--nocapture", "--test-threads=1"])
            .env(CHILD, "1")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{stdout}{stderr}");
        assert!(stdout.contains("1 passed"), "{stdout}");
        assert!(!stdout.contains(MARKER), "{stdout}");
    }

    /// Runs workspaces that write [`MARKER`] on stdout and stderr: attached,
    /// through an executor that writes it too, and through
    /// [`Local::execute_run`].
    #[cfg(unix)]
    async fn run_workspaces_that_print() {
        let src = TempDir::new();
        script(&src.0, &format!("echo {MARKER}\necho {MARKER} >&2"));
        let manifest = binary(ContentFormat::Json, &["main"]);
        let none = BTreeMap::new();
        let [attached, detached, executed] = ["local-stdout01", "local-stdout02", "local-stdout03"];
        for run_id in [attached, detached, executed] {
            queue(&src.0, run_id, "json", &[("input.json", "{}")]);
        }
        start(&local(), &src.0, attached, &manifest, &none)
            .await
            .unwrap();
        final_information(&src.0, attached).await;

        let done = src.0.join("executor-done");
        let executor = format!("cat > /dev/null; echo {MARKER}; touch \"$0\"");
        let args = ["-c", executor.as_str(), done.to_str().unwrap()];
        let local_with_executor = local().with_executor("/bin/sh", args);
        start(&local_with_executor, &src.0, detached, &manifest, &none)
            .await
            .unwrap();
        wait_for(&done).await;

        local()
            .execute_run(&payload(&src.0, executed, &manifest))
            .await
            .unwrap();
        for run_id in [attached, executed] {
            let dir = run_dir::run_dir(&src.0, run_id).unwrap();
            let solution = run_dir::solutions_dir(&dir).join(SOLUTION_FILE);
            assert_eq!(fs::read_to_string(solution).unwrap(), format!("{MARKER}\n"));
            assert_eq!(logs(&src.0, run_id), format!("{MARKER}\n"));
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn builds_the_command_of_each_workspace_type() {
        use crate::decision::{ManifestExecution, ManifestOptions, ManifestPython};

        let src = TempDir::new();
        let copy = Path::new("/tmp/copy");
        let options = options(&[("duration", "10")]);
        let built = async |manifest: &Manifest| -> Result<Vec<String>> {
            let uv = Some("/fico/bin/uv-0.13.5/uv".as_ref());
            let command = command(manifest, &src.0, copy, &options, uv).await?;
            let args = command.iter().map(|arg| arg.to_string_lossy().into_owned());
            Ok(args.collect())
        };
        let python = |requirements: Option<ManifestPipRequirements>| Manifest {
            python: Some(ManifestPython {
                pip_requirements: requirements,
                ..Default::default()
            }),
            ..Default::default()
        };
        let entry = ["/tmp/copy/./main.py", "-duration", "10"];
        let with = |args: &[&str]| -> Vec<String> {
            let args = ["/fico/bin/uv-0.13.5/uv", "run"]
                .iter()
                .chain(args)
                .chain(&entry);
            args.map(|&arg| arg.to_owned()).collect()
        };

        let packages =
            ManifestPipRequirements::Packages(vec!["ortools==9.12".into(), "pandas".into()]);
        let expected = with(&["--with", "ortools==9.12", "--with", "pandas"]);
        assert_eq!(built(&python(Some(packages))).await.unwrap(), expected);
        let file = ManifestPipRequirements::File("requirements.txt".into());
        let requirements = src.0.join("requirements.txt");
        let expected = with(&["--with-requirements", requirements.to_str().unwrap()]);
        assert_eq!(built(&python(Some(file))).await.unwrap(), expected);
        for none in [
            None,
            Some(ManifestPipRequirements::File(String::new())),
            Some(ManifestPipRequirements::Packages(Vec::new())),
        ] {
            assert_eq!(built(&python(none)).await.unwrap(), with(&[]));
        }

        // `uv run` doesn't take a `pyproject.toml` as requirements, so its
        // dependencies are passed one by one.
        let pyproject = ManifestPipRequirements::File("pyproject.toml".into());
        let err = built(&python(Some(pyproject.clone()))).await.unwrap_err();
        assert!(matches!(err, Error::Io { .. }), "{err}");
        write(&src.0.join("pyproject.toml"), "[project]\nname = \"x\"\n");
        let err = built(&python(Some(pyproject.clone()))).await.unwrap_err();
        assert!(err.to_string().contains("not found in `"), "{err}");
        let deps = "[project]\ndependencies = [\"a>=1\", 'b']\n";
        write(&src.0.join("pyproject.toml"), deps);
        let expected = with(&["--with", "a>=1", "--with", "b"]);
        assert_eq!(built(&python(Some(pyproject))).await.unwrap(), expected);

        let java = Manifest {
            manifest_type: ManifestType::Java,
            ..Default::default()
        };
        let expected = [
            "java",
            "-XX:MaxRAMPercentage=90",
            "-jar",
            "/tmp/copy/./main.jar",
        ];
        assert_eq!(built(&java).await.unwrap()[..4], expected);

        let mut binary = Manifest {
            manifest_type: ManifestType::Binary,
            execution: Some(ManifestExecution {
                entrypoint: Some("bin/solve".to_owned()),
                cwd: Some("./bin".to_owned()),
            }),
            ..Default::default()
        };
        binary.configuration.options = Some(ManifestOptions {
            format: Some(vec!["--{{name}}={{value}}".to_owned()]),
            ..Default::default()
        });
        let expected = ["/tmp/copy/bin/solve", "--duration=10"];
        assert_eq!(built(&binary).await.unwrap(), expected);
        assert_eq!(working_dir(&binary, copy), Path::new("/tmp/copy/./bin"));
        if let Some(execution) = &mut binary.execution {
            execution.cwd = Some("/work".to_owned());
        }
        assert_eq!(working_dir(&binary, copy), Path::new("/work"));
        assert_eq!(working_dir(&java, copy), copy);
    }

    #[test]
    fn passes_the_manifest_on_unchanged() {
        let templates = [
            include_str!("../manifest/templates/python_multi-file_opt.yaml"),
            include_str!("../manifest/templates/java_json_opt.yaml"),
        ];
        for template in templates {
            let manifest: Manifest = serde_saphyr::from_str(template).unwrap();
            let payload = Payload {
                src: PathBuf::from("/workspaces/routing"),
                run_id: RUN_ID.to_owned(),
                manifest: manifest.clone(),
                options: options(&[("duration", "10")]),
                uv: Some(PathBuf::from("/fico/bin/uv-0.13.5/uv")),
            };
            let sent = serde_json::to_string(&payload).unwrap();
            let received: Payload = serde_json::from_str(&sent).unwrap();
            assert_eq!(received.manifest, manifest);
            assert_eq!(received.uv, payload.uv);
            // And as the copy's `workspace.yaml`.
            let yaml = serde_saphyr::to_string(&manifest).unwrap();
            assert_eq!(serde_saphyr::from_str::<Manifest>(&yaml).unwrap(), manifest);
        }
    }
}
