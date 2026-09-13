# Development setup

What a machine needs before `pnpm dev` opens a window, and why each item is
there. Recorded the way §16 asks for everything else: with the versions it was
actually verified against, not the versions someone remembers working.

**Verified on:** Ubuntu 25.04 · 2026-09-13.

Note that the design (§4) names **Ubuntu 24.04 LTS** as the primary target.
25.04 is what this machine runs; the packages are the same, the versions are
newer, and CI is the 24.04 check. If a webview bug ever reproduces on one and
not the other, that gap is the first thing to suspect.

## Toolchains

| | Verified | Notes |
|---|---|---|
| Rust | 1.97.1 | workspace `rust-version` is 1.85 |
| Node | 22.23.1 | CI pins 22 |
| pnpm | 11.21.0 | `packageManager` in the root `package.json` |
| git | 2.48.1 | the §13.1 attribution mechanism was verified against this |
| Claude Code | 2.1.270 | fixtures are 2.1.269 — see `roadmap.md` §7 |

## System libraries (Linux)

Tauri ships no webview (§4). On Linux it borrows the system's, so these are
build dependencies of the app itself, not optional extras:

```sh
sudo apt install -y \
  libwebkit2gtk-4.1-dev libgtk-3-dev libdbus-1-dev libxdo-dev \
  libayatana-appindicator3-dev librsvg2-dev patchelf
```

| Package | Verified | Why it is needed |
|---|---|---|
| `libwebkit2gtk-4.1-dev` | 2.50.4 | **the webview**. §4 develops against WebKitGTK deliberately: it is the weakest of the three engines, so a feed that is smooth here is smooth everywhere. Pulls in `javascriptcoregtk-4.1` and `libsoup-3.0`. |
| `libgtk-3-dev` | 3.24.49 | the window itself (`gdk-3.0`, via `tao`) |
| `libdbus-1-dev` | 1.16.2 | `libdbus-sys`, reached through the menu/tray stack — required even with the tray disabled |
| `libxdo-dev` | 3.20160805.1 | X11 input synthesis, a hard dependency of the same stack |
| `libayatana-appindicator3-dev` | 0.5.94 | tray icon support |
| `librsvg2-dev` | 2.60.0 | SVG rasterization for icons at bundle time |
| `patchelf` | 0.18.0 | AppImage packaging rewrites rpaths |

`build-essential`, `libssl-dev`, `pkg-config` and `file` are also required and
were already present here.

Two of these were not in the list this repository started from: `libgtk-3-dev`
and `libdbus-1-dev` only surface as a `pkg-config` failure part-way through
`cargo build`, one at a time, because each comes from a different build script.
That is the reason this table exists rather than a link to Tauri's docs.

macOS and Windows are unverified — nothing in this repository has been built on
either, and §13.1's edge cases already list non-Linux platforms as an open debt.

## Running it

```sh
pnpm install                 # once
pnpm dev                     # vite on :1420, then the Tauri window
pnpm build                   # .deb and AppImage into target/release/bundle/
```

Under it, unchanged:

```sh
cargo test --all             # replays the fixtures, regenerates packages/shared-types
pnpm --filter @agent-workspace/desktop typecheck
```

`cargo test` is what writes `packages/shared-types` (`TS_RS_EXPORT_DIR` in
`.cargo/config.toml`), and the generated files are committed. Adding a type that
crosses the IPC boundary therefore means: write the Rust struct, run
`cargo test`, commit the `.ts` file it produced. CI fails if you skip the last
step — including for a brand-new file, which the check previously could not see.

**Where the app keeps its state:** `~/.local/share/dev.agent-workspace.desktop/log.sqlite`.
Deleting it is a clean reset; nothing else is written outside a run's worktree.
