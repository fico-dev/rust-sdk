default:
    @just --list

setup:
    bash scripts/setup.sh

build:
    cargo build --locked

check:
    cargo check --locked

# The unit and doc tests; extra arguments go to `cargo test`, such as `-q`
test *args:
    cargo test --locked {{args}}

fmt:
    cargo fmt

fmt-check:
    cargo fmt --check

lint:
    cargo clippy --locked --all-targets -- -D warnings

doc:
    RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps --document-private-items

deny:
    cargo deny --locked check

# Checks the workflows and the Dependabot config for security issues. With a
# GitHub token, from `GH_TOKEN` or `gh`, it also runs the audits that ask
# GitHub, such as for impostor commits; without one, only the offline audits.
zizmor:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -z "${GH_TOKEN:-}" ] && token=$(gh auth token 2>/dev/null); then
        export GH_TOKEN="$token"
    fi
    uv run zizmor --no-progress .

clean:
    cargo clean

clean-stale:
    bash scripts/clean.sh

hooks:
    uv run pre-commit run --all-files --hook-stage pre-push

verify: fmt-check lint build test doc deny zizmor

# Live suites against the platform (needs FICO_API_KEY): both, or `json` or `multi_file`
integration *args:
    cargo test --locked --test decision_platform_integration -- --ignored --nocapture {{args}}
