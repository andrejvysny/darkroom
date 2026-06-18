import { useEffect } from "react";
import { useAppStore } from "../store/app";
import { runExport } from "../lib/export";

export function useKeyboard() {
  const setPaletteOpen = useAppStore((s) => s.setPaletteOpen);
  const setView = useAppStore((s) => s.setView);
  const setGridMode = useAppStore((s) => s.setGridMode);

  useEffect(() => {
    function handler(e: KeyboardEvent) {
      const tag = (e.target as HTMLElement | null)?.tagName;
      const typing = tag === "INPUT" || tag === "TEXTAREA";

      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setPaletteOpen(true);
        return;
      }
      // Cmd/Ctrl+E opens the editor (Develop) on the selected/opened photo. Cmd/Ctrl+Shift+E
      // exports it (kept off the bare chord since E is now the edit shortcut).
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "e") {
        e.preventDefault();
        if (e.shiftKey) void runExport(useAppStore.getState().selectedId);
        else setView("develop");
        return;
      }
      if (e.key === "Escape") {
        setPaletteOpen(false);
        // Close the full-preview loupe and return to the library grid.
        if (useAppStore.getState().gridMode === "loupe") {
          setGridMode("grid");
          setView("library");
        }
        return;
      }
      if (e.key === "g" && !e.metaKey && !e.ctrlKey && !typing) {
        setView("library");
      }
      if (e.key === "d" && !e.metaKey && !e.ctrlKey && !typing) {
        setView("develop");
      }
    }
    document.addEventListener("keydown", handler);
    return () => document.removeEventListener("keydown", handler);
  }, [setPaletteOpen, setView, setGridMode]);
}
