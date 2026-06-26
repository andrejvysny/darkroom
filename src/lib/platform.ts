// Runtime OS detection + keyboard-shortcut rendering. Single source of truth so the UI shows the
// right modifier glyph per platform (⌘ on macOS, Ctrl on Windows). Mirrors the navigator check
// already used for the thumb:// protocol base in ipc.ts.
export const isWindows =
  typeof navigator !== "undefined" && navigator.userAgent.includes("Windows");
export const isMac = !isWindows;

// Mac modifier glyph → Windows word. Shortcuts are authored as compact mac strings (e.g. "⌘⇧N") and
// rendered per-platform: unchanged on macOS, expanded to "Ctrl+Shift+N" on Windows.
const WIN_NAMES: Record<string, string> = {
  "⌘": "Ctrl",
  "⌃": "Ctrl",
  "⇧": "Shift",
  "⌥": "Alt",
};

/** Render a mac-authored shortcut string for the current platform. Plain keys pass through. */
export function fmtShortcut(mac: string): string {
  if (!isWindows) return mac;
  const parts: string[] = [];
  let rest = mac;
  for (const glyph of Object.keys(WIN_NAMES)) {
    if (rest.includes(glyph)) {
      parts.push(WIN_NAMES[glyph]);
      rest = rest.replace(glyph, "");
    }
  }
  if (rest) parts.push(rest);
  return parts.join("+");
}
