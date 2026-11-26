//! Tests for the dependency cache, `fico_sdk::cache`: `info` and `clear`,
//! over entries written by hand in a temporary FICO home.

mod mock_server;
mod support;

use std::fs;
use std::path::{Path, PathBuf};

use fico_sdk::cache::{self, CacheCleared, CacheInfo};
use serde_json::{Value, json};
use support::TempDir;

/// The cache's directory in `home`.
fn deps(home: &Path) -> PathBuf {
    home.join("cache").join("deps")
}

/// Writes the entry `key` into the cache in `home`: `tarball` as its files,
/// and `info` as its `cache_info.json`.
fn write_entry(home: &Path, key: &str, info: &str, tarball: &[u8]) -> PathBuf {
    let entry = deps(home).join(key);
    fs::create_dir_all(&entry).unwrap();
    fs::write(entry.join("installed.tar.gz"), tarball).unwrap();
    fs::write(entry.join("cache_info.json"), info).unwrap();
    entry
}

/// The `cache_info.json` of the package `name` at `version`, last used at
/// `last_used_at`.
fn info_json(name: &str, version: &str, last_used_at: &str) -> String {
    json!({
        "created_at": "2026-01-01T00:00:00.000000Z",
        "last_used_at": last_used_at,
        "name": name,
        "version": version,
        "python_version": "3.11",
        "platform": "aarch64-manylinux_2_34",
    })
    .to_string()
}

/// A cache with three readable entries, one of them with an extra file,
/// and one whose `cache_info.json` is not JSON.
fn cache() -> TempDir {
    let home = TempDir::new("cache");
    let newest = "2026-10-02T00:00:00.000000Z";
    write_entry(
        home.path(),
        "k1",
        &info_json("numpy", "2.1.0", "2026-10-01T00:00:00.000000Z"),
        &[0; 100],
    );
    // A tie on `last_used_at`, broken by name, then version.
    write_entry(
        home.path(),
        "k2",
        &info_json("pandas", "2.2.0", newest),
        &[0; 20],
    );
    let entry = write_entry(
        home.path(),
        "k3",
        &info_json("pandas", "2.2.1", newest),
        &[0; 30],
    );
    fs::write(entry.join("extra"), [0; 5]).unwrap();
    write_entry(home.path(), "k4", "{", &[0; 7]);
    home
}

/// The size of the `cache_info.json` of each readable entry in [`cache`].
fn info_sizes(home: &Path) -> u64 {
    ["k1", "k2", "k3", "k4"]
        .iter()
        .map(|key| {
            let file = deps(home).join(key).join("cache_info.json");
            fs::metadata(file).unwrap().len()
        })
        .sum()
}

#[tokio::test]
async fn reports_a_missing_cache_as_empty() {
    let home = TempDir::new("cache");
    let info = cache::info(home.path()).await.unwrap();
    assert_eq!(info.path, deps(home.path()));
    assert_eq!(info.num_dependencies, 0);
    assert_eq!(info.total_size_bytes, 0);
    assert!(info.dependencies.is_empty());
    assert!(!home.path().join("cache").exists());
}

#[tokio::test]
async fn lists_the_readable_entries_newest_first() {
    let home = cache();
    let info = cache::info(home.path()).await.unwrap();

    // Every entry counts, also the unreadable one, but only the readable
    // ones are listed.
    assert_eq!(info.num_dependencies, 4);
    assert_eq!(
        info.total_size_bytes,
        100 + 20 + 30 + 5 + 7 + info_sizes(home.path())
    );
    let rows: Vec<(&str, &str, &str, u64)> = info
        .dependencies
        .iter()
        .map(|d| {
            (
                d.key.as_str(),
                d.name.as_str(),
                d.version.as_str(),
                d.size_bytes,
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            ("k3", "pandas", "2.2.1", 30),
            ("k2", "pandas", "2.2.0", 20),
            ("k1", "numpy", "2.1.0", 100),
        ]
    );
    let first = &info.dependencies[0];
    assert_eq!(first.python_version, "3.11");
    assert_eq!(first.platform, "aarch64-manylinux_2_34");
    assert_eq!(first.last_used_at, "2026-10-02T00:00:00.000000Z");
}

#[tokio::test]
async fn ignores_staging_directories_and_files() {
    let home = TempDir::new("cache");
    write_entry(
        home.path(),
        "k1",
        &info_json("numpy", "2.1.0", "2026-10-01T00:00:00.000000Z"),
        &[0; 10],
    );
    write_entry(
        home.path(),
        "k2-tmp-0123",
        &info_json("pandas", "2.2.0", "2026-10-01T00:00:00.000000Z"),
        &[0; 10],
    );
    fs::write(deps(home.path()).join("stray"), [0; 10]).unwrap();
    let info = cache::info(home.path()).await.unwrap();
    assert_eq!(info.num_dependencies, 1);
    assert_eq!(info.dependencies.len(), 1);
    assert_eq!(info.dependencies[0].key, "k1");
}

#[tokio::test]
async fn reads_missing_keys_and_files_as_empty() {
    let home = TempDir::new("cache");
    let entry = write_entry(home.path(), "k1", "{}", &[0; 10]);
    fs::remove_file(entry.join("installed.tar.gz")).unwrap();
    let info = cache::info(home.path()).await.unwrap();
    let row = &info.dependencies[0];
    assert_eq!(row.key, "k1");
    assert_eq!((row.name.as_str(), row.version.as_str()), ("", ""));
    assert_eq!(row.size_bytes, 0);
}

#[tokio::test]
async fn clears_the_cache() {
    let home = cache();
    let before = cache::info(home.path()).await.unwrap();
    let cleared = cache::clear(home.path()).await.unwrap();
    assert_eq!(cleared.deleted_dependencies, 4);
    assert_eq!(cleared.deleted_bytes, before.total_size_bytes);

    // The cache is there, and empty.
    let listing = fs::read_dir(deps(home.path())).unwrap().count();
    assert_eq!(listing, 0);
    let after = cache::info(home.path()).await.unwrap();
    assert_eq!(after.num_dependencies, 0);

    // Clearing an empty cache deletes nothing.
    let cleared = cache::clear(home.path()).await.unwrap();
    assert_eq!(cleared.deleted_dependencies, 0);
    assert_eq!(cleared.deleted_bytes, 0);
}

#[tokio::test]
async fn clears_a_missing_cache_without_creating_it() {
    let home = TempDir::new("cache");
    let cleared = cache::clear(home.path()).await.unwrap();
    assert_eq!(cleared.deleted_dependencies, 0);
    assert_eq!(cleared.deleted_bytes, 0);
    assert!(!home.path().join("cache").exists());
}

#[tokio::test]
async fn serializes_with_the_field_names_of_the_cli() {
    let home = cache();
    let info = cache::info(home.path()).await.unwrap();
    let value = serde_json::to_value(&info).unwrap();
    let keys = |value: &Value| {
        let mut keys: Vec<String> = value.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        keys
    };
    assert_eq!(
        keys(&value),
        [
            "dependencies",
            "num_dependencies",
            "path",
            "total_size_bytes"
        ]
    );
    assert_eq!(
        keys(&value["dependencies"][0]),
        [
            "key",
            "last_used_at",
            "name",
            "platform",
            "python_version",
            "size_bytes",
            "version"
        ]
    );
    assert_eq!(value["dependencies"][0]["size_bytes"], 30);
    let round_trip: CacheInfo = serde_json::from_value(value).unwrap();
    assert_eq!(round_trip, info);

    let cleared = cache::clear(home.path()).await.unwrap();
    let value = serde_json::to_value(cleared).unwrap();
    assert_eq!(
        value,
        json!({
            "deleted_dependencies": 4,
            "deleted_bytes": info.total_size_bytes,
        })
    );
    let round_trip: CacheCleared = serde_json::from_value(value).unwrap();
    assert_eq!(round_trip, cleared);
}
