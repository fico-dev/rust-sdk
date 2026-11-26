//! Tests for the local workspace operations and their tools, in a temporary
//! FICO home with temporary workspace directories.

mod mock_server;
mod support;

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use fico_sdk::Error;
use fico_sdk::decision::local::{Local, WorkspaceRegisterParams, WorkspaceUpdateParams};
use fico_sdk::decision::{ContentFormat, LocalWorkspace, LocalWorkspaceRef};
use fico_sdk::tool::{Context, ToolError};
use serde_json::{Value, json};
use support::TempDir;

/// The manifest of a workspace with JSON input and output.
const JSON_MANIFEST: &str = "files: [main.py]\n";

/// The manifest of a workspace with multi-file input and output.
const MULTI_FILE_MANIFEST: &str = "\
files: [main.py]
configuration:
  content:
    format: multi-file
";

/// A temporary directory with a FICO home, `.fico`, and workspace
/// directories next to it.
struct Setup {
    root: TempDir,
    local: Local,
}

impl Setup {
    fn new() -> Setup {
        let root = TempDir::new("local-workspace");
        let local = Local::with_home(root.path().join(".fico"));
        Setup { root, local }
    }

    /// The workspace directory `name`, with `manifest` as its
    /// `workspace.yaml`. Returns its canonical path, which the registry
    /// stores.
    fn workspace(&self, name: &str, manifest: &str) -> PathBuf {
        let dir = self.root.path().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("workspace.yaml"), manifest).unwrap();
        dir.canonicalize().unwrap()
    }

    /// The directory that holds the registry.
    fn registry_dir(&self) -> PathBuf {
        self.root.path().join(".fico").join("decision")
    }

    fn context(&self) -> Context {
        Context::new().with_local(self.local.clone())
    }

    async fn register(&self, src: &Path, workspace_id: Option<&str>) -> LocalWorkspace {
        self.local
            .register_workspace(register_params(src, workspace_id))
            .await
            .unwrap()
    }

    async fn list(&self) -> Vec<LocalWorkspace> {
        self.local.list_workspaces().await.unwrap()
    }
}

fn register_params(src: &Path, workspace_id: Option<&str>) -> WorkspaceRegisterParams {
    WorkspaceRegisterParams {
        src: src.to_owned(),
        workspace_id: workspace_id.map(str::to_owned),
        description: None,
    }
}

fn by_id(workspace_id: &str) -> LocalWorkspaceRef {
    LocalWorkspaceRef {
        workspace_id: Some(workspace_id.to_owned()),
        src: None,
    }
}

fn by_src(src: &Path) -> LocalWorkspaceRef {
    LocalWorkspaceRef {
        workspace_id: None,
        src: Some(src.to_owned()),
    }
}

fn by_both(workspace_id: &str, src: &Path) -> LocalWorkspaceRef {
    LocalWorkspaceRef {
        workspace_id: Some(workspace_id.to_owned()),
        src: Some(src.to_owned()),
    }
}

fn description(description: &str) -> WorkspaceUpdateParams {
    WorkspaceUpdateParams {
        description: Some(description.to_owned()),
        ..Default::default()
    }
}

fn new_id(workspace_id: &str) -> WorkspaceUpdateParams {
    WorkspaceUpdateParams {
        new_workspace_id: Some(workspace_id.to_owned()),
        ..Default::default()
    }
}

#[track_caller]
fn assert_invalid(result: Result<impl std::fmt::Debug, Error>) {
    let err = result.unwrap_err();
    assert!(matches!(err, Error::Invalid { .. }), "{err}");
}

#[track_caller]
fn assert_not_found(result: Result<impl std::fmt::Debug, Error>) {
    let err = result.unwrap_err();
    assert!(
        matches!(
            err,
            Error::NotFound {
                kind: "local workspace",
                ..
            }
        ),
        "{err}"
    );
}

/// Whether `err` is an [`Error::Io`] for `path` with an error of `kind`.
fn is_io(err: &Error, path: &Path, kind: io::ErrorKind) -> bool {
    matches!(err, Error::Io { path: p, source, .. } if p == path && source.kind() == kind)
}

#[tokio::test]
async fn registers_a_workspace() {
    let setup = Setup::new();
    let src = setup.workspace("routing", MULTI_FILE_MANIFEST);
    let params = WorkspaceRegisterParams {
        src: src.clone(),
        workspace_id: Some("routing".to_owned()),
        description: Some("Routes vehicles.".to_owned()),
    };
    let entry = setup.local.register_workspace(params).await.unwrap();
    assert_eq!(entry.workspace_id, "routing");
    assert_eq!(entry.src, src);
    assert_eq!(entry.content_format, ContentFormat::MultiFile);
    assert_eq!(entry.description.as_deref(), Some("Routes vehicles."));
    assert_eq!(entry.created_at.len(), "2026-10-07T14:19:02.123456Z".len());
    assert!(entry.created_at.ends_with('Z'), "{}", entry.created_at);
    assert_eq!(entry.updated_at, entry.created_at);
    assert_eq!(setup.list().await, [entry]);
}

#[tokio::test]
async fn generates_an_id_when_none_is_set() {
    let setup = Setup::new();
    let first = setup.workspace("first", JSON_MANIFEST);
    let second = setup.workspace("second", JSON_MANIFEST);
    let a = setup.register(&first, None).await;
    let b = setup.register(&second, Some("")).await;
    for entry in [&a, &b] {
        let suffix = entry.workspace_id.strip_prefix("local-workspace-").unwrap();
        assert_eq!(suffix.len(), 8, "{}", entry.workspace_id);
        assert!(
            suffix
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()),
            "{}",
            entry.workspace_id
        );
        assert_eq!(entry.content_format, ContentFormat::Json);
        assert_eq!(entry.description, None);
    }
    assert_ne!(a.workspace_id, b.workspace_id);
}

#[tokio::test]
async fn finds_a_directory_by_any_path_to_it() {
    let setup = Setup::new();
    let src = setup.workspace("routing", JSON_MANIFEST);
    let roundabout = setup.root.path().join("elsewhere/../routing/.");
    let entry = setup.register(&roundabout, Some("routing")).await;
    assert_eq!(entry.src, src);
    let registered = setup
        .local
        .workspace_registered(&by_src(&setup.root.path().join("routing")))
        .await;
    assert!(registered.unwrap());
    #[cfg(unix)]
    {
        let link = setup.root.path().join("link");
        std::os::unix::fs::symlink(&src, &link).unwrap();
        assert!(
            setup
                .local
                .workspace_registered(&by_src(&link))
                .await
                .unwrap()
        );
        assert_eq!(setup.register(&link, None).await, entry);
    }
    assert_eq!(setup.list().await, [entry]);
}

#[tokio::test]
async fn returns_the_entry_of_a_registered_directory() {
    let setup = Setup::new();
    let src = setup.workspace("routing", JSON_MANIFEST);
    let entry = setup.register(&src, Some("routing")).await;
    let file = setup.registry_dir().join("registry.json");
    let saved = std::fs::read(&file).unwrap();
    assert_eq!(setup.register(&src, None).await, entry);
    assert_eq!(setup.register(&src, Some("routing")).await, entry);
    assert_eq!(std::fs::read(&file).unwrap(), saved);
    assert_eq!(setup.list().await, [entry]);
}

#[tokio::test]
async fn rejects_ids_that_are_taken() {
    let setup = Setup::new();
    let a = setup.workspace("a", JSON_MANIFEST);
    let b = setup.workspace("b", JSON_MANIFEST);
    let entry = setup.register(&a, Some("a")).await;
    // A registered directory keeps its ID.
    let result = setup
        .local
        .register_workspace(register_params(&a, Some("b")));
    assert_invalid(result.await);
    // Another directory can't take it.
    let result = setup
        .local
        .register_workspace(register_params(&b, Some("a")));
    assert_invalid(result.await);
    assert_eq!(setup.list().await, [entry]);
}

#[tokio::test]
async fn needs_a_valid_manifest_to_register() {
    let setup = Setup::new();
    let empty = setup.root.path().join("empty");
    std::fs::create_dir(&empty).unwrap();
    let empty = empty.canonicalize().unwrap();
    let err = setup
        .local
        .register_workspace(register_params(&empty, None))
        .await
        .unwrap_err();
    let manifest = empty.join("workspace.yaml");
    assert!(is_io(&err, &manifest, io::ErrorKind::NotFound), "{err}");

    let missing = setup.root.path().join("missing");
    let err = setup
        .local
        .register_workspace(register_params(&missing, None))
        .await
        .unwrap_err();
    assert!(matches!(&err, Error::Io { source, .. } if source.kind() == io::ErrorKind::NotFound));

    let invalid = setup.workspace("invalid", "files: []\n");
    let err = setup
        .local
        .register_workspace(register_params(&invalid, None))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Manifest { .. }), "{err}");

    let result = setup
        .local
        .register_workspace(register_params(Path::new(""), None));
    assert_invalid(result.await);
    assert!(!setup.registry_dir().exists());
}

#[tokio::test]
async fn gets_a_workspace_by_id_by_directory_or_both() {
    let setup = Setup::new();
    let a_src = setup.workspace("a", JSON_MANIFEST);
    let b_src = setup.workspace("b", MULTI_FILE_MANIFEST);
    let a = setup.register(&a_src, Some("a")).await;
    let b = setup.register(&b_src, Some("b")).await;
    let local = &setup.local;
    assert_eq!(local.get_workspace(&by_id("a")).await.unwrap(), a);
    assert_eq!(local.get_workspace(&by_src(&b_src)).await.unwrap(), b);
    assert_eq!(local.get_workspace(&by_both("a", &a_src)).await.unwrap(), a);

    // With both, both must match.
    let err = local
        .get_workspace(&by_both("a", &b_src))
        .await
        .unwrap_err();
    let expected = format!("local workspace `a at {}` not found", b_src.display());
    assert_eq!(err.to_string(), expected);
    assert_not_found(local.get_workspace(&by_id("c")).await);
    // An empty value counts as unset.
    assert_invalid(local.get_workspace(&LocalWorkspaceRef::default()).await);
    let empty = LocalWorkspaceRef {
        workspace_id: Some(String::new()),
        src: Some(PathBuf::new()),
    };
    assert_invalid(local.get_workspace(&empty).await);
}

#[tokio::test]
async fn reads_the_manifest_on_get() {
    let setup = Setup::new();
    let src = setup.workspace("routing", JSON_MANIFEST);
    setup.register(&src, Some("routing")).await;
    std::fs::write(src.join("workspace.yaml"), MULTI_FILE_MANIFEST).unwrap();
    let entry = setup.local.get_workspace(&by_id("routing")).await.unwrap();
    assert_eq!(entry.content_format, ContentFormat::MultiFile);
    // Listing does not read manifests.
    assert_eq!(setup.list().await[0].content_format, ContentFormat::Json);

    std::fs::write(src.join("workspace.yaml"), "files: []\n").unwrap();
    let err = setup
        .local
        .get_workspace(&by_id("routing"))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Manifest { .. }), "{err}");
}

#[tokio::test]
async fn reports_a_workspace_whose_directory_is_gone() {
    let setup = Setup::new();
    let src = setup.workspace("routing", JSON_MANIFEST);
    let entry = setup.register(&src, Some("routing")).await;
    std::fs::remove_dir_all(&src).unwrap();
    let err = setup
        .local
        .get_workspace(&by_id("routing"))
        .await
        .unwrap_err();
    let manifest = src.join("workspace.yaml");
    assert!(is_io(&err, &manifest, io::ErrorKind::NotFound), "{err}");
    // The entry stays until it is deleted, by any path to it.
    assert_eq!(setup.list().await, [entry]);
    let path = setup.root.path().join("routing");
    assert!(
        setup
            .local
            .workspace_registered(&by_src(&path))
            .await
            .unwrap()
    );
    setup.local.delete_workspace(&by_src(&path)).await.unwrap();
    assert_eq!(setup.list().await, []);
}

#[tokio::test]
async fn lists_workspaces_in_the_order_they_were_registered() {
    let setup = Setup::new();
    for id in ["c", "a", "b"] {
        let src = setup.workspace(id, JSON_MANIFEST);
        setup.register(&src, Some(id)).await;
    }
    let ids: Vec<String> = setup
        .list()
        .await
        .into_iter()
        .map(|entry| entry.workspace_id)
        .collect();
    assert_eq!(ids, ["c", "a", "b"]);
}

#[tokio::test]
async fn reads_without_writing_anything() {
    let setup = Setup::new();
    assert_eq!(setup.list().await, []);
    let local = &setup.local;
    assert!(!local.workspace_registered(&by_id("a")).await.unwrap());
    assert_not_found(local.get_workspace(&by_id("a")).await);
    assert!(!setup.root.path().join(".fico").exists());
}

#[tokio::test]
async fn checks_whether_a_workspace_is_registered() {
    let setup = Setup::new();
    let a = setup.workspace("a", JSON_MANIFEST);
    let b = setup.workspace("b", JSON_MANIFEST);
    setup.register(&a, Some("a")).await;
    let local = &setup.local;
    assert!(local.workspace_registered(&by_id("a")).await.unwrap());
    assert!(local.workspace_registered(&by_src(&a)).await.unwrap());
    assert!(local.workspace_registered(&by_both("a", &a)).await.unwrap());
    assert!(!local.workspace_registered(&by_both("a", &b)).await.unwrap());
    assert!(!local.workspace_registered(&by_src(&b)).await.unwrap());
    assert!(!local.workspace_registered(&by_id("b")).await.unwrap());
    assert_invalid(
        local
            .workspace_registered(&LocalWorkspaceRef::default())
            .await,
    );
}

#[tokio::test]
async fn updates_the_description_and_the_id() {
    let setup = Setup::new();
    let src = setup.workspace("routing", JSON_MANIFEST);
    let params = WorkspaceRegisterParams {
        description: Some("Old.".to_owned()),
        ..register_params(&src, Some("routing"))
    };
    let registered = setup.local.register_workspace(params).await.unwrap();

    let local = &setup.local;
    let updated = local
        .update_workspace(&by_id("routing"), description("New."))
        .await
        .unwrap();
    assert_eq!(updated.workspace_id, "routing");
    assert_eq!(updated.description.as_deref(), Some("New."));
    assert_eq!(updated.created_at, registered.created_at);
    assert!(updated.updated_at >= registered.updated_at);

    let renamed = local
        .update_workspace(&by_src(&src), new_id("trucks"))
        .await
        .unwrap();
    assert_eq!(renamed.workspace_id, "trucks");
    assert_eq!(renamed.description.as_deref(), Some("New."));
    assert_not_found(local.get_workspace(&by_id("routing")).await);
    assert_eq!(
        local.get_workspace(&by_id("trucks")).await.unwrap(),
        renamed
    );
    assert_eq!(setup.list().await, [renamed]);
}

#[tokio::test]
async fn needs_a_change_to_update() {
    let setup = Setup::new();
    let src = setup.workspace("routing", JSON_MANIFEST);
    let entry = setup.register(&src, Some("routing")).await;
    let local = &setup.local;
    let ws = by_id("routing");
    assert_invalid(local.update_workspace(&ws, Default::default()).await);
    assert_invalid(local.update_workspace(&ws, new_id("")).await);
    assert_eq!(setup.list().await, [entry]);
}

#[tokio::test]
async fn rejects_a_new_id_that_another_workspace_has() {
    let setup = Setup::new();
    let a = setup.workspace("a", JSON_MANIFEST);
    let b = setup.workspace("b", JSON_MANIFEST);
    setup.register(&a, Some("a")).await;
    setup.register(&b, Some("b")).await;
    let local = &setup.local;
    assert_invalid(local.update_workspace(&by_id("a"), new_id("b")).await);
    let same = local.update_workspace(&by_id("a"), new_id("a")).await;
    assert_eq!(same.unwrap().workspace_id, "a");
}

#[tokio::test]
async fn registers_a_directory_before_updating_it() {
    let setup = Setup::new();
    let first = setup.workspace("first", JSON_MANIFEST);
    let second = setup.workspace("second", JSON_MANIFEST);
    let local = &setup.local;
    let entry = local
        .update_workspace(&by_src(&first), description("First."))
        .await
        .unwrap();
    assert!(entry.workspace_id.starts_with("local-workspace-"));
    assert_eq!(entry.src, first);
    assert_eq!(entry.description.as_deref(), Some("First."));

    let entry = local
        .update_workspace(&by_both("second", &second), description("Second."))
        .await
        .unwrap();
    assert_eq!(entry.workspace_id, "second");
    // An ID alone has no directory to register.
    assert_not_found(
        local
            .update_workspace(&by_id("third"), description("Third."))
            .await,
    );
    assert_eq!(setup.list().await.len(), 2);
}

/// Registering an unregistered `src` and updating it are one change, so a
/// rejected update registers nothing.
#[tokio::test]
async fn registers_nothing_when_an_update_is_rejected() {
    let setup = Setup::new();
    let a = setup.workspace("a", JSON_MANIFEST);
    let b = setup.workspace("b", JSON_MANIFEST);
    let entry = setup.register(&a, Some("taken")).await;
    let ws = by_src(&b);
    assert_invalid(setup.local.update_workspace(&ws, new_id("taken")).await);
    assert_eq!(setup.list().await, [entry]);
}

#[tokio::test]
async fn reads_the_content_format_again_on_update() {
    let setup = Setup::new();
    let src = setup.workspace("routing", JSON_MANIFEST);
    setup.register(&src, Some("routing")).await;
    std::fs::write(src.join("workspace.yaml"), MULTI_FILE_MANIFEST).unwrap();
    let entry = setup
        .local
        .update_workspace(&by_id("routing"), description("Multi-file now."))
        .await
        .unwrap();
    assert_eq!(entry.content_format, ContentFormat::MultiFile);
    assert_eq!(setup.list().await, [entry]);
}

#[tokio::test]
async fn deletes_a_workspace() {
    let setup = Setup::new();
    let a = setup.workspace("a", JSON_MANIFEST);
    let b = setup.workspace("b", JSON_MANIFEST);
    let c = setup.workspace("c", JSON_MANIFEST);
    for (id, src) in [("a", &a), ("b", &b), ("c", &c)] {
        setup.register(src, Some(id)).await;
    }
    let local = &setup.local;
    local.delete_workspace(&by_id("a")).await.unwrap();
    local.delete_workspace(&by_src(&b)).await.unwrap();
    assert_not_found(local.delete_workspace(&by_both("c", &a)).await);
    local.delete_workspace(&by_both("c", &c)).await.unwrap();
    assert_eq!(setup.list().await, []);
    assert_not_found(local.delete_workspace(&by_id("a")).await);
    assert_invalid(local.delete_workspace(&LocalWorkspaceRef::default()).await);
    // The files stay.
    assert!(a.join("workspace.yaml").exists());
}

#[tokio::test]
async fn keeps_the_registry_across_reloads() {
    let setup = Setup::new();
    let src = setup.workspace("routing", JSON_MANIFEST);
    let params = WorkspaceRegisterParams {
        description: Some("Routes vehicles.".to_owned()),
        ..register_params(&src, Some("routing"))
    };
    let entry = setup.local.register_workspace(params).await.unwrap();
    let reloaded = Local::with_home(setup.root.path().join(".fico"));
    assert_eq!(
        reloaded.list_workspaces().await.unwrap(),
        std::slice::from_ref(&entry)
    );

    let dir = setup.registry_dir();
    let saved: Value =
        serde_json::from_slice(&std::fs::read(dir.join("registry.json")).unwrap()).unwrap();
    assert_eq!(
        saved,
        json!({
            "workspaces": [{
                "workspace_id": "routing",
                "src": src,
                "content_format": "json",
                "created_at": entry.created_at,
                "updated_at": entry.updated_at,
                "description": "Routes vehicles.",
            }],
        })
    );
    // No temporary file is left behind.
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, ["registry.json", "registry.lock"]);
}

#[tokio::test]
async fn reads_registries_that_other_versions_wrote() {
    let setup = Setup::new();
    let dir = setup.registry_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("registry.json");
    let registry = json!({
        "version": 2,
        "workspaces": [{
            "workspace_id": "routing",
            "src": "/srv/routing",
            "content_format": "csv-archive",
            "created_at": "2026-10-13T12:00:00.000000Z",
            "updated_at": "2026-10-13T12:00:00.000000Z",
            "tags": ["new"],
        }],
    });
    std::fs::write(&file, registry.to_string()).unwrap();
    let list = setup.list().await;
    assert_eq!(list[0].workspace_id, "routing");
    assert_eq!(list[0].content_format, ContentFormat::Unknown);

    // A change writes back what this version does not know.
    let src = setup.workspace("trucks", JSON_MANIFEST);
    let trucks = setup.register(&src, Some("trucks")).await;
    let saved: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    let mut expected = registry.clone();
    expected["workspaces"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::to_value(&trucks).unwrap());
    assert_eq!(saved, expected);

    // A file with nothing in it is an empty registry.
    std::fs::write(&file, " \n").unwrap();
    assert_eq!(setup.list().await, []);

    std::fs::write(&file, "{\"workspaces\": [").unwrap();
    let err = setup.local.list_workspaces().await.unwrap_err();
    assert!(is_io(&err, &file, io::ErrorKind::InvalidData), "{err}");
}

#[tokio::test]
async fn keeps_unknown_keys_of_a_changed_entry() {
    let setup = Setup::new();
    let src = setup.workspace("routing", MULTI_FILE_MANIFEST);
    let dir = setup.registry_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("registry.json");
    let registry = json!({
        "workspaces": [{
            "workspace_id": "routing",
            "src": src,
            "content_format": "csv-archive",
            "created_at": "2026-10-13T12:00:00.000000Z",
            "updated_at": "2026-10-13T12:00:00.000000Z",
            "tags": ["new"],
        }],
    });
    std::fs::write(&file, registry.to_string()).unwrap();
    let updated = setup
        .local
        .update_workspace(&by_id("routing"), description("Routes vehicles."))
        .await
        .unwrap();
    let saved: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    let entry = &saved["workspaces"][0];
    assert_eq!(entry["tags"], json!(["new"]));
    assert_eq!(entry["content_format"], "multi-file");
    assert_eq!(entry["description"], "Routes vehicles.");
    assert_eq!(entry["updated_at"], json!(updated.updated_at));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn keeps_every_entry_when_registering_in_parallel() {
    let setup = Setup::new();
    let tasks: Vec<_> = (0..16)
        .map(|i| {
            let src = setup.workspace(&format!("ws-{i}"), JSON_MANIFEST);
            let local = setup.local.clone();
            tokio::spawn(async move { local.register_workspace(register_params(&src, None)).await })
        })
        .collect();
    for task in tasks {
        task.await.unwrap().unwrap();
    }
    let mut ids: Vec<String> = setup
        .list()
        .await
        .into_iter()
        .map(|entry| entry.workspace_id)
        .collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 16);
}

/// A change that waits for the registry's lock must not hold a thread of
/// tokio's blocking pool: the change that holds the lock needs one for its
/// file I/O, so waiters that fill the pool would block every change for
/// good.
#[test]
fn waits_for_the_lock_without_holding_blocking_threads() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let setup = Setup::new();
    let finished = runtime.block_on(async {
        let tasks: Vec<_> = (0..8)
            .map(|i| {
                let src = setup.workspace(&format!("ws-{i}"), JSON_MANIFEST);
                let local = setup.local.clone();
                tokio::spawn(
                    async move { local.register_workspace(register_params(&src, None)).await },
                )
            })
            .collect();
        let all = async {
            for task in tasks {
                task.await.unwrap().unwrap();
            }
        };
        tokio::time::timeout(Duration::from_secs(30), all).await
    });
    // Don't wait for blocking threads that a deadlock left behind.
    runtime.shutdown_timeout(Duration::from_secs(1));
    assert!(finished.is_ok(), "the registrations deadlocked");
    let local = setup.local.clone();
    let list = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(async move { local.list_workspaces().await.unwrap() });
    assert_eq!(list.len(), 8);
}

#[tokio::test]
async fn serves_the_register_tool() {
    let setup = Setup::new();
    let src = setup.workspace("routing", MULTI_FILE_MANIFEST);
    let args = json!({ "src": src, "workspace_id": "routing", "description": "Routes vehicles." });
    let result = support::call_tool(setup.context(), "decision_local_workspace_register", args)
        .await
        .unwrap();
    assert_eq!(result["workspace_id"], "routing");
    assert_eq!(result["src"], json!(src));
    assert_eq!(result["content_format"], "multi-file");
    assert_eq!(result["description"], "Routes vehicles.");
    assert_eq!(result["created_at"], result["updated_at"]);

    let err = support::call_tool(
        setup.context(),
        "decision_local_workspace_register",
        json!({}),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ToolError::InvalidArguments(_)), "{err}");
}

#[tokio::test]
async fn serves_the_get_tool() {
    let setup = Setup::new();
    let src = setup.workspace("routing", JSON_MANIFEST);
    setup.register(&src, Some("routing")).await;
    let args = json!({ "workspace_id": "routing" });
    let result = support::call_tool(setup.context(), "decision_local_workspace_get", args)
        .await
        .unwrap();
    assert_eq!(result["src"], json!(src));
    assert_eq!(result["content_format"], "json");
}

#[tokio::test]
async fn serves_the_list_tool() {
    let setup = Setup::new();
    let src = setup.workspace("routing", JSON_MANIFEST);
    setup.register(&src, Some("routing")).await;
    let result = support::call_tool(setup.context(), "decision_local_workspace_list", json!({}))
        .await
        .unwrap();
    assert_eq!(result.as_array().map(Vec::len), Some(1));
    assert_eq!(result[0]["workspace_id"], "routing");
}

#[tokio::test]
async fn serves_the_registered_tool() {
    let setup = Setup::new();
    let src = setup.workspace("routing", JSON_MANIFEST);
    setup.register(&src, Some("routing")).await;
    let name = "decision_local_workspace_registered";
    let result = support::call_tool(setup.context(), name, json!({ "src": src }));
    assert_eq!(result.await.unwrap(), json!(true));
    let result = support::call_tool(setup.context(), name, json!({ "workspace_id": "trucks" }));
    assert_eq!(result.await.unwrap(), json!(false));
}

#[tokio::test]
async fn serves_the_update_tool() {
    let setup = Setup::new();
    let src = setup.workspace("routing", JSON_MANIFEST);
    setup.register(&src, Some("routing")).await;
    let args = json!({
        "workspace_id": "routing",
        "new_workspace_id": "trucks",
        "description": "Routes trucks.",
    });
    let result = support::call_tool(setup.context(), "decision_local_workspace_update", args)
        .await
        .unwrap();
    assert_eq!(result["workspace_id"], "trucks");
    assert_eq!(result["description"], "Routes trucks.");
    assert_eq!(setup.list().await[0].workspace_id, "trucks");
}

#[tokio::test]
async fn serves_the_delete_tool() {
    let setup = Setup::new();
    let src = setup.workspace("routing", JSON_MANIFEST);
    setup.register(&src, Some("routing")).await;
    let args = json!({ "workspace_id": "routing", "src": src });
    let result = support::call_tool(setup.context(), "decision_local_workspace_delete", args)
        .await
        .unwrap();
    assert_eq!(result, Value::Null);
    assert_eq!(setup.list().await, []);
}
