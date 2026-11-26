//! Tests for pushing a workspace, `Platform::push_workspace`, and its tool,
//! against two mock servers: the platform, and the storage that the
//! presigned upload URL points to, on another origin. Also the progress
//! events that push reports.

mod mock_server;
mod support;

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fico_sdk::Error;
use fico_sdk::decision::local::Local;
use fico_sdk::decision::platform::{Platform, PushEvent, PushProgress, WorkspacePushParams};
use fico_sdk::tool::{Context, ToolError};
use flate2::read::GzDecoder;
use mock_server::{MockResponse, MockServer};
use serde_json::{Value, json};
use support::TempDir;

/// Writes `files`, as (path, contents), under `dir`.
fn write_files(dir: &Path, files: &[(&str, &str)]) {
    for (name, contents) in files {
        let path = dir.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }
}

/// A temp workspace directory whose `workspace.yaml` holds `yaml`, with
/// `files` next to it.
fn workspace(yaml: &str, files: &[(&str, &str)]) -> TempDir {
    let dir = TempDir::new("workspace-push");
    write_files(dir.path(), &[("workspace.yaml", yaml)]);
    write_files(dir.path(), files);
    dir
}

/// A Go workspace whose build and pre-push commands write files into it,
/// and whose `files` take some of the files in `configs/`.
const GO: &str = r#"
type: go
runtime: ghcr.io/nextmv-io/runtime/default:latest
files:
  - main
  - build.env
  - pre-push.txt
  - configs/*.json
  - "!configs/secret.json"
build:
  command: sh -c printenv>build.env
  environment:
    GREETING: hello
pre-push: echo pushed > pre-push.txt
"#;

fn go_workspace() -> TempDir {
    workspace(
        GO,
        &[
            ("main", "binary"),
            ("configs/a.json", "{}"),
            ("configs/secret.json", "{}"),
            ("notes.txt", "left out"),
        ],
    )
}

/// The platform and the storage, scripted for one push: `GET A/binary`
/// returns an upload URL on the storage, which takes the bundle, and `PUT
/// A/binary` answers `204 No Content`.
struct Servers {
    platform: MockServer,
    storage: MockServer,
}

impl Servers {
    async fn start() -> Servers {
        Servers::with_storage(MockResponse::new(200)).await
    }

    async fn with_storage(upload: MockResponse) -> Servers {
        let storage = MockServer::start(vec![upload]).await;
        let upload_url = format!("{}/bundle?X-Amz-Signature=abc", storage.url());
        let platform = MockServer::start(vec![
            MockResponse::json(200, json!({ "upload_url": upload_url })),
            MockResponse::new(204),
        ])
        .await;
        Servers { platform, storage }
    }

    async fn push(&self, src: &Path) -> fico_sdk::Result<()> {
        let params = WorkspacePushParams {
            src: Some(src.to_owned()),
            ..Default::default()
        };
        self.push_with(params).await
    }

    async fn push_with(&self, params: WorkspacePushParams) -> fico_sdk::Result<()> {
        support::platform(&self.platform)
            .push_workspace("routing", params)
            .await
    }

    /// The bundle that the storage received.
    fn bundle(&self) -> Vec<(String, String)> {
        let requests = self.storage.requests();
        assert_eq!(requests.len(), 1);
        entries(&requests[0].body)
    }

    /// The requirements that `PUT A/binary` sent.
    fn requirements(&self) -> Value {
        let requests = self.platform.requests();
        assert_eq!(requests.len(), 2);
        support::body(&requests[1])
    }

    fn no_requests(&self) -> bool {
        self.platform.requests().is_empty() && self.storage.requests().is_empty()
    }
}

/// The entries of the tar.gz `archive`, as (path, contents), in order.
fn entries(archive: &[u8]) -> Vec<(String, String)> {
    let mut archive = tar::Archive::new(GzDecoder::new(archive));
    let entries = archive.entries().unwrap().map(|entry| {
        let mut entry = entry.unwrap();
        let mut contents = String::new();
        entry.read_to_string(&mut contents).unwrap();
        (entry.path().unwrap().display().to_string(), contents)
    });
    entries.collect()
}

fn names(entries: &[(String, String)]) -> Vec<&str> {
    entries.iter().map(|(name, _)| name.as_str()).collect()
}

fn contents<'a>(entries: &'a [(String, String)], name: &str) -> &'a str {
    let (_, contents) = entries.iter().find(|(n, _)| n == name).unwrap();
    contents
}

#[tokio::test]
async fn pushes_a_workspace() {
    let dir = go_workspace();
    let servers = Servers::start().await;
    servers.push(dir.path()).await.unwrap();

    let requests = servers.platform.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[0].target, "/v1/applications/routing/binary");
    assert_eq!(requests[0].header("authorization"), Some("Bearer secret"));
    assert_eq!(requests[1].method, "PUT");
    assert_eq!(requests[1].target, "/v1/applications/routing/binary");
    assert_eq!(requests[1].header("authorization"), Some("Bearer secret"));
    assert_eq!(
        support::body(&requests[1]),
        json!({
            "requirements": {
                "executable_type": "go",
                "runtime": "ghcr.io/nextmv-io/runtime/default:latest",
                "io_configuration": { "format": "json" },
            },
        })
    );

    // The presigned URL takes no token.
    let upload = &servers.storage.requests()[0];
    assert_eq!(upload.method, "PUT");
    assert_eq!(upload.target, "/bundle?X-Amz-Signature=abc");
    assert_eq!(upload.header("authorization"), None);
    assert_eq!(
        upload.header("content-type"),
        Some("application/octet-stream")
    );

    // What the commands wrote, which shows that they ran in the workspace,
    // and the build with its environment, is bundled with the listed files
    // and the manifest.
    let bundle = servers.bundle();
    assert_eq!(
        names(&bundle),
        [
            "workspace.yaml",
            "build.env",
            "configs/a.json",
            "main",
            "pre-push.txt"
        ]
    );
    assert_eq!(contents(&bundle, "workspace.yaml"), GO);
    assert_eq!(contents(&bundle, "main"), "binary");
    assert_eq!(contents(&bundle, "pre-push.txt"), "pushed\n");
    let environment = contents(&bundle, "build.env");
    assert!(environment.lines().any(|line| line == "GREETING=hello"));
}

#[tokio::test]
async fn sends_the_manifest_as_requirements() {
    let yaml = r#"
type: python
runtime: ghcr.io/nextmv-io/runtime/python:3.11
files: [solve.py]
configuration:
  content:
    format: multi-file
    multi-file:
      input: {path: data}
      output: {metrics: out/metrics.json, assets: "", solutions: out/solutions/}
  options:
    strict: true
    items:
      - name: duration
        option_type: string
        default: 10s
        description: Time limit.
      - name: debug
        option_type: bool
        local_only: true
    format: ["--{{name}}={{value}}"]
execution:
  entrypoint: ./solve.py
  cwd: ./src/
"#;
    let dir = workspace(yaml, &[("solve.py", "print()")]);
    let servers = Servers::start().await;
    servers.push(dir.path()).await.unwrap();

    // The requirements sent for this manifest: without the local-only
    // option, and with the options format as `template`.
    let expected = json!({
        "requirements": {
            "executable_type": "python",
            "runtime": "ghcr.io/nextmv-io/runtime/python:3.11",
            "io_configuration": {
                "format": "multi-file",
                "multi_file": {
                    "input_path": "data",
                    "output_configuration": {
                        "metrics_path": "out/metrics.json",
                        "solutions_path": "out/solutions/",
                    },
                },
            },
            "options": {
                "strict": true,
                "items": [{
                    "name": "duration",
                    "option_type": "string",
                    "default": "10s",
                    "description": "Time limit.",
                    "required": false,
                    "local_only": false,
                }],
                "format": { "template": ["--{{name}}={{value}}"] },
            },
            "entrypoint": "./solve.py",
            "working_directory": "./src/",
        },
    });
    assert_eq!(servers.requirements(), expected);
    assert_eq!(names(&servers.bundle()), ["workspace.yaml", "solve.py"]);
}

#[tokio::test]
async fn pushes_with_a_manifest_from_another_path() {
    let dir = workspace(
        "files: [main.py]\n",
        &[("main.py", "print()"), ("tools.py", "")],
    );
    let other = TempDir::new("workspace-push");
    let yaml = "type: python\nfiles: [main.py, tools.py]\npre-push: echo hi > hi.txt\n";
    write_files(other.path(), &[("custom.yaml", yaml)]);
    let servers = Servers::start().await;
    let params = WorkspacePushParams {
        src: Some(dir.path().to_owned()),
        manifest: Some(other.path().join("custom.yaml")),
        ..Default::default()
    };
    servers.push_with(params).await.unwrap();

    // Its commands run in the workspace, and it is bundled as
    // `workspace.yaml`.
    assert!(dir.path().join("hi.txt").exists());
    let bundle = servers.bundle();
    assert_eq!(names(&bundle), ["workspace.yaml", "main.py", "tools.py"]);
    assert_eq!(contents(&bundle, "workspace.yaml"), yaml);
}

#[tokio::test]
async fn bundles_a_listed_workspace_yaml_once() {
    let yaml = "type: python\nfiles: ['*']\n";
    let dir = workspace(yaml, &[("main.py", "print()")]);
    let servers = Servers::start().await;
    servers.push(dir.path()).await.unwrap();
    let bundle = servers.bundle();
    assert_eq!(names(&bundle), ["main.py", "workspace.yaml"]);
    assert_eq!(contents(&bundle, "workspace.yaml"), yaml);
}

#[tokio::test]
async fn stops_when_the_build_command_fails() {
    let yaml = "type: go\nfiles: [main]\nbuild:\n  command: 'false'\npre-push: touch pre-pushed\n";
    let dir = workspace(yaml, &[("main", "")]);
    let servers = Servers::start().await;
    let err = servers.push(dir.path()).await.unwrap_err();
    match &err {
        Error::Command {
            program, status, ..
        } => {
            assert_eq!(program, "false");
            assert_eq!(*status, Some(1));
        }
        other => panic!("expected a command error, got {other:?}"),
    }
    assert!(!dir.path().join("pre-pushed").exists());
    assert!(servers.no_requests());
}

#[tokio::test]
async fn reports_the_exit_code_and_stderr_of_a_failing_pre_push_command() {
    let yaml = "type: go\nfiles: [main]\npre-push: echo broken >&2; exit 3\n";
    let dir = workspace(yaml, &[("main", "")]);
    let servers = Servers::start().await;
    let err = servers.push(dir.path()).await.unwrap_err();
    assert_eq!(
        err.to_string(),
        "command `echo broken >&2; exit 3` failed with exit code 3 and stderr: broken\n"
    );
    assert!(servers.no_requests());
}

#[tokio::test]
async fn splits_the_build_command_on_any_whitespace() {
    let yaml = "type: go\nfiles: [main, out.txt]\nbuild:\n  command: \"sh  -c\\techo>out.txt\"\n";
    let dir = workspace(yaml, &[("main", "")]);
    let servers = Servers::start().await;
    servers.push(dir.path()).await.unwrap();
    assert_eq!(contents(&servers.bundle(), "out.txt"), "\n");
}

/// Whether the process `pid` is alive, as `ps` tells. A zombie is not:
/// where nothing reaps orphans, such as a container whose first process is
/// `tail`, a killed process stays one.
#[cfg(unix)]
fn is_running(pid: &str) -> bool {
    std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", pid])
        .output()
        .is_ok_and(|output| {
            let state = String::from_utf8_lossy(&output.stdout);
            output.status.success() && !state.trim().starts_with('Z')
        })
}

#[cfg(unix)]
#[tokio::test]
async fn kills_what_the_command_started_when_the_push_is_dropped() {
    // The pre-push shell starts `sleep`, which outlives it unless the
    // whole process group is killed.
    let yaml =
        "type: go\nfiles: [main]\npre-push: sh -c 'echo $$ > sleep.pid; exec sleep 30'; true\n";
    let dir = workspace(yaml, &[("main", "")]);
    let servers = Servers::start().await;
    let platform = support::platform(&servers.platform);
    let params = WorkspacePushParams {
        src: Some(dir.path().to_owned()),
        ..Default::default()
    };
    let push = tokio::spawn(async move { platform.push_workspace("routing", params).await });
    let pid_file = dir.path().join("sleep.pid");
    let mut pid = String::new();
    for _ in 0..250 {
        pid = std::fs::read_to_string(&pid_file).unwrap_or_default();
        if pid.ends_with('\n') {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let pid = pid.trim();
    assert!(is_running(pid), "`sleep` did not start: {pid:?}");

    push.abort();
    assert!(push.await.unwrap_err().is_cancelled());
    for _ in 0..250 {
        if !is_running(pid) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let survived = is_running(pid);
    if survived {
        let _ = std::process::Command::new("kill")
            .args(["-KILL", pid])
            .status();
    }
    assert!(!survived, "`sleep` outlived the dropped push");
    assert!(servers.no_requests());
}

#[tokio::test]
async fn reports_a_program_that_cannot_start() {
    let yaml = "type: go\nfiles: [main]\nbuild:\n  command: no-such-program-for-fico-sdk --flag\n";
    let dir = workspace(yaml, &[("main", "")]);
    let servers = Servers::start().await;
    let err = servers.push(dir.path()).await.unwrap_err();
    assert!(
        matches!(&err, Error::Io { path, .. } if path == Path::new("no-such-program-for-fico-sdk")),
        "{err:?}"
    );
    assert!(servers.no_requests());
}

#[tokio::test]
async fn reports_files_the_manifest_lists_but_the_workspace_lacks() {
    let servers = Servers::start().await;
    for (files, reason) in [
        (
            "[main, missing.txt, 'data/']",
            "nothing matches the `files` entries `missing.txt`, `data/`",
        ),
        (
            "[other]",
            "`files` must include the workspace's entry point, `main`",
        ),
        (
            "[main, ../outside.txt]",
            "the `files` entry `../outside.txt` matches `../outside.txt`, which is outside \
             the workspace",
        ),
    ] {
        let parent = TempDir::new("workspace-push");
        write_files(parent.path(), &[("outside.txt", "")]);
        let src = parent.path().join("ws");
        let yaml = format!("type: go\nfiles: {files}\npre-push: touch pre-pushed\n");
        write_files(
            &src,
            &[("workspace.yaml", &yaml), ("main", ""), ("other", "")],
        );
        let err = servers.push(&src).await.unwrap_err();
        match &err {
            Error::Manifest {
                path, reason: got, ..
            } => {
                assert_eq!(path, &src.join("workspace.yaml"));
                assert_eq!(got, reason);
            }
            other => panic!("expected a manifest error, got {other:?}"),
        }
        // The commands ran before the files were checked.
        assert!(src.join("pre-pushed").exists());
    }
    assert!(servers.no_requests());
}

#[tokio::test]
async fn reports_a_missing_or_invalid_manifest() {
    let servers = Servers::start().await;
    let empty = TempDir::new("workspace-push");
    let err = servers.push(empty.path()).await.unwrap_err();
    assert!(
        matches!(&err, Error::Io { path, .. } if *path == empty.path().join("workspace.yaml")),
        "{err:?}"
    );
    let dir = workspace("files: []\n", &[]);
    let err = servers.push(dir.path()).await.unwrap_err();
    assert!(matches!(err, Error::Manifest { .. }), "{err:?}");
    assert!(servers.no_requests());
}

#[tokio::test]
async fn treats_an_empty_src_or_manifest_as_unset() {
    let servers = Servers::start().await;
    // Tests run in the package's directory, which has no `workspace.yaml`.
    for src in [None, Some(PathBuf::new())] {
        let params = WorkspacePushParams {
            src,
            ..Default::default()
        };
        let err = servers.push_with(params).await.unwrap_err();
        assert!(
            matches!(&err, Error::Io { path, .. } if path == Path::new("./workspace.yaml")),
            "{err:?}"
        );
    }
    let dir = go_workspace();
    let params = WorkspacePushParams {
        src: Some(dir.path().to_owned()),
        manifest: Some(PathBuf::new()),
        ..Default::default()
    };
    servers.push_with(params).await.unwrap();
    assert_eq!(contents(&servers.bundle(), "workspace.yaml"), GO);
}

#[tokio::test]
async fn reports_a_src_that_is_not_a_directory() {
    let servers = Servers::start().await;
    let other = TempDir::new("workspace-push");
    let yaml = "type: go\nfiles: [main]\nbuild:\n  command: go build -o main .\n";
    write_files(other.path(), &[("custom.yaml", yaml), ("file", "")]);
    for src in [other.path().join("missing"), other.path().join("file")] {
        let params = WorkspacePushParams {
            src: Some(src.clone()),
            manifest: Some(other.path().join("custom.yaml")),
            ..Default::default()
        };
        let err = servers.push_with(params).await.unwrap_err();
        // Not the build command's program.
        assert!(
            matches!(&err, Error::Io { path, .. } if *path == src),
            "{err:?}"
        );
    }
    assert!(servers.no_requests());
}

#[tokio::test]
async fn rejects_an_unsupported_python_version_before_running_anything() {
    let servers = Servers::start().await;
    for version in ["'3.9'", "'3'", "'3.10.1'"] {
        let yaml = format!(
            "type: python\nfiles: [main.py]\npython:\n  pip-requirements: [ortools]\n  \
             version: {version}\npre-push: touch pre-pushed\n"
        );
        let dir = workspace(&yaml, &[("main.py", "")]);
        let err = servers.push(dir.path()).await.unwrap_err();
        assert!(matches!(err, Error::Manifest { .. }), "{err:?}");
        assert!(err.to_string().contains("3.10 or later"), "{err}");
        assert!(!dir.path().join("pre-pushed").exists());
    }
    assert!(servers.no_requests());

    // Only Python workspaces bundle dependencies: a binary workspace may set
    // them, and nothing runs `uv`.
    let yaml = "type: binary\nfiles: [main]\npython:\n  pip-requirements: requirements.txt\n";
    let dir = workspace(yaml, &[("main", "")]);
    servers.push(dir.path()).await.unwrap();
}

#[tokio::test]
async fn rejects_an_invalid_workspace_id_before_running_anything() {
    let dir = workspace(
        "type: go\nfiles: [main]\npre-push: touch pre-pushed\n",
        &[("main", "")],
    );
    let servers = Servers::start().await;
    let platform = support::platform(&servers.platform);
    let params = WorkspacePushParams {
        src: Some(dir.path().to_owned()),
        ..Default::default()
    };
    let err = platform.push_workspace("..", params).await.unwrap_err();
    assert!(matches!(err, Error::Invalid { .. }), "{err:?}");
    assert!(!dir.path().join("pre-pushed").exists());
    assert!(servers.no_requests());
}

#[tokio::test]
async fn stops_when_the_upload_fails() {
    let dir = go_workspace();
    let servers = Servers::with_storage(MockResponse::new(403).body("expired")).await;
    let err = servers.push(dir.path()).await.unwrap_err();
    assert!(matches!(err, Error::Status { status: 403, .. }), "{err:?}");
    // No `PUT A/binary` after the failed upload.
    let requests = servers.platform.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "GET");
}

fn local_context(servers: &Servers, home: &TempDir) -> Context {
    Context::new()
        .with_platform(support::platform(&servers.platform))
        .with_local(Local::with_home(home.path().join(".fico")))
}

#[tokio::test]
async fn serves_the_push_tool() {
    let dir = go_workspace();
    let servers = Servers::start().await;
    // `no_cache` only matters for Python dependencies.
    let args = json!({ "workspace_id": "routing", "src": dir.path(), "no_cache": true });
    let result = support::call_tool(
        local_context(&servers, &dir),
        "decision_platform_workspace_push",
        args,
    )
    .await
    .unwrap();
    assert_eq!(result, Value::Null);
    assert_eq!(servers.platform.requests().len(), 2);
    assert!(names(&servers.bundle()).contains(&"main"));
}

#[tokio::test]
async fn needs_local_mode_for_the_push_tool() {
    let dir = go_workspace();
    let servers = Servers::start().await;
    let cx = Context::new().with_platform(support::platform(&servers.platform));
    let args = json!({ "workspace_id": "routing", "src": dir.path() });
    let err = support::call_tool(cx, "decision_platform_workspace_push", args)
        .await
        .unwrap_err();
    assert!(
        matches!(err, ToolError::Failed(Error::Invalid { .. })),
        "{err}"
    );
    assert!(!dir.path().join("pre-push.txt").exists());
    assert!(servers.no_requests());
}

/// A sink that collects every event, serialized: the variants are
/// `#[non_exhaustive]`, so tests cannot build them to compare.
fn collector() -> (PushProgress, Arc<Mutex<Vec<Value>>>) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&events);
    let progress = PushProgress::new(move |event: &PushEvent| {
        seen.lock()
            .unwrap()
            .push(serde_json::to_value(event).unwrap());
    });
    (progress, events)
}

fn taken(events: &Mutex<Vec<Value>>) -> Vec<Value> {
    std::mem::take(&mut *events.lock().unwrap())
}

/// The `event` key of each of `events`.
fn kinds(events: &[Value]) -> Vec<&str> {
    events
        .iter()
        .map(|event| event["event"].as_str().unwrap())
        .collect()
}

#[test]
fn shows_the_sink_in_debug_output() {
    let (progress, _) = collector();
    assert_eq!(format!("{progress:?}"), "PushProgress");
    let client = fico_sdk::Client::builder("http://127.0.0.1:1")
        .build()
        .unwrap();
    let platform = Platform::new(client).with_push_progress(progress);
    assert!(
        format!("{platform:?}").contains("PushProgress"),
        "{platform:?}"
    );
}

#[tokio::test]
async fn reports_each_step_of_a_push_with_the_commands_output() {
    let manifest = "\
type: go
files:
  - main
build:
  command: printf built\\nmain\\r\\n
pre-push: echo one; echo two >&2; printf 'three'
";
    let dir = workspace(manifest, &[("main", "binary")]);
    let servers = Servers::start().await;
    let (progress, events) = collector();
    let params = WorkspacePushParams {
        src: Some(dir.path().to_path_buf()),
        ..Default::default()
    };
    support::platform(&servers.platform)
        .with_push_progress(progress)
        .push_workspace("routing", params)
        .await
        .unwrap();

    let bundle = servers.storage.requests()[0].body.len() as u64;
    assert_eq!(
        taken(&events),
        [
            json!({ "event": "push_started", "workspace_id": "routing" }),
            json!({ "event": "build_command", "command": "printf built\\nmain\\r\\n" }),
            json!({ "event": "command_output", "text": "built" }),
            json!({ "event": "command_output", "text": "main" }),
            json!({
                "event": "pre_push_command",
                "command": "echo one; echo two >&2; printf 'three'",
            }),
            json!({ "event": "command_output", "text": "one" }),
            json!({ "event": "command_output", "text": "three" }),
            json!({ "event": "copying_files" }),
            json!({ "event": "compressing" }),
            json!({
                "event": "packaged",
                "files": 2,
                "bytes": bundle,
                "with_dependencies": false,
            }),
            json!({ "event": "uploading", "workspace_id": "routing" }),
            json!({ "event": "pushed", "workspace_id": "routing" }),
        ]
    );
}

#[tokio::test]
async fn stops_reporting_a_push_at_the_failing_command() {
    let manifest = "type: go\nfiles: [main]\npre-push: echo almost; exit 3\n";
    let dir = workspace(manifest, &[("main", "binary")]);
    let servers = Servers::start().await;
    let (progress, events) = collector();
    let params = WorkspacePushParams {
        src: Some(dir.path().to_path_buf()),
        ..Default::default()
    };
    let result = support::platform(&servers.platform)
        .with_push_progress(progress)
        .push_workspace("routing", params)
        .await;
    assert!(
        matches!(result, Err(fico_sdk::Error::Command { .. })),
        "{result:?}"
    );
    assert_eq!(
        kinds(&taken(&events)),
        ["push_started", "pre_push_command", "command_output"]
    );
}
