# AnyAI Architecture

## What AnyAI is

**AnyAI is a local API surface for local AI.** A single binary exposes an OpenAI-compatible HTTP API on `127.0.0.1` that resolves "what model should I run on this machine?" against a JSON file you (or someone else) host. The GUI and CLI are two clients of that same surface; nothing in the design assumes a human is watching.

The "centralized" piece is decentralized by construction: the source of truth for which models a team uses is a static JSON file at a URL the team controls. Any host (GitHub Pages, S3, an internal HTTP server) is sufficient. JSON files can `import` other JSON files to compose merged catalogs across publishers.

## One picture

```
   HTTP clients ───────►  ┌──────────────────────────────────────────────────┐
   (Cursor, Continue,     │   anyai (single binary)                          │
    Aider, agents,        │                                                  │
    your scripts)         │   axum API   (default :1473) ◄── primary surface │
                          │      │                                            │
                          │      ▼                                            │
                          │   resolver    (virtual ID → tag)                  │
                          │     │   ▲                                         │
                          │     │   │ per-file TTL, recursive imports         │
                          │     │   │ (manifests + source catalogs)           │
                          │     ▼   │                                         │
                          │   fetch & cache (~/.anyai/cache)                  │
                          │      │                                            │
                          │      ▼                                            │
                          │   preload     (pull, warm, ensure_tracked_models)│
                          │   watcher     (5-min ticks; hot-swap on update;  │
                          │                self-update check)                 │
                          │      │                                            │
                          │      ▼                                            │
                          │   ollama.rs   (manage `ollama serve` child)       │
                          │                                                  │
                          │   CLI         ◄── thin client of the same core   │
                          │   GUI (Tauri) ◄── thin client of the same core   │
                          └──────┬───────────────────────────────────────────┘
                                 │ subprocess + HTTP 127.0.0.1:11434
                                 ▼
                          ┌─────────────┐
                          │   Ollama    │
                          └─────────────┘
```

The same Rust binary handles three personas, picked at process-start by argv:

| Invocation       | Persona                                                          |
|------------------|------------------------------------------------------------------|
| `anyai serve`    | Headless OpenAI-compat server (the primary use case)             |
| `anyai <cmd>`    | CLI (status, models, providers, sources, preload, import/export) |
| `anyai`          | GUI (Tauri); also runs the API server alongside                  |

## The provider/source ecosystem

Two kinds of JSON file. Both are static, both are cached by `~/.anyai/cache/`, both honour their own `ttl_minutes` independently:

- **Manifest** — `{ name, version, ttl_minutes?, default_mode, modes, imports? }`. Maps hardware tiers to model tags. Resolved once per request by the API server.
- **Source catalog** — `{ name, ttl_minutes?, providers: [...], imports? }`. Lists provider URLs. Browsed when a user adds a provider.

`imports` on either file is an array of URLs to other files of the same kind. The fetcher walks them recursively, dedupes by URL, detects cycles, and merges results in document order (the importing file's own entries win on name collision). **Each imported file is fetched and cached against its own `ttl_minutes`** — the recursion does not flatten TTL, so a slow-changing top-level catalog can import a fast-moving one without the publisher having to coordinate.

That per-file TTL is also how publishers express rate-limit expectations: a manifest hosted on a free static host might say `ttl_minutes: 1440` to keep load down; a high-availability commercial endpoint might say `5`.

## Modules (Rust)

| File | Role |
|------|------|
| `main.rs` | argv branching; setup hook spawns watcher, self-update checker, and API server. |
| `cli.rs`  | Every CLI subcommand. |
| `api.rs`  | axum router, virtual-ID resolution, pull-on-demand, model rewrite. |
| `api_models.rs` | OpenAI-compatible request/response types. |
| `resolver.rs` | Manifest/source fetch + per-file TTL cache, recursive imports with cycle detection, hardware-tier walk, virtual-ID map. Mirrors `src/manifest.ts` and `src/sources.ts`. |
| `preload.rs` | `preload(modes, …)` + `ensure_tracked_models()` reconcile loop. |
| `watcher.rs` | Background ticker (every 5 min) that re-runs `ensure_tracked_models`, recomputes model-status, and triggers `self_update::tick`. Process lock at `~/.anyai/watcher.lock`. |
| `self_update.rs` | Periodic GitHub-releases check, channel-aware (stable/beta), patch auto-apply, atomic rename-on-restart, package-manager-install detection (no-op when installed via brew/apt/rpm/MSI). |
| `hardware.rs` | nvidia-smi / rocm-smi / sysctl / /proc detection. |
| `ollama.rs` | spawn/stop `ollama serve`, pull, list, delete, warm, has_model. |

## Modules (TypeScript)

The TS layer is the GUI's source of truth. The Rust layer reads the same on-disk caches/config so headless commands work without booting Node.

| File | Role |
|------|------|
| `config.ts` | Read/write `~/.anyai/config.json` with default-merge for upgrades. |
| `manifest.ts` | `getManifest(url)` (per-file TTL cached, recursive imports), `resolveModel`, `allRecommendedModels`. |
| `providers.ts`, `sources.ts` | CRUD over saved providers/sources. `fetchSourceCatalog(url)` walks `imports` recursively. |
| `model-lifecycle.ts` | `recomputeRecommendedSet`, `runCleanup`, `pruneNow`, `markEvictedNow`. |
| `import-export.ts` | Bundle config to/from `anyai:import:…` URLs. |
| `preload.ts`, `watcher.ts` | Thin Tauri-invoke wrappers for the Rust counterparts. |
| `ui/*.svelte` | Svelte 5 UI. |

## Live update lifecycle

```
  Manifest URL changes (provider edit) or contents change (TTL refresh) or
  imported manifest changes (its own TTL refresh)
       │
       ▼
  watcher tick (5 min)  ── or ──  CLI provider/source mutation
       │
       ▼
  preload::ensure_tracked_models()
       │
       ├─ for each tracked mode: resolver::resolve(mode) → new tag
       │       │   (resolve fetches the manifest, recurses imports,
       │       │    each at its own TTL, merged in document order)
       │       │
       │       ├─ if tag not pulled  → ollama::pull_with(...)
       │       └─ if tag changed     → emit anyai://mode-swap
       │
       ▼
  watcher::recompute_status_from_disk()
       │
       └─ writes ~/.anyai/cache/model-status.json
              old tag's recommended_by becomes empty
              last_recommended timestamp = now (clock starts)
              model-lifecycle.runCleanup() will evict after model_cleanup_days
```

Hot-swap semantics: the OpenAI server reads `resolver::resolve(mode)` per request, so the next call after a swap hits the new tag transparently. In-flight streams keep using the old tag (Ollama keeps it loaded for `keep_alive`).

## Self-update lifecycle

```
  watcher tick (every 5 min)
       │
       ▼
  self_update::tick()
       │
       ├─ install kind?
       │     └─ homebrew / dpkg / rpm / MSI / chocolatey  → return (defer to PM)
       │     └─ raw binary on PATH                        → continue
       │
       ├─ HEAD https://api.github.com/repos/…/releases/{channel}
       │     (etag-cached; cheap when unchanged)
       │
       ├─ new tag, same major.minor → patch:  auto-apply
       │   new tag, different minor or major:  download, stage, notify
       │
       ├─ download asset for current platform
       ├─ verify SHA256 from release manifest
       ├─ stage at  ~/.anyai/updates/<version>/anyai(.exe)
       │
       └─ on next launch (or on SIGTERM if running as daemon):
             atomically rename staged binary over the running one
             (Windows: scheduled rename via MoveFileEx + restart)
```

Config (in `~/.anyai/config.json`):

```jsonc
{
  "auto_update": {
    "enabled": true,
    "channel": "stable",          // "stable" | "beta"
    "auto_apply": "patch",        // "patch" | "minor" | "all" | "none"
    "check_interval_hours": 6
  }
}
```

Disabling: `auto_update.enabled = false`, or `ANYAI_AUTOUPDATE=0`. When AnyAI detects a package-manager install, the updater logs a one-line note and stays out of the way regardless of config.

## Why no extra HTTP framework?

- **axum** for the server: tower-compatible, ergonomic streaming via `Body::from_stream`, ~3 MB stripped impact. Already paired with `reqwest` for upstream calls (rustls-tls so we don't pull OpenSSL on Linux).
- **No router for the GUI** — Tauri IPC handles that.
- **No global state crate** — `OnceLock<Mutex<…>>` covers the per-process locks we need (Ollama child handle, watcher start gate, preload mutex).

## Persistence

```
~/.anyai/
├── config.json                       (user settings + tracked_modes + api + auto_update)
├── watcher.lock                      (PID; cooperative process lock)
├── updates/                          (staged self-update binaries)
└── cache/
    ├── manifests/<hash>.json         (manifest + fetched_at, per-URL — imports cached separately)
    ├── sources/<hash>.json           (source catalog + fetched_at, per-URL — imports cached separately)
    └── model-status.json             (recommended_by + last_recommended per tag)
```
