import React from "react";
import ReactDOM from "react-dom/client";
import "./styles.css";
import App from "./App";

async function bootstrap() {
  // Dev-only: when the frontend runs in a plain browser (no Tauri shell), install a mock Tauri
  // IPC backend so the UI is fully functional for Playwright testing. Tree-shaken from production
  // builds (DEV guard) and inert inside `tauri dev` (the runtime guard inside installTauriMock).
  if (import.meta.env.DEV && !("__TAURI_INTERNALS__" in window)) {
    const { installTauriMock } = await import("./dev/tauriMock");
    installTauriMock();
  }

  ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
    <React.StrictMode>
      <App />
    </React.StrictMode>,
  );
}

void bootstrap();
