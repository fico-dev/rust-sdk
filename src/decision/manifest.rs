//! The workspace manifest, `workspace.yaml`.
//!
//! [`validate`] reads and checks a manifest, [`write()`] writes one back,
//! and [`init`] writes a sample one. `option_args` turns run options into
//! the workspace's arguments in the manifest's options format.
//! `find_files` selects the files that the manifest's `files` patterns
//! list, for local runs and pushes, and `pyproject_dependencies` reads the
//! packages of a `pyproject.toml`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Component, Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

use crate::decision::fs::write_atomically;
use crate::decision::{
    ContentFormat, ControlType, Manifest, ManifestOption, ManifestOptionType, ManifestType,
};
use crate::tool::Tool;
use crate::{Error, Result};

/// Name of the manifest file in a workspace's directory.
pub(crate) const MANIFEST_FILE: &str = "workspace.yaml";

/// Parameters for [`init`].
#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
#[serde(default)]
pub struct ManifestInitParams {
    /// Directory to write `workspace.yaml` into, created if it does not exist. A
    /// leading `~` stands for the home directory. Defaults to the current
    /// directory.
    pub workspace_dir: Option<PathBuf>,
    /// Language or kind of the workspace. Defaults to `python`.
    pub manifest_type: ManifestType,
    /// Format of the workspace's input and output. Defaults to `json`.
    pub content_format: ContentFormat,
    /// Whether to add sample options (parameters) to the manifest. Defaults
    /// to `true`.
    pub with_options: bool,
    /// Whether to replace a `workspace.yaml` already in the directory.
    /// Defaults to `false`, which leaves it as it is and fails.
    pub overwrite: bool,
}

impl Default for ManifestInitParams {
    fn default() -> ManifestInitParams {
        ManifestInitParams {
            workspace_dir: None,
            manifest_type: ManifestType::default(),
            content_format: ContentFormat::default(),
            with_options: true,
            overwrite: false,
        }
    }
}

/// Parameters for [`validate`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
#[serde(default)]
pub struct ManifestValidateParams {
    /// Directory that holds the `workspace.yaml` to read. A leading `~` stands for
    /// the home directory. Defaults to the current directory.
    pub workspace_dir: Option<PathBuf>,
}

/// Writes a sample `workspace.yaml` manifest.
///
/// The sample fits the workspace's type and content format, with sample
/// options unless `params.with_options` is `false`. A `workspace.yaml`
/// already in the directory is replaced only if `params.overwrite` is set.
/// Returns the path of the file it wrote.
///
/// # Errors
///
/// Returns [`Error::Invalid`] if the content format is
/// [`ContentFormat::Unknown`], and [`Error::Io`] if the directory or the
/// file cannot be written, with [`ErrorKind::AlreadyExists`] if the
/// directory holds a `workspace.yaml` and `params.overwrite` is not set.
pub async fn init(params: ManifestInitParams) -> Result<PathBuf> {
    let sample = sample(
        params.manifest_type,
        params.content_format,
        params.with_options,
    )?;
    let dir = workspace_dir(params.workspace_dir);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|source| Error::Io {
            path: dir.clone(),
            source,
        })?;
    let path = dir.join(MANIFEST_FILE);
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true);
    match params.overwrite {
        true => options.create(true).truncate(true),
        false => options.create_new(true),
    };
    // tokio writes in the background: `flush` waits for the write to end.
    let written = match options.open(&path).await {
        Ok(mut file) => match file.write_all(sample.as_bytes()).await {
            Ok(()) => file.flush().await,
            Err(error) => Err(error),
        },
        Err(error) => Err(error),
    };
    written.map_err(|source| Error::Io {
        path: path.clone(),
        source,
    })?;
    Ok(path)
}

/// The command-line arguments that pass `options` to the workspace, by name, in
/// the manifest's options format (`configuration.options.format`).
///
/// For each option, every entry of the format becomes an argument, with
/// `{{name}}` and `{{value}}` replaced, and entries that end up empty are
/// left out, as the platform does. Without a format, or with an empty one,
/// each option becomes `-<name> <value>`.
pub(crate) fn option_args(manifest: &Manifest, options: &BTreeMap<String, String>) -> Vec<String> {
    let format = manifest
        .configuration
        .options
        .as_ref()
        .and_then(|options| options.format.as_deref())
        .filter(|format| !format.is_empty());
    let mut args = Vec::new();
    for (name, value) in options {
        let Some(format) = format else {
            args.extend([format!("-{name}"), value.clone()]);
            continue;
        };
        let rendered = format
            .iter()
            .map(|entry| entry.replace("{{name}}", name).replace("{{value}}", value))
            .filter(|arg| !arg.is_empty());
        args.extend(rendered);
    }
    args
}

/// The files that the patterns in `files` (a manifest's `files`) select in
/// the workspace directory `src`, relative to it and sorted, and the
/// patterns that select nothing.
///
/// Patterns follow Python's `glob` with `recursive=True`: `*`, `?` and
/// `[…]` match within a name, and `**` matches any number of directories.
/// None of them matches a name that starts with `.` unless the pattern
/// does too. A trailing `/` stands for the files directly in a directory,
/// and a leading `!` removes the files it matches from those that the
/// patterns before it selected.
///
/// Only files are selected. A pattern that matches only directories, such
/// as `src` without a trailing `/`, selects nothing but is not reported,
/// and neither is a negated one. `**` does not walk into links to
/// directories, so that a link can't make it loop.
///
/// # Errors
///
/// Returns [`Error::Invalid`] if a pattern is absolute or has a `..`
/// component, as it could select files outside `src`, and [`Error::Io`]
/// if a directory cannot be read.
pub(crate) async fn find_files(
    src: &Path,
    files: &[String],
) -> Result<(Vec<PathBuf>, Vec<String>)> {
    let (src, files) = (src.to_path_buf(), files.to_vec());
    let dir = src.clone();
    match tokio::task::spawn_blocking(move || select_files(&src, &files)).await {
        Ok(result) => result,
        Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
        Err(error) => Err(Error::Io {
            path: dir,
            source: io::Error::other(error),
        }),
    }
}

/// The workspace's entrypoint, relative to its directory: the manifest's
/// `execution.entrypoint`, or else `./main.py`, `./main` or `./main.jar`
/// by the workspace's type.
pub(crate) fn entrypoint(manifest: &Manifest) -> &str {
    let set = manifest.execution.as_ref();
    if let Some(entrypoint) = set.and_then(|execution| execution.entrypoint.as_deref()) {
        return entrypoint;
    }
    match manifest.manifest_type {
        ManifestType::Python => "./main.py",
        ManifestType::Go | ManifestType::Binary => "./main",
        ManifestType::Java => "./main.jar",
    }
}

/// Checks that `files`, as [`find_files`] selects them, hold the workspace's
/// [`entrypoint`].
///
/// # Errors
///
/// Returns [`Error::Invalid`] if they don't.
pub(crate) fn check_mandatory_files(manifest: &Manifest, files: &[PathBuf]) -> Result<()> {
    let entrypoint = normalized(Path::new(entrypoint(manifest)));
    if files.contains(&entrypoint) {
        return Ok(());
    }
    Err(Error::Invalid {
        reason: format!(
            "the manifest's `files` must select the workspace's entrypoint, `{}`",
            entrypoint.display()
        ),
    })
}

/// [`find_files`], blocking.
fn select_files(src: &Path, files: &[String]) -> Result<(Vec<PathBuf>, Vec<String>)> {
    let mut selected = BTreeSet::new();
    let mut missing = Vec::new();
    for filter in files {
        let pattern = match filter.ends_with('/') {
            true => format!("{filter}*"),
            false => filter.clone(),
        };
        let (negated, pattern) = match pattern.strip_prefix('!') {
            Some(pattern) => (true, pattern),
            None => (false, pattern.as_str()),
        };
        let matches = glob(src, pattern)?;
        if negated {
            selected.retain(|file| !matches.contains(file));
        } else if matches.is_empty() {
            missing.push(filter.clone());
        } else {
            selected.extend(matches.into_iter().filter(|path| src.join(path).is_file()));
        }
    }
    Ok((selected.into_iter().collect(), missing))
}

/// A component of a glob pattern.
#[derive(Debug, PartialEq)]
enum Part<'a> {
    /// A name without wildcards, which matches itself.
    Name(&'a str),
    /// A name with wildcards, which matches the names [`fnmatch`] accepts.
    Pattern(&'a str),
    /// `**`, which matches any number of directories.
    AnyDirs,
}

/// What the glob `pattern` matches under `src`, files and directories,
/// relative to `src`.
fn glob(src: &Path, pattern: &str) -> Result<BTreeSet<PathBuf>> {
    let escapes = Path::new(pattern).components().any(|component| {
        matches!(
            component,
            Component::Prefix(_) | Component::RootDir | Component::ParentDir
        )
    });
    if escapes {
        return Err(Error::Invalid {
            reason: format!(
                "the manifest's `files` pattern `{pattern}` is absolute or has a `..`, \
                 so it could select files outside the workspace"
            ),
        });
    }
    let mut parts: Vec<Part<'_>> = Vec::new();
    for part in pattern.split(std::path::is_separator) {
        let part = match part {
            "" | "." => continue,
            "**" if parts.last() == Some(&Part::AnyDirs) => continue,
            "**" => Part::AnyDirs,
            part if part.contains(['*', '?', '[']) => Part::Pattern(part),
            part => Part::Name(part),
        };
        parts.push(part);
    }
    let mut matches = BTreeSet::new();
    if !pattern.is_empty() {
        walk(src, PathBuf::new(), &parts, &mut matches)?;
    }
    Ok(matches)
}

/// Adds what `parts` match under `src.join(dir)` to `matches`, as paths
/// relative to `src`.
fn walk(
    src: &Path,
    dir: PathBuf,
    parts: &[Part<'_>],
    matches: &mut BTreeSet<PathBuf>,
) -> Result<()> {
    let Some((part, rest)) = parts.split_first() else {
        matches.insert(dir);
        return Ok(());
    };
    match part {
        Part::Name(name) => {
            let path = dir.join(name);
            let full = src.join(&path);
            let found = match rest.is_empty() {
                true => fs::symlink_metadata(&full).is_ok(),
                false => full.is_dir(),
            };
            if found {
                walk(src, path, rest, matches)?;
            }
        }
        Part::Pattern(pattern) => {
            for entry in entries(&src.join(&dir))? {
                let visible = !entry.name.starts_with('.') || pattern.starts_with('.');
                if visible && (rest.is_empty() || entry.is_dir) && fnmatch(pattern, &entry.name) {
                    walk(src, dir.join(&entry.file_name), rest, matches)?;
                }
            }
        }
        Part::AnyDirs => {
            // A leading `**` matches no directory without matching `src`
            // itself, as in Python.
            if !(rest.is_empty() && dir.as_os_str().is_empty()) {
                walk(src, dir.clone(), rest, matches)?;
            }
            for entry in entries(&src.join(&dir))? {
                if entry.name.starts_with('.') {
                    continue;
                }
                let path = dir.join(&entry.file_name);
                if entry.is_dir && !entry.is_link {
                    walk(src, path, parts, matches)?;
                } else if rest.is_empty() {
                    matches.insert(path);
                }
            }
        }
    }
    Ok(())
}

/// An entry of a directory, for [`walk`].
struct Entry {
    /// The entry's name.
    file_name: std::ffi::OsString,
    /// The entry's name as text, for matching.
    name: String,
    /// Whether it is a directory, or a link to one.
    is_dir: bool,
    /// Whether it is a link.
    is_link: bool,
}

/// The entries of `dir`, or none if it is not a directory.
fn entries(dir: &Path) -> Result<Vec<Entry>> {
    let read = match fs::read_dir(dir) {
        Ok(read) => read,
        Err(error) if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
            return Ok(Vec::new());
        }
        Err(source) => {
            return Err(Error::Io {
                path: dir.to_path_buf(),
                source,
            });
        }
    };
    let mut entries = Vec::new();
    for entry in read {
        let entry = entry.map_err(|source| Error::Io {
            path: dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        let is_link = entry.file_type().is_ok_and(|kind| kind.is_symlink());
        entries.push(Entry {
            name: entry.file_name().to_string_lossy().into_owned(),
            file_name: entry.file_name(),
            is_dir: path.is_dir(),
            is_link,
        });
    }
    Ok(entries)
}

/// Whether `name` matches the wildcard `pattern`, as Python's `fnmatch`
/// does: `*` matches any characters, `?` any one, `[…]` one in a set
/// (`[!…]`: not in it), with ranges such as `a-z`, and everything else
/// itself. Case matters, except on Windows.
fn fnmatch(pattern: &str, name: &str) -> bool {
    let chars = |text: &str| -> Vec<char> {
        match cfg!(windows) {
            true => text.to_lowercase().chars().collect(),
            false => text.chars().collect(),
        }
    };
    let (pattern, name) = (chars(pattern), chars(name));
    let (mut p, mut n) = (0, 0);
    // Where to go on after the last `*`: its position in the pattern, plus
    // one, and how much of the name it matches so far.
    let mut star = None;
    while n < name.len() {
        let step = match pattern.get(p) {
            Some('*') => {
                star = Some((p + 1, n));
                p += 1;
                continue;
            }
            Some('?') => Some(1),
            Some('[') => match set(&pattern[p..], name[n]) {
                Some((len, true)) => Some(len),
                Some((_, false)) => None,
                None => (name[n] == '[').then_some(1),
            },
            Some(&c) => (c == name[n]).then_some(1),
            None => None,
        };
        match (step, star) {
            (Some(len), _) => {
                p += len;
                n += 1;
            }
            (None, Some((after, matched))) => {
                p = after;
                n = matched + 1;
                star = Some((after, matched + 1));
            }
            (None, None) => return false,
        }
    }
    pattern[p..].iter().all(|&c| c == '*')
}

/// The set (`[…]`) at the start of `pattern`: its length, and whether `c`
/// is in it. `None` if it has no closing `]`, which makes the `[` a plain
/// character. A `]` right after the `[` or `[!` is a member.
fn set(pattern: &[char], c: char) -> Option<(usize, bool)> {
    let mut close = 1;
    if pattern.get(close) == Some(&'!') {
        close += 1;
    }
    if pattern.get(close) == Some(&']') {
        close += 1;
    }
    while *pattern.get(close)? != ']' {
        close += 1;
    }
    let mut members = &pattern[1..close];
    let negated = members.first() == Some(&'!');
    if negated {
        members = &members[1..];
    }
    let mut found = false;
    let mut i = 0;
    while i < members.len() {
        if i + 2 < members.len() && members[i + 1] == '-' {
            found |= (members[i]..=members[i + 2]).contains(&c);
            i += 3;
        } else {
            found |= members[i] == c;
            i += 1;
        }
    }
    Some((close + 1, found != negated))
}

/// `path` without `.` components, and with each `..` taking out the
/// component before it, as Python's `os.path.normpath` does.
fn normalized(path: &Path) -> PathBuf {
    let mut normal = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir
                if matches!(normal.components().next_back(), Some(Component::Normal(_))) =>
            {
                normal.pop();
            }
            component => normal.push(component),
        }
    }
    normal
}

/// The sample manifest for a workspace of `manifest_type` and `format`, with
/// sample options if `with_options` is set: one of the templates in
/// `manifest/templates/`.
fn sample(
    manifest_type: ManifestType,
    format: ContentFormat,
    with_options: bool,
) -> Result<&'static str> {
    use ContentFormat::{Json, MultiFile};
    use ManifestType::{Binary, Go, Java, Python};
    macro_rules! template {
        ($name:literal) => {
            include_str!(concat!("manifest/templates/", $name, ".yaml"))
        };
    }
    let sample = match (manifest_type, format, with_options) {
        (Python, Json, false) => template!("python_json"),
        (Python, Json, true) => template!("python_json_opt"),
        (Python, MultiFile, false) => template!("python_multi-file"),
        (Python, MultiFile, true) => template!("python_multi-file_opt"),
        (Go, Json, false) => template!("go_json"),
        (Go, Json, true) => template!("go_json_opt"),
        (Go, MultiFile, false) => template!("go_multi-file"),
        (Go, MultiFile, true) => template!("go_multi-file_opt"),
        (Java, Json, false) => template!("java_json"),
        (Java, Json, true) => template!("java_json_opt"),
        (Java, MultiFile, false) => template!("java_multi-file"),
        (Java, MultiFile, true) => template!("java_multi-file_opt"),
        (Binary, Json, false) => template!("binary_json"),
        (Binary, Json, true) => template!("binary_json_opt"),
        (Binary, MultiFile, false) => template!("binary_multi-file"),
        (Binary, MultiFile, true) => template!("binary_multi-file_opt"),
        (_, ContentFormat::Unknown, _) => {
            return Err(Error::Invalid {
                reason: "a manifest's content format must be `json` or `multi-file`".to_owned(),
            });
        }
    };
    Ok(sample)
}

/// Reads a `workspace.yaml` manifest and checks that it is valid.
///
/// Fields the file leaves out get their defaults: e.g. the `json` content
/// format, or the default input and output locations of a `multi-file`
/// workspace. The checks are:
///
/// - `files` lists at least one file;
/// - the `multi-file` locations are only set for the `multi-file` format;
/// - `enforce` is `none` or `all`;
/// - no option is both required and local only;
/// - each option's control fits its type, and the option sets the
///   additional attributes that the two require, and no others.
///
/// When a key appears twice in a mapping, the last one wins.
///
/// # Errors
///
/// Returns [`Error::Io`] if the file cannot be read, and
/// [`Error::Manifest`] if it is not a valid manifest.
pub async fn validate(params: ManifestValidateParams) -> Result<Manifest> {
    read(workspace_dir(params.workspace_dir).join(MANIFEST_FILE)).await
}

/// Writes `manifest` as the `workspace.yaml` of `workspace_dir`, the
/// counterpart of [`validate`].
///
/// The YAML is checked first, as [`validate`] checks a file it reads, so a
/// file this writes always reads back. A leading `~` in `workspace_dir`
/// stands for the home directory, which must exist. A `workspace.yaml`
/// already there is replaced, and comments are not kept. The file is
/// written to a temporary file and renamed into place, so a write that
/// fails leaves the old one as it was. Returns the path of the file it
/// wrote.
///
/// # Errors
///
/// Returns [`Error::Manifest`] if `manifest` is not valid, and
/// [`Error::Io`] if the file cannot be written.
pub async fn write(workspace_dir: impl AsRef<Path>, manifest: &Manifest) -> Result<PathBuf> {
    let path = expand_home(workspace_dir.as_ref().to_owned()).join(MANIFEST_FILE);
    let invalid = |reason| Error::Manifest {
        path: path.clone(),
        reason,
    };
    if manifest.configuration.content.format == ContentFormat::Unknown {
        return Err(invalid(
            "the content format must be `json` or `multi-file`".to_owned(),
        ));
    }
    let yaml = serde_saphyr::to_string(manifest)
        .map_err(|e| invalid(format!("the manifest could not be written as YAML: {e}")))?;
    // Check what `validate` will read, not the manifest: a value that
    // serializes but does not deserialize (e.g. an `Unknown` variant)
    // fails here, so the round trip holds.
    parse(yaml.as_bytes()).map_err(invalid)?;
    write_atomically(&path, yaml.as_bytes()).await?;
    Ok(path)
}

/// Reads the manifest file at `path`, which may have any name, and checks
/// that it is valid, as [`validate`] does.
///
/// # Errors
///
/// Returns [`Error::Io`] if the file cannot be read, and
/// [`Error::Manifest`] if it is not a valid manifest.
pub(crate) async fn read(path: PathBuf) -> Result<Manifest> {
    let bytes = tokio::fs::read(&path).await.map_err(|source| Error::Io {
        path: path.clone(),
        source,
    })?;
    parse(&bytes).map_err(|reason| Error::Manifest { path, reason })
}

/// The manifest in `bytes`, with its defaults filled in and checked, or why
/// it is not a valid manifest.
fn parse(bytes: &[u8]) -> Result<Manifest, String> {
    let text = std::str::from_utf8(bytes).map_err(|e| format!("the file is not UTF-8: {e}"))?;
    let options = serde_saphyr::options! {
        duplicate_keys: serde_saphyr::DuplicateKeyPolicy::LastWins,
    };
    let mut manifest: Manifest =
        serde_saphyr::from_str_with_options(text, options).map_err(|e| {
            // `LastWins` makes serde-saphyr replay each mapping, and a nested
            // error gets one `(defined at …) at …` per enclosing mapping after
            // its own location: keep only that one.
            let reason = e.without_snippet().to_string();
            match reason.split_once(" (defined at ") {
                Some((first, _)) => first.to_owned(),
                None => reason,
            }
        })?;
    let content = &mut manifest.configuration.content;
    if content.format == ContentFormat::MultiFile && content.multi_file.is_none() {
        content.multi_file = Some(Default::default());
    }
    check(&manifest)?;
    Ok(manifest)
}

/// Checks the manifest rules that the types do not enforce.
fn check(manifest: &Manifest) -> Result<(), String> {
    if manifest.files.is_empty() {
        return Err("`files` must list at least one file".to_owned());
    }
    let content = &manifest.configuration.content;
    if content.multi_file.is_some() && content.format != ContentFormat::MultiFile {
        return Err(format!(
            "`multi-file` is set, but the content format is `{}`, not `multi-file`: \
             remove `multi-file` or change the format",
            wire(content.format)
        ));
    }
    let Some(options) = &manifest.configuration.options else {
        return Ok(());
    };
    if let Some(validation) = &options.validation
        && !matches!(validation.enforce.as_str(), "none" | "all")
    {
        return Err(format!(
            "`enforce` is `{}`, but it must be `none` or `all`",
            validation.enforce
        ));
    }
    options.items.iter().flatten().try_for_each(check_option)
}

/// Checks an option: it is not both required and local only, its control
/// fits its type, and it sets the additional attributes that the two
/// require, and no others.
fn check_option(option: &ManifestOption) -> Result<(), String> {
    let name = &option.name;
    if option.required && option.local_only {
        return Err(format!(
            "option `{name}` cannot be both required and local only"
        ));
    }
    let Some(control) = option.ui.as_ref().and_then(|ui| ui.control_type) else {
        return Ok(());
    };
    let combination = format!(
        "option type `{}` with control type `{}`",
        wire(option.option_type),
        wire(control)
    );
    let Some((allowed, required)) = attributes(option.option_type, control) else {
        let fitting: Vec<String> = CONTROLS
            .into_iter()
            .filter(|&control| attributes(option.option_type, control).is_some())
            .map(wire)
            .collect();
        return Err(format!(
            "option `{name}` has {combination}, which does not fit: use control type {}",
            listed(fitting.iter().map(String::as_str))
        ));
    };
    let set = option.additional_attributes.as_ref();
    let missing: Vec<&str> = required
        .iter()
        .copied()
        .filter(|attribute| !set.is_some_and(|set| set.contains_key(*attribute)))
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "option `{name}` does not set the additional attributes {}, which {combination} \
             requires",
            listed(missing)
        ));
    }
    let extra: Vec<&str> = set
        .into_iter()
        .flat_map(|set| set.keys())
        .map(String::as_str)
        .filter(|attribute| !allowed.contains(attribute))
        .collect();
    if !extra.is_empty() {
        let takes = match allowed {
            [] => "none".to_owned(),
            allowed => listed(allowed.iter().copied()),
        };
        return Err(format!(
            "option `{name}` sets the additional attributes {}, which {combination} does not \
             take: it takes {takes}",
            listed(extra)
        ));
    }
    Ok(())
}

/// Every control type, in the order of their names.
const CONTROLS: [ControlType; 5] = [
    ControlType::Input,
    ControlType::Multiselect,
    ControlType::Select,
    ControlType::Slider,
    ControlType::Toggle,
];

/// The additional attributes that an option of `option_type` with `control`
/// may set, and those it must set, or `None` if the control does not fit the
/// type.
fn attributes(
    option_type: ManifestOptionType,
    control: ControlType,
) -> Option<(&'static [&'static str], &'static [&'static str])> {
    const LENGTH: &[&str] = &["max_length", "min_length"];
    const RANGE: &[&str] = &["max", "min", "step"];
    const VALUES: &[&str] = &["values"];
    use ControlType::{Input, Multiselect, Select, Slider, Toggle};
    use ManifestOptionType::{Bool, Float, Int};
    match (option_type, control) {
        (ManifestOptionType::String, Input) => Some((LENGTH, &[])),
        (ManifestOptionType::String, Select | Multiselect) => Some((VALUES, VALUES)),
        (Bool, Toggle) => Some((&[], &[])),
        (Int | Float, Input) => Some((RANGE, &[])),
        (Int | Float, Slider) => Some((RANGE, RANGE)),
        (Int | Float, Select) => Some((VALUES, VALUES)),
        _ => None,
    }
}

/// `names` as a list of code spans, e.g. `` `max`, `min` ``.
fn listed<'a>(names: impl IntoIterator<Item = &'a str>) -> String {
    names
        .into_iter()
        .map(|name| format!("`{name}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The YAML value of an enum variant, e.g. `multi-file`.
fn wire(value: impl Serialize) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(name)) => name,
        _ => String::new(),
    }
}

/// `dir`, or the current directory, with a leading `~` replaced by the home
/// directory.
pub(crate) fn workspace_dir(dir: Option<PathBuf>) -> PathBuf {
    expand_home(dir.unwrap_or_else(|| PathBuf::from(".")))
}

/// `path` with a leading `~` replaced by the home directory. `~user` is
/// left as it is.
pub(crate) fn expand_home(path: PathBuf) -> PathBuf {
    let Ok(rest) = path.strip_prefix("~") else {
        return path;
    };
    match std::env::home_dir() {
        Some(home) if rest.as_os_str().is_empty() => home,
        Some(home) => home.join(rest),
        None => path,
    }
}

/// The packages that `pyproject`, a `pyproject.toml`, lists in
/// `[project].dependencies`, or why it can't read them.
///
/// It reads the TOML that the key takes in practice: a `[project]` table
/// with a `dependencies` array of basic (`"…"`) or literal (`'…'`) strings,
/// which may span lines and hold comments and a trailing comma.
pub(crate) fn pyproject_dependencies(pyproject: &str) -> Result<Vec<String>, String> {
    let mut table = String::new();
    let mut offset = 0;
    for line in pyproject.split_inclusive('\n') {
        let start = offset;
        offset += line.len();
        let trimmed = line.trim();
        if let Some(header) = trimmed.strip_prefix('[') {
            table = header
                .split(']')
                .next()
                .unwrap_or_default()
                .trim()
                .to_owned();
            continue;
        }
        let assigned = trimmed
            .strip_prefix("dependencies")
            .is_some_and(|rest| rest.trim_start().starts_with('='));
        if table == "project" && assigned {
            let value = start + line.find('=').unwrap_or_default() + 1;
            return string_array(&pyproject[value..]);
        }
    }
    Err("`[project].dependencies` not found".to_owned())
}

/// The strings of the TOML array at the start of `text`.
fn string_array(text: &str) -> Result<Vec<String>, String> {
    let mut chars = text.chars().peekable();
    while chars.next_if(|&c| c == ' ' || c == '\t').is_some() {}
    if chars.next() != Some('[') {
        return Err("`[project].dependencies` must be a list".to_owned());
    }
    let mut items = Vec::new();
    loop {
        skip_blank(&mut chars);
        match chars.peek() {
            Some(']') => return Ok(items),
            Some(&quote @ ('"' | '\'')) => {
                chars.next();
                items.push(toml_string(&mut chars, quote)?);
            }
            Some(_) => return Err("`[project].dependencies` must hold only strings".to_owned()),
            None => return Err("`[project].dependencies` has no closing `]`".to_owned()),
        }
        skip_blank(&mut chars);
        match chars.next() {
            Some(',') => {}
            Some(']') => return Ok(items),
            Some(_) => return Err("`[project].dependencies` is not a valid array".to_owned()),
            None => return Err("`[project].dependencies` has no closing `]`".to_owned()),
        }
    }
}

/// Skips whitespace, line breaks and comments.
fn skip_blank(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    loop {
        match chars.peek() {
            Some(c) if c.is_whitespace() => {
                chars.next();
            }
            Some('#') => while chars.next_if(|&c| c != '\n').is_some() {},
            _ => return,
        }
    }
}

/// The rest of a TOML string opened with `quote`: a basic string with
/// escapes for `"`, a literal string for `'`.
fn toml_string(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    quote: char,
) -> Result<String, String> {
    if chars.next_if_eq(&quote).is_some() {
        if chars.peek() == Some(&quote) {
            let reason = "multi-line strings in `[project].dependencies` are not supported";
            return Err(reason.to_owned());
        }
        return Ok(String::new());
    }
    let mut value = String::new();
    loop {
        match chars.next() {
            None | Some('\n') => {
                return Err("a string in `[project].dependencies` is not closed".to_owned());
            }
            Some(c) if c == quote => return Ok(value),
            Some('\\') if quote == '"' => {
                let escaped = match chars.next() {
                    Some('b') => '\u{8}',
                    Some('t') => '\t',
                    Some('n') => '\n',
                    Some('f') => '\u{c}',
                    Some('r') => '\r',
                    Some('"') => '"',
                    Some('\\') => '\\',
                    Some(kind @ ('u' | 'U')) => {
                        let len = if kind == 'u' { 4 } else { 8 };
                        let hex: String = chars.by_ref().take(len).collect();
                        u32::from_str_radix(&hex, 16)
                            .ok()
                            .filter(|_| hex.len() == len)
                            .and_then(char::from_u32)
                            .ok_or("a string in `[project].dependencies` has a bad `\\u` escape")?
                    }
                    _ => {
                        return Err(
                            "a string in `[project].dependencies` has a bad escape".to_owned()
                        );
                    }
                };
                value.push(escaped);
            }
            Some(c) => value.push(c),
        }
    }
}

pub(crate) fn tools() -> Vec<Tool> {
    vec![
        Tool::new(
            "decision_manifest_init",
            "Writes a sample `workspace.yaml` manifest.",
            |cx, a: ManifestInitParams| async move {
                cx.local()?;
                init(a).await
            },
        )
        .local_only()
        .destructive(),
        Tool::new(
            "decision_manifest_validate",
            "Reads a `workspace.yaml` manifest and checks that it is valid.",
            |cx, a: ManifestValidateParams| async move {
                cx.local()?;
                validate(a).await
            },
        )
        .local_only()
        .read_only(),
    ]
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    use std::fs;
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::{
        check_mandatory_files, entrypoint, find_files, fnmatch, normalized, option_args,
        pyproject_dependencies, workspace_dir,
    };
    use crate::Error;
    use crate::decision::{
        Manifest, ManifestConfiguration, ManifestExecution, ManifestOptions, ManifestType,
    };

    /// A new directory under the system's temp directory, removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> TempDir {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("fico-sdk-manifest-{}-{n}", std::process::id()));
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

    /// A workspace with visible, hidden and nested files.
    fn workspace() -> TempDir {
        let dir = TempDir::new();
        for file in [
            "main.py",
            "src/a.py",
            "src/b.txt",
            "src/sub/c.py",
            "src/.hidden.py",
            ".config/x.py",
            ".top.py",
        ] {
            let path = dir.0.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, file).unwrap();
        }
        fs::create_dir(dir.0.join("empty")).unwrap();
        dir
    }

    async fn find(dir: &TempDir, files: &[&str]) -> (Vec<String>, Vec<String>) {
        let files: Vec<String> = files.iter().map(|&file| file.to_owned()).collect();
        let (found, missing) = find_files(&dir.0, &files).await.unwrap();
        let found = found.iter().map(|path| path.display().to_string());
        (found.collect(), missing)
    }

    #[tokio::test]
    async fn finds_files_like_python_glob() {
        let dir = workspace();
        let cases: &[(&[&str], &[&str])] = &[
            (&["main.py"], &["main.py"]),
            (&["./main.py"], &["main.py"]),
            (&["*"], &["main.py"]),
            (&["*.py"], &["main.py"]),
            (&["src/"], &["src/a.py", "src/b.txt"]),
            (&["src/*"], &["src/a.py", "src/b.txt"]),
            (&["src/*.py"], &["src/a.py"]),
            (&["src/[ab].*"], &["src/a.py", "src/b.txt"]),
            (&["src/[!a].*"], &["src/b.txt"]),
            (&["src/?.py"], &["src/a.py"]),
            (&["src/**"], &["src/a.py", "src/b.txt", "src/sub/c.py"]),
            (&["src/**/"], &["src/a.py", "src/b.txt", "src/sub/c.py"]),
            (&["src/**/*.py"], &["src/a.py", "src/sub/c.py"]),
            (&["src/**/**/*.py"], &["src/a.py", "src/sub/c.py"]),
            (
                &["**"],
                &["main.py", "src/a.py", "src/b.txt", "src/sub/c.py"],
            ),
            (&["**/*.py"], &["main.py", "src/a.py", "src/sub/c.py"]),
            (&["src/.*"], &["src/.hidden.py"]),
            (&[".config/"], &[".config/x.py"]),
            (&[".*"], &[".top.py"]),
            (&["src/", "!src/a.py"], &["src/b.txt"]),
            (&["!main.py", "main.py"], &["main.py"]),
            (&["**/*.py", "!src/**"], &["main.py"]),
        ];
        for (files, expected) in cases {
            let (found, missing) = find(&dir, files).await;
            let expected: Vec<String> = expected.iter().map(|&file| file.to_owned()).collect();
            assert_eq!(found, expected, "{files:?}");
            assert!(missing.is_empty(), "{files:?}: {missing:?}");
        }
    }

    #[tokio::test]
    async fn reports_patterns_that_match_nothing() {
        let dir = workspace();
        let (found, missing) = find(&dir, &["nope.py", "main.py", "nope/*", "!nope"]).await;
        assert_eq!(found, ["main.py"]);
        assert_eq!(missing, ["nope.py", "nope/*"]);
        // A pattern that matches only directories selects nothing, but is
        // not reported.
        assert_eq!(
            find(&dir, &["src", "empty", "empty/**"]).await,
            (vec![], vec![])
        );
        let (_, missing) = find(&dir, &["empty/", "", "**/*.go"]).await;
        assert_eq!(missing, ["empty/", "", "**/*.go"]);
    }

    #[tokio::test]
    async fn rejects_patterns_that_leave_the_workspace() {
        let dir = workspace();
        for pattern in ["../x", "src/../../x", "/etc/passwd"] {
            let err = find_files(&dir.0, &[pattern.to_owned()]).await.unwrap_err();
            assert!(matches!(err, Error::Invalid { .. }), "{pattern}: {err}");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn does_not_follow_links_to_directories_with_double_stars() {
        let dir = workspace();
        std::os::unix::fs::symlink(&dir.0, dir.0.join("src/loop")).unwrap();
        std::os::unix::fs::symlink(dir.0.join("main.py"), dir.0.join("src/link.py")).unwrap();
        let (found, _) = find(&dir, &["src/**/*.py"]).await;
        assert_eq!(found, ["src/a.py", "src/link.py", "src/sub/c.py"]);
        // A name or a single `*` still goes through the link.
        let (found, _) = find(&dir, &["src/loop/main.py", "src/loop/src/sub/*"]).await;
        assert_eq!(found, ["src/loop/main.py", "src/loop/src/sub/c.py"]);
    }

    #[test]
    fn matches_wildcards_like_fnmatch() {
        let cases = [
            ("*.py", "a.py", true),
            ("*.py", "a.pyc", false),
            ("*", "", true),
            ("a*b*c", "aXbYc", true),
            ("a*b*c", "aXbY", false),
            ("?.py", "ab.py", false),
            ("[a-c]x", "bx", true),
            ("[!a-c]x", "bx", false),
            ("[!a-c]x", "dx", true),
            ("[]]", "]", true),
            ("[!]]", "a", true),
            ("[a-]", "-", true),
            ("[z-a]", "m", false),
            ("[", "[", true),
            ("a[b", "a[b", true),
            ("[^a]", "^", true),
        ];
        for (pattern, name, expected) in cases {
            assert_eq!(fnmatch(pattern, name), expected, "{pattern} {name}");
        }
    }

    #[test]
    fn checks_the_entrypoint_is_among_the_files() {
        let python = Manifest::default();
        assert_eq!(entrypoint(&python), "./main.py");
        let files = [PathBuf::from("main.py"), PathBuf::from("src/solve.py")];
        check_mandatory_files(&python, &files).unwrap();
        let java = Manifest {
            manifest_type: ManifestType::Java,
            ..Default::default()
        };
        assert_eq!(entrypoint(&java), "./main.jar");
        let err = check_mandatory_files(&java, &files).unwrap_err();
        assert!(err.to_string().contains("`main.jar`"), "{err}");
        let custom = Manifest {
            execution: Some(ManifestExecution {
                entrypoint: Some("./src/x/../solve.py".to_owned()),
                cwd: None,
            }),
            ..Default::default()
        };
        assert_eq!(entrypoint(&custom), "./src/x/../solve.py");
        check_mandatory_files(&custom, &files).unwrap();
        assert_eq!(normalized(Path::new("../a/./b/..")), Path::new("../a"));
    }

    fn with_format(format: Option<&[&str]>) -> Manifest {
        let options = ManifestOptions {
            format: format.map(|format| format.iter().map(|&entry| entry.to_owned()).collect()),
            ..Default::default()
        };
        Manifest {
            configuration: ManifestConfiguration {
                options: Some(options),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn options(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|&(name, value)| (name.to_owned(), value.to_owned()))
            .collect()
    }

    #[test]
    fn passes_options_in_the_manifest_format() {
        let options = options(&[("solve.duration", "10s"), ("details", "")]);
        let manifest = with_format(Some(&["--{{name}}={{value}}"]));
        assert_eq!(
            option_args(&manifest, &options),
            ["--details=", "--solve.duration=10s"]
        );
        // As on the platform, entries that end up empty are left out.
        let manifest = with_format(Some(&["-{{name}}", "{{value}}"]));
        assert_eq!(
            option_args(&manifest, &options),
            ["-details", "-solve.duration", "10s"]
        );
        let manifest = with_format(Some(&["{{value}}"]));
        assert_eq!(option_args(&manifest, &options), ["10s"]);
    }

    #[test]
    fn passes_dash_name_and_value_without_a_format() {
        let options = options(&[("b", "2"), ("a", "")]);
        let expected = ["-a", "", "-b", "2"];
        assert_eq!(option_args(&Manifest::default(), &options), expected);
        assert_eq!(option_args(&with_format(None), &options), expected);
        assert_eq!(option_args(&with_format(Some(&[])), &options), expected);
        assert!(option_args(&Manifest::default(), &BTreeMap::new()).is_empty());
    }

    #[test]
    fn expands_the_home_directory() {
        let home = std::env::home_dir().unwrap();
        assert_eq!(workspace_dir(Some("~".into())), home);
        assert_eq!(
            workspace_dir(Some("~/workspaces/a".into())),
            home.join("workspaces/a")
        );
        assert_eq!(
            workspace_dir(Some("~other/a".into())),
            Path::new("~other/a")
        );
        assert_eq!(workspace_dir(Some("a/~".into())), Path::new("a/~"));
        assert_eq!(workspace_dir(None), PathBuf::from("."));
    }

    #[test]
    fn reads_pyproject_dependencies() {
        let pyproject = r#"
[build-system]
requires = ["hatchling"]

[project] # the package
name = "routing"
dependencies-extra = ["not this"]
dependencies = [
    "ortools==9.12.4544",  # pinned, with a ] and a , here
    'pandas>=2',
    "pkg[extra]; python_version < \"3.12\"",
    "caf\u00e9",
]

[project.optional-dependencies]
dev = ["pytest"]
"#;
        let expected = [
            "ortools==9.12.4544",
            "pandas>=2",
            "pkg[extra]; python_version < \"3.12\"",
            "café",
        ];
        assert_eq!(pyproject_dependencies(pyproject).unwrap(), expected);
        assert_eq!(
            pyproject_dependencies("[project]\ndependencies=[\"a\",]").unwrap(),
            ["a"]
        );
        assert!(
            pyproject_dependencies("[ project ]\n  dependencies = []\n")
                .unwrap()
                .is_empty()
        );
        let errors = [
            ("", "not found"),
            ("[tool.x]\ndependencies = [\"a\"]", "not found"),
            ("dependencies = [\"a\"]\n[project]", "not found"),
            ("[project]\ndependencies = \"a\"", "must be a list"),
            ("[project]\ndependencies = [1]", "only strings"),
            ("[project]\ndependencies = [\"a\"", "no closing"),
            (
                "[project]\ndependencies = [\"a\" \"b\"]",
                "not a valid array",
            ),
            ("[project]\ndependencies = [\"a]", "not closed"),
            ("[project]\ndependencies = [\"\"\"a\"\"\"]", "multi-line"),
            ("[project]\ndependencies = [\"\\x\"]", "bad escape"),
            ("[project]\ndependencies = [\"\\u12\"]", "bad `\\u` escape"),
        ];
        for (pyproject, reason) in errors {
            let err = pyproject_dependencies(pyproject).unwrap_err();
            assert!(err.contains(reason), "{pyproject:?}: {err}");
        }
    }
}
