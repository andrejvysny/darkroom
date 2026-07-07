import Slider from "./Slider";
import type { Denoise } from "../../lib/ipc";

/** AI raw-domain denoise (Lightroom "Denoise" parity). On-demand: toggling on runs the raw-mosaic
 *  denoise once (a few seconds, `busy`), then the Amount slider re-blends the cached result instantly.
 *  Mirrors Rust `Denoise`; the heavy compute + cache swap happens in the `denoise_apply` IPC. */
interface Props {
  value: Denoise;
  busy: boolean;
  onChange: (patch: Partial<Denoise>) => void;
}

export default function DenoisePanel({ value, busy, onChange }: Props) {
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 12 }}>
      <button
        type="button"
        onClick={() => onChange({ enabled: !value.enabled })}
        disabled={busy}
        style={{
          fontSize: 12,
          fontWeight: 600,
          padding: "7px 10px",
          borderRadius: "var(--radius-sm)",
          border: "1px solid var(--color-line-2)",
          background: value.enabled ? "var(--color-accent)" : "var(--color-elev)",
          color: value.enabled ? "#fff" : "var(--color-t1)",
          cursor: busy ? "default" : "pointer",
          opacity: busy ? 0.6 : 1,
        }}
      >
        {busy
          ? "Denoising…"
          : value.enabled
            ? "Denoise on"
            : "Apply denoise"}
      </button>

      {value.enabled && (
        <Slider
          label="Amount"
          min={0}
          max={100}
          defaultValue={50}
          value={value.amount}
          onChange={(v) => onChange({ amount: v })}
        />
      )}

      <span
        style={{
          fontSize: 10.5,
          color: "var(--color-t3)",
          lineHeight: 1.4,
        }}
      >
        Raw-domain noise reduction, applied before demosaic. Runs on apply (a few
        seconds); Amount then adjusts instantly.
      </span>
    </div>
  );
}
