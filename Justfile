# MyOwnLLM — one-command operations.
# Install `just` (https://just.systems) then run `just setup` to get going.

set shell := ["bash", "-cu"]

default: help

help:
    @just --list

# Install all dev prerequisites (Rust, Node, pnpm, Tauri CLI, GTK / Windows SDK deps).
# Platform-split — `[unix]` covers Linux + macOS, `[windows]` covers
# Windows. `set shell` above forces every recipe through bash, so on
# Windows the `[windows]` recipe still goes through git-bash — but git-
# bash can exec native Windows binaries by name, so a `powershell ...`
# line resolves to the real powershell.exe and the .ps1 bootstrap runs
# in a proper PS host.
[unix]
[doc("Install all dev prerequisites (Rust, Node, pnpm, Tauri CLI, GTK deps).")]
setup:
    @./scripts/bootstrap.sh

[windows]
[doc("Install all dev prerequisites (Rust, Node, pnpm, Tauri CLI, Windows SDK).")]
setup:
    @powershell -NoProfile -ExecutionPolicy Bypass -File scripts/bootstrap.ps1

# Run the GUI in dev mode with hot reload.
dev:
    @pnpm install --frozen-lockfile
    @pnpm tauri dev

# Build a production Tauri bundle.
build:
    @pnpm install --frozen-lockfile
    @pnpm tauri build

# Run the binary (build first if needed).
run *ARGS:
    @if [ -x src-tauri/target/release/myownllm ]; then \
        src-tauri/target/release/myownllm {{ARGS}}; \
    else \
        cargo run --release --manifest-path src-tauri/Cargo.toml -- {{ARGS}}; \
    fi

# Start the OpenAI-compatible HTTP server (default port 1473).
serve port="1473":
    @just run serve --port {{port}}

# Preload models for the listed modes (e.g. `just preload text vision code`).
preload +modes:
    @just run preload {{modes}} --track

# Format Rust + frontend.
fmt:
    @cd src-tauri && cargo fmt
    @pnpm exec prettier --write "src/**/*.{ts,svelte,json,md}" || true

# Lint Rust + run svelte-check.
lint:
    @cd src-tauri && cargo clippy --all-targets -- -W warnings
    @pnpm check

# Cheap subset of CI to run locally before pushing.
check: lint
    @cd src-tauri && cargo fmt --check
    @cd src-tauri && cargo test --no-fail-fast

# Cut a release: bump version everywhere, commit, push, trigger the workflow.
# Usage: just release 0.1.8
release version:
    @./scripts/bump-version.sh {{version}}
    @if ! git diff --quiet src-tauri/Cargo.toml src-tauri/Cargo.lock package.json; then \
        git add src-tauri/Cargo.toml src-tauri/Cargo.lock package.json; \
        git commit -m "chore(release): {{version}}"; \
    fi
    @git push
    @gh workflow run release.yml -f tag={{version}}
