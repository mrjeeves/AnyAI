# AnyAI

> Local AI that works on **your** hardware. One command. Zero decisions required.

AnyAI detects your GPU/RAM, selects the best model you can actually run, downloads it via [Ollama](https://ollama.com), and starts it. You never need to know what a quantization is.

```
anyai run
# Detecting hardware…  RTX 3060 12GB · 32GB RAM
# Model: Qwen2.5 14B — Downloading (8.9 GB)…
# > _
```

---

## Table of Contents

- [How it works](#how-it-works)
- [Installation](#installation)
- [CLI reference](#cli-reference)
  - [Run / Chat](#run--chat)
  - [Status](#status)
  - [Models](#models)
  - [Providers](#providers)
  - [Sources](#sources)
  - [Import & Export](#import--export)
- [GUI](#gui)
- [Provider system](#provider-system)
  - [What is a Provider?](#what-is-a-provider)
  - [What is a Source?](#what-is-a-source)
  - [Publishing your own](#publishing-your-own)
- [Manifest format](#manifest-format)
- [Model lifecycle & cleanup](#model-lifecycle--cleanup)
  - [Three TTL layers](#three-ttl-layers)
  - [Model eviction](#model-eviction)
  - [Keeping and overriding models](#keeping-and-overriding-models)
- [Import & Export](#import--export-1)
- [Scriptability](#scriptability)
- [Config files](#config-files)
- [Building from source](#building-from-source)
- [Repackaging for your org](#repackaging-for-your-org)
- [Architecture](#architecture)

---

## How it works

```
anyai run
  1. Detect GPU (nvidia-smi / rocm-smi / system_profiler) and RAM
  2. Fetch active provider's manifest (JSON, cached with TTL)
  3. Walk tiers top-to-bottom → pick best model this hardware can run
  4. Check if Ollama is installed → auto-install if not
  5. Check if model is pulled → pull if not (with progress)
  6. Start ollama serve (managed child process)
  7. Open chat (GUI or terminal depending on how you invoked it)
```

AnyAI manages Ollama silently. You never interact with Ollama directly. When AnyAI exits, it stops Ollama.

---

## Installation

### Requirements

- macOS 12+, Linux (x86_64 or aarch64), or Windows 10+
- Internet connection on first run (to pull the model — typically 3–15 GB)
- Ollama is installed automatically if missing

### One-line install (macOS / Linux)

```bash
curl -fsSL https://anyai.run/install.sh | sh
```

The installer tries to download a pre-built binary from the latest GitHub release. If no release matches your platform, it falls back to building from source via `scripts/bootstrap.sh`. Pass `--run` to launch immediately:

```bash
curl -fsSL https://anyai.run/install.sh | sh -s -- --run
```

### From source

```bash
git clone https://github.com/mrjeeves/AnyAI
cd AnyAI
just setup        # installs Rust, Node, pnpm, Tauri CLI, GTK/webkit2gtk on Linux
just build        # produces src-tauri/target/release/anyai
just run          # or: just dev (hot-reload GUI)
```

`just setup` is idempotent — re-run any time. See [Building from source](#building-from-source) for the full prereq list.

---

## CLI reference

Quick reference of subcommands:

| Command            | Purpose                                                |
|--------------------|--------------------------------------------------------|
| `anyai run`        | Chat in the terminal (auto-installs Ollama if missing) |
| `anyai serve`      | Start the OpenAI-compatible HTTP server                |
| `anyai preload`    | Pull and warm models for one or more modes             |
| `anyai status`     | Show provider, mode, hardware, ollama state            |
| `anyai stop`       | Stop the managed Ollama process                        |
| `anyai models`     | List / pin / override / prune pulled models            |
| `anyai providers`  | Manage provider URLs                                   |
| `anyai sources`    | Manage source catalog URLs                             |
| `anyai import`     | Import a config bundle                                 |
| `anyai export`     | Export the current config                              |

### Run / Chat

```bash
anyai                           # open GUI (no args = launch window)
anyai run                       # chat in the terminal
anyai run --mode vision         # use a vision-capable model
anyai run --mode code           # use a code-optimized model
anyai run --mode transcribe     # mic → text via Whisper
anyai run --model qwen2.5:7b    # force a specific model
anyai run --profile https://example.com/manifest.json   # one-off manifest URL
```

`--profile` does not save — it is a single-run override for the active provider's manifest. To save a provider permanently, use `anyai providers add`.

**Modes**

| Mode | What it loads | Notes |
|------|--------------|-------|
| `text` | General-purpose LLM | Default |
| `vision` | Multimodal LLM | Accepts image paths in chat |
| `code` | Code-optimized LLM | Same chat interface |
| `transcribe` | Whisper | Activates mic; outputs transcribed text |

**Exit chat**: `Ctrl+C` or type `exit`.

---

### Status

```bash
anyai status
anyai status --json
```

```
Provider : AnyAI Default
Mode     : text
Ollama   : running
VRAM     : 12.0 GB (nvidia)
RAM      : 32.0 GB
Disk free: 118.3 GB
```

```jsonc
// --json output
{
  "active_provider": "AnyAI Default",
  "active_mode": "text",
  "ollama_running": true,
  "hardware": { "vram_gb": 12.0, "ram_gb": 32.0, "disk_free_gb": 118.3, "gpu_type": "nvidia" }
}
```

---

### Models

Manage pulled Ollama models.

```bash
anyai models                    # list pulled models with status
anyai models --json             # machine-readable list

anyai models keep <model>       # pin — never auto-evict this model
anyai models unkeep <model>     # remove pin

anyai models override <mode> <model>    # force model for a mode
anyai models override <mode> --clear    # revert to provider recommendation

anyai models prune              # evict all unrecommended, non-kept, non-override models now
anyai models rm <model>         # force-remove (also clears keep/override)
```

**Column meanings in `anyai models`:**

```
NAME                                SIZE   FLAGS
qwen2.5:14b                         8.2G   (recommended)
qwen2.5-coder:7b                    4.3G   kept override:code
deepseek-r1:14b                     8.9G   unrecommended 2d
```

- `recommended` — still selected by at least one active provider for this hardware
- `unrecommended Xd` — no active provider recommends it; will be evicted after cleanup threshold
- `kept` — pinned by user, never auto-evicted
- `override:<mode>` — user-selected override for that mode; implicitly kept

---

### Providers

A **provider** is a named, saved URL pointing to a manifest. The active provider determines which model AnyAI recommends for your hardware.

```bash
anyai providers                         # list saved providers (* = active)
anyai providers add <url> --name <name> # save a provider by URL
anyai providers use <name>              # set as active
anyai providers rm <name>               # remove (cannot remove active)
anyai providers show [name]             # fetch and display a provider's manifest
anyai providers reset                   # re-merge bundled preset list
```

**Example:**

```bash
anyai providers add https://deepseek.com/anyai/r1.json --name "DeepSeek R1"
anyai providers use "DeepSeek R1"
anyai run
```

`ANYAI_PROFILE=<url>` env var overrides the active provider at runtime without touching saved config — useful in CI, Docker, or automation:

```bash
ANYAI_PROFILE=https://example.com/minimal.json anyai run
```

---

### Sources

A **source** is a URL returning a catalog of providers — like a package repository. You browse a source's providers, then add individual ones to your saved list.

```bash
anyai sources                           # list saved sources
anyai sources add <url> --name <name>   # add a source
anyai sources list <name>               # browse providers in a source
anyai sources rm <name>                 # remove a source
anyai sources refresh [name]            # force re-fetch (clears TTL cache)
anyai sources reset                     # re-merge bundled preset sources
```

**Example:**

```bash
anyai sources add https://deepseek.com/anyai/sources.json --name DeepSeek
anyai sources list DeepSeek
#   DeepSeek V3    — Flagship general-purpose
#   DeepSeek R1    — Reasoning-focused
#   DeepSeek Coder — Code generation

anyai providers add --from DeepSeek "DeepSeek R1"
anyai providers use "DeepSeek R1"
```

---

### Import & Export

Share your entire provider/source setup as a URL or JSON file.

```bash
anyai export                    # print config JSON to stdout
anyai export --url              # print a shareable anyai:import:... URL
anyai export --sources-only     # only export sources
anyai export --providers-only   # only export providers

anyai import <url-or-path>      # merge from URL, file, or anyai:import:... URL
```

**Share your setup:**

```bash
anyai export --url
# anyai:import:eyJzb3VyY2VzIjpbLi4uXSwicHJvdmlkZXJzIjpbLi4uXX0

# Anyone can import it:
anyai import anyai:import:eyJzb3VyY2VzIjpbLi4uXSwicHJvdmlkZXJzIjpbLi4uXX0
```

Import is always additive — it never overwrites existing entries. Merge is by name: if you already have a provider called "DeepSeek R1", it is not replaced.

---

### Preload

Prepare models for one or more modes ahead of time. Useful before going offline, before a demo, or during setup so the OpenAI server has everything warm.

```bash
anyai preload text                      # pull the text-mode model
anyai preload text vision code          # pull all three
anyai preload text vision --track       # also persist as tracked modes
anyai preload text --no-warm            # skip the post-pull warm-up call
anyai preload text --json               # newline-delimited JSON events
```

Tracked modes are kept current automatically: when a manifest update changes the recommended tag, AnyAI pulls the new one in the background and starts the eviction clock on the old one.

---

### Serve (OpenAI-compatible API)

`anyai serve` starts an OpenAI-compatible HTTP server on `127.0.0.1:1473` so any tool that speaks the OpenAI wire format (Cursor, Continue, Aider, custom agents) can use AnyAI as a drop-in provider.

```bash
anyai serve                             # 127.0.0.1:1473
anyai serve --port 8080
anyai serve --host 0.0.0.0 --bearer-token sk-…   # expose to LAN with auth
anyai serve --no-ollama                 # don't auto-start ollama
```

**Endpoints**

| Path | Behaviour |
|------|----------|
| `POST /v1/chat/completions` | OpenAI chat. Streams when `stream: true`. |
| `POST /v1/completions`      | Legacy completions. |
| `POST /v1/embeddings`       | Proxied to Ollama embeddings. |
| `GET  /v1/models`           | Virtual model IDs + raw pulled tags. |
| `GET  /healthz`             | 200 if Ollama reachable, else 503. |
| `POST /v1/anyai/preload`    | Body `{"modes":[…], "track":bool}`; SSE progress. |
| `GET  /v1/anyai/status`     | Current resolved tag per tracked mode. |

**Virtual model IDs** resolve at request-time to the best model for your hardware:

| Model ID         | Resolves to (example) |
|------------------|----------------------|
| `anyai-text`     | `qwen2.5:14b` |
| `anyai-vision`   | `qwen2.5vl:7b` |
| `anyai-code`     | `qwen2.5-coder:14b` |
| `anyai-transcribe` | `whisper:large` |

When the active provider's manifest changes, the underlying tag swaps automatically; the virtual model ID stays the same so external clients don't need updating. The response includes an `X-AnyAI-Resolved-Model` header showing which tag actually served the request.

**First-request behaviour**: if a virtual model's underlying tag isn't pulled yet, the server returns `503 + Retry-After: 10` with a JSON body describing pull progress. Pass `?wait=true` (or header `X-AnyAI-Wait: true`) to hold the connection and stream pull progress as SSE keep-alives instead.

**Use from Cursor / Continue / Aider** — point at:

```
Base URL: http://127.0.0.1:1473/v1
Model:    anyai-code
API key:  (any non-empty string, ignored unless --bearer-token set)
```

**Example**

```bash
curl -s http://127.0.0.1:1473/v1/chat/completions \
  -H 'content-type: application/json' \
  -d '{
    "model": "anyai-text",
    "messages": [{"role":"user","content":"hello"}],
    "stream": false
  }'
```

The GUI also runs the API server on the same port by default — you can use AnyAI as both a desktop chat app and a local OpenAI endpoint at the same time. Disable via `config.json` (`api.enabled: false`).

---

## GUI

Launch the GUI by running `anyai` with no arguments, or open the application bundle.

**First run** (auto-transitions, no user choices required):

```
"Detecting hardware…"
"Best model for your system: Qwen2.5 14B — Downloading 8.9 GB…"
  [progress bar]
→ chat opens automatically
```

**Main window:**

```
┌─────────────────────────────────────────────────────┐
│ ● qwen2.5:14b                            ⊞ Models   │  ← status bar
├─────────────────────────────────────────────────────┤
│                                                     │
│              (messages appear here)                  │
│                                                     │
├─────────────────────────────────────────────────────┤
│ [Text]  [Vision]  [Code]  [Transcribe]              │  ← mode bar
├─────────────────────────────────────────────────────┤
│  Message…                              [Send]        │
└─────────────────────────────────────────────────────┘
```

- **Status bar** — click the model name to open the provider panel; click "⊞ Models" for the model status panel
- **Mode bar** — switch modes; model hot-swaps without restarting the server
- No settings screen. No preferences. No model picker. Everything just works.

**Provider panel** (click the model pill in the status bar):

- Lists saved providers grouped by domain
- Click any provider to switch (model hot-swaps immediately)
- "Add provider" — paste a URL and name
- Sources tab — browse source catalogs and add providers from them

**Model status panel** (click "⊞ Models"):

- Every pulled model: size, which providers recommend it, age if unrecommended
- Pin icon to keep a model (exempt from cleanup)
- Per-mode override: click to pick a specific model from any provider's full tier list
- "Clean up" — evicts all unrecommended, non-pinned, non-override models

---

## Provider system

### What is a Provider?

A provider is a **URL** that returns a JSON manifest. The manifest maps hardware profiles to model recommendations across four modes. AnyAI fetches it, caches it (with a publisher-defined TTL), and uses it to pick the best model for your machine.

Providers are:
- Saved by name in your local config
- One of them is "active" at any time
- Switched via CLI or GUI without restarting anything

### What is a Source?

A source is a **URL** that returns a catalog of providers — a list of `{ name, url, description }` entries. Sources let publishers ship multiple providers under one URL that users add once.

```
User adds Source URL
  └─ fetches catalog of providers
       ├─ "DeepSeek V3"    → https://deepseek.com/anyai/v3.json
       ├─ "DeepSeek R1"    → https://deepseek.com/anyai/r1.json
       └─ "DeepSeek Coder" → https://deepseek.com/anyai/coder.json
User browses and adds individual providers from the catalog.
```

Sources are cached locally with a publisher-defined TTL (default 24h). When stale, AnyAI silently re-fetches in the background on next startup.

### Publishing your own

**To publish a provider** — host any static JSON file with the [manifest format](#manifest-format). That's it. One file, any static host (GitHub Pages, S3, a Cloudflare Worker, your company intranet).

```bash
# Your team adds it once:
anyai providers add https://ai.yourcompany.com/anyai-manifest.json --name "Company LLM"
anyai providers use "Company LLM"
```

**To publish a source** — host a JSON file listing your providers:

```json
{
  "name": "Your Org",
  "description": "Company AI providers",
  "ttl_minutes": 1440,
  "providers": [
    { "name": "Company LLM",  "url": "https://ai.yourco.com/manifest.json",       "description": "General chat" },
    { "name": "Company Code", "url": "https://ai.yourco.com/code-manifest.json",  "description": "Code assistant" }
  ]
}
```

```bash
anyai sources add https://ai.yourco.com/anyai-sources.json --name "Your Org"
anyai sources list "Your Org"   # browse available providers
```

**No account, no API key, no SDK.** One static JSON file is the entire participation contract.

---

## Manifest format

A manifest is the JSON file a provider URL serves. It maps hardware tiers to models.

```jsonc
{
  "name": "My Provider",         // display name
  "version": "1",               // schema version
  "ttl_minutes": 360,           // how long AnyAI caches this before re-fetching (default: 360)
  "default_mode": "text",       // fallback if requested mode isn't in manifest

  "modes": {
    "text": {
      "label": "Text",          // display label in UI
      "tiers": [
        // Tiers are evaluated top-to-bottom. First match wins.
        // A tier matches if vram_gb >= min_vram_gb OR ram_gb >= min_ram_gb.
        { "min_vram_gb": 24, "min_ram_gb": 48, "model": "qwen2.5:32b",  "fallback": "qwen2.5:14b" },
        { "min_vram_gb": 12, "min_ram_gb": 24, "model": "qwen2.5:14b",  "fallback": "qwen2.5:7b"  },
        { "min_vram_gb": 6,  "min_ram_gb": 12, "model": "qwen2.5:7b",   "fallback": "qwen2.5:3b"  },
        { "min_vram_gb": 3,  "min_ram_gb": 6,  "model": "qwen2.5:3b",   "fallback": "tinyllama"   },
        { "min_vram_gb": 0,  "min_ram_gb": 0,  "model": "tinyllama",    "fallback": "tinyllama"   }
        // Always include a zero-threshold catch-all as the last tier.
      ]
    },

    "vision": {
      "label": "Vision",
      "tiers": [
        { "min_vram_gb": 12, "min_ram_gb": 16, "model": "qwen2.5vl:7b", "fallback": "llava:7b" },
        { "min_vram_gb": 0,  "min_ram_gb": 0,  "model": "llava:7b",     "fallback": "llava:7b" }
      ]
    },

    "code": {
      "label": "Code",
      "tiers": [
        { "min_vram_gb": 12, "min_ram_gb": 24, "model": "qwen2.5-coder:14b", "fallback": "qwen2.5-coder:7b" },
        { "min_vram_gb": 0,  "min_ram_gb": 0,  "model": "qwen2.5-coder:3b",  "fallback": "qwen2.5-coder:3b" }
      ]
    },

    "transcribe": {
      "label": "Transcribe",
      "input": "audio",         // signals AnyAI to activate mic input
      "tiers": [
        { "min_vram_gb": 8, "min_ram_gb": 16, "model": "whisper:large",  "fallback": "whisper:medium" },
        { "min_vram_gb": 0, "min_ram_gb": 0,  "model": "whisper:base",   "fallback": "whisper:base"   }
      ]
    }
  }
}
```

**Rules:**
- `min_vram_gb` and `min_ram_gb` are checked with OR: either threshold qualifies
- Tiers are walked top-to-bottom; first match wins
- The last tier should always have `min_vram_gb: 0, min_ram_gb: 0` as a catch-all
- `fallback` is tried if the primary model fails to pull
- Unknown modes and unknown fields are ignored — manifests are forward-compatible
- `ttl_minutes` controls how long AnyAI caches this manifest before re-fetching (default 360 = 6h)

**Model tags** are standard Ollama tags (e.g. `qwen2.5:14b`, `llama3.2:3b`). Any model in the [Ollama library](https://ollama.com/library) works.

---

## Model lifecycle & cleanup

AnyAI manages disk automatically. Models accumulate as you switch providers — this system ensures they don't pile up forever.

### Three TTL layers

There are three distinct concepts. They are independent and do not interact.

| Layer | What has a TTL | Who sets it | What happens when it expires |
|-------|---------------|-------------|------------------------------|
| **Source list** | Cached catalog of providers from a source URL | Source publisher (in `ttl_minutes` field) | AnyAI silently re-fetches the catalog on next startup |
| **Model list** | Cached manifest from a provider URL | Provider publisher (in `ttl_minutes` field) | AnyAI silently re-fetches the manifest on next startup |
| **Model cleanup** | Pulled Ollama models that are no longer recommended | User (in config, default 1 day) | Model is deleted from disk |

The first two TTLs are about **freshness of remote data**. The third is about **disk cleanup**.

### Model eviction

A pulled model is **in use** if `resolveModel(your_hardware)` would return its tag for **any** provider across **all** your active sources and saved providers. AnyAI computes this set on every startup.

When a model drops out of every provider's recommendation set, a clock starts. Once it has been unrecommended for longer than your cleanup threshold (default: 1 day), it is deleted.

```
Startup:
  For each pulled model:
    recommended_by = [ providers whose manifests recommend this model for my hardware ]
    if recommended_by is empty:
      time_since_recommended = now - last_recommended_at
      if time_since_recommended > model_cleanup_days:
        delete model
```

Cleanup triggers on:
1. **App startup** — always
2. **Provider or source added/removed** — recomputes recommendation set immediately
3. **Pre-pull disk check** — if disk is tight, evicts unrecommended models before pulling

**No model is ever deleted silently the moment you remove a provider.** The clock starts when it becomes unrecommended, and you have a full day (or whatever you set) before it's gone.

### Keeping and overriding models

**Keep (pin)** — marks a model as permanent. Kept models are never auto-evicted.

```bash
anyai models keep qwen2.5:32b      # pin
anyai models unkeep qwen2.5:32b    # unpin
```

Kept models appear with a 📌 badge in the GUI.

**Mode override** — forces a specific model for a mode, regardless of what any provider recommends. Override models are implicitly kept.

```bash
anyai models override code qwen2.5-coder:14b    # always use this for Code mode
anyai models override code --clear              # revert to provider recommendation
```

In the GUI: open the Models panel → click "change" next to any mode → pick from all models any of your providers mentions.

**Cleanup order:**
1. Evict: unrecommended + not kept + not an override + older than threshold
2. Never touch: kept, override, or still-recommended models

**Prune now** (ignores age threshold):
```bash
anyai models prune    # immediately evict everything that qualifies
```

---

## Import & Export

The full config — sources, providers — is a plain JSON object. Share it however you want.

### Export

```bash
# As JSON to stdout (pipe to a file, gist, etc.)
anyai export > my-anyai-config.json

# As a self-contained URL (base64-encoded, no server required)
anyai export --url
# anyai:import:eyJzb3VyY2VzIjpbXSwicHJvdmlkZXJzIjpbXX0

# Partial exports
anyai export --sources-only
anyai export --providers-only
```

### Import

```bash
# From a file
anyai import ./my-anyai-config.json

# From a URL
anyai import https://gist.githubusercontent.com/you/abc123/raw/config.json

# From a share URL (the anyai:import:... format from --url)
anyai import anyai:import:eyJzb3VyY2VzIjpbXSwicHJvdmlkZXJzIjpbXX0
```

Import is **always additive**. Existing entries (matched by name) are never overwritten. If you import a config that has a provider called "DeepSeek R1" and you already have one, yours wins.

The GUI's provider panel also has an import field — paste any of the above formats directly.

---

## Scriptability

Every command supports `--json` for machine-readable output and `--quiet` to suppress all non-JSON prose.

**Exit codes:**
| Code | Meaning |
|------|---------|
| `0` | Success |
| `1` | User error (bad arguments, missing required config) |
| `2` | Network or I/O error |
| `3` | Not found (provider, source, or model doesn't exist) |
| `4` | Resource conflict (e.g. removing the active provider) |

**AnyAI is configurable by a local AI.** Because the CLI is fully scriptable and `--json` outputs are stable, a running model can be asked to set up or reconfigure AnyAI:

```bash
# Discover what's available
anyai sources list DeepSeek --json | jq '.providers[].name'

# Add a provider from a source and activate it
anyai providers add --from DeepSeek "DeepSeek R1"
anyai providers use "DeepSeek R1"

# Confirm
anyai status --json | jq '{model, provider: .active_provider, hardware}'

# Run in code mode
anyai run --mode code --quiet
```

Operations are **idempotent**: `anyai providers add` with an existing name updates the URL. `anyai providers use` is always safe to re-run. Scripts don't need to check first.

---

## Config files

AnyAI manages all its config files. You never need to open or edit them — use the CLI or GUI instead. They are documented here for transparency.

**Location:** `~/.anyai/`

```
~/.anyai/
├── config.json          # active provider, mode, cleanup settings, sources, providers
└── cache/
    ├── sources/         # cached source catalogs  (<hash>.json + fetched_at)
    ├── manifests/       # cached provider manifests (<hash>.json + fetched_at)
    └── model-status.json   # computed recommended-by set for all pulled models
```

### `~/.anyai/config.json`

```jsonc
{
  "active_provider": "AnyAI Default",
  "active_mode": "text",
  "model_cleanup_days": 1,          // how long before unrecommended models are evicted
  "kept_models": ["qwen2.5:32b"],   // pinned; never evicted
  "mode_overrides": {
    "code": "qwen2.5-coder:14b",    // force this model for Code mode
    "transcribe": null              // null = use provider recommendation
  },
  "sources": [
    { "name": "AnyAI",    "url": "https://anyai.run/sources/index.json" },
    { "name": "DeepSeek", "url": "https://deepseek.com/anyai/sources.json" }
  ],
  "providers": [
    { "name": "AnyAI Default", "url": "https://anyai.run/manifest/default.json", "source": "AnyAI" },
    { "name": "DeepSeek R1",   "url": "https://deepseek.com/anyai/r1.json",       "source": "DeepSeek" },
    { "name": "Local Dev",     "url": "https://ai.internal/manifest.json",        "source": null }
  ]
}
```

`source: null` = added directly by URL, not discovered via a source catalog.

---

## Building from source

### Prerequisites

**All platforms:**
- [Rust](https://rustup.rs) 1.88+
- [Node.js](https://nodejs.org) 18+
- [pnpm](https://pnpm.io) 8+
- [Tauri CLI v2](https://tauri.app): `cargo install tauri-cli`

**Linux only:**
```bash
sudo apt install libgtk-3-dev libwebkit2gtk-4.1-dev
```

**macOS only:**
- Xcode Command Line Tools: `xcode-select --install`

### Build

```bash
git clone https://github.com/mrjeeves/AnyAI
cd AnyAI
pnpm install
pnpm tauri build       # production bundle
pnpm tauri dev         # dev mode (hot reload)
```

**Type-check only:**
```bash
pnpm check             # TypeScript + Svelte
cargo check            # Rust (from src-tauri/)
```

### Project layout

```
AnyAI/
├── src-tauri/
│   ├── src/
│   │   ├── main.rs        # Tauri commands + CLI entry point + app setup
│   │   ├── hardware.rs    # GPU/RAM/disk detection (nvidia-smi, rocm-smi, sysctl, /proc)
│   │   ├── ollama.rs      # install, serve, pull (with progress events), stop, list, delete
│   │   └── cli.rs         # all CLI subcommands (run, status, models, sources, providers, import, export)
│   ├── tauri.conf.json
│   └── Cargo.toml
├── src/
│   ├── types.ts           # shared TypeScript types
│   ├── config.ts          # read/write ~/.anyai/config.json
│   ├── manifest.ts        # fetch + TTL-cache manifests, resolveModel(), allRecommendedModels()
│   ├── sources.ts         # source CRUD + TTL-cached catalog fetching
│   ├── providers.ts       # provider CRUD, active provider, getAllManifests()
│   ├── model-lifecycle.ts # recommended-by computation, keep/override, eviction
│   ├── import-export.ts   # importFromUrl(), exportBundle(), exportAsUrl()
│   └── ui/
│       ├── App.svelte       # root: loading → first-run → chat state machine
│       ├── FirstRun.svelte  # install Ollama → pull model → transition to chat
│       ├── Chat.svelte      # chat messages, Ollama /api/chat, mode switching
│       ├── StatusBar.svelte # model pill, provider switcher access, models panel access
│       ├── ModeBar.svelte   # [Text] [Vision] [Code] [Transcribe]
│       ├── ProviderPanel.svelte   # provider list + add + source browser
│       └── ModelStatus.svelte    # model list + keep/override + prune
├── manifests/
│   └── default.json       # bundled fallback manifest (used when offline)
└── providers/
    ├── preset-sources.json # bundled starter sources (replace to repackage)
    └── preset.json        # bundled starter providers (replace to repackage)
```

---

## Repackaging for your org

AnyAI is designed to be repackaged. You don't need to fork the code — just swap two JSON files and rebuild.

**`providers/preset-sources.json`** — the sources your users start with:
```json
[
  { "name": "Your Org", "url": "https://ai.yourco.com/anyai-sources.json" }
]
```

**`providers/preset.json`** — the providers pre-loaded on first run:
```json
[
  { "name": "Company LLM",  "url": "https://ai.yourco.com/manifest.json",      "source": "Your Org" },
  { "name": "Company Code", "url": "https://ai.yourco.com/code-manifest.json", "source": "Your Org" }
]
```

On first launch, AnyAI merges these into `~/.anyai/config.json`. Existing entries (by name) are never overwritten, so users who've customised their config are safe. The defaults just appear in their list.

Users can still add their own providers and sources on top. Company-provided entries have no special privilege — they're just pre-loaded defaults.

---

## Architecture

```
┌───────────────────────────────────────────────────────────────┐
│  GUI (Svelte 5)              CLI (Rust)                        │
│  App.svelte                  cli.rs                           │
│  Chat / FirstRun / Panels    run / status / models / ...      │
└─────────────────┬─────────────────────┬───────────────────────┘
                  │ Tauri invoke        │ direct call
┌─────────────────▼─────────────────────▼───────────────────────┐
│  TypeScript layer                                              │
│  manifest.ts   — fetch + cache manifest, resolveModel()       │
│  providers.ts  — active provider, getAllManifests()            │
│  sources.ts    — source CRUD, fetch + cache catalogs          │
│  model-lifecycle.ts — recommended-by, eviction, keep/override │
│  import-export.ts   — URL-encoded config bundles              │
│  config.ts     — ~/.anyai/config.json read/write              │
└─────────────────┬─────────────────────────────────────────────┘
                  │ Tauri commands
┌─────────────────▼───────────────────────────────────────────┐
│  Rust layer (src-tauri/)                                      │
│  hardware.rs  — nvidia-smi / rocm-smi / sysctl / /proc       │
│  ollama.rs    — manage ollama serve as child process          │
│  main.rs      — Tauri command handlers + app setup            │
└─────────────────┬───────────────────────────────────────────┘
                  │ subprocess / HTTP
              ┌───▼────┐     ┌──────────────────────┐
              │ Ollama │ ←── │ Ollama model registry │
              │ serve  │     │ (wraps HuggingFace)   │
              └────────┘     └──────────────────────┘
```

**Technology choices:**
| Concern | Choice | Reason |
|---------|--------|--------|
| Model backend | Ollama | Handles pull/serve/quantization/GPU routing. ~50 MB binary. |
| App framework | Tauri v2 | Rust shell + web frontend. Small binary, cross-platform, native OS APIs. |
| Frontend | Svelte 5 (runes) | Minimal bundle, no virtual DOM overhead, no React. |
| Core logic | TypeScript | Shared between GUI and CLI-adjacent code, easier to iterate than Rust for business logic. |
| Hardware detection | Rust | Direct process invocation without Node.js startup overhead. |

**Design principles:**
- Zero config for the user — hardware detection → model selection → pull → chat with no choices required
- The URL *is* the configuration — share a provider URL, share a complete AI setup
- Disk managed automatically — models clean up when no longer recommended
- Fully scriptable — every command has stable `--json` output and exit codes
- No accounts, no telemetry, no vendor lock-in — everything is a static JSON file on a URL you control

---

## License

MIT — see [LICENSE](LICENSE).
