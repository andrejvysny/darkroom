import { useState } from "react";
import Icon from "../../components/Icon";

interface ModuleProps {
  title: string;
  defaultCollapsed?: boolean;
  children: React.ReactNode;
  onReset?: () => void;
}

export default function Module({
  title,
  defaultCollapsed = false,
  children,
  onReset,
}: ModuleProps) {
  const [collapsed, setCollapsed] = useState(defaultCollapsed);

  return (
    <div style={{ borderBottom: "1px solid var(--color-line)" }}>
      <div
        onClick={() => setCollapsed((c) => !c)}
        style={{
          display: "flex",
          alignItems: "center",
          gap: 8,
          padding: "12px 14px",
          cursor: "pointer",
          userSelect: "none",
        }}
      >
        <Icon
          name="chev"
          size={12}
          style={
            {
              color: "var(--color-t3)",
              transform: collapsed ? "rotate(-90deg)" : "none",
              transition: "transform .15s",
              flexShrink: 0,
            } as React.CSSProperties
          }
        />
        <h3
          style={{
            fontSize: 12.5,
            fontWeight: 600,
            letterSpacing: ".01em",
            color: "var(--color-t1)",
            flex: 1,
          }}
        >
          {title}
        </h3>
        <span
          className="module-reset"
          onClick={(e) => {
            e.stopPropagation();
            if (onReset) onReset();
          }}
          style={{
            fontSize: 10,
            color: "var(--color-t3)",
            cursor: onReset ? "pointer" : "default",
          }}
        >
          Reset
        </span>
      </div>
      {!collapsed && (
        <div
          style={{
            padding: "2px 14px 14px",
            display: "flex",
            flexDirection: "column",
            gap: 13,
          }}
        >
          {children}
        </div>
      )}
    </div>
  );
}
