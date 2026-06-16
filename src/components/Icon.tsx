import React from "react";

export type IconName =
  | "search"
  | "cmd"
  | "import"
  | "export"
  | "copy"
  | "photos"
  | "folder"
  | "flag"
  | "clock"
  | "stack"
  | "bolt"
  | "star"
  | "edit"
  | "grid"
  | "square"
  | "grid-sm"
  | "grid-lg"
  | "chev"
  | "split"
  | "reset"
  | "pick"
  | "zoom"
  | "tag";

interface IconProps {
  name: IconName;
  className?: string;
  size?: number;
  style?: React.CSSProperties;
}

const PATHS: Record<IconName, React.ReactNode> = {
  search: (
    <>
      <circle cx="6.5" cy="6.5" r="5" />
      <path d="M11 11l3 3" />
    </>
  ),
  cmd: (
    <path d="M5 2a3 3 0 1 0 0 6h6a3 3 0 1 0 0-6 3 3 0 0 0-3 3v6a3 3 0 1 0 3-3H5a3 3 0 1 0 3 3" />
  ),
  import: <path d="M8 2v8m0 0l-3-3m3 3l3-3M3 13h10" />,
  export: <path d="M8 11V3m0 0l-3 3m3-3l3 3M3 13h10" />,
  copy: (
    <>
      <rect x="5" y="5" width="8" height="8" rx="1.5" />
      <path d="M3 11V4a1 1 0 0 1 1-1h7" />
    </>
  ),
  photos: (
    <>
      <rect x="2.5" y="3.5" width="11" height="9" rx="1.5" />
      <circle cx="6" cy="7" r="1.2" />
      <path d="M3 12l3-2.5L9 11l2-1.5 2 1.5" />
    </>
  ),
  folder: (
    <path d="M2.5 4.5a1 1 0 0 1 1-1h2.8l1.2 1.4H12.5a1 1 0 0 1 1 1v5.6a1 1 0 0 1-1 1h-9a1 1 0 0 1-1-1z" />
  ),
  flag: <path d="M4 2.5v11M4 3h7l-1.4 2.4L11 8H4" />,
  clock: (
    <>
      <circle cx="8" cy="8" r="5.5" />
      <path d="M8 5v3l2 1.4" />
    </>
  ),
  stack: (
    <>
      <path d="M8 2.5l5.5 3-5.5 3-5.5-3z" />
      <path d="M2.5 8.5L8 11.5l5.5-3" />
    </>
  ),
  bolt: <path d="M9 2L4 9h3l-1 5 5-7H8z" />,
  star: (
    <path d="M8 2.2l1.7 3.6 3.9.5-2.9 2.7.8 3.9L8 11l-3.5 1.9.8-3.9L2.4 6.3l3.9-.5z" />
  ),
  edit: <path d="M10.5 2.5l3 3L6 13l-3.5.5L3 10z" />,
  grid: (
    <>
      <rect x="2.5" y="2.5" width="4.5" height="4.5" rx="1" />
      <rect x="9" y="2.5" width="4.5" height="4.5" rx="1" />
      <rect x="2.5" y="9" width="4.5" height="4.5" rx="1" />
      <rect x="9" y="9" width="4.5" height="4.5" rx="1" />
    </>
  ),
  square: <rect x="2.5" y="2.5" width="11" height="11" rx="1.5" />,
  "grid-sm": (
    <>
      <rect x="3" y="3" width="3" height="3" />
      <rect x="7" y="3" width="3" height="3" />
      <rect x="3" y="7" width="3" height="3" />
      <rect x="7" y="7" width="3" height="3" />
    </>
  ),
  "grid-lg": (
    <>
      <rect x="2.5" y="2.5" width="5" height="5" />
      <rect x="8.5" y="2.5" width="5" height="5" />
    </>
  ),
  chev: <path d="M4 6l4 4 4-4" />,
  split: (
    <>
      <rect x="2.5" y="2.5" width="11" height="11" rx="1.5" />
      <path d="M8 2.5v11" />
    </>
  ),
  reset: <path d="M3 8a5 5 0 1 1 1.5 3.5M3 8V5m0 3h3" />,
  pick: (
    <>
      <path d="M3 13l2-5 5-5 2 2-5 5z" />
      <path d="M3 13l1.5-1.5" />
    </>
  ),
  zoom: (
    <>
      <circle cx="6.5" cy="6.5" r="4.5" />
      <path d="M10 10l3.5 3.5M5 6.5h3M6.5 5v3" />
    </>
  ),
  tag: (
    <>
      <path d="M8.2 2.5H13a.5.5 0 0 1 .5.5v4.8a1 1 0 0 1-.3.7l-5 5a1 1 0 0 1-1.4 0L3 9.8a1 1 0 0 1 0-1.4l4-4a1 1 0 0 1 .7-.3z" />
      <circle cx="10.8" cy="5.2" r="0.9" />
    </>
  ),
};

export default function Icon({ name, className, size = 14, style }: IconProps) {
  return (
    <svg
      viewBox="0 0 16 16"
      width={size}
      height={size}
      style={{ display: "block", flexShrink: 0, ...style }}
      stroke="currentColor"
      fill="none"
      strokeWidth="1.6"
      strokeLinecap="round"
      strokeLinejoin="round"
      className={className}
      aria-hidden="true"
    >
      {PATHS[name]}
    </svg>
  );
}
