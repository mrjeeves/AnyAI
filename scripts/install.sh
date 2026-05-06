#!/usr/bin/env bash
# AnyAI end-user installer.
#
# Tries (in order):
#   1. Download a pre-built release binary from GitHub for the current platform.
#   2. Fall back to building from source via scripts/bootstrap.sh.
#
# Usage:
#   curl -fsSL https://anyai.run/install.sh | sh
#   curl -fsSL https://anyai.run/install.sh | sh -s -- --run
#   ./scripts/install.sh --dry-run

set -euo pipefail

REPO="${ANYAI_REPO:-mrjeeves/AnyAI}"
DRY_RUN=false
RUN_AFTER=false
PREFIX_DIR="${ANYAI_PREFIX:-}"
FORCE_SOURCE=false

for arg in "$@"; do
  case "$arg" in
    --dry-run)     DRY_RUN=true ;;
    --run)         RUN_AFTER=true ;;
    --from-source) FORCE_SOURCE=true ;;
    --prefix=*)    PREFIX_DIR="${arg#*=}" ;;
    *) ;;
  esac
done

log()  { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m!!!\033[0m %s\n' "$*" >&2; }
err()  { printf '\033[1;31mxxx\033[0m %s\n' "$*" >&2; }

OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH_RAW="$(uname -m)"
case "$ARCH_RAW" in
  x86_64|amd64)  ARCH="x86_64" ;;
  aarch64|arm64) ARCH="aarch64" ;;
  *)             ARCH="$ARCH_RAW" ;;
esac
ASSET="anyai-${OS}-${ARCH}.tar.gz"

# Pick install prefix. Prefer /usr/local/bin if writable; else ~/.local/bin.
if [[ -z "$PREFIX_DIR" ]]; then
  if [[ -w /usr/local/bin ]] || sudo -n true 2>/dev/null; then
    PREFIX_DIR="/usr/local/bin"
  else
    PREFIX_DIR="$HOME/.local/bin"
  fi
fi

install_binary() {
  local src="$1"
  mkdir -p "$PREFIX_DIR"
  if [[ -w "$PREFIX_DIR" ]]; then
    install -m 0755 "$src" "$PREFIX_DIR/anyai"
  else
    sudo install -m 0755 "$src" "$PREFIX_DIR/anyai"
  fi
  log "Installed: $PREFIX_DIR/anyai"
}

try_release() {
  if ! command -v curl >/dev/null 2>&1; then
    warn "curl missing; skipping release download."
    return 1
  fi
  local api="https://api.github.com/repos/${REPO}/releases/latest"
  log "Looking up latest release: $api"
  local json
  if ! json="$(curl -fsSL "$api" 2>/dev/null)"; then
    warn "GitHub releases unreachable (or no release yet)."
    return 1
  fi
  local url
  url="$(printf '%s' "$json" | grep -Eo "https://[^\"]+/${ASSET}" | head -n1 || true)"
  if [[ -z "$url" ]]; then
    warn "No release asset matched ${ASSET}."
    return 1
  fi
  local sha_url="${url}.sha256"
  log "Downloading $url"
  $DRY_RUN && { log "(dry-run) would download $url"; return 0; }
  local tmp
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' RETURN
  curl -fsSL "$url" -o "$tmp/$ASSET"
  if curl -fsSL "$sha_url" -o "$tmp/$ASSET.sha256" 2>/dev/null; then
    (cd "$tmp" && sha256sum -c "$ASSET.sha256" 2>/dev/null || shasum -a 256 -c "$ASSET.sha256")
  else
    warn "No SHA256 sidecar; skipping integrity check."
  fi
  tar -xzf "$tmp/$ASSET" -C "$tmp"
  install_binary "$tmp/anyai"
  return 0
}

build_from_source() {
  log "Building from source…"
  if ! command -v git >/dev/null 2>&1; then
    err "git is required to build from source."
    exit 1
  fi
  local repo_dir
  if [[ -f Justfile && -d src-tauri ]]; then
    repo_dir="$(pwd)"
    log "Using current directory as source: $repo_dir"
  else
    repo_dir="$(mktemp -d)/AnyAI"
    log "Cloning into $repo_dir"
    $DRY_RUN || git clone --depth 1 "https://github.com/${REPO}.git" "$repo_dir"
  fi
  $DRY_RUN && { log "(dry-run) would bootstrap and build in $repo_dir"; return 0; }
  ( cd "$repo_dir" && bash scripts/bootstrap.sh )
  ( cd "$repo_dir" && pnpm install --frozen-lockfile && pnpm tauri build )
  local built="$repo_dir/src-tauri/target/release/anyai"
  if [[ ! -x "$built" ]]; then
    err "Build did not produce $built"
    exit 1
  fi
  install_binary "$built"
}

if [[ "$FORCE_SOURCE" == "true" ]] || ! try_release; then
  build_from_source
fi

if [[ "$RUN_AFTER" == "true" ]] && [[ "$DRY_RUN" != "true" ]]; then
  log "Launching anyai run…"
  exec "$PREFIX_DIR/anyai" run
fi

log "Done. Try: anyai run | anyai serve | anyai preload text vision"
