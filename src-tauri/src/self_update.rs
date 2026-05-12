//! Self-update.
//!
//! Goals: MyOwnLLM is set-it-and-forget-it. Once installed as a raw binary, the
//! background watcher periodically checks the GitHub releases endpoint, and
//! according to the user's `auto_apply` policy:
//!   - Stages a verified copy of the new binary at ~/.myownllm/updates/<version>/.
//!   - Writes ~/.myownllm/updates/pending.json so the next process start applies it.
//!
//! When a new process starts (`apply_pending_if_any`), it atomically renames
//! the staged binary over the current binary and clears the pending marker.
//! In-process restart of running daemons is intentionally NOT done here — that
//! would yank the rug out from under in-flight requests. The model is
//! "stage now, apply on next launch."
//!
//! Package-manager installs (Homebrew, dpkg/apt, rpm, MSI, Chocolatey) are
//! detected and skipped — the OS package manager owns versioning there.

use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::Duration;

// Build-time overridable defaults. A vendor can point the same binary at their
// own release host by setting these env vars at compile time:
//   MYOWNLLM_RELEASE_URL_STABLE=https://example.com/releases/latest cargo build
// At runtime, `auto_update.stable_url` / `auto_update.beta_url` in config.json
// take precedence over these defaults, so end users can also redirect without
// rebuilding.
pub(crate) fn default_release_api_stable() -> &'static str {
    option_env!("MYOWNLLM_RELEASE_URL_STABLE")
        .unwrap_or("https://api.github.com/repos/mrjeeves/MyOwnLLM/releases/latest")
}
pub(crate) fn default_release_api_beta() -> &'static str {
    option_env!("MYOWNLLM_RELEASE_URL_BETA")
        .unwrap_or("https://api.github.com/repos/mrjeeves/MyOwnLLM/releases")
}

const USER_AGENT: &str = concat!("myownllm-self-update/", env!("CARGO_PKG_VERSION"));

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallKind {
    /// Raw binary (curl install, manual placement). Eligible for self-update.
    Raw,
    /// Homebrew, dpkg/apt, rpm, MSI, Chocolatey. We never self-update these.
    PackageManager,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyPolicy {
    Patch,
    Minor,
    All,
    None,
}

impl ApplyPolicy {
    fn parse(s: &str) -> Self {
        match s {
            "minor" => Self::Minor,
            "all" => Self::All,
            "none" => Self::None,
            _ => Self::Patch,
        }
    }
}

/// Apply any staged update for the current process before it starts doing real
/// work. Idempotent. Errors are logged and swallowed: an update problem must
/// not prevent the binary from starting.
pub fn apply_pending_if_any() {
    cleanup_old_replaced_binary();
    if let Err(e) = apply_pending() {
        eprintln!("self-update: apply skipped: {e}");
    }
}

/// Apply pending and surface the error. Used by the GUI's "Restart to apply"
/// flow which needs to know whether the swap actually happened before it
/// triggers `app.restart()` — otherwise a non-fatal swap failure would leave
/// the user looking at the old version and assuming the update worked.
pub fn apply_pending_strict() -> Result<()> {
    cleanup_old_replaced_binary();
    apply_pending()
}

fn apply_pending() -> Result<()> {
    let dir = crate::myownllm_dir()?.join("updates");
    let pending = dir.join("pending.json");
    if !pending.exists() {
        return Ok(());
    }
    let pending_doc: Value = serde_json::from_str(&std::fs::read_to_string(&pending)?)?;
    let staged_path = pending_doc["path"]
        .as_str()
        .ok_or_else(|| anyhow!("pending.json missing path"))?;
    let target_version = pending_doc["version"].as_str().unwrap_or("?");

    // Refuse downgrades and same-version applies. A stale pending.json left
    // over from a previous version's broken self-update can otherwise replace
    // a freshly-installed binary with an older one — see the 0.1.4 → 0.1.5
    // regression where the defensive extract was unwittingly rolling users
    // back to 0.1.4 because a year-old pending.json still pointed at it.
    let current = env!("CARGO_PKG_VERSION");
    if compare_semver(target_version, current) != std::cmp::Ordering::Greater {
        let _ = std::fs::remove_file(&pending);
        return Ok(());
    }

    let staged = PathBuf::from(staged_path);
    if !staged.exists() {
        let _ = std::fs::remove_file(&pending);
        return Err(anyhow!(
            "staged binary {staged:?} missing — clearing marker"
        ));
    }

    // Legacy pending.json may point at the downloaded archive itself (a bug
    // in <=0.1.4 that wrote the .tar.gz over the binary, producing
    // "exec format error"). If we can see we're holding an archive, extract
    // it on the fly into the same directory and apply the embedded binary.
    let staged_dir = staged
        .parent()
        .ok_or_else(|| anyhow!("staged path has no parent"))?;
    let staged = extract_binary_if_archived(&staged, staged_dir, /*verbose=*/ false)?;

    let current_exe = std::env::current_exe().context("current_exe")?;
    atomic_replace(&staged, &current_exe)?;
    let _ = std::fs::remove_file(&pending);
    eprintln!("self-update: applied {target_version}");
    Ok(())
}

/// One tick of the update check, intended to be called from the watcher loop.
/// Cheap when nothing is due (gated by `check_interval_hours`).
pub async fn tick() -> Result<()> {
    run_check(false).await
}

/// Check + stage now, ignoring the cooldown and printing user-facing output.
/// Used by `myownllm update check`.
pub async fn force_check() -> Result<()> {
    run_check(true).await
}

async fn run_check(force: bool) -> Result<()> {
    let cfg = crate::resolver::load_config_value()?;
    let au = &cfg["auto_update"];
    if !au["enabled"].as_bool().unwrap_or(true) {
        if force {
            println!("self-update is disabled in config (auto_update.enabled=false).");
        }
        return Ok(());
    }
    if std::env::var("MYOWNLLM_AUTOUPDATE")
        .map(|v| v == "0" || v.eq_ignore_ascii_case("false"))
        .unwrap_or(false)
    {
        if force {
            println!("self-update is disabled via MYOWNLLM_AUTOUPDATE=0.");
        }
        return Ok(());
    }

    if detect_install_kind() == InstallKind::PackageManager {
        let marker = crate::myownllm_dir()?.join("updates/pm-detected.flag");
        if !marker.exists() {
            let _ = std::fs::create_dir_all(marker.parent().unwrap());
            let _ = std::fs::write(&marker, "skip");
            if !force {
                eprintln!(
                    "self-update: package-manager install detected; deferring to system updater."
                );
            }
        }
        if force {
            println!(
                "Package-manager install detected; self-update is deferred to the system updater."
            );
        }
        return Ok(());
    }

    if !force {
        let interval_hours = au["check_interval_hours"].as_f64().unwrap_or(6.0);
        if !is_due(interval_hours)? {
            return Ok(());
        }
    }
    stamp_check_now()?;

    let channel = au["channel"].as_str().unwrap_or("stable");
    let policy = ApplyPolicy::parse(au["auto_apply"].as_str().unwrap_or("patch"));
    let release = fetch_release(channel).await?;

    let latest_version = release["tag_name"]
        .as_str()
        .map(|s| s.trim_start_matches('v').to_string())
        .ok_or_else(|| anyhow!("release missing tag_name"))?;
    let current = env!("CARGO_PKG_VERSION");
    let cmp = compare_semver(current, &latest_version);
    if cmp != std::cmp::Ordering::Less {
        if force {
            if cmp == std::cmp::Ordering::Equal {
                println!("Already on the latest version ({current}).");
            } else {
                println!(
                    "Already up to date — you're on {current} (latest published: {latest_version})."
                );
            }
        }
        return Ok(());
    }

    if !policy_allows(policy, current, &latest_version) {
        let policy_str = au["auto_apply"].as_str().unwrap_or("patch");
        if force {
            println!(
                "{latest_version} is available (current {current}), but auto_apply='{policy_str}' \
                 does not permit this jump. Set auto_apply=\"all\" in ~/.myownllm/config.json to allow it."
            );
        } else {
            eprintln!(
                "self-update: {latest_version} available (current {current}); auto_apply='{policy_str}', staging skipped."
            );
        }
        return Ok(());
    }

    if force {
        println!("Staging {latest_version}…");
    }
    if let Err(e) = stage_release(&release, &latest_version).await {
        if force {
            return Err(anyhow!("stage failed: {e}"));
        }
        eprintln!("self-update: stage failed: {e}");
    } else if force {
        println!(
            "Run `myownllm update apply` (or just relaunch myownllm) to switch to {latest_version}."
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// GUI surface (Tauri commands).
//
// The Updates tab in Settings reads this status and drives a "Check now" /
// "Restart to apply" flow. Same building blocks as the watcher and CLI; just
// shaped as serde-friendly returns instead of stdout prints.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct PendingUpdate {
    pub version: String,
    pub staged_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpdateStatus {
    pub current_version: String,
    /// "raw" or "package_manager".
    pub install_kind: String,
    pub enabled: bool,
    pub channel: String,
    pub auto_apply: String,
    pub check_interval_hours: f64,
    /// Unix seconds of the last successful check, if any.
    pub last_check_unix: Option<i64>,
    pub pending: Option<PendingUpdate>,
    /// Effective release-feed URL for the active channel. Reflects, in priority
    /// order: `auto_update.stable_url`/`beta_url` in config → the build-time
    /// `MYOWNLLM_RELEASE_URL_*` override → the GitHub default.
    pub release_url: String,
    /// True when `release_url` is not the project's GitHub default — i.e. the
    /// binary was rebuilt with `MYOWNLLM_RELEASE_URL_*` set, or the user
    /// overrode it in `~/.myownllm/config.json`.
    pub release_url_overridden: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CheckOutcome {
    /// `auto_update.enabled = false` or `MYOWNLLM_AUTOUPDATE=0`.
    Disabled,
    /// Install lives under a package manager — defer to the OS.
    PackageManager,
    UpToDate {
        current: String,
        latest: String,
    },
    /// New binary downloaded, verified, and staged for next launch.
    Staged {
        version: String,
    },
    /// Newer release exists but `auto_apply` won't permit it.
    PolicyBlocked {
        current: String,
        latest: String,
        policy: String,
    },
}

pub fn status() -> Result<UpdateStatus> {
    let cfg = crate::resolver::load_config_value().unwrap_or_else(|_| serde_json::json!({}));
    let au = &cfg["auto_update"];
    let install_kind = match detect_install_kind() {
        InstallKind::Raw => "raw",
        InstallKind::PackageManager => "package_manager",
    };
    let channel = au["channel"].as_str().unwrap_or("stable").to_string();
    let release_url = resolve_release_url(au, &channel);
    let release_url_overridden = release_url != github_default_release_url(&channel);

    Ok(UpdateStatus {
        current_version: env!("CARGO_PKG_VERSION").to_string(),
        install_kind: install_kind.to_string(),
        enabled: au["enabled"].as_bool().unwrap_or(true),
        channel,
        auto_apply: au["auto_apply"].as_str().unwrap_or("patch").to_string(),
        check_interval_hours: au["check_interval_hours"].as_f64().unwrap_or(6.0),
        last_check_unix: read_last_check(),
        pending: read_pending_or_clean()?,
        release_url,
        release_url_overridden,
    })
}

/// The MyOwnLLM project's own GitHub release endpoints, used to detect when
/// the effective release URL has been redirected away from upstream.
fn github_default_release_url(channel: &str) -> &'static str {
    if channel == "beta" {
        "https://api.github.com/repos/mrjeeves/MyOwnLLM/releases"
    } else {
        "https://api.github.com/repos/mrjeeves/MyOwnLLM/releases/latest"
    }
}

/// Persist `auto_update.enabled`. Used by the CLI (`update enable`/`disable`)
/// and the GUI toggle so both surfaces share one code path.
pub fn set_enabled(enabled: bool) -> Result<()> {
    let mut cfg = crate::resolver::load_config_value()?;
    let au = cfg
        .get_mut("auto_update")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| anyhow!("config missing auto_update"))?;
    au.insert("enabled".to_string(), Value::Bool(enabled));
    crate::resolver::save_config_value(&cfg)?;
    Ok(())
}

/// Read `~/.myownllm/updates/pending.json` and return it only if it actually
/// represents an upgrade over the running binary. A stale entry left behind
/// after a manual install or a release rollback (e.g. user is now on 0.1.14
/// but pending still references 0.1.5) is silently deleted — otherwise the
/// GUI would keep displaying a misleading "Update staged" banner forever
/// even though `check_now()` correctly reports up-to-date.
fn read_pending_or_clean() -> Result<Option<PendingUpdate>> {
    read_pending_or_clean_at(
        &crate::myownllm_dir()?.join("updates/pending.json"),
        env!("CARGO_PKG_VERSION"),
    )
}

fn read_pending_or_clean_at(path: &Path, current: &str) -> Result<Option<PendingUpdate>> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => {
            let _ = std::fs::remove_file(path);
            return Ok(None);
        }
    };
    let v: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => {
            let _ = std::fs::remove_file(path);
            return Ok(None);
        }
    };
    let version = v["version"].as_str().unwrap_or("?").to_string();
    let staged_at = v["staged_at"].as_str().unwrap_or("?").to_string();
    if compare_semver(&version, current) != std::cmp::Ordering::Greater {
        let _ = std::fs::remove_file(path);
        return Ok(None);
    }
    Ok(Some(PendingUpdate { version, staged_at }))
}

fn read_last_check() -> Option<i64> {
    let path = check_marker_path().ok()?;
    if !path.exists() {
        return None;
    }
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| s.trim().parse::<i64>().ok())
}

/// Force a release check and return a structured outcome the GUI can render.
/// Mirrors the gating logic in `run_check` but reports the result instead of
/// printing it. Always bypasses the cooldown — a user clicking "Check now"
/// has consented to a network call.
pub async fn check_now() -> Result<CheckOutcome> {
    let cfg = crate::resolver::load_config_value()?;
    let au = &cfg["auto_update"];

    if !au["enabled"].as_bool().unwrap_or(true) {
        return Ok(CheckOutcome::Disabled);
    }
    if std::env::var("MYOWNLLM_AUTOUPDATE")
        .map(|v| v == "0" || v.eq_ignore_ascii_case("false"))
        .unwrap_or(false)
    {
        return Ok(CheckOutcome::Disabled);
    }
    if detect_install_kind() == InstallKind::PackageManager {
        return Ok(CheckOutcome::PackageManager);
    }

    stamp_check_now()?;

    let channel = au["channel"].as_str().unwrap_or("stable");
    let policy_str = au["auto_apply"].as_str().unwrap_or("patch").to_string();
    let policy = ApplyPolicy::parse(&policy_str);
    let release = fetch_release(channel).await?;

    let latest = release["tag_name"]
        .as_str()
        .map(|s| s.trim_start_matches('v').to_string())
        .ok_or_else(|| anyhow!("release missing tag_name"))?;
    let current = env!("CARGO_PKG_VERSION").to_string();

    if compare_semver(&current, &latest) != std::cmp::Ordering::Less {
        return Ok(CheckOutcome::UpToDate { current, latest });
    }
    if !policy_allows(policy, &current, &latest) {
        return Ok(CheckOutcome::PolicyBlocked {
            current,
            latest,
            policy: policy_str,
        });
    }

    stage_release(&release, &latest).await?;
    Ok(CheckOutcome::Staged { version: latest })
}

// ---------------------------------------------------------------------------
// Install-kind detection.
// ---------------------------------------------------------------------------

pub fn detect_install_kind() -> InstallKind {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return InstallKind::Raw,
    };
    detect_install_kind_from_path(&exe.to_string_lossy())
}

fn detect_install_kind_from_path(path_str: &str) -> InstallKind {
    // Homebrew on macOS / Linux.
    if path_str.contains("/Cellar/")
        || path_str.starts_with("/opt/homebrew/")
        || path_str.starts_with("/home/linuxbrew/")
    {
        return InstallKind::PackageManager;
    }

    // System paths typically mean dpkg/rpm.
    #[cfg(target_os = "linux")]
    if path_str.starts_with("/usr/bin/") || path_str.starts_with("/usr/sbin/") {
        return InstallKind::PackageManager;
    }

    // Windows: typical MSI install location and Chocolatey lib paths.
    #[cfg(target_os = "windows")]
    {
        let lower = path_str.to_lowercase();
        if lower.contains(r"\program files\")
            || lower.contains(r"\program files (x86)\")
            || lower.contains(r"\chocolatey\lib\")
            || lower.contains(r"\scoop\apps\")
        {
            return InstallKind::PackageManager;
        }
    }

    InstallKind::Raw
}

// ---------------------------------------------------------------------------
// GitHub releases fetch.
// ---------------------------------------------------------------------------

async fn fetch_release(channel: &str) -> Result<Value> {
    let cfg = crate::resolver::load_config_value().unwrap_or_else(|_| serde_json::json!({}));
    fetch_release_at(channel, &resolve_release_url(&cfg["auto_update"], channel)).await
}

/// Resolve which release-feed URL to use. Order: explicit `auto_update.stable_url`
/// / `auto_update.beta_url` in config → build-time `MYOWNLLM_RELEASE_URL_*`
/// override → the project's GitHub releases endpoint.
pub(crate) fn resolve_release_url(au: &Value, channel: &str) -> String {
    let (key, fallback) = if channel == "beta" {
        ("beta_url", default_release_api_beta())
    } else {
        ("stable_url", default_release_api_stable())
    };
    au.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or(fallback)
        .to_string()
}

async fn fetch_release_at(channel: &str, url: &str) -> Result<Value> {
    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(15))
        .build()?;
    let resp = client.get(url).send().await?;
    if !resp.status().is_success() {
        return Err(anyhow!("releases endpoint returned {}", resp.status()));
    }
    let body: Value = resp.json().await?;
    if channel == "beta" {
        // /releases returns an array. Pick the first non-draft, prefer prerelease.
        let arr = body.as_array().ok_or_else(|| anyhow!("expected array"))?;
        for r in arr {
            if r["draft"].as_bool().unwrap_or(false) {
                continue;
            }
            return Ok(r.clone());
        }
        return Err(anyhow!("no usable release on beta channel"));
    }
    Ok(body)
}

// ---------------------------------------------------------------------------
// Versioning.
// ---------------------------------------------------------------------------

fn compare_semver(a: &str, b: &str) -> std::cmp::Ordering {
    let parse = |s: &str| -> (u64, u64, u64) {
        let core = s.split('-').next().unwrap_or(s);
        let mut it = core.split('.').map(|p| p.parse::<u64>().unwrap_or(0));
        (
            it.next().unwrap_or(0),
            it.next().unwrap_or(0),
            it.next().unwrap_or(0),
        )
    };
    parse(a).cmp(&parse(b))
}

fn policy_allows(policy: ApplyPolicy, current: &str, latest: &str) -> bool {
    let parse = |s: &str| -> (u64, u64, u64) {
        let core = s.split('-').next().unwrap_or(s);
        let mut it = core.split('.').map(|p| p.parse::<u64>().unwrap_or(0));
        (
            it.next().unwrap_or(0),
            it.next().unwrap_or(0),
            it.next().unwrap_or(0),
        )
    };
    let (cm, cn, _cp) = parse(current);
    let (lm, ln, _lp) = parse(latest);
    match policy {
        ApplyPolicy::None => false,
        ApplyPolicy::All => true,
        ApplyPolicy::Minor => lm == cm,
        ApplyPolicy::Patch => lm == cm && ln == cn,
    }
}

// ---------------------------------------------------------------------------
// Asset matching, download, verify, stage.
// ---------------------------------------------------------------------------

fn current_target_triple_hint() -> &'static str {
    // Matches the substrings GitHub release assets typically embed.
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        "linux-x86_64"
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        "linux-aarch64"
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        "macos-x86_64"
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        "macos-aarch64"
    }
    #[cfg(target_os = "windows")]
    {
        "windows-x86_64"
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        "unknown"
    }
}

fn pick_asset(assets: &[Value]) -> Option<&Value> {
    let needle = current_target_triple_hint();
    // Both `myownllm-macos-aarch64.tar.gz` and `myownllm-macos-aarch64.tar.gz.sha256`
    // contain the platform needle. GitHub's asset ordering is not guaranteed,
    // and if the sidecar comes back first a naive .contains() check picks the
    // 64-byte checksum file as "the binary" — which is exactly how 0.1.6 →
    // 0.1.9 self-update wrote a hex string over `~/.local/bin/myownllm` on macOS.
    assets.iter().find(|a| {
        a["name"]
            .as_str()
            .is_some_and(|n| n.contains(needle) && !is_sidecar_asset(n))
    })
}

/// True for files that ride alongside a release artifact (checksums, signatures,
/// detached PGP, etc.) and must never be picked as the binary itself.
fn is_sidecar_asset(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".sha256")
        || lower.ends_with(".sha512")
        || lower.ends_with(".sig")
        || lower.ends_with(".asc")
        || lower.ends_with(".minisig")
        || lower.ends_with(".pem")
}

fn pick_sha_asset<'a>(assets: &'a [Value], asset_name: &str) -> Option<&'a Value> {
    let preferred = format!("{asset_name}.sha256");
    if let Some(matching) = assets
        .iter()
        .find(|a| a["name"].as_str() == Some(preferred.as_str()))
    {
        return Some(matching);
    }
    assets.iter().find(|a| {
        a["name"]
            .as_str()
            .map(|n| n.eq_ignore_ascii_case("SHA256SUMS"))
            .unwrap_or(false)
    })
}

async fn stage_release(release: &Value, version: &str) -> Result<()> {
    let staged_binary = download_verify_extract(release, version, /*verbose=*/ false).await?;
    write_pending_marker(&staged_binary, version)?;
    eprintln!(
        "self-update: staged {version} at {} (apply on next launch)",
        staged_binary.display()
    );
    Ok(())
}

/// Download the platform asset, verify its SHA256, and (if it's an archive)
/// extract the embedded `myownllm` / `myownllm.exe` binary. Returns the path of
/// the verified executable on disk. Does NOT write `pending.json` —
/// callers decide whether to apply now or stage for next launch.
async fn download_verify_extract(release: &Value, version: &str, verbose: bool) -> Result<PathBuf> {
    let assets = release["assets"]
        .as_array()
        .ok_or_else(|| anyhow!("release missing assets"))?;
    let asset = pick_asset(assets).ok_or_else(|| {
        anyhow!(
            "no release asset matches current platform ({})",
            current_target_triple_hint()
        )
    })?;
    let dl_url = asset["browser_download_url"]
        .as_str()
        .ok_or_else(|| anyhow!("asset missing browser_download_url"))?;
    let asset_name = asset["name"].as_str().unwrap_or("myownllm");
    let asset_size = asset["size"].as_u64();

    let updates_dir = crate::myownllm_dir()?.join("updates").join(version);
    std::fs::create_dir_all(&updates_dir)?;
    let archive_path = updates_dir.join(asset_name);
    let part_path = updates_dir.join(format!("{asset_name}.part"));

    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(300))
        .build()?;

    if verbose {
        match asset_size {
            Some(n) => println!("Downloading {asset_name} ({})…", human_bytes(n)),
            None => println!("Downloading {asset_name}…"),
        }
    }
    let bytes = client
        .get(dl_url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    std::fs::write(&part_path, &bytes)?;

    if let Some(sha_asset) = pick_sha_asset(assets, asset_name) {
        let sha_url = sha_asset["browser_download_url"]
            .as_str()
            .ok_or_else(|| anyhow!("sha asset missing url"))?;
        let sha_text = client
            .get(sha_url)
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;
        let expected = expected_sha_for(&sha_text, asset_name)
            .ok_or_else(|| anyhow!("SHA256SUMS does not list an entry for {asset_name}"))?;
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        let actual = hex::encode(hasher.finalize());
        if !actual.eq_ignore_ascii_case(&expected) {
            let _ = std::fs::remove_file(&part_path);
            return Err(anyhow!(
                "sha256 mismatch for {asset_name}: expected {expected}, got {actual}"
            ));
        }
        if verbose {
            println!("Verified SHA256: {}…", &actual[..12]);
        }
    } else if verbose {
        println!("warning: no SHA256SUMS in release; skipping integrity check.");
    }

    std::fs::rename(&part_path, &archive_path)?;

    let binary = extract_binary_if_archived(&archive_path, &updates_dir, verbose)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&binary)?.permissions();
        perms.set_mode(0o755);
        let _ = std::fs::set_permissions(&binary, perms);
    }

    Ok(binary)
}

fn write_pending_marker(staged: &Path, version: &str) -> Result<()> {
    let pending_path = crate::myownllm_dir()?.join("updates/pending.json");
    let pending_doc = serde_json::json!({
        "version": version,
        "path": staged.to_string_lossy(),
        "staged_at": iso_now(),
    });
    std::fs::write(&pending_path, serde_json::to_string_pretty(&pending_doc)?)?;
    Ok(())
}

/// If `archive` is a tar.gz / tgz / zip, run `tar -xf` and return the path
/// to the embedded `myownllm` (or `myownllm.exe`). If it's already a raw binary,
/// return it unchanged.
///
/// Uses the system `tar`. On every target we ship for (macOS, Linux,
/// Windows 10 1803+), `tar` is libarchive-backed and auto-detects gzipped
/// tarballs and zip files via `tar -xf`.
fn extract_binary_if_archived(archive: &Path, dest_dir: &Path, verbose: bool) -> Result<PathBuf> {
    let name = archive.file_name().and_then(|s| s.to_str()).unwrap_or("");
    // Refuse to treat a checksum/signature sidecar as the binary. A pre-fix
    // version (≤0.1.9) could write a `.sha256` path into pending.json; this
    // catches that case before atomic_replace clobbers the live binary.
    if is_sidecar_asset(name) {
        return Err(anyhow!(
            "refusing to install sidecar `{name}` as the myownllm binary"
        ));
    }
    let is_archive = name.ends_with(".tar.gz") || name.ends_with(".tgz") || name.ends_with(".zip");
    if !is_archive {
        return Ok(archive.to_path_buf());
    }

    #[cfg(windows)]
    let bin_name = "myownllm.exe";
    #[cfg(not(windows))]
    let bin_name = "myownllm";

    let bin_path = dest_dir.join(bin_name);
    // A stale extract from a previous run could shadow a corrupted re-download;
    // wipe it so we know the file in place came from THIS archive.
    let _ = std::fs::remove_file(&bin_path);

    if verbose {
        println!("Extracting {name}…");
    }
    let status = crate::process::quiet_command("tar")
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(dest_dir)
        .status()
        .with_context(|| format!("failed to spawn `tar` to extract {}", archive.display()))?;
    if !status.success() {
        return Err(anyhow!(
            "tar exited with {status} extracting {}",
            archive.display()
        ));
    }
    if !bin_path.exists() {
        return Err(anyhow!(
            "extracted archive does not contain `{bin_name}` at {}",
            bin_path.display()
        ));
    }
    Ok(bin_path)
}

fn human_bytes(n: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    if n >= GB {
        format!("{:.1} GB", n as f64 / GB as f64)
    } else if n >= MB {
        format!("{:.1} MB", n as f64 / MB as f64)
    } else if n >= KB {
        format!("{:.1} KB", n as f64 / KB as f64)
    } else {
        format!("{n} B")
    }
}

fn expected_sha_for(sha_text: &str, asset_name: &str) -> Option<String> {
    // Lines look like: "<hex>  <filename>" or "<hex> *<filename>". The
    // filename column may also be a relative path — Windows release builds
    // (≤0.1.13) ran `sha256sum dist-bin/<name>.zip` from the repo root and
    // baked `dist-bin/` into the recorded name. Match by basename so we
    // tolerate that without re-uploading old releases.
    let target = basename(asset_name);
    for line in sha_text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let Some(hash) = parts.next() else { continue };
        let Some(name) = parts.next() else { continue };
        let name = name.trim_start_matches('*');
        if basename(name) == target {
            return Some(hash.to_string());
        }
    }
    // Single-asset .sha256 file: just the hash.
    let stripped = sha_text.trim();
    if stripped.len() == 64 && stripped.chars().all(|c| c.is_ascii_hexdigit()) {
        return Some(stripped.to_string());
    }
    None
}

fn basename(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

// ---------------------------------------------------------------------------
// Atomic file replacement.
// ---------------------------------------------------------------------------

fn atomic_replace(staged: &Path, target: &Path) -> Result<()> {
    // On Unix, rename(2) is atomic when src and dst are on the same filesystem.
    // ~/.myownllm is in $HOME, target is wherever the binary lives — they are very
    // likely on the same FS, but not guaranteed. We try rename first, then fall
    // back to copy + rename via a sibling temp of the target.
    let target_dir = target
        .parent()
        .ok_or_else(|| anyhow!("target has no parent"))?;
    let tmp = target_dir.join(format!(".myownllm-update-{}.tmp", std::process::id()));
    if std::fs::copy(staged, &tmp).is_err() {
        // If copy itself failed (read-only FS, permissions), bubble up.
        return Err(anyhow!(
            "cannot copy staged binary into target dir {target_dir:?}"
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&tmp)?.permissions();
        perms.set_mode(0o755);
        let _ = std::fs::set_permissions(&tmp, perms);
    }
    // Atomic rename over the running binary. Unix allows replacing a running
    // executable; the running process keeps the old inode until exit.
    // Windows blocks rename of an open .exe, so we use the side-rename trick:
    // move the running binary to a sibling `.old` name (which Windows DOES
    // allow even while the file is mapped), then rename the new binary into
    // its place. The `.old` file is cleaned up on the next launch by
    // `cleanup_old_replaced_binary`.
    //
    // We avoid `MoveFileExW(MOVEFILE_DELAY_UNTIL_REBOOT)` deliberately: it
    // requires admin to write the pending entry to HKLM and only takes effect
    // on a full OS reboot, not when the user restarts MyOwnLLM. That mismatch
    // was why "Restart to apply" looked like it worked but never actually
    // changed the version on Windows.
    #[cfg(unix)]
    {
        std::fs::rename(&tmp, target)?;
        Ok(())
    }
    #[cfg(windows)]
    {
        match std::fs::rename(&tmp, target) {
            Ok(()) => Ok(()),
            Err(_) => rename_into_place_via_side_swap_windows(&tmp, target),
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        std::fs::rename(&tmp, target)?;
        Ok(())
    }
}

#[cfg(windows)]
fn rename_into_place_via_side_swap_windows(src: &Path, dst: &Path) -> Result<()> {
    let old = old_binary_path(dst);
    // A leftover .old from a previous swap would block this rename; remove it
    // first. If the old binary is still being held by another running process
    // the remove will fail silently and the rename below will surface the
    // real error.
    if old.exists() {
        let _ = std::fs::remove_file(&old);
    }
    std::fs::rename(dst, &old)
        .with_context(|| format!("could not rename running binary aside to {}", old.display()))?;
    if let Err(e) = std::fs::rename(src, dst) {
        // Roll back: put the original binary back so we don't leave the
        // install in a half-broken state where `dst` doesn't exist.
        let _ = std::fs::rename(&old, dst);
        return Err(anyhow!(
            "swap-in failed after side-rename ({e}); restored original binary"
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn old_binary_path(target: &Path) -> PathBuf {
    let mut name = target
        .file_name()
        .map(|s| s.to_owned())
        .unwrap_or_else(|| std::ffi::OsString::from("myownllm"));
    name.push(".old");
    target.with_file_name(name)
}

/// Delete the `<exe>.old` file left behind by a previous Windows side-swap.
/// Cheap and idempotent — runs once at startup. The file is the previous
/// version's binary; once we're running, nothing keeps it open and it can be
/// freely deleted.
fn cleanup_old_replaced_binary() {
    #[cfg(windows)]
    if let Ok(exe) = std::env::current_exe() {
        let old = old_binary_path(&exe);
        if old.exists() {
            let _ = std::fs::remove_file(&old);
        }
    }
}

// ---------------------------------------------------------------------------
// Check interval gating.
// ---------------------------------------------------------------------------

fn check_marker_path() -> Result<PathBuf> {
    Ok(crate::myownllm_dir()?.join("cache/last-update-check"))
}

fn is_due(interval_hours: f64) -> Result<bool> {
    let path = check_marker_path()?;
    if !path.exists() {
        return Ok(true);
    }
    let s = std::fs::read_to_string(&path).unwrap_or_default();
    let prev = s.trim().parse::<i64>().unwrap_or(0);
    let now = unix_secs();
    let elapsed_h = (now - prev) as f64 / 3600.0;
    Ok(elapsed_h >= interval_hours)
}

fn stamp_check_now() -> Result<()> {
    let path = check_marker_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, format!("{}\n", unix_secs()))?;
    Ok(())
}

fn unix_secs() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn iso_now() -> String {
    let secs = unix_secs();
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
// CLI surface (`myownllm update [check|status|apply]`).
// ---------------------------------------------------------------------------

pub async fn cmd_update(args: &[String]) -> Result<()> {
    match args.first().map(|s| s.as_str()) {
        // No args: do everything in one go — status, check, download,
        // verify, extract, apply. This is the path users should hit.
        None => run_update_now().await,

        // Escape hatches kept for scripts and the "I just want to look"
        // case. They are intentionally undocumented in the main `--help`.
        Some("status") => print_status().await,
        Some("check") => force_check().await,
        Some("apply") => {
            apply_pending()?;
            println!("Applied (or no pending update). The next process start runs the new binary.");
            Ok(())
        }
        Some("enable") => {
            set_enabled(true)?;
            println!("auto_update.enabled = true (written to ~/.myownllm/config.json)");
            Ok(())
        }
        Some("disable") => {
            set_enabled(false)?;
            println!("auto_update.enabled = false (written to ~/.myownllm/config.json)");
            println!("Background self-update checks will not run. Use `myownllm update enable` to re-enable.");
            Ok(())
        }
        Some(unknown) => Err(anyhow!("unknown update subcommand: {unknown}")),
    }
}

/// `myownllm update` (no args). The single source-of-truth command: prints
/// status, checks GitHub, downloads, verifies, extracts, and applies in
/// one shot. Ignores `auto_apply` — a user invoking this is consenting
/// to the upgrade right now.
async fn run_update_now() -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    let kind = detect_install_kind();

    println!(
        "Current : myownllm {current} ({})",
        match kind {
            InstallKind::Raw => "raw install",
            InstallKind::PackageManager => "package-manager install",
        }
    );

    if kind == InstallKind::PackageManager {
        println!(
            "Self-update is disabled here. Use your package manager (e.g. `brew upgrade myownllm`)."
        );
        return Ok(());
    }

    let cfg = crate::resolver::load_config_value()?;
    let au = &cfg["auto_update"];
    if !au["enabled"].as_bool().unwrap_or(true) {
        println!("Self-update is disabled in ~/.myownllm/config.json (auto_update.enabled=false).");
        return Ok(());
    }
    if std::env::var("MYOWNLLM_AUTOUPDATE")
        .map(|v| v == "0" || v.eq_ignore_ascii_case("false"))
        .unwrap_or(false)
    {
        println!("Self-update is disabled via MYOWNLLM_AUTOUPDATE=0.");
        return Ok(());
    }

    // Carry over any previously-staged update before we go look for a newer
    // one. This handles the watcher-staged-but-never-applied case.
    let pending_path = crate::myownllm_dir()?.join("updates/pending.json");
    if pending_path.exists() {
        match serde_json::from_str::<Value>(&std::fs::read_to_string(&pending_path)?) {
            Ok(v) => {
                let pv = v["version"].as_str().unwrap_or("?").to_string();
                if pv != current {
                    println!("Pending : {pv} already staged → applying first…");
                    if let Err(e) = apply_pending() {
                        eprintln!("warning: could not apply staged {pv}: {e}");
                    }
                } else {
                    let _ = std::fs::remove_file(&pending_path);
                }
            }
            Err(_) => {
                let _ = std::fs::remove_file(&pending_path);
            }
        }
    }

    let channel = au["channel"].as_str().unwrap_or("stable");
    println!("Checking GitHub releases ({channel})…");
    let release = fetch_release(channel).await?;
    let latest = release["tag_name"]
        .as_str()
        .map(|s| s.trim_start_matches('v').to_string())
        .ok_or_else(|| anyhow!("release missing tag_name"))?;

    let cmp = compare_semver(current, &latest);
    if cmp != std::cmp::Ordering::Less {
        if cmp == std::cmp::Ordering::Equal {
            println!("Already on the latest version ({latest}).");
        } else {
            println!("Already up to date — you're on {current} (latest published: {latest}).");
        }
        stamp_check_now()?;
        return Ok(());
    }

    println!("Update available: {current} → {latest}");

    let staged_binary = download_verify_extract(&release, &latest, /*verbose=*/ true).await?;

    println!("Applying…");
    let current_exe = std::env::current_exe().context("locating current_exe")?;
    atomic_replace(&staged_binary, &current_exe)?;
    let _ = std::fs::remove_file(&pending_path);
    stamp_check_now()?;

    println!("Updated to {latest}. Relaunch myownllm to use the new version.");
    Ok(())
}

async fn print_status() -> Result<()> {
    let kind = detect_install_kind();
    let s = status()?;
    println!("Current version : {}", s.current_version);
    println!(
        "Install kind    : {}",
        match kind {
            InstallKind::Raw => "raw (self-update eligible)",
            InstallKind::PackageManager => "package-manager (self-update disabled)",
        }
    );
    println!(
        "Auto-update     : {}",
        if s.enabled { "enabled" } else { "disabled" }
    );
    println!("Channel         : {}", s.channel);
    println!(
        "Release feed    : {}{}",
        s.release_url,
        if s.release_url_overridden {
            " (custom)"
        } else {
            ""
        }
    );
    match s.pending {
        Some(p) => println!("Pending         : {} staged at {}", p.version, p.staged_at),
        None => println!("Pending         : none"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::cmp::Ordering;

    #[test]
    fn resolve_release_url_falls_back_to_build_default_when_config_empty() {
        let au = json!({});
        assert_eq!(
            resolve_release_url(&au, "stable"),
            default_release_api_stable()
        );
        assert_eq!(resolve_release_url(&au, "beta"), default_release_api_beta());
    }

    #[test]
    fn resolve_release_url_honors_config_override() {
        let au = json!({
            "stable_url": "https://mirror.example/releases/latest",
            "beta_url":   "https://mirror.example/releases",
        });
        assert_eq!(
            resolve_release_url(&au, "stable"),
            "https://mirror.example/releases/latest"
        );
        assert_eq!(
            resolve_release_url(&au, "beta"),
            "https://mirror.example/releases"
        );
    }

    #[test]
    fn resolve_release_url_treats_unknown_channel_as_stable() {
        let au = json!({ "stable_url": "https://mirror.example/x" });
        assert_eq!(
            resolve_release_url(&au, "weekly-experimental"),
            "https://mirror.example/x"
        );
    }

    #[test]
    fn resolve_release_url_ignores_empty_string_override() {
        // A blank override in config.json (`"stable_url": ""`) is a
        // misconfiguration, not an intent to break updates — fall through to
        // the compile-time default rather than firing a request at "".
        let au = json!({ "stable_url": "" });
        assert_eq!(
            resolve_release_url(&au, "stable"),
            default_release_api_stable()
        );
    }

    #[test]
    fn compare_semver_orders_versions() {
        assert_eq!(compare_semver("1.2.3", "1.2.3"), Ordering::Equal);
        assert_eq!(compare_semver("1.2.3", "1.2.4"), Ordering::Less);
        assert_eq!(compare_semver("1.3.0", "1.2.9"), Ordering::Greater);
        assert_eq!(compare_semver("2.0.0", "1.99.99"), Ordering::Greater);
    }

    #[test]
    fn compare_semver_strips_prerelease_suffix() {
        assert_eq!(compare_semver("1.2.3-beta.1", "1.2.3"), Ordering::Equal);
        assert_eq!(compare_semver("1.2.3-rc.1", "1.2.4"), Ordering::Less);
    }

    #[test]
    fn compare_semver_treats_missing_components_as_zero() {
        assert_eq!(compare_semver("1", "1.0.0"), Ordering::Equal);
        assert_eq!(compare_semver("1.2", "1.2.0"), Ordering::Equal);
        assert_eq!(compare_semver("", "0.0.0"), Ordering::Equal);
    }

    #[test]
    fn apply_policy_parse_falls_back_to_patch() {
        assert_eq!(ApplyPolicy::parse("patch"), ApplyPolicy::Patch);
        assert_eq!(ApplyPolicy::parse("minor"), ApplyPolicy::Minor);
        assert_eq!(ApplyPolicy::parse("all"), ApplyPolicy::All);
        assert_eq!(ApplyPolicy::parse("none"), ApplyPolicy::None);
        assert_eq!(ApplyPolicy::parse("garbage"), ApplyPolicy::Patch);
        assert_eq!(ApplyPolicy::parse(""), ApplyPolicy::Patch);
    }

    #[test]
    fn policy_none_blocks_everything() {
        assert!(!policy_allows(ApplyPolicy::None, "1.0.0", "1.0.1"));
        assert!(!policy_allows(ApplyPolicy::None, "1.0.0", "2.0.0"));
    }

    #[test]
    fn policy_all_allows_everything() {
        assert!(policy_allows(ApplyPolicy::All, "1.0.0", "2.0.0"));
        assert!(policy_allows(ApplyPolicy::All, "1.0.0", "1.0.1"));
    }

    #[test]
    fn policy_minor_requires_same_major() {
        assert!(policy_allows(ApplyPolicy::Minor, "1.0.0", "1.5.0"));
        assert!(policy_allows(ApplyPolicy::Minor, "1.0.0", "1.0.1"));
        assert!(!policy_allows(ApplyPolicy::Minor, "1.0.0", "2.0.0"));
    }

    #[test]
    fn policy_patch_requires_same_major_and_minor() {
        assert!(policy_allows(ApplyPolicy::Patch, "1.2.0", "1.2.5"));
        assert!(!policy_allows(ApplyPolicy::Patch, "1.2.0", "1.3.0"));
        assert!(!policy_allows(ApplyPolicy::Patch, "1.2.0", "2.0.0"));
    }

    #[test]
    fn expected_sha_finds_entry_in_sums_file() {
        let sums = "\
abc123  myownllm-linux-x86_64.tar.gz
def456 *myownllm-macos-aarch64.tar.gz
";
        assert_eq!(
            expected_sha_for(sums, "myownllm-linux-x86_64.tar.gz"),
            Some("abc123".into())
        );
        assert_eq!(
            expected_sha_for(sums, "myownllm-macos-aarch64.tar.gz"),
            Some("def456".into())
        );
    }

    #[test]
    fn expected_sha_returns_none_for_missing_entry() {
        let sums = "abc123  myownllm-linux-x86_64.tar.gz\n";
        assert_eq!(expected_sha_for(sums, "nope.tar.gz"), None);
    }

    /// Regression: Windows release builds up through 0.1.13 ran
    /// `sha256sum dist-bin/myownllm-windows-x86_64.zip` from the repo root,
    /// so the published .sha256 sidecar reads
    /// `<hash>  dist-bin/myownllm-windows-x86_64.zip`. The GitHub asset is
    /// uploaded as the bare basename, so an exact match against the
    /// recorded filename failed and the GUI/CLI surfaced
    /// "SHA256SUMS does not list an entry for myownllm-windows-x86_64.zip".
    /// Lookup must compare basenames.
    #[test]
    fn expected_sha_matches_when_sums_line_has_path_prefix() {
        let sums = "abc123  dist-bin/myownllm-windows-x86_64.zip\n";
        assert_eq!(
            expected_sha_for(sums, "myownllm-windows-x86_64.zip"),
            Some("abc123".into())
        );
        let with_backslash = "abc123  dist-bin\\myownllm-windows-x86_64.zip\n";
        assert_eq!(
            expected_sha_for(with_backslash, "myownllm-windows-x86_64.zip"),
            Some("abc123".into())
        );
    }

    #[test]
    fn expected_sha_accepts_bare_64_char_hash() {
        let single = "0".repeat(64);
        assert_eq!(expected_sha_for(&single, "any-name"), Some(single.clone()));
        let with_newline = format!("{single}\n");
        assert_eq!(expected_sha_for(&with_newline, "any-name"), Some(single));
    }

    #[test]
    fn expected_sha_rejects_short_bare_hash() {
        assert_eq!(expected_sha_for("abc123", "any-name"), None);
    }

    #[test]
    fn expected_sha_skips_malformed_lines() {
        let sums = "\
malformed-line-with-only-one-token
abc123  myownllm-linux-x86_64.tar.gz
";
        assert_eq!(
            expected_sha_for(sums, "myownllm-linux-x86_64.tar.gz"),
            Some("abc123".into())
        );
    }

    #[test]
    fn expected_sha_skips_blanks_and_comments() {
        let sums = "\
# header

abc123  myownllm-linux-x86_64.tar.gz
";
        assert_eq!(
            expected_sha_for(sums, "myownllm-linux-x86_64.tar.gz"),
            Some("abc123".into())
        );
    }

    #[test]
    fn pick_asset_matches_current_platform() {
        let needle = current_target_triple_hint();
        let other = format!("myownllm-other-{}.tar.gz", "platform");
        let matching = format!("myownllm-{needle}.tar.gz");
        let assets = vec![
            json!({ "name": other }),
            json!({ "name": matching.clone() }),
        ];
        let picked = pick_asset(&assets).expect("expected platform match");
        assert_eq!(picked["name"].as_str(), Some(matching.as_str()));
    }

    #[test]
    fn pick_asset_returns_none_when_no_platform_match() {
        let assets = vec![json!({"name": "myownllm-mystery-platform.tar.gz"})];
        assert!(pick_asset(&assets).is_none());
    }

    /// Regression: 0.1.6 → 0.1.9 self-update on macOS picked the `.sha256`
    /// sidecar as the binary because GitHub returned it before the archive
    /// and `pick_asset` used a naive `.contains(needle)` filter. The hex
    /// checksum then got atomic_replaced over `~/.local/bin/myownllm`, leaving
    /// users with `line 1: <hash>: command not found`.
    #[test]
    fn pick_asset_skips_sha256_sidecar_listed_before_archive() {
        let needle = current_target_triple_hint();
        let archive = format!("myownllm-{needle}.tar.gz");
        let sidecar = format!("{archive}.sha256");
        let assets = vec![
            json!({ "name": sidecar }),
            json!({ "name": archive.clone() }),
        ];
        let picked = pick_asset(&assets).expect("expected archive to be picked");
        assert_eq!(picked["name"].as_str(), Some(archive.as_str()));
    }

    #[test]
    fn pick_asset_skips_signature_sidecars() {
        let needle = current_target_triple_hint();
        let archive = format!("myownllm-{needle}.tar.gz");
        for ext in [".sig", ".asc", ".minisig", ".sha512"] {
            let sidecar = format!("{archive}{ext}");
            let assets = vec![
                json!({ "name": sidecar }),
                json!({ "name": archive.clone() }),
            ];
            let picked = pick_asset(&assets).unwrap_or_else(|| panic!("ext {ext}"));
            assert_eq!(picked["name"].as_str(), Some(archive.as_str()), "ext {ext}");
        }
    }

    #[test]
    fn extract_binary_if_archived_refuses_sha256_sidecar() {
        let dir = tempdir_for_test("myownllm-extract-sidecar");
        let sha = dir.join("myownllm-macos-aarch64.tar.gz.sha256");
        std::fs::write(&sha, b"f737bc0e".repeat(8)).unwrap();
        let err = extract_binary_if_archived(&sha, &dir, false).expect_err("should refuse sidecar");
        assert!(
            err.to_string().contains("sidecar"),
            "unexpected error: {err}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pick_sha_asset_prefers_matching_sidecar_over_others() {
        let assets = vec![
            json!({"name": "myownllm-linux-x86_64.tar.gz"}),
            json!({"name": "myownllm-linux-x86_64.tar.gz.sha256"}),
            json!({"name": "myownllm-macos-aarch64.tar.gz"}),
            json!({"name": "myownllm-macos-aarch64.tar.gz.sha256"}),
        ];
        let picked =
            pick_sha_asset(&assets, "myownllm-linux-x86_64.tar.gz").expect("expected sidecar");
        assert_eq!(picked["name"], "myownllm-linux-x86_64.tar.gz.sha256");
    }

    #[test]
    fn pick_sha_asset_falls_back_to_sha256sums() {
        let assets = vec![
            json!({"name": "myownllm-linux-x86_64.tar.gz"}),
            json!({"name": "SHA256SUMS"}),
        ];
        let picked =
            pick_sha_asset(&assets, "myownllm-linux-x86_64.tar.gz").expect("expected SHA256SUMS");
        assert_eq!(picked["name"], "SHA256SUMS");
    }

    #[test]
    fn pick_sha_asset_returns_none_when_no_match_and_no_sums_file() {
        let assets = vec![
            json!({"name": "myownllm-linux-x86_64.tar.gz"}),
            json!({"name": "myownllm-macos-aarch64.tar.gz.sha256"}),
        ];
        assert!(pick_sha_asset(&assets, "myownllm-linux-x86_64.tar.gz").is_none());
    }

    #[test]
    fn pick_sha_asset_returns_none_when_assets_empty() {
        let assets: Vec<Value> = vec![];
        assert!(pick_sha_asset(&assets, "myownllm-linux-x86_64.tar.gz").is_none());
    }

    #[test]
    fn detect_install_kind_flags_homebrew_paths() {
        assert_eq!(
            detect_install_kind_from_path("/opt/homebrew/bin/myownllm"),
            InstallKind::PackageManager
        );
        assert_eq!(
            detect_install_kind_from_path("/usr/local/Cellar/myownllm/0.1.0/bin/myownllm"),
            InstallKind::PackageManager
        );
        assert_eq!(
            detect_install_kind_from_path("/home/linuxbrew/.linuxbrew/bin/myownllm"),
            InstallKind::PackageManager
        );
    }

    #[test]
    fn detect_install_kind_flags_user_install_as_raw() {
        assert_eq!(
            detect_install_kind_from_path("/home/alice/.local/bin/myownllm"),
            InstallKind::Raw
        );
        assert_eq!(
            detect_install_kind_from_path("/usr/local/bin/myownllm"),
            InstallKind::Raw
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn detect_install_kind_flags_linux_system_paths() {
        assert_eq!(
            detect_install_kind_from_path("/usr/bin/myownllm"),
            InstallKind::PackageManager
        );
        assert_eq!(
            detect_install_kind_from_path("/usr/sbin/myownllm"),
            InstallKind::PackageManager
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn detect_install_kind_flags_windows_package_paths() {
        assert_eq!(
            detect_install_kind_from_path(r"C:\Program Files\MyOwnLLM\myownllm.exe"),
            InstallKind::PackageManager
        );
        assert_eq!(
            detect_install_kind_from_path(r"C:\Program Files (x86)\MyOwnLLM\myownllm.exe"),
            InstallKind::PackageManager
        );
        assert_eq!(
            detect_install_kind_from_path(
                r"C:\ProgramData\chocolatey\lib\myownllm\tools\myownllm.exe"
            ),
            InstallKind::PackageManager
        );
        assert_eq!(
            detect_install_kind_from_path(r"C:\Users\me\scoop\apps\myownllm\current\myownllm.exe"),
            InstallKind::PackageManager
        );
    }

    #[test]
    fn human_bytes_formats_each_scale() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2 * 1024), "2.0 KB");
        assert_eq!(human_bytes(3 * 1024 * 1024 + 1024 * 512), "3.5 MB");
        assert_eq!(human_bytes(2u64 * 1024 * 1024 * 1024), "2.0 GB");
    }

    /// Builds a tiny tar.gz containing a fake `myownllm`/`myownllm.exe`
    /// (whichever name the helper expects on this platform), runs the
    /// extraction helper, and confirms the binary lands at the expected
    /// path. Skipped if `tar` isn't on PATH.
    #[test]
    fn extract_binary_if_archived_pulls_myownllm_out_of_targz() {
        if which::which("tar").is_err() {
            eprintln!("skipping: `tar` not found on PATH");
            return;
        }
        let bin_name = if cfg!(windows) {
            "myownllm.exe"
        } else {
            "myownllm"
        };
        let dir = tempdir_for_test("myownllm-extract-targz");
        let bin_inside = dir.join(bin_name);
        std::fs::write(&bin_inside, b"fake-binary").unwrap();
        let archive = dir.join("myownllm-test-x86_64.tar.gz");
        let status = std::process::Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(&dir)
            .arg(bin_name)
            .status()
            .expect("tar -czf");
        assert!(status.success(), "could not build test archive");
        std::fs::remove_file(&bin_inside).unwrap();

        let extracted = extract_binary_if_archived(&archive, &dir, false).expect("extraction");
        assert_eq!(extracted, dir.join(bin_name));
        assert!(extracted.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn extract_binary_if_archived_passes_raw_binary_through_unchanged() {
        let dir = tempdir_for_test("myownllm-extract-raw");
        let raw = dir.join("myownllm");
        std::fs::write(&raw, b"raw binary").unwrap();
        let out = extract_binary_if_archived(&raw, &dir, false).expect("passthrough");
        assert_eq!(out, raw);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn write_pending(path: &Path, version: &str) {
        std::fs::write(
            path,
            serde_json::json!({
                "version": version,
                "staged_at": "2026-01-01T00:00:00Z",
                "path": "/tmp/myownllm-staged",
            })
            .to_string(),
        )
        .unwrap();
    }

    #[test]
    fn read_pending_returns_none_when_file_missing() {
        let dir = tempdir_for_test("myownllm-pending-missing");
        let p = dir.join("pending.json");
        let got = read_pending_or_clean_at(&p, "0.1.5").expect("ok");
        assert!(got.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_pending_returns_entry_when_strictly_newer() {
        let dir = tempdir_for_test("myownllm-pending-newer");
        let p = dir.join("pending.json");
        write_pending(&p, "0.2.0");
        let got = read_pending_or_clean_at(&p, "0.1.5")
            .expect("ok")
            .expect("some");
        assert_eq!(got.version, "0.2.0");
        assert!(p.exists(), "valid pending must not be deleted");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Regression: the GUI's Updates panel was rendering a stale
    /// "Update staged 0.1.5" banner even after the user had installed
    /// 0.1.14 by other means, because `status()` returned the pending
    /// entry verbatim without comparing it to the running version.
    #[test]
    fn read_pending_clears_stale_entry_when_current_is_ahead() {
        let dir = tempdir_for_test("myownllm-pending-stale");
        let p = dir.join("pending.json");
        write_pending(&p, "0.1.5");
        let got = read_pending_or_clean_at(&p, "0.1.14").expect("ok");
        assert!(got.is_none());
        assert!(!p.exists(), "stale pending must be deleted");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_pending_clears_entry_for_already_applied_version() {
        let dir = tempdir_for_test("myownllm-pending-equal");
        let p = dir.join("pending.json");
        write_pending(&p, "0.1.14");
        let got = read_pending_or_clean_at(&p, "0.1.14").expect("ok");
        assert!(got.is_none());
        assert!(!p.exists(), "already-applied pending must be deleted");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_pending_clears_corrupt_json() {
        let dir = tempdir_for_test("myownllm-pending-corrupt");
        let p = dir.join("pending.json");
        std::fs::write(&p, b"{not valid json").unwrap();
        let got = read_pending_or_clean_at(&p, "0.1.5").expect("ok");
        assert!(got.is_none());
        assert!(!p.exists(), "corrupt pending must be deleted");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn tempdir_for_test(label: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("{label}-{}-{}", std::process::id(), unix_secs()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
