//! Manifest resolution: hardware-tier walking, virtual model IDs, config-aware lookups.
//!
//! Mirrors the TypeScript `src/manifest.ts` so the headless CLI / API server can resolve
//! models without booting the JS runtime. Reads the same on-disk caches the GUI writes.

use anyhow::{anyhow, Result};
use serde_json::{Map, Value};
use std::collections::HashSet;
use std::path::PathBuf;
use tokio::time::Duration;

use crate::hardware::HardwareProfile;

pub const VIRTUAL_PREFIX: &str = "anyai-";
pub const KNOWN_MODES: &[&str] = &["text", "vision", "code", "transcribe"];
const DEFAULT_TTL_MIN: f64 = 360.0;
const DEFAULT_SOURCE_TTL_MIN: f64 = 1440.0;
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

    let manifest = fetch_or_load_manifest(&manifest_url).await?;
    resolve_in_manifest(&manifest, hw, mode)
}

pub fn resolve_in_manifest(manifest: &Value, hw: &HardwareProfile, mode: &str) -> Result<String> {
    let default_mode = manifest["default_mode"].as_str().unwrap_or("text");
    let mode_spec = manifest["modes"][mode]
        .as_object()
        .or_else(|| manifest["modes"][default_mode].as_object())
        .ok_or_else(|| anyhow!("mode '{mode}' not found in manifest"))?;

    let tiers = mode_spec
        .get("tiers")
        .and_then(|t| t.as_array())
        .ok_or_else(|| anyhow!("no tiers in manifest"))?;

    let vram = hw.vram_gb.unwrap_or(0.0);
    let ram = hw.ram_gb;

    for tier in tiers {
        let min_vram = tier["min_vram_gb"].as_f64().unwrap_or(0.0);
        let min_ram = tier["min_ram_gb"].as_f64().unwrap_or(0.0);
        if vram >= min_vram || ram >= min_ram {
            if let Some(model) = tier["model"].as_str() {
                return Ok(model.to_string());
            }
        }
    }

    tiers
        .last()
        .and_then(|t| t["model"].as_str())
        .map(str::to_string)
        .ok_or_else(|| anyhow!("no model found in manifest tiers"))
}

pub fn tags_in_manifest(manifest: &Value) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(modes) = manifest["modes"].as_object() {
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

async fn fetch_or_load_manifest(url: &str) -> Result<Value> {
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
        let mut merged_modes: Map<String, Value> = Map::new();

        if let Some(imports) = raw["imports"].as_array() {
            for imp in imports {
                let Some(imp_url) = imp.as_str() else {
                    continue;
                };
                let imported = match walk_manifest(imp_url, visited).await {
                    Ok(v) => v,
                    Err(_) => continue, // Import failure is non-fatal; merge the rest.
                };
                if let Some(modes) = imported["modes"].as_object() {
                    for (k, v) in modes {
                        merged_modes.insert(k.clone(), v.clone());
                    }
                }
            }
        }

        // Importing file wins on mode-key collision (closer publisher).
        if let Some(modes) = raw["modes"].as_object() {
            for (k, v) in modes {
                merged_modes.insert(k.clone(), v.clone());
            }
        }

        Ok(serde_json::json!({
            "name": raw["name"].clone(),
            "version": raw["version"].clone(),
            "ttl_minutes": raw["ttl_minutes"].clone(),
            "default_mode": raw["default_mode"].clone(),
            "modes": Value::Object(merged_modes),
        }))
    })
}

fn empty_manifest() -> Value {
    serde_json::json!({
        "name": "",
        "version": "1",
        "default_mode": "text",
        "modes": {},
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

// ---------------------------------------------------------------------------
// Source-catalog fetch + recursive imports.
// Mirrors `src/sources.ts::fetchSourceCatalog`. Each imported catalog is
// fetched + cached against its own ttl_minutes; cycles broken by URL.
// ---------------------------------------------------------------------------

/// Fetch a source catalog with recursive imports merged in. The returned
/// `providers` array is flat; each entry has an `origin` field set to the URL
/// of the file that contributed it.
pub async fn fetch_source_catalog(url: &str) -> Result<Value> {
    let mut visited: HashSet<String> = HashSet::new();
    walk_catalog(url, &mut visited).await
}

fn walk_catalog<'a>(
    url: &'a str,
    visited: &'a mut HashSet<String>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value>> + Send + 'a>> {
    Box::pin(async move {
        if !visited.insert(url.to_string()) {
            return Ok(serde_json::json!({ "name": "", "providers": [] }));
        }

        let raw = fetch_one_catalog(url).await?;
        let mut seen: HashSet<String> = HashSet::new();
        let mut out: Vec<Value> = Vec::new();

        if let Some(imports) = raw["imports"].as_array() {
            for imp in imports {
                let Some(imp_url) = imp.as_str() else {
                    continue;
                };
                let imported = match walk_catalog(imp_url, visited).await {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                if let Some(providers) = imported["providers"].as_array() {
                    for p in providers {
                        let Some(name) = p["name"].as_str() else {
                            continue;
                        };
                        if seen.contains(name) {
                            continue;
                        }
                        seen.insert(name.to_string());
                        let mut entry = p.clone();
                        if entry.get("origin").is_none() {
                            entry["origin"] = Value::String(imp_url.to_string());
                        }
                        out.push(entry);
                    }
                }
            }
        }

        // Importing file wins on name collision (closer publisher).
        if let Some(providers) = raw["providers"].as_array() {
            for p in providers {
                let Some(name) = p["name"].as_str() else {
                    continue;
                };
                let mut entry = p.clone();
                entry["origin"] = Value::String(url.to_string());
                if seen.contains(name) {
                    if let Some(idx) = out.iter().position(|e| e["name"].as_str() == Some(name)) {
                        out[idx] = entry;
                    }
                    continue;
                }
                seen.insert(name.to_string());
                out.push(entry);
            }
        }

        Ok(serde_json::json!({
            "name": raw["name"].clone(),
            "description": raw["description"].clone(),
            "ttl_minutes": raw["ttl_minutes"].clone(),
            "providers": out,
        }))
    })
}

async fn fetch_one_catalog(url: &str) -> Result<Value> {
    if let Some(cached) = read_catalog_cache(url) {
        let ttl_min = cached["catalog"]["ttl_minutes"]
            .as_f64()
            .unwrap_or(DEFAULT_SOURCE_TTL_MIN);
        let fetched_at = cached["fetched_at"].as_str().unwrap_or("");
        if !is_stale(fetched_at, ttl_min) {
            return Ok(cached["catalog"].clone());
        }
    }
    match fetch_catalog_http(url).await {
        Ok(c) => {
            let _ = write_catalog_cache(url, &c);
            Ok(c)
        }
        Err(e) => {
            if let Some(cached) = read_catalog_cache(url) {
                return Ok(cached["catalog"].clone());
            }
            Err(e)
        }
    }
}

async fn fetch_catalog_http(url: &str) -> Result<Value> {
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

fn read_catalog_cache(url: &str) -> Option<Value> {
    let path = catalog_cache_path(url).ok()?;
    let s = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&s).ok()
}

fn write_catalog_cache(url: &str, catalog: &Value) -> Result<()> {
    let path = catalog_cache_path(url)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let entry = serde_json::json!({
        "fetched_at": chrono_iso_now(),
        "catalog": catalog,
    });
    std::fs::write(path, serde_json::to_string_pretty(&entry)?)?;
    Ok(())
}

fn catalog_cache_path(url: &str) -> Result<PathBuf> {
    Ok(crate::anyai_dir()?
        .join("cache/sources")
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

fn active_provider_url(config: &Value) -> Option<String> {
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
    serde_json::json!({
        "active_provider": "AnyAI Default",
        "active_mode": "text",
        "model_cleanup_days": 1,
        "kept_models": [],
        "mode_overrides": {},
        "tracked_modes": ["text"],
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
        "sources": [
            {
                "name": "AnyAI",
                "url": "https://raw.githubusercontent.com/mrjeeves/AnyAI/main/sources/index.json"
            }
        ],
        "providers": [
            {
                "name": "AnyAI Default",
                "url": "https://raw.githubusercontent.com/mrjeeves/AnyAI/main/manifests/default.json",
                "source": "AnyAI"
            }
        ]
    })
}

/// Shallow-merge missing top-level + nested-object keys from defaults so users
/// upgrading from older configs don't see crashes on first load. Also seeds
/// `tracked_modes` from `active_mode` for legacy configs.
pub fn merge_defaults(mut config: Value) -> Value {
    let defaults = default_config_value();
    if let (Some(obj), Some(def_obj)) = (config.as_object_mut(), defaults.as_object()) {
        for (k, v) in def_obj {
            if !obj.contains_key(k) {
                obj.insert(k.clone(), v.clone());
            }
        }
        for nested_key in ["api", "auto_update"] {
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
    config
}
