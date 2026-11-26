//! The local registry, `<home>/decision/registry.json`.
//!
//! The CLI and the MCP server can both write the registry, so it is always
//! written atomically: to a temporary file, then renamed over the old one.
//! Every change also holds the lock of `registry.lock`, from its read to its
//! write, so two writers never lose each other's change. Reads take no lock:
//! they see the old file or the new one, never a half-written one. A change
//! writes back what this version does not know, such as keys that a newer
//! version added (see [`Registry`]).

use std::ffi::OsStr;
use std::fs::{File, TryLockError};
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use tokio::io::AsyncWriteExt;

use crate::decision::id::safe_id;
use crate::decision::{LocalWorkspace, LocalWorkspaceRef, Registry};
use crate::{Error, Result};

/// Directory, in the FICO home, that holds the registry.
const DIR: &str = "decision";
/// Name of the registry file.
const FILE: &str = "registry.json";
/// Name of the file whose lock every change holds.
const LOCK_FILE: &str = "registry.lock";
/// How long a change waits before it tries again to take the lock.
const LOCK_RETRY: Duration = Duration::from_millis(10);

/// Reads the registry in `home`. A missing file is an empty registry.
///
/// # Errors
///
/// Returns [`Error::Io`] if the file cannot be read or is not a valid
/// registry.
pub(crate) async fn load(home: &Path) -> Result<Registry> {
    let path = home.join(DIR).join(FILE);
    let bytes = match tokio::fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            return Ok(Registry::default());
        }
        Err(source) => return Err(Error::Io { path, source }),
    };
    // A file with nothing in it is an empty registry.
    if bytes.trim_ascii().is_empty() {
        return Ok(Registry::default());
    }
    serde_json::from_slice(&bytes).map_err(|e| Error::Io {
        path,
        source: io::Error::new(io::ErrorKind::InvalidData, e),
    })
}

/// Writes `registry` to `home`, atomically.
///
/// # Errors
///
/// Returns [`Error::Io`] if the file cannot be written.
pub(crate) async fn save(home: &Path, registry: &Registry) -> Result<()> {
    let dir = home.join(DIR);
    let path = dir.join(FILE);
    let temp = write_temp(&dir, registry).await?;
    if let Err(source) = tokio::fs::rename(&temp, &path).await {
        let _ = tokio::fs::remove_file(&temp).await;
        return Err(Error::Io { path, source });
    }
    Ok(())
}

/// Writes `registry` to a new temporary file in `dir`, flushed to disk, and
/// returns its path. Renaming it over the registry file is the only step
/// that changes the registry.
async fn write_temp(dir: &Path, registry: &Registry) -> Result<PathBuf> {
    let mut bytes = serde_json::to_vec_pretty(registry).map_err(|e| Error::Io {
        path: dir.join(FILE),
        source: io::Error::new(io::ErrorKind::InvalidData, e),
    })?;
    bytes.push(b'\n');
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(|source| Error::Io {
            path: dir.to_owned(),
            source,
        })?;
    let temp = dir.join(format!(".{}.tmp", safe_id("registry")));
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .await
        .map_err(|source| Error::Io {
            path: temp.clone(),
            source,
        })?;
    let written = async {
        file.write_all(&bytes).await?;
        file.flush().await?;
        file.sync_all().await
    }
    .await;
    drop(file);
    if let Err(source) = written {
        let _ = tokio::fs::remove_file(&temp).await;
        return Err(Error::Io { path: temp, source });
    }
    Ok(temp)
}

/// Changes the registry in `home` with `apply`, saves it if it changed, and
/// returns what `apply` returns.
///
/// It holds the registry's lock from the read to the write, so changes that
/// other tasks and processes make at the same time are never lost.
///
/// # Errors
///
/// Returns the errors of `apply`, [`load`] and [`save`], and [`Error::Io`]
/// if the lock cannot be taken. Nothing is saved when `apply` fails.
pub(crate) async fn change<T>(
    home: &Path,
    apply: impl FnOnce(&mut Registry) -> Result<T>,
) -> Result<T> {
    let _lock = lock(home).await?;
    let mut registry = load(home).await?;
    let before = registry.clone();
    let result = apply(&mut registry)?;
    if registry != before {
        save(home, &registry).await?;
    }
    Ok(result)
}

/// Takes the registry's lock in `home`, waiting while another task or
/// process holds it. Dropping the returned file releases it.
///
/// It waits by trying again every [`LOCK_RETRY`], not by blocking a thread
/// on the lock: a wait that held a thread of tokio's blocking pool could
/// take the last one, which the change that holds the lock needs for its
/// file I/O.
async fn lock(home: &Path) -> Result<File> {
    let dir = home.join(DIR);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|source| Error::Io {
            path: dir.clone(),
            source,
        })?;
    let path = dir.join(LOCK_FILE);
    let file = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .await
        .map_err(|source| Error::Io {
            path: path.clone(),
            source,
        })?
        .into_std()
        .await;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(TryLockError::WouldBlock) => tokio::time::sleep(LOCK_RETRY).await,
            Err(TryLockError::Error(source)) => return Err(Error::Io { path, source }),
        }
    }
}

/// Finds the workspace that `workspace` identifies in the registry in
/// `home`.
///
/// # Errors
///
/// Returns [`Error::Invalid`] if `workspace` sets neither field,
/// [`Error::NotFound`] if no registered workspace matches it, and the
/// errors of [`load`].
pub(crate) async fn resolve(home: &Path, workspace: &LocalWorkspaceRef) -> Result<LocalWorkspace> {
    let lookup = Lookup::new(workspace).await?;
    let registry = load(home).await?;
    lookup
        .find(&registry)
        .cloned()
        .ok_or_else(|| lookup.not_found())
}

/// What a [`LocalWorkspaceRef`] matches in the registry: its ID and its
/// normalized source directory, each left out when it is unset or empty.
#[derive(Debug, Clone)]
pub(crate) struct Lookup {
    workspace_id: Option<String>,
    src: Option<PathBuf>,
}

impl Lookup {
    /// The lookup for `workspace`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] if `workspace` sets neither field, and the
    /// errors of [`normalize`].
    pub(crate) async fn new(workspace: &LocalWorkspaceRef) -> Result<Lookup> {
        let workspace_id = workspace.workspace_id.clone().filter(|id| !id.is_empty());
        let src = match workspace.src.as_deref() {
            Some(src) if !src.as_os_str().is_empty() => Some(normalize(src).await?),
            _ => None,
        };
        if workspace_id.is_none() && src.is_none() {
            return Err(Error::Invalid {
                reason: "set `workspace_id` or `src` to identify the local workspace".to_owned(),
            });
        }
        Ok(Lookup { workspace_id, src })
    }

    /// The ID it matches, if it has one.
    pub(crate) fn workspace_id(&self) -> Option<&str> {
        self.workspace_id.as_deref()
    }

    /// The normalized source directory it matches, if it has one.
    pub(crate) fn src(&self) -> Option<&Path> {
        self.src.as_deref()
    }

    /// The first registered workspace that matches: with both an ID and a
    /// directory, both must match.
    pub(crate) fn find<'a>(&self, registry: &'a Registry) -> Option<&'a LocalWorkspace> {
        let mut workspaces = registry.workspaces.iter().map(|entry| &entry.workspace);
        workspaces.find(|workspace| {
            self.workspace_id
                .as_ref()
                .is_none_or(|id| *id == workspace.workspace_id)
                && self.src.as_ref().is_none_or(|src| *src == workspace.src)
        })
    }

    /// The error for a lookup that matches nothing.
    pub(crate) fn not_found(&self) -> Error {
        let id = match (&self.workspace_id, &self.src) {
            (Some(id), Some(src)) => format!("{id} at {}", src.display()),
            (Some(id), None) => id.clone(),
            (None, Some(src)) => src.display().to_string(),
            (None, None) => String::new(),
        };
        Error::NotFound {
            kind: "local workspace",
            id,
        }
    }
}

/// `src` as the registry stores it: absolute, against the current directory,
/// with `.` and `..` resolved lexically (a `..` drops the name before it),
/// and with the symlinks in its longest existing part resolved. So every
/// path to a directory finds the same entry, even once the directory is
/// gone. On Windows, resolving symlinks gives the `\\?\` form of the path,
/// which the registry keeps.
///
/// # Errors
///
/// Returns [`Error::Invalid`] if `src` is empty, and [`Error::Io`] if the
/// current directory cannot be read.
pub(crate) async fn normalize(src: &Path) -> Result<PathBuf> {
    if src.as_os_str().is_empty() {
        return Err(Error::Invalid {
            reason: "`src` must not be empty".to_owned(),
        });
    }
    let absolute = std::path::absolute(src).map_err(|source| Error::Io {
        path: src.to_owned(),
        source,
    })?;
    let mut lexical = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                lexical.pop();
            }
            component => lexical.push(component),
        }
    }
    // The part that exists has its symlinks resolved, and the rest is
    // appended as written.
    let mut existing = lexical.as_path();
    let mut rest: Vec<&OsStr> = Vec::new();
    loop {
        if let Ok(canonical) = tokio::fs::canonicalize(existing).await {
            return Ok(rest
                .iter()
                .rev()
                .fold(canonical, |path, name| path.join(name)));
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name);
                existing = parent;
            }
            _ => return Ok(lexical.clone()),
        }
    }
}

#[cfg(test)]
pub(super) mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;
    use crate::decision::{ContentFormat, RegistryEntry};

    /// A new empty directory, removed when dropped. The tests of
    /// `workspace.rs` use it too.
    pub(in crate::decision::local) struct TempDir(pub(in crate::decision::local) PathBuf);

    impl TempDir {
        pub(in crate::decision::local) fn new() -> TempDir {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("fico-sdk-registry-{}-{n}", std::process::id()));
            // Left over from an earlier test process with the same ID.
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir(&path).unwrap();
            TempDir(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A registry with a workspace for each of `ids`.
    fn registry_of(ids: &[&str]) -> Registry {
        let mut registry = Registry::default();
        registry.workspaces = ids
            .iter()
            .map(|id| {
                RegistryEntry::new(LocalWorkspace {
                    workspace_id: (*id).to_owned(),
                    src: PathBuf::from(format!("/srv/{id}")),
                    content_format: ContentFormat::Json,
                    created_at: "2026-10-13T12:00:00.000000Z".to_owned(),
                    updated_at: "2026-10-13T12:00:00.000000Z".to_owned(),
                    description: None,
                })
            })
            .collect();
        registry
    }

    /// The workspaces of the registry in `home`.
    async fn loaded(home: &Path) -> Vec<LocalWorkspace> {
        let registry = load(home).await.unwrap();
        registry
            .workspaces
            .into_iter()
            .map(|entry| entry.workspace)
            .collect()
    }

    /// The workspaces of `registry`.
    fn workspaces(registry: Registry) -> Vec<LocalWorkspace> {
        registry
            .workspaces
            .into_iter()
            .map(|entry| entry.workspace)
            .collect()
    }

    #[tokio::test]
    async fn a_crash_before_the_rename_keeps_the_old_registry() {
        let home = TempDir::new();
        save(&home.0, &registry_of(&["old"])).await.unwrap();
        // A writer that stops between writing its temporary file and
        // renaming it leaves only that file behind, complete or not.
        let dir = home.0.join(DIR);
        let temp = write_temp(&dir, &registry_of(&["new"])).await.unwrap();
        std::fs::write(dir.join(".registry-cut.tmp"), r#"{"workspaces": [{"wor"#).unwrap();
        assert_eq!(loaded(&home.0).await, workspaces(registry_of(&["old"])));

        // Later changes start from the old registry.
        change(&home.0, |registry| {
            registry
                .workspaces
                .extend(registry_of(&["next"]).workspaces);
            Ok(())
        })
        .await
        .unwrap();
        assert_eq!(
            loaded(&home.0).await,
            workspaces(registry_of(&["old", "next"]))
        );
        assert!(temp.exists());
    }

    #[tokio::test]
    async fn saves_only_successful_changes() {
        let home = TempDir::new();
        let file = home.0.join(DIR).join(FILE);
        change(&home.0, |_| Ok(())).await.unwrap();
        assert!(!file.exists());

        let err = change(&home.0, |registry| {
            *registry = registry_of(&["lost"]);
            Err::<(), _>(Error::Invalid {
                reason: "no".to_owned(),
            })
        })
        .await
        .unwrap_err();
        assert!(matches!(err, Error::Invalid { .. }), "{err}");
        assert!(!file.exists());
        assert!(home.0.join(DIR).join(LOCK_FILE).exists());
    }

    #[tokio::test]
    async fn normalizes_sources() {
        let root = TempDir::new();
        let canonical = std::fs::canonicalize(&root.0).unwrap();
        std::fs::create_dir(root.0.join("ws")).unwrap();
        let normal = |path: PathBuf| async move { normalize(&path).await.unwrap() };
        assert_eq!(
            normal(root.0.join("ws/./sub/..")).await,
            canonical.join("ws")
        );
        // A missing directory gets the canonical path of the part that
        // exists.
        assert_eq!(
            normal(root.0.join("gone/deeper/../")).await,
            canonical.join("gone")
        );
        assert_eq!(
            normal(PathBuf::from(".")).await,
            std::env::current_dir().unwrap().canonicalize().unwrap()
        );
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.0.join("ws"), root.0.join("link")).unwrap();
            assert_eq!(normal(root.0.join("link")).await, canonical.join("ws"));
        }
        let err = normalize(Path::new("")).await.unwrap_err();
        assert!(matches!(err, Error::Invalid { .. }), "{err}");
    }
}
