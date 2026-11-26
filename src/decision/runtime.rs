//! Helpers for a workspace's own code, which runs inside a run.
//!
//! [`load`] reads the run's input and options, [`write()`] writes its
//! output, metrics and assets, and [`log`] writes a line to the run's
//! logs. They read and write where the local executor
//! ([`Local::with_executor`](crate::decision::local::Local::with_executor))
//! and the platform's runtime collect them, so the same workspace code runs
//! in both:
//!
//! | | `json` content format | `multi-file` content format |
//! | --- | --- | --- |
//! | Input | JSON on stdin | the files of the manifest's input directory, `inputs/` by default |
//! | Output | one JSON object on stdout | files under the manifest's solutions directory, `outputs/solutions/` by default |
//! | Metrics, assets | the `metrics` and `assets` keys of the output | the manifest's metrics and assets files, `outputs/metrics.json` and `outputs/assets.json` by default |
//! | Logs | stderr | stderr |
//! | Options | command-line arguments | command-line arguments |
//!
//! The functions take and return data (JSON values, paths and plain
//! structs), so that another SDK can bind them without wrapping any Rust
//! type. Nothing but [`write()`], and only for the `json` content format, ever
//! writes to stdout, which is the output of a `json` workspace.
//!
//! The manifest, `workspace.yaml`, decides the content format, the paths
//! and the options. It is the one in [`RuntimeLoadParams::workspace_dir`],
//! or else the first one found from the current directory upwards, which is
//! where the executor and the platform put it. Without a manifest, a
//! workspace has the `json` content format and no options.
//!
//! # Examples
//!
//! ```no_run
//! # async fn run() -> fico_sdk::Result<()> {
//! use fico_sdk::decision::runtime::{self, RuntimeLoadParams, RuntimeWriteParams};
//! use serde_json::json;
//!
//! let run = runtime::load(RuntimeLoadParams::default()).await?;
//! runtime::log("Solving...");
//! let duration = run.options["duration"].as_i64().unwrap_or(30);
//! let solution = json!({ "duration": duration, "input": run.input });
//! runtime::write(RuntimeWriteParams {
//!     output: Some(solution),
//!     metrics: Some(json!({ "cost": 42.0 }).as_object().cloned().unwrap_or_default()),
//!     ..RuntimeWriteParams::default()
//! })
//! .await?;
//! # Ok(())
//! # }
//! ```

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value, json};

use crate::decision::fs::{files_under, inside, write_atomically};
use crate::decision::manifest::{self, MANIFEST_FILE};
use crate::decision::{ContentFormat, Manifest, ManifestOption, ManifestOptionType};
use crate::{Error, Result};

/// The placeholder of an option's value in the manifest's options format.
const VALUE: &str = "{{value}}";

/// How [`load`] passes an option when the manifest sets no options format:
/// `-name value`, which the local executor passes, `-name=value`, which the
/// platform passes, and the same with two dashes.
const DEFAULT_FORMATS: [&[&str]; 4] = [
    &["-{{name}}", VALUE],
    &["-{{name}}={{value}}"],
    &["--{{name}}", VALUE],
    &["--{{name}}={{value}}"],
];

/// Parameters for [`load`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RuntimeLoadParams {
    /// Directory that holds the `workspace.yaml` to read, and that the
    /// manifest's paths are relative to. Defaults to the first directory
    /// from the current directory upwards that holds one, or else to the
    /// current directory, with no manifest.
    pub workspace_dir: Option<PathBuf>,
    /// The command-line arguments that carry the options, without the
    /// program. Defaults to this process's arguments.
    pub args: Option<Vec<String>>,
    /// Where to read the input from instead of the default: for the `json`
    /// content format, a file instead of stdin, and for `multi-file`, a
    /// directory instead of the manifest's input path.
    pub input_path: Option<PathBuf>,
}

/// A run's input and options, as [`load`] reads them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RuntimeRun {
    /// The workspace's directory, absolute: the one that holds the manifest,
    /// or the current directory when there is none.
    pub workspace_dir: PathBuf,
    /// The manifest's content format, `json` without a manifest.
    pub content_format: ContentFormat,
    /// The input of a `json` workspace: what stdin held, or the file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
    /// The directory of a `multi-file` workspace's input, absolute.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_dir: Option<PathBuf>,
    /// The files under `input_dir`, relative to it and sorted. They are not
    /// read: the workspace opens the ones it needs.
    pub input_files: Vec<PathBuf>,
    /// The directory where a `multi-file` workspace writes its solution
    /// files, absolute. [`load`] creates it, so the workspace can write
    /// files of any format there. [`write()`] puts JSON files in it too.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub solutions_dir: Option<PathBuf>,
    /// Every option that the manifest declares, by name, typed by the
    /// manifest: a string, a boolean, an integer or a float. An option with
    /// no value and no default is `null`.
    pub options: BTreeMap<String, Value>,
}

/// Reads the run's input and options.
///
/// **Options.** Only the options that the manifest declares are read. Each
/// one takes the first of these that is set:
///
/// 1. The command-line argument. Without an options format
///    (`configuration.options.format`), `-name value`, `-name=value`,
///    `--name value` and `--name=value` all work. With one, the arguments
///    are matched against its entries, with `{{name}}` and `{{value}}`
///    standing for the option's name and value, and an entry that is only
///    `{{value}}` may be missing at the end, as the executor and the
///    platform leave out an empty argument. The last occurrence wins, and
///    arguments that match no declared option are ignored.
/// 2. The environment variable named like the option, in upper case with
///    `-` as `_`: `DURATION` for `duration`. Beware of names that the
///    environment already uses, such as `path` or `home`.
/// 3. The option's default.
///
/// A value is typed by the option's type: an `int` is a 64-bit integer, a
/// `float` a finite number, and a `bool` is true for `true`, `1`, `t`, `y`
/// and `yes` in any case, and false for anything else. A default that is
/// text is read the same way.
///
/// **Input.** A `json` workspace's input is all of stdin, or the file of
/// `params.input_path`, which must hold JSON. For `multi-file`, the files
/// under the input directory are listed, not read, and the solutions
/// directory is created.
///
/// # Errors
///
/// Returns [`Error::Io`] if the manifest, the input or a directory cannot
/// be read or created, and [`Error::Manifest`] if the manifest is invalid.
/// Returns [`Error::Invalid`] if the input is not JSON, a manifest path is
/// absolute or has a `..`, the manifest's options format has no `{{name}}`,
/// an option's value or default does not fit its type, a required option
/// has no value, or the content format is unknown.
pub async fn load(params: RuntimeLoadParams) -> Result<RuntimeRun> {
    let (workspace_dir, manifest) = locate(params.workspace_dir).await?;
    let args = params.args.unwrap_or_else(|| {
        std::env::args_os()
            .skip(1)
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    });
    let options = resolve_options(&manifest, &args, &|name| std::env::var(name).ok())?;

    let content = &manifest.configuration.content;
    let mut run = RuntimeRun {
        workspace_dir: workspace_dir.clone(),
        content_format: content.format,
        input: None,
        input_dir: None,
        input_files: Vec::new(),
        solutions_dir: None,
        options,
    };
    match content.format {
        ContentFormat::Json => {
            let bytes = match params.input_path {
                Some(path) => tokio::fs::read(&path)
                    .await
                    .map_err(|source| Error::Io { path, source })?,
                None => read_stdin().await?,
            };
            run.input = Some(serde_json::from_slice(&bytes).map_err(|e| Error::Invalid {
                reason: format!("the run's input is not JSON: {e}"),
            })?);
        }
        ContentFormat::MultiFile => {
            let multi_file = content.multi_file.clone().unwrap_or_default();
            let input_dir = match params.input_path {
                Some(path) => absolute(&path)?,
                None => inside(&workspace_dir, &multi_file.input.path)?,
            };
            let solutions_dir = inside(&workspace_dir, &multi_file.output.solutions)?;
            let metadata = tokio::fs::metadata(&input_dir)
                .await
                .map_err(|source| Error::Io {
                    path: input_dir.clone(),
                    source,
                })?;
            if !metadata.is_dir() {
                return Err(Error::Invalid {
                    reason: format!(
                        "the input path `{}` is not a directory",
                        input_dir.display()
                    ),
                });
            }
            tokio::fs::create_dir_all(&solutions_dir)
                .await
                .map_err(|source| Error::Io {
                    path: solutions_dir.clone(),
                    source,
                })?;
            let dir = input_dir.clone();
            run.input_files = blocking(input_dir.clone(), move || files_under(&dir)).await?;
            run.input_dir = Some(input_dir);
            run.solutions_dir = Some(solutions_dir);
        }
        _ => {
            return Err(Error::Invalid {
                reason: "a manifest's content format must be `json` or `multi-file`".to_owned(),
            });
        }
    }
    Ok(run)
}

/// The workspace's directory, absolute, and its manifest, which is the
/// default one if the directory search finds none.
///
/// # Errors
///
/// Returns [`Error::Io`] if the current directory or the manifest cannot be
/// read, and [`Error::Manifest`] if the manifest is invalid.
async fn locate(dir: Option<PathBuf>) -> Result<(PathBuf, Manifest)> {
    if let Some(dir) = dir {
        let dir = absolute(&dir)?;
        let manifest = manifest::read(dir.join(MANIFEST_FILE)).await?;
        return Ok((dir, manifest));
    }
    let cwd = std::env::current_dir().map_err(|source| Error::Io {
        path: PathBuf::from("."),
        source,
    })?;
    for dir in cwd.ancestors() {
        let file = dir.join(MANIFEST_FILE);
        if tokio::fs::metadata(&file).await.is_ok_and(|m| m.is_file()) {
            let manifest = manifest::read(file).await?;
            return Ok((dir.to_path_buf(), manifest));
        }
    }
    Ok((cwd, Manifest::default()))
}

/// `path` as an absolute path, taken from the current directory.
fn absolute(path: &Path) -> Result<PathBuf> {
    std::path::absolute(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// All of stdin.
async fn read_stdin() -> Result<Vec<u8>> {
    blocking(PathBuf::from("<stdin>"), || {
        let mut bytes = Vec::new();
        std::io::stdin()
            .lock()
            .read_to_end(&mut bytes)
            .map_err(|source| Error::Io {
                path: PathBuf::from("<stdin>"),
                source,
            })?;
        Ok(bytes)
    })
    .await
}

/// Runs the blocking `f` on a thread of its own. `path` is what an
/// unexpected failure of the thread is reported for.
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
            source: std::io::Error::other(error),
        }),
    }
}

/// The value of every option that `manifest` declares, from `args`, then
/// `env`, then the option's default.
fn resolve_options(
    manifest: &Manifest,
    args: &[String],
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<BTreeMap<String, Value>> {
    let Some(options) = manifest.configuration.options.as_ref() else {
        return Ok(BTreeMap::new());
    };
    let items = options.items.as_deref().unwrap_or_default();
    let format = options
        .format
        .as_deref()
        .filter(|format| !format.is_empty());
    let given = scan_args(items, format, args)?;
    let mut resolved = BTreeMap::new();
    for option in items {
        let name = option.name.as_str();
        let raw = given
            .get(name)
            .cloned()
            .or_else(|| env(&name.to_uppercase().replace('-', "_")));
        let value = match raw {
            Some(raw) => typed(option, &raw)?,
            None => default_of(option)?,
        };
        if value.is_null() && option.required {
            return Err(Error::Invalid {
                reason: format!(
                    "option `{name}` is required, but it has no command-line argument, \
                     environment variable or default"
                ),
            });
        }
        resolved.insert(option.name.clone(), value);
    }
    Ok(resolved)
}

/// The text that `args` give each option of `items`, by name.
fn scan_args<'a>(
    items: &'a [ManifestOption],
    format: Option<&[String]>,
    args: &[String],
) -> Result<BTreeMap<&'a str, String>> {
    let templates: Vec<Template> = match format {
        Some(format) => {
            if !format.iter().any(|entry| entry.contains("{{name}}")) {
                return Err(Error::Invalid {
                    reason: "the manifest's options format has no `{{name}}`, so the arguments \
                             can't tell the options apart"
                        .to_owned(),
                });
            }
            vec![Template {
                entries: format.to_vec(),
            }]
        }
        None => DEFAULT_FORMATS
            .iter()
            .map(|entries| Template {
                entries: entries.iter().map(|entry| (*entry).to_owned()).collect(),
            })
            .collect(),
    };
    // The longest name first, so that a name that is the start of another
    // never takes its arguments.
    let mut names: Vec<&str> = items.iter().map(|option| option.name.as_str()).collect();
    names.sort_by_key(|name| std::cmp::Reverse(name.len()));

    // Whether `arg` is the start of an option's arguments.
    let starts_option = |arg: &str| {
        names.iter().any(|name| {
            templates
                .iter()
                .any(|template| template.starts_with(name, arg))
        })
    };
    let mut given = BTreeMap::new();
    let mut at = 0;
    'args: while at < args.len() {
        for name in &names {
            for template in &templates {
                if let Some((used, value)) = template.matches(name, &args[at..], &starts_option) {
                    given.insert(*name, value);
                    at += used.max(1);
                    continue 'args;
                }
            }
        }
        at += 1;
    }
    Ok(given)
}

/// How an option appears among the arguments: the entries of an options
/// format, which each become one argument.
struct Template {
    /// The entries, with `{{name}}` and `{{value}}` in them.
    entries: Vec<String>,
}

impl Template {
    /// Whether `arg` is the first argument of the option `name`.
    fn starts_with(&self, name: &str, arg: &str) -> bool {
        let first = self.entries[0].replace("{{name}}", name);
        match first.find(VALUE) {
            Some(at) => first != VALUE && arg.starts_with(&first[..at]),
            None => arg == first,
        }
    }

    /// How many of `args` the option `name` takes at their start, and its
    /// value, if they are its arguments.
    ///
    /// A last entry that is only `{{value}}` is missing when the value is
    /// empty, as the executor and the platform leave out an entry that ends
    /// up empty: it takes no argument if there is none left, or the next
    /// one `starts_option`.
    fn matches(
        &self,
        name: &str,
        args: &[String],
        starts_option: &dyn Fn(&str) -> bool,
    ) -> Option<(usize, String)> {
        let mut value: Option<String> = None;
        let mut used = 0;
        for (index, entry) in self.entries.iter().enumerate() {
            let pattern = entry.replace("{{name}}", name);
            let last = index + 1 == self.entries.len();
            let missing =
                last && pattern == VALUE && args.get(used).is_none_or(|arg| starts_option(arg));
            if missing {
                value.get_or_insert_with(String::new);
                break;
            }
            let arg = args.get(used)?;
            match (pattern.find(VALUE), pattern.rfind(VALUE)) {
                (Some(first), Some(last)) => {
                    let (prefix, suffix) = (&pattern[..first], &pattern[last + VALUE.len()..]);
                    let captured = arg.strip_prefix(prefix)?.strip_suffix(suffix)?;
                    value.get_or_insert_with(|| captured.to_owned());
                }
                _ if *arg == pattern => {}
                _ => return None,
            }
            used += 1;
        }
        Some((used, value.unwrap_or_default()))
    }
}

/// The text `raw` as a value of the type of `option`.
fn typed(option: &ManifestOption, raw: &str) -> Result<Value> {
    let name = &option.name;
    let bad = |expected: &str| Error::Invalid {
        reason: format!("option `{name}` is {raw:?}, which is not {expected}"),
    };
    match option.option_type {
        ManifestOptionType::String => Ok(Value::String(raw.to_owned())),
        ManifestOptionType::Bool => Ok(Value::Bool(matches!(
            raw.to_lowercase().as_str(),
            "true" | "1" | "t" | "y" | "yes"
        ))),
        ManifestOptionType::Int => {
            let text = raw.trim();
            text.parse::<i64>()
                .map(Value::from)
                .or_else(|_| text.parse::<u64>().map(Value::from))
                .map_err(|_| bad("an integer"))
        }
        ManifestOptionType::Float => raw
            .trim()
            .parse::<f64>()
            .ok()
            .and_then(Number::from_f64)
            .map(Value::Number)
            .ok_or_else(|| bad("a finite number")),
        _ => Err(Error::Invalid {
            reason: format!("option `{name}` has a type that this SDK does not know"),
        }),
    }
}

/// The default of `option` as a value of its type, or `null` if it has
/// none.
fn default_of(option: &ManifestOption) -> Result<Value> {
    let value = match &option.default {
        None | Some(Value::Null) => return Ok(Value::Null),
        Some(Value::String(text)) => return typed(option, text),
        Some(value) => value,
    };
    let fitting = match (option.option_type, value) {
        (ManifestOptionType::Bool, Value::Bool(_)) => Some(value.clone()),
        (ManifestOptionType::Int, Value::Number(n)) if n.is_i64() || n.is_u64() => {
            Some(value.clone())
        }
        (ManifestOptionType::Float, Value::Number(n)) => {
            n.as_f64().and_then(Number::from_f64).map(Value::Number)
        }
        (ManifestOptionType::String, Value::Bool(_) | Value::Number(_)) => {
            Some(Value::String(value.to_string()))
        }
        _ => None,
    };
    fitting.ok_or_else(|| Error::Invalid {
        reason: format!(
            "the default of option `{}`, {value}, does not fit its type",
            option.name
        ),
    })
}

/// Parameters for [`write()`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RuntimeWriteParams {
    /// Directory that holds the `workspace.yaml` to read, as in
    /// [`RuntimeLoadParams::workspace_dir`].
    pub workspace_dir: Option<PathBuf>,
    /// The output of a `json` workspace, its `solution`. Defaults to `{}`.
    /// A `multi-file` workspace has none: it writes `output_files`.
    pub output: Option<Value>,
    /// The solution files of a `multi-file` workspace, by path relative to
    /// its solutions directory, each written as a JSON file. Files of any
    /// other format are written by the workspace itself into
    /// [`RuntimeRun::solutions_dir`]. A `json` workspace has none.
    pub output_files: BTreeMap<String, Value>,
    /// The run's metrics: a JSON object, which can be nested.
    pub metrics: Option<Map<String, Value>>,
    /// The run's assets, each a JSON object with a `name`, a `content_type`,
    /// `content` and, optionally, a `description` and a `visual`.
    pub assets: Option<Vec<Value>>,
    /// The options to record in the `options` key of a `json` output,
    /// usually [`RuntimeRun::options`]. Defaults to `{}`.
    pub options: Option<BTreeMap<String, Value>>,
    /// A directory to write everything to instead of where the run collects
    /// it from, in the layout of a finished local run's `outputs/`:
    /// `solutions/`, `metrics.json` and `assets.json`. For a `json`
    /// workspace, `solutions/solution.json` holds what would go to stdout.
    /// Use it to write the output without a run, or to point a host at a
    /// directory.
    pub output_dir: Option<PathBuf>,
}

/// Writes the run's output, metrics and assets where the run collects them
/// from.
///
/// A `json` workspace writes one JSON object to stdout, followed by a
/// newline: `{"options": …, "solution": …, "assets": […], "metrics": {…}}`,
/// with the output as `solution`. The executor and the platform take the
/// metrics and the assets out of it. Call it once, as the last thing the
/// workspace prints.
///
/// A `multi-file` workspace writes its solution files, `{"metrics": …}` to
/// the manifest's metrics path and `{"assets": […]}` to its assets path,
/// the last two only when they are set and not empty. Paths that the
/// manifest leaves empty take their defaults, `outputs/metrics.json` and
/// `outputs/assets.json`. An empty solutions path is the workspace directory,
/// as in [`RuntimeRun::solutions_dir`] and for the executor.
///
/// Files are written atomically, creating their directories. With
/// `params.output_dir`, they are written there instead (see
/// [`RuntimeWriteParams::output_dir`]), and nothing goes to stdout.
///
/// # Errors
///
/// Returns [`Error::Io`] if the manifest or a file cannot be read or
/// written, or stdout cannot be written, and [`Error::Manifest`] if the
/// manifest is invalid. Returns [`Error::Invalid`] if a `multi-file`
/// workspace has an `output` or a `json` workspace has `output_files`, a
/// manifest path or the name of an output file is absolute or has a `..`,
/// or the content format is unknown.
pub async fn write(params: RuntimeWriteParams) -> Result<()> {
    let (workspace_dir, manifest) = locate(params.workspace_dir).await?;
    let content = &manifest.configuration.content;
    let sink = match &params.output_dir {
        Some(dir) => Some(absolute(dir)?),
        None => None,
    };
    match content.format {
        ContentFormat::Json => {
            if !params.output_files.is_empty() {
                return Err(Error::Invalid {
                    reason: "a `json` workspace has no output files: set `output`".to_owned(),
                });
            }
            let envelope = json!({
                "options": params.options.unwrap_or_default(),
                "solution": params.output.clone().unwrap_or_else(|| json!({})),
                "assets": params.assets.clone().unwrap_or_default(),
                "metrics": params.metrics.clone().unwrap_or_default(),
            });
            let bytes = pretty(&envelope);
            let Some(dir) = sink else {
                return blocking(PathBuf::from("<stdout>"), move || {
                    let mut stdout = std::io::stdout().lock();
                    stdout
                        .write_all(&bytes)
                        .and_then(|()| stdout.flush())
                        .map_err(|source| Error::Io {
                            path: PathBuf::from("<stdout>"),
                            source,
                        })
                })
                .await;
            };
            write_atomically(&dir.join("solutions").join("solution.json"), &bytes).await?;
            if let Some(metrics) = params.metrics {
                let value = json!({ "metrics": metrics });
                write_atomically(&dir.join("metrics.json"), &pretty(&value)).await?;
            }
            if let Some(assets) = params.assets {
                let value = json!({ "assets": assets });
                write_atomically(&dir.join("assets.json"), &pretty(&value)).await?;
            }
            Ok(())
        }
        ContentFormat::MultiFile => {
            if params.output.is_some() {
                return Err(Error::Invalid {
                    reason: "a `multi-file` workspace has no output: set `output_files`".to_owned(),
                });
            }
            let paths = match &sink {
                Some(dir) => (
                    dir.join("solutions"),
                    dir.join("metrics.json"),
                    dir.join("assets.json"),
                ),
                None => {
                    let output = content.multi_file.clone().unwrap_or_default().output;
                    let or = |path: &str, default: &str| {
                        inside(&workspace_dir, if path.is_empty() { default } else { path })
                    };
                    (
                        inside(&workspace_dir, &output.solutions)?,
                        or(&output.metrics, "outputs/metrics.json")?,
                        or(&output.assets, "outputs/assets.json")?,
                    )
                }
            };
            let (solutions, metrics_file, assets_file) = paths;
            for (name, value) in &params.output_files {
                write_atomically(&inside(&solutions, name)?, &pretty(value)).await?;
            }
            if let Some(metrics) = params.metrics.filter(|metrics| !metrics.is_empty()) {
                write_atomically(&metrics_file, &pretty(&json!({ "metrics": metrics }))).await?;
            }
            if let Some(assets) = params.assets.filter(|assets| !assets.is_empty()) {
                write_atomically(&assets_file, &pretty(&json!({ "assets": assets }))).await?;
            }
            Ok(())
        }
        _ => Err(Error::Invalid {
            reason: "a manifest's content format must be `json` or `multi-file`".to_owned(),
        }),
    }
}

/// Writes `message` and a newline to stderr, which becomes a line of the
/// run's logs.
///
/// Nothing is written to stdout, which is a `json` workspace's output. A
/// failure to write is ignored: a log line never fails the run.
pub fn log(message: &str) {
    let _ = writeln!(std::io::stderr().lock(), "{message}");
}

/// `value` as indented JSON followed by a newline.
fn pretty(value: &Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec_pretty(value).unwrap_or_default();
    bytes.push(b'\n');
    bytes
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::decision::{ManifestOptions, id};

    /// A new directory under the system's temp directory, removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> TempDir {
            let path = std::env::temp_dir().join(id::safe_id("fico-sdk-runtime"));
            std::fs::create_dir_all(&path).unwrap();
            TempDir(path.canonicalize().unwrap())
        }

        fn write(&self, relative: &str, contents: &str) {
            let path = self.0.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn option(
        name: &str,
        option_type: ManifestOptionType,
        default: Option<Value>,
    ) -> ManifestOption {
        ManifestOption {
            name: name.to_owned(),
            option_type,
            default,
            ..ManifestOption::default()
        }
    }

    fn manifest(items: Vec<ManifestOption>, format: Option<&[&str]>) -> Manifest {
        let mut manifest = Manifest::default();
        manifest.configuration.options = Some(ManifestOptions {
            items: Some(items),
            format: format.map(|f| f.iter().map(|e| (*e).to_owned()).collect()),
            ..ManifestOptions::default()
        });
        manifest
    }

    fn args(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| (*arg).to_owned()).collect()
    }

    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn reads_every_form_of_a_flag() {
        use ManifestOptionType::*;
        let m = manifest(
            vec![
                option("a", String, None),
                option("b", Int, None),
                option("c", Float, None),
                option("d", Bool, None),
            ],
            None,
        );
        let given = args(&["-a", "x y", "--b=7", "-c=2.5", "--d", "Yes", "--other", "1"]);
        let got = resolve_options(&m, &given, &no_env).unwrap();
        assert_eq!(
            got,
            BTreeMap::from([
                ("a".to_owned(), json!("x y")),
                ("b".to_owned(), json!(7)),
                ("c".to_owned(), json!(2.5)),
                ("d".to_owned(), json!(true)),
            ])
        );
    }

    #[test]
    fn takes_the_last_occurrence_and_the_longest_name() {
        use ManifestOptionType::*;
        let m = manifest(vec![option("d", Int, None), option("dur", Int, None)], None);
        let got = resolve_options(&m, &args(&["-d", "1", "-dur", "2", "-d=3"]), &no_env).unwrap();
        assert_eq!(got["d"], json!(3));
        assert_eq!(got["dur"], json!(2));
    }

    #[test]
    fn reads_the_options_format() {
        use ManifestOptionType::*;
        let items = vec![option("duration", Int, None), option("mode", String, None)];
        let m = manifest(items.clone(), Some(&["--{{name}}={{value}}"]));
        let got = resolve_options(&m, &args(&["--duration=30", "-mode", "x"]), &no_env).unwrap();
        assert_eq!(got["duration"], json!(30));
        assert_eq!(got["mode"], Value::Null);

        let m = manifest(items.clone(), Some(&["--set", "{{name}}:{{value}}"]));
        let given = args(&["--set", "mode:fast", "--set", "duration:5"]);
        let got = resolve_options(&m, &given, &no_env).unwrap();
        assert_eq!(got["mode"], json!("fast"));
        assert_eq!(got["duration"], json!(5));

        // The executor leaves out an empty value, so the last entry may be
        // missing: first at the end of the arguments, then before another
        // option's.
        let m = manifest(items, Some(&["--{{name}}", "{{value}}"]));
        let got = resolve_options(&m, &args(&["--mode"]), &no_env).unwrap();
        assert_eq!(got["mode"], json!(""));
        let got = resolve_options(&m, &args(&["--mode", "--duration", "9"]), &no_env).unwrap();
        assert_eq!(got["duration"], json!(9));
    }

    #[test]
    fn rejects_a_format_without_a_name() {
        let m = manifest(
            vec![option("a", ManifestOptionType::String, None)],
            Some(&["{{value}}"]),
        );
        let err = resolve_options(&m, &[], &no_env).unwrap_err();
        assert!(matches!(err, Error::Invalid { .. }), "{err}");
    }

    #[test]
    fn prefers_arguments_then_the_environment_then_the_default() {
        use ManifestOptionType::*;
        let m = manifest(
            vec![
                option("max-time", Int, Some(json!(10))),
                option("name", String, Some(json!("dflt"))),
                option("on", Bool, Some(json!("true"))),
            ],
            None,
        );
        let env = |name: &str| match name {
            "MAX_TIME" => Some("20".to_owned()),
            "NAME" => Some("from env".to_owned()),
            _ => None,
        };
        let got = resolve_options(&m, &args(&["-max-time", "30"]), &env).unwrap();
        assert_eq!(got["max-time"], json!(30));
        assert_eq!(got["name"], json!("from env"));
        assert_eq!(got["on"], json!(true));
        let got = resolve_options(&m, &[], &env).unwrap();
        assert_eq!(got["max-time"], json!(20));
        let got = resolve_options(&m, &[], &no_env).unwrap();
        assert_eq!(got["max-time"], json!(10));
        assert_eq!(got["name"], json!("dflt"));
    }

    #[test]
    fn types_values_and_defaults() {
        use ManifestOptionType::*;
        let m = manifest(
            vec![
                option("f", Float, Some(json!(1))),
                option("s", String, Some(json!(7))),
                option("b", Bool, Some(json!(false))),
                option("n", Int, None),
            ],
            None,
        );
        let got = resolve_options(&m, &args(&["-b", "T"]), &no_env).unwrap();
        assert_eq!(got["f"], json!(1.0));
        assert_eq!(got["s"], json!("7"));
        assert_eq!(got["b"], json!(true));
        assert_eq!(got["n"], Value::Null);
        let got = resolve_options(&m, &args(&["-b", "nope"]), &no_env).unwrap();
        assert_eq!(got["b"], json!(false));
    }

    #[test]
    fn rejects_values_and_defaults_that_do_not_fit() {
        use ManifestOptionType::*;
        let m = manifest(vec![option("n", Int, None), option("f", Float, None)], None);
        for bad in [
            &["-n", "1.5"][..],
            &["-n", "x"],
            &["-f", "NaN"],
            &["-f", "inf"],
        ] {
            let err = resolve_options(&m, &args(bad), &no_env).unwrap_err();
            assert!(matches!(err, Error::Invalid { .. }), "{bad:?}: {err}");
        }
        for default in [json!(1.5), json!(true), json!([1])] {
            let m = manifest(vec![option("n", Int, Some(default.clone()))], None);
            let err = resolve_options(&m, &[], &no_env).unwrap_err();
            assert!(matches!(err, Error::Invalid { .. }), "{default}: {err}");
        }
    }

    #[test]
    fn requires_a_required_option() {
        let mut required = option("n", ManifestOptionType::Int, None);
        required.required = true;
        let m = manifest(vec![required], None);
        let err = resolve_options(&m, &[], &no_env).unwrap_err();
        assert!(matches!(err, Error::Invalid { .. }), "{err}");
        assert!(resolve_options(&m, &args(&["-n", "1"]), &no_env).is_ok());
    }

    #[tokio::test]
    async fn loads_a_json_workspace() {
        let dir = TempDir::new();
        dir.write(
            "workspace.yaml",
            "files: [main.py]\nconfiguration:\n  options:\n    items:\n      - name: duration\n        option_type: int\n        default: 30\n",
        );
        dir.write("in.json", r#"{"stops": [1, 2]}"#);
        let run = load(RuntimeLoadParams {
            workspace_dir: Some(dir.0.clone()),
            args: Some(args(&["-duration", "5"])),
            input_path: Some(dir.0.join("in.json")),
        })
        .await
        .unwrap();
        assert_eq!(run.workspace_dir, dir.0);
        assert_eq!(run.content_format, ContentFormat::Json);
        assert_eq!(run.input, Some(json!({"stops": [1, 2]})));
        assert_eq!(run.options["duration"], json!(5));
        assert_eq!((run.input_dir, run.solutions_dir), (None, None));

        dir.write("in.json", "{ not json");
        let err = load(RuntimeLoadParams {
            workspace_dir: Some(dir.0.clone()),
            args: Some(Vec::new()),
            input_path: Some(dir.0.join("in.json")),
        })
        .await
        .unwrap_err();
        assert!(matches!(err, Error::Invalid { .. }), "{err}");
    }

    #[tokio::test]
    async fn loads_a_multi_file_workspace() {
        let dir = TempDir::new();
        dir.write(
            "workspace.yaml",
            "files: [main.py]\nconfiguration:\n  content:\n    format: multi-file\n    multi-file:\n      input:\n        path: data\n      output:\n        solutions: out/sols\n",
        );
        dir.write("data/b.csv", "b");
        dir.write("data/sub/a.csv", "a");
        let params = RuntimeLoadParams {
            workspace_dir: Some(dir.0.clone()),
            args: Some(Vec::new()),
            input_path: None,
        };
        let run = load(params.clone()).await.unwrap();
        assert_eq!(run.content_format, ContentFormat::MultiFile);
        assert_eq!(run.input, None);
        assert_eq!(run.input_dir, Some(dir.0.join("data")));
        assert_eq!(
            run.input_files,
            [PathBuf::from("b.csv"), PathBuf::from("sub/a.csv")]
        );
        assert_eq!(run.solutions_dir, Some(dir.0.join("out/sols")));
        assert!(dir.0.join("out/sols").is_dir());

        // Another directory, and one that is not there.
        dir.write("other/x.txt", "x");
        let run = load(RuntimeLoadParams {
            input_path: Some(dir.0.join("other")),
            ..params.clone()
        })
        .await
        .unwrap();
        assert_eq!(run.input_files, [PathBuf::from("x.txt")]);
        let err = load(RuntimeLoadParams {
            input_path: Some(dir.0.join("missing")),
            ..params
        })
        .await
        .unwrap_err();
        assert!(matches!(err, Error::Io { .. }), "{err}");
    }

    #[tokio::test]
    async fn rejects_manifest_paths_that_leave_the_workspace() {
        let dir = TempDir::new();
        dir.write(
            "workspace.yaml",
            "files: [main.py]\nconfiguration:\n  content:\n    format: multi-file\n    multi-file:\n      input:\n        path: ../inputs\n",
        );
        let err = load(RuntimeLoadParams {
            workspace_dir: Some(dir.0.clone()),
            args: Some(Vec::new()),
            input_path: None,
        })
        .await
        .unwrap_err();
        assert!(matches!(err, Error::Invalid { .. }), "{err}");
    }

    #[tokio::test]
    async fn needs_the_manifest_of_an_explicit_directory() {
        let dir = TempDir::new();
        let err = load(RuntimeLoadParams {
            workspace_dir: Some(dir.0.clone()),
            args: Some(Vec::new()),
            input_path: None,
        })
        .await
        .unwrap_err();
        assert!(matches!(err, Error::Io { .. }), "{err}");
    }

    fn write_params(dir: &TempDir) -> RuntimeWriteParams {
        RuntimeWriteParams {
            workspace_dir: Some(dir.0.clone()),
            ..RuntimeWriteParams::default()
        }
    }

    fn read_json(path: PathBuf) -> Value {
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    }

    fn metrics() -> Map<String, Value> {
        json!({ "cost": 4.5, "nested": { "n": 1 } })
            .as_object()
            .cloned()
            .unwrap()
    }

    #[tokio::test]
    async fn writes_a_json_output_to_a_directory() {
        let dir = TempDir::new();
        dir.write("workspace.yaml", "files: [main.py]\n");
        let out = dir.0.join("out");
        write(RuntimeWriteParams {
            output: Some(json!({ "route": [1, 2] })),
            metrics: Some(metrics()),
            assets: Some(vec![
                json!({ "name": "a", "content_type": "json", "content": {} }),
            ]),
            options: Some(BTreeMap::from([("duration".to_owned(), json!(5))])),
            output_dir: Some(out.clone()),
            ..write_params(&dir)
        })
        .await
        .unwrap();
        let envelope = read_json(out.join("solutions/solution.json"));
        assert_eq!(envelope["solution"], json!({ "route": [1, 2] }));
        assert_eq!(envelope["options"], json!({ "duration": 5 }));
        assert_eq!(envelope["metrics"]["cost"], json!(4.5));
        assert_eq!(envelope["assets"][0]["name"], json!("a"));
        assert_eq!(
            read_json(out.join("metrics.json"))["metrics"],
            json!(metrics())
        );
        assert_eq!(
            read_json(out.join("assets.json"))["assets"][0]["name"],
            json!("a")
        );
    }

    #[tokio::test]
    async fn fills_the_defaults_of_a_json_output() {
        let dir = TempDir::new();
        dir.write("workspace.yaml", "files: [main.py]\n");
        let out = dir.0.join("out");
        write(RuntimeWriteParams {
            output_dir: Some(out.clone()),
            ..write_params(&dir)
        })
        .await
        .unwrap();
        assert_eq!(
            read_json(out.join("solutions/solution.json")),
            json!({ "options": {}, "solution": {}, "assets": [], "metrics": {} })
        );
        assert!(!out.join("metrics.json").exists());
    }

    #[tokio::test]
    async fn writes_a_multi_file_output_to_the_manifest_paths() {
        let dir = TempDir::new();
        dir.write(
            "workspace.yaml",
            "files: [main.py]\nconfiguration:\n  content:\n    format: multi-file\n    multi-file:\n      output:\n        metrics: out/m.json\n        solutions: out/sols\n",
        );
        write(RuntimeWriteParams {
            output_files: BTreeMap::from([
                ("a.json".to_owned(), json!({ "a": 1 })),
                ("sub/b.json".to_owned(), json!([2])),
            ]),
            metrics: Some(metrics()),
            assets: Some(Vec::new()),
            ..write_params(&dir)
        })
        .await
        .unwrap();
        assert_eq!(read_json(dir.0.join("out/sols/a.json")), json!({ "a": 1 }));
        assert_eq!(read_json(dir.0.join("out/sols/sub/b.json")), json!([2]));
        assert_eq!(
            read_json(dir.0.join("out/m.json"))["metrics"],
            json!(metrics())
        );
        // Empty assets are not written, and the default path is unused.
        assert!(!dir.0.join("outputs/assets.json").exists());
    }

    #[tokio::test]
    async fn writes_to_the_workspace_dir_for_an_empty_solutions_path() {
        let dir = TempDir::new();
        dir.write(
            "workspace.yaml",
            "files: [main.py]\nconfiguration:\n  content:\n    format: multi-file\n    multi-file:\n      output:\n        solutions: \"\"\n",
        );
        write(RuntimeWriteParams {
            output_files: BTreeMap::from([("a.json".to_owned(), json!(1))]),
            ..write_params(&dir)
        })
        .await
        .unwrap();
        assert_eq!(read_json(dir.0.join("a.json")), json!(1));
    }

    #[tokio::test]
    async fn writes_a_multi_file_output_to_a_directory() {
        let dir = TempDir::new();
        dir.write(
            "workspace.yaml",
            "files: [main.py]\nconfiguration:\n  content:\n    format: multi-file\n",
        );
        let out = dir.0.join("out");
        write(RuntimeWriteParams {
            output_files: BTreeMap::from([("a.json".to_owned(), json!(1))]),
            assets: Some(vec![json!({ "name": "a" })]),
            output_dir: Some(out.clone()),
            ..write_params(&dir)
        })
        .await
        .unwrap();
        assert_eq!(read_json(out.join("solutions/a.json")), json!(1));
        assert_eq!(
            read_json(out.join("assets.json"))["assets"][0]["name"],
            json!("a")
        );
        assert!(!out.join("metrics.json").exists());
    }

    #[tokio::test]
    async fn rejects_output_of_the_wrong_kind_or_outside_the_workspace() {
        let dir = TempDir::new();
        dir.write("workspace.yaml", "files: [main.py]\n");
        let err = write(RuntimeWriteParams {
            output_files: BTreeMap::from([("a.json".to_owned(), json!(1))]),
            ..write_params(&dir)
        })
        .await
        .unwrap_err();
        assert!(matches!(err, Error::Invalid { .. }), "{err}");

        dir.write(
            "workspace.yaml",
            "files: [main.py]\nconfiguration:\n  content:\n    format: multi-file\n",
        );
        let err = write(RuntimeWriteParams {
            output: Some(json!({})),
            ..write_params(&dir)
        })
        .await
        .unwrap_err();
        assert!(matches!(err, Error::Invalid { .. }), "{err}");
        for name in ["../x.json", "/tmp/x.json"] {
            let err = write(RuntimeWriteParams {
                output_files: BTreeMap::from([(name.to_owned(), json!(1))]),
                ..write_params(&dir)
            })
            .await
            .unwrap_err();
            assert!(matches!(err, Error::Invalid { .. }), "{name}: {err}");
        }
    }
}
