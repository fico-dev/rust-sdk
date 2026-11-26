//! Clones of a run, as they are and with a new input, options or queuing.

use fico_sdk::decision::platform::RunCloneParams;
use fico_sdk::decision::{RunConfiguration, RunQueuing};
use serde_json::Value;

use crate::runs::Runs;
use crate::suite::{Suite, ended, join_all};

pub async fn run(suite: &Suite, runs: &Runs) {
    let platform = &suite.platform;
    let workspace_id = &suite.workspace_id;
    let original = &runs.first;
    let other_input = suite.other_input();
    let queuing = |queuing: RunQueuing| RunCloneParams {
        configuration: Some(RunConfiguration {
            queuing: Some(queuing),
            ..Default::default()
        }),
        ..Default::default()
    };

    // We can clone a run as it is, with a new input, with other options,
    // with queuing disabled and with another queuing priority, and get back
    // the new runs' IDs.
    let clones = [
        RunCloneParams::default(),
        other_input.clone_with(),
        RunCloneParams {
            options: suite.options(),
            ..Default::default()
        },
        queuing(RunQueuing {
            disabled: Some(true),
            ..Default::default()
        }),
        queuing(RunQueuing {
            priority: Some(4),
            ..Default::default()
        }),
    ];
    let mut run_ids = Vec::new();
    for params in clones {
        let created = platform
            .clone_run(workspace_id, original, params)
            .await
            .unwrap();
        assert_ne!(&created.run_id, original);
        run_ids.push(created.run_id);
    }
    let [as_is, new_input, options, disabled, priority] = <[String; 5]>::try_from(run_ids).unwrap();

    // The cloned run is discoverable.
    platform
        .get_run_information(workspace_id, &as_is)
        .await
        .unwrap();

    // The clones run to the end.
    let results = join_all(
        [&as_is, &new_input, &options, &disabled, &priority].map(|run_id| suite.wait(run_id)),
    )
    .await;
    for (result, _) in &results {
        let status = result.information.metadata.status;
        assert!(ended(status), "{} is {status:?}", result.information.id);
    }

    // The clone keeps the input, or takes the new one.
    suite.assert_input(&as_is, &suite.input()).await;
    suite.assert_input(&new_input, &other_input).await;

    // The clone takes the new options.
    let (name, value) = suite.option();
    let information = &results[2].0.information;
    let request_options = information.metadata.options.as_ref();
    let request_options = request_options.and_then(|o| o.request_options.as_ref());
    let option = request_options.and_then(|o| o.get(name));
    assert_eq!(option, Some(&Value::String(value.to_owned())));

    // The clone takes the new queuing.
    assert_eq!(
        results[3].0.information.metadata.queuing_disabled,
        Some(true)
    );
    assert_eq!(results[4].0.information.metadata.queuing_priority, Some(4));
}
