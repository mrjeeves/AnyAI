<div align="center">

# MyOwnLLM

### Local LLMs and real-time transcription, on your machine, in one binary.<br>The piece every &ldquo;bring your own LLM&rdquo; agent assumes you've already built.

[**myownllm.net**](https://myownllm.net) — installers, screenshots, the pitch

[Docs](DOCS.md) · [Architecture](ARCHITECTURE.md) · [Contributing](CONTRIBUTING.md) · [License](LICENSE)

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Platforms](https://img.shields.io/badge/platforms-macOS_·_Linux_·_Windows_·_Pi_5-2ea44f.svg)](DOCS.md#installation)
[![OpenAI-compatible](https://img.shields.io/badge/OpenAI-compatible-10a37f.svg)](DOCS.md#api-server)
[![Ollama-compatible](https://img.shields.io/badge/Ollama-compatible-ff7a59.svg)](DOCS.md#api-server)
[![Anthropic-compatible](https://img.shields.io/badge/Anthropic-compatible-d97757.svg)](DOCS.md#api-server)

</div>

---

## Why this exists

You starred OpenClaw, or Continue, or Cline, or opencode, or Aider. You imagined a local AI that just works on your laptop. Then you got to the part that says *"point it at an LLM."* Which one? Where from? On what hardware? Bring your own.

In practice, "bring your own" quietly turns into "bring your own paid API key," and the bills add up faster than anyone budgeted for — every agent loop, every autocomplete burst, every transcript summary, charged by the token to a different vendor. The local-AI dream was supposed to cut that line, not refactor it.

MyOwnLLM is what people thought was in the box. One binary that resolves *the right model for this machine* against a JSON manifest, serves it on OpenAI / Ollama / Anthropic ports, and ships the on-device real-time transcription pipeline that the rest of the ecosystem hand-waves as a solved problem. After 20 years writing AI software it still took a week of yak-shaving to wire all this up by hand — so if that's the floor for someone who does this for a living, nobody else stands a chance. Hence this.

**Two solved paths, one binary:**

|   |   |
|---|---|
| **A local LLM endpoint that just works** | OpenAI-compatible HTTP on `127.0.0.1:1473` (also Ollama, also Anthropic), serving whichever model fits the machine — picked by a JSON manifest you, your team, or someone you trust controls. Cursor, Continue, Aider, Cline, Zed, Open WebUI, opencode, **OpenClaw**, OpenClaude, and your own scripts target it on day one. No metered tokens, no vendor lock-in. |
| **Real-time transcription that just works** | Mic-to-text in ~1 s on a Pi 5 (English) or 80–200 ms on capable hardware (25 languages), with optional speaker diarization that stays stable across the whole session and a Talking-Points summary that grows alongside the live transcript. No second daemon, no Python venv, no cloud round-trip. |

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

## Live transcription, on your machine

A first-class capture pipeline, not a sidebar feature. Mic in, segmented transcript out, with speakers attributed and a live summary growing alongside it — all on the same binary, all on-device.

- **Streaming ASR.** Moonshine Small on a Pi 5 (English, ~500 ms), Parakeet TDT 0.6B v3 on capable hardware (25 languages, 80–200 ms). Streaming-native: one segment per audio chunk, no 5-second minimum.
- **Speaker diarization.** Opt-in toggle. `pyannote-segmentation-3.0` plus a speaker embedder (`wespeaker-r34` on capable hardware, `campp-small` on the lower rung), with online agglomerative clustering on the Rust side — speaker IDs stay stable across the entire conversation, not just a single window. Click a speaker pill to rename them; the labels persist with the session.
- **Talking Points.** A continuous LLM loop summarises the live transcript into a growing bullet list while you talk. The list updates as the conversation evolves, is persisted with the session, and can be paused, resumed, or stopped from the mode bar. It claims the chat-model slot while running so it can use whichever local model your hardware tier picked for text.
- **Crash-resilient by design.** Audio chunks land on disk before the ASR backend sees them, so a force-quit can be drained on next launch. Transcripts, speaker labels, diarize state, and the talking-points list are all part of the conversation record.
- **One binary.** No second daemon, no Python venv, no cloud round-trip. The same `myownllm` process hosts ASR, diarization, and the chat model used to summarise — coordinated through two singleton slots on the GUI's mode bar.

Both paths — chat and transcription — are designed to be available on the GUI, the headless `serve` API, and the LAN remote view. The desktop GUI is the most complete today; full audio capture over `serve` / remote is on the near-term roadmap.

## Highlights

|   |   |
|---|---|
| **Three wire formats, one server** | OpenAI on `:1473`, plus Ollama and Anthropic. Point Cursor, Continue, Aider, Cline, Zed, Open WebUI, opencode, OpenClaw, OpenClaude or your own scripts at it and it just works. |
| **Virtual model IDs** | `myownllm` and `myownllm-transcribe`. Stable names; the right tag for your hardware auto-resolves. |
| **Manifests, not config** | A JSON file at a URL is the source of truth. `imports` compose merged catalogs across publishers — no coordination required. |
| **Runs on a Pi 5** | Default manifest ships Gemma 4 edge variants (`e2b` / `e4b`), Apache-2.0, ~7.6 tok/s on a Pi 5. Same manifest gives a 4090 the 4090 tag. |
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
