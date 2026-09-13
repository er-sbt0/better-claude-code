#!/usr/bin/env bash
# Launch the desktop app in development (docs/development.md, "Running it").
#
#   ./run.sh                 # preflight, then vite on :1420 + the Tauri window
#   ./run.sh --release       # same, with an optimized Rust build
#   ./run.sh --check         # preflight only, exit non-zero if anything is missing
#
# Anything else on the command line is passed through to `tauri dev`.
#
# The preflight exists because the interesting failures here are not code
# failures: a missing system library surfaces as a pkg-config error part-way
# through `cargo build`, one library at a time, several minutes in. Checking
# them up front turns that into one apt line.
#
# Env overrides:
#   SKIP_PREFLIGHT=1   go straight to `tauri dev`
#   SKIP_INSTALL=1     never run `pnpm install`, even with no node_modules

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT"

CHECK_ONLY=0
TAURI_ARGS=()
for arg in "$@"; do
  case "$arg" in
    --check) CHECK_ONLY=1 ;;
    -h|--help) sed -n '2,12p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) TAURI_ARGS+=("$arg") ;;
  esac
done

# ------------------------------------------------------------- preflight ----
missing_tools=()
missing_apt=()

need_bin() {  # need_bin <binary> <how to get it>
  command -v "$1" >/dev/null 2>&1 || missing_tools+=("$1 — $2")
}

need_pkg() {  # need_pkg <pkg-config module> <apt package>
  pkg-config --exists "$1" 2>/dev/null || missing_apt+=("$2")
}

need_header() {  # need_header <header path> <apt package>
  # libxdo-dev ships no .pc file, so its header is the only thing to look for.
  [ -e "$1" ] || missing_apt+=("$2")
}

preflight() {
  need_bin cargo "https://rustup.rs"
  need_bin node "Node 22 (CI pins 22)"
  need_bin pnpm "corepack enable pnpm"
  need_bin pkg-config "sudo apt install pkg-config"
  need_bin cc "sudo apt install build-essential"

  if command -v pkg-config >/dev/null 2>&1; then
    # Tauri ships no webview; on Linux it borrows the system's, so every one of
    # these is a build dependency of the app itself (docs/development.md).
    need_pkg webkit2gtk-4.1            libwebkit2gtk-4.1-dev
    need_pkg gtk+-3.0                  libgtk-3-dev
    need_pkg dbus-1                    libdbus-1-dev
    need_pkg ayatana-appindicator3-0.1 libayatana-appindicator3-dev
    need_pkg librsvg-2.0               librsvg2-dev
    need_pkg openssl                   libssl-dev
    need_header /usr/include/xdo.h     libxdo-dev
  fi
  # Only the bundler needs patchelf, so a missing one is a warning, not a stop.
  command -v patchelf >/dev/null 2>&1 || echo "note: patchelf not found — 'pnpm build' will fail, 'pnpm dev' is fine" >&2

  local rust_min
  rust_min="$(sed -n 's/^rust-version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
  if command -v cargo >/dev/null 2>&1 && [ -n "$rust_min" ]; then
    local rust_have
    rust_have="$(cargo --version | awk '{print $2}')"
    # Sort the two and see which comes first: no semver comparison in bash.
    if [ "$(printf '%s\n%s\n' "$rust_min" "$rust_have" | sort -V | head -1)" != "$rust_min" ]; then
      missing_tools+=("rust >= $rust_min (have $rust_have) — rustup update")
    fi
  fi

  if [ ${#missing_apt[@]} -gt 0 ]; then
    echo "missing system libraries:" >&2
    printf '  sudo apt install -y %s\n' "${missing_apt[*]}" >&2
  fi
  if [ ${#missing_tools[@]} -gt 0 ]; then
    echo "missing tools:" >&2
    printf '  %s\n' "${missing_tools[@]}" >&2
  fi
  [ ${#missing_apt[@]} -eq 0 ] && [ ${#missing_tools[@]} -eq 0 ]
}

if [ "${SKIP_PREFLIGHT:-0}" != "1" ]; then
  if preflight; then
    echo "preflight ok — rust $(cargo --version | awk '{print $2}'), node $(node --version), pnpm $(pnpm --version)"
  else
    echo "see docs/development.md for what each of these is for" >&2
    exit 1
  fi
fi

if [ "$CHECK_ONLY" = "1" ]; then
  exit 0
fi

# --------------------------------------------------------------- run it -----
if [ ! -d node_modules ] || [ ! -d apps/desktop/node_modules ]; then
  if [ "${SKIP_INSTALL:-0}" = "1" ]; then
    echo "no node_modules and SKIP_INSTALL=1 — run 'pnpm install' first" >&2
    exit 1
  fi
  echo "installing workspace dependencies..."
  pnpm install
fi

# `tauri dev` starts vite on :1420 itself (beforeDevCommand) and opens the
# window once it answers. State lands in
# ~/.local/share/dev.agent-workspace.desktop/log.sqlite — delete it for a reset.
exec pnpm --filter @agent-workspace/desktop tauri dev "${TAURI_ARGS[@]}"
