#!/usr/bin/env bash
set -euo pipefail

REPO_URL="${REPO_URL:-https://github.com/KrArunT/code-agent.git}"
BRANCH="${BRANCH:-main}"
INSTALL_ROOT="${INSTALL_ROOT:-$HOME/.local}"

ensure_rust() {
    if command -v cargo >/dev/null 2>&1; then
        return 0
    fi

    if ! command -v curl >/dev/null 2>&1; then
        echo "error: curl is required to bootstrap Rust" >&2
        exit 1
    fi

    echo "Rust is not installed. Bootstrapping rustup..."
    curl -fsSL https://sh.rustup.rs | sh -s -- -y --no-modify-path

    if [ -f "$HOME/.cargo/env" ]; then
        # shellcheck disable=SC1090
        . "$HOME/.cargo/env"
    elif [ -d "$HOME/.cargo/bin" ]; then
        export PATH="$HOME/.cargo/bin:$PATH"
    fi

    if ! command -v cargo >/dev/null 2>&1; then
        echo "error: Rust bootstrap completed but cargo is still unavailable" >&2
        exit 1
    fi
}

if ! command -v git >/dev/null 2>&1; then
    echo "error: git is required to install coding-agent-rs" >&2
    exit 1
fi

ensure_rust

echo "Installing coding-agent-rs from ${REPO_URL} (${BRANCH}) into ${INSTALL_ROOT} ..."
cargo install \
    --git "$REPO_URL" \
    --branch "$BRANCH" \
    --locked \
    --force \
    --root "$INSTALL_ROOT"

echo
echo "Installed to: ${INSTALL_ROOT}/bin/autofix"
echo "If needed, add this to your PATH:"
echo "  export PATH=\"${INSTALL_ROOT}/bin:\$PATH\""
