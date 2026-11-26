//! Tests for following a local run's logs, against run directories written
//! to the run-directory layout (`local/run_dir.rs`) in temporary workspaces.

mod mock_server;
mod support;

use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use fico_sdk::Error;
use fico_sdk::decision::local::{Local, RunLogsFollowParams};
use fico_sdk::decision::{LocalWorkspaceRef, PollingOptions};
use serde_json::json;
use support::TempDir;

const RUN_ID: &str = "local-3k9x0q2a";

/// A workspace, `routing`, in a temporary directory, with a temporary FICO
/// home next to it.
struct Setup {
    _root: TempDir,
    local: Local,
    src: PathBuf,
}

impl Setup {
    fn new() -> Setup {
        let root = TempDir::new("follow-logs");
        let local = Local::with_home(root.path().join(".fico"));
        let src = root.path().join("routing");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("workspace.yaml"), "files: [main.py]\n").unwrap();
        let src = src.canonicalize().unwrap();
        Setup {
            _root: root,
            local,
            src,
        }
    }

    fn workspace(&self) -> LocalWorkspaceRef {
        LocalWorkspaceRef {
            src: Some(self.src.clone()),
            ..LocalWorkspaceRef::default()
        }
    }

    fn run_dir(&self) -> PathBuf {
        self.src.join(".fico").join("runs").join(RUN_ID)
    }

    /// Writes the run's information file with `status`, atomically, as the
    /// executor does.
    fn set_status(&self, status: &str) {
        set_status(&self.run_dir(), status);
    }

    /// Appends `text` to the run's `logs/logs.log`, creating it.
    fn append(&self, text: &str) {
        append(&self.run_dir(), text);
    }

    /// Follows the run's logs, and returns what the callback got and the
    /// result.
    async fn follow(
        &self,
        polling: PollingOptions,
    ) -> (Vec<String>, fico_sdk::Result<Vec<String>>) {
        let mut seen = Vec::new();
        let result = self
            .local
            .follow_run_logs(
                &self.workspace(),
                RUN_ID,
                RunLogsFollowParams { polling },
                |line| seen.push(line.to_owned()),
            )
            .await;
        (seen, result)
    }
}

fn set_status(dir: &std::path::Path, status: &str) {
    std::fs::create_dir_all(dir).unwrap();
    let information = json!({
        "id": RUN_ID,
        "description": "",
        "name": "",
        "user_email": "",
        "console_url": "",
        "metadata": {
            "application_id": "routing",
            "application_instance_id": "",
            "application_version_id": "",
            "created_at": "2026-10-13T15:00:00.123456Z",
            "duration": 0.0,
            "error": "",
            "execution_class": "local",
            "format": { "input": { "type": "json" }, "output": { "type": "json" } },
            "input_size": 18.0,
            "output_size": 0.0,
            "status_v2": status
        }
    });
    let temporary = dir.join(format!("{RUN_ID}.json.tmp"));
    std::fs::write(&temporary, serde_json::to_vec(&information).unwrap()).unwrap();
    std::fs::rename(temporary, dir.join(format!("{RUN_ID}.json"))).unwrap();
}

fn append(dir: &std::path::Path, text: &str) {
    let logs = dir.join("logs");
    std::fs::create_dir_all(&logs).unwrap();
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(logs.join("logs.log"))
        .unwrap();
    file.write_all(text.as_bytes()).unwrap();
}

/// Polling options that check every 10 ms, with at most `max_tries`
/// checks.
fn fast(max_tries: Option<u32>) -> PollingOptions {
    PollingOptions {
        backoff: 0.0,
        delay: 0.01,
        initial_delay: 0.0,
        max_delay: 0.01,
        max_duration: None,
        max_tries,
        jitter: 0.0,
    }
}

#[tokio::test]
async fn hands_over_every_line_once_while_the_run_writes_them() {
    let setup = Setup::new();
    setup.set_status("running");
    setup.append("start\n");
    let dir = setup.run_dir();
    let writer = tokio::spawn(async move {
        for i in 0..20 {
            append(&dir, &format!("line {i}\n"));
            tokio::time::sleep(Duration::from_millis(3)).await;
        }
        append(&dir, "last\n");
        set_status(&dir, "succeeded");
    });
    let (seen, result) = setup.follow(fast(None)).await;
    writer.await.unwrap();
    let mut expected = vec!["start".to_owned()];
    expected.extend((0..20).map(|i| format!("line {i}")));
    expected.push("last".to_owned());
    assert_eq!(seen, expected);
    assert_eq!(result.unwrap(), expected);
}

#[tokio::test]
async fn holds_back_a_partial_line_until_its_newline() {
    let setup = Setup::new();
    setup.set_status("running");
    setup.append("one\ntw");
    let (seen, result) = setup.follow(fast(Some(2))).await;
    assert!(matches!(result, Err(Error::Wait { .. })), "{result:?}");
    assert_eq!(seen, ["one"]);

    let dir = setup.run_dir();
    let writer = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        append(&dir, "o\nthree\n");
        set_status(&dir, "failed");
    });
    let (seen, result) = setup.follow(fast(None)).await;
    writer.await.unwrap();
    assert_eq!(seen, ["one", "two", "three"]);
    assert_eq!(result.unwrap(), seen);
}

#[tokio::test]
async fn hands_over_a_partial_last_line_once_the_run_is_final() {
    let setup = Setup::new();
    setup.set_status("succeeded");
    setup.append("one\r\ntwo\n\nend \u{1F600}");
    let (seen, result) = setup.follow(fast(Some(1))).await;
    assert_eq!(seen, ["one", "two", "", "end \u{1F600}"]);
    assert_eq!(result.unwrap(), seen);
}

#[tokio::test]
async fn replaces_bytes_that_are_not_utf8() {
    let setup = Setup::new();
    setup.set_status("succeeded");
    let logs = setup.run_dir().join("logs");
    std::fs::create_dir_all(&logs).unwrap();
    std::fs::write(logs.join("logs.log"), b"bad \xff\n").unwrap();
    let (seen, _) = setup.follow(fast(Some(1))).await;
    assert_eq!(seen, ["bad \u{FFFD}"]);
}

#[tokio::test]
async fn has_no_lines_until_the_log_file_appears() {
    let setup = Setup::new();
    setup.set_status("queued");
    let (seen, result) = setup.follow(fast(Some(3))).await;
    assert!(matches!(result, Err(Error::Wait { .. })), "{result:?}");
    assert!(seen.is_empty());

    let dir = setup.run_dir();
    let writer = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        set_status(&dir, "running");
        append(&dir, "hello\n");
        tokio::time::sleep(Duration::from_millis(50)).await;
        set_status(&dir, "succeeded");
    });
    let (seen, result) = setup.follow(fast(None)).await;
    writer.await.unwrap();
    assert_eq!(seen, ["hello"]);
    assert_eq!(result.unwrap(), seen);
}

#[tokio::test]
async fn a_final_run_without_logs_has_no_lines() {
    let setup = Setup::new();
    setup.set_status("canceled");
    let (seen, result) = setup.follow(fast(Some(1))).await;
    assert!(seen.is_empty());
    assert!(result.unwrap().is_empty());
}

#[tokio::test]
async fn reports_an_unknown_run() {
    let setup = Setup::new();
    let (seen, result) = setup.follow(fast(None)).await;
    match result {
        Err(Error::NotFound { kind, id, .. }) => {
            assert_eq!(kind, "local run");
            assert_eq!(id, RUN_ID);
        }
        other => panic!("expected a missing run, got {other:?}"),
    }
    assert!(seen.is_empty());
}

#[tokio::test]
async fn rejects_a_run_id_outside_the_runs_directory() {
    let setup = Setup::new();
    let result = setup
        .local
        .follow_run_logs(
            &setup.workspace(),
            "../escape",
            RunLogsFollowParams::default(),
            |_| {},
        )
        .await;
    assert!(matches!(result, Err(Error::Invalid { .. })), "{result:?}");
}
