//! Local-only live transcription.
//!
//! cpal captures from the default (or named) input device. Samples flow
//! through a small in-RAM hop into an *ingest* thread, which downmixes,
//! resamples to 16 kHz, accumulates 5-second chunks, and spills each
//! chunk to disk under `~/.anyai/transcribe-buffer/{stream_id}/{seq}.f32`.
//! A separate *inference* thread reads chunks from disk in sequence
//! order, runs whisper-rs on them, emits text deltas, and deletes the
//! chunk on success. Stitched-in-order text is therefore preserved even
//! when the model can't keep up with realtime — the backlog spills to
//! cheap disk instead of fighting for scarce RAM, and no audio is ever
//! dropped.
//!
//! Nothing is sent over the network at runtime. The whisper model is
//! loaded from `~/.anyai/whisper/ggml-{name}.bin`, which is downloaded on
//! demand by `whisper_model_pull` (see below). No model files ship with
//! the binary.

use anyhow::{anyhow, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_channel::{bounded, Receiver, RecvTimeoutError};
use dashmap::DashMap;
use serde::Serialize;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::Duration;
use tauri::{Emitter, WebviewWindow};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

/// Target sample rate for whisper. The ggml models are all trained on
/// 16 kHz mono audio.
const TARGET_SR: u32 = 16_000;
/// Length of each independent transcription chunk. 5 s gives whisper
/// enough context for sensible word boundaries without making users wait
/// too long for the first text to appear.
const CHUNK_SECONDS: f32 = 5.0;
/// Minimum length of the trailing partial chunk that the ingest thread
/// flushes when the session is cancelled. Whisper produces garbage on
/// sub-second inputs so we just drop tails shorter than this.
const TAIL_FLUSH_MIN_SECONDS: f32 = 1.0;

/// Frame shape emitted on `anyai://transcribe-stream/{stream_id}`. `delta`
/// is the new text since the last frame; the frontend appends. `final`
/// signals the worker has unwound (either user-stopped or errored).
/// `pending_chunks` is how many 5-second chunks are still queued on disk
/// waiting to be transcribed — the UI can multiply by 5 to surface a
/// "X seconds behind realtime" indicator.
#[derive(Debug, Serialize, Clone)]
pub struct TranscribeFrame {
    pub delta: String,
    pub elapsed_ms: u128,
    #[serde(rename = "final")]
    pub is_final: bool,
    pub pending_chunks: u32,
}

struct Session {
    cancel: Arc<AtomicBool>,
    /// When set, cpal callbacks early-return instead of forwarding samples
    /// to the ingest thread. The inference loop keeps draining whatever's
    /// already on disk — so the user can pause mic capture and let the
    /// backlog catch up without losing the running session. Resume just
    /// flips this back. Inference-only ("drain") sessions never read it.
    paused: Arc<AtomicBool>,
}

fn sessions() -> &'static DashMap<String, Session> {
    static M: OnceLock<DashMap<String, Session>> = OnceLock::new();
    M.get_or_init(DashMap::new)
}

/// Path to the directory whisper models are downloaded into. Mirrors the
/// `~/.anyai/` convention the rest of the app uses.
pub fn whisper_dir() -> Result<PathBuf> {
    let dir = crate::anyai_dir()?.join("whisper");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Per-session directory holding 16 kHz mono f32 chunk files queued for
/// inference. Created at session start, emptied on entry (defensive
/// cleanup against a previous crashed session leaving stale chunks),
/// and removed entirely on session end.
fn chunk_buffer_dir(stream_id: &str) -> Result<PathBuf> {
    let dir = crate::anyai_dir()?
        .join("transcribe-buffer")
        .join(sanitize_stream_id(stream_id));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Root of the per-session chunk directories. Used by storage / recovery
/// helpers that walk every stream the way Disk Usage does, rather than
/// drilling into one stream by id.
fn buffer_root() -> Result<PathBuf> {
    Ok(crate::anyai_dir()?.join("transcribe-buffer"))
}

/// Recursive size of `~/.anyai/transcribe-buffer/`. The Storage tab
/// surfaces this so the user can see how much disk a slow whisper backlog
/// is parked on. Errors collapse to 0 — a missing dir is the steady state
/// when there's no recording happening.
pub fn buffer_size_bytes() -> u64 {
    fn walk(p: &Path) -> u64 {
        let mut total = 0u64;
        let entries = match std::fs::read_dir(p) {
            Ok(e) => e,
            Err(_) => return 0,
        };
        for entry in entries.flatten() {
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            if meta.is_dir() {
                total = total.saturating_add(walk(&entry.path()));
            } else {
                total = total.saturating_add(meta.len());
            }
        }
        total
    }
    let root = match buffer_root() {
        Ok(p) => p,
        Err(_) => return 0,
    };
    walk(&root)
}

/// `_meta.json` written into a session's chunk dir on start so a later
/// drain-only resumption can recover the model name without the user
/// having to remember which whisper they were on. Tiny — one stable
/// field today; lives next to the chunks so it's atomically deleted with
/// them when the session ends.
#[derive(Serialize, serde::Deserialize, Clone)]
struct BufferMeta {
    model: String,
}

fn write_meta(buffer_dir: &Path, model: &str) {
    let meta = BufferMeta {
        model: model.to_string(),
    };
    let path = buffer_dir.join("_meta.json");
    if let Ok(s) = serde_json::to_string(&meta) {
        let _ = std::fs::write(path, s);
    }
}

fn read_meta(buffer_dir: &Path) -> Option<BufferMeta> {
    let path = buffer_dir.join("_meta.json");
    let s = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&s).ok()
}

/// One pending stream entry, surfaced on app start so the UI can offer
/// to drain whatever was left over from a crashed previous session.
#[derive(Debug, Serialize, Clone)]
pub struct PendingStream {
    pub stream_id: String,
    pub pending_chunks: u32,
    /// Whisper model that was running when the chunks were spilled. Used
    /// by the drain command so the user doesn't have to repick.
    pub model: Option<String>,
}

pub fn list_pending_streams() -> Vec<PendingStream> {
    let mut out = Vec::new();
    let root = match buffer_root() {
        Ok(p) => p,
        Err(_) => return out,
    };
    let entries = match std::fs::read_dir(&root) {
        Ok(e) => e,
        Err(_) => return out,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let pending = count_pending_chunks(&path);
        if pending == 0 {
            continue;
        }
        let stream_id = match path.file_name().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        // Skip dirs that are part of an in-flight session — those are
        // already being drained by the running worker and surfacing them
        // here would invite a double-start race.
        if sessions().contains_key(&stream_id) {
            continue;
        }
        let model = read_meta(&path).map(|m| m.model);
        out.push(PendingStream {
            stream_id,
            pending_chunks: pending,
            model,
        });
    }
    out
}

/// `stream_id` comes from the frontend (UUIDs in practice), but we
/// don't trust callers — strip anything that isn't a-z, 0-9, `-`, or
/// `_` so the path can't escape `~/.anyai/transcribe-buffer/`.
fn sanitize_stream_id(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Resolve a friendly model name (e.g. `"tiny.en"`) to its on-disk path.
/// Returns the path even if the file doesn't exist yet — callers decide
/// whether to error or download.
pub fn model_path(name: &str) -> Result<PathBuf> {
    Ok(whisper_dir()?.join(format!("ggml-{name}.bin")))
}

/// Catalogue of models AnyAI knows how to download. Sizes verified
/// against the HuggingFace API on 2026-05-10 (`approx`), with `min_bytes`
/// set to about 60% of the real size so a successful download has to
/// transfer most of the payload — anything smaller is almost certainly
/// an HTML error page or a truncated stream and gets rejected post-hoc.
pub const KNOWN_MODELS: &[(&str, u64, u64)] = &[
    // (name, approx_size_bytes, min_acceptable_bytes)
    // English-only (.en) variants — faster/more accurate on English
    // input. Default tier picks use these.
    ("tiny.en", 77_704_715, 50_000_000),
    ("base.en", 147_964_211, 100_000_000),
    ("small.en", 487_614_201, 400_000_000),
    ("medium.en", 1_533_774_781, 1_300_000_000),
    // Multilingual variants — same architectures, trained without the
    // English-only filter. Pick these via `mode_overrides.transcribe`
    // when the speaker isn't English.
    ("tiny", 77_691_713, 50_000_000),
    ("base", 147_951_465, 100_000_000),
    ("small", 487_601_967, 400_000_000),
    ("medium", 1_533_763_059, 1_300_000_000),
    // Large variants are multilingual-only.
    ("large-v3-turbo", 1_624_555_275, 1_400_000_000),
    ("large-v3", 3_095_033_483, 2_700_000_000),
    ("large-v2", 3_094_623_691, 2_700_000_000),
    ("large-v1", 3_094_623_691, 2_700_000_000),
];

fn known(name: &str) -> Option<&'static (&'static str, u64, u64)> {
    KNOWN_MODELS.iter().find(|(n, _, _)| *n == name)
}

fn hf_url(name: &str) -> String {
    format!("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-{name}.bin")
}

#[derive(Debug, Serialize, Clone)]
pub struct WhisperModelInfo {
    pub name: String,
    pub approx_size_bytes: u64,
    pub installed: bool,
    pub installed_size_bytes: Option<u64>,
}

pub fn list_models() -> Result<Vec<WhisperModelInfo>> {
    let dir = whisper_dir()?;
    let mut out = Vec::new();
    for (name, approx, min_bytes) in KNOWN_MODELS {
        let path = dir.join(format!("ggml-{name}.bin"));
        let size = std::fs::metadata(&path).ok().map(|m| m.len()).unwrap_or(0);
        // A file that's there but smaller than `min_bytes` is almost
        // always a leftover from a UA-less HF response that returned
        // HTML where we expected ggml weights. Treat it as not installed
        // so the UI re-prompts a real download instead of letting
        // `transcribe::start` fail later with a confusing
        // "model header invalid" inside whisper.cpp.
        let installed = size >= *min_bytes;
        let installed_size_bytes = if installed { Some(size) } else { None };
        out.push(WhisperModelInfo {
            name: (*name).to_string(),
            approx_size_bytes: *approx,
            installed,
            installed_size_bytes,
        });
    }
    Ok(out)
}

#[derive(Debug, Serialize, Clone)]
pub struct WhisperPullProgress {
    pub name: String,
    pub bytes: u64,
    pub total: u64,
    pub done: bool,
    pub error: Option<String>,
}

/// Download `ggml-{name}.bin` from HuggingFace into `~/.anyai/whisper/`.
/// Streams to a temp file then renames into place. Defends against the
/// three failure modes that previously surfaced as "pull finished but
/// model isn't installed":
///   1. HF LFS occasionally returns HTML (login wall / license page) to
///      User-Agent-less requests with a 200 status.
///   2. A leftover too-small final file from a previous broken pull
///      blocking re-download via the early-exists short-circuit.
///   3. The early-exists path swallowing the `done: true` event so the
///      UI hangs in "downloading" state forever.
pub async fn pull_model(name: String, window: WebviewWindow) -> Result<()> {
    let (_, approx, min_bytes) = match known(&name) {
        Some(m) => *m,
        None => return Err(anyhow!("unknown whisper model: {name}")),
    };

    let dir = whisper_dir()?;
    let final_path = dir.join(format!("ggml-{name}.bin"));
    let event = format!("anyai://whisper-pull/{name}");
    let emit = |frame: WhisperPullProgress| {
        let _ = window.emit(&event, frame);
    };

    // If the file is already there AND looks complete, reaffirm the done
    // state to the UI (so any lingering progress row clears) and return.
    if let Ok(meta) = std::fs::metadata(&final_path) {
        if meta.len() >= min_bytes {
            emit(WhisperPullProgress {
                name: name.clone(),
                bytes: meta.len(),
                total: meta.len(),
                done: true,
                error: None,
            });
            return Ok(());
        }
        // Too small to be the real model — almost certainly an HTML
        // error page or a truncated previous run. Drop it and fetch
        // again instead of pretending we're done.
        let _ = std::fs::remove_file(&final_path);
    }

    // Clean up any stale `.partial` from a previous interrupted pull
    // before opening a fresh temp file.
    let tmp = dir.join(format!("ggml-{name}.bin.partial"));
    let _ = std::fs::remove_file(&tmp);

    let url = hf_url(&name);

    // HuggingFace's LFS storage can serve different responses to
    // requests without a User-Agent — sometimes a redirect to a login
    // page rendered as HTML, sometimes a 403. Always identify
    // ourselves so we get the binary.
    let client = reqwest::Client::builder()
        .user_agent(concat!(
            "AnyAI/",
            env!("CARGO_PKG_VERSION"),
            " (whisper-pull; +https://github.com/mrjeeves/AnyAI)"
        ))
        .build()?;
    let resp = client.get(&url).send().await?;
    if !resp.status().is_success() {
        let err = format!("HTTP {} fetching {url}", resp.status());
        emit(WhisperPullProgress {
            name: name.clone(),
            bytes: 0,
            total: 0,
            done: true,
            error: Some(err.clone()),
        });
        return Err(anyhow!(err));
    }
    // Some LFS redirects strip Content-Length; fall back to the verified
    // catalogue size so the progress bar shows real-looking numbers
    // instead of 0 / unknown.
    let total = resp.content_length().unwrap_or(approx);
    let mut file = tokio::fs::File::create(&tmp).await?;
    let mut stream = resp.bytes_stream();
    use futures_util::StreamExt;
    use tokio::io::AsyncWriteExt;
    let mut downloaded: u64 = 0;
    let mut last_emit_bytes: u64 = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        file.write_all(&chunk).await?;
        downloaded += chunk.len() as u64;
        // Throttle progress emits — every 1 MB is plenty for the
        // settings panel without flooding the IPC channel.
        if downloaded - last_emit_bytes > 1_048_576 {
            last_emit_bytes = downloaded;
            emit(WhisperPullProgress {
                name: name.clone(),
                bytes: downloaded,
                total,
                done: false,
                error: None,
            });
        }
    }
    file.flush().await?;
    drop(file);

    // Sanity-check the size before renaming. A 200 can still carry a
    // license / HTML response that we'd otherwise rename in as a
    // "valid" model file.
    if downloaded < min_bytes {
        let _ = tokio::fs::remove_file(&tmp).await;
        let err = format!(
            "downloaded {downloaded} bytes for {name}, expected ≥{min_bytes}. \
             The server may have returned an error page; try again."
        );
        emit(WhisperPullProgress {
            name: name.clone(),
            bytes: downloaded,
            total,
            done: true,
            error: Some(err.clone()),
        });
        return Err(anyhow!(err));
    }

    tokio::fs::rename(&tmp, &final_path).await?;
    emit(WhisperPullProgress {
        name: name.clone(),
        bytes: downloaded,
        total,
        done: true,
        error: None,
    });
    Ok(())
}

pub fn remove_model(name: &str) -> Result<()> {
    let path = model_path(name)?;
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

/// Spin up an audio capture + inference worker for `stream_id`. Returns
/// once the worker is alive; the actual transcript flows back through
/// `anyai://transcribe-stream/{stream_id}` events.
pub fn start(
    stream_id: String,
    model_name: String,
    device_name: Option<String>,
    window: WebviewWindow,
) -> Result<()> {
    if sessions().contains_key(&stream_id) {
        return Err(anyhow!("transcription {stream_id} is already running"));
    }
    let model_path = model_path(&model_name)?;
    if !model_path.exists() {
        return Err(anyhow!(
            "whisper model '{model_name}' isn't installed yet — pull it first from Settings → Transcription."
        ));
    }
    // Catch the truncated-file case before whisper-rs trips over a
    // malformed ggml header. A model under its known floor was almost
    // certainly an aborted pull or a sneaky HTML error page.
    if let Some((_, _, min_bytes)) = known(&model_name) {
        if let Ok(meta) = std::fs::metadata(&model_path) {
            if meta.len() < *min_bytes {
                return Err(anyhow!(
                    "whisper model '{model_name}' looks truncated ({} bytes; expected ≥{} bytes). \
                     Re-download from Settings → Transcription.",
                    meta.len(),
                    min_bytes
                ));
            }
        }
    }
    let cancel = Arc::new(AtomicBool::new(false));
    let paused = Arc::new(AtomicBool::new(false));
    sessions().insert(
        stream_id.clone(),
        Session {
            cancel: cancel.clone(),
            paused: paused.clone(),
        },
    );

    let stream_id_for_thread = stream_id.clone();
    let cancel_for_thread = cancel.clone();
    let paused_for_thread = paused.clone();
    let model_for_thread = model_name.clone();
    thread::spawn(move || {
        let event = format!("anyai://transcribe-stream/{stream_id_for_thread}");
        let res = run_session(
            &event,
            &stream_id_for_thread,
            &model_path,
            &model_for_thread,
            device_name.as_deref(),
            cancel_for_thread,
            paused_for_thread,
            &window,
        );
        sessions().remove(&stream_id_for_thread);
        let final_frame = match res {
            Ok(()) => TranscribeFrame {
                delta: String::new(),
                elapsed_ms: 0,
                is_final: true,
                pending_chunks: 0,
            },
            Err(e) => TranscribeFrame {
                delta: format!("[transcription error: {e}]"),
                elapsed_ms: 0,
                is_final: true,
                pending_chunks: 0,
            },
        };
        let _ = window.emit(&event, final_frame);
    });
    Ok(())
}

pub fn stop(stream_id: &str) -> Result<()> {
    if let Some(s) = sessions().get(stream_id) {
        s.cancel.store(true, Ordering::SeqCst);
    }
    Ok(())
}

/// Silence the cpal capture path without tearing the session down. The
/// inference thread keeps draining whatever's already on disk, so a long
/// whisper backlog still finishes draining while the user types or
/// switches modes; it just stops collecting fresh audio. Idempotent: a
/// no-op for unknown ids and for sessions started in drain-only mode
/// (which never had a mic in the first place).
pub fn pause(stream_id: &str) -> Result<()> {
    if let Some(s) = sessions().get(stream_id) {
        s.paused.store(true, Ordering::SeqCst);
    }
    Ok(())
}

pub fn resume(stream_id: &str) -> Result<()> {
    if let Some(s) = sessions().get(stream_id) {
        s.paused.store(false, Ordering::SeqCst);
    }
    Ok(())
}

/// Start an inference-only worker against an existing buffer dir. Used
/// when AnyAI relaunches and finds chunks left over from a previous
/// session — we don't open the mic, we just chew through what's there
/// and emit deltas the same way a normal session would. The worker
/// exits as soon as the buffer is empty (or on cancel).
pub fn start_drain(stream_id: String, model_name: String, window: WebviewWindow) -> Result<()> {
    if sessions().contains_key(&stream_id) {
        return Err(anyhow!("transcription {stream_id} is already running"));
    }
    let model_path = model_path(&model_name)?;
    if !model_path.exists() {
        return Err(anyhow!(
            "whisper model '{model_name}' isn't installed yet — install it from Settings → Models."
        ));
    }
    let cancel = Arc::new(AtomicBool::new(false));
    sessions().insert(
        stream_id.clone(),
        Session {
            cancel: cancel.clone(),
            // Drain has no mic to gate, so pause is a no-op for it. The
            // field stays present so DashMap entries are uniformly shaped.
            paused: Arc::new(AtomicBool::new(false)),
        },
    );

    let stream_id_for_thread = stream_id.clone();
    let cancel_for_thread = cancel.clone();
    thread::spawn(move || {
        let event = format!("anyai://transcribe-stream/{stream_id_for_thread}");
        let res = run_drain(
            &event,
            &stream_id_for_thread,
            &model_path,
            cancel_for_thread,
            &window,
        );
        sessions().remove(&stream_id_for_thread);
        let final_frame = match res {
            Ok(()) => TranscribeFrame {
                delta: String::new(),
                elapsed_ms: 0,
                is_final: true,
                pending_chunks: 0,
            },
            Err(e) => TranscribeFrame {
                delta: format!("[transcription error: {e}]"),
                elapsed_ms: 0,
                is_final: true,
                pending_chunks: 0,
            },
        };
        let _ = window.emit(&event, final_frame);
    });
    Ok(())
}

fn run_session(
    event: &str,
    stream_id: &str,
    model_path: &Path,
    model_name: &str,
    device_name: Option<&str>,
    cancel: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    window: &WebviewWindow,
) -> Result<()> {
    let started = std::time::Instant::now();
    let model_path_str = model_path
        .to_str()
        .ok_or_else(|| anyhow!("model path is not utf-8"))?;
    let ctx = WhisperContext::new_with_params(model_path_str, WhisperContextParameters::default())
        .map_err(|e| anyhow!("whisper init failed: {e}"))?;

    let buffer_dir = chunk_buffer_dir(stream_id)?;
    // A previous crashed session might have left stale chunks; wipe the
    // dir on entry so we start at seq=1 with a clean slate.
    if let Ok(entries) = std::fs::read_dir(&buffer_dir) {
        for entry in entries.flatten() {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    // Drop a tiny `_meta.json` next to the chunks so a future drain-only
    // resumption (after a crash or a forced quit) knows which whisper
    // model produced these samples. Best-effort: a missing meta just
    // means the recovery flow has to ask the user.
    write_meta(&buffer_dir, model_name);

    let host = cpal::default_host();
    let device = match device_name {
        Some(name) if !name.is_empty() => host
            .input_devices()?
            .find(|d| d.name().map(|n| n == name).unwrap_or(false))
            .ok_or_else(|| anyhow!("input device '{name}' not found"))?,
        _ => host
            .default_input_device()
            .ok_or_else(|| anyhow!("no default input device"))?,
    };
    let cfg = device
        .default_input_config()
        .map_err(|e| anyhow!("input config: {e}"))?;
    let sr = cfg.sample_rate().0;
    let channels = cfg.channels() as usize;
    let format = cfg.sample_format();
    let stream_cfg: cpal::StreamConfig = cfg.into();

    // Hop from the cpal callback to the ingest thread. Each send is one
    // callback's worth of mono samples (~10 ms), so 128 entries =
    // ~1.3 s of headroom — far more than the ingest thread (which only
    // resamples and writes to disk) can ever fall behind. Stays bounded
    // so a wedged ingest thread can't grow memory without bound, but
    // because the consumer is so light this should never fill in
    // practice.
    let (tx, rx) = bounded::<Vec<f32>>(128);

    let err_fn = |e| eprintln!("audio stream error: {e}");
    let cancel_audio = cancel.clone();
    let stream = match format {
        cpal::SampleFormat::F32 => {
            let tx = tx.clone();
            let cancel = cancel_audio.clone();
            device.build_input_stream(
                &stream_cfg,
                {
                    let paused = paused.clone();
                    move |data: &[f32], _| {
                        if cancel.load(Ordering::Relaxed) || paused.load(Ordering::Relaxed) {
                            return;
                        }
                        let _ = tx.try_send(downmix_f32(data, channels));
                    }
                },
                err_fn,
                None,
            )?
        }
        cpal::SampleFormat::I16 => {
            let tx = tx.clone();
            let cancel = cancel_audio.clone();
            device.build_input_stream(
                &stream_cfg,
                {
                    let paused = paused.clone();
                    move |data: &[i16], _| {
                        if cancel.load(Ordering::Relaxed) || paused.load(Ordering::Relaxed) {
                            return;
                        }
                        let f: Vec<f32> =
                            data.iter().map(|&s| s as f32 / i16::MAX as f32).collect();
                        let _ = tx.try_send(downmix_f32(&f, channels));
                    }
                },
                err_fn,
                None,
            )?
        }
        cpal::SampleFormat::U16 => {
            let tx = tx.clone();
            let cancel = cancel_audio.clone();
            device.build_input_stream(
                &stream_cfg,
                {
                    let paused = paused.clone();
                    move |data: &[u16], _| {
                        if cancel.load(Ordering::Relaxed) || paused.load(Ordering::Relaxed) {
                            return;
                        }
                        let f: Vec<f32> = data
                            .iter()
                            .map(|&s| (s as f32 - 32768.0) / 32768.0)
                            .collect();
                        let _ = tx.try_send(downmix_f32(&f, channels));
                    }
                },
                err_fn,
                None,
            )?
        }
        other => return Err(anyhow!("unsupported sample format: {other:?}")),
    };
    stream.play()?;
    // Drop our tx clone. After this the only tx live is inside the cpal
    // callback closures; once `stream` is dropped at the end of this
    // function those go away too, the channel disconnects, and the
    // ingest thread's `recv_timeout` returns Err — letting it clean up.
    drop(tx);

    // Whisper state has to come up before we spawn anything that
    // touches `buffer_dir` — a state-create failure here lets us bail
    // without orphaning a worker thread or leaving stale chunk files.
    let mut state = ctx
        .create_state()
        .map_err(|e| anyhow!("state create: {e}"))?;

    // Spawn the ingest thread. It owns rx, accumulates 5 s chunks at the
    // device rate, resamples each to TARGET_SR, and writes them to
    // sequenced files under buffer_dir. On cancel, it flushes whatever
    // tail it has if it's long enough to be worth transcribing.
    let ingest_buffer_dir = buffer_dir.clone();
    let ingest_cancel = cancel.clone();
    let ingest_handle = thread::spawn(move || {
        ingest_loop(rx, sr, ingest_buffer_dir, ingest_cancel);
    });

    // Inference loop runs on this thread. We read chunk files in
    // ascending seq order so cross-chunk context (set_no_context(false))
    // still primes the next chunk from the previous decode's tokens.
    let mut next_seq: u64 = 1;
    let mut ingest_alive = true;

    loop {
        let next_path = buffer_dir.join(format!("{next_seq:010}.f32"));
        if !next_path.exists() {
            // Nothing ready yet. If we were cancelled and the ingest
            // thread has finished writing its tail, we're done. Until
            // then, sit tight — short sleep beats a busy spin.
            if cancel.load(Ordering::SeqCst) && !ingest_alive {
                break;
            }
            if ingest_alive && ingest_handle.is_finished() {
                ingest_alive = false;
            }
            thread::sleep(Duration::from_millis(75));
            continue;
        }

        let samples = match read_f32_chunk(&next_path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("transcribe-buffer read failed for {next_path:?}: {e}");
                let _ = std::fs::remove_file(&next_path);
                next_seq += 1;
                continue;
            }
        };

        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_translate(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_special(false);
        params.set_print_timestamps(false);
        params.set_no_context(false);
        params.set_single_segment(true);
        if let Err(e) = state.full(params, &samples) {
            eprintln!("whisper full failed: {e}");
            let _ = std::fs::remove_file(&next_path);
            next_seq += 1;
            continue;
        }
        let n = state.full_n_segments().unwrap_or(0);
        let mut text = String::new();
        for i in 0..n {
            if let Ok(seg) = state.full_get_segment_text(i) {
                text.push_str(&seg);
            }
        }
        let _ = std::fs::remove_file(&next_path);
        next_seq += 1;

        let trimmed = text.trim();
        if !trimmed.is_empty() {
            let frame = TranscribeFrame {
                delta: format!("{trimmed} "),
                elapsed_ms: started.elapsed().as_millis(),
                is_final: false,
                pending_chunks: count_pending_chunks(&buffer_dir),
            };
            let _ = window.emit(event, frame);
        }
    }

    drop(stream);
    let _ = ingest_handle.join();
    let _ = std::fs::remove_dir_all(&buffer_dir);
    Ok(())
}

/// Inference-only counterpart of `run_session`. Reads sequenced f32
/// chunks from an existing buffer dir, runs whisper on each, deletes on
/// success, and exits when the dir contains no more chunks. We don't
/// open cpal here — the audio was captured by a previous (now dead)
/// session, so there's nothing to record.
fn run_drain(
    event: &str,
    stream_id: &str,
    model_path: &Path,
    cancel: Arc<AtomicBool>,
    window: &WebviewWindow,
) -> Result<()> {
    let started = std::time::Instant::now();
    let model_path_str = model_path
        .to_str()
        .ok_or_else(|| anyhow!("model path is not utf-8"))?;
    let ctx = WhisperContext::new_with_params(model_path_str, WhisperContextParameters::default())
        .map_err(|e| anyhow!("whisper init failed: {e}"))?;
    let buffer_dir = chunk_buffer_dir(stream_id)?;
    let mut state = ctx
        .create_state()
        .map_err(|e| anyhow!("state create: {e}"))?;

    // Find the lowest seq present so we don't try to process a hole at
    // seq 1 forever when a partial pre-crash flush left us starting at
    // seq 4 (e.g. the first three chunks were already drained before
    // the crash). Walking the dir once is fine — there can only be a
    // few thousand entries even on the worst backlog.
    let mut next_seq: u64 = lowest_pending_seq(&buffer_dir).unwrap_or(1);

    loop {
        let next_path = buffer_dir.join(format!("{next_seq:010}.f32"));
        if !next_path.exists() {
            // Nothing at this seq. If anything else is queued (a hole),
            // skip forward to the next real chunk; otherwise we're done.
            if cancel.load(Ordering::SeqCst) {
                break;
            }
            match lowest_pending_seq(&buffer_dir) {
                Some(s) if s > next_seq => {
                    next_seq = s;
                    continue;
                }
                Some(_) => {
                    // The lowest seq IS next_seq but the file vanished
                    // between the existence check and now (race with
                    // an external cleanup). Step over it.
                    next_seq += 1;
                    continue;
                }
                None => break, // dir is empty — drain complete.
            }
        }

        let samples = match read_f32_chunk(&next_path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("transcribe-buffer read failed for {next_path:?}: {e}");
                let _ = std::fs::remove_file(&next_path);
                next_seq += 1;
                continue;
            }
        };

        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_translate(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_special(false);
        params.set_print_timestamps(false);
        params.set_no_context(false);
        params.set_single_segment(true);
        if let Err(e) = state.full(params, &samples) {
            eprintln!("whisper full failed: {e}");
            let _ = std::fs::remove_file(&next_path);
            next_seq += 1;
            continue;
        }
        let n = state.full_n_segments().unwrap_or(0);
        let mut text = String::new();
        for i in 0..n {
            if let Ok(seg) = state.full_get_segment_text(i) {
                text.push_str(&seg);
            }
        }
        let _ = std::fs::remove_file(&next_path);
        next_seq += 1;

        let trimmed = text.trim();
        if !trimmed.is_empty() {
            let frame = TranscribeFrame {
                delta: format!("{trimmed} "),
                elapsed_ms: started.elapsed().as_millis(),
                is_final: false,
                pending_chunks: count_pending_chunks(&buffer_dir),
            };
            let _ = window.emit(event, frame);
        }
    }

    let _ = std::fs::remove_dir_all(&buffer_dir);
    Ok(())
}

/// Smallest `{seq}.f32` filename in `dir`, parsed as u64. Returns None if
/// no chunk file is present. Used by drain mode to skip leading holes
/// from a partially-drained pre-crash session.
fn lowest_pending_seq(dir: &Path) -> Option<u64> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut best: Option<u64> = None;
    for entry in entries.flatten() {
        let p = entry.path();
        if p.extension().and_then(|s| s.to_str()) != Some("f32") {
            continue;
        }
        let stem = match p.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s,
            None => continue,
        };
        if let Ok(n) = stem.parse::<u64>() {
            best = Some(best.map_or(n, |b| b.min(n)));
        }
    }
    best
}

/// Drain `rx`, downmix-already-applied samples accumulate at device
/// rate, resample to TARGET_SR each time we cross the chunk boundary,
/// and write the resulting chunk as `{seq:010}.f32` (raw little-endian
/// f32 mono). On cancel, flush a final partial chunk if it's at least
/// `TAIL_FLUSH_MIN_SECONDS` long — anything shorter is whisper-grade
/// noise.
fn ingest_loop(
    rx: Receiver<Vec<f32>>,
    device_sr: u32,
    buffer_dir: PathBuf,
    cancel: Arc<AtomicBool>,
) {
    let chunk_at_device_rate = (device_sr as f32 * CHUNK_SECONDS) as usize;
    let tail_min = (device_sr as f32 * TAIL_FLUSH_MIN_SECONDS) as usize;
    let mut buf: Vec<f32> = Vec::with_capacity(chunk_at_device_rate * 2);
    let mut seq: u64 = 1;

    let flush = |seq: u64, buf: &[f32], buffer_dir: &Path| {
        let resampled = resample_linear(buf, device_sr, TARGET_SR);
        let path = buffer_dir.join(format!("{seq:010}.f32"));
        if let Err(e) = write_f32_chunk(&path, &resampled) {
            eprintln!("transcribe-buffer write failed for {path:?}: {e}");
        }
    };

    loop {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(samples) => buf.extend_from_slice(&samples),
            Err(RecvTimeoutError::Timeout) => {
                if cancel.load(Ordering::SeqCst) {
                    break;
                }
                continue;
            }
            // Sender side closed (stream was dropped). Treat the same
            // as cancel so the tail still gets flushed.
            Err(RecvTimeoutError::Disconnected) => break,
        }
        while buf.len() >= chunk_at_device_rate {
            flush(seq, &buf[..chunk_at_device_rate], &buffer_dir);
            buf.drain(..chunk_at_device_rate);
            seq += 1;
        }
    }

    // Drain anything still queued (sender may have racing samples even
    // after cancel) before the tail check.
    while let Ok(samples) = rx.try_recv() {
        buf.extend_from_slice(&samples);
        while buf.len() >= chunk_at_device_rate {
            flush(seq, &buf[..chunk_at_device_rate], &buffer_dir);
            buf.drain(..chunk_at_device_rate);
            seq += 1;
        }
    }

    if buf.len() >= tail_min {
        flush(seq, &buf, &buffer_dir);
    }
}

/// Atomic-ish chunk write: `.tmp` then rename, so a partially-written
/// file can never be read by the inference loop. f32 → little-endian
/// bytes. We don't trust process-native endian here because someday
/// these chunk files might survive a process restart and we'd want
/// readers to see consistent bytes.
fn write_f32_chunk(path: &Path, samples: &[f32]) -> std::io::Result<()> {
    let tmp = path.with_extension("f32.tmp");
    let mut bytes = Vec::with_capacity(samples.len() * 4);
    for s in samples {
        bytes.extend_from_slice(&s.to_le_bytes());
    }
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&bytes)?;
        f.sync_data()?;
    }
    std::fs::rename(&tmp, path)
}

fn read_f32_chunk(path: &Path) -> std::io::Result<Vec<f32>> {
    let bytes = std::fs::read(path)?;
    Ok(bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect())
}

/// Count `.f32` files (excluding any in-flight `.tmp`) so the frame
/// can surface a backlog gauge to the UI. Errors on read are swallowed
/// — a transient failure here just means one frame reports 0; better
/// than aborting the inference loop.
fn count_pending_chunks(buffer_dir: &Path) -> u32 {
    let entries = match std::fs::read_dir(buffer_dir) {
        Ok(e) => e,
        Err(_) => return 0,
    };
    let mut n: u32 = 0;
    for entry in entries.flatten() {
        if entry.path().extension().and_then(|s| s.to_str()) == Some("f32") {
            n = n.saturating_add(1);
        }
    }
    n
}

/// Average across `channels` to produce mono samples. Whisper only takes
/// mono input and most consumer mics are stereo or quad, so this runs
/// unconditionally.
fn downmix_f32(data: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return data.to_vec();
    }
    let frames = data.len() / channels;
    let mut out = Vec::with_capacity(frames);
    for f in 0..frames {
        let mut sum = 0.0f32;
        for c in 0..channels {
            sum += data[f * channels + c];
        }
        out.push(sum / channels as f32);
    }
    out
}

/// Linear-interpolated resampling. Less accurate than a polyphase
/// filter, but cheap and good enough for whisper, which is robust to
/// mild resampling artefacts.
fn resample_linear(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to {
        return input.to_vec();
    }
    let ratio = from as f64 / to as f64;
    let out_len = (input.len() as f64 / ratio) as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let src = i as f64 * ratio;
        let idx = src as usize;
        let frac = (src - idx as f64) as f32;
        let a = input.get(idx).copied().unwrap_or(0.0);
        let b = input.get(idx + 1).copied().unwrap_or(a);
        out.push(a + (b - a) * frac);
    }
    out
}

/// Enumerate input devices via cpal so the Hardware → Microphone settings
/// page can populate its dropdown without going through the WebView's
/// `mediaDevices` API (which isn't exposed on every platform).
#[derive(Debug, Serialize, Clone)]
pub struct AudioInputDevice {
    pub name: String,
    pub is_default: bool,
}

pub fn list_input_devices() -> Result<Vec<AudioInputDevice>> {
    let host = cpal::default_host();
    let default_name = host.default_input_device().and_then(|d| d.name().ok());
    let mut out = Vec::new();
    for dev in host.input_devices()? {
        if let Ok(name) = dev.name() {
            let is_default = default_name.as_deref() == Some(name.as_str());
            out.push(AudioInputDevice { name, is_default });
        }
    }
    Ok(out)
}
