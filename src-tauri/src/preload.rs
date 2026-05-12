//! Preload modes ahead of time + `ensure_tracked_models` reconcile loop.
//!
//! Used by:
//!   - `myownllm preload <modes...>` (CLI)
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

    // Look up the active family's manifest once so we can ask each mode
    // which runtime owns it and skip the Ollama pull step for non-Ollama
    // runtimes (moonshine / parakeet / pyannote-diarize) without round-
    // tripping through `resolve()` for each one.
    let (active_family, manifest_opt) = match crate::resolver::load_config_value() {
        Ok(cfg) => {
            let family = cfg["active_family"].as_str().unwrap_or("").to_string();
            let url = crate::resolver::active_provider_url(&cfg);
            let manifest = if let Some(u) = url {
                crate::resolver::fetch_or_load_manifest(&u).await.ok()
            } else {
                None
            };
            (family, manifest)
        }
        Err(_) => (String::new(), None),
    };

    for mode in modes {
        // Modes whose runtime isn't Ollama (moonshine / parakeet /
        // pyannote-diarize) live under `~/.myownllm/models/` and are
        // managed by Settings → Transcription, not by `ollama pull`.
        // Skipping here avoids handing a phantom tag like `moonshine:x`
        // to ollama and silently writing nothing to disk.
        if let Some(ref m) = manifest_opt {
            if let Some(rt) = crate::resolver::mode_runtime(m, mode, &active_family) {
                if rt != "ollama" {
                    on_event(PreloadEvent {
                        mode: mode.clone(),
                        model: String::new(),
                        status: "ready".into(),
                        detail: format!(
                            "{mode} uses the {rt} runtime (manage via Settings → Transcription)"
                        ),
                    });
                    continue;
                }
            }
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
