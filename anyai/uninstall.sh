#!/bin/sh
# AnyAI uninstaller for macOS and Linux.
#
# Removes every artifact the AnyAI installer + AnyAI runtime put on disk:
#   - the `anyai` binary in /usr/local/bin or ~/.local/bin
#   - ~/.anyai (config, conversations, whisper models, transcribe buffers,
#                staged self-updates, watcher.lock, etc.)
#   - Tauri app data under the old `run.anyai.app` bundle identifier
#   - the `# added by anyai installer` PATH line in shell rc files
#   - any leftover anyai self-update temp/staging directories
#
# Pre-rename project; safe to run on test machines before installing the
# renamed `myownllm` build. Will be deleted from the repo once test machines
# are clean.
#
# Usage:
#   ./uninstall.sh            # actually remove
#   ./uninstall.sh --dry-run  # just print what would be removed

set -u

DRY_RUN=false
for arg in "$@"; do
  case "$arg" in
    --dry-run|-n) DRY_RUN=true ;;
    -h|--help)
      sed -n '2,18p' "$0"
      exit 0
      ;;
    *) ;;
  esac
done

log()  { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m!!!\033[0m %s\n' "$*" >&2; }

OS_RAW="$(uname -s | tr '[:upper:]' '[:lower:]')"
case "$OS_RAW" in
  darwin) OS="macos" ;;
  linux)  OS="linux" ;;
  *)
    warn "Unsupported OS: $OS_RAW (expected darwin or linux)"
    exit 1
    ;;
esac
log "Detected OS: $OS"
[ "$DRY_RUN" = "true" ] && log "(dry-run mode — nothing will be deleted)"

# Stop any running anyai processes so file removal succeeds. Best-effort.
stop_running() {
  if command -v pgrep >/dev/null 2>&1 && pgrep -x anyai >/dev/null 2>&1; then
    log "Stopping running anyai process(es)…"
    if [ "$DRY_RUN" != "true" ]; then
      pkill -x anyai 2>/dev/null || true
      sleep 1
      pkill -9 -x anyai 2>/dev/null || true
    fi
  fi
}

# Remove a path if it exists. Uses sudo only when the path isn't writable by
# the current user (the binary in /usr/local/bin is the typical case).
remove_path() {
  p="$1"
  if [ ! -e "$p" ] && [ ! -L "$p" ]; then
    return 0
  fi
  if [ "$DRY_RUN" = "true" ]; then
    log "would remove: $p"
    return 0
  fi
  parent="$(dirname "$p")"
  if [ -w "$parent" ]; then
    rm -rf -- "$p"
    log "removed: $p"
  else
    if sudo -n true 2>/dev/null || [ -t 0 ]; then
      sudo rm -rf -- "$p"
      log "removed (sudo): $p"
    else
      warn "Cannot remove $p — parent not writable and sudo unavailable."
    fi
  fi
}

# Strip the installer-added PATH line from a shell rc file. The installer
# tagged the line with `# added by anyai installer`, so remove only matching
# lines and leave everything else alone.
clean_rc() {
  rc="$1"
  [ -f "$rc" ] || return 0
  if ! grep -qF "# added by anyai installer" "$rc" 2>/dev/null; then
    return 0
  fi
  if [ "$DRY_RUN" = "true" ]; then
    log "would clean PATH line in: $rc"
    return 0
  fi
  tmp="$(mktemp)"
  # Drop any line containing the installer marker.
  grep -vF "# added by anyai installer" "$rc" > "$tmp" || true
  # Preserve permissions.
  cat "$tmp" > "$rc"
  rm -f "$tmp"
  log "cleaned PATH line in: $rc"
}

stop_running

# 1) Binaries.
remove_path "/usr/local/bin/anyai"
remove_path "$HOME/.local/bin/anyai"

# 2) Main app data dir.
remove_path "$HOME/.anyai"

# 3) Tauri app data — bundle identifier was `run.anyai.app`.
if [ "$OS" = "macos" ]; then
  remove_path "$HOME/Library/Application Support/run.anyai.app"
  remove_path "$HOME/Library/Caches/run.anyai.app"
  remove_path "$HOME/Library/Logs/run.anyai.app"
  remove_path "$HOME/Library/Preferences/run.anyai.app.plist"
  remove_path "$HOME/Library/WebKit/run.anyai.app"
  remove_path "$HOME/Library/Saved Application State/run.anyai.app.savedState"
  # HTTPStorages directories are suffix-versioned; glob and remove each match.
  for d in "$HOME/Library/HTTPStorages/run.anyai.app"*; do
    [ -e "$d" ] && remove_path "$d"
  done
else
  remove_path "$HOME/.config/run.anyai.app"
  remove_path "$HOME/.local/share/run.anyai.app"
  remove_path "$HOME/.cache/run.anyai.app"
fi

# 4) Shell rc PATH cleanup.
clean_rc "$HOME/.bashrc"
clean_rc "$HOME/.zshrc"
clean_rc "$HOME/.profile"
clean_rc "$HOME/.bash_profile"
clean_rc "$HOME/.config/fish/config.fish"

# 5) Stray installer/source-build temp directories. The installer cleans its
#    own tempdir on success, but failed runs sometimes leave them behind.
patterns="/tmp/anyai-install-* /tmp/AnyAI-*"
if [ -n "${TMPDIR:-}" ] && [ "${TMPDIR%/}" != "/tmp" ]; then
  patterns="$patterns ${TMPDIR%/}/anyai-install-* ${TMPDIR%/}/AnyAI-*"
fi
for d in $patterns; do
  case "$d" in
    *'*'*) continue ;;  # no glob match — pattern returned literal
  esac
  [ -e "$d" ] && remove_path "$d"
done

log "Done."
if [ "$DRY_RUN" = "true" ]; then
  log "Dry-run complete. Re-run without --dry-run to actually remove."
else
  log "AnyAI artifacts removed. Open a new terminal so PATH changes take effect."
fi
