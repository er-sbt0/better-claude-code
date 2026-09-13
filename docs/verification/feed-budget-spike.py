#!/usr/bin/env python3
"""Drive `bench.html` in a WebKitGTK window and print what it measured.

§4 says the performance budget is enforced "on WebKitGTK, not on the
developer's fastest platform", so a measurement taken in Chrome answers a
question nobody asked. This opens a real `WebKitWebView` — the same
`webkit2gtk-4.1` library Tauri borrows on Linux (`docs/development.md`) — loads
each URL in turn, and prints the JSON the page hands back through a script
message handler.

Not Tauri itself, and the difference is worth stating: the page under test uses
no `invoke` and no event channel, so it needs no host. What it shares with the
app is everything that renders — the engine, the bundle, React, the canvas, the
columns and the feed. What it does not measure is the IPC underneath; that is
`cargo run -p document-model --example project_cost`.

Usage:
    python3 feed-budget-spike.py <url> [<url> ...]
"""

import json
import sys

import gi

gi.require_version("Gtk", "3.0")
gi.require_version("WebKit2", "4.1")
from gi.repository import GLib, Gtk, WebKit2  # noqa: E402

# The window the design ships (`tauri.conf.json`). Frame cost scales with the
# pixels being composited, so measuring in a different window measures a
# different product.
WIDTH, HEIGHT = 1400, 900

# A run that never reports is a broken page, not a slow one, and it must not
# hang a script somebody is waiting on.
TIMEOUT_S = 180


def main(urls):
    results = []
    pending = list(urls)

    window = Gtk.Window(title="feed budget")
    window.set_default_size(WIDTH, HEIGHT)

    manager = WebKit2.UserContentManager()
    view = WebKit2.WebView.new_with_user_content_manager(manager)

    settings = view.get_settings()
    # Exactly what a Tauri window gets: accelerated compositing on, developer
    # extras off. Turning either the other way would measure a different engine
    # configuration from the one the app ships.
    settings.set_enable_developer_extras(False)
    settings.set_hardware_acceleration_policy(WebKit2.HardwareAccelerationPolicy.ON_DEMAND)

    window.add(view)
    window.show_all()
    # Kept above and presented, and this is not cosmetic: WebKit throttles
    # `requestAnimationFrame` in a window it believes nobody is looking at, and
    # a throttled run reports a frame time that is the compositor's patience
    # rather than the page's cost. The first matrix run of this spike lost a
    # configuration to exactly that, and the page reports `hidden` so the
    # symptom is visible in the output rather than inferred from an outlier.
    window.set_keep_above(True)
    window.present()

    def next_url():
        if not pending:
            Gtk.main_quit()
            return
        url = pending.pop(0)
        print(f"# loading {url}", file=sys.stderr)
        view.load_uri(url)

    def on_message(_manager, message):
        payload = json.loads(message.get_js_value().to_string())
        # The page reports its own failures on this channel too (`window.onerror`
        # in `bench/main.tsx`); a silent crash would otherwise be indistinguishable
        # from a slow run.
        if "error" in payload:
            print(f"# page error: {payload['error']}", file=sys.stderr)
        results.append(payload)
        # Straight on to the next configuration; the window is reused so that
        # every run shares one process, one compositor and one warm engine.
        GLib.idle_add(next_url)

    def on_crash(_view, reason):
        # A page that died looks exactly like a page that is slow, and the
        # difference is the whole finding.
        print(f"# web process gone: {reason}", file=sys.stderr)
        Gtk.main_quit()

    view.connect("web-process-terminated", on_crash)
    manager.connect("script-message-received::bench", on_message)
    manager.register_script_message_handler("bench")

    def give_up():
        print("# timed out waiting for a report", file=sys.stderr)
        Gtk.main_quit()
        return False

    GLib.timeout_add_seconds(TIMEOUT_S, give_up)
    window.connect("destroy", Gtk.main_quit)
    next_url()
    Gtk.main()

    print(json.dumps(results, indent=2))
    return 0 if len(results) == len(urls) else 1


if __name__ == "__main__":
    if len(sys.argv) < 2:
        print(__doc__, file=sys.stderr)
        raise SystemExit(2)
    raise SystemExit(main(sys.argv[1:]))
