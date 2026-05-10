# AnyAI Reference

Full reference manual for AnyAI. For a one-page overview and quick start, see [README.md](README.md). For internals, see [ARCHITECTURE.md](ARCHITECTURE.md).

## Contents

- [How it works](#how-it-works)
- [Installation](#installation)
- [API server](#api-server)
- [Connecting client apps](#connecting-client-apps)
- [CLI reference](#cli-reference)
  - [Run / Chat](#run--chat)
  - [Status](#status)
  - [Models](#models)
  - [Providers](#providers)
  - [Families](#families)
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

### Small systems (Raspberry Pi 4 / Pi 5)

AnyAI ships native `linux-aarch64` builds, so a 64-bit Raspberry Pi OS install is a one-liner. The hardware detector reads `/proc/device-tree/model` and `/proc/cpuinfo`, surfaces the board name (e.g. "Raspberry Pi 5 Model B") in the GUI and `anyai status`, and walks a CPU-friendly tier ladder so a 2 GB Pi 4 lands on `llama3.2:1b` while a 16 GB Pi 5 reaches `qwen3:8b`.

| Board                 | Default text model |
|-----------------------|--------------------|
| Pi 4 / Pi 5 — 2 GB    | `llama3.2:1b`      |
| Pi 4 / Pi 5 — 4 GB    | `llama3.2:3b`      |
| Pi 4 / Pi 5 — 8 GB    | `gemma3:4b`        |
| Pi 5 — 16 GB          | `qwen3:8b`         |

Notes:
- Use **64-bit Raspberry Pi OS** (Bookworm or newer). 32-bit (`armv7l`) is not a release target.
- Ollama installs through its official script on Pi 4/5 (aarch64). If that fails on a constrained image, run `anyai serve --no-ollama` and point Ollama at `127.0.0.1:11434` yourself.
- Override the picked model anytime: `anyai preload text --model llama3.2:1b --track`.

### One-line (macOS / Linux)

```bash
curl -fsSL https://raw.githubusercontent.com/mrjeeves/AnyAI/main/scripts/install.sh | sh
```

The installer downloads a pre-built binary from the latest GitHub release. If no release matches your platform it falls back to building from source via `scripts/bootstrap.sh`. Pass `--run` to launch immediately:

```bash
curl -fsSL https://raw.githubusercontent.com/mrjeeves/AnyAI/main/scripts/install.sh | sh -s -- --run
```

### One-line (Windows, PowerShell)

```powershell
irm https://raw.githubusercontent.com/mrjeeves/AnyAI/main/scripts/install.ps1 | iex
```

To launch immediately after install:

```powershell
iex "& { $(irm https://raw.githubusercontent.com/mrjeeves/AnyAI/main/scripts/install.ps1) } -Run"
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

## Connecting client apps

Most consumer-facing AI apps advertise a "local model" toggle and then leave you to figure out the actual fields. AnyAI speaks OpenAI's HTTP wire format on `127.0.0.1:1473`, so anything that supports a custom OpenAI base URL is a drop-in.

The universal answer to *"how do I point [client] at my local LLM?"* is always these four values:

| Field    | Value                                              |
|----------|----------------------------------------------------|
| Base URL | `http://127.0.0.1:1473/v1`                         |
| API key  | any non-empty string (e.g. `anyai`)                |
| Model    | `anyai-text` · `anyai-code` · `anyai-vision`       |
| Auth     | `Authorization: Bearer <key>` (clients add this for you) |

Below are exact, copy-pasteable configs for the apps that most commonly ship with the toggle but bury the fields. Start `anyai serve` first; everything else just points at the same URL.

### opencode

`opencode.json` (project root, or `~/.config/opencode/opencode.json`):

```json
{
  "$schema": "https://opencode.ai/config.json",
  "provider": {
    "anyai": {
      "npm": "@ai-sdk/openai-compatible",
      "name": "AnyAI (local)",
      "options": { "baseURL": "http://127.0.0.1:1473/v1" },
      "models": {
        "anyai-text":   { "name": "AnyAI (text)" },
        "anyai-code":   { "name": "AnyAI (code)" },
        "anyai-vision": { "name": "AnyAI (vision)" }
      }
    }
  }
}
```

Restart opencode, then `/models` → pick `AnyAI`.

### OpenClaw

In OpenClaw's Settings → Providers → Add → **OpenAI-compatible**:

```
Name:     AnyAI
Base URL: http://127.0.0.1:1473/v1
API key:  anyai
Model:    anyai-text
```

Equivalent CLI:

```bash
openclaw provider add anyai \
  --kind openai-compatible \
  --base-url http://127.0.0.1:1473/v1 \
  --api-key anyai \
  --model anyai-text
openclaw provider use anyai
```

### OpenClaude (Gitlawb / mjohnnywest / hatixntsoa forks)

OpenClaude reads its OpenAI-mode settings from environment variables:

```bash
export CLAUDE_CODE_USE_OPENAI=1
export OPENAI_BASE_URL=http://127.0.0.1:1473/v1
export OPENAI_API_KEY=anyai
export OPENAI_MODEL=anyai-code     # or anyai-text
openclaude
```

Drop those four lines in your shell rc and every OpenClaude session routes through AnyAI.

### Cursor

Settings → Models → enable **Override OpenAI Base URL**:

```
http://127.0.0.1:1473/v1
```

API key field: `anyai`. Add `anyai-text` / `anyai-code` to the **Model Names** list and click **Verify**. Cursor caches model lists — toggle the override off and on once after adding new model IDs.

### Continue.dev

`~/.continue/config.yaml`:

```yaml
models:
  - name: AnyAI (text)
    provider: openai
    model: anyai-text
    apiBase: http://127.0.0.1:1473/v1
    apiKey: anyai
  - name: AnyAI (code)
    provider: openai
    model: anyai-code
    apiBase: http://127.0.0.1:1473/v1
    apiKey: anyai
```

Legacy `config.json` form, if you haven't migrated yet:

```json
{ "title": "AnyAI", "provider": "openai",
  "model": "anyai-text",
  "apiBase": "http://127.0.0.1:1473/v1",
  "apiKey": "anyai" }
```

### Cline / Roo Code

⚙️ → API Provider → **OpenAI Compatible**:

```
Base URL:  http://127.0.0.1:1473/v1
API Key:   anyai
Model ID:  anyai-text
```

If the Base URL field is missing, update the extension — it was hidden briefly in some 3.x builds and has since been restored. CLI users: `cline provider configure openai-compatible`.

### Aider

Flags:

```bash
aider \
  --openai-api-base http://127.0.0.1:1473/v1 \
  --openai-api-key  anyai \
  --model           openai/anyai-code
```

Or `.env` in your project:

```
OPENAI_API_BASE=http://127.0.0.1:1473/v1
OPENAI_API_KEY=anyai
AIDER_MODEL=openai/anyai-code
```

The `openai/` prefix tells aider's LiteLLM layer to treat it as a generic OpenAI-compatible model and skip token-cost lookups.

### Zed

`~/.config/zed/settings.json`:

```json
{
  "language_models": {
    "openai_compatible": {
      "AnyAI": {
        "api_url": "http://127.0.0.1:1473/v1",
        "available_models": [
          { "name": "anyai-text", "display_name": "AnyAI (text)", "max_tokens": 32768 },
          { "name": "anyai-code", "display_name": "AnyAI (code)", "max_tokens": 32768 }
        ]
      }
    }
  }
}
```

Zed prompts for the API key on first use — type `anyai` (it's stored in the system keychain, not the JSON file).

### Open WebUI

Open WebUI's **Ollama** panel won't see AnyAI — AnyAI exposes OpenAI's wire format, not Ollama's native API. Use the **OpenAI** panel instead:

Settings → Connections → OpenAI API:

```
API Base URL: http://127.0.0.1:1473/v1
API Key:      anyai
```

### LibreChat

`librechat.yaml`:

```yaml
endpoints:
  custom:
    - name: AnyAI
      apiKey: anyai
      baseURL: http://127.0.0.1:1473/v1
      models:
        default: ["anyai-text", "anyai-code", "anyai-vision"]
        fetch: false
      titleConvo: true
      modelDisplayLabel: AnyAI
```

### Raw SDK use

```python
from openai import OpenAI
client = OpenAI(base_url="http://127.0.0.1:1473/v1", api_key="anyai")
client.chat.completions.create(
    model="anyai-text",
    messages=[{"role": "user", "content": "hi"}],
)
```

```js
import OpenAI from "openai";
const client = new OpenAI({
  baseURL: "http://127.0.0.1:1473/v1",
  apiKey:  "anyai",
});
```

### Clients that only speak Ollama (port 11434)

A handful of tools (Msty, some Obsidian plugins, older Open WebUI builds) only know how to talk to `http://localhost:11434`. AnyAI already runs Ollama as a managed child process, so those tools can hit `http://127.0.0.1:11434` directly and see exactly the models AnyAI pulled. Confirm with `anyai status`.

The trade-off: going through Ollama directly bypasses AnyAI's virtual model IDs (`anyai-text` etc.), so you'll be naming raw tags like `qwen3.5:9b`. Use AnyAI's URL whenever the client lets you.

### Clients that only speak Anthropic's wire format

If a tool only accepts `ANTHROPIC_BASE_URL` and the Anthropic Messages API (vanilla Claude Code, some Anthropic-only desktop apps), put an Anthropic→OpenAI shim in front of AnyAI — `claude-code-router`, `anthropic-proxy`, or LiteLLM in `--anthropic` mode all work. Point the shim's upstream at `http://127.0.0.1:1473/v1` and the client at the shim. AnyAI itself does not translate the Anthropic wire format.

### Troubleshooting

- **`Connection refused`** — `anyai serve` isn't running, or the client is on a different host. AnyAI binds `127.0.0.1` by default; for LAN access run `anyai serve --host 0.0.0.0 --bearer-token sk-…` and point the client at that host with the matching key.
- **`model not found: anyai-text`** — the client is hitting AnyAI but the manifest doesn't expose that mode. `curl http://127.0.0.1:1473/v1/models` lists what's actually available; `anyai status` shows which mode resolved.
- **`503 Retry-After`** — the model isn't pulled yet. Wait, or run `anyai preload text` ahead of time. Clients that respect `Retry-After` will recover on their own.
- **Client streams nothing then errors** — some clients send `stream: true` but don't handle SSE keep-alive frames. Disable streaming in the client, or pass `?wait=true` so AnyAI streams progress as keep-alives.

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
| `anyai families`   | Pick the model family inside the active provider      |
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
Family   : gemma4
Mode     : text
Ollama   : running
VRAM     : 12.0 GB (nvidia)
RAM      : 32.0 GB
Disk free: 118.3 GB

Families in AnyAI Default:
 * gemma4         Gemma 4
   qwen3          Qwen 3

Recommended models for this hardware:
  text       → gemma4:e4b
```

```jsonc
// --json output
{
  "active_provider": "AnyAI Default",
  "active_family": "gemma4",
  "active_mode": "text",
  "ollama_running": true,
  "hardware": { "vram_gb": 12.0, "ram_gb": 32.0, "disk_free_gb": 118.3, "gpu_type": "nvidia" },
  "families":  [ { "name": "gemma4", "label": "Gemma 4" }, { "name": "qwen3", "label": "Qwen 3" } ],
  "recommendations": { "text": "gemma4:e4b" }
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

### Families

A **family** is a named bundle of model versions inside a provider's manifest — e.g. `gemma4`, `qwen3`. Each family owns its own per-mode tier table; AnyAI resolves the active family's tiers against your hardware to pick a model. The default provider ships two families (`gemma4`, `qwen3`); `gemma4` is the default.

```bash
anyai families                           # list families in the active provider (* = active)
anyai families use <name>                # set as active (hot-swap)
anyai families show [name]               # print tiers for a family across all modes
anyai families --json                    # machine-readable list
```

```bash
anyai families
# Families in AnyAI Default:
#  * gemma4         Gemma 4
#    qwen3          Qwen 3

anyai families use qwen3
anyai families show qwen3
# qwen3  (Qwen 3)
#   Alibaba Qwen 3 — strong multilingual and reasoning performance at every size.
#   default mode: text
#
#   mode text:
#     ≥ 24 GB VRAM · ≥ 48 GB RAM   qwen3.6:35b
#     ≥ 16 GB VRAM · ≥ 32 GB RAM   qwen3.6:27b
#     ≥  8 GB VRAM · ≥ 16 GB RAM   qwen3.5:9b
#     ≥  4 GB VRAM · ≥  8 GB RAM   qwen3.5:1b
#     ≥  0 GB VRAM · ≥  0 GB RAM   qwen3.5:1b
```

`anyai providers use <name>` automatically resets the active family to the new manifest's `default_family` — no stale-name fallthrough.

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

The full config — providers — is plain JSON. Share it however you want.

```bash
anyai export                       # JSON to stdout
anyai export --url                 # base64-encoded anyai:import:... URL

anyai import ./config.json
anyai import https://gist.../config.json
anyai import anyai:import:eyJwcm92aWRlcnMiOltdfQ
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

**Settings panel** (click the model pill or the gear):
- **Family tab** — pick which family inside the active provider AnyAI uses for recommendations. Each family card shows its full tier list with the tier picked for your hardware highlighted, so you can see exactly what's running and why.
- **Providers tab** — list of saved providers. Click any provider to switch (model and family hot-swap immediately to the new manifest's default family).
- **Models tab** — every pulled model with its size, recommendation status, and pin/override controls.

**Model status panel** (click "⊞ Models"):
- Every pulled model: size, which providers recommend it, age if unrecommended.
- Pin icon to keep a model (exempt from cleanup).
- Per-mode override: pick a specific model from any provider's full tier list.
- "Clean up" — evicts all unrecommended, non-pinned, non-override models.

---

## Provider system

### What is a Provider?

A **URL** that returns a JSON [manifest](#manifest-format). The manifest publishes one or more **families** (e.g. `gemma4`, `qwen3`); each family carries its own per-mode tier table. AnyAI fetches the manifest, caches it against the publisher-defined TTL, and resolves `families[active_family].modes[active_mode]` against your hardware.

Providers are saved by name in your local config. One provider is active at a time; switching is hot — the model swaps without restarting anything.

### What is a Family?

A **family** is a model family inside a provider's manifest — Gemma 4, Qwen 3, etc. Picking the family is how the user picks "which model line do I want", letting AnyAI keep tiering by hardware. The user's choice of family is saved alongside the active provider; the resolver always walks `families[active_family].modes[active_mode].tiers`.

```
Provider "AnyAI Default"
  └─ default_family = "gemma4"
       families:
         ├─ gemma4   tiers: [31b → 26b → e4b → e2b]
         └─ qwen3    tiers: [35b → 27b → 9b  → 1b ]
```

Default ships with `gemma4` as the active family. Switching families is one CLI call (`anyai families use qwen3`) or one click in the GUI's Family tab.

### Publishing your own

Host any static JSON file in the [manifest format](#manifest-format). One file, any static host (GitHub Pages, S3, a Cloudflare Worker, your company intranet).

```bash
anyai providers add https://ai.yourcompany.com/anyai-manifest.json --name "Company LLM"
anyai providers use "Company LLM"
```

A single manifest can expose multiple families — that's how you ship "use our 8B for fast, 70B for slow" choices behind one URL. The user picks which family to use; AnyAI tiers within it. No account, no API key, no SDK. One static JSON file is the entire participation contract.

---

## Manifest format

```jsonc
{
  "name": "My Provider",         // display name
  "version": "4",
  "ttl_minutes": 360,            // how long AnyAI caches THIS file before re-fetching (default: 360).
                                 // Publisher's rate-limit signal — pick what fits your host.
  "default_family": "gemma4",    // family used until the user picks one

  "imports": [                   // optional: URLs to other manifests whose families are merged in.
    "https://example.com/base-families.json"
  ],
                                 // Each imported manifest is fetched + cached against ITS OWN ttl_minutes.
                                 // Importing file wins on family-key collision.

  "families": {
    "gemma4": {
      "label": "Gemma 4",
      "description": "Google Gemma 4 — versatile general-purpose models.",
      "default_mode": "text",
      "modes": {
        "text": {
          "label": "Text",
          "tiers": [
            // Tiers are walked top-to-bottom. First match wins.
            // A tier matches if vram_gb >= min_vram_gb OR ram_gb >= min_ram_gb.
            { "min_vram_gb": 20, "min_ram_gb": 40, "model": "gemma4:31b", "fallback": "gemma4:26b" },
            { "min_vram_gb": 14, "min_ram_gb": 28, "model": "gemma4:26b", "fallback": "gemma4:e4b" },
            { "min_vram_gb": 6,  "min_ram_gb": 12, "model": "gemma4:e4b", "fallback": "gemma4:e2b" },
            { "min_vram_gb": 0,  "min_ram_gb": 0,  "model": "gemma4:e2b", "fallback": "gemma4:e2b" }
            // Always include a zero-threshold catch-all as the last tier.
          ]
        }
      }
    },
    "qwen3": {
      "label": "Qwen 3",
      "default_mode": "text",
      "modes": {
        "text": {
          "label": "Text",
          "tiers": [
            { "min_vram_gb": 24, "min_ram_gb": 48, "model": "qwen3.6:35b", "fallback": "qwen3.6:27b" },
            { "min_vram_gb": 16, "min_ram_gb": 32, "model": "qwen3.6:27b", "fallback": "qwen3.5:9b"  },
            { "min_vram_gb": 8,  "min_ram_gb": 16, "model": "qwen3.5:9b",  "fallback": "qwen3.5:1b"  },
            { "min_vram_gb": 0,  "min_ram_gb": 0,  "model": "qwen3.5:1b",  "fallback": "qwen3.5:1b"  }
          ]
        }
      }
    }
  }
}
```

**Rules:**
- A family **must** define `default_mode` and at least one entry under `modes`.
- `min_vram_gb` and `min_ram_gb` are checked with OR — either threshold qualifies.
- Tiers walked top-to-bottom; first match wins.
- Last tier should always be `min_vram_gb: 0, min_ram_gb: 0` as a catch-all.
- `fallback` is tried if the primary model fails to pull.
- If the user's saved `active_family` doesn't exist in the manifest, the resolver falls back to `default_family`, then to the first family in document order.
- Unknown fields are ignored — manifests are forward-compatible within the schema version.
- `ttl_minutes` controls how long AnyAI caches **this file** before re-fetching (default 360). It is the publisher's rate-limit signal; AnyAI honours it.
- `imports` lets a manifest pull families from other manifests; each imported file obeys its own TTL and is cached separately. Family-key collisions favour the importing file.

Model tags are standard Ollama tags (e.g. `gemma4:e4b`, `qwen3.5:9b`). Anything in the [Ollama library](https://ollama.com/library) works.

---

## Imports & merged manifests

A manifest can `imports` other manifests by URL. Their families are merged into the importing file:

```jsonc
// A manifest at https://yourco.com/anyai/manifest.json
{
  "name": "Your Org",
  "version": "4",
  "ttl_minutes": 1440,
  "default_family": "gemma4",
  "imports": [
    "https://raw.githubusercontent.com/mrjeeves/AnyAI/main/manifests/default.json",     // pulls the AnyAI default families in
    "https://partner.com/anyai/manifest.json"       // and a partner's family
  ],
  "families": {
    "company-llm": { "label": "Company LLM", "default_mode": "text", "modes": { /* … */ } }
  }
}
```

**Resolution rules:**

- Imports are walked recursively, depth-first, before the importing file's own families are added.
- **Cycles are detected** by URL and broken silently — each URL appears once and only once in the merge.
- **Each imported file has its own `ttl_minutes` and its own cache entry.** A daily top-level manifest importing an hourly manifest will see the hourly one refresh hourly without bumping the top-level fetch.
- **Document order matters.** Imports are merged first, then the importing file's families. On family-key collision, the importing file wins — the closer-to-you publisher gets the last word.

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
| **Manifest** | Cached manifest from a provider URL | Provider publisher (`ttl_minutes`) | AnyAI silently re-fetches |
| **Imported manifest** | Cached imports of the active manifest | Each import's publisher (`ttl_minutes`) | AnyAI silently re-fetches that one file |
| **Model cleanup** | Pulled Ollama models no longer recommended | User (`model_cleanup_days`, default 1) | Model is deleted from disk |

The first two are about freshness of remote data; the third is about disk cleanup.

**TTLs are per-file.** When a file `imports` other files, each imported file is cached independently against its own `ttl_minutes`. Don't host on a free static CDN with a 5-minute TTL.

### Model eviction

A pulled model is **in use** if any saved provider's manifest mentions its tag in any family/mode/tier. AnyAI computes this set on every startup and after every provider/family change.

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
2. **Provider or family change** — recomputes the recommendation set immediately.
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
| `3` | Not found (provider, family, or model doesn't exist) |
| `4` | Resource conflict (e.g. removing the active provider) |

Because the CLI is fully scriptable and `--json` outputs are stable, AnyAI can be set up or reconfigured by a running model:

```bash
anyai providers add https://example.com/manifest.json --name "Example"
anyai providers use "Example"
anyai families --json | jq '.[].name'
anyai families use qwen3
anyai status --json | jq '{provider: .active_provider, family: .active_family, recommendations}'
anyai run --mode code --quiet
```

Operations are idempotent. `anyai providers add` with an existing name updates the URL. `anyai providers use` is always safe to re-run. Scripts don't need to check first.

---

## Config files

AnyAI manages all its config files. You shouldn't need to open them — use the CLI or GUI. They are documented here for transparency.

**Location:** `~/.anyai/`

```
~/.anyai/
├── config.json          # active provider, active family, mode, cleanup, providers, api, auto_update
├── watcher.lock         # PID; cooperative process lock
├── updates/             # staged self-update binaries (<version>/anyai)
└── cache/
    ├── manifests/       # cached provider manifests (<hash>.json + fetched_at, per-URL)
    └── model-status.json   # computed recommended-by set for all pulled models
```

The `manifests/` cache stores one entry per URL. When a manifest reached via an `import`, it gets its own cache entry and obeys its own TTL.

### `~/.anyai/config.json`

```jsonc
{
  "active_provider": "AnyAI Default",
  "active_family": "gemma4",
  "active_mode": "text",
  "model_cleanup_days": 1,
  "kept_models": ["qwen3.6:35b"],
  "mode_overrides": {
    "code": "qwen3.5:9b",
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
  "providers": [
    { "name": "AnyAI Default", "url": "https://raw.githubusercontent.com/mrjeeves/AnyAI/main/manifests/default.json" },
    { "name": "Local Dev",     "url": "https://ai.internal/manifest.json" }
  ]
}
```

---

## Building from source

### Prerequisites

**All platforms:**
- [Rust](https://rustup.rs) 1.88+
- [Node.js](https://nodejs.org) 18+
- [pnpm](https://pnpm.io) 8+
- [Tauri CLI v2](https://tauri.app): `cargo install tauri-cli`
- `cmake` — whisper-rs builds whisper.cpp from source for local transcription

**Linux:** `sudo apt install libgtk-3-dev libwebkit2gtk-4.1-dev libasound2-dev cmake`
(`libasound2-dev` is the ALSA dev headers cpal links against for mic capture.)

**macOS:** Xcode Command Line Tools (`xcode-select --install`) and `brew install cmake`

**Windows:** WebView2 (auto-installed by `bootstrap.ps1`) and CMake
(`winget install Kitware.CMake`)

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
src/             # TypeScript: config, manifest fetching with imports, lifecycle
src-tauri/src/   # Rust: API server, CLI, hardware/Ollama, resolver mirror, self-update
manifests/       # bundled fallback manifest (with families)
providers/       # bundled preset providers (replace to repackage)
```

---

## Repackaging for your org

You don't need to fork the code — swap one JSON file and rebuild.

**`providers/preset.json`** — providers pre-loaded on first run:

```json
[
  { "name": "Company LLM",  "url": "https://ai.yourco.com/manifest.json"      },
  { "name": "Company Code", "url": "https://ai.yourco.com/code-manifest.json" }
]
```

On first launch, AnyAI merges these into `~/.anyai/config.json`. Existing entries (by name) are never overwritten, so users who've customised their config are safe; defaults just appear in their list.

Users can still add their own providers on top. Company-provided entries have no special privilege — they're just pre-loaded defaults.
