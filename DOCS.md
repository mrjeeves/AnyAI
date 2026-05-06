# AnyAI Reference

Full reference manual for AnyAI. For a one-page overview and quick start, see [README.md](README.md). For internals, see [ARCHITECTURE.md](ARCHITECTURE.md).

## Contents

- [How it works](#how-it-works)
- [Installation](#installation)
- [API server](#api-server)
- [CLI reference](#cli-reference)
  - [Run / Chat](#run--chat)
  - [Status](#status)
  - [Models](#models)
  - [Providers](#providers)
  - [Sources](#sources)
  - [Preload](#preload)
  - [Import & Export](#import--export)
  - [Update](#update)
- [GUI](#gui)
- [Provider system](#provider-system)
- [Manifest format](#manifest-format)
- [Imports & merged catalogs](#imports--merged-catalogs)
- [Auto-update](#auto-update)
- [Model lifecycle & cleanup](#model-lifecycle--cleanup)
- [Scriptability](#scriptability)
- [Config files](#config-files)
- [Building from source](#building-from-source)
- [Repackaging for your org](#repackaging-for-your-org)

---

## How it works

```
anyai serve
  1. Detect GPU (nvidia-smi / rocm-smi / system_profiler) and RAM
  2. Fetch active provider's manifest (cached against the manifest's own TTL)
  3. Walk tiers top-to-bottom → pick best model this hardware can run
  4. Auto-install Ollama if missing
  5. Pull the resolved tag if not already on disk (with progress)
  6. Start ollama serve (managed child process)
  7. Listen on 127.0.0.1:1473, expose virtual model IDs
```

On every request: re-resolve, hot-swap if upstream changed, return. A 5-minute background watcher keeps tracked modes warm and checks for self-updates so the binary itself stays current with no user intervention. You never interact with Ollama directly; AnyAI manages it as a child process.

---

## Installation

### Requirements

- macOS 12+, Linux (x86_64 or aarch64), or Windows 10+
- Internet on first run (to pull the model — typically 3–15 GB)
- Ollama is auto-installed if missing

### One-line (macOS / Linux)

```bash
curl -fsSL https://anyai.run/install.sh | sh
```

The installer downloads a pre-built binary from the latest GitHub release. If no release matches your platform it falls back to building from source via `scripts/bootstrap.sh`. Pass `--run` to launch immediately:

```bash
curl -fsSL https://anyai.run/install.sh | sh -s -- --run
```

### From source

See [Building from source](#building-from-source).

---

## API server

`anyai serve` is the primary surface. It speaks OpenAI's wire format on `127.0.0.1:1473` so anything that already speaks that wire format — Cursor, Continue, Aider, custom agents, your own scripts — works against it as a drop-in provider.

```bash
anyai serve                                       # 127.0.0.1:1473
anyai serve --port 8080
anyai serve --host 0.0.0.0 --bearer-token sk-…    # expose to LAN with auth
anyai serve --no-ollama                           # don't auto-start ollama
```

### Endpoints

| Path | Behaviour |
|------|-----------|
| `POST /v1/chat/completions` | OpenAI chat. Streams when `stream: true`. |
| `POST /v1/completions`      | Legacy completions. |
| `POST /v1/embeddings`       | Proxied to Ollama embeddings. |
| `GET  /v1/models`           | Virtual model IDs + raw pulled tags. |
| `GET  /healthz`             | 200 if Ollama reachable, else 503. |
| `POST /v1/anyai/preload`    | Body `{"modes":[…], "track":bool}`; SSE progress. |
| `GET  /v1/anyai/status`     | Current resolved tag per tracked mode. |

### Virtual model IDs

These resolve at request-time to whatever tag your manifest currently selects for your hardware. Client-side configuration stays stable forever — the underlying tag swaps automatically when upstream JSON changes.

| Model ID            | Resolves to (example) |
|---------------------|-----------------------|
| `anyai-text`        | `qwen2.5:14b`         |
| `anyai-vision`      | `qwen2.5vl:7b`        |
| `anyai-code`        | `qwen2.5-coder:14b`   |
| `anyai-transcribe`  | `whisper:large`       |

Every response includes `X-AnyAI-Resolved-Model` so a client (or a log) can see what tag actually served the request.

If a virtual model's tag isn't pulled yet, the server returns `503` with `Retry-After: 10` and a JSON body describing pull progress. Pass `?wait=true` (or header `X-AnyAI-Wait: true`) to hold the connection and stream pull progress as SSE keep-alives instead.

The GUI also runs the API server on the same port by default — disable via `config.json` (`api.enabled: false`).

---

## CLI reference

| Command            | Purpose                                                |
|--------------------|--------------------------------------------------------|
| `anyai serve`      | Start the OpenAI-compatible HTTP server (primary)      |
| `anyai run`        | Chat in the terminal (auto-installs Ollama if missing) |
| `anyai preload`    | Pull and warm models for one or more modes             |
| `anyai status`     | Show provider, mode, hardware, ollama state            |
| `anyai stop`       | Stop the managed Ollama process                        |
| `anyai models`     | List / pin / override / prune pulled models            |
| `anyai providers`  | Manage provider URLs                                   |
| `anyai sources`    | Manage source catalog URLs                             |
| `anyai import`     | Import a config bundle                                 |
| `anyai export`     | Export the current config                              |
| `anyai update`     | Self-update: `status`, `check`, `apply`                |

### Run / Chat

```bash
anyai                          # open GUI (no args = launch window)
anyai run                      # chat in the terminal
anyai run --mode vision        # use a vision-capable model
anyai run --mode code          # use a code-optimized model
anyai run --mode transcribe    # mic → text via Whisper
anyai run --model qwen2.5:7b   # force a specific model
anyai run --profile https://example.com/manifest.json   # one-off manifest URL
```

`--profile` does not save — it's a single-run override for the active provider's manifest. To save a provider permanently, use `anyai providers add`.

| Mode | What it loads | Notes |
|------|---------------|-------|
| `text` | General-purpose LLM | Default |
| `vision` | Multimodal LLM | Accepts image paths in chat |
| `code` | Code-optimized LLM | Same chat interface |
| `transcribe` | Whisper | Activates mic; outputs transcribed text |

Exit chat: `Ctrl+C` or type `exit`.

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

### Models

```bash
anyai models                          # list pulled models with status
anyai models --json                   # machine-readable list

anyai models keep <model>             # pin — never auto-evict this model
anyai models unkeep <model>

anyai models override <mode> <model>  # force model for a mode
anyai models override <mode> --clear  # revert to provider recommendation

anyai models prune                    # evict all unrecommended, non-kept, non-override now
anyai models rm <model>               # force-remove (also clears keep/override)
```

Column meanings:

```
NAME                                SIZE   FLAGS
qwen2.5:14b                         8.2G   (recommended)
qwen2.5-coder:7b                    4.3G   kept override:code
deepseek-r1:14b                     8.9G   unrecommended 2d
```

- `recommended` — still selected by at least one active provider for this hardware
- `unrecommended Xd` — no active provider recommends it; will be evicted after the cleanup threshold
- `kept` — pinned by user, never auto-evicted
- `override:<mode>` — user-selected override for that mode; implicitly kept

### Providers

A **provider** is a named, saved URL pointing to a manifest. The active provider determines which model AnyAI recommends for your hardware.

```bash
anyai providers                          # list (* = active)
anyai providers add <url> --name <name>
anyai providers use <name>               # set as active (hot-swap)
anyai providers rm <name>                # cannot remove active
anyai providers show [name]              # fetch and display manifest
anyai providers reset                    # re-merge bundled preset list
```

```bash
anyai providers add https://deepseek.com/anyai/r1.json --name "DeepSeek R1"
anyai providers use "DeepSeek R1"
anyai run
```

`ANYAI_PROFILE=<url>` overrides the active provider at runtime without touching saved config — useful in CI, Docker, or automation:

```bash
ANYAI_PROFILE=https://example.com/minimal.json anyai run
```

### Sources

A **source** is a URL returning a catalog of providers — like a package repository. Browse a source's providers, then add individual ones to your saved list.

```bash
anyai sources                            # list saved sources
anyai sources add <url> --name <name>
anyai sources list <name>                # browse providers in a source
anyai sources rm <name>
anyai sources refresh [name]             # force re-fetch (clears TTL cache)
anyai sources reset                      # re-merge bundled presets
```

```bash
anyai sources add https://deepseek.com/anyai/sources.json --name DeepSeek
anyai sources list DeepSeek
#   DeepSeek V3    — Flagship general-purpose
#   DeepSeek R1    — Reasoning-focused
#   DeepSeek Coder — Code generation

anyai providers add --from DeepSeek "DeepSeek R1"
anyai providers use "DeepSeek R1"
```

When a source has [imports](#imports--merged-catalogs), `anyai sources list` shows entries from imported catalogs with a `[from <url>]` suffix.

### Preload

Pull and warm models for one or more modes ahead of time. Useful before going offline, before a demo, or during setup so the OpenAI server has everything warm.

```bash
anyai preload text                       # pull the text-mode model
anyai preload text vision code           # pull all three
anyai preload text vision --track        # also persist as tracked modes
anyai preload text --no-warm             # skip post-pull warm-up call
anyai preload text --json                # NDJSON event output
```

Tracked modes are kept current automatically: when a manifest update changes the recommended tag, AnyAI pulls the new one in the background and starts the eviction clock on the old one.

### Import & Export

The full config — sources, providers — is plain JSON. Share it however you want.

```bash
anyai export                       # JSON to stdout
anyai export --url                 # base64-encoded anyai:import:... URL
anyai export --sources-only
anyai export --providers-only

anyai import ./config.json
anyai import https://gist.../config.json
anyai import anyai:import:eyJzb3VyY2VzIjpbXSwicHJvdmlkZXJzIjpbXX0
```

Import is **always additive**. Existing entries (matched by name) are never overwritten. The GUI's provider panel also has an import field — paste any of the above formats directly.

### Update

```bash
anyai update              # alias for status
anyai update status       # current version, install kind, pending updates
anyai update check        # force a release check now
anyai update apply        # apply any staged update (or no-op)
```

See [Auto-update](#auto-update) for behaviour.

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
│              (messages appear here)                 │
│                                                     │
├─────────────────────────────────────────────────────┤
│ [Text]  [Vision]  [Code]  [Transcribe]              │  ← mode bar
├─────────────────────────────────────────────────────┤
│  Message…                              [Send]       │
└─────────────────────────────────────────────────────┘
```

- **Status bar** — click the model name to open the provider panel; click "⊞ Models" for the model status panel.
- **Mode bar** — switch modes; the model hot-swaps without restarting the server.
- No settings screen, no preferences, no model picker. Everything just works.

**Provider panel** (click the model pill):
- Lists saved providers grouped by domain. Click any provider to switch (model hot-swaps immediately).
- "Add provider" — paste a URL and name.
- Sources tab — browse source catalogs (with imports merged in) and add providers from them.

**Model status panel** (click "⊞ Models"):
- Every pulled model: size, which providers recommend it, age if unrecommended.
- Pin icon to keep a model (exempt from cleanup).
- Per-mode override: pick a specific model from any provider's full tier list.
- "Clean up" — evicts all unrecommended, non-pinned, non-override models.

---

## Provider system

### What is a Provider?

A **URL** that returns a JSON [manifest](#manifest-format). The manifest maps hardware profiles to model recommendations across four modes. AnyAI fetches it, caches it (against the publisher-defined TTL), and uses it to pick the best model for your machine.

Providers are saved by name in your local config; one is "active" at any time; switched via CLI or GUI without restarting anything.

### What is a Source?

A **URL** that returns a catalog of providers — a list of `{ name, url, description }` entries. Sources let publishers ship multiple providers under one URL that users add once.

```
User adds Source URL
  └─ fetches catalog
       ├─ "DeepSeek V3"    → https://deepseek.com/anyai/v3.json
       ├─ "DeepSeek R1"    → https://deepseek.com/anyai/r1.json
       └─ "DeepSeek Coder" → https://deepseek.com/anyai/coder.json
```

Sources are cached locally against the publisher's `ttl_minutes` (default 24h). When stale, AnyAI silently re-fetches in the background.

### Publishing your own

**Publish a provider** — host any static JSON file in the [manifest format](#manifest-format). One file, any static host (GitHub Pages, S3, a Cloudflare Worker, your company intranet).

```bash
anyai providers add https://ai.yourcompany.com/anyai-manifest.json --name "Company LLM"
anyai providers use "Company LLM"
```

**Publish a source** — host a JSON catalog:

```json
{
  "name": "Your Org",
  "description": "Company AI providers",
  "ttl_minutes": 1440,
  "providers": [
    { "name": "Company LLM",  "url": "https://ai.yourco.com/manifest.json",      "description": "General chat" },
    { "name": "Company Code", "url": "https://ai.yourco.com/code-manifest.json", "description": "Code assistant" }
  ]
}
```

```bash
anyai sources add https://ai.yourco.com/anyai-sources.json --name "Your Org"
anyai sources list "Your Org"
```

No account, no API key, no SDK. One static JSON file is the entire participation contract.

---

## Manifest format

```jsonc
{
  "name": "My Provider",         // display name
  "version": "1",
  "ttl_minutes": 360,            // how long AnyAI caches THIS file before re-fetching (default: 360).
                                 // Publisher's rate-limit signal — pick what fits your host.
  "default_mode": "text",

  "imports": [                   // optional: URLs to other manifests whose modes/tiers are merged in.
    "https://example.com/base-tiers.json"
  ],
                                 // Each imported manifest is fetched + cached against ITS OWN ttl_minutes.
                                 // Importing file wins on collision.

  "modes": {
    "text": {
      "label": "Text",
      "tiers": [
        // Tiers are walked top-to-bottom. First match wins.
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
      "input": "audio",
      "tiers": [
        { "min_vram_gb": 8, "min_ram_gb": 16, "model": "whisper:large",  "fallback": "whisper:medium" },
        { "min_vram_gb": 0, "min_ram_gb": 0,  "model": "whisper:base",   "fallback": "whisper:base"   }
      ]
    }
  }
}
```

**Rules:**
- `min_vram_gb` and `min_ram_gb` are checked with OR — either threshold qualifies.
- Tiers walked top-to-bottom; first match wins.
- Last tier should always be `min_vram_gb: 0, min_ram_gb: 0` as a catch-all.
- `fallback` is tried if the primary model fails to pull.
- Unknown modes and unknown fields are ignored — manifests are forward-compatible.
- `ttl_minutes` controls how long AnyAI caches **this file** before re-fetching (default 360). It is the publisher's rate-limit signal; AnyAI honours it.
- `imports` lets a manifest pull modes/tiers from other manifests; each imported file obeys its own TTL and is cached separately.

Model tags are standard Ollama tags (e.g. `qwen2.5:14b`, `llama3.2:3b`). Anything in the [Ollama library](https://ollama.com/library) works.

---

## Imports & merged catalogs

Both manifests and source catalogs accept an `imports` array of URLs to other files of the same kind:

```jsonc
// A source catalog at https://yourco.com/anyai/sources.json
{
  "name": "Your Org",
  "ttl_minutes": 1440,
  "imports": [
    "https://anyai.run/sources/index.json",         // pulls the AnyAI default catalog in
    "https://partner.com/anyai/sources.json"        // and a partner's catalog
  ],
  "providers": [
    { "name": "Company LLM", "url": "https://yourco.com/anyai/manifest.json" }
  ]
}
```

**Resolution rules:**

- Imports are walked recursively, depth-first, before the importing file's own entries are added.
- **Cycles are detected** by URL and broken silently — each URL appears once and only once in the merge.
- **Each imported file has its own `ttl_minutes` and its own cache entry.** A daily top-level catalog importing an hourly catalog will see the hourly catalog refresh hourly without bumping the top-level fetch.
- **Document order matters.** Imports are merged first, then the importing file's entries. On name collision, the importing file wins — the closer-to-you publisher gets the last word.
- Each entry is tagged in the cache with the URL it came from, so the GUI / `sources list` can show "from `partner.com`" alongside the entry name.

The same model applies to manifests: a small "tier scaffold" manifest can be imported by company-specific manifests that override only a few tiers.

The "centralized" aspect of an org's setup is that one root JSON file. The decentralized aspect is that nothing forces it to be hosted in one place — federate by importing.

---

## Auto-update

AnyAI is built to be installed once and never thought about again. A background updater runs alongside the watcher, checks the GitHub releases endpoint at most every `check_interval_hours` (default 6), and applies new releases according to `auto_apply`:

| Policy   | Behaviour                                                                       |
|----------|---------------------------------------------------------------------------------|
| `patch`  | (default) Auto-apply patch releases (`0.4.x → 0.4.y`); notify on minor / major. |
| `minor`  | Auto-apply patch and minor; notify on major.                                    |
| `all`    | Auto-apply everything.                                                          |
| `none`   | Just notify; never auto-apply.                                                  |

The updater stages the new binary at `~/.anyai/updates/<version>/`, verifies its SHA256 against the release's `SHA256SUMS` asset, and atomically swaps it over the running binary on the next process restart (Windows uses the standard rename-on-boot dance). For long-running `anyai serve` daemons under systemd / launchd / a Windows service, the swap takes effect after the next service restart.

**Package-manager installs are detected and skipped.** If AnyAI is installed via Homebrew, dpkg/apt, rpm, MSI, or Chocolatey, the updater logs a one-line note and lets the package manager handle versioning.

**Disable:**

```jsonc
// ~/.anyai/config.json (defaults shown)
"auto_update": {
  "enabled": true,
  "channel": "stable",          // "stable" | "beta"
  "auto_apply": "patch",        // "patch" | "minor" | "all" | "none"
  "check_interval_hours": 6
}
```

```bash
ANYAI_AUTOUPDATE=0 anyai serve   # one-shot opt-out
```

`anyai update status` shows the current version, install kind, and any pending update.

---

## Model lifecycle & cleanup

AnyAI manages disk automatically. Models accumulate as you switch providers; this system keeps the pile bounded.

### Three TTL layers

There are three distinct concepts. They are independent and do not interact.

| Layer | What has a TTL | Who sets it | What happens when it expires |
|-------|----------------|-------------|------------------------------|
| **Source list** | Cached catalog from a source URL | Source publisher (`ttl_minutes`) | AnyAI silently re-fetches |
| **Model list**  | Cached manifest from a provider URL | Provider publisher (`ttl_minutes`) | AnyAI silently re-fetches |
| **Model cleanup** | Pulled Ollama models no longer recommended | User (`model_cleanup_days`, default 1) | Model is deleted from disk |

The first two are about freshness of remote data; the third is about disk cleanup.

**TTLs are per-file.** When a file `imports` other files, each imported file is cached independently against its own `ttl_minutes`. Don't host on a free static CDN with a 5-minute TTL.

### Model eviction

A pulled model is **in use** if `resolveModel(your_hardware)` would return its tag for **any** provider across **all** your active sources and saved providers. AnyAI computes this set on every startup.

When a model drops out of every provider's recommendation set, a clock starts. Once it's been unrecommended for longer than `model_cleanup_days` (default 1), it's deleted.

```
Startup:
  For each pulled model:
    recommended_by = [providers whose manifests recommend this model for my hardware]
    if recommended_by is empty:
      time_since_recommended = now - last_recommended_at
      if time_since_recommended > model_cleanup_days:
        delete model
```

Cleanup triggers on:

1. **App startup** — always.
2. **Provider or source added/removed** — recomputes the recommendation set immediately.
3. **Pre-pull disk check** — if disk is tight, evicts unrecommended models before pulling.

No model is ever deleted silently the moment you remove a provider. The clock starts when it becomes unrecommended; you have a full day (or whatever you set) before it's gone.

### Keeping and overriding

**Keep (pin)** — never auto-evict.

```bash
anyai models keep qwen2.5:32b
anyai models unkeep qwen2.5:32b
```

**Mode override** — force a specific model for a mode regardless of provider recommendations. Override models are implicitly kept.

```bash
anyai models override code qwen2.5-coder:14b
anyai models override code --clear
```

In the GUI: open the Models panel → click "change" next to any mode → pick from any model any of your providers mentions.

**Cleanup order:**
1. Evict: unrecommended + not kept + not an override + older than threshold.
2. Never touch: kept, override, or still-recommended models.

```bash
anyai models prune    # immediately evict everything that qualifies (ignores age)
```

---

## Scriptability

Every command supports `--json` for machine-readable output and `--quiet` to suppress non-JSON prose.

**Exit codes:**

| Code | Meaning |
|------|---------|
| `0` | Success |
| `1` | User error (bad arguments, missing required config) |
| `2` | Network or I/O error |
| `3` | Not found (provider, source, or model doesn't exist) |
| `4` | Resource conflict (e.g. removing the active provider) |

Because the CLI is fully scriptable and `--json` outputs are stable, AnyAI can be set up or reconfigured by a running model:

```bash
anyai sources list DeepSeek --json | jq '.providers[].name'
anyai providers add --from DeepSeek "DeepSeek R1"
anyai providers use "DeepSeek R1"
anyai status --json | jq '{model, provider: .active_provider, hardware}'
anyai run --mode code --quiet
```

Operations are idempotent. `anyai providers add` with an existing name updates the URL. `anyai providers use` is always safe to re-run. Scripts don't need to check first.

---

## Config files

AnyAI manages all its config files. You shouldn't need to open them — use the CLI or GUI. They are documented here for transparency.

**Location:** `~/.anyai/`

```
~/.anyai/
├── config.json          # active provider, mode, cleanup, sources, providers, api, auto_update
├── watcher.lock         # PID; cooperative process lock
├── updates/             # staged self-update binaries (<version>/anyai)
└── cache/
    ├── sources/         # cached source catalogs    (<hash>.json + fetched_at, per-URL)
    ├── manifests/       # cached provider manifests (<hash>.json + fetched_at, per-URL)
    └── model-status.json   # computed recommended-by set for all pulled models
```

The `sources/` and `manifests/` caches store one entry per URL. When a file is reached via an `import`, it gets its own cache entry and obeys its own TTL.

### `~/.anyai/config.json`

```jsonc
{
  "active_provider": "AnyAI Default",
  "active_mode": "text",
  "model_cleanup_days": 1,
  "kept_models": ["qwen2.5:32b"],
  "mode_overrides": {
    "code": "qwen2.5-coder:14b",
    "transcribe": null
  },
  "api": {
    "enabled": true,
    "host": "127.0.0.1",
    "port": 1473,
    "cors_allow_all": false,
    "bearer_token": null
  },
  "auto_update": {
    "enabled": true,
    "channel": "stable",
    "auto_apply": "patch",
    "check_interval_hours": 6
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

**Linux:** `sudo apt install libgtk-3-dev libwebkit2gtk-4.1-dev`

**macOS:** Xcode Command Line Tools (`xcode-select --install`)

### Build

```bash
git clone https://github.com/mrjeeves/AnyAI
cd AnyAI
pnpm install
pnpm tauri build       # production bundle
pnpm tauri dev         # dev mode (hot-reload GUI)
```

`just setup` does the prereq install in one step (idempotent).

### Type-check only

```bash
pnpm check                                      # TypeScript + Svelte
cargo check --manifest-path src-tauri/Cargo.toml   # Rust
```

### Project layout

See [ARCHITECTURE.md](ARCHITECTURE.md) for module-by-module roles. High level:

```
src/             # TypeScript: config, manifest/source fetching with imports, lifecycle
src-tauri/src/   # Rust: API server, CLI, hardware/Ollama, resolver mirror, self-update
manifests/       # bundled fallback manifest
providers/       # bundled preset providers/sources (replace to repackage)
```

---

## Repackaging for your org

You don't need to fork the code — swap two JSON files and rebuild.

**`providers/preset-sources.json`** — sources users start with:

```json
[
  { "name": "Your Org", "url": "https://ai.yourco.com/anyai-sources.json" }
]
```

**`providers/preset.json`** — providers pre-loaded on first run:

```json
[
  { "name": "Company LLM",  "url": "https://ai.yourco.com/manifest.json",      "source": "Your Org" },
  { "name": "Company Code", "url": "https://ai.yourco.com/code-manifest.json", "source": "Your Org" }
]
```

On first launch, AnyAI merges these into `~/.anyai/config.json`. Existing entries (by name) are never overwritten, so users who've customised their config are safe; defaults just appear in their list.

Users can still add their own providers and sources on top. Company-provided entries have no special privilege — they're just pre-loaded defaults.
