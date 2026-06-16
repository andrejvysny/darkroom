import { useEffect } from "react";
import { useAppStore } from "../store/app";

/** Transient bottom-center status message (export feedback, etc.). */
export default function Toast() {
  const toast = useAppStore((s) => s.toast);
  const setToast = useAppStore((s) => s.setToast);

  useEffect(() => {
    if (!toast || toast === "Exporting…") return;
    const t = setTimeout(() => setToast(null), 2800);
    return () => clearTimeout(t);
  }, [toast, setToast]);

  if (!toast) return null;

  return (
    <div
      style={{
        position: "fixed",
        bottom: 56,
        left: "50%",
        transform: "translateX(-50%)",
        background: "#26262a",
        color: "var(--color-t1)",
        border: "1px solid var(--color-line-2)",
        borderRadius: "var(--radius-md)",
        padding: "9px 16px",
        fontSize: 12.5,
        fontFamily: "var(--font-ui)",
        boxShadow: "0 12px 40px rgba(0,0,0,.5)",
        zIndex: 100,
        maxWidth: "80vw",
        whiteSpace: "nowrap",
        overflow: "hidden",
        textOverflow: "ellipsis",
      }}
    >
      {toast}
    </div>
  );
}
