//! Danger-zone purges. Exposed both as Tauri commands the Storage tab's
//! "Danger zone" calls and as `myownllm purge ...` CLI subcommands.
//!
//! Three tiers, each fully destructive, none reversible:
//!
//! | Tier            | What it removes                                                                    |
//! |-----------------|-------------------------------------------------------------------------------------|
//! | `models`        | Every pulled Ollama tag; every ASR / diarize artifact under `~/.myownllm/models/`; |
//! |                 | clears `kept_models`, `mode_overrides`, `family_overrides`, model-status cache.    |
//! | `conversations` | Every saved conversation under `conversation_dir` (sidecars and folders included). |
//! | `data`          | Stops the managed Ollama, then `models` + the entire `~/.myownllm/` tree           |
//! |                 | (config, cache, transcribe buffer, updates, legacy dirs, …) and a redirected      |
//! |                 | `conversation_dir` if it lives outside `~/.myownllm/`.                             |
//!
//! Errors on individual files are collected into the returned report rather
//! than aborting — a partial purge still leaves the system in a more useful
//! state than a half-rolled-back failure.

use anyhow::{Context, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};

/// Outcome summary returned to the caller. `bytes_freed` is a best-effort
/// pre-delete sum (the directory is gone by the time we'd `stat` it again);
/// `errors` carries per-file failures so the UI can surface what didn't
/// clear without losing the count of what did.
#[derive(Debug, Default, Serialize, Clone)]
pub struct PurgeReport {
    pub bytes_freed: u64,
    pub items_removed: u32,
    pub errors: Vec<String>,
}

/// Wipe every pulled Ollama model + every ASR / diarize artifact, and reset
/// the config keys that pin / override models so the next pull is a clean
/// slate. Leaves provider list and active family alone — the user's choice
/// of "which provider to talk to" is config, not data.
pub async fn purge_models() -> Result<PurgeReport> {
    let mut report = PurgeReport::default();

    // Ollama tags. We don't `ensure_running()` first — if the daemon isn't
    // up there's nothing for `ollama rm` to do, and we don't want a purge
    // to side-effect a daemon launch.
    if let Ok(pulled) = crate::ollama::list_models().await {
        for m in pulled {
            match crate::ollama::delete_model(&m.name).await {
                Ok(_) => {
                    report.bytes_freed = report.bytes_freed.saturating_add(m.size);
                    report.items_removed = report.items_removed.saturating_add(1);
                }
                Err(e) => report.errors.push(format!("ollama rm {}: {e}", m.name)),
            }
        }
    }

    // ASR + diarize artifacts under ~/.myownllm/models/.
    if let Ok(models_root) = crate::models::models_root() {
        if models_root.exists() {
            let bytes = crate::models::dir_size_bytes(&models_root);
            let files = count_files(&models_root);
            match std::fs::remove_dir_all(&models_root) {
                Ok(_) => {
                    report.bytes_freed = report.bytes_freed.saturating_add(bytes);
                    report.items_removed = report.items_removed.saturating_add(files);
                }
                Err(e) => report.errors.push(format!("rm {}: {e}", models_root.display())),
            }
        }
    }

    // Reset config keys that name specific tags. Leaving them populated
    // would surface "kept" / "override" rows in the GUI for tags that no
    // longer exist on disk, and the next preload would re-pull whatever
    // override was sitting there.
    if let Ok(mut cfg) = crate::resolver::load_config_value() {
        cfg["kept_models"] = serde_json::json!([]);
        cfg["mode_overrides"] = serde_json::json!({});
        cfg["family_overrides"] = serde_json::json!({});
        let _ = crate::resolver::save_config_value(&cfg);
    }

    // Drop the recommended-by snapshot so the next startup recomputes
    // against an empty pull list rather than a stale one.
    if let Ok(root) = crate::myownllm_dir() {
        let _ = std::fs::remove_file(root.join("cache/model-status.json"));
    }

    Ok(report)
}

/// Wipe every saved conversation (JSON files, talking-points sidecars,
/// folders, and any user-dropped files in the tree). Honours the
/// user-overridden `conversation_dir` when set; recreates the directory
/// empty so the next save isn't met with ENOENT.
pub fn purge_conversations() -> Result<PurgeReport> {
    let mut report = PurgeReport::default();
    let dir = conversation_dir()?;
    if dir.exists() {
        let bytes = crate::models::dir_size_bytes(&dir);
        let files = count_files(&dir);
        match std::fs::remove_dir_all(&dir) {
            Ok(_) => {
                report.bytes_freed = bytes;
                report.items_removed = files;
                let _ = std::fs::create_dir_all(&dir);
            }
            Err(e) => report.errors.push(format!("rm {}: {e}", dir.display())),
        }
    }
    Ok(report)
}

/// The nuclear option. Stops the managed Ollama daemon (so it isn't
/// holding handles into a directory we're about to delete), then wipes
/// every model and the entire `~/.myownllm/` tree. If the user has
/// redirected `conversation_dir` outside `~/.myownllm/`, that gets
/// removed too — anything else they pointed us at, they own.
pub async fn purge_all() -> Result<PurgeReport> {
    let mut report = purge_models().await?;

    // Best-effort: a managed `ollama serve` we spawned holds open handles
    // to its own data dir, not to ours, but stopping it before the big
    // tree-walk keeps the watcher from re-creating its lockfile mid-purge.
    let _ = crate::ollama::stop().await;

    // Conversations dir if redirected outside the root.
    if let Some(outside) = redirected_conversation_dir()? {
        if outside.exists() {
            let bytes = crate::models::dir_size_bytes(&outside);
            let files = count_files(&outside);
            match std::fs::remove_dir_all(&outside) {
                Ok(_) => {
                    report.bytes_freed = report.bytes_freed.saturating_add(bytes);
                    report.items_removed = report.items_removed.saturating_add(files);
                }
                Err(e) => report.errors.push(format!("rm {}: {e}", outside.display())),
            }
        }
    }

    // The whole tree. Includes config, cache, watcher.lock, updates,
    // transcribe buffer, legacy dirs, models (now empty after purge_models),
    // and conversations when they live under the default path.
    let root = crate::myownllm_dir()?;
    if root.exists() {
        let bytes = crate::models::dir_size_bytes(&root);
        match std::fs::remove_dir_all(&root) {
            Ok(_) => {
                report.bytes_freed = report.bytes_freed.saturating_add(bytes);
            }
            Err(e) => report.errors.push(format!("rm {}: {e}", root.display())),
        }
    }

    Ok(report)
}

/// Resolve `conversation_dir` the same way `conversations::dir` does —
/// honour the config override if set, otherwise default to the path
/// under `~/.myownllm/`. Pulled out so `purge_conversations` and
/// `purge_all` agree on the location without crossing module boundaries.
fn conversation_dir() -> Result<PathBuf> {
    if let Ok(cfg) = crate::resolver::load_config_value() {
        if let Some(p) = cfg.get("conversation_dir").and_then(|v| v.as_str()) {
            if !p.is_empty() {
                return Ok(PathBuf::from(p));
            }
        }
    }
    Ok(crate::myownllm_dir()
        .context("locate ~/.myownllm")?
        .join("conversations"))
}

/// Returns the conversation dir IFF the user pointed it somewhere outside
/// `~/.myownllm/` — `purge_all` handles the inside case implicitly by
/// wiping the whole root. None when the dir is unset, identical to the
/// default, or contained under the root.
fn redirected_conversation_dir() -> Result<Option<PathBuf>> {
    let root = crate::myownllm_dir().context("locate ~/.myownllm")?;
    let dir = conversation_dir()?;
    if dir.starts_with(&root) {
        Ok(None)
    } else {
        Ok(Some(dir))
    }
}

/// Best-effort recursive file count. Used for the "removed N items" line
/// in the report; mirrors `models::dir_size_bytes`'s tolerance for
/// permission errors so a single unreadable subdir doesn't poison the
/// whole walk.
fn count_files(path: &Path) -> u32 {
    let mut total = 0u32;
    let entries = match std::fs::read_dir(path) {
        Ok(e) => e,
        Err(_) => return 0,
    };
    for entry in entries.flatten() {
        let kind = match entry.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        if kind.is_dir() {
            total = total.saturating_add(count_files(&entry.path()));
        } else {
            total = total.saturating_add(1);
        }
    }
    total
}
