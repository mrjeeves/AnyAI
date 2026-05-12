# Diarization + ASR-swap progress

Working notes for the `claude/implement-diarization-5TGwZ` branch. The full plan lives at `/root/.claude/plans/because-we-need-this-lexical-hinton.md` (also archived in the PR description). This file tracks what's landed, what's stubbed, and what's still ahead.

## Status

| Area | State |
|---|---|
| Manifest schema v13 (per-tier `runtime`) | ✅ landed |
| Default manifest (transcribe + diarize tier ladders) | ✅ landed |
| `models.rs` central downloader (Moonshine, Parakeet, pyannote-seg, embedders) | ✅ landed |
| `asr/` module: trait, Moonshine + Parakeet | ⚠️ scaffolded — ONNX forward stubbed |
| `diarize/` module: pyannote-diarize backend, segmenter, embedder, online clusterer | ⚠️ scaffolded — ONNX forward stubbed; **clusterer is fully working** |
| `transcribe.rs` rewrite (AsrBackend, segment frames, backpressure, diarize join) | ✅ landed |
| Tauri commands (`asr_*`, `diarize_*`) | ✅ landed |
| `preload.rs` (per-tier runtime aware) | ✅ landed |
| `Conversation.transcript: TranscriptSegment[]` migration | ✅ landed |
| `TranscribeView.svelte` segment-grouped rendering, speaker rename, diarize toggle | ✅ landed |
| Renaming Tauri command call sites in frontend (`whisper_*` → `asr_*` / `diarize_*`) | ✅ landed |
| `transcribe-state.svelte.ts` segment-based frame protocol | ✅ landed |
| `chat-slot.svelte.ts` Talking Points reads new transcript shape | ✅ landed |
| Frontend type checks (`pnpm check`) | ✅ green (0 errors) |
| Backend type checks (`cargo check`) | ✅ green |
| Unit tests (86 passing, including 13 new ones for cluster/segmenter/models/resolver/parakeet) | ✅ green |
| `NOTICE.md` (model licenses + attributions) | ✅ landed |
| DOCS.md + README.md + ARCHITECTURE.md refresh | ✅ landed |
| Golden-audio integration test | ⏳ deferred — needs `Emitter` trait abstraction |

## What you can do today

- `cargo check` and `cargo test --bins` both pass.
- `pnpm check` passes (0 errors).
- The Rust resolver routes per-tier through `parakeet` on capable hardware and `moonshine` on Pi-class via the v13 manifest. Tests cover the promotion path.
- The frame protocol carries `segments: Vec<EmittedSegment>` with optional `speaker` / `overlap` fields. The whisper-era `delta: String` field is gone — frontend callers that depend on it will need updating (see "Deferred frontend work" below).

## What's stubbed (the ort wire-up)

`ort` 2.0.0-rc.12 is in `Cargo.toml` and successfully compiles. **The actual ONNX inference loops are stubbed with `Err(anyhow!("…not yet implemented — see PROGRESS.md"))`** in four places:

| File | What needs the ort wire-up |
|---|---|
| `src-tauri/src/asr/moonshine.rs::process_chunk` | Build `Session` from encoder.onnx + decoder.onnx in `warm_up`. Per chunk: encoder forward → autoregressive merged-decoder loop with past-KV → tokenizer.decode. |
| `src-tauri/src/asr/parakeet.rs::process_chunk` | Build `Session` from model.onnx in `warm_up`. Per chunk: single-pass forward with `[1, T]` f32 audio + optional `[1]` i64 lengths → token IDs + frame indices → existing `decode_to_segments`. |
| `src-tauri/src/diarize/segmenter.rs::segment` | Build `Session` from segmentation.onnx in `warm_up`. Per window: forward → `[1, T, 7]` powerset logits → argmax per frame → existing `rle_to_slices`. |
| `src-tauri/src/diarize/embedder.rs::embed` | Build `Session` from embedder.onnx in `warm_up`. Per slice: forward → `[1, D]` (or `[1, 1, D]`) embedding → L2-normalize → cache `dim`. |

### Key ort 2.0.0-rc.12 API notes

I had to back the inference code out because of API churn between RC versions. When you wire it back in:

- `session.inputs` is a private field; use `session.inputs()` (method) and similarly `session.outputs()`.
- Build tensors with `ort::value::TensorRef::from_array_view(&arr)?` rather than `Tensor::from_array`.
- The `ort::inputs![ name => tensor, ... ]` macro is the canonical way to build the inputs map.
- `ort::Error` is not `Send + Sync`, so `?` against an `anyhow::Result` fails. Use `.map_err(|e| anyhow!("ort: {e}"))` instead.
- Default features pull in `api-24` which transitively enables every EP; we use `default-features = false` with `features = ["std", "load-dynamic", "ndarray", "api-22"]` to dodge the broken VitisAI binding while keeping the modern OrtApi.

## Frontend work landed in this session

All Tauri command renames + data-model migration + segment rendering are now in. Specifically:

- `App.svelte` — `asr_models_list` / `asr_model_pull`, runtime-aware FirstRun props, recovery probe passes runtime + diarize composite to `startDrain`.
- `FirstRun.svelte` — `ModelPullEvent` shape with per-artifact progress; subscribed to `myownllm://model-pull/asr/{name}`.
- `TranscribeView.svelte` — full rewrite: segment-grouped rendering with per-speaker color, inline speaker rename, "Identify speakers" toggle with lazy diarize-model pull, "X s behind realtime" backlog indicator.
- `transcribe-state.svelte.ts` — `liveSegments: EmittedSegment[]`, `takeLiveSegments()` helper, runtime + diarizeModel parameters on `startRecording` / `startUpload` / `startDrain`.
- `chat-slot.svelte.ts` — Talking Points reads `transcript: TranscriptSegment[]`, flattens via `.map(s => s.text).join(" ")`.
- `model-lifecycle.ts` — `ModelInfo` interface, parallel queries against `asr_models_list` and `diarize_models_list`, surfaces both kinds in the unified Models list.
- `settings/ModelsSection.svelte` — routes `asr_model_remove` for local-runtime models; surfaces friendly error when user tries to delete a diarize model directly (those are managed via the transcribe toggle).
- `settings/FamiliesSection.svelte` + `settings/StorageSection.svelte` — query both `asr_models_list` and `diarize_models_list`, sum sizes correctly.
- `conversations.ts` — `TranscriptSegment` interface, `Conversation.transcript: TranscriptSegment[]`, `speaker_labels`, `diarize_enabled`, lazy `migrateConversationInPlace` on load so legacy string transcripts auto-wrap into a single zero-timestamped segment.

## Critical files modified this session

- `manifests/default.json` — v13, new transcribe + diarize blocks.
- `src/types.ts` — `ModelRuntime` enum + per-tier `runtime` field.
- `src/manifest.ts` — `defaultRuntimeFor`, new `tierRuntime` helper, `resolveModelEx` uses per-tier runtime.
- `src-tauri/src/resolver.rs` — per-tier runtime resolution, hardware-aware `mode_runtime`.
- `src-tauri/src/main.rs` — module registrations + new Tauri commands.
- `src-tauri/src/transcribe.rs` — full rewrite around `AsrBackend` + optional `DiarizeBackend`, new frame shape, backpressure on small-chunk backlogs.
- `src-tauri/src/preload.rs` — comment updates for the new runtimes.
- `src-tauri/Cargo.toml` — `whisper-rs` out, `ort 2.0.0-rc.12` + `ndarray 0.16` + `tokenizers 0.20` in.

New files:
- `src-tauri/src/models.rs` — central downloader + model registry.
- `src-tauri/src/asr/{mod,moonshine,parakeet}.rs`
- `src-tauri/src/diarize/{mod,cluster,segmenter,embedder}.rs`

## Verification commands

```
cd src-tauri && cargo check          # passes
cd src-tauri && cargo test --bins    # 86 passing
pnpm install && pnpm check           # passes
```

## Merged from `main`

- Picked up the `0.2.5` version bump (package.json + Cargo.toml).
- PR #100's spirit ported into the new pipeline: `ASR_CONSECUTIVE_ERROR_LIMIT` (3) in `transcribe.rs`. On a backend error the worker bumps a per-loop counter, calls `backend.reset_state()`, and continues; after 3 consecutive failures the session aborts with a clear error so a non-transient problem (model corruption, OOM, ONNX runtime wedge) surfaces instead of silently chewing through chunks. Applied to all three run loops (`run_session` / `run_drain` / `run_upload`).
- The whisper-state-recreate code from PR #100 itself doesn't apply — that logic was tied to whisper-rs's stateful KV cache, which the new `AsrBackend` trait replaces with per-backend `reset_state()`. Moonshine/Parakeet are stateless across chunks today (`state_reset_chunks: 0`), so the reset is currently a no-op; once their ONNX inference is wired up they can opt in by maintaining a session-mutable state and dropping it on reset.

## What's left for the next session

Only one item, and it's the **load-bearing** one: **wire ort 2.x in the four stub bodies** (Moonshine encoder→decoder, Parakeet merged graph, pyannote-seg powerset, embedder forward) against the real ONNX models pulled by `models::pull_model`. Test against a golden audio fixture.

After that:

1. **Add the golden-audio integration test** (`src-tauri/tests/fixtures/diarize_two_speakers.wav` + `tests/diarize_e2e.rs`) via the `Emitter` trait abstraction the plan calls for.
2. (Optional) Add a `diarize_model_remove` Tauri command + Settings UI hook so users can manage diarize disk usage directly from the Models pane.

Everything else — schema, modules, transcribe.rs, frontend, data model, segment rendering, diarize toggle, NOTICE, docs — is **landed**.
