//! Pushes the fixture, which is in the repo, with its Python dependencies.

use fico_sdk::decision::platform::{PushEvent, WorkspacePushParams};

use crate::suite::Suite;

pub async fn run(suite: &Suite) {
    let params = WorkspacePushParams {
        src: Some(suite.fixture.clone()),
        ..Default::default()
    };

    // We can push the workspace. The suite's FICO home is new, so push
    // installs the SDK's `uv` first; then the dependencies are downloaded,
    // and the steps are reported in order.
    suite
        .platform
        .push_workspace(&suite.workspace_id, params.clone())
        .await
        .unwrap();
    let events = suite.take_events();
    assert!(
        matches!(events.first(), Some(PushEvent::PushStarted { workspace_id, .. }) if workspace_id == &suite.workspace_id),
        "{events:?}"
    );
    assert!(
        matches!(events.last(), Some(PushEvent::Pushed { workspace_id, .. }) if workspace_id == &suite.workspace_id),
        "{events:?}"
    );
    let position = |matches: fn(&PushEvent) -> bool| {
        events
            .iter()
            .position(matches)
            .unwrap_or_else(|| panic!("an event is missing: {events:?}"))
    };
    let steps = [
        position(|e| matches!(e, PushEvent::InstallingUv { .. })),
        position(|e| matches!(e, PushEvent::BundlingStarted { cache: true, .. })),
        position(|e| matches!(e, PushEvent::DependenciesDownloading { .. })),
        position(|e| matches!(e, PushEvent::CopyingFiles { .. })),
        position(|e| matches!(e, PushEvent::Compressing { .. })),
        position(|e| {
            matches!(
                e,
                PushEvent::Packaged {
                    with_dependencies: true,
                    ..
                }
            )
        }),
        position(|e| matches!(e, PushEvent::Uploading { .. })),
    ];
    assert!(steps.is_sorted(), "{events:?}");

    // A second push takes every dependency from the cache, with the `uv`
    // that the first one installed.
    suite
        .platform
        .push_workspace(&suite.workspace_id, params)
        .await
        .unwrap();
    let events = suite.take_events();
    assert!(
        events.iter().any(|e| matches!(
            e,
            PushEvent::DependenciesCached { cached, total, .. } if cached == total
        )),
        "{events:?}"
    );
    assert!(
        !events.iter().any(|e| matches!(
            e,
            PushEvent::DependenciesDownloading { .. } | PushEvent::InstallingUv { .. }
        )),
        "{events:?}"
    );
}
