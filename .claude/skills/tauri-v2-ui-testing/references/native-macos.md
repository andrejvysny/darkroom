# Tier 2 — native macOS automation of the real app

Use when Tier 1's mock can't answer the question (the real render — GPU/Canvas/WebGL —, native
dialogs/menus, real backend output) **and** you don't want the full Tier-3 bridge setup. Playwright
can't attach to WKWebView on macOS, so we automate at the OS level: `screencapture` for pixels,
`cliclick` / `osascript` for input. Coordinate-based and slower than Tier 1 — reach for it
deliberately. (For real DOM-aware E2E with the real backend, prefer **Tier 3** —
`references/full-app-e2e.md`.)

## Targeting the process — bundle id vs. process name (IMPORTANT)

How you find the running app depends on how it was launched (verified on a real app):

- **`tauri dev` runs a BARE binary** (`target/debug/<cargo-bin-name>`), **not a `.app` bundle** — so
  it has **no bundle identifier**. System Events sees it only by **process name = the cargo
  `[package] name`** in `src-tauri/Cargo.toml` (e.g. `darkroom`).
- **`tauri build` produces a `.app`** which **does** carry the bundle id (`identifier` in
  `tauri.conf.json`).

So target by **process** (never `tell application "<ProductName>"`, which can launch a same-named app
like the App Store "Darkroom"). `scripts/tauri-native-shot.sh` handles both: it tries the bundle id
first, then falls back to process-name candidates (cargo bin name, then productName), focuses that
exact process via `set frontmost`, and reads its front-window bounds — no app launching.

## Prerequisites (macOS permissions)

- The real app running: `npm run tauri dev` (or a built `.app`).
- **Automation → System Events** permission for your terminal: required to query the process and read
  window bounds. Without it the bounds query returns empty.
- **Screen Recording** permission for your terminal: required for `screencapture` of another app's
  window. Without it the capture is blank/black (not an error).
  (Both under System Settings → Privacy & Security. `cliclick` input injection additionally needs
  **Accessibility**.)
- `cliclick`: `brew install cliclick`. `screencapture` and `osascript` are built in.

## Screenshot the real window

```bash
.claude/skills/tauri-v2-ui-testing/scripts/tauri-native-shot.sh /tmp/app-real.png   # [out] [bundle-id|proc-name]
# then Read /tmp/app-real.png
```

It finds the process (bundle id → else process name), focuses it, reads `{position, size}` of its
front window via System Events, and `screencapture -o -x -R x,y,w,h`. Pass an explicit bundle id or
process name as arg 2 to override the auto-detection.

`screencapture` flags: `-R x,y,w,h` region (scriptable) · `-l <windowid>` a specific window · `-o` no
shadow · `-x` silent · `-c` to clipboard · `-t jpg` format · `-T n` delay.

Alternative window-id route (needs `brew install smokris/getwindowid/getwindowid`) to capture the
window's own bounds regardless of overlap:

```bash
screencapture -o -l"$(GetWindowID '<ProductName>' --list | head -1 | awk '{print $1}')" /tmp/app-real.png
```

## Inject input

Coordinates are **screen-absolute**. Read them from a fresh screenshot (the script's region origin
`x,y` is the window's top-left, so window-relative `(wx,wy)` → screen `(x+wx, y+wy)`).

`cliclick` (frontmost app receives events):

```bash
cliclick c:640,400          # left click at screen 640,400
cliclick dc:640,400         # double click
cliclick m:640,400          # move only
cliclick c:200,80 t:hello   # click then type "hello"
cliclick kp:return          # press Return (kp:esc, kp:tab, kp:arrow-down, …)
cliclick dd:100,100 du:300,100   # drag: down then up (e.g. a slider)
```

`osascript` for app-level actions (target by bundle id):

```bash
osascript -e 'tell application id "<bundle-id>" to activate'
osascript -e 'tell application "System Events" to keystroke "s" using command down'
```

## Hard limit: the web content is opaque

Inside the WKWebView the entire web UI is a single `AXWebArea` to the accessibility tree — you
**cannot** address individual web buttons/inputs by name via System Events. You can only:

- click by **pixel coordinate** (from a screenshot), and
- send keystrokes to whatever has focus (so the app's global shortcuts and focused controls work).

So Tier 2 is best for "capture the real render and eyeball it / pixel-click a known spot." For
structural, element-aware interaction with the real backend, use **Tier 3** (`full-app-e2e.md`).
