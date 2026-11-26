//! Where the curated tools put content too large to return inline: files in
//! a cache directory in local mode, and resource links in remote mode.
//!
//! The cache holds platform runs in
//! `<FICO home>/decision/cache/runs/<run_id>/`, with the file names of a
//! local run (`local/run_dir.rs`): `outputs/solutions/solution.json`,
//! `logs/logs.log`, `inputs/input.json`, and the files of multi-file
//! content in `outputs/solutions/` and `inputs/`. Only content that no
//! longer changes is cached, a run's input, and its output and logs once it
//! has ended, so a file that is there is never downloaded again. The server
//! picks the directory, never the caller, so the tools that write it stay
//! read-only, and remote mode, which has no FICO home, never writes it.
//!
//! A resource link names a part of a platform run as
//! `fico://decision/workspaces/{workspace_id}/runs/{run_id}/{part}`, which
//! `resources/read` serves with the caller's context.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::decision::local::run_dir;
use crate::rand::random_fraction;
use crate::{Error, Result};

/// Most bytes of content that a result holds inline: about 40,000
/// characters of JSON or logs, about 10,000 tokens.
pub(crate) const MAX_INLINE_BYTES: u64 = 40_000;

/// Scheme and authority of the resource links.
const URI_PREFIX: &str = "fico://decision/workspaces/";

/// The parts of a platform run that a resource link can name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Part {
    Output,
    Logs,
    Input,
}

impl Part {
    /// Every part, in the order the templates list them.
    pub(crate) const ALL: [Part; 3] = [Part::Output, Part::Logs, Part::Input];

    /// The part's name in a URI and in a result: `output`, `logs` or
    /// `input`.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Part::Output => "output",
            Part::Logs => "logs",
            Part::Input => "input",
        }
    }

    /// The MIME type of the part's content.
    pub(crate) fn mime_type(self) -> &'static str {
        match self {
            Part::Logs => "text/plain",
            Part::Output | Part::Input => "application/json",
        }
    }

    /// What the part holds, for a template's description.
    pub(crate) fn description(self) -> &'static str {
        match self {
            Part::Output => "The output of a platform run, as JSON.",
            Part::Logs => "The logs of a platform run: what it wrote to stderr.",
            Part::Input => "The input of a platform run, as JSON.",
        }
    }

    fn parse(part: &str) -> Option<Part> {
        Part::ALL.into_iter().find(|known| known.as_str() == part)
    }
}

/// The URI template of the resource links to `part`.
pub(crate) fn uri_template(part: Part) -> String {
    format!(
        "{URI_PREFIX}{{workspace_id}}/runs/{{run_id}}/{}",
        part.as_str()
    )
}

/// The resource link to `part` of the platform run `run_id`. IDs are
/// percent-encoded, as in request paths.
pub(crate) fn uri(workspace_id: &str, run_id: &str, part: Part) -> String {
    format!(
        "{URI_PREFIX}{}/runs/{}/{}",
        encode(workspace_id),
        encode(run_id),
        part.as_str()
    )
}

/// The workspace ID, the run ID and the part that `uri`, a resource link,
/// names.
///
/// # Errors
///
/// Returns [`Error::Invalid`] if `uri` is not a resource link of this
/// server.
pub(crate) fn parse_uri(uri: &str) -> Result<(String, String, Part)> {
    let invalid = || Error::Invalid {
        reason: format!(
            "`{uri}` is not a resource of this server: resources look like `{}`",
            uri_template(Part::Output)
        ),
    };
    let rest = uri.strip_prefix(URI_PREFIX).ok_or_else(invalid)?;
    let segments: Vec<&str> = rest.split('/').collect();
    let [workspace_id, "runs", run_id, part] = segments.as_slice() else {
        return Err(invalid());
    };
    let part = Part::parse(part).ok_or_else(invalid)?;
    let workspace_id = decode(workspace_id).ok_or_else(invalid)?;
    let run_id = decode(run_id).ok_or_else(invalid)?;
    if workspace_id.is_empty() || run_id.is_empty() {
        return Err(invalid());
    }
    Ok((workspace_id, run_id, part))
}

/// `segment` with every byte but the RFC 3986 unreserved characters
/// percent-encoded, as `api_path` encodes path segments.
fn encode(segment: &str) -> String {
    let mut encoded = String::new();
    for byte in segment.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

/// `segment` with its percent-encoding undone, or `None` if it is not
/// valid percent-encoded UTF-8.
fn decode(segment: &str) -> Option<String> {
    let bytes = segment.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            b'%' => {
                let hex = segment.get(at + 1..at + 3)?;
                decoded.push(u8::from_str_radix(hex, 16).ok()?);
                at += 3;
            }
            byte => {
                decoded.push(byte);
                at += 1;
            }
        }
    }
    String::from_utf8(decoded).ok()
}

/// The cache directory of the platform run `run_id`, in the FICO home
/// `home`.
///
/// # Errors
///
/// Returns [`Error::Invalid`] unless `run_id` can name a directory, as a
/// local run ID must.
pub(crate) fn run_dir(home: &Path, run_id: &str) -> Result<PathBuf> {
    let root = home.join("decision").join("cache");
    // A run's directory is `<root>/.fico/runs/<run_id>` for a local run, so
    // the same check keeps a platform run inside `<root>/runs`.
    let dir = run_dir::run_dir(&root, run_id)?;
    Ok(root.join("runs").join(dir.file_name().unwrap_or_default()))
}

/// The file in a run directory, local or cached, that holds `part`, for
/// JSON content.
pub(crate) fn file(run_dir: &Path, part: Part) -> PathBuf {
    match part {
        Part::Output => run_dir::output_json_file(run_dir),
        Part::Logs => run_dir::logs_file(run_dir),
        Part::Input => run_dir::input_json_file(run_dir),
    }
}

/// The directory in a run directory, local or cached, that holds the files
/// of multi-file `part`.
pub(crate) fn dir(run_dir: &Path, part: Part) -> PathBuf {
    match part {
        Part::Input => run_dir::inputs_dir(run_dir),
        Part::Output | Part::Logs => run_dir::solutions_dir(run_dir),
    }
}

/// A new path next to `path` to write it through, unique to the call, as
/// the repo's other atomic writes name theirs: tool calls run at the same
/// time, so a name per process would let two of them collide.
pub(crate) fn temp_path(path: &Path) -> PathBuf {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let suffix = (random_fraction() * (1u64 << 53) as f64) as u64;
    path.with_file_name(format!(".{name}.{suffix:x}.tmp"))
}

/// Writes `contents` to `path` through a new file next to it and a rename,
/// so that a reader never sees a partly written file. If another call put
/// the file there first, that one stays.
///
/// # Errors
///
/// Returns [`Error::Io`] if the file cannot be written.
pub(crate) async fn write(path: &Path, contents: &[u8]) -> Result<()> {
    let io = |path: &Path| {
        let path = path.to_path_buf();
        move |source| Error::Io { path, source }
    };
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(io(parent))?;
    }
    let temp = temp_path(path);
    tokio::fs::write(&temp, contents).await.map_err(io(&temp))?;
    if let Err(source) = tokio::fs::rename(&temp, path).await {
        let _ = tokio::fs::remove_file(&temp).await;
        if !exists(path).await {
            return Err(Error::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    }
    Ok(())
}

/// Whether `path` is a file, or a directory with something in it.
pub(crate) async fn exists(path: &Path) -> bool {
    match tokio::fs::metadata(path).await {
        Ok(metadata) if metadata.is_file() => true,
        Ok(metadata) if metadata.is_dir() => match tokio::fs::read_dir(path).await {
            Ok(mut entries) => matches!(entries.next_entry().await, Ok(Some(_))),
            Err(_) => false,
        },
        _ => false,
    }
}

/// `value` as the text a result would hold: compact JSON, or the string
/// itself.
pub(crate) fn text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        value => value.to_string(),
    }
}

/// Whether content of `bytes` bytes is too large to return inline.
pub(crate) fn too_large(bytes: u64) -> bool {
    bytes > MAX_INLINE_BYTES
}

/// Where a part of a run ended up in a result.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Placed {
    /// In the result itself.
    Inline(Value),
    /// In a file of this many bytes, too large to return inline.
    File(PathBuf, u64),
    /// In the files of a directory, for multi-file content.
    Dir(PathBuf),
    /// Behind a resource link to this many bytes, too large to return
    /// inline.
    Link(String, u64),
    /// Nowhere, for this reason.
    Missing(String),
}

impl Placed {
    /// Adds the part `name` to `result`: as `name` itself when inline, else
    /// as `name_file` and `name_bytes`, `name_dir`, `name_resource_link` and
    /// `name_bytes`, or `name_missing` with the reason.
    pub(crate) fn insert(self, result: &mut Map<String, Value>, name: &str) {
        let mut put = |suffix: &str, value: Value| {
            result.insert(format!("{name}{suffix}"), value);
        };
        match self {
            Placed::Inline(value) => put("", value),
            Placed::File(path, bytes) => {
                put("_file", json!(path));
                put("_bytes", json!(bytes));
            }
            Placed::Dir(path) => put("_dir", json!(path)),
            Placed::Link(uri, bytes) => {
                put("_resource_link", json!(uri));
                put("_bytes", json!(bytes));
            }
            Placed::Missing(reason) => put("_missing", json!(reason)),
        }
    }
}

/// The resource links that `result`, a tool result, holds at any depth:
/// the `*_resource_link` fields that [`Placed::insert`] writes, each with
/// a name, the run and the part it links to, and the part.
pub(crate) fn links(result: &Value) -> Vec<(String, String, Part)> {
    fn walk(value: &Value, found: &mut Vec<(String, String, Part)>) {
        match value {
            Value::Object(fields) => {
                for (key, value) in fields {
                    if let (true, Value::String(uri)) = (key.ends_with("_resource_link"), value)
                        && let Ok((_, run_id, part)) = parse_uri(uri)
                    {
                        let name = format!("{run_id} {}", part.as_str());
                        found.push((uri.clone(), name, part));
                    }
                    walk(value, found);
                }
            }
            Value::Array(items) => items.iter().for_each(|value| walk(value, found)),
            _ => {}
        }
    }
    let mut found = Vec::new();
    walk(result, &mut found);
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_resource_links() {
        let uri = uri("routing", "run-1", Part::Logs);
        assert_eq!(uri, "fico://decision/workspaces/routing/runs/run-1/logs");
        assert_eq!(
            parse_uri(&uri).unwrap(),
            ("routing".to_owned(), "run-1".to_owned(), Part::Logs)
        );
        let odd = super::uri("a/b c", "é", Part::Input);
        assert_eq!(
            odd,
            "fico://decision/workspaces/a%2Fb%20c/runs/%C3%A9/input"
        );
        assert_eq!(
            parse_uri(&odd).unwrap(),
            ("a/b c".to_owned(), "é".to_owned(), Part::Input)
        );
    }

    #[test]
    fn rejects_other_uris() {
        for uri in [
            "https://example.com",
            "fico://decision/workspaces/routing/runs/run-1",
            "fico://decision/workspaces/routing/runs/run-1/visuals",
            "fico://decision/workspaces/routing/builds/b/output",
            "fico://decision/workspaces//runs/run-1/output",
            "fico://decision/workspaces/routing/runs/%zz/output",
            "fico://decision/workspaces/routing/runs/run-1/output/extra",
        ] {
            let error = parse_uri(uri).unwrap_err();
            assert!(matches!(error, Error::Invalid { .. }), "{uri}");
        }
    }

    #[tokio::test]
    async fn writes_files_at_the_same_time_without_colliding() {
        let dir = std::env::temp_dir().join(format!(
            "fico-sdk-cache-write-{}-{:x}",
            std::process::id(),
            (random_fraction() * (1u64 << 53) as f64) as u64
        ));
        let path = dir.join("logs").join("logs.log");
        assert_ne!(temp_path(&path), temp_path(&path));
        let writes = (0..8).map(|n| {
            let path = path.clone();
            tokio::spawn(async move { write(&path, format!("logs {n}").as_bytes()).await })
        });
        for write in writes {
            write.await.unwrap().unwrap();
        }
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.starts_with("logs "), "{written}");
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(leftovers, ["logs.log"]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn keeps_cached_runs_inside_the_cache() {
        let home = Path::new("/home/ana/.fico");
        assert_eq!(
            run_dir(home, "run-1").unwrap(),
            Path::new("/home/ana/.fico/decision/cache/runs/run-1")
        );
        for run_id in ["", ".", "..", "../x", "a/b", "a\\b"] {
            assert!(run_dir(home, run_id).is_err(), "{run_id:?}");
        }
    }

    #[test]
    fn finds_the_links_of_a_result() {
        let mut run = Map::new();
        Placed::Link(uri("routing", "run-1", Part::Output), 50_000).insert(&mut run, "output");
        Placed::Inline(json!("ok")).insert(&mut run, "logs");
        let mut other = Map::new();
        Placed::Link(uri("routing", "run-2", Part::Logs), 41_000).insert(&mut other, "logs");
        Placed::Missing("not yet".to_owned()).insert(&mut other, "input");
        let result = json!({ "run": run, "runs": [other] });
        assert_eq!(result["run"]["output_bytes"], 50_000);
        assert_eq!(result["run"]["logs"], "ok");
        assert_eq!(result["runs"][0]["input_missing"], "not yet");
        assert_eq!(
            links(&result),
            [
                (
                    "fico://decision/workspaces/routing/runs/run-1/output".to_owned(),
                    "run-1 output".to_owned(),
                    Part::Output,
                ),
                (
                    "fico://decision/workspaces/routing/runs/run-2/logs".to_owned(),
                    "run-2 logs".to_owned(),
                    Part::Logs,
                ),
            ]
        );
    }

    #[test]
    fn places_files_and_directories() {
        let mut result = Map::new();
        Placed::File(PathBuf::from("/c/runs/r/logs/logs.log"), 50_001).insert(&mut result, "logs");
        Placed::Dir(PathBuf::from("/c/runs/r/inputs")).insert(&mut result, "input");
        assert_eq!(
            Value::Object(result),
            json!({
                "logs_file": "/c/runs/r/logs/logs.log",
                "logs_bytes": 50_001,
                "input_dir": "/c/runs/r/inputs",
            })
        );
        assert!(!too_large(MAX_INLINE_BYTES));
        assert!(too_large(MAX_INLINE_BYTES + 1));
        assert_eq!(text(&json!("logs")), "logs");
        assert_eq!(text(&json!({ "a": 1 })), r#"{"a":1}"#);
    }
}
