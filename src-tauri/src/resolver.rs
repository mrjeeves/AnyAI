//! Manifest resolution: hardware-tier walking, virtual model IDs, config-aware lookups.
//!
//! Mirrors the TypeScript `src/manifest.ts` so the headless CLI / API server can resolve
//! models without booting the JS runtime. Reads the same on-disk caches the GUI writes.
//!
//! Schema (v4): a manifest exposes named **families** (e.g. `gemma4`, `qwen3`); each
//! family owns its own per-mode tier table. The resolver picks
//! `families[active_family].modes[mode].tiers` and walks them against current hardware.

use anyhow::{anyhow, Result};
use serde_json::{Map, Value};
use std::collections::HashSet;
use std::path::PathBuf;
use tokio::time::Duration;

use crate::hardware::HardwareProfile;

pub const VIRTUAL_PREFIX: &str = "anyai-";
pub const KNOWN_MODES: &[&str] = &["text", "vision", "code", "transcribe"];
const DEFAULT_TTL_MIN: f64 = 360.0;
const FALLBACK_MANIFEST_URL: &str =
    "https://raw.githubusercontent.com/mrjeeves/AnyAI/main/manifests/default.json";

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

    let exact_spec = family
        .get("modes")
        .and_then(|m| m.get(mode))
        .and_then(|v| v.as_object());

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

    let vram = effective_vram_gb(hw);
    let ram = hw.ram_gb;

    for tier in tiers {
        let min_vram = tier["min_vram_gb"].as_f64().unwrap_or(0.0);
        let min_ram = tier["min_ram_gb"].as_f64().unwrap_or(0.0);
        if vram >= min_vram || ram >= min_ram {
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
        .and_then(|v| v.as_str());
    Some(
        declared
            .unwrap_or_else(|| default_runtime_for(mode))
            .to_string(),
    )
}

/// VRAM the resolver should credit toward `min_vram_gb` checks. Mirrors
/// `effectiveVramGb` in `src/manifest.ts`: only discrete GPUs (NVIDIA, AMD)
/// own VRAM separately from system RAM. Apple Silicon and integrated GPUs
/// share the same physical pool `ram_gb` already counts, so crediting their
/// "VRAM" again would let an 8 GB Mac match a `vram>=6` tier and pick a
/// model the system can't fit.
fn effective_vram_gb(hw: &HardwareProfile) -> f64 {
    use crate::hardware::GpuType;
    match hw.gpu_type {
        GpuType::Nvidia | GpuType::Amd => hw.vram_gb.unwrap_or(0.0),
        GpuType::Apple | GpuType::None => 0.0,
    }
}

/// All model tags recommended by a manifest across every family/mode/tier.
pub fn tags_in_manifest(manifest: &Value) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(families) = manifest["families"].as_object() {
        for (_name, family) in families {
            if let Some(modes) = family["modes"].as_object() {
                for (_, mode_spec) in modes {
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
                }
            }
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

/// Translate a virtual model ID (e.g. "anyai-text") to its current resolved tag.
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
            }
        }

        // Importing file wins on family-key collision (closer publisher).
        if let Some(families) = raw["families"].as_object() {
            for (k, v) in families {
                merged_families.insert(k.clone(), v.clone());
            }
        }

        Ok(serde_json::json!({
            "name": raw["name"].clone(),
            "version": raw["version"].clone(),
            "ttl_minutes": raw["ttl_minutes"].clone(),
            "default_family": raw["default_family"].clone(),
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

/// Fetch a single manifest URL, honouring its own ttl_minutes. No import recursion.
async fn fetch_one_manifest(url: &str) -> Result<Value> {
    if let Some(cached) = read_manifest_cache(url) {
        let ttl_min = cached["manifest"]["ttl_minutes"]
            .as_f64()
            .unwrap_or(DEFAULT_TTL_MIN);
        let fetched_at = cached["fetched_at"].as_str().unwrap_or("");
        if !is_stale(fetched_at, ttl_min) {
            return Ok(cached["manifest"].clone());
        }
    }

    match fetch_manifest_http(url).await {
        Ok(m) => {
            let _ = write_manifest_cache(url, &m);
            Ok(m)
        }
        Err(_) => {
            if let Some(cached) = read_manifest_cache(url) {
                return Ok(cached["manifest"].clone());
            }
            // Bundled fallback shipped at compile time.
            let bundled = include_str!("../../manifests/default.json");
            Ok(serde_json::from_str(bundled)?)
        }
    }
}

async fn fetch_manifest_http(url: &str) -> Result<Value> {
    let body = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::process::Command::new("curl")
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
    Ok(crate::anyai_dir()?
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
    let path = crate::anyai_dir()?.join("config.json");
    if !path.exists() {
        return Ok(default_config_value());
    }
    let s = std::fs::read_to_string(&path)?;
    let v: Value = serde_json::from_str(&s).map_err(|e| anyhow!("invalid config.json: {e}"))?;
    Ok(merge_defaults(v))
}

pub fn save_config_value(config: &Value) -> Result<()> {
    let path = crate::anyai_dir()?.join("config.json");
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
    let conv_dir = crate::anyai_dir()
        .map(|d| d.join("conversations").to_string_lossy().into_owned())
        .unwrap_or_default();
    serde_json::json!({
        "active_provider": "AnyAI Default",
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
                "name": "AnyAI Default",
                "url": "https://raw.githubusercontent.com/mrjeeves/AnyAI/main/manifests/default.json"
            }
        ]
    })
}

/// Shallow-merge missing top-level + nested-object keys from defaults so users
/// upgrading from older configs don't see crashes on first load. Also seeds
/// `tracked_modes` from `active_mode` for legacy configs, rewrites any saved
/// `anyai.run` provider URLs to the canonical raw.githubusercontent.com URL
/// (the host they used to point to is no longer authoritative), and drops
/// removed fields (e.g. the retired `sources`).
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
    // Rewrite stale anyai.run provider URLs from pre-1.0 builds.
    rewrite_legacy_provider_urls(&mut config);
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
        if let Ok(d) = crate::anyai_dir() {
            config["conversation_dir"] =
                serde_json::json!(d.join("conversations").to_string_lossy());
        }
    }
    config
}

const CANONICAL_DEFAULT_URL: &str =
    "https://raw.githubusercontent.com/mrjeeves/AnyAI/main/manifests/default.json";

fn rewrite_legacy_provider_urls(config: &mut Value) {
    let Some(arr) = config["providers"].as_array_mut() else {
        return;
    };
    for entry in arr {
        let Some(url) = entry.get("url").and_then(|v| v.as_str()) else {
            continue;
        };
        // Match by host so `anyai.run`, `www.anyai.run`, etc. all retarget.
        let host_start = url.find("//").map(|i| i + 2).unwrap_or(0);
        let after_host = &url[host_start..];
        let host_end = after_host.find('/').unwrap_or(after_host.len());
        let host = &after_host[..host_end];
        if host == "anyai.run" || host == "www.anyai.run" {
            entry["url"] = serde_json::json!(CANONICAL_DEFAULT_URL);
        }
    }
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
    /// file: that couples the resolver test to manifest content.
    fn manifest() -> Value {
        serde_json::json!({
            "default_family": "test",
            "families": {
                "test": {
                    "label": "Test",
                    "default_mode": "text",
                    "modes": {
                        "text": {
                            "tiers": [
                                { "min_vram_gb": 24, "min_ram_gb": 48, "model": "big:35b"   },
                                { "min_vram_gb":  8, "min_ram_gb": 16, "model": "mid:9b"    },
                                { "min_vram_gb":  4, "min_ram_gb": 10, "model": "small:2b"  },
                                { "min_vram_gb":  0, "min_ram_gb":  0, "model": "tiny:1b"   }
                            ]
                        }
                    }
                }
            }
        })
    }

    #[test]
    fn apple_8gb_unified_lands_on_tiny_not_mid() {
        // Regression: pre-fix, Apple 8 GB reported vram=8 AND ram=8, the
        // resolver OR-matched `vram >= 6` at the 9 B tier, picked a model
        // the system couldn't fit, and ground at ~1 token / 10 s.
        let mac = hw(GpuType::Apple, Some(8.0), 8.0);
        assert_eq!(
            resolve_in_manifest(&manifest(), &mac, "text", "test").unwrap(),
            "tiny:1b"
        );
    }

    #[test]
    fn pi_8gb_no_gpu_lands_on_tiny() {
        let pi = hw(GpuType::None, None, 8.0);
        assert_eq!(
            resolve_in_manifest(&manifest(), &pi, "text", "test").unwrap(),
            "tiny:1b"
        );
    }

    #[test]
    fn apple_16gb_unified_lands_on_mid() {
        // Mac with enough headroom for a 9 B model — picks `mid` via
        // ram>=16, not via the (still-zero) effective vram.
        let mac = hw(GpuType::Apple, Some(16.0), 16.0);
        assert_eq!(
            resolve_in_manifest(&manifest(), &mac, "text", "test").unwrap(),
            "mid:9b"
        );
    }

    #[test]
    fn discrete_nvidia_vram_still_credited() {
        // 12 GB NVIDIA card with 8 GB system RAM should still get the 9 B
        // tier — the VRAM is its own pool here, so the check stays useful.
        let pc = hw(GpuType::Nvidia, Some(12.0), 8.0);
        assert_eq!(
            resolve_in_manifest(&manifest(), &pc, "text", "test").unwrap(),
            "mid:9b"
        );
    }

    #[test]
    fn unknown_family_falls_back_to_default_family() {
        // Stale config still resolves: the family the user has saved is gone,
        // so the resolver falls back to the manifest's default_family.
        let pc = hw(GpuType::Nvidia, Some(12.0), 8.0);
        assert_eq!(
            resolve_in_manifest(&manifest(), &pc, "text", "no-such-family").unwrap(),
            "mid:9b"
        );
    }
}
