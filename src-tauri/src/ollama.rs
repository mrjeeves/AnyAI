use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::process::Stdio;
use std::sync::OnceLock;
use tauri::Emitter;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

static OLLAMA_PROCESS: OnceLock<Mutex<Option<Child>>> = OnceLock::new();

fn process_lock() -> &'static Mutex<Option<Child>> {
    OLLAMA_PROCESS.get_or_init(|| Mutex::new(None))
}

pub fn is_installed() -> bool {
    which::which("ollama").is_ok()
}

pub async fn install() -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        let status = Command::new("sh")
            .args(["-c", "curl -fsSL https://ollama.com/install.sh | sh"])
            .status()
            .await
            .context("failed to run ollama install script")?;
        if !status.success() {
            return Err(anyhow!("ollama install failed"));
        }
    }
    #[cfg(target_os = "macos")]
    {
        // Try brew first, then fall back to the install script
        let brew = Command::new("brew").args(["install", "ollama"]).status().await;
        if brew.map(|s| !s.success()).unwrap_or(true) {
            let status = Command::new("sh")
                .args(["-c", "curl -fsSL https://ollama.com/install.sh | sh"])
                .status()
                .await
                .context("failed to run ollama install script")?;
            if !status.success() {
                return Err(anyhow!("ollama install failed"));
            }
        }
    }
    #[cfg(target_os = "windows")]
    {
        return Err(anyhow!(
            "Please install Ollama from https://ollama.com/download"
        ));
    }
    Ok(())
}

pub async fn ensure_running() -> Result<()> {
    if api_reachable().await { return Ok(()); }

    let mut guard = process_lock().lock().await;
    // Check again after acquiring the lock.
    if api_reachable().await { return Ok(()); }

    let child = Command::new("ollama")
        .arg("serve")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to spawn ollama serve")?;

    *guard = Some(child);

    // Wait up to 10 seconds for API to become reachable.
    for _ in 0..20 {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        if api_reachable().await { return Ok(()); }
    }
    Err(anyhow!("ollama serve did not become reachable within 10s"))
}

async fn api_reachable() -> bool {
    reqwest_get("http://127.0.0.1:11434/").await.is_ok()
}

// Minimal HTTP GET using std (avoids reqwest dep in Rust; frontend uses Tauri http plugin)
async fn reqwest_get(url: &str) -> Result<String> {
    let out = Command::new("curl")
        .args(["-sf", "--max-time", "2", url])
        .output()
        .await
        .context("curl not available")?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(anyhow!("HTTP error"))
    }
}

pub async fn pull(model: &str, window: &tauri::WebviewWindow) -> Result<()> {
    let model = model.to_string();
    let mut child = Command::new("ollama")
        .args(["pull", &model])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to spawn ollama pull")?;

    use tokio::io::{AsyncBufReadExt, BufReader};
    let stdout = child.stdout.take().expect("stdout");
    let mut lines = BufReader::new(stdout).lines();

    while let Ok(Some(line)) = lines.next_line().await {
        let _ = window.emit("ollama-pull-progress", &line);
    }

    let status = child.wait().await.context("ollama pull wait")?;
    if !status.success() {
        return Err(anyhow!("ollama pull failed for {model}"));
    }
    Ok(())
}

pub async fn stop() -> Result<()> {
    let mut guard = process_lock().lock().await;
    if let Some(mut child) = guard.take() {
        let _ = child.kill().await;
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub name: String,
    pub size: u64,
}

pub async fn list_models() -> Result<Vec<ModelInfo>> {
    let out = Command::new("ollama")
        .args(["list", "--json"])
        .output()
        .await
        .context("ollama list")?;

    if !out.status.success() {
        return Ok(vec![]);
    }

    // `ollama list --json` outputs one JSON object per line with keys: name, size, ...
    let mut models = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let line = line.trim();
        if line.is_empty() { continue; }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
            let name = v["name"].as_str().unwrap_or("").to_string();
            let size = v["size"].as_u64().unwrap_or(0);
            if !name.is_empty() {
                models.push(ModelInfo { name, size });
            }
        }
    }
    Ok(models)
}

pub async fn delete_model(name: &str) -> Result<()> {
    let status = Command::new("ollama")
        .args(["rm", name])
        .status()
        .await
        .context("ollama rm")?;
    if !status.success() {
        return Err(anyhow!("ollama rm {name} failed"));
    }
    Ok(())
}
