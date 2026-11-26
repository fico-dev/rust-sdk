//! The dependency cache, in `<home>/cache/deps`: the Python packages that
//! [`Platform::push_workspace`](crate::decision::platform::Platform::push_workspace)
//! bundles, kept so that later pushes reuse them.
//!
//! [`info`] shows the cache and [`clear`] empties it. Each entry is a
//! directory named after its key, with the package's installed files,
//! compressed, as `installed.tar.gz`, and a `cache_info.json` that says
//! which package it is and when a push last used it. Push keeps at most
//! 2,000 entries and 5 GiB, and evicts the least recently used first.
//!
//! The functions take the FICO home, as
//! [`Local::with_home`](crate::decision::local::Local::with_home) does, so
//! that push and its host see the same cache. They need no profile.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use aws_lc_rs::digest::{SHA256, digest};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::rand::random_fraction;
use crate::time::{parse_rfc3339, rfc3339};
use crate::{Error, Result};

/// The cache's directory in the FICO home.
const CACHE_DIR: &str = "cache";
/// The directory of the dependency entries in the cache.
const DEPS_DIR: &str = "deps";
/// The file of an entry that describes it.
const INFO_FILE: &str = "cache_info.json";
/// The file of an entry that holds the package's installed files.
const TARBALL_FILE: &str = "installed.tar.gz";
/// What the name of a directory holds while an entry is staged in it.
const STAGING: &str = "-tmp-";

/// What the dependency cache holds, as [`info`] reads it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct CacheInfo {
    /// The cache's directory, `<home>/cache/deps`.
    pub path: PathBuf,
    /// The number of cached dependencies.
    pub num_dependencies: u64,
    /// The size of every file in the cache, in bytes.
    pub total_size_bytes: u64,
    /// The cached dependencies whose description can be read, most recently
    /// used first.
    pub dependencies: Vec<CachedDependency>,
}

/// A Python package in the dependency cache.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct CachedDependency {
    /// The entry's key, which is the name of its directory.
    pub key: String,
    /// The package's name, e.g. `pydantic-core`.
    pub name: String,
    /// The package's version, e.g. `2.23.4`.
    pub version: String,
    /// The Python version it was installed for, e.g. `3.11`.
    pub python_version: String,
    /// The platform it was installed for, e.g. `aarch64-manylinux_2_34`.
    pub platform: String,
    /// When a push last used it, as an RFC 3339 timestamp.
    pub last_used_at: String,
    /// The size of its compressed files, in bytes.
    pub size_bytes: u64,
}

/// What [`clear`] deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct CacheCleared {
    /// The number of dependencies deleted.
    pub deleted_dependencies: u64,
    /// The size of the files deleted, in bytes.
    pub deleted_bytes: u64,
}

/// Reads the dependency cache in the FICO home `home`.
///
/// Every entry counts toward the number and the size, but only the entries
/// whose `cache_info.json` can be read are listed. A missing cache is
/// empty.
///
/// # Errors
///
/// Returns [`Error::Io`] if the cache's directory cannot be read.
pub async fn info(home: &Path) -> Result<CacheInfo> {
    let deps = deps_dir(home);
    blocking(deps.clone(), move || {
        let entries = entries(&deps)?;
        let total_size_bytes = entries.iter().map(|(_, dir)| dir_size(dir)).sum();
        let mut dependencies: Vec<CachedDependency> = entries
            .iter()
            .filter_map(|(key, dir)| {
                let info: EntryInfo =
                    serde_json::from_slice(&fs::read(dir.join(INFO_FILE)).ok()?).ok()?;
                let size_bytes = fs::metadata(dir.join(TARBALL_FILE)).map_or(0, |meta| meta.len());
                Some(CachedDependency {
                    key: key.clone(),
                    name: info.name,
                    version: info.version,
                    python_version: info.python_version,
                    platform: info.platform,
                    last_used_at: info.last_used_at,
                    size_bytes,
                })
            })
            .collect();
        // Most recently used first, by the timestamps as strings, which
        // sort by time when the SDK writes them.
        dependencies.sort_by(|a, b| {
            (&b.last_used_at, &b.name, &b.version).cmp(&(&a.last_used_at, &a.name, &a.version))
        });
        Ok(CacheInfo {
            path: deps,
            num_dependencies: entries.len() as u64,
            total_size_bytes,
            dependencies,
        })
    })
    .await
}

/// Deletes the dependency cache in the FICO home `home`, `<home>/cache`,
/// and leaves an empty one. Returns what it deleted, counted as [`info`]
/// counts it. A missing cache gives zeros.
///
/// # Errors
///
/// Returns [`Error::Io`] if the cache cannot be read or deleted.
pub async fn clear(home: &Path) -> Result<CacheCleared> {
    let cache = home.join(CACHE_DIR);
    blocking(cache.clone(), move || {
        match fs::metadata(&cache) {
            Ok(_) => {}
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                return Ok(CacheCleared {
                    deleted_dependencies: 0,
                    deleted_bytes: 0,
                });
            }
            Err(source) => {
                return Err(Error::Io {
                    path: cache,
                    source,
                });
            }
        }
        let entries = entries(&cache.join(DEPS_DIR))?;
        let deleted_bytes = entries.iter().map(|(_, dir)| dir_size(dir)).sum();
        fs::remove_dir_all(&cache).map_err(io_error(&cache))?;
        let deps = cache.join(DEPS_DIR);
        fs::create_dir_all(&deps).map_err(io_error(&deps))?;
        Ok(CacheCleared {
            deleted_dependencies: entries.len() as u64,
            deleted_bytes,
        })
    })
    .await
}

/// The cache's limits, and when a staging directory counts as left over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Limits {
    /// The most entries to keep.
    pub(crate) max_entries: u64,
    /// The most bytes to keep.
    pub(crate) max_bytes: u64,
    /// How long ago a staging directory must have changed to count as left
    /// over by a push that ended before it stored its entry.
    pub(crate) stale_after: Duration,
}

impl Default for Limits {
    /// 2,000 entries and 5 GiB, and an hour for staging directories.
    fn default() -> Limits {
        Limits {
            max_entries: 2_000,
            max_bytes: 5 * 1024 * 1024 * 1024,
            stale_after: Duration::from_secs(60 * 60),
        }
    }
}

/// A Python package, installed for a Python version and a platform: what a
/// cache entry holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Dependency {
    /// The package's name, as the lockfile writes it.
    pub(crate) name: String,
    /// The package's pinned version.
    pub(crate) version: String,
    /// The Python version it is installed for, e.g. `3.11`.
    pub(crate) python_version: String,
    /// The platform it is installed for, e.g. `aarch64-manylinux_2_34`.
    pub(crate) platform: String,
    /// The package indexes it is installed from, without credentials, or
    /// `None` for PyPI alone.
    pub(crate) sources: Option<String>,
}

impl Dependency {
    /// The key of the dependency's entry: the hex SHA-256 of
    /// `<name>==<version>|<python version>|<platform>`, with the name
    /// lowercased and each run of `-`, `_` and `.` in it turned into `_`, so
    /// that `pydantic-core` and `pydantic_core` share an entry. PEP 503 uses
    /// `-` instead; either works, as no other tool reads this cache.
    ///
    /// A package installed from other indexes than PyPI alone has
    /// `|<sources>` appended, so that it never shares an entry with PyPI's
    /// package of the same name and version.
    pub(crate) fn key(&self) -> String {
        let mut name = String::with_capacity(self.name.len());
        let mut in_run = false;
        for c in self.name.to_lowercase().chars() {
            let separator = matches!(c, '-' | '_' | '.');
            if !separator {
                name.push(c);
            } else if !in_run {
                name.push('_');
            }
            in_run = separator;
        }
        let mut raw = format!(
            "{name}=={}|{}|{}",
            self.version, self.python_version, self.platform
        );
        if let Some(sources) = &self.sources {
            raw.push('|');
            raw.push_str(sources);
        }
        let hash = digest(&SHA256, raw.as_bytes());
        hash.as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }
}

/// The contents of an entry's `cache_info.json`. A key it lacks reads as
/// empty.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct EntryInfo {
    /// When the entry was stored.
    created_at: String,
    /// When a push last used the entry.
    last_used_at: String,
    /// The package's name.
    name: String,
    /// The package's version.
    version: String,
    /// The Python version the package is installed for.
    python_version: String,
    /// The platform the package is installed for.
    platform: String,
}

/// The cached files of `dependency` in the FICO home `home`, or `None` if
/// the cache has none or they cannot be read. A hit updates the entry's
/// `last_used_at`, atomically; if that fails, the hit still counts.
///
/// The files are read here, not later, so that a push that evicts the entry
/// before it bundles them still has them.
pub(crate) async fn lookup(home: &Path, dependency: &Dependency) -> Option<Vec<u8>> {
    let entry = deps_dir(home).join(dependency.key());
    let found = blocking(entry.clone(), move || {
        let info_file = entry.join(INFO_FILE);
        if !info_file.is_file() {
            return Ok(None);
        }
        let Ok(tarball) = fs::read(entry.join(TARBALL_FILE)) else {
            return Ok(None);
        };
        let _ = touch(&info_file);
        Ok(Some(tarball))
    });
    found.await.ok().flatten()
}

/// Sets `last_used_at` in the entry description `info_file` to now, keeping
/// its other keys, through a new file and a rename.
fn touch(info_file: &Path) -> io::Result<()> {
    let mut info: Map<String, Value> = serde_json::from_slice(&fs::read(info_file)?)?;
    info.insert("last_used_at".to_owned(), now().into());
    write_atomically(info_file, &serde_json::to_vec_pretty(&info)?)
}

/// Stores `tarball`, the compressed installed files of `dependency`, in the
/// cache in the FICO home `home`, then evicts the least recently used
/// entries beyond `limits`.
///
/// The entry is written into a staging directory, `<key>-tmp-<random>`,
/// and renamed into place, so that no reader sees a partial entry. An entry
/// that another push stored first is kept. Staging directories that have
/// not changed for `limits.stale_after`, left by a push that ended early,
/// are removed first; newer ones are kept, as a push running at the same
/// time may still be writing them.
///
/// # Errors
///
/// Returns [`Error::Io`] if the entry cannot be written.
pub(crate) async fn store(
    home: &Path,
    dependency: &Dependency,
    tarball: &[u8],
    limits: Limits,
) -> Result<()> {
    let deps = deps_dir(home);
    let key = dependency.key();
    let now = now();
    let info = EntryInfo {
        created_at: now.clone(),
        last_used_at: now,
        name: dependency.name.clone(),
        version: dependency.version.clone(),
        python_version: dependency.python_version.clone(),
        platform: dependency.platform.clone(),
    };
    let tarball = tarball.to_vec();
    blocking(deps.clone(), move || {
        fs::create_dir_all(&deps).map_err(io_error(&deps))?;
        remove_stale_staging(&deps, limits.stale_after);
        let entry = deps.join(&key);
        if !entry.exists() {
            let staging = deps.join(format!("{key}{STAGING}{}", random_suffix()));
            let staged = stage(&staging, &tarball, &info).and_then(|()| {
                match fs::rename(&staging, &entry) {
                    // Another push stored it first.
                    Err(_) if entry.exists() => Ok(()),
                    renamed => renamed.map_err(io_error(&entry)),
                }
            });
            let _ = fs::remove_dir_all(&staging);
            staged?;
        }
        evict(&deps, limits);
        Ok(())
    })
    .await
}

/// Writes an entry with `tarball` and `info` into the new directory
/// `staging`.
fn stage(staging: &Path, tarball: &[u8], info: &EntryInfo) -> Result<()> {
    fs::create_dir(staging).map_err(io_error(staging))?;
    let tarball_file = staging.join(TARBALL_FILE);
    fs::write(&tarball_file, tarball).map_err(io_error(&tarball_file))?;
    let info_file = staging.join(INFO_FILE);
    let info = serde_json::to_vec_pretty(info).unwrap_or_default();
    fs::write(&info_file, info).map_err(io_error(&info_file))
}

/// Removes the staging directories in `deps` that have not changed for
/// `stale_after`. Errors are ignored: the next store tries again.
fn remove_stale_staging(deps: &Path, stale_after: Duration) {
    let Ok(listing) = fs::read_dir(deps) else {
        return;
    };
    for entry in listing.flatten() {
        let staging = entry.file_name().to_string_lossy().contains(STAGING);
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let stale = meta
            .modified()
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age >= stale_after);
        if staging && meta.is_dir() && stale {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

/// Removes the least recently used entries of `deps` until at most
/// `limits.max_entries` entries and `limits.max_bytes` bytes remain. An
/// entry whose `last_used_at` can't be read counts as the oldest. Errors
/// are ignored: the next store evicts again.
fn evict(deps: &Path, limits: Limits) {
    let Ok(entries) = entries(deps) else {
        return;
    };
    let mut entries: Vec<(i64, PathBuf, u64)> = entries
        .into_iter()
        .map(|(_, dir)| {
            let last_used = fs::read(dir.join(INFO_FILE))
                .ok()
                .and_then(|info| serde_json::from_slice::<EntryInfo>(&info).ok())
                .and_then(|info| parse_rfc3339(&info.last_used_at))
                .unwrap_or(i64::MIN);
            let size = dir_size(&dir);
            (last_used, dir, size)
        })
        .collect();
    entries.sort();
    let mut count = entries.len() as u64;
    let mut total: u64 = entries.iter().map(|(_, _, size)| size).sum();
    for (_, dir, size) in entries {
        if count <= limits.max_entries && total <= limits.max_bytes {
            break;
        }
        let _ = fs::remove_dir_all(dir);
        count -= 1;
        total -= size;
    }
}

/// The dependency cache's directory in the FICO home `home`.
fn deps_dir(home: &Path) -> PathBuf {
    home.join(CACHE_DIR).join(DEPS_DIR)
}

/// The entries in `deps`, as their keys and directories, without staging
/// directories. A missing `deps` has none.
fn entries(deps: &Path) -> Result<Vec<(String, PathBuf)>> {
    let listing = match fs::read_dir(deps) {
        Ok(listing) => listing,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(Error::Io {
                path: deps.to_path_buf(),
                source,
            });
        }
    };
    let mut entries = Vec::new();
    for entry in listing {
        let entry = entry.map_err(io_error(deps))?;
        let key = entry.file_name().to_string_lossy().into_owned();
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) && !key.contains(STAGING) {
            entries.push((key, entry.path()));
        }
    }
    Ok(entries)
}

/// The size of every file under `dir`, in bytes. Links are not followed,
/// and what can't be read counts as nothing.
fn dir_size(dir: &Path) -> u64 {
    let Ok(listing) = fs::read_dir(dir) else {
        return 0;
    };
    listing
        .flatten()
        .map(|entry| match entry.file_type() {
            Ok(kind) if kind.is_dir() => dir_size(&entry.path()),
            Ok(kind) if kind.is_file() => entry.metadata().map_or(0, |meta| meta.len()),
            _ => 0,
        })
        .sum()
}

/// Writes `contents` to `path` through a new file next to it and a rename,
/// so that a reader never sees a partly written file.
fn write_atomically(path: &Path, contents: &[u8]) -> io::Result<()> {
    let temp = path.with_file_name(format!(".tmp-{}", random_suffix()));
    let written = File::create_new(&temp)
        .and_then(|mut file| file.write_all(contents))
        .and_then(|()| fs::rename(&temp, path));
    if written.is_err() {
        let _ = fs::remove_file(&temp);
    }
    written
}

/// The current time as an RFC 3339 timestamp in UTC, with microseconds.
fn now() -> String {
    rfc3339(SystemTime::now())
}

/// 13 random lowercase hex digits, for the names of temporary files and
/// directories.
fn random_suffix() -> String {
    format!("{:013x}", (random_fraction() * (1u64 << 52) as f64) as u64)
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

/// Turns an I/O error on `path` into [`Error::Io`].
fn io_error(path: &Path) -> impl FnOnce(io::Error) -> Error {
    let path = path.to_path_buf();
    move |source| Error::Io { path, source }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use serde_json::json;

    use super::*;

    /// A new directory under the system's temp directory, removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> TempDir {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("fico-sdk-cache-{}-{n}", std::process::id()));
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

    fn dependency(name: &str, version: &str) -> Dependency {
        Dependency {
            name: name.to_owned(),
            version: version.to_owned(),
            python_version: "3.11".to_owned(),
            platform: "aarch64-manylinux_2_34".to_owned(),
            sources: None,
        }
    }

    /// Writes an entry for `dependency` into the cache in `home`, last used
    /// at `last_used_at`, with `tarball`.
    fn write_entry(home: &Path, dependency: &Dependency, last_used_at: &str, tarball: &[u8]) {
        let entry = deps_dir(home).join(dependency.key());
        fs::create_dir_all(&entry).unwrap();
        fs::write(entry.join(TARBALL_FILE), tarball).unwrap();
        let info = json!({
            "created_at": "2026-01-01T00:00:00.000000Z",
            "last_used_at": last_used_at,
            "name": dependency.name,
            "version": dependency.version,
            "python_version": dependency.python_version,
            "platform": dependency.platform,
            "extra": "kept",
        });
        fs::write(entry.join(INFO_FILE), info.to_string()).unwrap();
    }

    fn read_info(home: &Path, dependency: &Dependency) -> Value {
        let file = deps_dir(home).join(dependency.key()).join(INFO_FILE);
        serde_json::from_slice(&fs::read(file).unwrap()).unwrap()
    }

    /// The names in the cache's directory in `home`, sorted.
    fn listing(home: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(deps_dir(home))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// `digest` in lowercase hex.
    fn hex(digest: [u8; 32]) -> String {
        digest.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    #[test]
    fn keys_dependencies_by_sha256_with_normalized_names() {
        // The expected keys, computed with Python's `hashlib`. Written as
        // bytes, so that secret scanners don't read the hashes as
        // credentials.
        let pydantic = hex([
            0x20, 0x69, 0x54, 0xb6, 0x24, 0x47, 0x9a, 0x61, 0x44, 0xbb, 0xe7, 0x88, 0x6f, 0x17,
            0x29, 0x31, 0x80, 0x5b, 0x24, 0x1d, 0x44, 0x23, 0xdf, 0xe0, 0xd9, 0x32, 0x3a, 0x54,
            0x98, 0x57, 0xc5, 0x7f,
        ]);
        assert_eq!(dependency("Pydantic-Core", "2.23.4").key(), pydantic);
        assert_eq!(dependency("pydantic_core", "2.23.4").key(), pydantic);
        let mut zope = dependency("zope.interface", "7.1.1");
        zope.python_version = "3.12".to_owned();
        zope.platform = "x86_64-manylinux_2_34".to_owned();
        let expected = hex([
            0x9a, 0x90, 0xa4, 0xfc, 0x1b, 0x61, 0x3a, 0x7b, 0xd5, 0xdd, 0x63, 0x61, 0xc7, 0x53,
            0x03, 0x8a, 0xaf, 0x9e, 0xf7, 0x30, 0x09, 0xa2, 0x08, 0x30, 0x68, 0x2f, 0x82, 0xc3,
            0xeb, 0x0d, 0x49, 0x64,
        ]);
        assert_eq!(zope.key(), expected);
        let mut runs = dependency("A__B--.c", "1.0");
        runs.python_version = "3.10".to_owned();
        runs.platform = "x86_64-manylinux_2_34".to_owned();
        let expected = hex([
            0xb2, 0xb9, 0xb2, 0x30, 0x7a, 0x40, 0xf7, 0x06, 0x28, 0x05, 0x41, 0x57, 0xf1, 0xfc,
            0x6f, 0x71, 0x32, 0xf6, 0x2e, 0x51, 0x73, 0xea, 0x80, 0x08, 0x78, 0x4c, 0x8f, 0xf3,
            0xf4, 0x88, 0xe6, 0x03,
        ]);
        assert_eq!(runs.key(), expected);
        assert_ne!(dependency("pydantic", "2.23.4").key(), pydantic);
    }

    #[tokio::test]
    async fn a_hit_returns_the_files_and_updates_last_used_at() {
        let home = TempDir::new();
        let ortools = dependency("ortools", "9.12.4544");
        assert_eq!(lookup(&home.0, &ortools).await, None);

        write_entry(&home.0, &ortools, "2020-01-01T00:00:00.000000Z", b"files");
        assert_eq!(lookup(&home.0, &ortools).await.unwrap(), b"files");
        let info = read_info(&home.0, &ortools);
        let last_used_at = info["last_used_at"].as_str().unwrap();
        assert!(last_used_at > "2026", "{last_used_at}");
        assert!(parse_rfc3339(last_used_at).is_some(), "{last_used_at}");
        assert_eq!(info["created_at"], "2026-01-01T00:00:00.000000Z");
        assert_eq!(info["extra"], "kept");
        // The update leaves no temporary file.
        let entry = deps_dir(&home.0).join(ortools.key());
        assert_eq!(fs::read_dir(entry).unwrap().count(), 2);

        // An entry without its description, or without its files, is a
        // miss.
        let numpy = dependency("numpy", "2.1.0");
        write_entry(&home.0, &numpy, "2020-01-01T00:00:00.000000Z", b"numpy");
        let entry = deps_dir(&home.0).join(numpy.key());
        fs::remove_file(entry.join(TARBALL_FILE)).unwrap();
        assert_eq!(lookup(&home.0, &numpy).await, None);
        fs::write(entry.join(TARBALL_FILE), b"numpy").unwrap();
        fs::remove_file(entry.join(INFO_FILE)).unwrap();
        assert_eq!(lookup(&home.0, &numpy).await, None);

        // A description that can't be updated still gives a hit.
        fs::write(entry.join(INFO_FILE), "not json").unwrap();
        assert_eq!(lookup(&home.0, &numpy).await.unwrap(), b"numpy");
    }

    #[tokio::test]
    async fn stores_an_entry() {
        let home = TempDir::new();
        let ortools = dependency("ortools", "9.12.4544");
        store(&home.0, &ortools, b"files", Limits::default())
            .await
            .unwrap();
        assert_eq!(listing(&home.0), [ortools.key()]);
        let entry = deps_dir(&home.0).join(ortools.key());
        assert_eq!(fs::read(entry.join(TARBALL_FILE)).unwrap(), b"files");
        let info = read_info(&home.0, &ortools);
        assert_eq!(info["name"], "ortools");
        assert_eq!(info["version"], "9.12.4544");
        assert_eq!(info["python_version"], "3.11");
        assert_eq!(info["platform"], "aarch64-manylinux_2_34");
        assert_eq!(info["created_at"], info["last_used_at"]);
        assert!(parse_rfc3339(info["created_at"].as_str().unwrap()).is_some());
        assert_eq!(lookup(&home.0, &ortools).await.unwrap(), b"files");
    }

    #[tokio::test]
    async fn keeps_an_entry_that_was_stored_first() {
        let home = TempDir::new();
        let ortools = dependency("ortools", "9.12.4544");
        write_entry(&home.0, &ortools, "2020-01-01T00:00:00.000000Z", b"first");
        store(&home.0, &ortools, b"second", Limits::default())
            .await
            .unwrap();
        assert_eq!(listing(&home.0), [ortools.key()]);
        assert_eq!(lookup(&home.0, &ortools).await.unwrap(), b"first");
    }

    #[tokio::test]
    async fn removes_only_stale_staging_directories() {
        let home = TempDir::new();
        let staging = deps_dir(&home.0).join(format!("abc{STAGING}123"));
        fs::create_dir_all(&staging).unwrap();
        fs::write(staging.join(TARBALL_FILE), b"partial").unwrap();
        let ortools = dependency("ortools", "9.12.4544");

        // A push may still be writing it.
        store(&home.0, &ortools, b"files", Limits::default())
            .await
            .unwrap();
        assert!(staging.is_dir());

        let limits = Limits {
            stale_after: Duration::ZERO,
            ..Limits::default()
        };
        store(&home.0, &ortools, b"files", limits).await.unwrap();
        assert_eq!(listing(&home.0), [ortools.key()]);
    }

    /// A cache in a temp home with the entries `a`, `b` and `c`, last used
    /// in that order, each with 10 bytes of files, and `broken`, whose
    /// description can't be read.
    fn full_cache() -> (TempDir, [Dependency; 4]) {
        let home = TempDir::new();
        let deps = ["a", "b", "c", "broken"].map(|name| dependency(name, "1.0"));
        for (dependency, day) in deps.iter().zip(["01", "02", "03", "04"]) {
            let last_used_at = format!("2026-01-{day}T00:00:00.000000Z");
            write_entry(&home.0, dependency, &last_used_at, b"0123456789");
        }
        let broken = deps_dir(&home.0).join(deps[3].key()).join(INFO_FILE);
        fs::write(broken, "{").unwrap();
        (home, deps)
    }

    fn keys(deps: &[&Dependency]) -> Vec<String> {
        let mut keys: Vec<String> = deps.iter().map(|dependency| dependency.key()).collect();
        keys.sort();
        keys
    }

    #[tokio::test]
    async fn evicts_the_least_recently_used_entries_by_count() {
        let (home, [_, b, c, _]) = full_cache();
        let d = dependency("d", "1.0");
        let limits = Limits {
            max_entries: 3,
            ..Limits::default()
        };
        store(&home.0, &d, b"0123456789", limits).await.unwrap();
        // `broken` counts as the oldest, then `a`.
        assert_eq!(listing(&home.0), keys(&[&b, &c, &d]));
    }

    #[test]
    fn evicts_the_least_recently_used_entries_by_size() {
        let (home, [_, b, c, _]) = full_cache();
        // `a`, `b` and `c` have the same size.
        let entry_size = dir_size(&deps_dir(&home.0).join(c.key()));
        let limits = Limits {
            max_bytes: 2 * entry_size,
            ..Limits::default()
        };
        evict(&deps_dir(&home.0), limits);
        assert_eq!(listing(&home.0), keys(&[&b, &c]));
        // Within the limits, nothing goes.
        evict(&deps_dir(&home.0), limits);
        assert_eq!(listing(&home.0), keys(&[&b, &c]));
    }
}
