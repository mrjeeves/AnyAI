//! Local-only live transcription with optional speaker diarization.
//!
//! cpal captures from the default (or named) input device. Samples flow
//! through a small in-RAM hop into an *ingest* thread, which downmixes,
//! resamples to 16 kHz, accumulates `chunk_seconds`-second chunks, and
//! spills each chunk to disk under
//! `~/.myownllm/transcribe-buffer/{stream_id}/{seq}.f32`. A separate
//! *inference* thread reads chunks in sequence order, hands each to the
//! [`crate::asr::AsrBackend`] (Moonshine on Pi-class hardware, Parakeet
//! TDT on capable hardware), and emits text segments with timestamps.
//!
//! When the user enables "Identify speakers" on the transcribe pane, a
//! second worker runs the [`crate::diarize::DiarizeBackend`] on the
//! same chunks. A small join task combines the two streams: ASR
//! segments get tagged with the speaker whose turn most overlaps their
//! timing, then the result goes out as a `TranscribeFrame`.
//!
//! Chunk size is **backend-specific** (Moonshine wants 1 s, Parakeet
//! wants 1 s, a future whisper-style backend would want 5 s); the
//! ingest thread reads `backend.caps().chunk_seconds` once per session
//! and slices accordingly. Backpressure: if the on-disk backlog grows
//! past 300 s of audio while the mic is live, the oldest chunk is
//! dropped (favouring recent audio over historical accuracy) and the
//! UI is warned via a status frame.
//!
//! Nothing is sent over the network at runtime. Models live in
//! `~/.myownllm/models/asr/` and `~/.myownllm/models/diarize/`,
//! downloaded on demand via [`crate::models::pull_model`].

use anyhow::{anyhow, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_channel::{bounded, Receiver, RecvTimeoutError};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::Duration;
use tauri::{Emitter, WebviewWindow};

use crate::asr::{self, AsrBackend, AsrCaps, AsrSegment};
use crate::diarize::{self, DiarizeBackend, SpeakerTurn};
use crate::models::{self, ModelKind};

/// Target sample rate. Every ASR / diarize backend we ship is trained
/// on 16 kHz mono audio.
const TARGET_SR: u32 = 16_000;

/// Linear-amplitude RMS below which we treat a chunk as silence and
/// skip inference. Both Moonshine and Parakeet hallucinate on pure
/// silence (the canonical "Thanks for watching." phantom from the
/// whisper era), and pyannote-seg emits no voiced regions in silence
/// anyway. ~ -45 dBFS is well above ambient mic noise on a quiet
/// desktop and well below conversational speech (~0.05–0.3 RMS).
const SILENCE_RMS_THRESHOLD: f32 = 0.005;

/// Cap on the on-disk backlog (in seconds of audio). Beyond this we
/// drop the **oldest** pending chunk on every new ingest so the
/// transcript stays close to live rather than playing minutes-old
/// audio. Chosen larger than any plausible per-chunk inference time
/// even on a Pi 5.
const MAX_BACKLOG_SECONDS: f32 = 300.0;

/// Build a cpal `err_fn` closure that latches the first error into the
/// shared slot. Used per-branch in the sample-format match so each cpal
/// `build_input_stream` call gets its own owned closure (the closures
/// aren't `Copy` because they hold an `Arc<Mutex<…>>`). Runs on the
/// audio thread, so the body has to stay short.
fn stream_err_fn(
    slot: Arc<Mutex<Option<String>>>,
) -> impl FnMut(cpal::StreamError) + Send + 'static {
    move |e| {
        eprintln!("audio stream error: {e}");
        if let Ok(mut s) = slot.lock() {
            if s.is_none() {
                *s = Some(format!("{e}"));
            }
        }
    }
}

fn chunk_rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sumsq: f64 = samples.iter().map(|s| (*s as f64) * (*s as f64)).sum();
    (sumsq / samples.len() as f64).sqrt() as f32
}

/// Frame shape emitted on `myownllm://transcribe-stream/{stream_id}`.
///
/// v13 protocol: `segments` carries the structured output (start_ms,
/// end_ms, text, optional speaker). `is_final` signals the worker has
/// unwound (either user-stopped or errored). `pending_chunks` * the
/// session's `chunk_seconds` is how many seconds of audio are still
/// queued on disk — the UI surfaces this as a "behind realtime"
/// indicator. `chunk_seconds` is sent in the first frame and stays
/// constant for the session.
#[derive(Debug, Serialize, Clone)]
pub struct TranscribeFrame {
    pub elapsed_ms: u128,
    pub segments: Vec<EmittedSegment>,
    #[serde(rename = "final")]
    pub is_final: bool,
    pub pending_chunks: u32,
    /// Set on the first frame of every session so the UI knows the
    /// cadence at which `pending_chunks` accrues. None after the
    /// first frame.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chunk_seconds: Option<f32>,
    /// Ephemeral state surfaced as a subtitle ("Loading model…",
    /// "Listening…", "Low mic level", inference errors). None clears
    /// the status display.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

/// One unit of ASR output, optionally tagged with a speaker.
#[derive(Debug, Serialize, Clone)]
pub struct EmittedSegment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
    /// Cluster ID assigned by the diarize worker, or `None` when
    /// diarization is off / hasn't seen this segment yet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speaker: Option<u32>,
    /// `true` when pyannote reported overlapping speakers in this
    /// segment's timing window. The text is usually garbled (two
    /// voices mixed into one stream); the UI flags it but doesn't
    /// try to split.
    #[serde(default, skip_serializing_if = "is_false")]
    pub overlap: bool,
    /// `true` while the segment's speaker assignment is still
    /// provisional (cold-start cluster warm-up window). After the
    /// first ~30 s of audio the worker re-emits provisional segments
    /// with stable IDs.
    #[serde(default, skip_serializing_if = "is_false")]
    pub provisional: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl TranscribeFrame {
    fn heartbeat(
        elapsed_ms: u128,
        pending_chunks: u32,
        chunk_seconds: Option<f32>,
        status: Option<String>,
    ) -> Self {
        Self {
            elapsed_ms,
            segments: Vec::new(),
            is_final: false,
            pending_chunks,
            chunk_seconds,
            status,
        }
    }
}

struct Session {
    cancel: Arc<AtomicBool>,
    /// When set, cpal callbacks early-return instead of forwarding
    /// samples to the ingest thread. The inference loop keeps
    /// draining whatever's already on disk — so the user can pause
    /// mic capture and let the backlog catch up without losing the
    /// running session. Resume just flips this back. Inference-only
    /// ("drain") sessions never read it.
    paused: Arc<AtomicBool>,
}

fn sessions() -> &'static DashMap<String, Session> {
    static M: OnceLock<DashMap<String, Session>> = OnceLock::new();
    M.get_or_init(DashMap::new)
}

/// Per-session directory holding 16 kHz mono f32 chunk files queued
/// for inference. Created at session start, emptied on entry
/// (defensive cleanup against a previous crashed session leaving
/// stale chunks), and removed entirely on session end.
fn chunk_buffer_dir(stream_id: &str) -> Result<PathBuf> {
    let dir = crate::myownllm_dir()?
        .join("transcribe-buffer")
        .join(sanitize_stream_id(stream_id));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Root of the per-session chunk directories. Used by storage /
/// recovery helpers that walk every stream the way Disk Usage does,
/// rather than drilling into one stream by id.
fn buffer_root() -> Result<PathBuf> {
    Ok(crate::myownllm_dir()?.join("transcribe-buffer"))
}

/// Recursive size of `~/.myownllm/transcribe-buffer/`. The Storage
/// tab surfaces this so the user can see how much disk a slow ASR
/// backlog is parked on. Errors collapse to 0 — a missing dir is the
/// steady state when there's no recording happening.
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

/// `_meta.json` written into a session's chunk dir on start so a
/// later drain-only resumption can recover the runtime + model name
/// without the user having to remember.
#[derive(Serialize, Deserialize, Clone)]
struct BufferMeta {
    runtime: String,
    model: String,
    /// If diarize was on when the chunks were spilled, the composite
    /// name is here so drain can re-warm the same pipeline.
    #[serde(default)]
    diarize_model: Option<String>,
}

fn write_meta(buffer_dir: &Path, runtime: &str, model: &str, diarize_model: Option<&str>) {
    let meta = BufferMeta {
        runtime: runtime.to_string(),
        model: model.to_string(),
        diarize_model: diarize_model.map(str::to_string),
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

/// One pending stream entry, surfaced on app start so the UI can
/// offer to drain whatever was left over from a crashed previous
/// session.
#[derive(Debug, Serialize, Clone)]
pub struct PendingStream {
    pub stream_id: String,
    pub pending_chunks: u32,
    pub runtime: Option<String>,
    pub model: Option<String>,
    pub diarize_model: Option<String>,
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
        // already being drained by the running worker and surfacing
        // them here would invite a double-start race.
        if sessions().contains_key(&stream_id) {
            continue;
        }
        let meta = read_meta(&path);
        out.push(PendingStream {
            stream_id,
            pending_chunks: pending,
            runtime: meta.as_ref().map(|m| m.runtime.clone()),
            model: meta.as_ref().map(|m| m.model.clone()),
            diarize_model: meta.and_then(|m| m.diarize_model),
        });
    }
    out
}

/// `stream_id` comes from the frontend (UUIDs in practice), but we
/// don't trust callers — strip anything that isn't a-z, 0-9, `-`, or
/// `_` so the path can't escape `~/.myownllm/transcribe-buffer/`.
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

/// Spin up an audio capture + inference (+ optional diarize) worker
/// for `stream_id`. Returns once the worker is alive; the actual
/// transcript flows back through
/// `myownllm://transcribe-stream/{stream_id}` events.
pub fn start(
    stream_id: String,
    runtime: String,
    model_name: String,
    device_name: Option<String>,
    diarize_model: Option<String>,
    window: WebviewWindow,
) -> Result<()> {
    if sessions().contains_key(&stream_id) {
        return Err(anyhow!("transcription {stream_id} is already running"));
    }
    if !models::find(&model_name, ModelKind::Asr).map(models::is_installed).unwrap_or(false) {
        return Err(anyhow!(
            "ASR model '{model_name}' ({runtime}) isn't installed yet — pull it first from Settings → Transcription."
        ));
    }
    if let Some(d) = &diarize_model {
        if !models::composite_installed(d, ModelKind::Diarize) {
            return Err(anyhow!(
                "diarize model '{d}' isn't installed yet — toggle off diarization or pull it first."
            ));
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
    let runtime_for_thread = runtime.clone();
    let model_for_thread = model_name.clone();
    let diarize_for_thread = diarize_model.clone();
    thread::spawn(move || {
        let event = format!("myownllm://transcribe-stream/{stream_id_for_thread}");
        let res = run_session(
            &event,
            &stream_id_for_thread,
            &runtime_for_thread,
            &model_for_thread,
            diarize_for_thread.as_deref(),
            device_name.as_deref(),
            cancel_for_thread,
            paused_for_thread,
            &window,
        );
        sessions().remove(&stream_id_for_thread);
        let final_frame = match res {
            Ok(()) => TranscribeFrame {
                elapsed_ms: 0,
                segments: Vec::new(),
                is_final: true,
                pending_chunks: 0,
                chunk_seconds: None,
                status: None,
            },
            Err(e) => TranscribeFrame {
                elapsed_ms: 0,
                segments: Vec::new(),
                is_final: true,
                pending_chunks: 0,
                chunk_seconds: None,
                status: Some(format!("transcription error: {e}")),
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

/// Start an inference-only worker against an existing buffer dir.
/// Used when MyOwnLLM relaunches and finds chunks left over from a
/// previous session — we don't open the mic, we just chew through
/// what's there and emit segments the same way a normal session
/// would. The worker exits as soon as the buffer is empty (or on
/// cancel).
pub fn start_drain(
    stream_id: String,
    runtime: String,
    model_name: String,
    diarize_model: Option<String>,
    window: WebviewWindow,
) -> Result<()> {
    if sessions().contains_key(&stream_id) {
        return Err(anyhow!("transcription {stream_id} is already running"));
    }
    if !models::find(&model_name, ModelKind::Asr).map(models::is_installed).unwrap_or(false) {
        return Err(anyhow!(
            "ASR model '{model_name}' isn't installed — install it from Settings → Models."
        ));
    }
    let cancel = Arc::new(AtomicBool::new(false));
    sessions().insert(
        stream_id.clone(),
        Session {
            cancel: cancel.clone(),
            paused: Arc::new(AtomicBool::new(false)),
        },
    );

    let stream_id_for_thread = stream_id.clone();
    let cancel_for_thread = cancel.clone();
    thread::spawn(move || {
        let event = format!("myownllm://transcribe-stream/{stream_id_for_thread}");
        let res = run_drain(
            &event,
            &stream_id_for_thread,
            &runtime,
            &model_name,
            diarize_model.as_deref(),
            cancel_for_thread,
            &window,
        );
        sessions().remove(&stream_id_for_thread);
        let final_frame = match res {
            Ok(()) => TranscribeFrame {
                elapsed_ms: 0,
                segments: Vec::new(),
                is_final: true,
                pending_chunks: 0,
                chunk_seconds: None,
                status: None,
            },
            Err(e) => TranscribeFrame {
                elapsed_ms: 0,
                segments: Vec::new(),
                is_final: true,
                pending_chunks: 0,
                chunk_seconds: None,
                status: Some(format!("transcription error: {e}")),
            },
        };
        let _ = window.emit(&event, final_frame);
    });
    Ok(())
}

/// Transcribe an existing audio file. Decodes via symphonia,
/// downmixes to mono + resamples to 16 kHz, runs the chosen ASR
/// backend on chunks the same way a live session does. Lifecycle
/// mirrors `start_drain`: no mic is touched, the user gets one final
/// frame on completion.
pub fn start_upload(
    stream_id: String,
    runtime: String,
    model_name: String,
    file_path: PathBuf,
    diarize_model: Option<String>,
    window: WebviewWindow,
) -> Result<()> {
    if sessions().contains_key(&stream_id) {
        return Err(anyhow!("transcription {stream_id} is already running"));
    }
    if !models::find(&model_name, ModelKind::Asr).map(models::is_installed).unwrap_or(false) {
        return Err(anyhow!(
            "ASR model '{model_name}' isn't installed — install it from Settings → Models."
        ));
    }
    if !file_path.exists() {
        return Err(anyhow!("audio file not found: {}", file_path.display()));
    }
    let cancel = Arc::new(AtomicBool::new(false));
    sessions().insert(
        stream_id.clone(),
        Session {
            cancel: cancel.clone(),
            paused: Arc::new(AtomicBool::new(false)),
        },
    );

    let stream_id_for_thread = stream_id.clone();
    let cancel_for_thread = cancel.clone();
    thread::spawn(move || {
        let event = format!("myownllm://transcribe-stream/{stream_id_for_thread}");
        let res = run_upload(
            &event,
            &runtime,
            &model_name,
            &file_path,
            diarize_model.as_deref(),
            cancel_for_thread,
            &window,
        );
        sessions().remove(&stream_id_for_thread);
        let final_frame = match res {
            Ok(()) => TranscribeFrame {
                elapsed_ms: 0,
                segments: Vec::new(),
                is_final: true,
                pending_chunks: 0,
                chunk_seconds: None,
                status: None,
            },
            Err(e) => TranscribeFrame {
                elapsed_ms: 0,
                segments: Vec::new(),
                is_final: true,
                pending_chunks: 0,
                chunk_seconds: None,
                status: Some(format!("transcription error: {e}")),
            },
        };
        let _ = window.emit(&event, final_frame);
    });
    Ok(())
}

/// Build + warm up the ASR + (optional) diarize backends. Returns
/// `(asr, diarize_opt, caps)` ready for the chunk loop.
fn build_backends(
    runtime: &str,
    model_name: &str,
    diarize_composite: Option<&str>,
) -> Result<(Box<dyn AsrBackend>, Option<Box<dyn DiarizeBackend>>, AsrCaps)> {
    let mut asr = asr::make_backend(runtime, model_name)?;
    asr.warm_up()?;
    let caps = asr.caps();

    let diarize = if let Some(name) = diarize_composite {
        let mut d = diarize::make_backend("pyannote-diarize", name)?;
        d.warm_up()?;
        Some(d)
    } else {
        None
    };

    Ok((asr, diarize, caps))
}

#[allow(clippy::too_many_arguments)]
fn run_session(
    event: &str,
    stream_id: &str,
    runtime: &str,
    model_name: &str,
    diarize_composite: Option<&str>,
    device_name: Option<&str>,
    cancel: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    window: &WebviewWindow,
) -> Result<()> {
    let started = std::time::Instant::now();
    let _ = window.emit(
        event,
        TranscribeFrame::heartbeat(
            0,
            0,
            None,
            Some(format!("Loading {} model…", runtime)),
        ),
    );

    let (mut asr, mut diarize, caps) =
        build_backends(runtime, model_name, diarize_composite)?;

    let buffer_dir = chunk_buffer_dir(stream_id)?;
    if let Ok(entries) = std::fs::read_dir(&buffer_dir) {
        for entry in entries.flatten() {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    write_meta(&buffer_dir, runtime, model_name, diarize_composite);

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

    let (tx, rx) = bounded::<Vec<f32>>(128);

    let stream_err: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
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
                stream_err_fn(stream_err.clone()),
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
                stream_err_fn(stream_err.clone()),
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
                stream_err_fn(stream_err.clone()),
                None,
            )?
        }
        other => return Err(anyhow!("unsupported sample format: {other:?}")),
    };
    stream.play()?;
    drop(tx);

    // Ingest thread: accumulates `caps.chunk_seconds`-second chunks at
    // device rate, resamples each to 16 kHz, writes them as
    // `{seq:010}.f32` under `buffer_dir`. On cancel flushes any tail
    // that's ≥ `min_tail_seconds` long.
    let ingest_buffer_dir = buffer_dir.clone();
    let ingest_cancel = cancel.clone();
    let ingest_caps = caps;
    let ingest_event = event.to_string();
    let ingest_window = window.clone();
    let ingest_handle = thread::spawn(move || {
        ingest_loop(
            rx,
            sr,
            ingest_buffer_dir,
            ingest_caps,
            ingest_cancel,
            &ingest_event,
            &ingest_window,
            started,
        );
    });

    // First frame announces the cadence.
    let _ = window.emit(
        event,
        TranscribeFrame::heartbeat(
            started.elapsed().as_millis(),
            0,
            Some(caps.chunk_seconds),
            Some(format!("Listening… first chunk in ~{:.0} s", caps.chunk_seconds)),
        ),
    );

    let mut next_seq: u64 = 1;
    let mut chunks_since_reset: u64 = 0;
    let mut chunk_t0_ms: u64 = 0;

    loop {
        if cancel.load(Ordering::SeqCst) {
            break;
        }
        if let Some(err) = stream_err.lock().ok().and_then(|mut s| s.take()) {
            return Err(anyhow!("audio capture failed: {err}"));
        }
        let next_path = buffer_dir.join(format!("{next_seq:010}.f32"));
        if !next_path.exists() {
            thread::sleep(Duration::from_millis(50));
            continue;
        }

        let samples = match read_f32_chunk(&next_path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("transcribe-buffer read failed for {next_path:?}: {e}");
                let _ = std::fs::remove_file(&next_path);
                next_seq += 1;
                let _ = window.emit(
                    event,
                    TranscribeFrame::heartbeat(
                        started.elapsed().as_millis(),
                        count_pending_chunks(&buffer_dir),
                        None,
                        Some(format!("Chunk read failed: {e}")),
                    ),
                );
                continue;
            }
        };

        let chunk_ms = (samples.len() as u64 * 1000) / TARGET_SR as u64;
        let rms = chunk_rms(&samples);
        if rms < SILENCE_RMS_THRESHOLD {
            let _ = std::fs::remove_file(&next_path);
            next_seq += 1;
            chunk_t0_ms += chunk_ms;
            let _ = window.emit(
                event,
                TranscribeFrame::heartbeat(
                    started.elapsed().as_millis(),
                    count_pending_chunks(&buffer_dir),
                    None,
                    Some(format!(
                        "Low mic level (RMS {rms:.4} < {SILENCE_RMS_THRESHOLD})"
                    )),
                ),
            );
            continue;
        }

        let asr_out = match asr.process_chunk(&samples, chunk_t0_ms, &cancel) {
            Ok(o) => o,
            Err(e) => {
                if cancel.load(Ordering::SeqCst) {
                    break;
                }
                eprintln!("ASR inference failed: {e}");
                let _ = std::fs::remove_file(&next_path);
                next_seq += 1;
                chunk_t0_ms += chunk_ms;
                let _ = window.emit(
                    event,
                    TranscribeFrame::heartbeat(
                        started.elapsed().as_millis(),
                        count_pending_chunks(&buffer_dir),
                        None,
                        Some(format!("ASR inference error: {e}")),
                    ),
                );
                continue;
            }
        };

        // Diarize on the same chunk, in series. (Running in parallel
        // with rayon would shave a bit of latency but complicates
        // cancel handling; the win is modest given the diarize stage
        // is faster than the ASR stage on every tier we ship.)
        let turns: Vec<SpeakerTurn> = if let Some(d) = diarize.as_mut() {
            match d.process_chunk(&samples, chunk_t0_ms, &cancel) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("diarize inference failed: {e}");
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        };

        let _ = std::fs::remove_file(&next_path);
        next_seq += 1;

        let mut segments = join_segments(&asr_out.segments, &turns, chunk_t0_ms);
        chunk_t0_ms += chunk_ms;

        // Filter out empties before emitting.
        segments.retain(|s| !s.text.trim().is_empty());

        let frame = if !segments.is_empty() {
            TranscribeFrame {
                elapsed_ms: started.elapsed().as_millis(),
                segments,
                is_final: false,
                pending_chunks: count_pending_chunks(&buffer_dir),
                chunk_seconds: None,
                status: None,
            }
        } else {
            TranscribeFrame::heartbeat(
                started.elapsed().as_millis(),
                count_pending_chunks(&buffer_dir),
                None,
                Some("No speech detected in this chunk".into()),
            )
        };
        let _ = window.emit(event, frame);

        if asr_out.used_state && caps.state_reset_chunks > 0 {
            chunks_since_reset += 1;
            if chunks_since_reset >= caps.state_reset_chunks {
                chunks_since_reset = 0;
                asr.reset_state();
            }
        }
    }

    drop(stream);
    let _ = ingest_handle.join();
    let _ = std::fs::remove_dir_all(&buffer_dir);
    Ok(())
}

fn run_drain(
    event: &str,
    stream_id: &str,
    runtime: &str,
    model_name: &str,
    diarize_composite: Option<&str>,
    cancel: Arc<AtomicBool>,
    window: &WebviewWindow,
) -> Result<()> {
    let started = std::time::Instant::now();
    let _ = window.emit(
        event,
        TranscribeFrame::heartbeat(
            0,
            0,
            None,
            Some(format!("Loading {} model…", runtime)),
        ),
    );
    let (mut asr, mut diarize, caps) =
        build_backends(runtime, model_name, diarize_composite)?;
    let buffer_dir = chunk_buffer_dir(stream_id)?;

    let mut next_seq: u64 = lowest_pending_seq(&buffer_dir).unwrap_or(1);
    let mut chunks_since_reset: u64 = 0;
    let mut chunk_t0_ms: u64 = 0;
    let initial_pending = count_pending_chunks(&buffer_dir);
    let _ = window.emit(
        event,
        TranscribeFrame::heartbeat(
            started.elapsed().as_millis(),
            initial_pending,
            Some(caps.chunk_seconds),
            Some(format!("Draining {initial_pending} recovered chunk(s)…")),
        ),
    );

    loop {
        if cancel.load(Ordering::SeqCst) {
            break;
        }
        let next_path = buffer_dir.join(format!("{next_seq:010}.f32"));
        if !next_path.exists() {
            match lowest_pending_seq(&buffer_dir) {
                Some(s) if s > next_seq => {
                    next_seq = s;
                    continue;
                }
                Some(_) => {
                    next_seq += 1;
                    continue;
                }
                None => break,
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
        let chunk_ms = (samples.len() as u64 * 1000) / TARGET_SR as u64;

        if chunk_rms(&samples) < SILENCE_RMS_THRESHOLD {
            let _ = std::fs::remove_file(&next_path);
            next_seq += 1;
            chunk_t0_ms += chunk_ms;
            continue;
        }

        let asr_out = match asr.process_chunk(&samples, chunk_t0_ms, &cancel) {
            Ok(o) => o,
            Err(e) => {
                if cancel.load(Ordering::SeqCst) {
                    break;
                }
                eprintln!("ASR inference failed: {e}");
                let _ = std::fs::remove_file(&next_path);
                next_seq += 1;
                chunk_t0_ms += chunk_ms;
                continue;
            }
        };
        let turns: Vec<SpeakerTurn> = if let Some(d) = diarize.as_mut() {
            d.process_chunk(&samples, chunk_t0_ms, &cancel)
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        let _ = std::fs::remove_file(&next_path);
        next_seq += 1;

        let mut segments = join_segments(&asr_out.segments, &turns, chunk_t0_ms);
        chunk_t0_ms += chunk_ms;
        segments.retain(|s| !s.text.trim().is_empty());

        if !segments.is_empty() {
            let _ = window.emit(
                event,
                TranscribeFrame {
                    elapsed_ms: started.elapsed().as_millis(),
                    segments,
                    is_final: false,
                    pending_chunks: count_pending_chunks(&buffer_dir),
                    chunk_seconds: None,
                    status: None,
                },
            );
        }

        if asr_out.used_state && caps.state_reset_chunks > 0 {
            chunks_since_reset += 1;
            if chunks_since_reset >= caps.state_reset_chunks {
                chunks_since_reset = 0;
                asr.reset_state();
            }
        }
    }

    let _ = std::fs::remove_dir_all(&buffer_dir);
    Ok(())
}

fn run_upload(
    event: &str,
    runtime: &str,
    model_name: &str,
    file_path: &Path,
    diarize_composite: Option<&str>,
    cancel: Arc<AtomicBool>,
    window: &WebviewWindow,
) -> Result<()> {
    use std::fs::File;
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
    use symphonia::core::errors::Error as SymError;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let started = std::time::Instant::now();
    let _ = window.emit(
        event,
        TranscribeFrame::heartbeat(
            0,
            0,
            None,
            Some(format!("Loading {} model…", runtime)),
        ),
    );
    let (mut asr, mut diarize, caps) =
        build_backends(runtime, model_name, diarize_composite)?;

    let file = File::open(file_path).map_err(|e| anyhow!("open audio file: {e}"))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = file_path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| anyhow!("probe audio: {e}"))?;
    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| anyhow!("no audio track in {}", file_path.display()))?;
    let track_id = track.id;
    let codec_params = track.codec_params.clone();
    let src_rate = codec_params
        .sample_rate
        .ok_or_else(|| anyhow!("audio file has no declared sample rate"))?;
    let src_channels = codec_params.channels.map(|c| c.count()).unwrap_or(1);

    let mut decoder = symphonia::default::get_codecs()
        .make(&codec_params, &DecoderOptions::default())
        .map_err(|e| anyhow!("make decoder: {e}"))?;

    let chunk_at_src_rate = (src_rate as f32 * caps.chunk_seconds) as usize;
    let tail_min_src = (src_rate as f32 * caps.min_tail_seconds) as usize;
    let mut buf: Vec<f32> = Vec::with_capacity(chunk_at_src_rate * 2);
    let mut sb: Option<SampleBuffer<f32>> = None;
    let mut chunk_t0_ms: u64 = 0;
    let mut chunks_since_reset: u64 = 0;

    'outer: loop {
        if cancel.load(Ordering::SeqCst) {
            break;
        }
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymError::IoError(ref e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                break;
            }
            Err(e) => return Err(anyhow!("symphonia read packet: {e}")),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(SymError::IoError(_)) => continue,
            Err(SymError::DecodeError(_)) => continue,
            Err(e) => return Err(anyhow!("symphonia decode: {e}")),
        };
        let frames = decoded.frames();
        let spec = *decoded.spec();
        let sb = match sb.as_mut() {
            Some(b) => {
                if (b.capacity() as usize) < decoded.capacity() {
                    sb = Some(SampleBuffer::new(decoded.capacity() as u64, spec));
                    sb.as_mut().unwrap()
                } else {
                    b
                }
            }
            None => {
                sb = Some(SampleBuffer::new(decoded.capacity() as u64, spec));
                sb.as_mut().unwrap()
            }
        };
        sb.copy_interleaved_ref(decoded);
        let samples = sb.samples();
        if src_channels == 1 {
            buf.extend_from_slice(samples);
        } else {
            for f in 0..frames {
                let base = f * src_channels;
                let mut sum = 0.0f32;
                for c in 0..src_channels {
                    sum += samples[base + c];
                }
                buf.push(sum / src_channels as f32);
            }
        }

        while buf.len() >= chunk_at_src_rate {
            if cancel.load(Ordering::SeqCst) {
                break 'outer;
            }
            let chunk: Vec<f32> = buf.drain(..chunk_at_src_rate).collect();
            let resampled = resample_linear(&chunk, src_rate, TARGET_SR);
            let chunk_ms = (resampled.len() as u64 * 1000) / TARGET_SR as u64;
            if chunk_rms(&resampled) < SILENCE_RMS_THRESHOLD {
                chunk_t0_ms += chunk_ms;
                continue;
            }
            let asr_out = match asr.process_chunk(&resampled, chunk_t0_ms, &cancel) {
                Ok(o) => o,
                Err(e) => {
                    if cancel.load(Ordering::SeqCst) {
                        break 'outer;
                    }
                    eprintln!("ASR inference failed: {e}");
                    chunk_t0_ms += chunk_ms;
                    continue;
                }
            };
            let turns: Vec<SpeakerTurn> = if let Some(d) = diarize.as_mut() {
                d.process_chunk(&resampled, chunk_t0_ms, &cancel)
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            let mut segments = join_segments(&asr_out.segments, &turns, chunk_t0_ms);
            chunk_t0_ms += chunk_ms;
            segments.retain(|s| !s.text.trim().is_empty());

            if !segments.is_empty() {
                let _ = window.emit(
                    event,
                    TranscribeFrame {
                        elapsed_ms: started.elapsed().as_millis(),
                        segments,
                        is_final: false,
                        pending_chunks: 0,
                        chunk_seconds: None,
                        status: None,
                    },
                );
            }

            if asr_out.used_state && caps.state_reset_chunks > 0 {
                chunks_since_reset += 1;
                if chunks_since_reset >= caps.state_reset_chunks {
                    chunks_since_reset = 0;
                    asr.reset_state();
                }
            }
        }
    }

    // Tail
    if !cancel.load(Ordering::SeqCst) && buf.len() >= tail_min_src {
        let resampled = resample_linear(&buf, src_rate, TARGET_SR);
        if chunk_rms(&resampled) >= SILENCE_RMS_THRESHOLD {
            if let Ok(asr_out) = asr.process_chunk(&resampled, chunk_t0_ms, &cancel) {
                let turns: Vec<SpeakerTurn> = if let Some(d) = diarize.as_mut() {
                    d.process_chunk(&resampled, chunk_t0_ms, &cancel)
                        .unwrap_or_default()
                } else {
                    Vec::new()
                };
                let mut segments = join_segments(&asr_out.segments, &turns, chunk_t0_ms);
                segments.retain(|s| !s.text.trim().is_empty());
                if !segments.is_empty() {
                    let _ = window.emit(
                        event,
                        TranscribeFrame {
                            elapsed_ms: started.elapsed().as_millis(),
                            segments,
                            is_final: false,
                            pending_chunks: 0,
                            chunk_seconds: None,
                            status: None,
                        },
                    );
                }
            }
        }
    }

    Ok(())
}

/// Align ASR segments to diarize speaker turns by timestamp overlap.
/// Each ASR segment's `start_ms` / `end_ms` is relative to the chunk
/// start; the chunk's `chunk_t0_ms` is added before comparing to
/// turns (which are session-relative). The speaker for an ASR segment
/// is the turn that overlaps it most (ties → earlier start). When no
/// turn overlaps, `speaker` is `None`. Overlap-flagged turns
/// propagate the flag onto the resulting segment.
fn join_segments(asr_segments: &[AsrSegment], turns: &[SpeakerTurn], chunk_t0_ms: u64) -> Vec<EmittedSegment> {
    let mut out = Vec::with_capacity(asr_segments.len());
    for seg in asr_segments {
        let seg_abs_start = chunk_t0_ms + seg.start_ms;
        let seg_abs_end = chunk_t0_ms + seg.end_ms;
        let mut best: Option<(&SpeakerTurn, u64)> = None;
        for turn in turns {
            let lo = seg_abs_start.max(turn.start_ms);
            let hi = seg_abs_end.min(turn.end_ms);
            if hi > lo {
                let overlap_ms = hi - lo;
                if best.map(|(_, o)| overlap_ms > o).unwrap_or(true) {
                    best = Some((turn, overlap_ms));
                }
            }
        }
        let (speaker, overlap) = match best {
            Some((t, _)) => (Some(t.speaker), t.overlap),
            None => (None, false),
        };
        out.push(EmittedSegment {
            start_ms: seg_abs_start,
            end_ms: seg_abs_end,
            text: seg.text.clone(),
            speaker,
            overlap,
            provisional: false,
        });
    }
    out
}

/// Smallest `{seq}.f32` filename in `dir`, parsed as u64. None if no
/// chunk file is present.
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

/// Drain `rx`, accumulate at device rate, resample to TARGET_SR each
/// time we cross the chunk boundary, write `{seq:010}.f32`. On cancel
/// flush a final partial chunk if it's at least `min_tail_seconds`
/// long. Enforces the backlog cap: if more than `MAX_BACKLOG_SECONDS`
/// of chunks accumulate, delete the oldest before writing the new
/// one and warn via a status frame.
#[allow(clippy::too_many_arguments)]
fn ingest_loop(
    rx: Receiver<Vec<f32>>,
    device_sr: u32,
    buffer_dir: PathBuf,
    caps: AsrCaps,
    cancel: Arc<AtomicBool>,
    event: &str,
    window: &WebviewWindow,
    started: std::time::Instant,
) {
    let chunk_at_device_rate = (device_sr as f32 * caps.chunk_seconds) as usize;
    let tail_min = (device_sr as f32 * caps.min_tail_seconds) as usize;
    let mut buf: Vec<f32> = Vec::with_capacity(chunk_at_device_rate * 2);
    let mut seq: u64 = 1;
    let max_backlog_chunks =
        ((MAX_BACKLOG_SECONDS / caps.chunk_seconds.max(0.1)).ceil() as u32).max(1);

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
            Err(RecvTimeoutError::Disconnected) => break,
        }
        while buf.len() >= chunk_at_device_rate {
            // Backpressure: if we're already piled up past the cap,
            // drop the oldest f32 file so the worker stays close to
            // realtime. Surface as a status frame so the UI can warn.
            let pending = count_pending_chunks(&buffer_dir);
            if pending >= max_backlog_chunks {
                if let Some(oldest) = lowest_pending_seq(&buffer_dir) {
                    let p = buffer_dir.join(format!("{oldest:010}.f32"));
                    let _ = std::fs::remove_file(&p);
                }
                let _ = window.emit(
                    event,
                    TranscribeFrame::heartbeat(
                        started.elapsed().as_millis(),
                        count_pending_chunks(&buffer_dir),
                        None,
                        Some(format!(
                            "Backlog full ({:.0} s); dropping oldest chunk to stay live.",
                            MAX_BACKLOG_SECONDS
                        )),
                    ),
                );
            }
            flush(seq, &buf[..chunk_at_device_rate], &buffer_dir);
            buf.drain(..chunk_at_device_rate);
            seq += 1;
        }
    }

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

/// Average across `channels` to produce mono samples.
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

/// Linear-interpolated resampling. Cheap, good enough for the
/// preprocessing step before a Mel front-end or raw-waveform encoder.
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

/// Enumerate input devices via cpal so the Hardware → Microphone
/// settings page can populate its dropdown.
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
