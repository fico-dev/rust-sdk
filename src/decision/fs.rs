//! Files on disk: writing them so that a reader never sees a partly written
//! one, listing a directory, and keeping manifest paths inside a workspace.

use std::io::{self, ErrorKind};
use std::path::{Component, Path, PathBuf};

use crate::decision::id;
use crate::{Error, Result};

/// Writes `contents` to `path` through a new file next to it and a rename,
/// creating `path`'s directory if needed. A reader never sees a partly
/// written file, and a write that fails, e.g. on a full disk, leaves the
/// file at `path` as it was. Two writers at the same time never mix their
/// writes: the last rename wins.
///
/// # Errors
///
/// Returns [`Error::Io`] if the directory, the temporary file or the
/// rename fails. The temporary file is removed then.
pub(crate) async fn write_atomically(path: &Path, contents: &[u8]) -> Result<()> {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let temp = path.with_file_name(format!(".{name}.{}", id::safe_id("tmp")));
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|source| Error::Io {
                path: parent.to_path_buf(),
                source,
            })?;
    }
    if let Err(source) = tokio::fs::write(&temp, contents).await {
        let _ = tokio::fs::remove_file(&temp).await;
        return Err(Error::Io { path: temp, source });
    }
    if let Err(source) = tokio::fs::rename(&temp, path).await {
        let _ = tokio::fs::remove_file(&temp).await;
        return Err(Error::Io {
            path: path.to_path_buf(),
            source,
        });
    }
    Ok(())
}

/// `copy.join(path)`, for a `path` that the manifest gives relative to the
/// workspace.
///
/// # Errors
///
/// Returns [`Error::Invalid`] if `path` is absolute or has a `..`, so that
/// a run never reads or writes outside the copy.
pub(crate) fn inside(copy: &Path, path: &str) -> Result<PathBuf> {
    let leaves = Path::new(path).components().any(|component| {
        matches!(
            component,
            Component::Prefix(_) | Component::RootDir | Component::ParentDir
        )
    });
    if leaves {
        return Err(Error::Invalid {
            reason: format!(
                "the manifest's path `{path}` is absolute or has a `..`, so it leaves the \
                 workspace"
            ),
        });
    }
    Ok(copy.join(path))
}

/// The files under `dir`, relative to it and sorted: files and links to
/// files. Links to directories are not followed, so that they can't make
/// it loop. A missing `dir` has none.
pub(crate) fn files_under(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut dirs = vec![PathBuf::new()];
    while let Some(relative) = dirs.pop() {
        let path = dir.join(&relative);
        let entries = match std::fs::read_dir(&path) {
            Ok(entries) => entries,
            Err(source) if source.kind() == ErrorKind::NotFound && relative == Path::new("") => {
                return Ok(files);
            }
            Err(source) => return Err(Error::Io { path, source }),
        };
        for entry in entries {
            let entry = entry.map_err(io_error(&path))?;
            let name = relative.join(entry.file_name());
            let kind = entry.file_type().map_err(io_error(&entry.path()))?;
            if kind.is_dir() {
                dirs.push(name);
            } else if std::fs::metadata(entry.path()).is_ok_and(|meta| meta.is_file()) {
                files.push(name);
            }
        }
    }
    files.sort();
    Ok(files)
}

/// Turns an I/O error on `path` into [`Error::Io`].
fn io_error(path: &Path) -> impl FnOnce(io::Error) -> Error {
    let path = path.to_path_buf();
    move |source| Error::Io { path, source }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    /// A new directory under the system's temp directory, removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> TempDir {
            let path = std::env::temp_dir().join(id::safe_id("fico-sdk-fs"));
            std::fs::create_dir_all(&path).unwrap();
            TempDir(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    #[tokio::test]
    async fn replaces_the_file_and_leaves_no_temporary_file() {
        let dir = TempDir::new();
        let path = dir.0.join("sub").join("workspace.yaml");
        write_atomically(&path, b"first").await.unwrap();
        write_atomically(&path, b"second").await.unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"second");
        assert_eq!(names(&dir.0.join("sub")), ["workspace.yaml"]);
    }

    #[tokio::test]
    async fn keeps_the_file_when_the_rename_fails() {
        let dir = TempDir::new();
        // A directory at the path makes the rename fail after the
        // temporary file was written.
        let path = dir.0.join("workspace.yaml");
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("keep"), "kept").unwrap();
        let err = write_atomically(&path, b"new").await.unwrap_err();
        assert!(
            matches!(&err, Error::Io { path: failed, .. } if *failed == path),
            "{err:?}"
        );
        assert_eq!(std::fs::read_to_string(path.join("keep")).unwrap(), "kept");
        assert_eq!(names(&dir.0), ["workspace.yaml"]);
    }
}
