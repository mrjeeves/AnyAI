# AnyAI uninstaller (Windows).
#
# Removes every artifact the AnyAI installer + AnyAI runtime put on disk:
#   - %LOCALAPPDATA%\Programs\AnyAI\ (contains anyai.exe)
#   - %USERPROFILE%\.anyai (config, conversations, whisper models, transcribe
#                           buffers, staged self-updates, watcher.lock, etc.)
#   - Tauri app data under the old `run.anyai.app` bundle identifier
#     (%APPDATA%\run.anyai.app and %LOCALAPPDATA%\run.anyai.app)
#   - the AnyAI entry from the user PATH environment variable
#   - any leftover anyai-install-* / AnyAI-* directories under %TEMP%
#
# Pre-rename project; safe to run on test machines before installing the
# renamed `myownllm` build. Will be deleted from the repo once test machines
# are clean.
#
# Usage:
#   .\uninstall.ps1
#   .\uninstall.ps1 -DryRun

[CmdletBinding()]
param(
    [switch]$DryRun
)

$ErrorActionPreference = "Stop"

function Log($msg)  { Write-Host "==> $msg" -ForegroundColor Cyan }
function Warn($msg) { Write-Host "!!! $msg" -ForegroundColor Yellow }

if ($DryRun) { Log "(dry-run mode - nothing will be deleted)" }

# Stop any running anyai process so file removal succeeds.
$running = Get-Process -Name anyai -ErrorAction SilentlyContinue
if ($running) {
    Log "Stopping running anyai process(es)..."
    if (-not $DryRun) {
        $running | Stop-Process -Force -ErrorAction SilentlyContinue
        Start-Sleep -Seconds 1
    }
}

function Remove-IfExists([string]$path) {
    if (-not (Test-Path -LiteralPath $path)) { return }
    if ($DryRun) {
        Log "would remove: $path"
        return
    }
    try {
        Remove-Item -LiteralPath $path -Recurse -Force -ErrorAction Stop
        Log "removed: $path"
    } catch {
        Warn "could not remove $path : $($_.Exception.Message)"
    }
}

# Glob-style matches under a parent dir; -Filter uses a wildcard pattern.
function Remove-Matching([string]$parent, [string]$pattern) {
    if (-not (Test-Path -LiteralPath $parent)) { return }
    Get-ChildItem -LiteralPath $parent -Filter $pattern -Force -ErrorAction SilentlyContinue |
        ForEach-Object { Remove-IfExists $_.FullName }
}

# 1) Install prefix (entire folder; the installer creates it fresh).
Remove-IfExists (Join-Path $env:LOCALAPPDATA "Programs\AnyAI")

# 2) Main app data dir.
Remove-IfExists (Join-Path $env:USERPROFILE ".anyai")

# 3) Tauri app data — bundle identifier was `run.anyai.app`.
Remove-IfExists (Join-Path $env:APPDATA "run.anyai.app")
Remove-IfExists (Join-Path $env:LOCALAPPDATA "run.anyai.app")

# 4) Stray installer/source-build temp directories under %TEMP%.
Remove-Matching $env:TEMP "anyai-install-*"
Remove-Matching $env:TEMP "AnyAI-*"

# 5) Strip the AnyAI install dir from the user PATH (if the installer added it).
$anyaiPrefix = Join-Path $env:LOCALAPPDATA "Programs\AnyAI"
$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
if ($userPath) {
    $entries = $userPath -split ";"
    $kept = $entries | Where-Object {
        $_ -and ($_.TrimEnd('\') -ine $anyaiPrefix.TrimEnd('\'))
    }
    $newPath = ($kept -join ";")
    if ($newPath -ne $userPath) {
        if ($DryRun) {
            Log "would remove $anyaiPrefix from user PATH"
        } else {
            [Environment]::SetEnvironmentVariable("Path", $newPath, "User")
            Log "removed $anyaiPrefix from user PATH"
        }
    }
}

Log "Done."
if ($DryRun) {
    Log "Dry-run complete. Re-run without -DryRun to actually remove."
} else {
    Log "AnyAI artifacts removed. Open a new terminal so PATH changes take effect."
}
