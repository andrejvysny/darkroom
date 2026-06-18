# Worked example — Darkroom (a real Tauri v2 app)

A complete, verified instantiation of this skill, so you can see what "build the mock + drive it"
looks like end to end. Darkroom = Tauri v2 + React 19 + Vite, a RAW photo library/editor.

## Config (from `src-tauri/tauri.conf.json`)

- `productName: "Darkroom"`, `identifier: "com.andrejvysny.darkroom"`, dev URL `http://localhost:1420`,
  `beforeDevCommand: "npm run dev"`. Frontend at repo-root `src/`.

## The mock (the complete file)

`assets/example-darkroom-mock.ts` is the actual working mock — read it as a reference. It covers the
~55 commands `tauri-discover-ipc.sh` finds, with:

- A 48-image fixture set + working filter/sort/paging mirroring `library_query`.
- Binary returns (`develop_render`, `develop_preview_jpeg`, `loupe_jpeg`) as canvas-generated JPEG
  `ArrayBuffer`s whose look responds to exposure/temp/tint — so slider→render is verifiable.
- A synthetic histogram, stubbed AI/analysis, cull edits persisted in module scope.

In the live repo it's wired as three pieces (the standard pattern):

- `src/dev/tauriMock.ts` — the mock.
- `src/main.tsx` — `bootstrap()` installs it before render under
  `import.meta.env.DEV && !("__TAURI_INTERNALS__" in window)`.
- `src/lib/ipc.ts` — `thumbUrl()` consults `window.__darkroomThumbMock` in dev (the custom-protocol
  hook for `thumb://`).

## Custom protocol: `thumb://`

Grid thumbnails are `thumb://localhost/<hash>?size=N`, used in CSS `background: url(...)` (the grid)
and `<img src>` (the loupe). The browser can't fetch them. Fix = the URL-builder hook (option 1 in
`building-the-mock.md` §3): `thumbUrl()` returns a placeholder SVG data URL in dev. Two traps learned
here:

- Intercepting only `<img>.src` is NOT enough — the grid uses CSS `background:url()`. The
  builder-hook covers both.
- `encodeURIComponent` leaves `(`/`)` intact; the SVG's `hsl(...)` then closes the unquoted CSS
  `url()` early. Escape parens to `%28`/`%29`.

## Driving it (verified)

```bash
.claude/skills/tauri-v2-ui-testing/scripts/tauri-dev-web.sh                       # → http://localhost:1420
playwright-cli -s=app open --browser=chrome http://localhost:1420
playwright-cli -s=app --raw console | grep tauriMock                              # mock live (48 fixtures)
```

- **Library:** click a grid cell to select. **Double-click opens the Loupe (zoom in Library), NOT
  Develop.** To open Develop: select, then `press d` (or click the `Develop` toggle). Typing in the
  search box filters the grid (mock honors filename/star/flag/label filters).
- **Develop sliders** (Exposure/Temp/Tint/…) are custom div sliders. The lower ones (Light section)
  sit under the fixed footer, so **`scrollIntoView({block:'center'})` the track before dragging** (a
  click at the raw layout position hits the `FOOTER`). Then map value→x on the track. Verified:
  Exposure → +2.00 visibly brightens the render and the stage prints `exposure 2.00 …`.

These two behaviors (double-click=loupe; lower sliders under the footer) are app-specific — the point
of the example is the _method_ of discovering them: drive, read state with `eval`, and when a click
"does nothing", check `document.elementFromPoint`.

## Keyboard map (Darkroom)

`g`→Library, `d`→Develop, `Esc`→back, `⌘K`→command palette (`src/hooks/useKeyboard.ts`).

## All three tiers were validated on Darkroom

- **Tier 1** — mock frontend in Chromium via `playwright-cli`: library grid, filter-by-typing
  (48→N), select, keyboard nav to Develop, custom-slider drag (Exposure → +2.00 → render changed).
- **Tier 2** — `scripts/tauri-native-shot.sh` captured the real app window (real RAW thumbnails +
  EXIF); `cliclick` mouse + keyboard injection switched the real app to Develop. (Surfaced the
  fix: a `tauri dev` binary has no bundle id → target by process name.)
- **Tier 3** — `tauri-plugin-playwright` (`tauri` mode) integrated behind an `e2e-testing` feature:
  socket bridge came up, and `invoke('thumb_cache_cap')` / `library_count` answered from the **real
  Rust backend** while the mock stayed inactive. (The integration was reverted afterward — it was
  skill validation, not a permanent app change; re-apply via `references/full-app-e2e.md`.)
