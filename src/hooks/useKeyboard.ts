import { useEffect } from "react";
import { useAppStore } from "../store/app";
import { runExport } from "../lib/export";

export function useKeyboard() {
  const setPaletteOpen = useAppStore((s) => s.setPaletteOpen);
  const setView = useAppStore((s) => s.setView);

  useEffect(() => {
    function handler(e: KeyboardEvent) {
      const tag = (e.target as HTMLElement | null)?.tagName;
      const typing = tag === "INPUT" || tag === "TEXTAREA";

      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setPaletteOpen(true);
      }
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "e") {
        e.preventDefault();
        void runExport(useAppStore.getState().selectedId);
      }
      if (e.key === "Escape") {
        setPaletteOpen(false);
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
  }, [setPaletteOpen, setView]);
}
