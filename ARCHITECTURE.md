# AnyAI Architecture

## One picture

```
                          ┌──────────────────────────────────────────────────┐
                          │   anyai (single binary)                          │
                          │                                                  │
   stdin/stdout ◄──────►  │   CLI         (anyai run / preload / models …)   │
                          │                                                  │
   GUI window  ◄──────►   │   Tauri v2    (Svelte 5 UI)                      │
                          │                                                  │
   HTTP clients ◄────►    │   axum API   (anyai serve, default :1473)        │
   (Cursor, Continue,     │      │                                            │
    Aider, agents)        │      ▼                                            │
                          │   resolver    (virtual ID → tag, manifest TTL)   │
                          │      │                                            │
                          │      ▼                                            │
                          │   preload     (pull, warm, ensure_tracked_models)│
                          │   watcher     (5-min ticks; hot-swap on update)  │
                          │      │                                            │
                          │      ▼                                            │
                          │   ollama.rs   (manage `ollama serve` child)       │
                          └──────┬───────────────────────────────────────────┘
                                 │ subprocess + HTTP 127.0.0.1:11434
                                 ▼
                          ┌─────────────┐
                          │   Ollama    │
                          └─────────────┘
```

The same Rust binary handles three personas, picked at process-start by argv:

| Invocation       | Persona                                                |
|------------------|--------------------------------------------------------|
| `anyai`          | GUI window (Tauri); also runs the API server alongside |
| `anyai serve`    | Headless OpenAI-compat server                          |
| `anyai <cmd>`    | CLI                                                    |

## Modules (Rust)

| File | Role |
|------|------|
| `main.rs` | argv branching; Tauri setup hook spawns watcher + API server. |
| `cli.rs`  | Every CLI subcommand. |
| `api.rs`  | axum router, virtual-ID resolution, pull-on-demand, model rewrite. |
| `api_models.rs` | OpenAI-compatible request/response types. |
| `resolver.rs` | Manifest fetch + TTL cache, hardware-tier walk, virtual-ID map. Mirrors `src/manifest.ts`. |
| `preload.rs` | `preload(modes, …)` + `ensure_tracked_models()` reconcile loop. |
| `watcher.rs` | Background ticker that re-runs `ensure_tracked_models` and recomputes model-status. Process lock at `~/.anyai/watcher.lock`. |
| `hardware.rs` | nvidia-smi / rocm-smi / sysctl / /proc detection. |
| `ollama.rs` | spawn/stop `ollama serve`, pull, list, delete, warm, has_model. |

## Modules (TypeScript)

The TS layer is the GUI's source of truth. The Rust layer reads the same on-disk caches/config so headless commands work without booting Node.

| File | Role |
|------|------|
| `config.ts` | Read/write `~/.anyai/config.json` with default-merge for upgrades. |
| `manifest.ts` | `getManifest(url)` (TTL cached), `resolveModel`, `allRecommendedModels`. |
| `providers.ts`, `sources.ts` | CRUD over saved providers/sources. |
| `model-lifecycle.ts` | `recomputeRecommendedSet`, `runCleanup`, `pruneNow`, `markEvictedNow`. |
| `import-export.ts` | Bundle config to/from `anyai:import:…` URLs. |
| `preload.ts`, `watcher.ts` | Thin Tauri-invoke wrappers for the Rust counterparts. |
| `ui/*.svelte` | Svelte 5 UI. |

## Live update lifecycle

```
  Manifest URL changes (provider edit) or contents change (TTL refresh)
       │
       ▼
  watcher tick (5 min)  ── or ──  CLI provider/source mutation
       │
       ▼
  preload::ensure_tracked_models()
       │
       ├─ for each tracked mode: resolver::resolve(mode) → new tag
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

## Why no extra HTTP framework?

- **axum** for the server: tower-compatible, ergonomic streaming via `Body::from_stream`, ~3 MB stripped impact. Already paired with `reqwest` for upstream calls (rustls-tls so we don't pull OpenSSL on Linux).
- **No router for the GUI** — Tauri IPC handles that.
- **No global state crate** — `OnceLock<Mutex<…>>` covers the per-process locks we need (Ollama child handle, watcher start gate, preload mutex).

## Persistence

```
~/.anyai/
├── config.json                       (user settings + tracked_modes + api block)
├── watcher.lock                      (PID; cooperative process lock)
└── cache/
    ├── manifests/<hash>.json         (provider manifest + fetched_at)
    ├── sources/<hash>.json           (source catalog + fetched_at)
    └── model-status.json             (recommended_by + last_recommended per tag)
```
