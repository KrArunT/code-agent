#!/usr/bin/env bash
set -euo pipefail

REPO_URL="${REPO_URL:-https://github.com/KrArunT/code-agent.git}"
BRANCH="${BRANCH:-main}"
INSTALL_ROOT="${INSTALL_ROOT:-$HOME/.local}"

if ! command -v cargo >/dev/null 2>&1; then
    echo "error: cargo is required to install coding-agent-rs" >&2
    echo "install Rust first: https://rustup.rs/" >&2
    exit 1
fi

if ! command -v git >/dev/null 2>&1; then
    echo "error: git is required to install coding-agent-rs" >&2
    exit 1
fi

echo "Installing coding-agent-rs from ${REPO_URL} (${BRANCH}) into ${INSTALL_ROOT} ..."
cargo install \
    --git "$REPO_URL" \
    --branch "$BRANCH" \
    --locked \
    --force \
    --root "$INSTALL_ROOT"

echo
echo "Installed to: ${INSTALL_ROOT}/bin/coding-agent-rs"
echo "If needed, add this to your PATH:"
echo "  export PATH=\"${INSTALL_ROOT}/bin:\$PATH\""
