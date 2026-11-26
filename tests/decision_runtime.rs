//! Tests for `decision::runtime`, end to end: the local executor runs
//! workspaces whose code is this test binary, which calls the runtime
//! helpers like a workspace's own code does.
//!
//! The binary has no libtest harness (`harness = false` in `Cargo.toml`),
//! since libtest prints to stdout, which is a `json` workspace's output. It
//! runs the tests below, or, when `FICO_RUNTIME_WORKSPACE` is set, the
//! workspace named by it: the shell script `main` of each test's workspace
//! sets that variable and starts this binary.

#[cfg(unix)]
mod unix {
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::process::ExitCode;

    use fico_sdk::decision::local::{Local, RunCreateParams, RunGetParams};
    use fico_sdk::decision::runtime::{self, RuntimeLoadParams, RuntimeWriteParams};
    use fico_sdk::decision::{LocalWorkspaceRef, PollingOptions, RunResult, RunStatus};
    use serde_json::{Value, json};

    /// The environment variable that makes this binary run as a workspace.
    const WORKSPACE: &str = "FICO_RUNTIME_WORKSPACE";

    pub fn main() -> ExitCode {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("a tokio runtime");
        if let Ok(workspace) = std::env::var(WORKSPACE) {
            return match runtime.block_on(run_workspace(&workspace)) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    runtime::log(&format!("workspace failed: {error}"));
                    ExitCode::FAILURE
                }
            };
        }
        runtime.block_on(async {
            runs_a_json_workspace().await;
            println!("runs_a_json_workspace ... ok");
            runs_a_multi_file_workspace().await;
            println!("runs_a_multi_file_workspace ... ok");
            fails_a_run_whose_options_do_not_fit().await;
            println!("fails_a_run_whose_options_do_not_fit ... ok");
        });
        ExitCode::SUCCESS
    }

    /// The code of the workspaces: what a solver's `main` does.
    async fn run_workspace(workspace: &str) -> fico_sdk::Result<()> {
        let run = runtime::load(RuntimeLoadParams::default()).await?;
        runtime::log("solving");
        let metrics = json!({ "cost": 42.5, "nested": { "stops": 2 } });
        let assets = vec![json!({
            "name": "map",
            "content_type": "json",
            "content": { "type": "FeatureCollection", "features": [] },
        })];
        match workspace {
            "json" => {
                let solution = json!({ "input": run.input, "options": run.options });
                runtime::write(RuntimeWriteParams {
                    output: Some(solution),
                    options: Some(run.options.clone()),
                    metrics: metrics.as_object().cloned(),
                    assets: Some(assets),
                    ..RuntimeWriteParams::default()
                })
                .await
            }
            "multi-file" => {
                let names: Vec<String> = run
                    .input_files
                    .iter()
                    .map(|file| file.to_string_lossy().into_owned())
                    .collect();
                // Files of any format can be written to the solutions directory.
                let solutions = run.solutions_dir.clone().expect("a solutions directory");
                std::fs::write(solutions.join("notes.txt"), "plain text").expect("a file");
                runtime::write(RuntimeWriteParams {
                    output_files: [("result.json".to_owned(), json!({ "inputs": names }))].into(),
                    metrics: metrics.as_object().cloned(),
                    assets: Some(assets),
                    ..RuntimeWriteParams::default()
                })
                .await
            }
            other => panic!("unknown workspace `{other}`"),
        }
    }

    /// A temporary directory with a FICO home, `.fico`, and a workspace
    /// directory, `routing`, removed when dropped.
    struct Setup {
        root: PathBuf,
        local: Local,
        src: PathBuf,
    }

    impl Setup {
        /// A workspace with `manifest` whose `main` runs this binary as
        /// `workspace`, with `env` set, such as `REGION=eu `.
        fn new(name: &str, manifest: &str, workspace: &str, env: &str) -> Setup {
            let root = std::env::temp_dir()
                .join(format!("fico-sdk-runtime-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            let src = root.join("routing");
            std::fs::create_dir_all(&src).unwrap();
            let src = src.canonicalize().unwrap();
            std::fs::write(src.join("workspace.yaml"), manifest).unwrap();
            let exe = std::env::current_exe().unwrap();
            let main = src.join("main");
            std::fs::write(
                &main,
                format!(
                    "#!/bin/sh\n{env}{WORKSPACE}={workspace} exec '{}' \"$@\"\n",
                    exe.display()
                ),
            )
            .unwrap();
            std::fs::set_permissions(&main, std::fs::Permissions::from_mode(0o755)).unwrap();
            let local = Local::with_home(root.join(".fico"));
            Setup { root, local, src }
        }

        fn workspace(&self) -> LocalWorkspaceRef {
            LocalWorkspaceRef {
                src: Some(self.src.clone()),
                ..LocalWorkspaceRef::default()
            }
        }

        /// The directory of the run `run_id`.
        fn run_dir(&self, run_id: &str) -> PathBuf {
            self.src.join(".fico").join("runs").join(run_id)
        }

        /// Waits for the run `run_id` to end, and returns it.
        async fn wait(&self, run_id: &str) -> RunResult {
            let polling = PollingOptions {
                backoff: 0.0,
                delay: 0.05,
                initial_delay: 0.0,
                max_delay: 0.05,
                max_duration: Some(60.0),
                max_tries: None,
                jitter: 0.0,
            };
            let params = RunGetParams {
                polling: Some(polling),
                ..RunGetParams::default()
            };
            self.local
                .get_run(&self.workspace(), run_id, params)
                .await
                .unwrap()
        }
    }

    impl Drop for Setup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn read_json(path: &Path) -> Value {
        serde_json::from_slice(&std::fs::read(path).unwrap_or_else(|e| panic!("{path:?}: {e}")))
            .unwrap()
    }

    async fn runs_a_json_workspace() {
        let manifest = "\
type: binary
files: [main]
configuration:
  options:
    items:
      - name: duration
        option_type: int
        default: 30
      - name: region
        option_type: string
      - name: verbose
        option_type: bool
        default: false
      - name: ratio
        option_type: float
        default: 0.5
";
        // `region` comes from the environment, `duration` from the command
        // line the executor passes, and the rest from their defaults.
        let setup = Setup::new("json", manifest, "json", "REGION=eu ");
        let created = setup
            .local
            .create_run(
                &setup.workspace(),
                RunCreateParams {
                    input: Some(json!({ "stops": [1, 2] })),
                    options: [("duration".to_owned(), "5".to_owned())].into(),
                    ..RunCreateParams::default()
                },
            )
            .await
            .unwrap();
        let result = setup.wait(&created.run_id).await;
        let logs = std::fs::read_to_string(setup.run_dir(&created.run_id).join("logs/logs.log"));
        assert_eq!(
            result.information.metadata.status,
            RunStatus::Succeeded,
            "{logs:?}"
        );

        let options = json!({ "duration": 5, "region": "eu", "verbose": false, "ratio": 0.5 });
        let output = result.output.expect("the run's output");
        assert_eq!(output["solution"]["input"], json!({ "stops": [1, 2] }));
        assert_eq!(output["solution"]["options"], options);
        assert_eq!(output["options"], options);
        // The envelope is all that went to stdout, with the output as `solution`.
        let dir = setup.run_dir(&created.run_id);
        let envelope = read_json(&dir.join("outputs/solutions/solution.json"));
        assert_eq!(envelope["options"], options);
        assert_eq!(envelope["solution"]["input"], json!({ "stops": [1, 2] }));
        assert_eq!(
            read_json(&dir.join("outputs/metrics.json")),
            json!({ "metrics": { "cost": 42.5, "nested": { "stops": 2 } } })
        );
        assert_eq!(
            read_json(&dir.join("outputs/assets.json"))["assets"][0]["name"],
            "map"
        );
        assert_eq!(logs.unwrap(), "solving\n");
    }

    async fn runs_a_multi_file_workspace() {
        let manifest = "\
type: binary
files: [main]
configuration:
  content:
    format: multi-file
    multi-file:
      input:
        path: data
      output:
        solutions: results
        metrics: out/metrics.json
        assets: out/assets.json
";
        let setup = Setup::new("multi", manifest, "multi-file", "");
        let input = setup.root.join("input");
        std::fs::create_dir_all(input.join("nested")).unwrap();
        std::fs::write(input.join("b.csv"), "b").unwrap();
        std::fs::write(input.join("nested/a.csv"), "a").unwrap();
        let created = setup
            .local
            .create_run(
                &setup.workspace(),
                RunCreateParams {
                    input_dir: Some(input),
                    ..RunCreateParams::default()
                },
            )
            .await
            .unwrap();
        let result = setup.wait(&created.run_id).await;
        let logs = std::fs::read_to_string(setup.run_dir(&created.run_id).join("logs/logs.log"));
        assert_eq!(
            result.information.metadata.status,
            RunStatus::Succeeded,
            "{logs:?}"
        );

        let dir = setup.run_dir(&created.run_id);
        let solutions = dir.join("outputs/solutions");
        assert_eq!(
            read_json(&solutions.join("result.json")),
            json!({ "inputs": ["b.csv", "nested/a.csv"] })
        );
        assert_eq!(
            std::fs::read_to_string(solutions.join("notes.txt")).unwrap(),
            "plain text"
        );
        assert_eq!(
            read_json(&dir.join("outputs/metrics.json")),
            json!({ "metrics": { "cost": 42.5, "nested": { "stops": 2 } } })
        );
        assert_eq!(
            read_json(&dir.join("outputs/assets.json"))["assets"][0]["name"],
            "map"
        );
        assert_eq!(logs.unwrap(), "solving\n");
    }

    async fn fails_a_run_whose_options_do_not_fit() {
        let manifest = "\
type: binary
files: [main]
configuration:
  options:
    items:
      - name: duration
        option_type: int
        default: 30
";
        let setup = Setup::new("bad", manifest, "json", "");
        let created = setup
            .local
            .create_run(
                &setup.workspace(),
                RunCreateParams {
                    input: Some(json!({})),
                    options: [("duration".to_owned(), "soon".to_owned())].into(),
                    ..RunCreateParams::default()
                },
            )
            .await
            .unwrap();
        let result = setup.wait(&created.run_id).await;
        assert_eq!(result.information.metadata.status, RunStatus::Failed);
        let logs =
            std::fs::read_to_string(setup.run_dir(&created.run_id).join("logs/logs.log")).unwrap();
        assert!(logs.contains("option `duration`"), "{logs}");
        // Nothing went to stdout, so there is no output.
        assert!(result.output.is_none());
    }
}

#[cfg(unix)]
use unix::main;

#[cfg(not(unix))]
fn main() {}
