# AnyAI

> A local API surface for local AI. Self-host the JSON, set it, forget it.

AnyAI is a single binary that exposes an OpenAI-compatible HTTP API on `127.0.0.1`. Every request asks one question — _"what model should this machine run for this mode?"_ — and answers it from a static JSON file at a URL you (or your team, or a publisher you trust) host. JSON files can `import` other JSON files, so an org or community can compose merged catalogs without coordinating servers. The binary auto-updates itself in the background, so once installed it keeps working.

## Install

macOS / Linux:

```bash
curl -fsSL https://raw.githubusercontent.com/mrjeeves/AnyAI/main/scripts/install.sh | sh
```

Windows (PowerShell):

```powershell
irm https://raw.githubusercontent.com/mrjeeves/AnyAI/main/scripts/install.ps1 | iex
```

Or grab a binary from [Releases](https://github.com/mrjeeves/AnyAI/releases).

## Quick start

```
$ anyai serve
Listening on http://127.0.0.1:1473
Tracking: text → qwen2.5:14b   code → qwen2.5-coder:14b
```

In another shell, hit it:

```bash
curl http://127.0.0.1:1473/v1/chat/completions \
  -H 'content-type: application/json' \
  -d '{"model":"anyai-text","messages":[{"role":"user","content":"hello"}]}'
```

To use AnyAI from Cursor / Continue / Aider / any OpenAI-compatible client, point at:

```
Base URL: http://127.0.0.1:1473/v1
Model:    anyai-text     (also: anyai-code, anyai-vision, anyai-transcribe)
API key:  any non-empty string
```

The model behind `anyai-text` auto-resolves to the best tag for your hardware and stays current as upstream JSON changes — no client-side reconfiguration needed, ever.

## Other ways in

```bash
anyai run            # terminal chat
anyai                # desktop GUI
anyai status         # provider, hardware, ollama state
anyai update         # self-update status / check / apply
```

## Documentation

- **[DOCS.md](DOCS.md)** — full CLI reference, manifest format, imports & merged catalogs, auto-update, model lifecycle, scripting, repackaging.
- **[ARCHITECTURE.md](ARCHITECTURE.md)** — internals, modules, data flow.

## Build from source

```bash
git clone https://github.com/mrjeeves/AnyAI
cd AnyAI
just setup       # rust, node, pnpm, tauri CLI, GTK on Linux
just build       # → src-tauri/target/release/anyai
```

See [DOCS.md › Building from source](DOCS.md#building-from-source) for the prereq list and `pnpm tauri dev`.

## License

MIT — see [LICENSE](LICENSE).
