# Diarization + ASR-swap progress

Working notes started on `claude/implement-diarization-5TGwZ` (merged
as #101) and continued on `claude/wire-ort-inference` (the
follow-up). The original plan lives at
`/root/.claude/plans/because-we-need-this-lexical-hinton.md` and the
PR descriptions on GitHub.

## Status

| Area | State |
|---|---|
| Manifest schema v13 (per-tier `runtime`) | ✅ landed (#101) |
| Default manifest (transcribe + diarize tier ladders) | ✅ landed (#101) |
| `models.rs` central downloader | ✅ landed (#101) |
| `asr/` module: trait, Moonshine + Parakeet | ✅ **ONNX forward wired** |
| `diarize/` module: pyannote-diarize backend, segmenter, embedder, online clusterer | ✅ **ONNX forward wired** |
| `transcribe.rs` rewrite (AsrBackend, segment frames, backpressure, diarize join) | ✅ landed (#101) |
| Tauri commands (`asr_*`, `diarize_*`) | ✅ landed (#101) |
| `preload.rs` (per-tier runtime aware) | ✅ landed (#101) |
| Frontend (TranscribeView segment rendering, diarize toggle, etc.) | ✅ landed (#101) |
| `Conversation.transcript: TranscriptSegment[]` migration | ✅ landed (#101) |
| NOTICE.md + DOCS.md + README.md + ARCHITECTURE.md refresh | ✅ landed (#101) |
| **`FrameSink` trait abstraction over `WebviewWindow`** | ✅ **new** |
| **`CaptureSink` for headless transcribe tests** | ✅ **new** |
| **`join_segments` unit tests (5)** | ✅ **new** |
| Golden-audio integration test (full pipeline on real audio) | ⏳ deferred — needs ONNX models in CI |
| Cached-path Moonshine decoder (past-KV, faster) | ⏳ follow-up optimisation |
| Real-hardware verification (Pi 5 + Apple Silicon + x86) | ⏳ next |

## What landed in the ort wire-up PR (`claude/wire-ort-inference`)

### Four ONNX forward passes wired against `ort 2.0.0-rc.12`

| File | What's now live |
|---|---|
| `src-tauri/src/asr/moonshine.rs` | Encoder `[1, N]` PCM → `[1, T, D]` hidden; autoregressive **no-cache** greedy decoder loop with `use_cache_branch=false` and zero-shape past-KV dummy tensors; tokenizer.decode at the end. O(n²) per chunk but `n ≤ ~30` so a 1 s chunk completes in tens of ms even on Pi-class hardware. The cached past-KV path is a follow-up optimisation when there's a measured latency win to chase. |
| `src-tauri/src/asr/parakeet.rs` | Single-pass forward `[1, N]` PCM (+ optional `[1]` lengths i64) → token IDs (i64 or i32 → widen to i64) + optional per-token frame indices → existing `decode_to_segments`. |
| `src-tauri/src/diarize/segmenter.rs` | Forward `[1, N]` PCM → `[1, T, 7]` powerset logits → per-frame argmax → existing `rle_to_slices`. |
| `src-tauri/src/diarize/embedder.rs` | Forward `[1, N]` PCM → `[1, D]` (or `[1, 1, D]`) → L2-normalize → cache `dim`. |

All four backends sniff input / output tensor names by suffix-match at
warm-up, so the canonical names (`audio_signal`, `tokens`, `logits`,
`embedding`, `past_key_values.N.*`, `use_cache_branch`) can be
renamed by a future ONNX re-export without breaking the wire-up.

### `FrameSink` testability seam

`src-tauri/src/frame_sink.rs` (new):

- `pub trait FrameSink: Send + Sync { fn emit_frame(&self, event: &str, frame: TranscribeFrame); }`
- `impl FrameSink for tauri::WebviewWindow` (delegates to `tauri::Emitter::emit`).
- `pub struct CaptureSink` (test-only) that records `(event, frame)` pairs in a `Mutex<Vec>`.

`transcribe.rs` now takes `&Arc<dyn FrameSink>` instead of
`&WebviewWindow`. Public Tauri commands still receive `WebviewWindow`
and wrap it in `Arc::new(window)` before calling the internals. The
ingest thread `Arc::clone`s the sink across worker boundaries.

### Important Cargo.toml change

`ndarray` pinned to `0.17` to match what `ort 2.0.0-rc.12`
internally depends on. Mixing `0.16` and `0.17` compiles them as
distinct types, so `Tensor::from_array` doesn't accept an array from
the wrong version — symptomatic failure is a confusing "trait not
satisfied" error pointing at the same-looking type.

## ort 2.0.0-rc.12 API notes (in case the next ort version churns)

- `session.inputs()` is a method (not a field). `Outlet::name()` →
  `&str`.
- Owned tensor: `Tensor::from_array(arr)` consumes the
  `ndarray::Array`. Borrowed: `TensorRef::from_array_view(&arr)`
  works but the generics inference is fragile — turbofish it if you
  hit the "trait not satisfied" wall.
- Run with `session.run(ort::inputs![ name => tensor, ... ])` or a
  built-up `Vec<(Cow<'static, str>, SessionInputValue)>` for cases
  with a dynamic input set (Moonshine's past-KV).
- `ort::Error` is **not** `Send + Sync`, so `?` against an
  `anyhow::Result` won't work. Use `.map_err(|e| anyhow!("ort: {e}"))`.
- `outputs.get(name)` returns `Option<&DynValue>`. Extract with
  `value.try_extract_array::<f32>()` → `ArrayViewD<'_, f32>` (borrowed
  view). Call `.to_owned()` for an `ArrayD` that survives past the
  next `.run`.
- Build with `default-features = false, features = ["std", "load-dynamic", "ndarray", "api-22"]`.
  Skipping `api-22` gives "unknown field
  `SessionOptionsAppendExecutionProvider_VitisAI`" — `vitis.rs`
  references it unconditionally despite being feature-gated on the
  bindings side.

## Real-hardware verification — the load-bearing next step

Everything compiles and `cargo test` is green (98 tests, including
new ones for `join_segments`, `CaptureSink`, and `l2_normalize`),
**but none of the four ONNX forward passes have run against a real
model file**. The shapes and tensor names are inferred from the
upstream model cards + sherpa-onnx / istupakov community exports.
The next session should:

1. On a Pi 5: pull the Moonshine Small composite via the GUI's "first
   run" flow or `myownllm preload transcribe`. Open the transcribe
   pane, record 30 seconds of speech, verify segments arrive and the
   transcript is sensible.
2. On Apple Silicon / x86 with ≥ 16 GB unified RAM: same but for
   Parakeet TDT.
3. Toggle "Identify speakers" on either platform. Verify the
   diarize models pull, the segmenter + embedder forward without
   shape errors, and speaker pills render correctly.
4. If any forward errors with a shape mismatch, the most likely
   cause is a renamed I/O tensor on a fresh upstream export — the
   suffix-match should handle most cases, but rare ones may need an
   added pattern in the `warm_up` sniffer.

## Deferred items (still useful, no longer load-bearing)

- **Cached Moonshine decoder**. The no-cache loop is O(n²); the
  cached path is O(n). For 1 s chunks (`n ≤ 30`) the absolute cost
  is small but a longer chunk size or a wider model would benefit.
- **Golden-audio integration test**. Now genuinely tractable since
  `CaptureSink` exists. Blocker is shipping ~150 MB of ONNX models
  to CI (Moonshine alone) plus a small fixture WAV. Options:
  download-on-demand in the test (slow CI), gate on a
  `MYOWNLLM_TEST_ASSETS_DIR` env var (skip when unset), or store
  fixtures via Git LFS. Worth deciding once one platform has been
  manually verified.
- **`diarize_model_remove` Tauri command + Settings UI hook**. Lets
  users free disk by removing pyannote / wespeaker / CAM++ without
  touching the filesystem. Low priority — `models::remove` is
  already there; just needs the command + UI wiring.

## CI gates (all green locally on this branch)

```
cd src-tauri && cargo fmt --check    # OK
cd src-tauri && cargo clippy --all-targets   # 1 pre-existing warning (remote_ui)
cd src-tauri && cargo test --no-fail-fast    # 98 passing
pnpm install && pnpm check                   # 0 errors
```
