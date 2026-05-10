# MyOwnLLM

> A local API surface for local AI. Self-host the JSON, set it, forget it.

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Platforms](https://img.shields.io/badge/platforms-macOS_·_Linux_·_Windows_·_Pi_5-2ea44f.svg)](DOCS.md#installation)
[![OpenAI-compatible](https://img.shields.io/badge/OpenAI-compatible-10a37f.svg)](DOCS.md#api-server)

This is the entire README. The rest is in [DOCS.md](DOCS.md), because it turns out a tool that picks a model, downloads it, hosts it, talks four wire formats, ships a desktop app, transcribes meetings, and updates itself can't really be explained in a README. Sorry.

## Install

**macOS / Linux**

```bash
curl -fsSL https://raw.githubusercontent.com/mrjeeves/MyOwnLLM/main/scripts/install.sh | sh
```

**Windows**

```powershell
irm https://raw.githubusercontent.com/mrjeeves/MyOwnLLM/main/scripts/install.ps1 | iex
```

Run `myownllm` to open the GUI. That's it for setup.

> Power users: `myownllm serve` starts the headless API server, and `myownllm run` opens a terminal chat. See the [CLI](#cli) section below.

## Features

**API**
- OpenAI-compatible HTTP API on `127.0.0.1:1473` — drop into Cursor / Continue / Aider / Cline / Zed / Open WebUI / LibreChat / opencode / OpenClaw / OpenClaude / anything that speaks OpenAI
- Also speaks the Ollama wire format (port 11434) and Anthropic's wire format, so clients that only speak those just work
- Virtual model IDs: `myownllm-text`, `myownllm-vision`, `myownllm-code`, `myownllm-transcribe` — one stable name per mode, the actual model auto-resolves to the best tag for your hardware
- Streaming, tool use, vision input, JSON mode — passes through whatever the underlying model supports

**Models**
- Static JSON manifest decides which model runs where — your machine, your team's machine, a publisher you trust
- JSON `import`s let an org or community compose merged catalogs without coordinating servers
- Automatic hardware-tier resolution — same manifest gives a Pi 5 the right tag and a 4090 the right tag
- Default manifest ships Gemma 4's edge variants (`e2b` / `e4b`) — agentic, multimodal, Apache-2.0, ~7.6 tok/s on a Pi 5
- Per-mode TTL eviction so disks don't fill up with models you tried once
- Background pre-pull of mode-relevant models so a switch into transcribe / vision "just works"

**Desktop GUI** (Tauri + Svelte 5)
- Two singleton slots: one chat-model, one transcription — pause/stop controls live on the mode buttons themselves
- Conversation sidebar with folders, drag-to-organise, rename in place
- Live transcription with whisper.cpp, mic-pause-keeps-draining-the-backlog
- **Talking Points** — continuously summarises a live transcript into a bullet list, claims the chat-model slot while running
- Auto-titled conversations, persistent across sessions, recoverable after a crash
- LAN remote: open the GUI from your phone on the same network, single-user lock with kick-and-hide

**Operations**
- Self-updating binary — checks GitHub on launch, stages quietly, applies on next start
- `myownllm status` shows provider, hardware tier, ollama state, update status in one screen
- Graceful degradation: no network, no problem — last good manifest is cached and kept
- Crash-resilient transcription buffer — chunks land on disk before whisper sees them, so a force-quit can be drained on next launch
- Scriptable end to end — every CLI subcommand returns parseable text or `--json`

**Providers & Families**
- Provider = a manifest URL. Family = a curated bundle inside it (e.g. `gemma4`, `qwen3-coder`).
- Switch providers, families, or per-mode overrides without touching client config
- Publish your own manifest — anyone pointing at the URL gets your picks
- See [DOCS.md › Provider system](DOCS.md#provider-system)

## CLI

```bash
myownllm                 # GUI
myownllm serve           # API server
myownllm run             # terminal chat
myownllm status          # provider, hardware, daemon, update
myownllm models          # what's pulled, what could be
myownllm families        # list / switch family
myownllm providers       # list / switch provider
myownllm update          # check / apply / configure self-update
```

## Build from source

```bash
git clone https://github.com/mrjeeves/MyOwnLLM && cd MyOwnLLM
just setup && just build
```

## Everything else

- [DOCS.md](DOCS.md) — full CLI, manifest format, client configs, provider/family system, auto-update, lifecycle, scripting, repackaging
- [ARCHITECTURE.md](ARCHITECTURE.md) — internals, modules, data flow

## License

MIT — see [LICENSE](LICENSE).
