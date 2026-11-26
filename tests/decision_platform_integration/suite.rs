//! What the stages share: the platform, the suite's workspace and fixture,
//! its inputs, and the checks of a run's input and output, which depend on
//! the content format.

use std::collections::BTreeMap;
use std::fs;
use std::future::Future;
use std::hash::{BuildHasher, Hasher, RandomState};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::Instant;

use fico_sdk::config::{Config, Env};
use fico_sdk::decision::platform::{
    DataUploadParams, Platform, PushEvent, PushProgress, RunCloneParams, RunCreateParams,
    RunGetParams, RunInputGetParams, RunLogsGetParams,
};
use fico_sdk::decision::{ContentFormat, PollingOptions, RunResult, RunStatus};
use flate2::Compression;
use flate2::write::GzEncoder;
use serde_json::{Value, json};

use crate::support::TempDir;

/// The content format of a suite's workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Content {
    /// `python-hello-world`: JSON on stdin and stdout.
    Json,
    /// `python-ortools-multiknapsack-multiexcel`: files in `inputs/` and
    /// `outputs/`.
    MultiFile,
}

/// A run's input: JSON, or the files of a directory.
#[derive(Debug, Clone)]
pub enum Input {
    /// A JSON object.
    Json(Value),
    /// The files directly in a directory.
    Dir(PathBuf),
}

impl Input {
    /// Parameters that start a run on this input.
    pub fn create(&self) -> RunCreateParams {
        match self {
            Input::Json(input) => RunCreateParams {
                input: Some(input.clone()),
                ..Default::default()
            },
            Input::Dir(dir) => RunCreateParams {
                input_dir: Some(dir.clone()),
                ..Default::default()
            },
        }
    }

    /// Parameters that clone a run with this input.
    pub fn clone_with(&self) -> RunCloneParams {
        match self {
            Input::Json(input) => RunCloneParams {
                input: Some(input.clone()),
                ..Default::default()
            },
            Input::Dir(dir) => RunCloneParams {
                input_dir: Some(dir.clone()),
                ..Default::default()
            },
        }
    }
}

/// A suite: the platform, the workspace it pushes and runs, and the
/// fixture it pushes.
pub struct Suite {
    /// `json` or `multi-file`, at the start of each line the suite prints.
    pub label: &'static str,
    pub content: Content,
    pub platform: Platform,
    pub workspace_id: String,
    /// The workspace's directory, in `tests/decision_platform_integration/fixtures`.
    pub fixture: PathBuf,
    /// What push reported, since the last [`Suite::take_events`].
    events: Arc<Mutex<Vec<PushEvent>>>,
    /// Files the stages write and download.
    scratch: TempDir,
    /// The FICO home of the platform, for push's dependency cache.
    _home: TempDir,
}

impl Suite {
    /// A suite for `content`, with its own FICO home and workspace ID.
    ///
    /// Panics when `FICO_API_KEY` is not set: a suite that cannot reach the
    /// platform fails rather than passing without running.
    pub async fn new(content: Content) -> Suite {
        let (label, fixture) = match content {
            Content::Json => ("json", "python-hello-world"),
            Content::MultiFile => ("multi-file", "python-ortools-multiknapsack-multiexcel"),
        };
        let home = TempDir::new(&format!("integration-{label}-home"));
        let mut env = Env::from_process();
        // The suite's home has no profiles: only the key and the endpoint
        // count (the default endpoint, unless `FICO_ENDPOINT` is set), so a `FICO_PROFILE` set for the CLI does not break it.
        env.profile = None;
        let config = Config::load(home.path()).await.unwrap();
        let resolved = config
            .resolve(None, &env)
            .unwrap()
            .expect("set FICO_API_KEY to run the integration suites");
        let client = resolved.client_builder(home.path()).build().unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = events.clone();
        let started = Instant::now();
        let platform = Platform::new(client)
            .with_home(home.path())
            .with_push_progress(PushProgress::new(move |event| {
                let seconds = started.elapsed().as_secs_f64();
                eprintln!("[{label}] push: {seconds:.1} s: {}", event_name(event));
                sink.lock().unwrap().push(event.clone());
            }));
        Suite {
            label,
            content,
            platform,
            workspace_id: format!("integration-{label}-{}", random_suffix()),
            fixture: Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/decision_platform_integration/fixtures")
                .join(fixture),
            events,
            scratch: TempDir::new(&format!("integration-{label}")),
            _home: home,
        }
    }

    /// Prints `message`, after the suite's label.
    pub fn log(&self, message: &str) {
        eprintln!("[{}] {message}", self.label);
    }

    /// Runs `stage`, named `name`, and prints how long it took. The stage
    /// is boxed, so the futures that join stages stay small.
    pub async fn stage<T>(&self, name: &str, stage: impl Future<Output = T>) -> T {
        let started = Instant::now();
        self.log(&format!("{name}: started"));
        let output = Box::pin(stage).await;
        let seconds = started.elapsed().as_secs_f64();
        self.log(&format!("{name}: done in {seconds:.1} s"));
        output
    }

    /// The push events reported since the last call.
    pub fn take_events(&self) -> Vec<PushEvent> {
        std::mem::take(&mut *self.events.lock().unwrap())
    }

    /// A new empty directory for the stage's files.
    pub fn dir(&self, name: &str) -> PathBuf {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = self.scratch.path().join(format!("{name}-{n}"));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The fixture's input: its `input.json`, or its `inputs` directory.
    pub fn input(&self) -> Input {
        match self.content {
            Content::Json => {
                let input = fs::read(self.fixture.join("input.json")).unwrap();
                Input::Json(serde_json::from_slice(&input).unwrap())
            }
            Content::MultiFile => Input::Dir(self.fixture.join("inputs")),
        }
    }

    /// Another input, for a clone that replaces the cloned run's: another
    /// JSON object for JSON, and the fixture's files and a `notes.txt` for
    /// multi-file.
    pub fn other_input(&self) -> Input {
        match self.content {
            Content::Json => {
                Input::Json(json!({ "name": "cloned", "radius": 1000, "distance": 50.0 }))
            }
            Content::MultiFile => {
                let dir = self.dir("other-input");
                fs::copy(
                    self.fixture.join("inputs/input.xlsx"),
                    dir.join("input.xlsx"),
                )
                .unwrap();
                fs::write(dir.join("notes.txt"), "cloned\n").unwrap();
                Input::Dir(dir)
            }
        }
    }

    /// A file whose contents are the fixture's input: `input.json`, or a
    /// tar.gz of `inputs/`.
    pub fn input_file(&self) -> PathBuf {
        match self.content {
            Content::Json => self.fixture.join("input.json"),
            Content::MultiFile => {
                let file = self.dir("input-file").join("input.tar.gz");
                let mut archive =
                    tar::Builder::new(GzEncoder::new(Vec::new(), Compression::default()));
                for entry in fs::read_dir(self.fixture.join("inputs")).unwrap() {
                    let entry = entry.unwrap();
                    archive
                        .append_path_with_name(entry.path(), entry.file_name())
                        .unwrap();
                }
                fs::write(&file, archive.into_inner().unwrap().finish().unwrap()).unwrap();
                file
            }
        }
    }

    /// Parameters that upload the fixture's input to `upload_url`.
    pub fn upload(&self, upload_url: String) -> DataUploadParams {
        match self.input() {
            Input::Json(data) => DataUploadParams {
                upload_url,
                data: Some(data),
                ..Default::default()
            },
            Input::Dir(dir) => DataUploadParams {
                upload_url,
                data_dir: Some(dir),
                ..Default::default()
            },
        }
    }

    /// The format of the workspace's input, for a managed input: `None`,
    /// the default, for JSON.
    pub fn content_format(&self) -> Option<ContentFormat> {
        match self.content {
            Content::Json => None,
            Content::MultiFile => Some(ContentFormat::MultiFile),
        }
    }

    /// An option of the workspace, and a value other than its default:
    /// `details=false` for JSON, and `duration=10` for multi-file.
    pub fn option(&self) -> (&'static str, &'static str) {
        match self.content {
            Content::Json => ("details", "false"),
            Content::MultiFile => ("duration", "10"),
        }
    }

    /// The JSON path of the `value` metric in a run's metrics: JSON output
    /// carries its metrics as they are, and the multi-file fixture writes
    /// `outputs/metrics.json` as `{"metrics": {…}}`.
    pub fn metrics_path(&self) -> &'static str {
        match self.content {
            Content::Json => "$.value",
            Content::MultiFile => "$.metrics.value",
        }
    }

    /// The option of [`Suite::option`], as run options.
    pub fn options(&self) -> BTreeMap<String, String> {
        let (name, value) = self.option();
        BTreeMap::from([(name.to_owned(), value.to_owned())])
    }

    /// Waits for the run `run_id` to end and gets it. Multi-file output is
    /// downloaded into the returned directory.
    ///
    /// `get_run` polls the run's metadata, then reads the run, which the
    /// platform can still show as running for a moment: then it reads the
    /// run again, a few times.
    pub async fn wait(&self, run_id: &str) -> (RunResult, Option<PathBuf>) {
        let output_dir = (self.content == Content::MultiFile).then(|| self.dir("output"));
        let params = RunGetParams {
            polling: Some(polling()),
            output_dir: output_dir.clone(),
        };
        let mut tries = 0;
        loop {
            let result = self
                .platform
                .get_run(&self.workspace_id, run_id, params.clone())
                .await
                .unwrap();
            let status = result.information.metadata.status;
            tries += 1;
            if status.is_terminal() || tries == 10 {
                return (result, output_dir);
            }
            self.log(&format!(
                "{run_id} ended, but the platform still shows it as {status:?}"
            ));
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    }

    /// Asserts that the run `run_id`'s input is `expected`: the same JSON,
    /// or the same files.
    pub async fn assert_input(&self, run_id: &str, expected: &Input) {
        match expected {
            Input::Json(expected) => {
                let input = self
                    .platform
                    .get_run_input(&self.workspace_id, run_id, RunInputGetParams::default())
                    .await
                    .unwrap();
                assert_eq!(input.as_ref(), Some(expected), "the input of {run_id}");
            }
            Input::Dir(expected) => {
                let dir = self.dir("input");
                let params = RunInputGetParams {
                    output_dir: Some(dir.clone()),
                };
                self.platform
                    .get_run_input(&self.workspace_id, run_id, params)
                    .await
                    .unwrap();
                for entry in fs::read_dir(expected).unwrap() {
                    let entry = entry.unwrap();
                    let name = entry.file_name();
                    let name = name.to_str().unwrap();
                    let found = find_file(&dir, name)
                        .unwrap_or_else(|| panic!("the input of {run_id} has no `{name}`"));
                    assert!(
                        fs::read(found).unwrap() == fs::read(entry.path()).unwrap(),
                        "the input of {run_id} has another `{name}`"
                    );
                }
            }
        }
    }

    /// Asserts that `result`, a succeeded run, has an output: JSON that is
    /// not empty, or the fixture's solution files in `output_dir`.
    pub fn assert_output(&self, result: &RunResult, output_dir: Option<&Path>) {
        let run_id = &result.information.id;
        match output_dir {
            None => {
                let output = result.output.as_ref();
                assert!(
                    output.is_some_and(|output| !output.is_empty()),
                    "{run_id} has no output"
                );
            }
            Some(dir) => {
                for name in ["assignments.xlsx", "assignments.csv"] {
                    assert!(
                        find_file(dir, name).is_some(),
                        "the output of {run_id} has no `{name}`"
                    );
                }
            }
        }
    }

    /// Waits for the run `run_id` and checks it: it succeeded or was
    /// canceled, its input is `input`, it has logs and information, and, if
    /// it succeeded, an output.
    pub async fn assert_run(&self, run_id: &str, input: &Input) -> RunResult {
        let (result, output_dir) = self.wait(run_id).await;
        let status = result.information.metadata.status;
        assert!(
            ended(status),
            "{run_id} is {status:?}: {}",
            failure(&result)
        );
        self.assert_input(run_id, input).await;
        let logs = self
            .platform
            .get_run_logs(&self.workspace_id, run_id, RunLogsGetParams::default())
            .await
            .unwrap();
        assert!(!logs.log.is_empty(), "{run_id} has no logs");
        let information = self
            .platform
            .get_run_information(&self.workspace_id, run_id)
            .await
            .unwrap();
        assert!(ended(information.metadata.status), "{run_id} is not done");
        if status == RunStatus::Succeeded {
            self.assert_output(&result, output_dir.as_deref());
        }
        result
    }

    /// Waits for the run `run_id` and asserts that it succeeded.
    pub async fn assert_succeeded(&self, run_id: &str) -> RunResult {
        let (result, _) = self.wait(run_id).await;
        let status = result.information.metadata.status;
        assert!(
            status == RunStatus::Succeeded,
            "{run_id} is {status:?}: {}",
            failure(&result)
        );
        result
    }
}

/// Why the run of `result` failed: its error and error log.
pub fn failure(result: &RunResult) -> String {
    let error = &result.information.metadata.error;
    let log = result.error_log.as_ref();
    let log = log.map(|log| (&log.error, &log.stderr));
    format!("error {error:?}, error log {log:?}")
}

/// Whether a run with `status` ended as the suites expect: it succeeded or
/// was canceled.
pub fn ended(status: RunStatus) -> bool {
    matches!(status, RunStatus::Succeeded | RunStatus::Canceled)
}

/// How the suites poll: every 2 to 3 seconds, for at most 5 minutes.
pub fn polling() -> PollingOptions {
    PollingOptions {
        backoff: 0.0,
        delay: 2.0,
        initial_delay: 1.0,
        max_delay: 2.0,
        max_duration: Some(300.0),
        max_tries: None,
        jitter: 1.0,
    }
}

/// The file named `name` under `dir`, at any depth.
pub fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    for entry in fs::read_dir(dir).ok()? {
        let path = entry.ok()?.path();
        if path.is_dir() {
            if let Some(found) = find_file(&path, name) {
                return Some(found);
            }
        } else if path.file_name().is_some_and(|file| file == name) {
            return Some(path);
        }
    }
    None
}

/// Awaits `futures` at the same time, on this task, and returns their
/// outputs in order.
pub async fn join_all<F: Future>(futures: impl IntoIterator<Item = F>) -> Vec<F::Output> {
    let mut futures: Vec<Option<Pin<Box<F>>>> =
        futures.into_iter().map(|f| Some(Box::pin(f))).collect();
    let mut outputs: Vec<Option<F::Output>> = futures.iter().map(|_| None).collect();
    std::future::poll_fn(|cx| {
        let mut pending = false;
        for (future, output) in futures.iter_mut().zip(&mut outputs) {
            if let Some(running) = future {
                match running.as_mut().poll(cx) {
                    Poll::Ready(value) => {
                        *output = Some(value);
                        *future = None;
                    }
                    Poll::Pending => pending = true,
                }
            }
        }
        if pending {
            Poll::Pending
        } else {
            Poll::Ready(())
        }
    })
    .await;
    outputs.into_iter().map(Option::unwrap).collect()
}

/// The name of `event`'s variant, e.g. `packaged`, and its counts.
fn event_name(event: &PushEvent) -> String {
    let value = serde_json::to_value(event).unwrap_or_default();
    let mut fields = value.as_object().cloned().unwrap_or_default();
    let name = fields.remove("event").unwrap_or_default();
    fields.retain(|_, value| value.is_number() || value.is_boolean());
    format!(
        "{} {}",
        name.as_str().unwrap_or_default(),
        Value::Object(fields)
    )
}

/// 8 random lowercase letters and digits, for a workspace ID.
fn random_suffix() -> String {
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    );
    let mut n = hasher.finish();
    (0..8)
        .map(|_| {
            let digit = (n % 36) as u32;
            n /= 36;
            char::from_digit(digit, 36).unwrap()
        })
        .collect()
}
