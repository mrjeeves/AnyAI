# Contributing to AnyAI

## Setup

```bash
git clone https://github.com/mrjeeves/AnyAI
cd AnyAI
just setup        # installs Rust 1.88, Node, pnpm, Tauri CLI, GTK deps on Linux
just dev          # hot-reload GUI
# or
just run -- run   # CLI chat
just serve        # OpenAI-compat server on :1473
just preload text vision
```

`just setup` is idempotent — re-run any time. See [`scripts/bootstrap.sh`](scripts/bootstrap.sh) for what it does.

## Repo layout

- `src-tauri/src/` — Rust: CLI, OpenAI-compat server, hardware detection, Ollama wrapper, watcher.
- `src/` — TypeScript + Svelte: GUI, manifest/source/provider logic, model lifecycle.
- `manifests/`, `providers/` — bundled defaults shipped with the binary.
- `scripts/` — one-line installer + bootstrap.
- `Justfile` — task runner.

## Before opening a PR

```bash
just check    # cargo fmt + clippy + svelte-check + tests
just fmt      # auto-format
```

CI runs the same on Linux/macOS/Windows. PRs that don't pass `just check` won't be reviewed.

## Commit style

Follow [Conventional Commits](https://www.conventionalcommits.org/):

- `feat(api): add /v1/embeddings`
- `fix(cli): preload --json should not interleave with stderr`
- `docs(readme): document virtual model IDs`

## Architecture notes

See [`ARCHITECTURE.md`](ARCHITECTURE.md) for the high-level design and the request-flow diagrams.

## Filing bugs

Include the output of `anyai status --json` and `anyai --version` in every bug report. If the API server is involved, include a `curl -i` of the failing request.
