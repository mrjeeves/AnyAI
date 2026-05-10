//! Local-only live transcription.
//!
//! cpal captures from the default (or named) input device, the buffer is
//! downmixed to mono and resampled to 16 kHz in fixed 5-second chunks, and
//! whisper-rs transcribes each chunk independently. Text deltas stream
//! back to the frontend via Tauri events keyed by `stream_id`.
//!
//! Nothing is sent over the network at runtime. The whisper model is
//! loaded from `~/.anyai/whisper/ggml-{name}.bin`, which is downloaded on
//! demand by `whisper_model_pull` (see below). No model files ship with
//! the binary.

use anyhow::{anyhow, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_channel::bounded;
use dashmap::DashMap;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread;
use tauri::{Emitter, WebviewWindow};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

/// Target sample rate for whisper. The ggml models are all trained on
/// 16 kHz mono audio.
const TARGET_SR: u32 = 16_000;
/// Length of each independent transcription chunk. 5 s gives whisper
/// enough context for sensible word boundaries without making users wait
/// too long for the first text to appear.
const CHUNK_SECONDS: f32 = 5.0;

/// Frame shape emitted on `anyai://transcribe-stream/{stream_id}`. `delta`
/// is the new text since the last frame; the frontend appends. `final`
/// signals the worker has unwound (either user-stopped or errored).
#[derive(Debug, Serialize, Clone)]
pub struct TranscribeFrame {
    pub delta: String,
    pub elapsed_ms: u128,
    #[serde(rename = "final")]
    pub is_final: bool,
}

struct Session {
    cancel: Arc<AtomicBool>,
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
    ("tiny.en", 77_704_715, 50_000_000),
    ("base.en", 147_964_211, 100_000_000),
    ("small.en", 487_614_201, 400_000_000),
    ("medium.en", 1_533_774_781, 1_300_000_000),
    ("large-v3-turbo", 1_624_555_275, 1_400_000_000),
    ("large-v3", 3_095_033_483, 2_700_000_000),
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
    sessions().insert(
        stream_id.clone(),
        Session {
            cancel: cancel.clone(),
        },
    );

    let stream_id_for_thread = stream_id.clone();
    let cancel_for_thread = cancel.clone();
    thread::spawn(move || {
        let event = format!("anyai://transcribe-stream/{stream_id_for_thread}");
        let res = run_session(
            &event,
            &model_path,
            device_name.as_deref(),
            cancel_for_thread,
            &window,
        );
        sessions().remove(&stream_id_for_thread);
        let final_frame = match res {
            Ok(()) => TranscribeFrame {
                delta: String::new(),
                elapsed_ms: 0,
                is_final: true,
            },
            Err(e) => TranscribeFrame {
                delta: format!("[transcription error: {e}]"),
                elapsed_ms: 0,
                is_final: true,
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

fn run_session(
    event: &str,
    model_path: &Path,
    device_name: Option<&str>,
    cancel: Arc<AtomicBool>,
    window: &WebviewWindow,
) -> Result<()> {
    let started = std::time::Instant::now();
    let model_path_str = model_path
        .to_str()
        .ok_or_else(|| anyhow!("model path is not utf-8"))?;
    let ctx = WhisperContext::new_with_params(model_path_str, WhisperContextParameters::default())
        .map_err(|e| anyhow!("whisper init failed: {e}"))?;

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

    // Channel feeding mono-f32 samples (still at the device's native
    // rate) from the audio callback to the inference loop. Bounded so a
    // stalled inference loop drops samples instead of growing memory
    // without bound.
    let (tx, rx) = bounded::<Vec<f32>>(64);

    let err_fn = |e| eprintln!("audio stream error: {e}");
    let cancel_audio = cancel.clone();
    let stream = match format {
        cpal::SampleFormat::F32 => {
            let tx = tx.clone();
            let cancel = cancel_audio.clone();
            device.build_input_stream(
                &stream_cfg,
                move |data: &[f32], _| {
                    if cancel.load(Ordering::Relaxed) {
                        return;
                    }
                    let _ = tx.try_send(downmix_f32(data, channels));
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
                move |data: &[i16], _| {
                    if cancel.load(Ordering::Relaxed) {
                        return;
                    }
                    let f: Vec<f32> = data.iter().map(|&s| s as f32 / i16::MAX as f32).collect();
                    let _ = tx.try_send(downmix_f32(&f, channels));
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
                move |data: &[u16], _| {
                    if cancel.load(Ordering::Relaxed) {
                        return;
                    }
                    let f: Vec<f32> = data
                        .iter()
                        .map(|&s| (s as f32 - 32768.0) / 32768.0)
                        .collect();
                    let _ = tx.try_send(downmix_f32(&f, channels));
                },
                err_fn,
                None,
            )?
        }
        other => return Err(anyhow!("unsupported sample format: {other:?}")),
    };
    stream.play()?;

    let chunk_at_device_rate = (sr as f32 * CHUNK_SECONDS) as usize;
    let mut buf: Vec<f32> = Vec::with_capacity(chunk_at_device_rate * 2);
    let mut state = ctx
        .create_state()
        .map_err(|e| anyhow!("state create: {e}"))?;

    while !cancel.load(Ordering::SeqCst) {
        match rx.recv_timeout(std::time::Duration::from_millis(200)) {
            Ok(chunk) => buf.extend_from_slice(&chunk),
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
            Err(_) => break,
        }
        if buf.len() < chunk_at_device_rate {
            continue;
        }
        let take: Vec<f32> = buf.drain(..chunk_at_device_rate).collect();
        let resampled = resample_linear(&take, sr, TARGET_SR);

        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_translate(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_special(false);
        params.set_print_timestamps(false);
        params.set_no_context(false);
        params.set_single_segment(true);
        if let Err(e) = state.full(params, &resampled) {
            eprintln!("whisper full failed: {e}");
            continue;
        }
        let n = state.full_n_segments().unwrap_or(0);
        let mut text = String::new();
        for i in 0..n {
            if let Ok(seg) = state.full_get_segment_text(i) {
                text.push_str(&seg);
            }
        }
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            let frame = TranscribeFrame {
                delta: format!("{trimmed} "),
                elapsed_ms: started.elapsed().as_millis(),
                is_final: false,
            };
            let _ = window.emit(event, frame);
        }
    }
    drop(stream);
    Ok(())
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
