//! tar.gz archives: packing workspace binaries and inputs, unpacking
//! outputs and examples.
//!
//! Packing and unpacking block, so they run in `spawn_blocking`. Unpacking
//! never writes outside the destination: it checks the whole archive
//! first, and rejects entries with absolute paths or `..` components, links
//! and special files.

use std::fs::{self, File};
use std::io;
use std::path::{Component, Path, PathBuf};

use flate2::Compression;
use flate2::read::MultiGzDecoder;
use flate2::write::GzEncoder;

use crate::{Error, Result};

/// Packs every file under `dir` into a tar.gz, with paths relative to
/// `dir`.
///
/// Only files are stored, so empty directories are left out. A symbolic
/// link to a file is stored as a file with the target's contents. Links to
/// directories, broken links and special files such as sockets are skipped.
///
/// # Errors
///
/// Returns [`Error::Io`] if a directory or file cannot be read.
pub(crate) async fn pack_dir(dir: PathBuf) -> Result<Vec<u8>> {
    blocking(dir.clone(), move || {
        let mut names = Vec::new();
        collect_files(&dir, Path::new(""), &mut names)?;
        let files: Vec<(PathBuf, PathBuf)> = names
            .into_iter()
            .map(|name| {
                let path = dir.join(&name);
                (name, path)
            })
            .collect();
        pack(&files, &dir)
    })
    .await
}

/// Packs `files` into a tar.gz, in order: each `(name, path)` stores the
/// file at `path` under the relative path `name`, so a file can be stored
/// under another name. A symbolic link is stored as a file with the
/// target's contents.
///
/// # Errors
///
/// Returns [`Error::Invalid`] if a name is absolute or has `..`, and
/// [`Error::Io`] if a file cannot be read.
pub(crate) async fn pack_files(files: Vec<(PathBuf, PathBuf)>) -> Result<Vec<u8>> {
    if let Some((name, _)) = files.iter().find(|(name, _)| !stays_inside(name)) {
        return Err(Error::Invalid {
            reason: format!(
                "`{}` is not a relative path inside the archive",
                name.display()
            ),
        });
    }
    let first = files
        .first()
        .map(|(_, path)| path.clone())
        .unwrap_or_default();
    blocking(first.clone(), move || pack(&files, &first)).await
}

/// Unpacks the tar.gz `archive` into `dest`, creating it if needed. Files
/// already in `dest` with the same paths are overwritten.
///
/// Nothing is written until the whole archive is checked: it may only hold
/// files and directories, at relative paths without `..`. Links are
/// rejected wherever they point. File modes are kept without group and
/// other write permission, times are kept, and owners are not.
///
/// # Errors
///
/// Returns [`Error::Invalid`] if the archive is malformed, or an entry is
/// not a file or directory or would land outside `dest`, and [`Error::Io`]
/// if a file cannot be written.
pub(crate) async fn unpack(archive: Vec<u8>, dest: PathBuf) -> Result<()> {
    blocking(dest.clone(), move || {
        check(&archive)?;
        let mut tar = tar::Archive::new(MultiGzDecoder::new(archive.as_slice()));
        // `tar` applies the archive's modes and ignores the umask, so clear
        // group and other write, as Python's `tar` filter does.
        tar.set_mask(0o022);
        // `tar` creates `dest`, refuses to write through links already in
        // it, and replaces existing files instead of writing into them.
        tar.unpack(&dest).map_err(io_error(&dest))
    })
    .await
}

/// Checks that every entry of `archive` is a file or directory at a path
/// inside the destination. `tar` alone would skip `..` entries, strip
/// leading `/` and create links to anywhere.
fn check(archive: &[u8]) -> Result<()> {
    let malformed = |error: io::Error| Error::Invalid {
        reason: format!("malformed tar.gz archive: {error}"),
    };
    let mut archive = tar::Archive::new(MultiGzDecoder::new(archive));
    for entry in archive.entries().map_err(malformed)? {
        let entry = entry.map_err(malformed)?;
        let kind = entry.header().entry_type();
        // Metadata that `tar` applies to other entries or ignores, such as
        // the comment that `git archive` writes.
        if kind.is_pax_global_extensions()
            || kind.is_pax_local_extensions()
            || kind.is_gnu_longname()
            || kind.is_gnu_longlink()
        {
            continue;
        }
        let path = entry.path().map_err(malformed)?;
        let problem = if kind.is_symlink() {
            "is a symbolic link"
        } else if kind.is_hard_link() {
            "is a hard link"
        } else if !(kind.is_file() || kind.is_dir() || kind.is_contiguous() || kind.is_gnu_sparse())
        {
            "is not a file or directory"
        } else if !stays_inside(&path) {
            "is outside the destination"
        } else {
            continue;
        };
        return Err(Error::Invalid {
            reason: format!("archive entry `{}` {problem}", path.display()),
        });
    }
    Ok(())
}

/// Runs `f` on tokio's blocking pool. A panic in `f` resumes here. `path`
/// names the error if the runtime shuts down before `f` runs.
async fn blocking<T, F>(path: PathBuf, f: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(result) => result,
        Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
        Err(error) => Err(Error::Io {
            path,
            source: io::Error::other(error),
        }),
    }
}

/// Appends the files under `root.join(dir)` to `files`, as paths relative
/// to `root`, sorted by name so that archives are reproducible. Only real
/// directories are walked into, so links can't make it loop.
fn collect_files(root: &Path, dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    let path = root.join(dir);
    let mut entries = fs::read_dir(&path)
        .and_then(|entries| entries.collect::<io::Result<Vec<_>>>())
        .map_err(io_error(&path))?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let name = dir.join(entry.file_name());
        let file_type = entry.file_type().map_err(io_error(&entry.path()))?;
        if file_type.is_dir() {
            collect_files(root, &name, files)?;
        } else if file_type.is_file() || fs::metadata(entry.path()).is_ok_and(|meta| meta.is_file())
        {
            files.push(name);
        }
    }
    Ok(())
}

/// Packs `files`, each a `(name, path)`, into a tar.gz. `context` names the
/// error if the archive itself cannot be written.
fn pack(files: &[(PathBuf, PathBuf)], context: &Path) -> Result<Vec<u8>> {
    let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::default()));
    // Plain entries, which any tar can read.
    builder.sparse(false);
    for (name, path) in files {
        File::open(path)
            .and_then(|mut file| builder.append_file(name, &mut file))
            .map_err(io_error(path))?;
    }
    builder
        .into_inner()
        .and_then(GzEncoder::finish)
        .map_err(io_error(context))
}

/// Whether `path` is relative and has no `..`, so that joining it to a
/// directory stays inside that directory.
fn stays_inside(path: &Path) -> bool {
    path.components()
        .all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
}

/// Turns an I/O error on `path` into [`Error::Io`].
fn io_error(path: &Path) -> impl FnOnce(io::Error) -> Error {
    let path = path.to_path_buf();
    move |source| Error::Io { path, source }
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::sync::atomic::{AtomicU32, Ordering};

    use tar::{EntryType, Header};

    use super::*;

    /// A new directory under the system's temp directory, removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> TempDir {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("fico-sdk-archive-{}-{n}", std::process::id()));
            // Left over from an earlier test process with the same ID.
            let _ = fs::remove_dir_all(&path);
            fs::create_dir(&path).unwrap();
            TempDir(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Writes `files`, as (path, contents), under `dir`.
    fn write_files(dir: &Path, files: &[(&str, &str)]) {
        for (name, contents) in files {
            let path = dir.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }
    }

    /// The entries of `archive`, as (path, contents), in order.
    fn entries(archive: &[u8]) -> Vec<(String, String)> {
        let mut archive = tar::Archive::new(MultiGzDecoder::new(archive));
        let entries = archive.entries().unwrap().map(|entry| {
            let mut entry = entry.unwrap();
            let mut contents = String::new();
            entry.read_to_string(&mut contents).unwrap();
            (entry.path().unwrap().display().to_string(), contents)
        });
        entries.collect()
    }

    fn owned(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        let pairs = pairs.iter().map(|(a, b)| (a.to_string(), b.to_string()));
        pairs.collect()
    }

    /// A tar.gz of `entries`, as (path, type, link target, contents), with
    /// paths that `tar::Builder` would refuse to write.
    fn raw_archive(entries: &[(&str, EntryType, &str, &str)]) -> Vec<u8> {
        let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::default()));
        for &(path, kind, link, contents) in entries {
            let mut header = Header::new_gnu();
            header.as_old_mut().name[..path.len()].copy_from_slice(path.as_bytes());
            header.as_old_mut().linkname[..link.len()].copy_from_slice(link.as_bytes());
            header.set_entry_type(kind);
            header.set_mode(if kind.is_dir() { 0o755 } else { 0o644 });
            header.set_size(contents.len() as u64);
            header.set_cksum();
            builder.append(&header, contents.as_bytes()).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    #[tokio::test]
    async fn round_trips_nested_directories_and_empty_files() {
        let src = TempDir::new();
        let files = [
            ("a.txt", "a"),
            ("empty", ""),
            ("sub/b.json", "{}"),
            ("sub/deep/c.csv", "x,y"),
        ];
        write_files(&src.0, &files);
        fs::create_dir(src.0.join("sub/void")).unwrap();
        let archive = pack_dir(src.0.clone()).await.unwrap();

        let dest = TempDir::new();
        let out = dest.0.join("out");
        unpack(archive, out.clone()).await.unwrap();
        for (name, contents) in files {
            assert_eq!(fs::read_to_string(out.join(name)).unwrap(), contents);
        }
        assert!(!out.join("sub/void").exists());
    }

    #[tokio::test]
    async fn rejects_entries_that_could_leave_the_destination() {
        let root = TempDir::new();
        write_files(&root.0, &[("secret", "secret")]);
        let dest = root.0.join("dest");
        let cases = [
            ("../x", EntryType::Regular, "", "is outside the destination"),
            (
                "/abs/path",
                EntryType::Regular,
                "",
                "is outside the destination",
            ),
            ("up", EntryType::Symlink, "../secret", "is a symbolic link"),
            ("inside", EntryType::Symlink, "ok.txt", "is a symbolic link"),
            ("hard", EntryType::Link, "../secret", "is a hard link"),
            ("fifo", EntryType::Fifo, "", "is not a file or directory"),
        ];
        for (path, kind, link, problem) in cases {
            // The valid entry first: nothing is written when a later one is
            // rejected.
            let entries = [
                ("ok.txt", EntryType::Regular, "", "ok"),
                (path, kind, link, ""),
            ];
            let err = unpack(raw_archive(&entries), dest.clone())
                .await
                .unwrap_err();
            assert_eq!(
                err.to_string(),
                format!("invalid argument: archive entry `{path}` {problem}")
            );
            assert!(!dest.exists(), "{path}");
        }
        assert!(!root.0.join("x").exists());
        assert_eq!(fs::read_to_string(root.0.join("secret")).unwrap(), "secret");
    }

    #[tokio::test]
    async fn rejects_malformed_archives() {
        let dest = TempDir::new();
        let content = (0..4096u32)
            .map(|i| (i * 7919 % 251) as u8 as char)
            .collect::<String>();
        let mut truncated = raw_archive(&[("big.txt", EntryType::Regular, "", &content)]);
        truncated.truncate(truncated.len() / 2);
        let mut not_tar = GzEncoder::new(Vec::new(), Compression::default());
        io::Write::write_all(&mut not_tar, &[b'x'; 1024]).unwrap();
        let not_tar = not_tar.finish().unwrap();

        for archive in [b"not a tar.gz".to_vec(), truncated, not_tar] {
            let err = unpack(archive, dest.0.clone()).await.unwrap_err();
            assert!(
                err.to_string()
                    .starts_with("invalid argument: malformed tar.gz archive: "),
                "{err}"
            );
        }
        assert_eq!(fs::read_dir(&dest.0).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn clears_group_and_other_write_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::default()));
        for (path, kind, mode) in [
            ("open/", EntryType::Directory, 0o777),
            ("open/data.txt", EntryType::Regular, 0o666),
            ("run.sh", EntryType::Regular, 0o755),
        ] {
            let mut header = Header::new_gnu();
            header.set_path(path).unwrap();
            header.set_entry_type(kind);
            header.set_mode(mode);
            header.set_size(0);
            header.set_cksum();
            builder.append(&header, io::empty()).unwrap();
        }
        let archive = builder.into_inner().unwrap().finish().unwrap();

        let dest = TempDir::new();
        unpack(archive, dest.0.clone()).await.unwrap();
        let mode = |path| {
            fs::metadata(dest.0.join(path))
                .unwrap()
                .permissions()
                .mode()
                & 0o777
        };
        assert_eq!(mode("open"), 0o755);
        assert_eq!(mode("open/data.txt"), 0o644);
        assert_eq!(mode("run.sh"), 0o755);
    }

    #[tokio::test]
    async fn overwrites_existing_files() {
        let dest = TempDir::new();
        write_files(&dest.0, &[("a.txt", "old"), ("kept.txt", "kept")]);
        let archive = raw_archive(&[("a.txt", EntryType::Regular, "", "new")]);
        unpack(archive, dest.0.clone()).await.unwrap();
        assert_eq!(fs::read_to_string(dest.0.join("a.txt")).unwrap(), "new");
        assert_eq!(fs::read_to_string(dest.0.join("kept.txt")).unwrap(), "kept");
    }

    #[tokio::test]
    async fn unpacks_directory_entries_and_dot_prefixes() {
        // As `git archive` and `tar -C dir .` write them.
        let archive = raw_archive(&[
            (
                "pax_global_header",
                EntryType::XGlobalHeader,
                "",
                "17 comment=hello\n",
            ),
            ("./", EntryType::Directory, "", ""),
            ("./workspace/", EntryType::Directory, "", ""),
            ("./workspace/main.py", EntryType::Regular, "", "print()"),
        ]);
        let dest = TempDir::new();
        unpack(archive, dest.0.clone()).await.unwrap();
        let main = fs::read_to_string(dest.0.join("workspace/main.py")).unwrap();
        assert_eq!(main, "print()");
        assert!(!dest.0.join("pax_global_header").exists());
    }

    #[tokio::test]
    async fn packs_the_files_under_a_directory() {
        let dir = TempDir::new();
        write_files(
            &dir.0,
            &[
                ("b.txt", "b"),
                ("a/z.txt", "z"),
                ("a/deep/y.txt", "y"),
                ("empty", ""),
            ],
        );
        fs::create_dir(dir.0.join("void")).unwrap();

        let archive = pack_dir(dir.0.clone()).await.unwrap();
        assert_eq!(
            entries(&archive),
            owned(&[
                ("a/deep/y.txt", "y"),
                ("a/z.txt", "z"),
                ("b.txt", "b"),
                ("empty", "")
            ])
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn packs_links_to_files_as_files() {
        use std::os::unix::fs::symlink;

        let outside = TempDir::new();
        write_files(&outside.0, &[("target.txt", "target"), ("tree/x.txt", "x")]);
        let dir = TempDir::new();
        write_files(&dir.0, &[("plain.txt", "plain")]);
        symlink(outside.0.join("target.txt"), dir.0.join("file_link")).unwrap();
        symlink(outside.0.join("tree"), dir.0.join("dir_link")).unwrap();
        symlink(dir.0.join("missing"), dir.0.join("broken_link")).unwrap();

        let archive = pack_dir(dir.0.clone()).await.unwrap();
        assert_eq!(
            entries(&archive),
            owned(&[("file_link", "target"), ("plain.txt", "plain")])
        );
    }

    #[tokio::test]
    async fn fails_on_a_missing_directory() {
        let dir = TempDir::new();
        let missing = dir.0.join("missing");
        let err = pack_dir(missing.clone()).await.unwrap_err();
        assert!(
            matches!(&err, Error::Io { path, .. } if *path == missing),
            "{err}"
        );
    }

    #[tokio::test]
    async fn packs_the_given_files_under_their_names() {
        let root = TempDir::new();
        write_files(
            &root.0,
            &[
                ("main.py", "print()"),
                ("src/lib.py", ""),
                ("left_out.txt", ""),
                ("other/custom.yaml", "files: [main.py]"),
            ],
        );
        let files = vec![
            (PathBuf::from("src/lib.py"), root.0.join("src/lib.py")),
            (PathBuf::from("./main.py"), root.0.join("main.py")),
            (
                PathBuf::from("workspace.yaml"),
                root.0.join("other/custom.yaml"),
            ),
        ];

        let archive = pack_files(files).await.unwrap();
        assert_eq!(
            entries(&archive),
            owned(&[
                ("src/lib.py", ""),
                ("main.py", "print()"),
                ("workspace.yaml", "files: [main.py]")
            ])
        );
    }

    #[tokio::test]
    async fn rejects_names_outside_the_archive() {
        let root = TempDir::new();
        write_files(&root.0, &[("secret", "")]);
        let path = root.0.join("secret");
        for name in [PathBuf::from("../secret"), path.clone()] {
            let err = pack_files(vec![(name.clone(), path.clone())])
                .await
                .unwrap_err();
            assert!(
                matches!(err, Error::Invalid { .. }),
                "{}: {err}",
                name.display()
            );
        }
    }

    #[tokio::test]
    async fn fails_on_files_that_cannot_be_read() {
        let root = TempDir::new();
        let missing = root.0.join("missing.py");
        let files = vec![(PathBuf::from("missing.py"), missing.clone())];
        let err = pack_files(files).await.unwrap_err();
        assert!(
            matches!(&err, Error::Io { path, .. } if *path == missing),
            "{err}"
        );
    }
}
