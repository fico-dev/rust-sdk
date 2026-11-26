//! The layout of a local run's directory: the paths of its files, which
//! local runs and the executor both read and write.
//!
//! A local run lives in `<src>/.fico/runs/<run_id>/`, where `<src>` is the
//! workspace's directory and `<run_id>` looks like `local-3k9x0q2a`:
//!
//! ```text
//! <run_id>.json          RunInformation, with API names
//! inputs/input.json      the input, for JSON input
//! inputs/…               the input files, for multi-file input
//! logs/logs.log          what the workspace wrote to stderr
//! outputs/metrics.json   the metrics
//! outputs/assets.json    the assets
//! outputs/solutions/     the output: solution.json for JSON output, the
//!                        output files for multi-file output
//! visuals/               HTML files built from the assets
//! ```
//!
//! There is no `outputs/statistics.json`, as FICO only has metrics, and no
//! `inputs/input`, as FICO has no text input.
//!
//! `<src>/.fico/.gitignore` ignores everything under `.fico` but itself, so
//! it can be committed and the runs stay ignored for everyone.
//!
//! The status in `<run_id>.json` goes from `queued` to `running`, and then
//! to `succeeded` or `failed`. Finalization (the status, metrics, assets and
//! visuals) is idempotent, and also runs on the first read of a run whose
//! process has exited.

use std::path::{Path, PathBuf};

use crate::{Error, Result};

/// Directory, in a workspace's directory, that holds its local runs.
pub(crate) const FICO_DIR: &str = ".fico";

/// Prefix of every local run ID.
pub(crate) const RUN_ID_PREFIX: &str = "local";

/// Content of `<src>/.fico/.gitignore`: it ignores everything but itself.
pub(crate) const GITIGNORE: &str = "*\n!.gitignore\n";

/// `<src>/.fico/.gitignore`.
pub(crate) fn gitignore_file(src: &Path) -> PathBuf {
    src.join(FICO_DIR).join(".gitignore")
}

/// `<src>/.fico/runs`, which holds every local run of the workspace.
pub(crate) fn runs_dir(src: &Path) -> PathBuf {
    src.join(FICO_DIR).join("runs")
}

/// `<src>/.fico/runs/<run_id>`, the directory of a local run.
///
/// # Errors
///
/// Returns [`Error::Invalid`] unless `run_id` is made of ASCII letters,
/// digits, `.`, `_` and `-` and is not `.` or `..`, so that it can never
/// point outside the runs directory.
pub(crate) fn run_dir(src: &Path, run_id: &str) -> Result<PathBuf> {
    let safe = !run_id.is_empty()
        && !matches!(run_id, "." | "..")
        && run_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    if !safe {
        return Err(Error::Invalid {
            reason: format!("`{run_id}` is not a valid local run ID"),
        });
    }
    Ok(runs_dir(src).join(run_id))
}

/// `<run_dir>/<run_id>.json`, the run's information.
pub(crate) fn information_file(run_dir: &Path, run_id: &str) -> PathBuf {
    run_dir.join(format!("{run_id}.json"))
}

/// `<run_dir>/inputs`, the run's input.
pub(crate) fn inputs_dir(run_dir: &Path) -> PathBuf {
    run_dir.join("inputs")
}

/// `<run_dir>/inputs/input.json`, the input of a run with JSON input.
pub(crate) fn input_json_file(run_dir: &Path) -> PathBuf {
    inputs_dir(run_dir).join("input.json")
}

/// `<run_dir>/logs/logs.log`, what the workspace wrote to stderr.
pub(crate) fn logs_file(run_dir: &Path) -> PathBuf {
    run_dir.join("logs").join("logs.log")
}

/// `<run_dir>/outputs`, the run's metrics, assets and output.
pub(crate) fn outputs_dir(run_dir: &Path) -> PathBuf {
    run_dir.join("outputs")
}

/// `<run_dir>/outputs/metrics.json`.
pub(crate) fn metrics_file(run_dir: &Path) -> PathBuf {
    outputs_dir(run_dir).join("metrics.json")
}

/// `<run_dir>/outputs/assets.json`.
pub(crate) fn assets_file(run_dir: &Path) -> PathBuf {
    outputs_dir(run_dir).join("assets.json")
}

/// `<run_dir>/outputs/solutions`, the run's output.
pub(crate) fn solutions_dir(run_dir: &Path) -> PathBuf {
    outputs_dir(run_dir).join("solutions")
}

/// `<run_dir>/outputs/solutions/solution.json`, the output of a run with
/// JSON output.
pub(crate) fn output_json_file(run_dir: &Path) -> PathBuf {
    solutions_dir(run_dir).join("solution.json")
}

/// `<run_dir>/visuals`, the HTML files built from the run's assets.
pub(crate) fn visuals_dir(run_dir: &Path) -> PathBuf {
    run_dir.join("visuals")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lays_out_a_run() {
        let src = Path::new("/workspaces/routing");
        let dir = run_dir(src, "local-3k9x0q2a").unwrap();
        assert_eq!(
            dir,
            Path::new("/workspaces/routing/.fico/runs/local-3k9x0q2a")
        );
        let files = [
            (
                information_file(&dir, "local-3k9x0q2a"),
                "local-3k9x0q2a.json",
            ),
            (input_json_file(&dir), "inputs/input.json"),
            (logs_file(&dir), "logs/logs.log"),
            (metrics_file(&dir), "outputs/metrics.json"),
            (assets_file(&dir), "outputs/assets.json"),
            (solutions_dir(&dir), "outputs/solutions"),
            (output_json_file(&dir), "outputs/solutions/solution.json"),
            (visuals_dir(&dir), "visuals"),
        ];
        for (path, relative) in files {
            assert_eq!(path, dir.join(relative));
        }
        assert_eq!(
            gitignore_file(src),
            Path::new("/workspaces/routing/.fico/.gitignore")
        );
    }

    #[test]
    fn rejects_run_ids_that_leave_the_runs_directory() {
        let src = Path::new("/workspaces/routing");
        for run_id in ["", ".", "..", "../x", "a/b", "a\\b", "local-é", "/etc"] {
            let err = run_dir(src, run_id).unwrap_err();
            assert!(matches!(err, Error::Invalid { .. }), "{run_id}: {err}");
        }
        assert!(run_dir(src, "local-a.b_c").is_ok());
    }
}
