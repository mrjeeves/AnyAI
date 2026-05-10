//! Preload modes ahead of time + `ensure_tracked_models` reconcile loop.
//!
//! Used by:
//!   - `anyai preload <modes...>` (CLI)
//!   - The API server's pull-on-demand handler
//!   - The watcher (background ticks + post-config-change reconciliation)

use anyhow::Result;
use serde::Serialize;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Debug, Clone, Serialize)]
pub struct PreloadEvent {
    pub mode: String,
    pub model: String,
    pub status: String, // "resolved" | "pulling" | "pulled" | "warming" | "ready" | "error"
    pub detail: String,
}

/// Preload one or more modes. Sequential — Ollama doesn't parallelise pulls well.
pub async fn preload<F: FnMut(PreloadEvent)>(
    modes: &[String],
    track: bool,
    warm: bool,
    mut on_event: F,
) -> Result<()> {
    let _g = preload_lock().lock().await;

    if track {
        persist_tracked(modes)?;
    }

    for mode in modes {
        // Transcribe runs on whisper-rs (models live under
        // `~/.anyai/whisper/`), not Ollama. Skip it in the Ollama
        // preload loop instead of resolving + pulling a phantom tag —
        // the previous behaviour pulled a non-existent `whisper:*`
        // manifest from Ollama Hub and silently wrote nothing to disk.
        if mode == "transcribe" {
            on_event(PreloadEvent {
                mode: mode.clone(),
                model: String::new(),
                status: "ready".into(),
                detail: "transcribe uses whisper-rs (manage via Settings → Transcription)".into(),
            });
            continue;
        }
        let model = match crate::resolver::resolve(mode).await {
            Ok(m) => m,
            Err(e) => {
                on_event(PreloadEvent {
                    mode: mode.clone(),
                    model: String::new(),
                    status: "error".into(),
                    detail: format!("resolve failed: {e}"),
                });
                continue;
            }
        };
        on_event(PreloadEvent {
            mode: mode.clone(),
            model: model.clone(),
            status: "resolved".into(),
            detail: String::new(),
        });

        let already = crate::ollama::has_model(&model).await.unwrap_or(false);
        if !already {
            on_event(PreloadEvent {
                mode: mode.clone(),
                model: model.clone(),
                status: "pulling".into(),
                detail: "starting".into(),
            });
            let m_for_cb = mode.clone();
            let model_for_cb = model.clone();
            // Tokio Mutex would be tricky around &mut closure; collect lines and forward periodically.
            let pull_res = crate::ollama::pull_with(&model, |evt| {
                on_event(PreloadEvent {
                    mode: m_for_cb.clone(),
                    model: model_for_cb.clone(),
                    status: "pulling".into(),
                    detail: evt.render(),
                });
            })
            .await;
            if let Err(e) = pull_res {
                on_event(PreloadEvent {
                    mode: mode.clone(),
                    model: model.clone(),
                    status: "error".into(),
                    detail: format!("pull failed: {e}"),
                });
                continue;
            }
            on_event(PreloadEvent {
                mode: mode.clone(),
                model: model.clone(),
                status: "pulled".into(),
                detail: String::new(),
            });
        }

        if warm {
            on_event(PreloadEvent {
                mode: mode.clone(),
                model: model.clone(),
                status: "warming".into(),
                detail: String::new(),
            });
            if let Err(e) = crate::ollama::warm(&model).await {
                // Non-fatal: warm-up is a perf optimisation.
                on_event(PreloadEvent {
                    mode: mode.clone(),
                    model: model.clone(),
                    status: "error".into(),
                    detail: format!("warm failed (non-fatal): {e}"),
                });
            }
        }

        on_event(PreloadEvent {
            mode: mode.clone(),
            model: model.clone(),
            status: "ready".into(),
            detail: String::new(),
        });
    }

    Ok(())
}

/// Reconcile every tracked mode against the current manifest.
/// Pulls any missing tag, warms it, and returns the list of models now ready.
/// Safe to call concurrently — guarded by an in-process mutex.
pub async fn ensure_tracked_models(warm: bool) -> Result<Vec<String>> {
    let modes = crate::resolver::tracked_modes()?;
    if modes.is_empty() {
        return Ok(vec![]);
    }
    let mut ready: Vec<String> = Vec::new();
    preload(&modes, false, warm, |evt| {
        if evt.status == "ready" {
            ready.push(evt.model.clone());
        }
    })
    .await?;
    ready.sort();
    ready.dedup();
    Ok(ready)
}

fn persist_tracked(modes: &[String]) -> Result<()> {
    let mut config = crate::resolver::load_config_value()?;
    let mut existing: Vec<String> = config["tracked_modes"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    for m in modes {
        if !existing.iter().any(|e| e == m) {
            existing.push(m.clone());
        }
    }
    config["tracked_modes"] = serde_json::json!(existing);
    crate::resolver::save_config_value(&config)?;
    Ok(())
}

fn preload_lock() -> &'static Mutex<()> {
    static LOCK: std::sync::OnceLock<Arc<Mutex<()>>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| Arc::new(Mutex::new(())))
}
