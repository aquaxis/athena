#!/bin/sh
# ATHENA one-liner installer (requirements R-OPS-09, design §2.5).
#
# One-line install from the published repository:
#   curl -fsSL https://raw.githubusercontent.com/aquaxis/athena/main/install.sh | sh
# or from an existing clone:
#   ./install.sh
#
# Installs the tool only: the `athena` and `athena-api` binaries onto PATH via
# `cargo install` (R-OPS-03's route). Never creates or modifies a project's
# `.athena/` — project configuration and data stay per-project (R-OPS-01/02) —
# and adds no secrets anywhere (ATH-CFG-005). Idempotent: a re-run refreshes the
# installed binaries.
set -eu

REPO_URL="${ATHENA_REPO_URL:-https://github.com/aquaxis/athena.git}"

# Locate the source: beside this script when run from a clone; otherwise fetch a
# worktree into a temporary directory (the curl | sh case). Override the source
# with ATHENA_REPO_URL when installing from a different location.
script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" 2>/dev/null && pwd -P) || script_dir=""
workdir=""
if [ -n "$script_dir" ] && [ -f "$script_dir/Cargo.toml" ] && [ -d "$script_dir/crates" ]; then
    repo="$script_dir"
else
    command -v git >/dev/null 2>&1 || {
        echo "install.sh: git is required to fetch the source" >&2
        exit 1
    }
    workdir=$(mktemp -d)
    repo="$workdir/athena"
    echo "install.sh: fetching the source from $REPO_URL"
    git clone --depth 1 "$REPO_URL" "$repo" || {
        echo "install.sh: could not clone $REPO_URL — set ATHENA_REPO_URL to the repository to install from" >&2
        exit 1
    }
fi
trap '[ -n "$workdir" ] && rm -rf "$workdir"' EXIT

# 1. Prerequisites: cargo/rustc; rustup is the route when a toolchain must be added.
command -v cargo >/dev/null 2>&1 || {
    echo "install.sh: cargo not found — install Rust first (https://rustup.rs)" >&2
    exit 1
}

# 2. Build the workspace in release mode.
echo "install.sh: building the workspace (release)"
( cd "$repo" && cargo build --release )

# 3. Install the deliverable binaries onto PATH.
echo "install.sh: installing athena (crates/athena-cli)"
( cd "$repo" && cargo install --path crates/athena-cli )
echo "install.sh: installing athena-api (crates/athena-api)"
( cd "$repo" && cargo install --path crates/athena-api )

# 4. Next steps.
echo
echo "ATHENA installed: athena, athena-api"
echo "Next steps:"
echo "  1. cd to your project and run:          athena init"
echo "  2. edit .athena/config.yaml and set:    core.agent.provider,"
echo "                                          core.agent.model,"
echo "                                          core.agent.delegable_tools"
echo "     (secrets come from the environment or a secret manager, never the file)"
echo "  3. start the resident Core API:         athena-api"
echo "  4. submit work:                         athena run \"<instruction>\""
