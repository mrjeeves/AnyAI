# Diarization + ASR-swap progress

Working notes for the `claude/implement-diarization-5TGwZ` branch. The full plan lives at `/root/.claude/plans/because-we-need-this-lexical-hinton.md` (also archived in the PR description). This file tracks what's landed, what's stubbed, and what's still ahead.

## Status

| Area | State |
|---|---|
| Manifest schema v13 (per-tier `runtime`) | ✅ landed |
| Default manifest (transcribe + diarize tier ladders) | ✅ landed |
| `models.rs` central downloader (Moonshine, Parakeet, pyannote-seg, embedders) | ✅ landed |
| `asr/` module: trait, Moonshine + Parakeet skeletons | ⚠️ scaffolded — ONNX forward stubbed |
| `diarize/` module: pyannote-diarize backend, segmenter, embedder, online clusterer | ⚠️ scaffolded — ONNX forward stubbed; **clusterer is fully working** |
| `transcribe.rs` rewrite (AsrBackend, segment frames, backpressure, diarize join) | ✅ landed |
| Tauri commands (`asr_*`, `diarize_*`) | ✅ landed |
| `preload.rs` (per-tier runtime aware) | ✅ landed |
| Frontend type checks (`pnpm check`) | ✅ green |
| Backend type checks (`cargo check`) | ✅ green |
| Unit tests (86 passing, including 13 new ones for cluster/segmenter/models/resolver/parakeet) | ✅ green |
| `Conversation.transcript: TranscriptSegment[]` migration | ⏳ deferred |
| `TranscribeView.svelte` segment-grouped rendering, speaker rename, diarize toggle | ⏳ deferred |
| Renaming Tauri command call sites in frontend (whisper_models_list → asr_models_list etc.) | ⏳ deferred |
| `NOTICE.md` + DOCS.md / README.md refresh | ⏳ deferred |
| Golden-audio integration test | ⏳ deferred |

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

## Deferred frontend work

The frontend type-checks today because of stop-gap fixes (changing `runtime === "whisper"` → `runtime !== "ollama"` everywhere). It will **NOT** function at runtime until the call sites that still invoke whisper-named Tauri commands are renamed:

```
src/ui/App.svelte:               whisper_models_list, whisper_model_pull
src/ui/TranscribeView.svelte:    whisper_models_list (twice)
src/ui/transcribe-state.svelte.ts: copy comments only
src/model-lifecycle.ts:          whisper_models_list, WhisperModelInfo interface, runtime: "ollama" | "whisper"
src/config.ts:                   legacy mic.whisper_model migration (keep)
```

Mapping:
- `whisper_models_list` → `asr_models_list`
- `whisper_model_pull` → `asr_model_pull`
- `whisper_model_remove` → `asr_model_remove`
- New: `diarize_models_list`, `diarize_model_pull`, `diarize_model_present`
- New on `transcribe_start` / `transcribe_drain_start` / `transcribe_upload_start`: required `runtime: string` parameter, optional `diarize_model: string | null`.

### Data model migration

`Conversation.transcript: string` → `Conversation.transcript: TranscriptSegment[]` (+ `speaker_labels?: Record<number,string>`, + `diarize_enabled?: boolean`). See plan file § "Data model migration" for the exact shape and the load-time migration helper.

### UI segment rendering

`src/ui/TranscribeView.svelte` currently appends `frame.delta` to a string. The new frame shape carries `frame.segments: EmittedSegment[]`. The UI needs:

1. Render segments grouped by consecutive same-speaker runs.
2. Per-speaker color (deterministic HSL hash from `speaker_id`).
3. Inline rename: click speaker pill → input → persist in `conversation.speaker_labels`.
4. Header toggle "Identify speakers" — on first enable, call `diarize_model_present` then `diarize_model_pull` if absent, then re-start the session with `diarize_model` set.

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

## Order of operations for the next session

1. **Wire ort 2.x in the four stub bodies** (Moonshine forward, Parakeet forward, pyannote-seg forward, embedder forward) against the real ONNX models pulled by `models::pull_model`. Test against the golden audio fixture.
2. **Rename Tauri command call sites** in the frontend (~5 places).
3. **Migrate `Conversation.transcript`** to `TranscriptSegment[]` with a lazy load-time migration helper.
4. **Rewrite `TranscribeView.svelte`** for segment rendering + diarize toggle + speaker rename.
5. **Add `NOTICE.md`** with the model licenses (MIT Moonshine / Apache-2.0 Parakeet & wespeaker & CAM++ / CC-BY-SA-4.0 pyannote-seg).
6. **Update `DOCS.md` + `README.md`** — Pi 5 English-only caveat, drop whisper claims.
7. **Add the golden-audio integration test** (`src-tauri/tests/fixtures/diarize_two_speakers.wav` + `tests/diarize_e2e.rs`) via the `Emitter` trait abstraction the plan calls for.
