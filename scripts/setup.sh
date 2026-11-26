#!/usr/bin/env bash
set -e

###### README ######
# This script is meant to be executed once after a fresh clone of the repo.
# It is safe to run any number of times.

# Ensure we are operating from the root of the repository.
REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

# Verify that the required tools are installed on the developer's system.
for tool in rustup cargo just cargo-deny uv; do
    echo "⚙️ Checking if '$tool' is installed..."
    if ! command -v "$tool" &> /dev/null; then
        echo "❌ Error: '$tool' is not installed or not in your PATH. See the prerequisites in CONTRIBUTING.md." >&2
        exit 1
    fi
    echo "✨ '$tool' found: $("$tool" --version 2>/dev/null | head -n 1)"
done

# Install the toolchain pinned in rust-toolchain.toml, with its components.
echo "💾 Installing the pinned Rust toolchain."
rustup toolchain install

# Sync the Python dev tooling (pre-commit, zizmor) pinned in pyproject.toml.
echo "💾 Syncing the Python dev tooling."
uv sync

# Set up pre-push hooks (using the pre-commit tool framework).
echo "⚙️ Setting up pre-push hooks."
uv run pre-commit install --hook-type pre-push
echo "✨ Successfully set up pre-push hooks."

# Run the validation checks as a simulation.
echo "⚙️ Doing a dry-run of pre-push hooks to make sure they work."
uv run pre-commit run --all-files --hook-stage pre-push
echo "✨ Successfully did a dry-run of pre-push hooks."

echo "✅ Successfully set up the project."
