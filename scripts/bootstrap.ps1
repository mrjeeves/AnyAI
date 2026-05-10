# MyOwnLLM dev bootstrap (Windows). Idempotent: re-running is a no-op.
# Run from an elevated PowerShell prompt: `powershell -ExecutionPolicy Bypass -File scripts/bootstrap.ps1`

$ErrorActionPreference = "Stop"

function Have($cmd) { $null -ne (Get-Command $cmd -ErrorAction SilentlyContinue) }
function Log($msg)  { Write-Host "==> $msg" -ForegroundColor Cyan }
function Warn($msg) { Write-Host "!!! $msg" -ForegroundColor Yellow }

if (-not (Have "winget")) {
    Warn "winget not found. Install App Installer from the Microsoft Store and re-run."
    exit 1
}

if (-not (Have "rustup")) {
    Log "Installing rustup…"
    winget install --id Rustlang.Rustup --silent --accept-source-agreements --accept-package-agreements
    $env:Path = "$env:Path;$env:USERPROFILE\.cargo\bin"
}

Log "Installing Rust 1.88.0 toolchain (no-op if present)…"
rustup toolchain install 1.88.0 -c clippy,rustfmt --profile minimal | Out-Null

if (-not (Have "node")) {
    Log "Installing Node.js LTS…"
    winget install --id OpenJS.NodeJS.LTS --silent --accept-source-agreements --accept-package-agreements
}

if (-not (Have "pnpm")) {
    # winget updates the persistent PATH but not the running session's, so a
    # freshly installed Node (and the corepack shim that ships with it) won't
    # be on PATH yet. Refresh from the machine + user envs before probing.
    $env:Path = [Environment]::GetEnvironmentVariable("Path", "Machine") + ";" + [Environment]::GetEnvironmentVariable("Path", "User")

    if (Have "corepack") {
        Log "Enabling pnpm via corepack…"
        corepack enable
        corepack prepare pnpm@latest --activate
    } elseif (Have "npm") {
        # Node 25+ unbundled corepack; older Node may also not ship it. npm
        # is always there, so install pnpm directly.
        Log "Installing pnpm via npm…"
        npm install -g pnpm
    } else {
        Warn "Neither corepack nor npm is on PATH. Open a new terminal (so the post-install PATH refreshes) and re-run scripts/bootstrap.ps1."
        exit 1
    }
}

# WebView2 is required by Tauri on Windows.
$webView2 = Get-ItemProperty -Path "HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}" -ErrorAction SilentlyContinue
if (-not $webView2) {
    Log "Installing Microsoft Edge WebView2 Runtime…"
    winget install --id Microsoft.EdgeWebView2Runtime --silent --accept-source-agreements --accept-package-agreements
}

# cmake is required by whisper-rs's build.rs (it builds whisper.cpp from
# source for local transcription). Visual Studio Build Tools provide the
# C++ toolchain whisper.cpp needs at link time.
if (-not (Have "cmake")) {
    Log "Installing CMake (needed by whisper-rs)…"
    winget install --id Kitware.CMake --silent --accept-source-agreements --accept-package-agreements
    $env:Path = [Environment]::GetEnvironmentVariable("Path", "Machine") + ";" + [Environment]::GetEnvironmentVariable("Path", "User")
}

Log "Installing tauri-cli@^2…"
cargo install tauri-cli --version "^2" --locked

if (-not (Have "just")) {
    Log "Installing just…"
    winget install --id Casey.Just --silent --accept-source-agreements --accept-package-agreements
}

Log "Done. Try: just dev | just build | just run | just serve | just preload text vision"
