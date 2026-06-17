import { useRef, useState, useCallback } from "react";

interface SliderProps {
  label: string;
  min: number;
  max: number;
  defaultValue: number;
  bipolar?: boolean;
  decimals?: number;
  suffix?: string;
  /** If provided, slider is controlled: value is driven externally. */
  value?: number;
  /** Required when value prop is provided. */
  onChange?: (value: number) => void;
}

function fmt(
  v: number,
  decimals: number,
  bipolar: boolean,
  suffix: string,
): string {
  const s = decimals > 0 ? v.toFixed(decimals) : String(Math.round(v));
  return (bipolar && v > 0 ? "+" : "") + s + suffix;
}

export default function Slider({
  label,
  min,
  max,
  defaultValue,
  bipolar = false,
  decimals = 0,
  suffix = "",
  value: externalValue,
  onChange,
}: SliderProps) {
  const isControlled = externalValue !== undefined && onChange !== undefined;
  const [localValue, setLocalValue] = useState(defaultValue);

  const value = isControlled ? externalValue : localValue;
  const trackRef = useRef<HTMLDivElement>(null);
  const dragging = useRef(false);

  const setFromClientX = useCallback(
    (clientX: number) => {
      if (!trackRef.current) return;
      const rect = trackRef.current.getBoundingClientRect();
      // A zero-width track (collapsed/unmeasured module) makes the position ratio NaN/±Infinity,
      // which would flow through onChange straight into the persisted develop params. Bail instead.
      if (rect.width <= 0) return;
      const p = Math.max(0, Math.min(1, (clientX - rect.left) / rect.width));
      const next = min + p * (max - min);
      if (!Number.isFinite(next)) return;
      if (isControlled) {
        onChange(next);
      } else {
        setLocalValue(next);
      }
    },
    [min, max, isControlled, onChange],
  );

  const resetValue = bipolar ? 0 : defaultValue;

  const pct = ((value - min) / (max - min)) * 100;
  const zeroPct = bipolar ? ((0 - min) / (max - min)) * 100 : 0;
  const fillLeft = bipolar ? Math.min(zeroPct, pct) : 0;
  const fillWidth = bipolar ? Math.abs(pct - zeroPct) : pct;

  return (
    <div
      style={{
        display: "grid",
        gridTemplateColumns: "1fr auto",
        gap: "3px 8px",
        alignItems: "center",
      }}
    >
      <span style={{ fontSize: 12, color: "var(--color-t2)" }}>{label}</span>
      <span
        style={{
          fontFamily: "var(--font-mono)",
          fontSize: 11.5,
          color: "var(--color-t1)",
          textAlign: "right",
          minWidth: 42,
        }}
      >
        {fmt(value, decimals, bipolar, suffix)}
      </span>
      {/* Track */}
      <div
        ref={trackRef}
        onPointerDown={(e) => {
          dragging.current = true;
          (e.currentTarget as HTMLDivElement).setPointerCapture(e.pointerId);
          setFromClientX(e.clientX);
        }}
        onPointerMove={(e) => {
          if (dragging.current) setFromClientX(e.clientX);
        }}
        onPointerUp={() => {
          dragging.current = false;
        }}
        onDoubleClick={() => {
          if (isControlled) {
            onChange(resetValue);
          } else {
            setLocalValue(resetValue);
          }
        }}
        style={{
          gridColumn: "1 / 3",
          position: "relative",
          height: 14,
          cursor: "ew-resize",
        }}
      >
        {/* Rail */}
        <div
          style={{
            position: "absolute",
            top: 6,
            left: 0,
            right: 0,
            height: 2,
            background: "var(--color-hover)",
            borderRadius: 2,
          }}
        />
        {/* Bipolar center marker */}
        {bipolar && (
          <div
            style={{
              position: "absolute",
              top: 3,
              height: 8,
              width: 1,
              background: "var(--color-line-2)",
              left: "50%",
            }}
          />
        )}
        {/* Fill */}
        <div
          style={{
            position: "absolute",
            top: 6,
            height: 2,
            background: "var(--color-accent)",
            borderRadius: 2,
            left: `${fillLeft}%`,
            width: `${fillWidth}%`,
          }}
        />
        {/* Knob */}
        <div
          style={{
            position: "absolute",
            top: 1,
            width: 12,
            height: 12,
            borderRadius: "50%",
            background: "var(--color-t1)",
            border: "2px solid var(--color-app)",
            transform: "translateX(-50%)",
            boxShadow: "0 1px 3px rgba(0,0,0,.5)",
            left: `${pct}%`,
            pointerEvents: "none",
          }}
        />
      </div>
    </div>
  );
}
