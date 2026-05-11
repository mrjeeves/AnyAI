//! Manifest resolution: hardware-tier walking, virtual model IDs, config-aware lookups.
//!
//! Mirrors the TypeScript `src/manifest.ts` so the headless CLI / API server can resolve
//! models without booting the JS runtime. Reads the same on-disk caches the GUI writes.
//!
//! Schema (v12): a manifest exposes named **families** (e.g. `gemma4`, `qwen3.6`); each
//! family owns its own per-mode tier table. The resolver picks
//! `families[active_family].modes[mode].tiers` and walks them against current hardware.
//! Tiers carry separate thresholds for discrete-GPU hosts (`min_vram_gb` /
//! `min_ram_gb`, the latter taken after the manifest's `headroom_gb`) and for
//! unified-memory hosts (`min_unified_ram_gb`, raw RAM that already reserves OS
//! headroom + the paired transcribe model). The shared transcribe ladder
//! collapses to a single rung — `large-v3-turbo` is the only whisper variant
//! fast enough to be usable in practice; smaller ggml models exist in
//! `KNOWN_MODELS` but are not recommended by the default manifest.

use anyhow::{anyhow, Result};
use serde_json::{Map, Value};
use std::collections::HashSet;
use std::path::PathBuf;
use tokio::time::Duration;

use crate::hardware::HardwareProfile;

pub const VIRTUAL_PREFIX: &str = "myownllm-";
pub const KNOWN_MODES: &[&str] = &["text", "vision", "code", "transcribe"];
const DEFAULT_TTL_MIN: f64 = 360.0;
const FALLBACK_MANIFEST_URL: &str =
    "https://raw.githubusercontent.com/mrjeeves/MyOwnLLM/main/manifests/default.json";

/// Resolve a single mode against the active provider's manifest using current hardware.
pub async fn resolve(mode: &str) -> Result<String> {
    let hw = crate::hardware::detect()?;
    resolve_with_hardware(mode, &hw, None).await
}

/// As `resolve`, but with a one-off manifest URL override (for `--profile`).
pub async fn resolve_with_hardware(
    mode: &str,
    hw: &HardwareProfile,
    profile_url: Option<&str>,
) -> Result<String> {
    let config = load_config_value()?;

    if let Some(over) = config["mode_overrides"][mode].as_str() {
        if !over.is_empty() {
            return Ok(over.to_string());
        }
    }

    let manifest_url = match profile_url {
        Some(u) => u.to_string(),
        None => active_provider_url(&config).unwrap_or_else(|| FALLBACK_MANIFEST_URL.to_string()),
    };

    let active_family = config["active_family"].as_str().unwrap_or("");
    let manifest = fetch_or_load_manifest(&manifest_url).await?;
    resolve_in_manifest(&manifest, hw, mode, active_family)
}

/// Pick the family the user has selected. Falls back to `default_family`,
/// then to whichever family appears first in document order. Returns the
/// (name, family-object) pair so callers can attribute the decision.
pub fn pick_family<'a>(
    manifest: &'a Value,
    requested: &str,
) -> Option<(String, &'a Map<String, Value>)> {
    let families = manifest["families"].as_object()?;
    let candidates = [requested, manifest["default_family"].as_str().unwrap_or("")];
    for k in candidates {
        if k.is_empty() {
            continue;
        }
        if let Some(f) = families.get(k).and_then(|v| v.as_object()) {
            return Some((k.to_string(), f));
        }
    }
    families
        .iter()
        .next()
        .and_then(|(k, v)| v.as_object().map(|f| (k.clone(), f)))
}

pub fn resolve_in_manifest(
    manifest: &Value,
    hw: &HardwareProfile,
    mode: &str,
    active_family: &str,
) -> Result<String> {
    Ok(resolve_full(manifest, hw, mode, active_family)?.0)
}

/// Default runtime for a mode when the manifest doesn't declare one.
/// Mirror of `defaultRuntimeFor` on the TS side. Transcribe always uses
/// whisper-rs in this app; everything else routes through Ollama. Used
/// so a stale cached manifest from before the `runtime` field landed
/// can't trick the resolver into handing whisper-shaped names to
/// `ollama pull`.
pub fn default_runtime_for(mode: &str) -> &'static str {
    if mode == "transcribe" {
        "whisper"
    } else {
        "ollama"
    }
}

/// Resolve a `(model, runtime)` pair against the active family's tier
/// table. Runtime is read **strictly from the requested mode's spec** —
/// never inherited from a fallback default mode — so a transcribe
/// request whose mode is missing from the manifest still routes through
/// whisper-rs instead of inheriting `text`'s `ollama` runtime.
pub fn resolve_full(
    manifest: &Value,
    hw: &HardwareProfile,
    mode: &str,
    active_family: &str,
) -> Result<(String, String)> {
    let (_family_name, family) = pick_family(manifest, active_family)
        .ok_or_else(|| anyhow!("manifest exposes no families"))?;

    // Look up the mode in the family first, then the manifest's
    // shared_modes block (the canonical whisper transcribe ladder
    // lives there). The family's own declaration always wins so a
    // family can override a shared mode without forking the schema.
    let exact_spec = family
        .get("modes")
        .and_then(|m| m.get(mode))
        .and_then(|v| v.as_object())
        .or_else(|| {
            manifest
                .get("shared_modes")
                .and_then(|m| m.get(mode))
                .and_then(|v| v.as_object())
        });

    // Runtime is bound to the requested mode, not the fallback. If the
    // requested mode isn't declared we use the well-known default for
    // that mode (transcribe → whisper, everything else → ollama).
    let runtime = exact_spec
        .and_then(|s| s.get("runtime"))
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| default_runtime_for(mode))
        .to_string();

    // No explicit block AND we're on a non-ollama runtime — return a
    // safe whisper default rather than crossing tier ladders with text
    // mode (which would surface nonsense like the text model + whisper
    // runtime, then trip whisper-rs at load time).
    if exact_spec.is_none() && runtime == "whisper" {
        return Ok(("tiny.en".to_string(), runtime));
    }

    let default_mode = family
        .get("default_mode")
        .and_then(|v| v.as_str())
        .unwrap_or("text");
    let tier_spec = exact_spec.or_else(|| {
        family
            .get("modes")
            .and_then(|m| m.get(default_mode))
            .and_then(|v| v.as_object())
    });
    let Some(tier_spec) = tier_spec else {
        return Err(anyhow!("mode '{mode}' not found in active family"));
    };

    let tiers = tier_spec
        .get("tiers")
        .and_then(|t| t.as_array())
        .ok_or_else(|| anyhow!("no tiers in active family"))?;

    let unified = is_unified_memory(hw);
    let headroom = headroom_gb(manifest, &hw.gpu_type);

    for tier in tiers {
        if tier_matches(tier, hw, unified, headroom) {
            if let Some(model) = tier["model"].as_str() {
                return Ok((model.to_string(), runtime));
            }
        }
    }

    let last = tiers
        .last()
        .and_then(|t| t["model"].as_str())
        .map(str::to_string)
        .ok_or_else(|| anyhow!("no model found in active family tiers"))?;
    Ok((last, runtime))
}

/// Look up the effective runtime for `mode` under the active family.
/// Reads the manifest's declared runtime when present, otherwise falls
/// back to `default_runtime_for(mode)` so the preload loop skips
/// `ollama pull` for whisper modes even when the cached manifest
/// predates the `runtime` field.
pub fn mode_runtime(manifest: &Value, mode: &str, active_family: &str) -> Option<String> {
    let (_, family) = pick_family(manifest, active_family)?;
    let declared = family
        .get("modes")
        .and_then(|m| m.get(mode))
        .and_then(|v| v.get("runtime"))
        .and_then(|v| v.as_str())
        .or_else(|| {
            manifest
                .get("shared_modes")
                .and_then(|m| m.get(mode))
                .and_then(|v| v.get("runtime"))
                .and_then(|v| v.as_str())
        });
    Some(
        declared
            .unwrap_or_else(|| default_runtime_for(mode))
            .to_string(),
    )
}

/// Compiled-in headroom defaults when a manifest omits `headroom_gb`. Mirror
/// of `DEFAULT_HEADROOM_GB` in `src/manifest.ts`. Sized to cover the OS +
/// WebView + ollama overhead each GPU class pays once large-v3-turbo
/// (~2 GB resident) is also loaded: Apple reserves macOS + browser tabs,
/// Linux SBCs reserve the base distro, and discrete-GPU hosts only need a
/// sliver of system RAM because the LLM lives on the GPU.
fn default_headroom_gb(gpu: &crate::hardware::GpuType) -> f64 {
    use crate::hardware::GpuType;
    match gpu {
        GpuType::Apple => 5.0,
        GpuType::None => 2.0,
        GpuType::Nvidia | GpuType::Amd => 1.0,
    }
}

fn headroom_gb(manifest: &Value, gpu: &crate::hardware::GpuType) -> f64 {
    use crate::hardware::GpuType;
    let key = match gpu {
        GpuType::Apple => "apple",
        GpuType::None => "none",
        GpuType::Nvidia => "nvidia",
        GpuType::Amd => "amd",
    };
    manifest
        .get("headroom_gb")
        .and_then(|h| h.get(key))
        .and_then(|v| v.as_f64())
        .unwrap_or_else(|| default_headroom_gb(gpu))
}

/// A host is "unified memory" when its GPU shares the same physical pool as
/// system RAM — Apple Silicon and the no-GPU SBC / desktop case. On these
/// hosts crediting `vram_gb` toward `min_vram_gb` would double-count the
/// same bytes; tiers are matched purely off `min_unified_ram_gb` (or a
/// synthesised default) against raw RAM with full headroom factored in.
fn is_unified_memory(hw: &HardwareProfile) -> bool {
    use crate::hardware::GpuType;
    matches!(hw.gpu_type, GpuType::Apple | GpuType::None)
}

/// Raw-RAM threshold a tier requires on a unified-memory host. Explicit
/// `min_unified_ram_gb` always wins; otherwise we synthesise it from
/// `min_ram_gb + headroom_gb[gpu]` so a legacy tier without the field still
/// reserves OS overhead.
fn unified_threshold_gb(tier: &Value, headroom: f64) -> f64 {
    if let Some(u) = tier.get("min_unified_ram_gb").and_then(|v| v.as_f64()) {
        return u;
    }
    tier["min_ram_gb"].as_f64().unwrap_or(0.0) + headroom
}

fn tier_matches(tier: &Value, hw: &HardwareProfile, unified: bool, headroom: f64) -> bool {
    if unified {
        // Single shared pool — VRAM column is the same bytes as RAM, so the
        // only meaningful check is whether raw RAM is large enough to host
        // the OS, the LLM, and the paired transcribe model.
        return hw.ram_gb >= unified_threshold_gb(tier, headroom);
    }
    // Discrete GPU: either the GPU is big enough for the model to live on
    // it entirely, or system RAM (after headroom) is enough for CPU
    // inference. Either path qualifies the tier.
    let min_vram = tier["min_vram_gb"].as_f64().unwrap_or(0.0);
    let vram = hw.vram_gb.unwrap_or(0.0);
    if vram >= min_vram {
        return true;
    }
    let min_ram = tier["min_ram_gb"].as_f64().unwrap_or(0.0);
    let cpu_budget = (hw.ram_gb - headroom).max(0.0);
    cpu_budget >= min_ram
}

/// All model tags recommended by a manifest across every family/mode/tier.
pub fn tags_in_manifest(manifest: &Value) -> Vec<String> {
    let mut out = Vec::new();
    let mut push_mode = |mode_spec: &Value| {
        // Cleanup is Ollama-only: skip non-Ollama runtimes (whisper
        // models live under ~/.myownllm/whisper/ and aren't reachable
        // from `ollama list` anyway).
        let runtime = mode_spec
            .get("runtime")
            .and_then(|v| v.as_str())
            .unwrap_or("ollama");
        if runtime != "ollama" {
            return;
        }
        if let Some(tiers) = mode_spec["tiers"].as_array() {
            for tier in tiers {
                if let Some(t) = tier["model"].as_str() {
                    out.push(t.to_string());
                }
                if let Some(t) = tier["fallback"].as_str() {
                    out.push(t.to_string());
                }
            }
        }
    };
    if let Some(families) = manifest["families"].as_object() {
        for (_name, family) in families {
            if let Some(modes) = family["modes"].as_object() {
                for (_, mode_spec) in modes {
                    push_mode(mode_spec);
                }
            }
        }
    }
    if let Some(shared) = manifest["shared_modes"].as_object() {
        for (_, mode_spec) in shared {
            push_mode(mode_spec);
        }
    }
    out.sort();
    out.dedup();
    out
}

pub fn tracked_modes() -> Result<Vec<String>> {
    let config = load_config_value()?;
    let modes = config["tracked_modes"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(modes)
}

/// Translate a virtual model ID (e.g. "myownllm-text") to its current resolved tag.
/// Returns the input unchanged if it doesn't look like a virtual ID.
pub async fn translate_virtual(requested: &str) -> Result<String> {
    if let Some(mode) = requested.strip_prefix(VIRTUAL_PREFIX) {
        if KNOWN_MODES.contains(&mode) {
            return resolve(mode).await;
        }
    }
    if KNOWN_MODES.contains(&requested) {
        return resolve(requested).await;
    }
    Ok(requested.to_string())
}

// ---------------------------------------------------------------------------
// Manifest fetch + cache (mirrors src/manifest.ts cache directory layout).
//
// Each URL is fetched and cached against ITS OWN ttl_minutes — imports are
// walked recursively, with each imported file obeying its own TTL.
// ---------------------------------------------------------------------------

pub async fn fetch_or_load_manifest(url: &str) -> Result<Value> {
    let mut visited: HashSet<String> = HashSet::new();
    walk_manifest(url, &mut visited).await
}

fn walk_manifest<'a>(
    url: &'a str,
    visited: &'a mut HashSet<String>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value>> + Send + 'a>> {
    Box::pin(async move {
        if !visited.insert(url.to_string()) {
            return Ok(empty_manifest());
        }

        let raw = fetch_one_manifest(url).await?;
        let mut merged_families: Map<String, Value> = Map::new();
        let mut merged_shared: Map<String, Value> = Map::new();

        if let Some(imports) = raw["imports"].as_array() {
            for imp in imports {
                let Some(imp_url) = imp.as_str() else {
                    continue;
                };
                let imported = match walk_manifest(imp_url, visited).await {
                    Ok(v) => v,
                    Err(_) => continue, // Import failure is non-fatal; merge the rest.
                };
                if let Some(families) = imported["families"].as_object() {
                    for (k, v) in families {
                        merged_families.insert(k.clone(), v.clone());
                    }
                }
                if let Some(shared) = imported["shared_modes"].as_object() {
                    for (k, v) in shared {
                        merged_shared.insert(k.clone(), v.clone());
                    }
                }
            }
        }

        // Importing file wins on key collision (closer publisher).
        if let Some(families) = raw["families"].as_object() {
            for (k, v) in families {
                merged_families.insert(k.clone(), v.clone());
            }
        }
        if let Some(shared) = raw["shared_modes"].as_object() {
            for (k, v) in shared {
                merged_shared.insert(k.clone(), v.clone());
            }
        }

        Ok(serde_json::json!({
            "name": raw["name"].clone(),
            "version": raw["version"].clone(),
            "ttl_minutes": raw["ttl_minutes"].clone(),
            "default_family": raw["default_family"].clone(),
            "shared_modes": Value::Object(merged_shared),
            "families": Value::Object(merged_families),
        }))
    })
}

fn empty_manifest() -> Value {
    serde_json::json!({
        "name": "",
        "version": "1",
        "default_family": "",
        "families": {},
    })
}

/// Bundled manifest source, included at compile time. We keep a
/// const_cell-like helper to parse-once-and-share since several call
/// sites compare the bundled version to whatever's cached.
fn bundled_manifest() -> Result<Value> {
    let bundled = include_str!("../../manifests/default.json");
    Ok(serde_json::from_str(bundled)?)
}

/// True when the binary's bundled manifest declares a newer schema
/// version than what the cache has. Lets `just dev` rebuilds drop a
/// stale cached manifest instead of letting it linger up to the
/// configured TTL.
fn bundled_is_newer(cached_manifest: &Value) -> bool {
    let bundled = match bundled_manifest() {
        Ok(b) => b,
        Err(_) => return false,
    };
    let parse = |v: &Value| v["version"].as_str().and_then(|s| s.parse::<u64>().ok());
    match (parse(&bundled), parse(cached_manifest)) {
        (Some(b), Some(c)) => b > c,
        _ => false,
    }
}

/// Fetch a single manifest URL, honouring its own ttl_minutes. No import recursion.
async fn fetch_one_manifest(url: &str) -> Result<Value> {
    if let Some(cached) = read_manifest_cache(url) {
        let ttl_min = cached["manifest"]["ttl_minutes"]
            .as_f64()
            .unwrap_or(DEFAULT_TTL_MIN);
        let fetched_at = cached["fetched_at"].as_str().unwrap_or("");
        // Cache is OK if fresh AND the bundled binary doesn't already
        // know about a newer schema. The version-bump escape hatch
        // keeps `just dev` rebuilds from reading a stale cached
        // manifest until TTL.
        if !is_stale(fetched_at, ttl_min) && !bundled_is_newer(&cached["manifest"]) {
            return Ok(cached["manifest"].clone());
        }
    }

    match fetch_manifest_http(url).await {
        Ok(m) => {
            let _ = write_manifest_cache(url, &m);
            Ok(m)
        }
        Err(_) => {
            // Network failed — prefer the cache, but only if our bundled
            // isn't ahead of it; otherwise the bundled is the authoritative
            // source for the schema this binary understands.
            if let Some(cached) = read_manifest_cache(url) {
                if !bundled_is_newer(&cached["manifest"]) {
                    return Ok(cached["manifest"].clone());
                }
            }
            bundled_manifest()
        }
    }
}

async fn fetch_manifest_http(url: &str) -> Result<Value> {
    let body = tokio::time::timeout(
        Duration::from_secs(10),
        crate::process::quiet_tokio_command("curl")
            .args(["-sf", "--max-time", "10", url])
            .output(),
    )
    .await??;
    if !body.status.success() {
        return Err(anyhow!("HTTP fetch failed for {url}"));
    }
    Ok(serde_json::from_slice(&body.stdout)?)
}

fn read_manifest_cache(url: &str) -> Option<Value> {
    let path = manifest_cache_path(url).ok()?;
    let s = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&s).ok()
}

fn write_manifest_cache(url: &str, manifest: &Value) -> Result<()> {
    let path = manifest_cache_path(url)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let entry = serde_json::json!({
        "fetched_at": chrono_iso_now(),
        "manifest": manifest,
    });
    std::fs::write(path, serde_json::to_string_pretty(&entry)?)?;
    Ok(())
}

fn manifest_cache_path(url: &str) -> Result<PathBuf> {
    Ok(crate::myownllm_dir()?
        .join("cache/manifests")
        .join(format!("{:x}.json", djb2(url))))
}

fn djb2(s: &str) -> u64 {
    s.bytes()
        .fold(5381u64, |h, b| h.wrapping_mul(33).wrapping_add(b as u64))
}

fn is_stale(fetched_at: &str, ttl_minutes: f64) -> bool {
    let parsed = parse_iso_secs(fetched_at);
    let now = unix_secs();
    match parsed {
        Some(t) => (now - t) as f64 / 60.0 > ttl_minutes,
        None => true,
    }
}

fn unix_secs() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn parse_iso_secs(s: &str) -> Option<i64> {
    // Minimal ISO-8601 parser sufficient for cache freshness checks.
    // Format: 2024-01-02T03:04:05.678Z (fractional seconds optional, Z required).
    let s = s.trim_end_matches('Z');
    let (date, rest) = s.split_once('T')?;
    let mut date_parts = date.split('-');
    let y: i64 = date_parts.next()?.parse().ok()?;
    let m: i64 = date_parts.next()?.parse().ok()?;
    let d: i64 = date_parts.next()?.parse().ok()?;
    let time = rest.split('.').next()?;
    let mut time_parts = time.split(':');
    let hh: i64 = time_parts.next()?.parse().ok()?;
    let mm: i64 = time_parts.next()?.parse().ok()?;
    let ss: i64 = time_parts.next()?.parse().ok()?;

    // Days from 1970-01-01 to (y, m, d) using a Gregorian formula.
    let m_adj = if m <= 2 { m + 12 } else { m };
    let y_adj = if m <= 2 { y - 1 } else { y };
    let era = y_adj.div_euclid(400);
    let yoe = y_adj - era * 400;
    let doy = (153 * (m_adj - 3) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days_since_epoch = era * 146097 + doe - 719468;

    Some(days_since_epoch * 86400 + hh * 3600 + mm * 60 + ss)
}

fn chrono_iso_now() -> String {
    let secs = unix_secs();
    // Reverse of parse_iso_secs.
    let z = secs + 719468 * 86400;
    let days = z.div_euclid(86400);
    let secs_of_day = z.rem_euclid(86400);
    let hh = secs_of_day / 3600;
    let mm = (secs_of_day / 60) % 60;
    let ss = secs_of_day % 60;
    let era = days.div_euclid(146097);
    let doe = days - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y_adj = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y_adj + 1 } else { y_adj };
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

// ---------------------------------------------------------------------------
// Config helpers (read-only views; mutations live in cli.rs / preload.rs).
// ---------------------------------------------------------------------------

pub fn load_config_value() -> Result<Value> {
    let path = crate::myownllm_dir()?.join("config.json");
    if !path.exists() {
        return Ok(default_config_value());
    }
    let s = std::fs::read_to_string(&path)?;
    let v: Value = serde_json::from_str(&s).map_err(|e| anyhow!("invalid config.json: {e}"))?;
    Ok(merge_defaults(v))
}

pub fn save_config_value(config: &Value) -> Result<()> {
    let path = crate::myownllm_dir()?.join("config.json");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(config)?)?;
    Ok(())
}

pub fn active_provider_url(config: &Value) -> Option<String> {
    let active = config["active_provider"].as_str()?;
    config["providers"]
        .as_array()?
        .iter()
        .find(|p| p["name"].as_str() == Some(active))?
        .get("url")?
        .as_str()
        .map(str::to_string)
}

pub fn default_config_value() -> Value {
    let conv_dir = crate::myownllm_dir()
        .map(|d| d.join("conversations").to_string_lossy().into_owned())
        .unwrap_or_default();
    serde_json::json!({
        "active_provider": "MyOwnLLM Default",
        "active_family": "gemma4",
        "active_mode": "text",
        "model_cleanup_days": 1,
        "kept_models": [],
        "mode_overrides": {},
        "tracked_modes": ["text"],
        "conversation_dir": conv_dir,
        "api": {
            "enabled": true,
            "host": "127.0.0.1",
            "port": 1473,
            "cors_allow_all": false,
            "bearer_token": null
        },
        "auto_update": {
            "enabled": true,
            "channel": "stable",
            "auto_apply": "patch",
            "check_interval_hours": 6
        },
        "remote_ui": {
            "enabled": false,
            "port": 1474
        },
        "providers": [
            {
                "name": "MyOwnLLM Default",
                "url": "https://raw.githubusercontent.com/mrjeeves/MyOwnLLM/main/manifests/default.json"
            }
        ]
    })
}

/// Shallow-merge missing top-level + nested-object keys from defaults so users
/// upgrading from older configs don't see crashes on first load. Also seeds
/// `tracked_modes` from `active_mode` for legacy configs and drops removed
/// fields (e.g. the retired `sources`).
pub fn merge_defaults(mut config: Value) -> Value {
    let defaults = default_config_value();
    if let (Some(obj), Some(def_obj)) = (config.as_object_mut(), defaults.as_object()) {
        // Strip retired fields so they don't linger in the saved config.
        obj.remove("sources");
        for (k, v) in def_obj {
            if !obj.contains_key(k) {
                obj.insert(k.clone(), v.clone());
            }
        }
        for nested_key in ["api", "auto_update", "remote_ui"] {
            if let (Some(nested), Some(def_nested)) = (
                obj.get_mut(nested_key).and_then(Value::as_object_mut),
                def_obj.get(nested_key).and_then(Value::as_object),
            ) {
                for (k, v) in def_nested {
                    if !nested.contains_key(k) {
                        nested.insert(k.clone(), v.clone());
                    }
                }
            }
        }
    }
    // One-shot upgrade: if tracked_modes is empty, seed from active_mode.
    let needs_seed = config["tracked_modes"]
        .as_array()
        .map(|a| a.is_empty())
        .unwrap_or(true);
    if needs_seed {
        let active = config["active_mode"].as_str().unwrap_or("text").to_string();
        config["tracked_modes"] = serde_json::json!([active]);
    }
    // Fill active_family on legacy configs (predates the families schema).
    if config["active_family"].as_str().unwrap_or("").is_empty() {
        config["active_family"] = serde_json::json!("gemma4");
    }
    // Fill conversation_dir on legacy configs (predates the Storage tab).
    if config["conversation_dir"].as_str().unwrap_or("").is_empty() {
        if let Ok(d) = crate::myownllm_dir() {
            config["conversation_dir"] =
                serde_json::json!(d.join("conversations").to_string_lossy());
        }
    }
    config
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hardware::{GpuType, HardwareProfile};

    fn hw(gpu: GpuType, vram: Option<f64>, ram: f64) -> HardwareProfile {
        HardwareProfile {
            vram_gb: vram,
            ram_gb: ram,
            disk_free_gb: 100.0,
            gpu_type: gpu,
            arch: "x86_64".into(),
            soc: None,
        }
    }

    /// Tier table mirroring the bundled manifest's shape so the test stays
    /// stable if `manifests/default.json` is retuned. Don't load the real
    /// file: that couples the resolver test to manifest content. Numbers
    /// match the v12 ladder where every rung pairs with large-v3-turbo.
    fn manifest() -> Value {
        serde_json::json!({
            "default_family": "test",
            "headroom_gb": { "apple": 5, "none": 2, "nvidia": 1, "amd": 1 },
            "families": {
                "test": {
                    "label": "Test",
                    "default_mode": "text",
                    "modes": {
                        "text": {
                            "tiers": [
                                { "min_vram_gb": 24, "min_ram_gb": 24, "min_unified_ram_gb": 32, "model": "big:31b"   },
                                { "min_vram_gb": 12, "min_ram_gb": 12, "min_unified_ram_gb": 18, "model": "mid:12b"   },
                                { "min_vram_gb":  5, "min_ram_gb":  6, "min_unified_ram_gb": 10, "model": "e4b"       },
                                { "min_vram_gb":  4, "min_ram_gb":  4, "min_unified_ram_gb":  8, "model": "e2b"       },
                                { "min_vram_gb":  0, "min_ram_gb":  0, "min_unified_ram_gb":  0, "model": "tiny:270m" }
                            ]
                        }
                    }
                }
            }
        })
    }

    #[test]
    fn apple_8gb_unified_lands_on_e2b() {
        // Smallest Mac — e2b's per-layer arch keeps it at ~2 GB resident
        // so it fits alongside whisper turbo + macOS in 8 GB.
        let mac = hw(GpuType::Apple, Some(8.0), 8.0);
        assert_eq!(
            resolve_in_manifest(&manifest(), &mac, "text", "test").unwrap(),
            "e2b"
        );
    }

    #[test]
    fn pi_4gb_no_gpu_lands_on_tiny() {
        // 4 GB Pi 5 catches the bottom rung — 270m + turbo barely fit
        // alongside the Linux base distro.
        let pi = hw(GpuType::None, None, 4.0);
        assert_eq!(
            resolve_in_manifest(&manifest(), &pi, "text", "test").unwrap(),
            "tiny:270m"
        );
    }

    #[test]
    fn pi_8gb_no_gpu_lands_on_e2b() {
        // 8 GB Pi / Jetson Orin Nano 8 GB: headroom of 2 GB leaves 6 GB
        // for e2b (~2) + turbo (~2). Clears the `min_unified_ram_gb: 8`
        // threshold (low OS overhead on the `none` GPU class).
        let pi = hw(GpuType::None, None, 8.0);
        assert_eq!(
            resolve_in_manifest(&manifest(), &pi, "text", "test").unwrap(),
            "e2b"
        );
    }

    #[test]
    fn apple_16gb_unified_lands_on_e4b() {
        // 16 GB Mac: e4b (~3 GB) + turbo (~2 GB) + macOS (~5 GB) = 10 GB
        // resident. Comfortable headroom; doesn't reach for 12b which
        // needs 18 GB raw.
        let mac = hw(GpuType::Apple, Some(16.0), 16.0);
        assert_eq!(
            resolve_in_manifest(&manifest(), &mac, "text", "test").unwrap(),
            "e4b"
        );
    }

    #[test]
    fn apple_24gb_unified_lands_on_mid() {
        // M-Pro 24 GB — enough budget to host 12b (~8.5 GB) + turbo (~2)
        // alongside macOS. Regression test for the original report where
        // 24 GB Macs were landing on a 26 B model and grinding through
        // swap.
        let mac = hw(GpuType::Apple, Some(24.0), 24.0);
        assert_eq!(
            resolve_in_manifest(&manifest(), &mac, "text", "test").unwrap(),
            "mid:12b"
        );
    }

    #[test]
    fn apple_36gb_unified_reaches_big() {
        // 36 GB Mac clears the `big:31b` threshold (32 GB) — the v12
        // ladder has it sized for 31 B + turbo + macOS comfortably.
        let mac = hw(GpuType::Apple, Some(36.0), 36.0);
        assert_eq!(
            resolve_in_manifest(&manifest(), &mac, "text", "test").unwrap(),
            "big:31b"
        );
    }

    #[test]
    fn apple_28gb_unified_stops_at_mid() {
        // 28 GB Mac doesn't reach the 32 GB threshold for `big`, so it
        // sits on `mid:12b`. Guards against any future regression to the
        // old OR-on-RAM logic.
        let mac = hw(GpuType::Apple, Some(28.0), 28.0);
        assert_eq!(
            resolve_in_manifest(&manifest(), &mac, "text", "test").unwrap(),
            "mid:12b"
        );
    }

    #[test]
    fn discrete_nvidia_vram_still_credited() {
        // 12 GB NVIDIA card with 8 GB system RAM picks `mid:12b` via
        // VRAM — the model lives on GPU, system RAM only needs headroom
        // for whisper + ollama.
        let pc = hw(GpuType::Nvidia, Some(12.0), 8.0);
        assert_eq!(
            resolve_in_manifest(&manifest(), &pc, "text", "test").unwrap(),
            "mid:12b"
        );
    }

    #[test]
    fn discrete_nvidia_cpu_fallback_subtracts_headroom() {
        // 4 GB GPU + 16 GB RAM: VRAM misses `mid` (needs 12 GB), but
        // 16 - 1 = 15 GB CPU budget clears `mid`'s min_ram_gb=12 — so
        // we run on CPU rather than overshooting.
        let pc = hw(GpuType::Nvidia, Some(4.0), 16.0);
        assert_eq!(
            resolve_in_manifest(&manifest(), &pc, "text", "test").unwrap(),
            "mid:12b"
        );
    }

    #[test]
    fn unknown_family_falls_back_to_default_family() {
        // Stale config still resolves: the family the user has saved is gone,
        // so the resolver falls back to the manifest's default_family.
        let pc = hw(GpuType::Nvidia, Some(12.0), 8.0);
        assert_eq!(
            resolve_in_manifest(&manifest(), &pc, "text", "no-such-family").unwrap(),
            "mid:12b"
        );
    }

    #[test]
    fn legacy_tier_without_unified_field_synthesises_threshold() {
        // A tier missing `min_unified_ram_gb` should be treated as
        // `min_ram_gb + headroom_gb[gpu]` so older manifests still
        // reserve OS overhead on Apple. With ram=14, the legacy `mid`
        // tier (min_ram_gb=10) needs 10+5=15 of unified RAM and so
        // misses; the resolver drops to `small`.
        let legacy = serde_json::json!({
            "default_family": "test",
            "headroom_gb": { "apple": 5 },
            "families": {
                "test": {
                    "default_mode": "text",
                    "modes": {
                        "text": {
                            "tiers": [
                                { "min_vram_gb": 24, "min_ram_gb": 24, "model": "big"   },
                                { "min_vram_gb": 12, "min_ram_gb": 10, "model": "mid"   },
                                { "min_vram_gb":  0, "min_ram_gb":  0, "model": "small" }
                            ]
                        }
                    }
                }
            }
        });
        let mac = hw(GpuType::Apple, Some(14.0), 14.0);
        assert_eq!(
            resolve_in_manifest(&legacy, &mac, "text", "test").unwrap(),
            "small"
        );
    }

    #[test]
    fn whisper_ladder_returns_turbo_everywhere() {
        // The default manifest's transcribe ladder collapses to a single
        // rung at threshold 0: any hardware running text gets turbo
        // because smaller whisper variants are unusably slow in practice.
        let turbo = serde_json::json!({
            "default_family": "f",
            "shared_modes": {
                "transcribe": {
                    "runtime": "whisper",
                    "tiers": [
                        { "min_vram_gb": 0, "min_ram_gb": 0, "min_unified_ram_gb": 0, "model": "large-v3-turbo", "fallback": "large-v3-turbo" }
                    ]
                }
            },
            "families": {
                "f": { "default_mode": "text", "modes": { "text": { "tiers": [
                    { "min_vram_gb": 0, "min_ram_gb": 0, "min_unified_ram_gb": 0, "model": "x" }
                ]}}}
            }
        });
        let pi = hw(GpuType::None, None, 4.0);
        let mac = hw(GpuType::Apple, Some(64.0), 64.0);
        assert_eq!(
            resolve_in_manifest(&turbo, &pi, "transcribe", "f").unwrap(),
            "large-v3-turbo"
        );
        assert_eq!(
            resolve_in_manifest(&turbo, &mac, "transcribe", "f").unwrap(),
            "large-v3-turbo"
        );
    }
}
