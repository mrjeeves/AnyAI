<div align="center">

# MyOwnLLM

### A local API surface for local AI.<br>Self-host the JSON, set it, forget it.

[**myownllm.net**](https://myownllm.net) — installers, screenshots, the pitch

[Docs](DOCS.md) · [Architecture](ARCHITECTURE.md) · [Contributing](CONTRIBUTING.md) · [License](LICENSE)

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Platforms](https://img.shields.io/badge/platforms-macOS_·_Linux_·_Windows_·_Pi_5-2ea44f.svg)](DOCS.md#installation)
[![OpenAI-compatible](https://img.shields.io/badge/OpenAI-compatible-10a37f.svg)](DOCS.md#api-server)
[![Ollama-compatible](https://img.shields.io/badge/Ollama-compatible-ff7a59.svg)](DOCS.md#api-server)
[![Anthropic-compatible](https://img.shields.io/badge/Anthropic-compatible-d97757.svg)](DOCS.md#api-server)

</div>

---

A single binary turns whatever machine it lands on — a Pi 5, a laptop, a workstation — into a local OpenAI-compatible endpoint. A JSON manifest (yours, your team's, or one you trust) decides which model runs where. Clients keep speaking OpenAI; the hardware-shaped substitution happens underneath.

## Install

The fast path is [**myownllm.net**](https://myownllm.net) — signed installers for every platform.

Or one line in a shell:

```sh
# macOS / Linux
curl -fsSL https://raw.githubusercontent.com/mrjeeves/MyOwnLLM/main/scripts/install.sh | sh

# Windows
irm https://raw.githubusercontent.com/mrjeeves/MyOwnLLM/main/scripts/install.ps1 | iex
```

Then:

```sh
myownllm          # opens the GUI
myownllm serve    # headless API on :1473
myownllm run      # terminal chat
```

## Highlights

|   |   |
|---|---|
| **Three wire formats, one server** | OpenAI on `:1473`, plus Ollama and Anthropic. Point Cursor, Continue, Aider, Cline, Zed, Open WebUI, opencode, OpenClaw, OpenClaude or your own scripts at it and it just works. |
| **Virtual model IDs** | `myownllm-text`, `myownllm-vision`, `myownllm-code`, `myownllm-transcribe`. Stable names; the right tag for your hardware auto-resolves. |
| **Manifests, not config** | A JSON file at a URL is the source of truth. `imports` compose merged catalogs across publishers — no coordination required. |
| **Runs on a Pi 5** | Default manifest ships Gemma 4 edge variants (`e2b` / `e4b`), Apache-2.0, ~7.6 tok/s on a Pi 5. Same manifest gives a 4090 the 4090 tag. |
| **Live transcription** | Moonshine on Pi 5, Parakeet TDT 0.6B v3 elsewhere, ~1 s end-to-end. Opt-in speaker diarization via pyannote-seg-3.0. |
| **Talking Points** | Continuously summarises a live transcript into a bullet list while you talk. |
| **Desktop GUI** | Tauri + Svelte 5. Two singleton slots (chat-model, transcription) with conversation folders, in-place rename, crash-recoverable state. |
| **LAN remote** | Open the GUI from your phone on the same network. Single-user lock with kick-and-hide. |
| **Self-updating** | Stages quietly on launch, applies on next start. Last good manifest stays cached for offline runs. |
| **Scriptable end-to-end** | Every CLI subcommand returns parseable text or `--json`. |

## CLI

```sh
myownllm                 # GUI
myownllm serve           # API server
myownllm run             # terminal chat
myownllm status          # provider, hardware, daemon, update
myownllm models          # what's pulled, what could be
myownllm families        # list / switch family
myownllm providers       # list / switch provider
myownllm update          # check / apply / configure self-update
```

Full reference: [DOCS.md › CLI](DOCS.md#cli).

## Build from source

```sh
git clone https://github.com/mrjeeves/MyOwnLLM && cd MyOwnLLM
just setup && just build
```

Repo layout, dev loop, and commit style live in [CONTRIBUTING.md](CONTRIBUTING.md).

## More

- [**myownllm.net**](https://myownllm.net) — installers, screenshots, the pitch
- [DOCS.md](DOCS.md) — manifest format, client configs, provider/family system, auto-update, lifecycle, scripting, repackaging
- [ARCHITECTURE.md](ARCHITECTURE.md) — internals, modules, data flow
- [CONTRIBUTING.md](CONTRIBUTING.md) — setup, repo layout, commit style
- [LICENSE](LICENSE) — MIT
