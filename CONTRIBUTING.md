# Contributing

## Prerequisites

Install these tools before setting up the project. `just setup` needs `just`
to run; it then checks for the other tools and stops if any is missing.

- [Rust](https://www.rust-lang.org/tools/install), via `rustup`. `just setup`
  installs the exact toolchain pinned in `rust-toolchain.toml`.
- [`just`](https://github.com/casey/just): `cargo install --locked just@1.58.0`,
  the version CI installs.
- [`cargo-deny`](https://github.com/EmbarkStudios/cargo-deny):
  `cargo install --locked cargo-deny@0.20.2`, the version CI installs. Other
  versions can give `just deny` a different result.
- [`uv`](https://docs.astral.sh/uv/). It manages `pre-commit` and `zizmor`,
  pinned in `pyproject.toml`. On a FICO® Corporate machine, also let `uv` use the
  company's TLS certificates (consider adding this to your shell profile):

  ```bash
  export UV_SYSTEM_CERTS=true
  ```

## Setup

Set up the project. Safe to run any number of times:

```bash
just setup
```

## Common tasks

```bash
just build        # cargo build
just check        # cargo check (fast type-check, no codegen)
just test         # cargo test; extra arguments go to cargo test
just fmt          # cargo fmt (rewrites files)
just fmt-check    # cargo fmt --check
just lint         # cargo clippy
just doc          # cargo doc, failing on rustdoc warnings
just deny         # cargo deny check (licenses, advisories)
just zizmor       # zizmor (security issues in the GitHub workflows)
just verify       # fmt-check + lint + build + test + doc + deny + zizmor
just clean        # cargo clean (deletes target/)
just clean-stale  # cargo clean, only when target/ is old or large
just hooks        # run all pre-push hooks manually
just integration  # run the live integration suites (see below)
```

## Pre-push hooks

`just setup` installs [`pre-commit`](https://pre-commit.com) hooks that run on
every push: stale-artifact cleanup, formatting, clippy, build, tests, docs,
`cargo deny` and `zizmor`. They are configured in `.pre-commit-config.yaml`.
Run them manually with `just hooks`.

A hook prints its output only when it fails, except the cleanup, which always
says whether it cleaned. The tests run with `just test -q`, which prints a dot
per passing test and a failing test's output in full.

CI (`.github/workflows/ci.yml`) runs the same `just` recipes on every pull
request, one job per hook, except the cleanup. The `zizmor` job runs zizmor
through its GitHub action instead of `just zizmor`.

`Cargo.lock` is committed, so CI builds the exact dependency versions you
tested. Dependabot updates them, the pinned actions, and the Python dev tools
in `pyproject.toml`, in weekly PRs. CI's zizmor comes from its action, so it can
differ from `pyproject.toml`'s until both Dependabot PRs are merged. The
`just` recipes pass `--locked`, so they fail when `Cargo.lock` doesn't match
`Cargo.toml`. After changing a dependency, run plain `cargo check` once to
update `Cargo.lock`, and commit it.

## Cleaning build artifacts

Cargo never deletes old artifacts, so `target/` keeps growing. The first
pre-push hook runs `just clean-stale`, which runs `cargo clean` when the last
clean is older than `CLEAN_MAX_AGE_DAYS` days (default 7) or `target/` exceeds
`CLEAN_MAX_SIZE_GIB` GiB (default 10).

## Integration tests

`tests/decision_platform_integration` holds live suites that reach the
platform. There is one suite per content format: `json` pushes
`fixtures/python-hello-world`, and `multi_file` pushes
`fixtures/python-ortools-multiknapsack-multiexcel`, both in that directory.
Each suite creates its own workspace, runs every platform operation on it, and
deletes it at the end, also when it fails. They are `#[ignore]`d, so
`just test`, the hooks and the `CI/CD checks` workflow never reach the network.

They need:

- `FICO_API_KEY`, the API key of the account the workspaces are made in.
- `FICO_ENDPOINT` to reach another endpoint than the default,
  `https://api.cloud.nextmv.io`.

Each suite's FICO home is new, so its first push downloads the SDK's `uv`,
which bundles the fixtures' Python dependencies.

```bash
just integration             # both suites, at the same time
just integration multi_file  # one suite
```

Each suite takes a few minutes, and prints each stage and how long it took.
A suite that takes more than 10 minutes fails. If a suite cannot delete its
workspace, it prints the workspace's ID.

The `integration tests` workflow (`.github/workflows/integration.yml`) runs
each suite in its own job, with `FICO_API_KEY` set from the repository secret
`FICO_DECISIONOPS_API_KEY`.

## Upgrading `uv`

The SDK pins the `uv` it installs for push and local Python runs, in
`src/uv.rs`. To move to a new release, set `UV_VERSION` and replace each
SHA-256 in `ASSETS` with the one for that archive in the release's
`sha256.sum`, written as bytes:

```bash
curl -sL https://github.com/astral-sh/uv/releases/download/<version>/sha256.sum
```

Then run a push and a local run of a Python workspace with dependencies,
to check the new `uv` with the SDK's commands.

## Style

- Follow standard Rust idioms and `rustfmt` defaults.
- Public items must have doc comments (`///` or `//!`), enforced by the
  `missing_docs` lint in `Cargo.toml`.
- A public function that returns a `Result` has an `# Errors` section, and one
  that can panic a `# Panics` section, enforced by clippy's
  `missing_errors_doc` and `missing_panics_doc` lints in `Cargo.toml`.
- Docs build without rustdoc warnings, private items included (`just doc`), so
  every intra-doc link resolves.
- No `unsafe` code, enforced for all targets by `unsafe_code = "forbid"` in
  `Cargo.toml`.
- New dependencies must pass `cargo deny check` (permissive license, no known
  advisories).
