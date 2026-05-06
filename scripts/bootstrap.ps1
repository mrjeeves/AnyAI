# AnyAI dev bootstrap (Windows). Idempotent: re-running is a no-op.
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

Log "Installing Rust 1.77.2 toolchain (no-op if present)…"
rustup toolchain install 1.77.2 -c clippy,rustfmt --profile minimal | Out-Null

if (-not (Have "node")) {
    Log "Installing Node.js LTS…"
    winget install --id OpenJS.NodeJS.LTS --silent --accept-source-agreements --accept-package-agreements
}

if (-not (Have "pnpm")) {
    Log "Enabling pnpm via corepack…"
    corepack enable
    corepack prepare pnpm@latest --activate
}

# WebView2 is required by Tauri on Windows.
$webView2 = Get-ItemProperty -Path "HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}" -ErrorAction SilentlyContinue
if (-not $webView2) {
    Log "Installing Microsoft Edge WebView2 Runtime…"
    winget install --id Microsoft.EdgeWebView2Runtime --silent --accept-source-agreements --accept-package-agreements
}

Log "Installing tauri-cli@^2…"
cargo install tauri-cli --version "^2" --locked

if (-not (Have "just")) {
    Log "Installing just…"
    winget install --id Casey.Just --silent --accept-source-agreements --accept-package-agreements
}

Log "Done. Try: just dev | just build | just run | just serve | just preload text vision"
