#!/bin/sh
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
#
# This script is intentionally POSIX sh-compatible so that `curl … | sh` works
# under dash, ash/busybox sh, and bash alike. Avoid bash-only constructs
# ([[ ]], RETURN traps, ${var^^}, arrays, etc.).

set -eu
# pipefail is supported by bash, ksh, zsh, and dash >= 0.5.10. Enable it when
# the running shell understands it; otherwise carry on without it.
if (set -o pipefail) 2>/dev/null; then
  set -o pipefail
fi

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

OS_RAW="$(uname -s | tr '[:upper:]' '[:lower:]')"
case "$OS_RAW" in
  darwin) OS="macos" ;;
  linux)  OS="linux" ;;
  *)      OS="$OS_RAW" ;;
esac
ARCH_RAW="$(uname -m)"
case "$ARCH_RAW" in
  x86_64|amd64)  ARCH="x86_64" ;;
  aarch64|arm64) ARCH="aarch64" ;;
  *)             ARCH="$ARCH_RAW" ;;
esac
ASSET="anyai-${OS}-${ARCH}.tar.gz"

# Pick install prefix. Prefer /usr/local/bin if writable; else ~/.local/bin.
if [ -z "$PREFIX_DIR" ]; then
  if [ -w /usr/local/bin ] || sudo -n true 2>/dev/null; then
    PREFIX_DIR="/usr/local/bin"
  else
    PREFIX_DIR="$HOME/.local/bin"
  fi
fi

install_binary() {
  src="$1"
  mkdir -p "$PREFIX_DIR"
  if [ -w "$PREFIX_DIR" ]; then
    install -m 0755 "$src" "$PREFIX_DIR/anyai"
  else
    sudo install -m 0755 "$src" "$PREFIX_DIR/anyai"
  fi
  log "Installed: $PREFIX_DIR/anyai"
}

# AnyAI is a Tauri app: every binary, including CLI subcommands, is dynamically
# linked against libwebkit2gtk-4.1.so.0 (Tauri's webview). On a fresh Linux box
# without those system libs, the dynamic loader bails before main() runs and
# the user sees:
#   anyai: error while loading shared libraries: libwebkit2gtk-4.1.so.0: …
# Even `anyai setup` can't recover from that — the binary never executes.
# Install the runtime libs at install time so the first launch just works.
install_linux_runtime_deps() {
  [ "$OS" = "linux" ] || return 0
  [ "$DRY_RUN" = "true" ] && { log "(dry-run) would install Linux runtime deps"; return 0; }

  if command -v apt-get >/dev/null 2>&1; then
    log "Installing Linux runtime libraries (libwebkit2gtk-4.1, libayatana-appindicator3)…"
    pkgs="libwebkit2gtk-4.1-0 libayatana-appindicator3-1 librsvg2-2"
    if [ "$(id -u)" = "0" ]; then
      apt-get update -qq && apt-get install -y --no-install-recommends $pkgs
    elif sudo -n true 2>/dev/null || [ -t 0 ]; then
      sudo apt-get update -qq && sudo apt-get install -y --no-install-recommends $pkgs
    else
      warn "Cannot run sudo non-interactively; skipping runtime-lib install."
      warn "If 'anyai' fails with 'libwebkit2gtk-4.1.so.0: cannot open shared object file', run:"
      warn "  sudo apt-get install -y $pkgs"
    fi
  elif command -v dnf >/dev/null 2>&1; then
    log "Installing Linux runtime libraries via dnf…"
    pkgs="webkit2gtk4.1 libappindicator-gtk3 librsvg2"
    if [ "$(id -u)" = "0" ]; then
      dnf install -y $pkgs
    else
      sudo dnf install -y $pkgs || warn "dnf install failed; install manually: sudo dnf install -y $pkgs"
    fi
  elif command -v pacman >/dev/null 2>&1; then
    log "Installing Linux runtime libraries via pacman…"
    pkgs="webkit2gtk-4.1 libappindicator-gtk3 librsvg"
    if [ "$(id -u)" = "0" ]; then
      pacman -S --noconfirm --needed $pkgs
    else
      sudo pacman -S --noconfirm --needed $pkgs || warn "pacman install failed; install manually: sudo pacman -S $pkgs"
    fi
  else
    warn "Unrecognized Linux distro — cannot auto-install runtime libs."
    warn "If 'anyai' fails with 'libwebkit2gtk-4.1.so.0: cannot open shared object file',"
    warn "install your distro's webkit2gtk-4.1, libayatana-appindicator3, and librsvg2 packages."
  fi
}

ensure_on_path() {
  case ":$PATH:" in
    *":$PREFIX_DIR:"*) return 0 ;;
  esac

  shell_name="$(basename "${SHELL:-bash}")"
  marker="# added by anyai installer"
  case "$shell_name" in
    zsh)
      rc="$HOME/.zshrc"
      line="export PATH=\"$PREFIX_DIR:\$PATH\"  $marker"
      ;;
    fish)
      rc="$HOME/.config/fish/config.fish"
      line="fish_add_path -g $PREFIX_DIR  $marker"
      ;;
    *)
      rc="$HOME/.bashrc"
      line="export PATH=\"$PREFIX_DIR:\$PATH\"  $marker"
      ;;
  esac

  if grep -qsF "$marker" "$rc" 2>/dev/null; then
    warn "$PREFIX_DIR not on current PATH; PATH already added to $rc — open a new terminal."
    return 0
  fi

  mkdir -p "$(dirname "$rc")"
  if printf '\n%s\n' "$line" >> "$rc" 2>/dev/null; then
    log "Added $PREFIX_DIR to PATH in $rc"
    log "Open a new terminal (or run: source $rc) for it to take effect."
  else
    warn "$PREFIX_DIR is not on PATH. Add this to your shell rc:"
    warn "  $line"
  fi
}

# Tracked for cleanup since POSIX sh has no function-scoped RETURN trap.
_TRY_RELEASE_TMP=""
_cleanup_try_release() {
  if [ -n "$_TRY_RELEASE_TMP" ] && [ -d "$_TRY_RELEASE_TMP" ]; then
    rm -rf "$_TRY_RELEASE_TMP"
  fi
  _TRY_RELEASE_TMP=""
}

try_release() {
  if ! command -v curl >/dev/null 2>&1; then
    warn "curl missing; skipping release download."
    return 1
  fi
  api="https://api.github.com/repos/${REPO}/releases/latest"
  log "Looking up latest release: $api"
  if ! json="$(curl -fsSL "$api" 2>/dev/null)"; then
    warn "GitHub releases unreachable (or no release yet)."
    return 1
  fi
  url="$(printf '%s' "$json" | grep -Eo "https://[^\"]+/${ASSET}" | head -n1 || true)"
  if [ -z "$url" ]; then
    warn "No release asset matched ${ASSET}."
    return 1
  fi
  sha_url="${url}.sha256"
  log "Downloading $url"
  if [ "$DRY_RUN" = "true" ]; then
    log "(dry-run) would download $url"
    return 0
  fi
  _TRY_RELEASE_TMP="$(mktemp -d)"
  trap _cleanup_try_release EXIT INT TERM
  curl -fsSL "$url" -o "$_TRY_RELEASE_TMP/$ASSET"
  if curl -fsSL "$sha_url" -o "$_TRY_RELEASE_TMP/$ASSET.sha256" 2>/dev/null; then
    (cd "$_TRY_RELEASE_TMP" && sha256sum -c "$ASSET.sha256" 2>/dev/null || shasum -a 256 -c "$ASSET.sha256")
  else
    warn "No SHA256 sidecar; skipping integrity check."
  fi
  tar -xzf "$_TRY_RELEASE_TMP/$ASSET" -C "$_TRY_RELEASE_TMP"
  install_binary "$_TRY_RELEASE_TMP/anyai"
  _cleanup_try_release
  trap - EXIT INT TERM
  return 0
}

build_from_source() {
  log "Building from source…"
  if ! command -v git >/dev/null 2>&1; then
    err "git is required to build from source."
    exit 1
  fi
  if [ -f Justfile ] && [ -d src-tauri ]; then
    repo_dir="$(pwd)"
    log "Using current directory as source: $repo_dir"
  else
    repo_dir="$(mktemp -d)/AnyAI"
    log "Cloning into $repo_dir"
    if [ "$DRY_RUN" != "true" ]; then
      git clone --depth 1 "https://github.com/${REPO}.git" "$repo_dir"
    fi
  fi
  if [ "$DRY_RUN" = "true" ]; then
    log "(dry-run) would bootstrap and build in $repo_dir"
    return 0
  fi
  ( cd "$repo_dir" && bash scripts/bootstrap.sh )
  ( cd "$repo_dir" && pnpm install --frozen-lockfile && pnpm tauri build )
  built="$repo_dir/src-tauri/target/release/anyai"
  if [ ! -x "$built" ]; then
    err "Build did not produce $built"
    exit 1
  fi
  install_binary "$built"
}

if [ "$FORCE_SOURCE" = "true" ] || ! try_release; then
  build_from_source
fi

# Install runtime libs after the binary is in place. Doing it here (rather than
# inside try_release / build_from_source) means we run it once even if we fall
# back from a release download to a source build.
install_linux_runtime_deps

if [ "$DRY_RUN" != "true" ]; then
  ensure_on_path
fi

if [ "$RUN_AFTER" = "true" ] && [ "$DRY_RUN" != "true" ]; then
  log "Launching anyai run…"
  exec "$PREFIX_DIR/anyai" run
fi

log "Done. Try: anyai run | anyai serve | anyai preload text vision"
