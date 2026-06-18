# Driving a Tauri webview UI with playwright-cli

`playwright-cli` (a terminal Playwright client; `playwright-cli --help`) drives the mocked frontend
in Chromium. Use a **named session** (`-s=app`) so every call shares one browser. Workflow:
`snapshot` to find refs / read state, act on the ref, `screenshot` + `Read` only when you must see
pixels.

## Core loop

```bash
playwright-cli -s=app open --browser=chrome <DEV_URL>
playwright-cli -s=app snapshot                 # accessibility tree with refs (e3, e15, …)
playwright-cli -s=app click e15                # by ref
playwright-cli -s=app type "hello"             # type into the focused/last-touched editable
playwright-cli -s=app press Enter
playwright-cli -s=app fill e5 "user@example.com" --submit
playwright-cli -s=app --raw eval "document.title"     # read anything from the page (cheap)
playwright-cli -s=app screenshot --filename=/tmp/x.png   # then Read /tmp/x.png
playwright-cli -s=app close
```

**Targeting:** prefer snapshot refs (`e15`). Also accepts CSS (`"#save"`), roles
(`"getByRole('button', { name: 'Save' })"`), text (`"getByText('Settings')"`), test ids
(`"getByTestId('row-3')"`), placeholders (`"getByPlaceholder('Search…')"`).

**Snapshot vs screenshot.** `snapshot` is a cheap text a11y tree — use it to navigate and to read
state (counts, labels, which view is active). `screenshot` (then `Read` the PNG) is for _pixels only_
— rendered media, layout, visual regressions. Screenshots cost many tokens; don't spam them.

**Reading state cheaply** without a screenshot:

```bash
playwright-cli -s=app --raw eval "document.body.innerText.slice(0,400)"
playwright-cli -s=app --raw eval "[...document.querySelectorAll('[data-testid=row]')].length"
playwright-cli -s=app --raw console | grep -i mock     # confirm the mock installed
```

## Gotchas specific to Tauri webview UIs

These bit us in practice and apply broadly:

- **Navigation isn't always a click.** Many desktop UIs use keyboard shortcuts and modes. Check the
  app's key handler (e.g. `useKeyboard`) for global keys and use `press`. Double-click may open a
  zoom/loupe/detail rather than switch screens — verify the resulting state with `eval`, don't
  assume. Read the app's view/router state to confirm where you landed.

- **Custom controls aren't accessible.** Many apps build sliders/toggles/knobs as `<div>`s with
  pointer handlers, not native `<input>`. They won't appear as `role=slider` in a snapshot and can't
  be `fill`ed. Drive them by mapping a value to a coordinate on the control, using `run-code`:

  ```bash
  playwright-cli -s=app run-code "async (page) => {
    const label='Volume', value=0.8, min=0, max=1;     // control's range
    const box = await page.evaluate((lbl) => {
      const el = [...document.querySelectorAll('*')].find(e =>
        e.previousElementSibling?.textContent?.trim()===lbl || e.getAttribute('aria-label')===lbl)
        || [...document.querySelectorAll('span')].find(e=>e.textContent.trim()===lbl)?.parentElement
             ?.querySelector('[style*=\"ew-resize\"],[role=slider],input');
      el.scrollIntoView({ block: 'center' });            // <-- see next bullet
      const r = el.getBoundingClientRect();
      return { l:r.left, t:r.top, w:r.width, h:r.height };
    }, label);
    const x = box.l + ((value-min)/(max-min))*box.w, y = box.t + box.h/2;
    await page.mouse.move(box.l+box.w*0.1, y); await page.mouse.down(); await page.mouse.move(x,y); await page.mouse.up();
  }"
  ```

  Adapt the element-finding to the app's DOM (inspect with `snapshot`/`eval` first).

- **Fixed headers/footers intercept clicks.** A control can be _in the layout_ but visually under a
  sticky toolbar/footer. A click at its `getBoundingClientRect()` position then hits the bar, not the
  control — verify with `document.elementFromPoint(x,y)` (you'll see e.g. `FOOTER`). Fix: call
  `el.scrollIntoView({ block: 'center' })` first (clears both bars), then re-measure and click. This
  is why the slider recipe above scrolls before measuring.

- **`page.mouse` and `mousedown/move/up`** both dispatch pointer events, so they drive
  `onPointerDown`/`setPointerCapture` controls. A click with no drag sets the value at the down
  position; add an explicit `move` between down and up to simulate a drag.

- **Custom-protocol images** (e.g. `asset://`, app-specific schemes) only render if the mock rewrote
  them (see `building-the-mock.md` §3). If thumbnails/images are blank and the console shows
  `ERR_UNKNOWN_URL_SCHEME`, the URL hook is missing or didn't cover CSS `background:url()`.

## Driving event-based UI (progress bars, live updates)

With `shouldMockEvents: true`, `listen()` resolves but handlers only fire if something emits. To
exercise a progress bar, emit the event from the page:

```bash
playwright-cli -s=app run-code "async (page) => {
  await page.evaluate(async () => {
    const { emit } = await import('@tauri-apps/api/event');
    await emit('download://progress', { done: 50, total: 100 });
  });
}"
```

(If the bare import path doesn't resolve in the page, emit via the app's own event module path, or
have the relevant `invoke` handler return data that drives the UI instead.)

## Forms, dialogs, waiting

- **Forms:** `fill <ref> <text>` (per field) then `click` submit, or `fill … --submit`.
- **File dialogs:** the mock returns "cancelled" by default (so flows don't hang). To exercise the
  post-pick flow, change the `plugin:dialog|open` handler to return a fake path, or
  `playwright-cli -s=app upload <file>` against a real `<input type=file>` if the app uses one.
- **Waiting:** after an action that triggers async work, poll with `eval` (re-read a count/label) or
  `playwright-cli -s=app run-code "async (page)=>{ await page.waitForSelector('[data-testid=ready]'); }"`.

## Verifying

Assert on state, not vibes: read a count/label via `eval`, check a class/attribute
(`eval "el => el.getAttribute('aria-pressed')" <ref>`), or diff snapshots
(`--raw snapshot > before.yml; …; --raw snapshot > after.yml; diff`). Reserve a screenshot for the
final visual confirmation.
