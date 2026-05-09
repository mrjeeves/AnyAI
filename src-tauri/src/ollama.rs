use anyhow::{anyhow, Context, Result};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::Duration;
use tauri::Emitter;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

static OLLAMA_PROCESS: OnceLock<Mutex<Option<Child>>> = OnceLock::new();

fn process_lock() -> &'static Mutex<Option<Child>> {
    OLLAMA_PROCESS.get_or_init(|| Mutex::new(None))
}

pub fn is_installed() -> bool {
    if which::which("ollama").is_ok() {
        return true;
    }
    // After a fresh manual install on Windows the user's running anyai still
    // has the pre-install PATH, so `which` keeps reporting "not installed"
    // even after they click Retry. Probe the standard install location and
    // augment PATH for the rest of this process so subsequent
    // Command::new("ollama") calls also resolve.
    #[cfg(target_os = "windows")]
    {
        if ensure_windows_default_on_path() {
            return which::which("ollama").is_ok();
        }
    }
    false
}

#[cfg(target_os = "windows")]
fn ensure_windows_default_on_path() -> bool {
    use std::env;
    let Some(local) = env::var_os("LOCALAPPDATA") else {
        return false;
    };
    let ollama_dir = std::path::PathBuf::from(local)
        .join("Programs")
        .join("Ollama");
    if !ollama_dir.join("ollama.exe").exists() {
        return false;
    }
    let existing = env::var_os("PATH").unwrap_or_default();
    let mut new_path = ollama_dir.into_os_string();
    if !existing.is_empty() {
        new_path.push(";");
        new_path.push(&existing);
    }
    env::set_var("PATH", new_path);
    true
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
        let brew = Command::new("brew")
            .args(["install", "ollama"])
            .status()
            .await;
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
        // No silent install on Windows — the user has to run the official
        // installer. Surface the URL so the GUI can render it as a link.
        return Err(anyhow!(
            "Ollama for Windows must be installed manually. Download it from https://ollama.com/download then click Retry."
        ));
    }
    Ok(())
}

pub async fn ensure_running() -> Result<()> {
    if api_reachable().await {
        return Ok(());
    }

    let mut guard = process_lock().lock().await;
    // Check again after acquiring the lock.
    if api_reachable().await {
        return Ok(());
    }

    // OLLAMA_ORIGINS=* belt-and-suspenders: when WE spawn the server (e.g. Linux
    // or a fresh standalone Windows install), this lets the GUI fetch directly
    // from `http://127.0.0.1:11434` without Ollama's CORS allowlist rejecting
    // the WebView's `Origin` (which on Tauri 2 / Windows is `http://tauri.localhost`,
    // not in Ollama's defaults). When the Windows installer runs Ollama as a
    // tray service we can't influence its env — that's why the GUI also routes
    // chat through anyai's API server (see Chat.svelte).
    let child = Command::new("ollama")
        .arg("serve")
        .env("OLLAMA_ORIGINS", "*")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to spawn ollama serve")?;

    *guard = Some(child);

    // Wait up to 10 seconds for API to become reachable.
    for _ in 0..20 {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        if api_reachable().await {
            return Ok(());
        }
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

/// Structured progress emitted while pulling a model.
///
/// `total` and `completed` are byte counts for the layer being pulled when
/// available; both are 0 for status-only frames (`pulling manifest`,
/// `verifying sha256 digest`, `writing manifest`, `success`, …).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PullEvent {
    pub status: String,
    #[serde(default)]
    pub digest: String,
    #[serde(default)]
    pub total: u64,
    #[serde(default)]
    pub completed: u64,
    /// 0.0–1.0 if `total > 0`, otherwise None.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub percent: Option<f64>,
    #[serde(default)]
    pub done: bool,
}

impl PullEvent {
    /// Compact human-readable rendering used by the CLI and by API/preload
    /// fallbacks where the consumer expects a single string.
    pub fn render(&self) -> String {
        if self.total > 0 {
            let pct = self.percent.unwrap_or(0.0) * 100.0;
            format!(
                "{} {:.1}% ({}/{})",
                self.status,
                pct,
                fmt_bytes(self.completed),
                fmt_bytes(self.total),
            )
        } else {
            self.status.clone()
        }
    }
}

fn fmt_bytes(n: u64) -> String {
    const K: f64 = 1024.0;
    let n = n as f64;
    if n >= K * K * K {
        format!("{:.2} GB", n / (K * K * K))
    } else if n >= K * K {
        format!("{:.1} MB", n / (K * K))
    } else if n >= K {
        format!("{:.0} KB", n / K)
    } else {
        format!("{n} B")
    }
}

pub async fn pull(model: &str, window: &tauri::WebviewWindow) -> Result<()> {
    pull_with(model, |evt| {
        let _ = window.emit("ollama-pull-progress", evt.clone());
    })
    .await
}

/// Pull a model via Ollama's HTTP API (`POST /api/pull`) and invoke `on_event`
/// for each streamed progress frame. Idempotent: returns immediately if the
/// model is already present.
///
/// Why HTTP instead of `ollama pull` subprocess:
/// 1. The CLI emits its progress to stderr using `\r`-replaced lines, which
///    `BufReader::lines()` only sees at the end — the GUI saw "Starting…"
///    forever.
/// 2. We were piping stderr without ever reading it. Once the kernel pipe
///    buffer (~64 KB) filled, the child blocked on its next stderr write,
///    making the download appear to stall — that's the "very slow" report.
///
/// The HTTP API streams reliable JSON frames and avoids both pitfalls.
pub async fn pull_with<F: FnMut(&PullEvent)>(model: &str, mut on_event: F) -> Result<()> {
    if has_model(model).await? {
        let mut done = PullEvent {
            status: "already pulled".into(),
            done: true,
            ..Default::default()
        };
        done.percent = Some(1.0);
        on_event(&done);
        return Ok(());
    }

    // The HTTP API needs the daemon up. ensure_running is a no-op when it's
    // already reachable (the common Windows path: tray app already serving).
    ensure_running().await?;

    let client = reqwest::Client::builder()
        // No total timeout — large pulls take many minutes.
        .pool_idle_timeout(Duration::from_secs(30))
        .build()
        .context("reqwest client")?;

    let body = serde_json::json!({ "name": model, "stream": true });
    let resp = client
        .post("http://127.0.0.1:11434/api/pull")
        .json(&body)
        .send()
        .await
        .context("POST /api/pull")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let detail = resp.text().await.unwrap_or_default();
        return Err(anyhow!("ollama pull HTTP {status}: {detail}"));
    }

    // Frames are NDJSON. A single chunk can hold partial frames or several
    // frames concatenated, so buffer and split on '\n'.
    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::with_capacity(8 * 1024);

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("read /api/pull stream")?;
        buf.extend_from_slice(&chunk);
        while let Some(nl) = buf.iter().position(|b| *b == b'\n') {
            let line = buf.drain(..=nl).collect::<Vec<u8>>();
            let line = &line[..line.len() - 1]; // drop '\n'
            if line.is_empty() {
                continue;
            }
            let mut evt: PullEvent = match serde_json::from_slice(line) {
                Ok(e) => e,
                Err(_) => continue,
            };
            if evt.total > 0 {
                evt.percent = Some((evt.completed as f64 / evt.total as f64).clamp(0.0, 1.0));
            }
            // Ollama signals success with {"status":"success"}.
            if evt.status.eq_ignore_ascii_case("success") {
                evt.done = true;
                evt.percent = Some(1.0);
            }
            // Ollama can also surface errors mid-stream:
            // {"error":"pull model manifest: ..."}.
            // serde_json::from_slice into PullEvent drops `error`; re-parse to catch it.
            if let Ok(v) = serde_json::from_slice::<serde_json::Value>(line) {
                if let Some(err) = v.get("error").and_then(|e| e.as_str()) {
                    return Err(anyhow!("ollama pull error: {err}"));
                }
            }
            on_event(&evt);
        }
    }

    // Final confirmation: if the daemon ended the stream without a "success"
    // frame, the model still has to actually exist locally for us to call this
    // a successful pull.
    if !has_model(model).await.unwrap_or(false) {
        return Err(anyhow!(
            "ollama pull finished but model {model} is not present"
        ));
    }
    Ok(())
}

/// True if the named model+tag is already pulled.
pub async fn has_model(model: &str) -> Result<bool> {
    let out = Command::new("ollama")
        .args(["show", "--modelfile", model])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .context("ollama show")?;
    Ok(out.success())
}

/// Fire a 1-token chat call so Ollama mmaps the weights and keeps the model loaded
/// for `keep_alive`. Used by `anyai preload --warm`.
pub async fn warm(model: &str) -> Result<()> {
    let body = serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": "ok"}],
        "stream": false,
        "keep_alive": "10m",
        "options": { "num_predict": 1 }
    })
    .to_string();
    let out = Command::new("curl")
        .args([
            "-sf",
            "--max-time",
            "120",
            "-X",
            "POST",
            "http://127.0.0.1:11434/api/chat",
            "-H",
            "Content-Type: application/json",
            "-d",
            &body,
        ])
        .output()
        .await
        .context("curl warm")?;
    if !out.status.success() {
        return Err(anyhow!("warm-up call failed for {model}"));
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
    // `ollama list` has no `--json` flag; cobra rejects unknown flags with a
    // non-zero exit, which silently turned every "list installed models" call
    // into Ok(vec![]). The Models tab showed nothing and the cleanup pass had
    // nothing to evaluate, so pulled models piled up indefinitely. The HTTP
    // API at /api/tags is the canonical structured-output surface.
    let body = match reqwest_get("http://127.0.0.1:11434/api/tags").await {
        Ok(b) => b,
        Err(_) => return Ok(vec![]),
    };
    Ok(parse_tags_response(&body))
}

/// Parse the `/api/tags` response body. Pulled out of `list_models` so the
/// JSON shape contract is testable without a running daemon.
fn parse_tags_response(body: &str) -> Vec<ModelInfo> {
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(body) else {
        return vec![];
    };
    let Some(arr) = parsed["models"].as_array() else {
        return vec![];
    };
    let mut models = Vec::with_capacity(arr.len());
    for entry in arr {
        let name = entry["name"].as_str().unwrap_or("").to_string();
        let size = entry["size"].as_u64().unwrap_or(0);
        if !name.is_empty() {
            models.push(ModelInfo { name, size });
        }
    }
    models
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

/// Streamed chat completion. Calls `on_delta` for each token chunk and
/// `on_done` once when the stream ends. Same CORS-bypass rationale as
/// `chat_once`.
pub async fn chat_stream<FD, FE>(
    model: &str,
    messages: serde_json::Value,
    mut on_delta: FD,
    on_done: FE,
) -> Result<()>
where
    FD: FnMut(&str),
    FE: FnOnce(),
{
    ensure_running().await?;
    let client = reqwest::Client::builder()
        .pool_idle_timeout(Duration::from_secs(30))
        .build()
        .context("reqwest client")?;
    let body = serde_json::json!({
        "model": model,
        "messages": messages,
        "stream": true,
    });
    let resp = client
        .post("http://127.0.0.1:11434/api/chat")
        .json(&body)
        .send()
        .await
        .context("POST /api/chat")?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(anyhow!("ollama HTTP {status}: {text}"));
    }

    // Frames are NDJSON. Buffer + split on '\n' since chunks can carry
    // partial frames or several at once (same shape as /api/pull).
    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::with_capacity(4 * 1024);
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("read /api/chat stream")?;
        buf.extend_from_slice(&chunk);
        while let Some(nl) = buf.iter().position(|b| *b == b'\n') {
            let line = buf.drain(..=nl).collect::<Vec<u8>>();
            let line = &line[..line.len() - 1];
            if line.is_empty() {
                continue;
            }
            let v: serde_json::Value = match serde_json::from_slice(line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if let Some(err) = v.get("error").and_then(|e| e.as_str()) {
                return Err(anyhow!("ollama error: {err}"));
            }
            if let Some(delta) = v["message"]["content"].as_str() {
                if !delta.is_empty() {
                    on_delta(delta);
                }
            }
            if v["done"].as_bool().unwrap_or(false) {
                on_done();
                return Ok(());
            }
        }
    }
    on_done();
    Ok(())
}

/// One-shot non-streaming chat completion against the local Ollama daemon.
///
/// Used by the Tauri GUI chat: going through the WebView's fetch fails on
/// Windows because Tauri 2 serves pages from `http://tauri.localhost`, which
/// isn't in Ollama's default CORS allowlist (it lists `tauri://*` but not
/// `http://tauri.localhost`) — the daemon answers 403. Calling Ollama from
/// Rust via reqwest sidesteps that entirely: reqwest doesn't set an Origin
/// header, so Ollama treats the request as same-origin and lets it through.
pub async fn chat_once(model: &str, messages: serde_json::Value) -> Result<String> {
    ensure_running().await?;
    let client = reqwest::Client::builder()
        .pool_idle_timeout(Duration::from_secs(30))
        .build()
        .context("reqwest client")?;
    let body = serde_json::json!({
        "model": model,
        "messages": messages,
        "stream": false,
    });
    let resp = client
        .post("http://127.0.0.1:11434/api/chat")
        .json(&body)
        .send()
        .await
        .context("POST /api/chat")?;
    let status = resp.status();
    let text = resp.text().await.context("read /api/chat response")?;
    if !status.is_success() {
        return Err(anyhow!("ollama HTTP {status}: {text}"));
    }
    let v: serde_json::Value =
        serde_json::from_str(&text).with_context(|| format!("parse /api/chat response: {text}"))?;
    if let Some(err) = v.get("error").and_then(|e| e.as_str()) {
        return Err(anyhow!("ollama error: {err}"));
    }
    Ok(v["message"]["content"]
        .as_str()
        .unwrap_or_default()
        .to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real-world `/api/tags` response (trimmed) — wrapper object with a
    /// `models` array, each entry carrying `name` and `size` (bytes).
    #[test]
    fn parse_tags_response_extracts_name_and_size() {
        let body = r#"{
          "models": [
            { "name": "llama3.2:3b", "size": 2019393189, "modified_at": "2024-09-01T00:00:00Z" },
            { "name": "qwen2.5:7b",  "size": 4683072932 }
          ]
        }"#;
        let models = parse_tags_response(body);
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].name, "llama3.2:3b");
        assert_eq!(models[0].size, 2019393189);
        assert_eq!(models[1].name, "qwen2.5:7b");
        assert_eq!(models[1].size, 4683072932);
    }

    #[test]
    fn parse_tags_response_returns_empty_on_garbage() {
        assert!(parse_tags_response("").is_empty());
        assert!(parse_tags_response("not json").is_empty());
        assert!(parse_tags_response(r#"{"foo":"bar"}"#).is_empty());
        assert!(parse_tags_response(r#"{"models":"not an array"}"#).is_empty());
    }

    #[test]
    fn parse_tags_response_skips_entries_without_name() {
        let body = r#"{"models":[{"size":100},{"name":"","size":1},{"name":"a","size":1}]}"#;
        let models = parse_tags_response(body);
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].name, "a");
    }

    #[test]
    fn parse_tags_response_handles_empty_models_array() {
        assert!(parse_tags_response(r#"{"models":[]}"#).is_empty());
    }
}
