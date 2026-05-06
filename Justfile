# AnyAI — one-command operations.
# Install `just` (https://just.systems) then run `just setup` to get going.

set shell := ["bash", "-cu"]

default: help

help:
    @just --list

# Install all dev prerequisites (Rust, Node, pnpm, Tauri CLI, Linux GTK deps).
setup:
    @./scripts/bootstrap.sh

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
    @if [ -x src-tauri/target/release/anyai ]; then \
        src-tauri/target/release/anyai {{ARGS}}; \
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

# Cut a release via the GitHub release workflow.
release tag:
    @gh workflow run release.yml -f tag={{tag}}
