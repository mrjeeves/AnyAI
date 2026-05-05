use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HardwareProfile {
    pub vram_gb: Option<f64>,
    pub ram_gb: f64,
    pub disk_free_gb: f64,
    pub gpu_type: GpuType,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GpuType {
    Nvidia,
    Amd,
    Apple,
    None,
}

pub fn detect() -> Result<HardwareProfile> {
    let ram_gb = detect_ram_gb();
    let disk_free_gb = detect_disk_free_gb();

    if let Some(vram) = detect_nvidia_vram() {
        return Ok(HardwareProfile { vram_gb: Some(vram), ram_gb, disk_free_gb, gpu_type: GpuType::Nvidia });
    }
    if let Some(vram) = detect_amd_vram() {
        return Ok(HardwareProfile { vram_gb: Some(vram), ram_gb, disk_free_gb, gpu_type: GpuType::Amd });
    }
    if let Some(vram) = detect_apple_unified_memory() {
        return Ok(HardwareProfile { vram_gb: Some(vram), ram_gb, disk_free_gb, gpu_type: GpuType::Apple });
    }
    Ok(HardwareProfile { vram_gb: None, ram_gb, disk_free_gb, gpu_type: GpuType::None })
}

fn detect_nvidia_vram() -> Option<f64> {
    let out = Command::new("nvidia-smi")
        .args(["--query-gpu=memory.total", "--format=csv,noheader,nounits"])
        .output()
        .ok()?;
    if !out.status.success() { return None; }
    let s = String::from_utf8_lossy(&out.stdout);
    let mib: f64 = s.trim().lines().next()?.trim().parse().ok()?;
    Some(mib / 1024.0)
}

fn detect_amd_vram() -> Option<f64> {
    let out = Command::new("rocm-smi")
        .args(["--showmeminfo", "vram", "--json"])
        .output()
        .ok()?;
    if !out.status.success() { return None; }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let bytes: u64 = v.as_object()?
        .values()
        .filter_map(|card| card["VRAM Total Memory (B)"].as_str())
        .filter_map(|s| s.parse().ok())
        .next()?;
    Some(bytes as f64 / 1024.0 / 1024.0 / 1024.0)
}

fn detect_apple_unified_memory() -> Option<f64> {
    #[cfg(target_os = "macos")]
    {
        let out = Command::new("system_profiler")
            .args(["SPHardwareDataType", "-json"])
            .output()
            .ok()?;
        if !out.status.success() { return None; }
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
        let hardware = v["SPHardwareDataType"].as_array()?.first()?;
        let chip = hardware["chip_type"].as_str().unwrap_or("");
        if !chip.to_lowercase().contains("apple") { return None; }
        let mem_str = hardware["physical_memory"].as_str()?;
        let gb: f64 = mem_str.split_whitespace().next()?.parse().ok()?;
        return Some(gb);
    }
    #[allow(unreachable_code)]
    None
}

fn detect_ram_gb() -> f64 {
    #[cfg(target_os = "linux")]
    if let Ok(content) = std::fs::read_to_string("/proc/meminfo") {
        for line in content.lines() {
            if line.starts_with("MemTotal:") {
                if let Some(kb) = line.split_whitespace().nth(1).and_then(|s| s.parse::<f64>().ok()) {
                    return kb / 1024.0 / 1024.0;
                }
            }
        }
    }

    #[cfg(target_os = "macos")]
    if let Ok(out) = Command::new("sysctl").args(["-n", "hw.memsize"]).output() {
        if let Ok(s) = String::from_utf8(out.stdout) {
            if let Ok(bytes) = s.trim().parse::<u64>() {
                return bytes as f64 / 1024.0 / 1024.0 / 1024.0;
            }
        }
    }

    #[cfg(target_os = "windows")]
    if let Ok(out) = Command::new("wmic").args(["ComputerSystem", "get", "TotalPhysicalMemory"]).output() {
        if let Ok(s) = String::from_utf8(out.stdout) {
            if let Some(bytes) = s.lines().nth(1).and_then(|l| l.trim().parse::<u64>().ok()) {
                return bytes as f64 / 1024.0 / 1024.0 / 1024.0;
            }
        }
    }

    8.0
}

fn detect_disk_free_gb() -> f64 {
    // `df -k /` on Unix; `wmic logicaldisk` on Windows
    #[cfg(unix)]
    if let Ok(out) = Command::new("df").args(["-k", "/"]).output() {
        if let Ok(s) = String::from_utf8(out.stdout) {
            // Line 2, field 4 = Available (in 1K blocks)
            if let Some(line) = s.lines().nth(1) {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if let Some(kb) = parts.get(3).and_then(|s| s.parse::<u64>().ok()) {
                    return kb as f64 / 1024.0 / 1024.0;
                }
            }
        }
    }

    #[cfg(target_os = "windows")]
    if let Ok(out) = Command::new("wmic")
        .args(["logicaldisk", "where", "DeviceID='C:'", "get", "FreeSpace"])
        .output()
    {
        if let Ok(s) = String::from_utf8(out.stdout) {
            if let Some(bytes) = s.lines().nth(1).and_then(|l| l.trim().parse::<u64>().ok()) {
                return bytes as f64 / 1024.0 / 1024.0 / 1024.0;
            }
        }
    }

    50.0
}
