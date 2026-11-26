//! Python dependency bundling for push.
//!
//! `uv pip compile` pins the manifest's `python.pip-requirements` for the
//! workspace's Python version and platform. Each pinned package is
//! installed with `uv pip install --no-deps` into a directory of its own,
//! whose files become one gzip member: a tar of them under
//! `.nextmv/python/deps/`, where the platform's runtime reads them, without
//! the end-of-archive blocks. Members are cached per package
//! ([`crate::cache`]), and push puts them in front of the workspace's own
//! tar.gz, which ends the archive. Gzip members concatenate into one valid
//! gzip stream (RFC 1952), so nothing is compressed twice.

use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;

use flate2::Compression;
use flate2::write::GzEncoder;
use tokio::io::AsyncReadExt;

use super::id::safe_id;
use super::manifest;
#[cfg(windows)]
use super::platform::CREATE_NEW_PROCESS_GROUP;
use super::platform::{KillGroupOnDrop, PushEvent};
use super::{ManifestPipRequirements, ManifestPython, ManifestPythonArch};
use crate::cache::{self, Dependency, Limits};
use crate::tool::AbortOnDrop;
use crate::{Error, Result};

/// Where the platform's runtime reads a workspace's Python dependencies, in
/// its bundle.
const DEPS_DIR: [&str; 3] = [".nextmv", "python", "deps"];

/// The Python version that dependencies are bundled for when the manifest
/// sets none.
const DEFAULT_PYTHON_VERSION: &str = "3.11";

/// The Python version and platform that dependencies are bundled for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Target {
    /// The Python version, e.g. `3.11`.
    pub(crate) python_version: String,
    /// The platform, as `uv` names it, e.g. `aarch64-manylinux_2_34`.
    pub(crate) platform: &'static str,
}

/// What the manifest's `python` settings, `python`, bundle dependencies
/// for: `python.version`, `3.11` when it is unset or empty, and the
/// platform of `python.arch`, `arm64` when it is unset.
///
/// # Errors
///
/// Returns [`Error::Manifest`], naming `manifest_path`, if the version is
/// not 3.10 or later in `major.minor` form.
pub(crate) fn target(python: Option<&ManifestPython>, manifest_path: &Path) -> Result<Target> {
    let version = python
        .and_then(|python| python.version.as_deref())
        .filter(|version| !version.is_empty());
    if let Some(version) = version
        && !bundles_for(version)
    {
        return Err(Error::Manifest {
            path: manifest_path.to_path_buf(),
            reason: format!(
                "`python.version` must be 3.10 or later, in `major.minor` form, to bundle \
                 Python dependencies, not `{version}`"
            ),
        });
    }
    let platform = match python.and_then(|python| python.arch) {
        None | Some(ManifestPythonArch::Arm64) => "aarch64-manylinux_2_34",
        Some(ManifestPythonArch::Amd64) => "x86_64-manylinux_2_34",
    };
    Ok(Target {
        python_version: version.unwrap_or(DEFAULT_PYTHON_VERSION).to_owned(),
        platform,
    })
}

/// Whether `version` is `3.<minor>` with a minor version of 10 or more, in
/// ASCII digits, which may have leading zeros, as Python's `int` reads
/// them.
fn bundles_for(version: &str) -> bool {
    let Some((major, minor)) = version.split_once('.') else {
        return false;
    };
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
    digits(major)
        && digits(minor)
        && major.trim_start_matches('0') == "3"
        && minor.trim_start_matches('0').len() >= 2
}

/// What `uv pip compile --emit-index-url --emit-find-links` resolved: the
/// pinned packages, the other requirements, and where it found them.
#[derive(Debug, Default, PartialEq, Eq)]
struct Lockfile {
    /// The pinned packages, as `(name, version)`.
    packages: Vec<(String, String)>,
    /// The other requirements, such as local paths, URLs and VCS
    /// references.
    direct: Vec<String>,
    /// The package index, from `--index-url`, if the lockfile names one.
    index_url: Option<String>,
    /// The extra package indexes, from `--extra-index-url`.
    extra_index_urls: Vec<String>,
    /// The other locations of packages, from `--find-links`.
    find_links: Vec<String>,
}

/// `lockfile`, the output of `uv pip compile`: its pinned packages, its
/// other requirements and its index options. Only lines that start with
/// neither whitespace nor `#` count, so comments and the indented `# via`
/// lines are skipped. Lines that start with `-`, other than an editable
/// requirement, are options, not requirements: the index options are read,
/// and the others, such as `--no-binary`, are skipped.
fn parse_lockfile(lockfile: &str) -> Lockfile {
    let mut parsed = Lockfile::default();
    for line in lockfile.lines() {
        if line.is_empty() || line.starts_with([' ', '\t', '#']) {
            continue;
        }
        let line = line.trim();
        if let Some((option, value)) = index_option(line) {
            let value = value.to_owned();
            match option {
                "--index-url" | "-i" => parsed.index_url = Some(value),
                "--extra-index-url" => parsed.extra_index_urls.push(value),
                _ => parsed.find_links.push(value),
            }
            continue;
        }
        match pin(line) {
            Some((name, version)) => {
                parsed.packages.push((name.to_owned(), version.to_owned()));
            }
            None if line.starts_with('-') && !is_editable(line) => {}
            None => parsed.direct.push(line.to_owned()),
        }
    }
    parsed
}

/// The index option `line` sets, as `(option, value)`, for an
/// `--index-url`, `-i`, `--extra-index-url`, `--find-links` or `-f` line, in
/// the `--option value` and `--option=value` forms.
fn index_option(line: &str) -> Option<(&str, &str)> {
    let (option, value) = line
        .split_once(['=', ' ', '\t'])
        .map(|(option, value)| (option, value.trim()))?;
    let known = matches!(
        option,
        "--index-url" | "-i" | "--extra-index-url" | "--find-links" | "-f"
    );
    (known && !value.is_empty()).then_some((option, value))
}

/// Whether `line` is an editable requirement, `-e <path>` or
/// `--editable <path>`.
fn is_editable(line: &str) -> bool {
    line.starts_with("-e ") || line.starts_with("--editable ")
}

/// The name and version that `line` pins as `name==version`, as the
/// pattern `^([A-Za-z0-9][A-Za-z0-9._-]*)==([^\s;#\[]+)` matches them, or
/// `None`.
fn pin(line: &str) -> Option<(&str, &str)> {
    if !line.starts_with(|c: char| c.is_ascii_alphanumeric()) {
        return None;
    }
    let name_end = line
        .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')))
        .unwrap_or(line.len());
    let rest = line[name_end..].strip_prefix("==")?;
    let version_end = rest
        .find(|c: char| c.is_whitespace() || matches!(c, ';' | '#' | '['))
        .unwrap_or(rest.len());
    (version_end > 0).then(|| (&line[..name_end], &rest[..version_end]))
}

/// The `uv` argument that installs the lockfile requirement `spec`. An
/// editable one, `-e <path>` or `--editable <path>`, is installed from its
/// path as a normal package: an editable install is only a `.pth` file
/// that points at this machine's copy, so the bundle would hold no code.
/// An editable target is always a path or a URL, but to `uv pip install` a
/// target that looks like a package name, such as `local` or
/// `local[extra]`, is the PyPI package of that name, so it gets a leading
/// `./`. Every other target, e.g. `sub/local`, `file:./local` or
/// `${ROOT}/local`, stays as it is.
fn spec_arg(spec: &str) -> String {
    let spec = spec.trim();
    let Some(target) = is_editable(spec)
        .then(|| spec.split_once(' '))
        .flatten()
        .map(|(_, target)| target.trim())
    else {
        return spec.to_owned();
    };
    // A PEP 508 name, with or without extras.
    let name = target.split_once('[').map_or(target, |(name, _)| name);
    let is_name = name.starts_with(|c: char| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    match is_name {
        true => format!("./{target}"),
        false => target.to_owned(),
    }
}

/// The program that runs `uv`: `program`, then `args`, then the `uv`
/// arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Uv {
    /// The program, found on the `PATH` if it is a bare name.
    pub(crate) program: OsString,
    /// Arguments before the `uv` arguments.
    pub(crate) args: Vec<OsString>,
}

impl Uv {
    /// The `uv` binary at `program`, with no arguments before the `uv`
    /// ones: the SDK's own ([`crate::uv`]), or the one the host set.
    pub(crate) fn binary(program: OsString) -> Uv {
        Uv {
            program,
            args: Vec::new(),
        }
    }
}

/// Bundles a workspace's Python dependencies.
#[derive(Debug, Clone)]
pub(crate) struct Bundler {
    /// The program that runs `uv`.
    pub(crate) uv: Uv,
    /// The FICO home whose dependency cache to use, and its limits, or
    /// `None` to bundle without the cache.
    pub(crate) cache: Option<(PathBuf, Limits)>,
    /// The directory to create temporary files and directories in.
    pub(crate) temp_root: PathBuf,
}

impl Bundler {
    /// The dependencies of the workspace in `src` that `requirements`, its
    /// manifest's `python.pip-requirements`, lists, installed for `target`,
    /// as gzip members to put in front of the workspace's tar.gz: the
    /// cached packages, then the ones it installed, then the other
    /// requirements, in lockfile order.
    ///
    /// Every `uv` runs in `src`, with `UV_SYSTEM_CERTS=true` unless it is
    /// set, its stdin empty and its output captured, in a new process
    /// group that is killed if the future is dropped. Pinned packages are
    /// installed from wheels only, and stored in the cache; other
    /// requirements are built from source if needed, and never cached,
    /// since a local tree changes without a new version. `emit` gets the
    /// events `DependenciesCached` (if any package is cached),
    /// `DependenciesDownloading` (if any is not) and
    /// `DirectRequirementsBuilding` (if there are other requirements).
    /// Temporary files are removed, also when it fails.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Manifest`], naming `manifest_path`, if
    /// `requirements` names a file that is not in `src`, [`Error::Invalid`]
    /// if a `pyproject.toml` has no list of dependencies,
    /// [`Error::Command`] if `uv` fails, and [`Error::Io`] if `uv` cannot
    /// start or a file cannot be read or written, the cache's included.
    pub(crate) async fn bundle(
        &self,
        src: &Path,
        manifest_path: &Path,
        requirements: &ManifestPipRequirements,
        target: &Target,
        emit: impl Fn(PushEvent),
    ) -> Result<Vec<u8>> {
        let temp = self.temp_root.join(safe_id("fico-bundle"));
        // As Python's `mkdtemp` does: only this user can read the
        // requirements, which may hold an index's credentials, and the
        // packages, even in a shared `/tmp`.
        let mut dir = tokio::fs::DirBuilder::new();
        #[cfg(unix)]
        dir.mode(0o700);
        dir.create(&temp).await.map_err(io_error(&temp))?;
        let _removed = RemoveOnDrop(temp.clone());
        let requirements = requirements_file(requirements, src, manifest_path, &temp).await?;
        let mut compile: Vec<OsString> = vec!["pip".into(), "compile".into(), requirements.into()];
        // So that the installs use the indexes that resolved the packages.
        compile.extend(["--emit-index-url".into(), "--emit-find-links".into()]);
        compile.extend(target_args(target));
        let lockfile = self.run(src, compile, &[]).await?;
        let lockfile = parse_lockfile(&String::from_utf8_lossy(&lockfile));
        let index = Index {
            env: index_env(&lockfile),
            args: index_args(&lockfile),
        };
        let sources = index_sources(&lockfile);
        let cache = match has_local_find_links(&lockfile) {
            true => None,
            false => self.cache.as_ref(),
        };
        let (packages, direct) = (lockfile.packages, lockfile.direct);

        let mut bundle = Vec::new();
        let mut missing = Vec::new();
        for (name, version) in packages.iter().cloned() {
            let dependency = Dependency {
                name,
                version,
                python_version: target.python_version.clone(),
                platform: target.platform.to_owned(),
                sources: sources.clone(),
            };
            let cached = match cache {
                Some((home, _)) => cache::lookup(home, &dependency).await,
                None => None,
            };
            match cached {
                Some(member) => bundle.extend(member),
                None => missing.push(dependency),
            }
        }
        let total = packages.len() as u64;
        let cached = total - missing.len() as u64;
        if cached > 0 {
            emit(PushEvent::DependenciesCached { cached, total });
        }
        if !missing.is_empty() {
            let count = missing.len() as u64;
            emit(PushEvent::DependenciesDownloading { count });
        }
        let mut installs = 0;
        for dependency in &missing {
            let spec = format!("{}=={}", dependency.name, dependency.version);
            let member = self
                .install(src, &temp, &mut installs, spec, target, true, &index)
                .await?;
            if let Some((home, limits)) = cache {
                cache::store(home, dependency, &member, *limits).await?;
            }
            bundle.extend(member);
        }
        if !direct.is_empty() {
            let count = direct.len() as u64;
            emit(PushEvent::DirectRequirementsBuilding { count });
        }
        for spec in &direct {
            let member = self
                .install(
                    src,
                    &temp,
                    &mut installs,
                    spec_arg(spec),
                    target,
                    false,
                    &index,
                )
                .await?;
            bundle.extend(member);
        }
        Ok(bundle)
    }

    /// Installs `spec`, without its dependencies, for `target`, from the
    /// indexes that `index` sets, into a new directory in `temp`, numbered
    /// with `installs`, and returns its files as a gzip member. With
    /// `only_binary`, it installs from wheels only.
    #[allow(clippy::too_many_arguments)]
    async fn install(
        &self,
        src: &Path,
        temp: &Path,
        installs: &mut u32,
        spec: String,
        target: &Target,
        only_binary: bool,
        index: &Index,
    ) -> Result<Vec<u8>> {
        *installs += 1;
        let dir = temp.join(format!("install-{installs}"));
        tokio::fs::create_dir(&dir).await.map_err(io_error(&dir))?;
        let mut args: Vec<OsString> = vec!["pip".into(), "install".into(), spec.into()];
        args.push("--no-deps".into());
        if only_binary {
            args.push("--only-binary=:all:".into());
        }
        args.extend(index.args.iter().cloned());
        args.extend(["--target".into(), dir.clone().into_os_string()]);
        args.extend(target_args(target));
        self.run(src, args, &index.env).await?;
        let installed = dir.clone();
        let member = blocking(dir.clone(), move || compress(&installed)).await?;
        let _ = tokio::fs::remove_dir_all(&dir).await;
        Ok(member)
    }

    /// Runs `uv` with `args` and the environment variables `env` in `src`,
    /// and returns its stdout.
    async fn run(
        &self,
        src: &Path,
        args: Vec<OsString>,
        env: &[(&'static str, String)],
    ) -> Result<Vec<u8>> {
        let mut command = tokio::process::Command::new(&self.uv.program);
        command
            .args(&self.uv.args)
            .args(args)
            .envs(env.iter().map(|(name, value)| (name, value)))
            .current_dir(src)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        // So that `uv` trusts the system's certificates.
        if std::env::var_os("UV_SYSTEM_CERTS").is_none() {
            command.env("UV_SYSTEM_CERTS", "true");
        }
        #[cfg(unix)]
        command.process_group(0);
        #[cfg(windows)]
        command.creation_flags(CREATE_NEW_PROCESS_GROUP);
        let program = PathBuf::from(&self.uv.program);
        let io_error = |source| Error::Io {
            path: program.clone(),
            source,
        };
        let mut child = command.spawn().map_err(io_error)?;
        // `kill_on_drop` only kills `uv`, not what it starts, such as a
        // build backend. Declared after `child`, the guard drops first, so
        // it finds the group or tree while `uv` still runs.
        let mut group = KillGroupOnDrop(child.id());
        // stderr is read in a task of its own, so that `uv` never blocks on
        // a full pipe.
        let stderr = child.stderr.take().map(|mut pipe| {
            AbortOnDrop(tokio::spawn(async move {
                let mut stderr = Vec::new();
                pipe.read_to_end(&mut stderr).await.map(|_| stderr)
            }))
        });
        let mut stdout = Vec::new();
        if let Some(mut pipe) = child.stdout.take() {
            pipe.read_to_end(&mut stdout).await.map_err(io_error)?;
        }
        let stderr = match stderr {
            Some(task) => match task.await {
                Ok(read) => read.map_err(io_error)?,
                Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
                Err(error) => return Err(io_error(io::Error::other(error))),
            },
            None => Vec::new(),
        };
        let status = child.wait().await.map_err(io_error)?;
        group.0 = None;
        if status.success() {
            return Ok(stdout);
        }
        Err(Error::Command {
            program: "uv".to_owned(),
            status: status.code(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
        })
    }
}

/// Where the installs take packages from: the sources of the lockfile.
#[derive(Debug, Default)]
struct Index {
    /// The environment variables, [`index_env`].
    env: Vec<(&'static str, String)>,
    /// The arguments, [`index_args`].
    args: Vec<OsString>,
}

/// The environment variables that make `uv pip install` use the indexes of
/// `lockfile`: `UV_INDEX_URL`, `UV_EXTRA_INDEX_URL` and `UV_FIND_LINKS`.
/// Variables, not arguments, so that an index's credentials don't show in
/// the process list, and relative `--find-links` paths resolve against
/// `src`, where `uv` runs, as in the requirements; `uv` rejects them in a
/// `-r` file of its own.
fn index_env(lockfile: &Lockfile) -> Vec<(&'static str, String)> {
    let mut env = Vec::new();
    if let Some(url) = &lockfile.index_url {
        env.push(("UV_INDEX_URL", url.clone()));
    }
    if !lockfile.extra_index_urls.is_empty() {
        env.push(("UV_EXTRA_INDEX_URL", lockfile.extra_index_urls.join(" ")));
    }
    // `uv` reads `UV_FIND_LINKS` as a comma-separated list, unlike
    // `UV_EXTRA_INDEX_URL`.
    if !lockfile.find_links.is_empty() {
        env.push(("UV_FIND_LINKS", lockfile.find_links.join(",")));
    }
    env
}

/// The `uv pip install` arguments that keep the installs to the sources of
/// `lockfile`: `--no-index` when it names no `--index-url`, which `uv pip
/// compile --emit-index-url` writes unless the requirements set
/// `--no-index`, and which `uv` doesn't read from `UV_NO_INDEX`. Without
/// it, an install would also search PyPI, and could take PyPI's package of
/// a private package's name.
fn index_args(lockfile: &Lockfile) -> Vec<OsString> {
    match lockfile.index_url {
        Some(_) => Vec::new(),
        None => vec!["--no-index".into()],
    }
}

/// Whether `lockfile` takes packages from a local directory, a
/// `--find-links` that is not an `http` or `https` URL. A wheel there can
/// change without a new version, so its packages are never cached, like
/// direct requirements. The lockfile doesn't say which packages came from
/// it, so none of its packages are.
fn has_local_find_links(lockfile: &Lockfile) -> bool {
    lockfile.find_links.iter().any(|location| {
        let lower = location.to_ascii_lowercase();
        !(lower.starts_with("http://") || lower.starts_with("https://"))
    })
}

/// Where the packages of `lockfile` come from, for their cache keys: its
/// index options, without credentials, or `None` for PyPI alone, so that a
/// package from another index never shares an entry with PyPI's.
fn index_sources(lockfile: &Lockfile) -> Option<String> {
    let index_url = lockfile
        .index_url
        .as_deref()
        .filter(|url| without_credentials(url) != PYPI_SIMPLE);
    let mut sources: Vec<String> = Vec::new();
    if let Some(url) = index_url {
        sources.push(format!("--index-url {}", without_credentials(url)));
    }
    for url in &lockfile.extra_index_urls {
        sources.push(format!("--extra-index-url {}", without_credentials(url)));
    }
    for location in &lockfile.find_links {
        sources.push(format!("--find-links {}", without_credentials(location)));
    }
    (!sources.is_empty()).then(|| sources.join(" "))
}

/// PyPI's index, which `uv pip compile` writes as the `--index-url` when
/// the requirements set none.
const PYPI_SIMPLE: &str = "https://pypi.org/simple";

/// `url` without the `user:password@` of its authority, if it has one.
fn without_credentials(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_owned();
    };
    let authority_end = rest.find('/').unwrap_or(rest.len());
    match rest[..authority_end].rfind('@') {
        Some(at) => format!("{scheme}://{}", &rest[at + 1..]),
        None => url.to_owned(),
    }
}

/// The `uv` arguments that target `target`, after `--quiet`.
fn target_args(target: &Target) -> [OsString; 3] {
    [
        "--quiet".into(),
        format!("--python-platform={}", target.platform).into(),
        format!("--python-version={}", target.python_version).into(),
    ]
}

/// The requirements file for `uv pip compile`: a list of packages, or the
/// `[project].dependencies` of a `pyproject.toml`, written one per line to
/// a file in `temp`, or else the file that `requirements` names, relative
/// to `src`, as an absolute path.
///
/// # Errors
///
/// Returns [`Error::Manifest`], naming `manifest_path`, if the file is not
/// in `src`, [`Error::Invalid`] if a `pyproject.toml` has no list of
/// dependencies, and [`Error::Io`] if a file cannot be read or written.
async fn requirements_file(
    requirements: &ManifestPipRequirements,
    src: &Path,
    manifest_path: &Path,
    temp: &Path,
) -> Result<PathBuf> {
    let packages = match requirements {
        ManifestPipRequirements::Packages(packages) => packages.clone(),
        ManifestPipRequirements::File(file) => {
            let file = file.trim();
            let path = src.join(file);
            let is_file = tokio::fs::metadata(&path)
                .await
                .is_ok_and(|meta| meta.is_file());
            if !is_file {
                return Err(Error::Manifest {
                    path: manifest_path.to_path_buf(),
                    reason: format!(
                        "`python.pip-requirements` names `{file}`, which is not a file in the \
                         workspace"
                    ),
                });
            }
            if Path::new(file).file_name() != Some("pyproject.toml".as_ref()) {
                // `uv` runs in `src`, so a relative path would not resolve.
                return std::path::absolute(&path).map_err(io_error(&path));
            }
            let pyproject = tokio::fs::read_to_string(&path)
                .await
                .map_err(io_error(&path))?;
            manifest::pyproject_dependencies(&pyproject).map_err(|reason| Error::Invalid {
                reason: format!("{reason} in `{}`", path.display()),
            })?
        }
    };
    let file = temp.join("requirements.txt");
    let lines: String = packages
        .iter()
        .map(|package| format!("{package}\n"))
        .collect();
    tokio::fs::write(&file, lines)
        .await
        .map_err(io_error(&file))?;
    Ok(file)
}

/// The files under `dir`, an installed package, as a gzip member: a tar of
/// `dir` as [`DEPS_DIR`], without the end-of-archive blocks. Directories
/// are stored too, in name order. A link to a file is stored as a file with
/// the target's contents, and other links and special files are left out,
/// so that the bundle only holds what an unpack accepts.
fn compress(dir: &Path) -> Result<Vec<u8>> {
    let gzip = GzEncoder::new(Vec::new(), Compression::default());
    let mut builder = tar::Builder::new(WithoutEnd {
        inner: gzip,
        ended: false,
    });
    builder.sparse(false);
    let name: PathBuf = DEPS_DIR.iter().collect();
    builder.append_dir(&name, dir).map_err(io_error(dir))?;
    append_tree(&mut builder, dir, &name)?;
    // Everything after this is the end-of-archive blocks.
    builder.get_mut().ended = true;
    builder
        .into_inner()
        .and_then(|writer| writer.inner.finish())
        .map_err(io_error(dir))
}

/// Appends what is under `dir` to `builder`, as paths under `name`, in name
/// order. Only real directories are walked into, so links can't make it
/// loop.
fn append_tree(builder: &mut tar::Builder<impl Write>, dir: &Path, name: &Path) -> Result<()> {
    let mut entries = fs::read_dir(dir)
        .and_then(|entries| entries.collect::<io::Result<Vec<_>>>())
        .map_err(io_error(dir))?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let entry_name = name.join(entry.file_name());
        let kind = entry.file_type().map_err(io_error(&path))?;
        if kind.is_dir() {
            builder
                .append_dir(&entry_name, &path)
                .map_err(io_error(&path))?;
            append_tree(builder, &path, &entry_name)?;
        } else if kind.is_file() || fs::metadata(&path).is_ok_and(|meta| meta.is_file()) {
            File::open(&path)
                .and_then(|mut file| builder.append_file(&entry_name, &mut file))
                .map_err(io_error(&path))?;
        }
    }
    Ok(())
}

/// A writer that passes what is written to `inner` until `ended` is set,
/// and drops it after that: the end-of-archive blocks that a
/// [`tar::Builder`] writes when it finishes.
struct WithoutEnd<W> {
    /// Where the tar entries go.
    inner: W,
    /// Whether the entries are all written.
    ended: bool,
}

impl<W: Write> Write for WithoutEnd<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.ended {
            return Ok(buf.len());
        }
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// A directory that is removed, with everything in it, when dropped.
struct RemoveOnDrop(PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
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
pub(crate) mod tests {
    use std::io::Read;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU32, Ordering};

    use flate2::read::GzDecoder;

    use super::*;
    use crate::decision::archive;

    /// A new directory under the system's temp directory, removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> TempDir {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("fico-sdk-bundle-{}-{n}", std::process::id()));
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

    /// A shell script that stands in for `uv`, with its directory as `$0`.
    /// It appends its working directory, its `UV_SYSTEM_CERTS` and its
    /// arguments to `calls` there. `pip compile` copies the requirements
    /// file to `requirements`, writes the mode of the file's directory to
    /// `mode` if `record-mode` exists, and prints `lockfile`, or fails if
    /// `compile-fails` exists. `pip install` writes the requirement into
    /// `<target>/<requirement, as a name>/__init__.py`, or fails for a
    /// requirement with `broken` in it.
    const FAKE_UV: &str = r#"
dir=$0
{
    printf 'cwd=%s\n' "$(pwd -P)"
    printf 'certs=%s\n' "${UV_SYSTEM_CERTS-unset}"
    printf 'index=%s|%s|%s\n' "${UV_INDEX_URL-}" "${UV_EXTRA_INDEX_URL-}" "${UV_FIND_LINKS-}"
    for arg in "$@"; do printf 'arg=%s\n' "$arg"; done
    printf 'end\n'
} >> "$dir/calls"
case "$2" in
compile)
    if [ -e "$dir/record-mode" ]; then ls -ld "$(dirname "$3")" | cut -c1-10 > "$dir/mode"; fi
    cat "$3" > "$dir/requirements"
    if [ -e "$dir/compile-fails" ]; then echo 'resolution failed' >&2; exit 1; fi
    cat "$dir/lockfile"
    ;;
install)
    target=
    previous=
    for arg in "$@"; do
        if [ "$previous" = --target ]; then target=$arg; fi
        previous=$arg
    done
    spec=$3
    case "$spec" in *broken*) echo "no wheel for $spec" >&2; exit 2 ;; esac
    name=$(printf '%s' "$spec" | tr -c 'A-Za-z0-9' _)
    mkdir -p "$target/$name"
    printf '%s' "$spec" > "$target/$name/__init__.py"
    ;;
esac
"#;

    /// The fake `uv` of [`FAKE_UV`], working in `dir`, which holds its
    /// `lockfile`.
    pub(crate) fn fake_uv(dir: &Path, lockfile: &str) -> Uv {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join("lockfile"), lockfile).unwrap();
        Uv {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), FAKE_UV.into(), dir.into()],
        }
    }

    /// The fake `uv` of [`FAKE_UV`] as one executable script, `uv` in
    /// `dir`, which holds its `lockfile`: a `uv` binary as the SDK installs
    /// it, or as `with_uv` takes it. Its contents, for an archive.
    #[cfg(unix)]
    pub(crate) fn fake_uv_script(dir: &Path, lockfile: &str) -> (PathBuf, Vec<u8>) {
        use std::os::unix::fs::PermissionsExt;

        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join("lockfile"), lockfile).unwrap();
        // `$0` is the script's path, so its directory is `dir`.
        let body = FAKE_UV.replacen("dir=$0", "dir=$(dirname \"$0\")", 1);
        let script = format!("#!/bin/sh\n{body}").into_bytes();
        let path = dir.join("uv");
        fs::write(&path, &script).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        (path, script)
    }

    /// One run of the fake `uv`.
    #[derive(Debug, PartialEq)]
    struct Call {
        /// Its working directory, with links resolved.
        cwd: PathBuf,
        /// Its `UV_SYSTEM_CERTS`, or `unset`.
        certs: String,
        /// Its `UV_INDEX_URL`, `UV_EXTRA_INDEX_URL` and `UV_FIND_LINKS`,
        /// joined by `|`.
        index: String,
        /// Its arguments.
        args: Vec<String>,
    }

    /// The runs of the fake `uv` in `dir`, and forgets them.
    fn calls(dir: &Path) -> Vec<Call> {
        let Ok(text) = fs::read_to_string(dir.join("calls")) else {
            return Vec::new();
        };
        fs::remove_file(dir.join("calls")).unwrap();
        let mut calls = Vec::new();
        let mut call = Call {
            cwd: PathBuf::new(),
            certs: String::new(),
            index: String::new(),
            args: Vec::new(),
        };
        for line in text.lines() {
            if let Some(cwd) = line.strip_prefix("cwd=") {
                call.cwd = cwd.into();
            } else if let Some(certs) = line.strip_prefix("certs=") {
                call.certs = certs.to_owned();
            } else if let Some(index) = line.strip_prefix("index=") {
                call.index = index.to_owned();
            } else if let Some(arg) = line.strip_prefix("arg=") {
                call.args.push(arg.to_owned());
            } else {
                let next = Call {
                    cwd: PathBuf::new(),
                    certs: String::new(),
                    index: String::new(),
                    args: Vec::new(),
                };
                calls.push(std::mem::replace(&mut call, next));
            }
        }
        calls
    }

    /// `args`, with the temporary paths under `temp_root` that follow
    /// `--target` and `compile` replaced by `<target>` and
    /// `<requirements>`.
    fn placeholders(args: &[String], temp_root: &Path) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for arg in args {
            let previous = out.last().map(String::as_str);
            let placeholder = match previous {
                Some("--target") => "<target>",
                Some("compile") => "<requirements>",
                _ => {
                    out.push(arg.clone());
                    continue;
                }
            };
            assert!(Path::new(arg).starts_with(temp_root), "{arg}");
            out.push(placeholder.to_owned());
        }
        out
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    /// The `UV_SYSTEM_CERTS` that `uv` gets: this process's, else `true`.
    fn expected_certs() -> String {
        std::env::var("UV_SYSTEM_CERTS").unwrap_or_else(|_| "true".to_owned())
    }

    fn amd64_312() -> Target {
        Target {
            python_version: "3.12".to_owned(),
            platform: "x86_64-manylinux_2_34",
        }
    }

    /// The names in `dir`.
    fn listing(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn python(version: Option<&str>, arch: Option<ManifestPythonArch>) -> ManifestPython {
        ManifestPython {
            version: version.map(str::to_owned),
            arch,
            ..Default::default()
        }
    }

    #[test]
    fn targets_the_manifests_python_version_and_architecture() {
        let path = Path::new("workspace.yaml");
        let default = target(None, path).unwrap();
        assert_eq!(default.python_version, "3.11");
        assert_eq!(default.platform, "aarch64-manylinux_2_34");
        let empty = target(Some(&python(Some(""), None)), path).unwrap();
        assert_eq!(empty.python_version, "3.11");

        let amd64 = Some(ManifestPythonArch::Amd64);
        assert_eq!(
            target(Some(&python(Some("3.12"), amd64)), path).unwrap(),
            amd64_312()
        );
        let arm64 = target(Some(&python(None, Some(ManifestPythonArch::Arm64))), path).unwrap();
        assert_eq!(arm64.platform, "aarch64-manylinux_2_34");

        // Python's `int` reads leading zeros.
        for version in ["3.10", "3.13", "3.100", "03.010"] {
            let target = target(Some(&python(Some(version), None)), path).unwrap();
            assert_eq!(target.python_version, version);
        }
        for version in [
            "3.9", "3", "3.09", "3.00", "4.10", "2.10", "3.10.1", "3.x", "3.", ".10",
        ] {
            let err = target(Some(&python(Some(version), None)), path).unwrap_err();
            assert!(
                matches!(&err, Error::Manifest { path: p, .. } if p == path),
                "{err:?}"
            );
            assert!(
                err.to_string().contains("3.10 or later"),
                "{version}: {err}"
            );
        }
    }

    #[test]
    fn parses_the_pinned_packages_and_other_requirements() {
        let lockfile = "\
# This file was autogenerated by uv via the following command:
#    uv pip compile requirements.txt --quiet
annotated-types==0.7.0
    # via pydantic
numpy==2.1.0 ; python_version >= \"3.10\"
Pydantic_Core==2.23.4;python_version<\"4\"
torch==2.4.0+cpu
zope.interface==7.1.1  # pinned
\t# via tab
pkg[extra]==1.0
weird==
./local/pkg
mylib @ file:///abs/path
git+https://github.com/x/y.git@abc
-e ../shared   

";
        let Lockfile {
            packages, direct, ..
        } = parse_lockfile(lockfile);
        let pairs: Vec<(&str, &str)> = packages
            .iter()
            .map(|(name, version)| (name.as_str(), version.as_str()))
            .collect();
        assert_eq!(
            pairs,
            [
                ("annotated-types", "0.7.0"),
                ("numpy", "2.1.0"),
                ("Pydantic_Core", "2.23.4"),
                ("torch", "2.4.0+cpu"),
                ("zope.interface", "7.1.1"),
            ]
        );
        assert_eq!(
            direct,
            [
                "pkg[extra]==1.0",
                "weird==",
                "./local/pkg",
                "mylib @ file:///abs/path",
                "git+https://github.com/x/y.git@abc",
                "-e ../shared",
            ]
        );
        assert_eq!(parse_lockfile(""), Lockfile::default());
    }

    /// A private index URL with credentials, built from parts, so that
    /// secret scanners don't read it as a credential.
    fn private_index() -> String {
        format!(
            "https://{}@pkgs.example.com/simple",
            ["user", "token"].join(":")
        )
    }

    #[test]
    fn reads_the_index_options_of_a_lockfile() {
        // As `uv pip compile --emit-index-url --emit-find-links` writes
        // them, plus the `=` and short forms.
        let lockfile = format!(
            "\
# This file was autogenerated by uv.
--index-url {}
--extra-index-url https://download.pytorch.org/whl/cpu
--extra-index-url=https://mirror.example.com/simple
--find-links ./wheels
-f /opt/wheels
--no-binary :none:

torch==2.4.0+cpu
-e ./local
",
            private_index()
        );
        assert_eq!(
            parse_lockfile(&lockfile),
            Lockfile {
                packages: vec![("torch".to_owned(), "2.4.0+cpu".to_owned())],
                direct: strings(&["-e ./local"]),
                index_url: Some(private_index()),
                extra_index_urls: strings(&[
                    "https://download.pytorch.org/whl/cpu",
                    "https://mirror.example.com/simple",
                ]),
                find_links: strings(&["./wheels", "/opt/wheels"]),
            }
        );
        let short = parse_lockfile("-i https://pypi.org/simple\nsix==1.17.0\n");
        assert_eq!(short.index_url.as_deref(), Some("https://pypi.org/simple"));
    }

    #[test]
    fn passes_the_indexes_to_uv_without_credentials_in_the_cache_key() {
        let lockfile = parse_lockfile(&format!(
            "--index-url {}\n\
             --extra-index-url https://a.example.com/simple\n\
             --extra-index-url https://b.example.com/simple\n\
             --find-links ./wheels\n",
            private_index()
        ));
        assert_eq!(
            index_env(&lockfile),
            [
                ("UV_INDEX_URL", private_index()),
                (
                    "UV_EXTRA_INDEX_URL",
                    "https://a.example.com/simple https://b.example.com/simple".to_owned()
                ),
                ("UV_FIND_LINKS", "./wheels".to_owned()),
            ]
        );
        assert_eq!(
            index_sources(&lockfile).as_deref(),
            Some(
                "--index-url https://pkgs.example.com/simple \
                 --extra-index-url https://a.example.com/simple \
                 --extra-index-url https://b.example.com/simple --find-links ./wheels"
            )
        );
        // PyPI alone, as `uv pip compile` writes it by default, adds no
        // sources to the key.
        let pypi = parse_lockfile("--index-url https://pypi.org/simple\nsix==1.17.0\n");
        assert_eq!(index_sources(&pypi), None);
        assert_eq!(index_sources(&Lockfile::default()), None);
        assert!(index_env(&Lockfile::default()).is_empty());
        let with_at = format!("https://{}@h.example.com/x@y", ["u", "p"].join(":"));
        for (url, expected) in [
            (with_at.as_str(), "https://h.example.com/x@y"),
            (
                "https://h.example.com/simple",
                "https://h.example.com/simple",
            ),
            ("./wheels", "./wheels"),
        ] {
            assert_eq!(without_credentials(url), expected, "{url}");
        }
    }

    #[test]
    fn keeps_the_installs_to_the_lockfiles_sources() {
        // `uv` reads `UV_FIND_LINKS` as comma-separated.
        let two = parse_lockfile(
            "--index-url https://pypi.org/simple\n--find-links ./a\n--find-links ./b\n",
        );
        assert!(index_env(&two).contains(&("UV_FIND_LINKS", "./a,./b".to_owned())));
        // With an index, PyPI's by default, the installs use it.
        assert!(index_args(&two).is_empty());
        // Without one, the requirements set `--no-index`, so the installs do
        // too.
        let no_index = parse_lockfile("--find-links ./a\n\nsix==1.17.0\n");
        assert_eq!(index_args(&no_index), [OsString::from("--no-index")]);
        // A local directory's packages are never cached; an URL's are.
        assert!(has_local_find_links(&two));
        assert!(has_local_find_links(&parse_lockfile("-f /opt/wheels\n")));
        let remote = parse_lockfile("--find-links HTTPS://wheels.example.com/\n");
        assert!(!has_local_find_links(&remote));
        assert!(!has_local_find_links(&Lockfile::default()));
    }

    #[test]
    fn installs_editable_requirements_from_their_path() {
        assert_eq!(spec_arg("-e ../shared"), "../shared");
        assert_eq!(spec_arg(" --editable  ./pkg "), "./pkg");
        // A bare name would be the PyPI package of that name.
        assert_eq!(spec_arg("-e local"), "./local");
        assert_eq!(spec_arg("--editable local[extra]"), "./local[extra]");
        assert_eq!(spec_arg("-e my.pkg_2-x"), "./my.pkg_2-x");
        // Other targets are paths or URLs as they are.
        assert_eq!(spec_arg("-e sub/local"), "sub/local");
        assert_eq!(spec_arg("-e ."), ".");
        assert_eq!(spec_arg("-e file:./local"), "file:./local");
        assert_eq!(spec_arg("-e ${ROOT}/local"), "${ROOT}/local");
        assert_eq!(spec_arg("-e /abs/local"), "/abs/local");
        assert_eq!(
            spec_arg("-e git+https://h.example.com/r.git"),
            "git+https://h.example.com/r.git"
        );
        assert_eq!(spec_arg("./local"), "./local");
        assert_eq!(
            spec_arg("mylib @ file:///abs/path"),
            "mylib @ file:///abs/path"
        );
    }

    #[tokio::test]
    async fn writes_the_requirements_file() {
        let root = TempDir::new();
        let (src, temp) = (root.0.join("src"), root.0.join("temp"));
        fs::create_dir_all(src.join("sub")).unwrap();
        fs::create_dir(&temp).unwrap();
        let manifest_path = src.join("workspace.yaml");
        let read = |file: &str| {
            let requirements = ManifestPipRequirements::File(file.to_owned());
            let (src, manifest_path, temp) = (src.clone(), manifest_path.clone(), temp.clone());
            async move { requirements_file(&requirements, &src, &manifest_path, &temp).await }
        };

        let list = ManifestPipRequirements::Packages(strings(&["ortools==9.12.4544", "pyyaml"]));
        let file = requirements_file(&list, &src, &manifest_path, &temp)
            .await
            .unwrap();
        assert!(file.starts_with(root.0.join("temp")), "{file:?}");
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            "ortools==9.12.4544\npyyaml\n"
        );

        // Any other file is used where it is.
        fs::write(root.0.join("src/sub/reqs.txt"), "numpy\n").unwrap();
        let file = read(" sub/reqs.txt ").await.unwrap();
        assert!(file.is_absolute(), "{file:?}");
        assert_eq!(
            file,
            std::path::absolute(root.0.join("src/sub/reqs.txt")).unwrap()
        );

        fs::write(
            root.0.join("src/sub/pyproject.toml"),
            "[project]\ndependencies = [\"numpy>=2\", 'pandas']\n",
        )
        .unwrap();
        let file = read("sub/pyproject.toml").await.unwrap();
        assert!(file.starts_with(root.0.join("temp")), "{file:?}");
        assert_eq!(fs::read_to_string(&file).unwrap(), "numpy>=2\npandas\n");

        for pyproject in [
            "[project]\nname = \"x\"\n",
            "[project]\ndependencies = [1]\n",
        ] {
            fs::write(root.0.join("src/pyproject.toml"), pyproject).unwrap();
            let err = read("pyproject.toml").await.unwrap_err();
            assert!(matches!(err, Error::Invalid { .. }), "{err:?}");
            assert!(err.to_string().contains("pyproject.toml"), "{err}");
        }

        for missing in ["missing.txt", "sub"] {
            let err = read(missing).await.unwrap_err();
            assert!(
                matches!(&err, Error::Manifest { path, .. } if path.ends_with("workspace.yaml")),
                "{err:?}"
            );
            assert!(
                err.to_string().contains("not a file in the workspace"),
                "{err}"
            );
        }
    }

    /// The entries of the gzip members `members`, decompressed, as (path,
    /// contents), and the number of bytes they take.
    fn members(members: &[u8]) -> (Vec<(String, String)>, usize) {
        let mut tar = Vec::new();
        flate2::read::MultiGzDecoder::new(members)
            .read_to_end(&mut tar)
            .unwrap();
        let mut archive = tar::Archive::new(tar.as_slice());
        let entries = archive.entries().unwrap().map(|entry| {
            let mut entry = entry.unwrap();
            let mut contents = String::new();
            entry.read_to_string(&mut contents).unwrap();
            (entry.path().unwrap().display().to_string(), contents)
        });
        (entries.collect(), tar.len())
    }

    #[test]
    fn compresses_an_installed_package_without_the_end_of_the_archive() {
        let root = TempDir::new();
        let dir = root.0.join("pkg");
        fs::create_dir_all(dir.join("numpy/core")).unwrap();
        fs::write(dir.join("numpy/__init__.py"), "x").unwrap();
        fs::write(dir.join("numpy/core/a.py"), "").unwrap();
        fs::create_dir(dir.join("numpy-2.1.0.dist-info")).unwrap();
        let member = compress(&dir).unwrap();

        // A tar of `.nextmv/python/deps`, then the tree, in name order.
        let (entries, length) = members(&member);
        let names: Vec<&str> = entries.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            names,
            [
                ".nextmv/python/deps",
                ".nextmv/python/deps/numpy",
                ".nextmv/python/deps/numpy/__init__.py",
                ".nextmv/python/deps/numpy/core",
                ".nextmv/python/deps/numpy/core/a.py",
                ".nextmv/python/deps/numpy-2.1.0.dist-info",
            ]
        );
        // Each entry takes a header and its contents in 512-byte blocks,
        // and nothing follows: no end-of-archive blocks.
        assert_eq!(length, 6 * 512 + 512);
        let mut gzip = GzDecoder::new(member.as_slice());
        let mut first = Vec::new();
        gzip.read_to_end(&mut first).unwrap();
        assert_eq!(first.len(), length, "one gzip member");
    }

    #[cfg(unix)]
    #[test]
    fn stores_links_to_files_as_files_and_leaves_out_other_links() {
        use std::os::unix::fs::symlink;

        let root = TempDir::new();
        let dir = root.0.join("pkg");
        fs::create_dir_all(dir.join("lib")).unwrap();
        fs::write(dir.join("lib/real.py"), "real").unwrap();
        symlink(dir.join("lib/real.py"), dir.join("lib/link.py")).unwrap();
        symlink(dir.join("lib"), dir.join("dir-link")).unwrap();
        symlink(root.0.join("missing"), dir.join("broken")).unwrap();
        let (entries, _) = members(&compress(&dir).unwrap());
        assert_eq!(
            entries,
            [
                (".nextmv/python/deps".to_owned(), String::new()),
                (".nextmv/python/deps/lib".to_owned(), String::new()),
                (
                    ".nextmv/python/deps/lib/link.py".to_owned(),
                    "real".to_owned()
                ),
                (
                    ".nextmv/python/deps/lib/real.py".to_owned(),
                    "real".to_owned()
                ),
            ]
        );
    }

    /// A workspace, the fake `uv`, a FICO home and a temp root in one temp
    /// directory.
    struct Setup {
        root: TempDir,
        src: PathBuf,
        uv_dir: PathBuf,
        home: PathBuf,
        temp_root: PathBuf,
    }

    impl Setup {
        fn new() -> Setup {
            let root = TempDir::new();
            let src = root.0.join("src");
            let temp_root = root.0.join("temp");
            for dir in [&src, &temp_root] {
                fs::create_dir_all(dir).unwrap();
            }
            Setup {
                src,
                uv_dir: root.0.join("uv"),
                home: root.0.join("home"),
                temp_root,
                root,
            }
        }

        fn bundler(&self, lockfile: &str, cache: bool) -> Bundler {
            Bundler {
                uv: fake_uv(&self.uv_dir, lockfile),
                cache: cache.then(|| (self.home.clone(), Limits::default())),
                temp_root: self.temp_root.clone(),
            }
        }

        /// Bundles `requirements` with `bundler`, for [`amd64_312`], and
        /// returns the members and the events.
        async fn bundle(
            &self,
            bundler: &Bundler,
            requirements: &ManifestPipRequirements,
        ) -> (Result<Vec<u8>>, Vec<PushEvent>) {
            let events = Mutex::new(Vec::new());
            let manifest_path = self.src.join("workspace.yaml");
            let emit = |event| events.lock().unwrap().push(event);
            let result = bundler
                .bundle(&self.src, &manifest_path, requirements, &amd64_312(), emit)
                .await;
            (result, events.into_inner().unwrap())
        }

        /// The runs of the fake `uv`, with temporary paths as placeholders,
        /// after checking that each ran in the workspace with the system's
        /// certificates.
        fn commands(&self) -> Vec<Vec<String>> {
            self.commands_with_index()
                .into_iter()
                .map(|(args, _)| args)
                .collect()
        }

        /// [`Setup::commands`], each with the `index` the fake `uv` saw.
        fn commands_with_index(&self) -> Vec<(Vec<String>, String)> {
            let src = self.src.canonicalize().unwrap();
            let calls = calls(&self.uv_dir);
            for call in &calls {
                assert_eq!(call.cwd, src);
                assert_eq!(call.certs, expected_certs());
            }
            let temp_root = &self.temp_root;
            calls
                .iter()
                .map(|call| (placeholders(&call.args, temp_root), call.index.clone()))
                .collect()
        }
    }

    /// The `uv pip install` arguments for `spec`.
    fn install(spec: &[&str], only_binary: bool) -> Vec<String> {
        let mut args = strings(&["pip", "install"]);
        args.extend(strings(spec));
        args.push("--no-deps".to_owned());
        if only_binary {
            args.push("--only-binary=:all:".to_owned());
        }
        args.extend(strings(&[
            "--target",
            "<target>",
            "--quiet",
            "--python-platform=x86_64-manylinux_2_34",
            "--python-version=3.12",
        ]));
        args
    }

    fn compile() -> Vec<String> {
        strings(&[
            "pip",
            "compile",
            "<requirements>",
            "--emit-index-url",
            "--emit-find-links",
            "--quiet",
            "--python-platform=x86_64-manylinux_2_34",
            "--python-version=3.12",
        ])
    }

    const LOCKFILE: &str = "\
# This file was autogenerated by uv.
--index-url https://pypi.org/simple

numpy==2.1.0
    # via -r requirements.txt
Pandas==2.2.0 ; python_version >= \"3.10\"
./local
-e ../shared
";

    #[cfg(unix)]
    #[tokio::test]
    async fn bundles_dependencies_through_the_cache() {
        let setup = Setup::new();
        let bundler = setup.bundler(LOCKFILE, true);
        let requirements = ManifestPipRequirements::Packages(strings(&["numpy", "./local"]));

        // The fake `uv` records the mode of the bundle's temp directory.
        fs::write(setup.uv_dir.join("record-mode"), "").unwrap();
        let (first, events) = setup.bundle(&bundler, &requirements).await;
        let first = first.unwrap();
        let mode = fs::read_to_string(setup.uv_dir.join("mode")).unwrap();
        assert_eq!(mode.trim(), "drwx------", "{mode}");
        assert_eq!(
            events,
            [
                PushEvent::DependenciesDownloading { count: 2 },
                PushEvent::DirectRequirementsBuilding { count: 2 },
            ]
        );
        assert_eq!(
            setup.commands(),
            [
                compile(),
                install(&["numpy==2.1.0"], true),
                install(&["Pandas==2.2.0"], true),
                install(&["./local"], false),
                install(&["../shared"], false),
            ]
        );
        let requirements_sent = fs::read_to_string(setup.uv_dir.join("requirements")).unwrap();
        assert_eq!(requirements_sent, "numpy\n./local\n");
        assert!(listing(&setup.temp_root).is_empty());
        let info = cache::info(&setup.home).await.unwrap();
        let mut cached: Vec<(String, String)> = info
            .dependencies
            .iter()
            .map(|d| (d.name.clone(), d.platform.clone()))
            .collect();
        cached.sort();
        let platform = "x86_64-manylinux_2_34".to_owned();
        assert_eq!(
            cached,
            [
                ("Pandas".to_owned(), platform.clone()),
                ("numpy".to_owned(), platform)
            ]
        );

        // The packages come from the cache now, but the other requirements
        // are built again.
        let (second, events) = setup.bundle(&bundler, &requirements).await;
        let second = second.unwrap();
        assert_eq!(
            events,
            [
                PushEvent::DependenciesCached {
                    cached: 2,
                    total: 2
                },
                PushEvent::DirectRequirementsBuilding { count: 2 },
            ]
        );
        assert_eq!(
            setup.commands(),
            [
                compile(),
                install(&["./local"], false),
                install(&["../shared"], false),
            ]
        );
        assert!(listing(&setup.temp_root).is_empty());

        // Both unpack, with the workspace's files after them.
        fs::write(setup.src.join("main.py"), "print()").unwrap();
        for members in [first, second] {
            let mut bundle = members;
            let files = vec![("main.py".into(), setup.src.join("main.py"))];
            bundle.extend(archive::pack_files(files).await.unwrap());
            let out = setup.root.0.join("out");
            let _ = fs::remove_dir_all(&out);
            archive::unpack(bundle, out.clone()).await.unwrap();
            let deps = out.join(".nextmv/python/deps");
            assert_eq!(
                listing(&deps),
                ["Pandas__2_2_0", "___shared", "__local", "numpy__2_1_0"]
            );
            let numpy = fs::read_to_string(deps.join("numpy__2_1_0/__init__.py")).unwrap();
            assert_eq!(numpy, "numpy==2.1.0");
            assert_eq!(fs::read_to_string(out.join("main.py")).unwrap(), "print()");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn installs_from_the_indexes_that_resolved_the_packages() {
        let setup = Setup::new();
        let lockfile = format!(
            "--index-url {}\n--find-links https://wheels.example.com/\n\nnumpy==2.1.0\n./local\n",
            private_index()
        );
        let bundler = setup.bundler(&lockfile, true);
        let requirements = ManifestPipRequirements::Packages(strings(&["numpy"]));
        let (members, _) = setup.bundle(&bundler, &requirements).await;
        members.unwrap();

        // Every install gets the indexes, through the environment, and the
        // options are not read as requirements.
        let index = format!("{}||https://wheels.example.com/", private_index());
        assert_eq!(
            setup.commands_with_index(),
            [
                (compile(), "||".to_owned()),
                (install(&["numpy==2.1.0"], true), index.clone()),
                (install(&["./local"], false), index),
            ]
        );
        // The entry's key isn't PyPI's numpy, and holds no credentials.
        let pypi_numpy = Dependency {
            name: "numpy".to_owned(),
            version: "2.1.0".to_owned(),
            python_version: "3.12".to_owned(),
            platform: "x86_64-manylinux_2_34".to_owned(),
            sources: None,
        };
        let info = cache::info(&setup.home).await.unwrap();
        assert_eq!(info.dependencies.len(), 1);
        assert_ne!(info.dependencies[0].key, pypi_numpy.key());
        let private_numpy = Dependency {
            sources: Some(
                "--index-url https://pkgs.example.com/simple \
                 --find-links https://wheels.example.com/"
                    .to_owned(),
            ),
            ..pypi_numpy
        };
        assert_eq!(info.dependencies[0].key, private_numpy.key());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn neither_caches_nor_takes_from_the_cache_with_a_local_find_links() {
        let setup = Setup::new();
        let lockfile = "--find-links ./wheels\n\nmylib==0.1.0\n";
        let bundler = setup.bundler(lockfile, true);
        let requirements = ManifestPipRequirements::Packages(strings(&["mylib"]));
        for _ in 0..2 {
            let (members, events) = setup.bundle(&bundler, &requirements).await;
            members.unwrap();
            // Built again each time, as the wheel may have changed.
            assert_eq!(events, [PushEvent::DependenciesDownloading { count: 1 }]);
        }
        let commands = setup.commands_with_index();
        assert_eq!(commands.len(), 4);
        let mut expected = install(&["mylib==0.1.0"], true);
        let at = expected.iter().position(|arg| arg == "--target").unwrap();
        expected.insert(at, "--no-index".to_owned());
        assert_eq!(commands[1], (expected, "||./wheels".to_owned()));
        assert_eq!(cache::info(&setup.home).await.unwrap().num_dependencies, 0);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bundles_without_reading_or_writing_the_cache() {
        let setup = Setup::new();
        let numpy = Dependency {
            name: "numpy".to_owned(),
            version: "2.1.0".to_owned(),
            python_version: "3.12".to_owned(),
            platform: "x86_64-manylinux_2_34".to_owned(),
            sources: None,
        };
        cache::store(&setup.home, &numpy, b"cached", Limits::default())
            .await
            .unwrap();
        let before = cache::info(&setup.home).await.unwrap();

        let bundler = setup.bundler(
            "--index-url https://pypi.org/simple\nnumpy==2.1.0\npandas==2.2.0\n",
            false,
        );
        let requirements = ManifestPipRequirements::Packages(strings(&["pandas"]));
        let (members, events) = setup.bundle(&bundler, &requirements).await;
        let (entries, _) = self::members(&members.unwrap());
        assert!(
            entries
                .iter()
                .any(|(name, _)| name.ends_with("numpy__2_1_0/__init__.py"))
        );
        assert_eq!(events, [PushEvent::DependenciesDownloading { count: 2 }]);
        assert_eq!(
            setup.commands(),
            [
                compile(),
                install(&["numpy==2.1.0"], true),
                install(&["pandas==2.2.0"], true),
            ]
        );
        assert_eq!(cache::info(&setup.home).await.unwrap(), before);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bundles_nothing_for_an_empty_lockfile() {
        let setup = Setup::new();
        let bundler = setup.bundler("# nothing\n", true);
        let requirements = ManifestPipRequirements::Packages(strings(&["-r other.txt"]));
        let (members, events) = setup.bundle(&bundler, &requirements).await;
        assert!(members.unwrap().is_empty());
        assert!(events.is_empty());
        assert_eq!(setup.commands(), [compile()]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fails_when_uv_fails() {
        let setup = Setup::new();
        let requirements = ManifestPipRequirements::Packages(strings(&["numpy"]));
        let bundler = setup.bundler(
            "--index-url https://pypi.org/simple\nnumpy==2.1.0\nbroken==1.0\n",
            true,
        );
        let (result, _) = setup.bundle(&bundler, &requirements).await;
        let err = result.unwrap_err();
        assert!(
            matches!(
                &err,
                Error::Command { program, status: Some(2), stderr }
                    if program == "uv" && stderr == "no wheel for broken==1.0\n"
            ),
            "{err:?}"
        );
        assert!(listing(&setup.temp_root).is_empty());

        fs::write(setup.uv_dir.join("compile-fails"), "").unwrap();
        let (result, events) = setup.bundle(&bundler, &requirements).await;
        let err = result.unwrap_err();
        assert!(
            matches!(
                &err,
                Error::Command { program, status: Some(1), stderr }
                    if program == "uv" && stderr == "resolution failed\n"
            ),
            "{err:?}"
        );
        assert!(events.is_empty());
        assert!(listing(&setup.temp_root).is_empty());
    }

    #[tokio::test]
    async fn fails_before_running_uv_for_a_missing_requirements_file() {
        let setup = Setup::new();
        let bundler = setup.bundler("numpy==2.1.0\n", true);
        let requirements = ManifestPipRequirements::File("requirements.txt".to_owned());
        let (result, _) = setup.bundle(&bundler, &requirements).await;
        assert!(matches!(result, Err(Error::Manifest { .. })), "{result:?}");
        assert!(setup.commands().is_empty());
        assert!(listing(&setup.temp_root).is_empty());
    }

    #[tokio::test]
    async fn reports_a_uv_that_cannot_start() {
        let setup = Setup::new();
        let mut bundler = setup.bundler("", true);
        bundler.uv = Uv::binary("/nonexistent/uv".into());
        let requirements = ManifestPipRequirements::Packages(strings(&["numpy"]));
        let (result, _) = setup.bundle(&bundler, &requirements).await;
        let Err(Error::Io { path, source }) = result else {
            panic!("{result:?}");
        };
        assert_eq!(path, Path::new("/nonexistent/uv"));
        assert_eq!(source.kind(), io::ErrorKind::NotFound);
        assert!(listing(&setup.temp_root).is_empty());
    }
}
