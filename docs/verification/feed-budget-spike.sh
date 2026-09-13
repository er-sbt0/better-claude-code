#!/usr/bin/env bash
# Re-measure the WebKitGTK budget for §7's feed (§4, §2's first item).
#
#   bash docs/verification/feed-budget-spike.sh
#   bash docs/verification/feed-budget-spike.sh 1 3 6 12      # column counts
#   BLOCKS=10000 bash docs/verification/feed-budget-spike.sh
#
# Builds the app the way it ships (`BENCH=1 pnpm build` adds `bench.html` to the
# inputs and changes nothing else), serves `dist/` over http, and opens each
# configuration in a real WebKitGTK window. What it prints is JSON, one object
# per configuration, and what those numbers mean is
# `docs/verification/webkitgtk-feed-budget.md`.
#
# A display is required — this measures compositing, so there is nothing to
# learn from a headless run. Under Wayland it goes through XWayland like the app
# does.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

COLUMNS=("$@")
[ ${#COLUMNS[@]} -eq 0 ] && COLUMNS=(1 3 6 12)
BLOCKS="${BLOCKS:-5000}"
PORT="${PORT:-1421}"

if [ -z "${DISPLAY:-}" ] && [ -z "${WAYLAND_DISPLAY:-}" ]; then
  echo "no display — this measures compositing, so a headless run measures nothing" >&2
  exit 1
fi

python3 -c 'import gi; gi.require_version("WebKit2", "4.1")' 2>/dev/null || {
  echo "missing the WebKitGTK GObject bindings:" >&2
  echo "  sudo apt install -y python3-gi gir1.2-webkit2-4.1" >&2
  exit 1
}

echo "# building (BENCH=1, so bench.html is an input)" >&2
BENCH=1 pnpm --filter @agent-workspace/desktop build >/dev/null

cd apps/desktop/dist
python3 -m http.server "$PORT" --bind 127.0.0.1 >/dev/null 2>&1 &
server=$!
trap 'kill $server 2>/dev/null || true' EXIT
sleep 1

urls=()
for n in "${COLUMNS[@]}"; do
  urls+=("http://127.0.0.1:$PORT/bench.html?columns=$n&blocks=$BLOCKS")
done

cd "$ROOT"
python3 docs/verification/feed-budget-spike.py "${urls[@]}"
