import { useAppStore } from "./store/app";
import { useKeyboard } from "./hooks/useKeyboard";
import TopBar from "./components/TopBar";
import CommandPalette from "./components/CommandPalette";
import Toast from "./components/Toast";
import LibraryView from "./views/Library/LibraryView";
import DevelopView from "./views/Develop/DevelopView";

export default function App() {
  useKeyboard();
  const view = useAppStore((s) => s.view);

  return (
    <div
      style={{
        display: "flex",
        flexDirection: "column",
        height: "100vh",
        overflow: "hidden",
      }}
    >
      <TopBar />
      {view === "library" ? <LibraryView /> : <DevelopView />}
      <CommandPalette />
      <Toast />
    </div>
  );
}
