//! Live tests against the platform: one suite per content format, each on
//! its own workspace, run at the same time. Each suite pushes a workspace
//! from `fixtures`, then runs its stages in the order they depend on each
//! other, with the stages that do not depend on each other at the same time,
//! and deletes the workspace at the end, also when a stage fails.
//!
//! They need `FICO_API_KEY`. `FICO_ENDPOINT` replaces the default endpoint,
//! `https://api.cloud.nextmv.io`. Run them with `just integration`.

#[path = "../mock_server/mod.rs"]
mod mock_server;
#[path = "../support/mod.rs"]
mod support;

mod acceptance_tests;
mod builds;
mod cleanup;
mod clone_runs;
mod ensembles;
mod environments;
mod init_workspace;
mod input_sets;
mod managed_inputs;
mod push;
mod runs;
mod scenario_tests;
mod secrets;
mod shadow_tests;
mod suite;
mod switchback_tests;
mod track_runs;

use std::panic::resume_unwind;
use std::sync::Arc;
use std::time::{Duration, Instant};

use suite::{Content, Suite};

/// How long a suite may take before it fails. A suite takes a few minutes.
const SUITE_LIMIT: Duration = Duration::from_secs(10 * 60);

/// The stack of each thread of a suite's runtime. The stages' futures are
/// deep, so the default 2 MiB is not enough.
const STACK_SIZE: usize = 16 << 20;

#[test]
#[ignore = "reaches the platform: run it with `just integration`"]
fn json() {
    runtime().block_on(run(Content::Json));
}

#[test]
#[ignore = "reaches the platform: run it with `just integration`"]
fn multi_file() {
    runtime().block_on(run(Content::MultiFile));
}

/// A runtime for a suite, whose threads have a [`STACK_SIZE`] stack.
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(STACK_SIZE)
        .build()
        .unwrap()
}

/// Runs the suite for `content`. When a stage fails or the suite takes
/// longer than [`SUITE_LIMIT`], it deletes what the suite made, best effort,
/// and then fails.
async fn run(content: Content) {
    let started = Instant::now();
    let suite = Arc::new(Suite::new(content).await);
    let task = tokio::spawn({
        let suite = suite.clone();
        async move { stages(&suite).await }
    });
    let abort = task.abort_handle();
    match tokio::time::timeout(SUITE_LIMIT, task).await {
        Ok(Ok(())) => suite.log(&format!(
            "passed in {:.1} s",
            started.elapsed().as_secs_f64()
        )),
        Ok(Err(error)) => {
            cleanup::after_failure(&suite).await;
            match error.try_into_panic() {
                Ok(panic) => resume_unwind(panic),
                Err(error) => panic!("the {} suite stopped: {error}", suite.label),
            }
        }
        Err(_) => {
            abort.abort();
            cleanup::after_failure(&suite).await;
            panic!(
                "the {} suite took more than {} minutes",
                suite.label,
                SUITE_LIMIT.as_secs() / 60
            );
        }
    }
}

/// The stages, in the order they depend on each other: each one starts once
/// the stages it needs are done.
async fn stages(suite: &Suite) {
    suite
        .stage("init_workspace", init_workspace::run(suite))
        .await;
    suite.stage("push", push::run(suite)).await;
    let (_, (build_id, environments, input_set_id)) =
        tokio::join!(suite.stage("secrets", secrets::run(suite)), async {
            let build_id = suite.stage("builds", builds::run(suite)).await;
            let environments = suite
                .stage("environments", environments::run(suite, &build_id))
                .await;
            let (input_set_id, _, _, _) = tokio::join!(
                async {
                    let runs = suite.stage("runs", runs::run(suite, &environments)).await;
                    let (_, _, input_set_id) = tokio::join!(
                        suite.stage("clone_runs", clone_runs::run(suite, &runs)),
                        suite.stage("managed_inputs", managed_inputs::run(suite, &runs)),
                        async {
                            let input_set_id = suite
                                .stage("input_sets", input_sets::run(suite, &environments, &runs))
                                .await;
                            tokio::join!(
                                suite.stage(
                                    "scenario_tests",
                                    scenario_tests::run(suite, &environments, &input_set_id)
                                ),
                                suite.stage(
                                    "acceptance_tests",
                                    acceptance_tests::run(suite, &environments, &input_set_id)
                                ),
                            );
                            input_set_id
                        },
                    );
                    input_set_id
                },
                suite.stage("track_runs", track_runs::run(suite, &environments)),
                suite.stage("ensembles", ensembles::run(suite, &environments)),
                async {
                    suite
                        .stage("shadow_tests", shadow_tests::run(suite, &environments))
                        .await;
                    // After the shadow test, so that the two tests do not
                    // share the environments' runs.
                    suite
                        .stage(
                            "switchback_tests",
                            switchback_tests::run(suite, &environments),
                        )
                        .await;
                },
            );
            (build_id, environments, input_set_id)
        },);
    suite
        .stage(
            "cleanup",
            cleanup::run(suite, &build_id, &environments, &input_set_id),
        )
        .await;
}
