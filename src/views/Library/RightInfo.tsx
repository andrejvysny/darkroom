import { Fragment, useState, useEffect } from "react";
import {
  thumbUrl,
  imageDetections,
  imageCaption,
  imagePresence,
  imageUserLabels,
  setImageUserLabel,
  imageHistogram,
  imageFaces,
  DETECTION_CATEGORIES,
  type HistData,
  type ImageRow,
  type KeywordRow,
  type CollectionRow,
  type Detection,
  type ImageCaption,
  type Presence,
  type UserLabels,
  type ImageFace,
} from "../../lib/ipc";
import { useAppStore } from "../../store/app";

const LABEL_COLORS: { key: string; bg: string }[] = [
  { key: "red", bg: "var(--color-lab-red)" },
  { key: "yellow", bg: "var(--color-lab-yellow)" },
  { key: "green", bg: "var(--color-lab-green)" },
  { key: "blue", bg: "var(--color-lab-blue)" },
  { key: "purple", bg: "var(--color-lab-purple)" },
];

// Real per-channel histogram drawn from `HistData` (256 bins/channel). Log-scaled so shadow detail
// is visible; `null` while loading renders an empty axis (never synthetic data).
function HistogramSvg({ data }: { data: HistData | null }) {
  const W = 256;
  const H = 64;
  if (!data) {
    return (
      <svg
        width="100%"
        height="100%"
        viewBox={`0 0 ${W} ${H}`}
        preserveAspectRatio="none"
      />
    );
  }
  const maxv = Math.max(1, ...data.r, ...data.g, ...data.b);
  const norm = Math.log1p(maxv);
  const path = (bins: number[]): string => {
    let d = `M0 ${H} `;
    const n = bins.length;
    for (let i = 0; i < n; i++) {
      const x = (i / (n - 1)) * W;
      const y = H - (Math.log1p(bins[i]) / norm) * H;
      d += `L${x.toFixed(1)} ${y.toFixed(1)} `;
    }
    return d + `L${W} ${H} Z`;
  };
  const channels: [string, number[]][] = [
    ["#c56d6d", data.r],
    ["#6db074", data.g],
    ["#5b93cf", data.b],
  ];
  return (
    <svg
      width="100%"
      height="100%"
      viewBox={`0 0 ${W} ${H}`}
      preserveAspectRatio="none"
    >
      {channels.map(([color, bins]) => (
        <path
          key={color}
          d={path(bins)}
          fill={color}
          opacity={0.5}
          style={{ mixBlendMode: "screen" }}
        />
      ))}
    </svg>
  );
}

function formatDate(epoch: number): string {
  const d = new Date(epoch * 1000);
  const y = d.getFullYear();
  const m = String(d.getMonth() + 1).padStart(2, "0");
  const day = String(d.getDate()).padStart(2, "0");
  return `${y}-${m}-${day}`;
}

export interface RightInfoHandlers {
  onSetRating: (stars: number) => void;
  onSetFlag: (flag: "none" | "pick" | "reject") => void;
  onSetLabel: (label: string | null) => void;
  onAddKeyword: (name: string) => void;
  onRemoveKeyword: (keywordId: number) => void;
  onAddToCollection: (collectionId: number) => void;
  onRemoveFromCollection: (collectionId: number) => void;
  /** Called after a Contains-person/animal label toggle, so the parent can refresh facet counts. */
  onPresenceChanged?: () => void;
}

interface RightInfoProps {
  selectedImage: ImageRow | null;
  /** Keywords applied to the selected image. */
  keywords: KeywordRow[];
  /** All known keywords (for add-field autocomplete). */
  keywordSuggestions: KeywordRow[];
  /** Static collections containing the selected image. */
  imageCollections: CollectionRow[];
  /** All collections (static + smart) — used to populate the add-to dropdown. */
  allCollections: CollectionRow[];
  handlers: RightInfoHandlers;
  /** Bumped after analysis:done so the AI section re-fetches for the same image. */
  analysisVersion: number;
}

export default function RightInfo({
  selectedImage,
  keywords,
  keywordSuggestions,
  imageCollections,
  allCollections,
  handlers,
  analysisVersion,
}: RightInfoProps) {
  const meta = selectedImage;
  const thumbVersions = useAppStore((s) => s.thumbVersions);
  const [kwInput, setKwInput] = useState("");
  const [aiCaption, setAiCaption] = useState<ImageCaption | null>(null);
  const [aiDetections, setAiDetections] = useState<Detection[]>([]);
  const [aiPresence, setAiPresence] = useState<Presence | null>(null);
  const [aiFaces, setAiFaces] = useState<ImageFace[]>([]);
  const [labels, setLabels] = useState<UserLabels>({
    containsPerson: null,
    containsAnimal: null,
  });
  const [hist, setHist] = useState<HistData | null>(null);

  useEffect(() => {
    if (meta === null) {
      setAiCaption(null);
      setAiDetections([]);
      setAiPresence(null);
      setAiFaces([]);
      setLabels({ containsPerson: null, containsAnimal: null });
      setHist(null);
      return;
    }
    let cancelled = false;
    setHist(null);
    void imageHistogram(meta.id).then((h) => {
      if (!cancelled) setHist(h);
    });
    void Promise.all([
      imageCaption(meta.id),
      imageDetections(meta.id),
      imagePresence(meta.id),
      imageUserLabels(meta.id),
      imageFaces(meta.id),
    ]).then(([cap, dets, pres, lab, faces]) => {
      if (!cancelled) {
        setAiCaption(cap);
        setAiDetections(dets);
        setAiPresence(pres);
        setLabels(lab);
        setAiFaces(faces);
      }
    });
    return () => {
      cancelled = true;
    };
  }, [meta?.id, analysisVersion]);

  const toggleLabel = (field: "person" | "animal", checked: boolean) => {
    if (meta === null) return;
    const key = field === "person" ? "containsPerson" : "containsAnimal";
    setLabels((prev) => ({ ...prev, [key]: checked })); // optimistic
    void setImageUserLabel(meta.id, field, checked)
      .then(() => handlers.onPresenceChanged?.())
      .catch(() => {
        setLabels((prev) => ({ ...prev, [key]: !checked })); // revert on error
      });
  };

  const memberIds = new Set(imageCollections.map((c) => c.id));
  const addableCollections = allCollections.filter(
    (c) => !c.isSmart && !memberIds.has(c.id),
  );
  const stars = meta?.stars ?? 0;
  const flag = meta?.flag ?? "none";
  const activeLabel = meta?.colorLabel ?? "";

  const metaRows: { label: string; value: string }[] = meta
    ? [
        { label: "Camera", value: meta.cameraModel ?? "—" },
        { label: "Lens", value: meta.lens ?? "—" },
        {
          label: "Focal",
          value: meta.focalLength != null ? `${meta.focalLength} mm` : "—",
        },
        {
          label: "Aperture",
          value: meta.aperture != null ? `f/${meta.aperture}` : "—",
        },
        { label: "Shutter", value: meta.shutter ?? "—" },
        { label: "ISO", value: meta.iso != null ? String(meta.iso) : "—" },
        {
          label: "Size",
          value:
            meta.width != null && meta.height != null
              ? `${meta.width}×${meta.height}`
              : "—",
        },
        {
          label: "Date",
          value: meta.captureDate != null ? formatDate(meta.captureDate) : "—",
        },
        { label: "File", value: meta.filename },
      ]
    : [];

  const previewSrc = meta
    ? thumbUrl(meta.contentHash, 512, meta.editedAt, thumbVersions[meta.id])
    : null;

  const appliedNames = new Set(keywords.map((k) => k.name.toLowerCase()));
  const suggestions = keywordSuggestions.filter(
    (k) => !appliedNames.has(k.name.toLowerCase()),
  );

  function commitKeyword() {
    const name = kwInput.trim();
    if (!name || !meta) return;
    if (!appliedNames.has(name.toLowerCase())) handlers.onAddKeyword(name);
    setKwInput("");
  }

  return (
    <aside
      style={{
        background: "var(--color-app)",
        borderLeft: "1px solid var(--color-line)",
        overflowY: "auto",
        minHeight: 0,
        height: "100%",
      }}
    >
      {/* Preview */}
      <div
        style={{
          margin: 14,
          borderRadius: "var(--radius-md)",
          overflow: "hidden",
          outline: "1px solid var(--color-line)",
          position: "relative",
          background: "var(--color-stage)",
          lineHeight: 0,
        }}
      >
        {previewSrc && (
          <img
            src={previewSrc}
            alt={meta?.filename ?? ""}
            style={{ width: "100%", height: "auto", display: "block" }}
          />
        )}
        {previewSrc && (
          <div
            style={{
              position: "absolute",
              inset: 0,
              boxShadow: "inset 0 0 50px rgba(0,0,0,.4)",
              pointerEvents: "none",
            }}
          />
        )}
      </div>

      {/* Rating */}
      <div
        style={{
          padding: "14px 16px",
          borderTop: "1px solid var(--color-line)",
        }}
      >
        <div
          style={{
            fontSize: 10.5,
            letterSpacing: ".06em",
            textTransform: "uppercase",
            color: "var(--color-t3)",
            fontWeight: 600,
            marginBottom: 10,
          }}
        >
          Rating
        </div>
        <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
          {/* Stars */}
          <div data-testid="rating-stars" style={{ display: "flex", gap: 1 }}>
            {[1, 2, 3, 4, 5].map((n) => (
              <svg
                key={n}
                viewBox="0 0 16 16"
                width={16}
                height={16}
                fill={n <= stars ? "var(--color-star)" : "none"}
                stroke={n <= stars ? "var(--color-star)" : "var(--color-t3)"}
                strokeWidth="1.2"
                style={{
                  cursor: meta ? "pointer" : "default",
                  display: "block",
                }}
                onClick={() => {
                  if (!meta) return;
                  handlers.onSetRating(n === stars ? 0 : n);
                }}
              >
                <path d="M8 2.2l1.7 3.6 3.9.5-2.9 2.7.8 3.9L8 11l-3.5 1.9.8-3.9L2.4 6.3l3.9-.5z" />
              </svg>
            ))}
          </div>
          {/* Pick/Reject */}
          <div style={{ display: "flex", gap: 6, flex: 1 }}>
            <button
              disabled={!meta}
              onClick={() =>
                handlers.onSetFlag(flag === "pick" ? "none" : "pick")
              }
              style={{
                flex: 1,
                padding: 6,
                borderRadius: "var(--radius-sm)",
                border: "1px solid",
                fontSize: 12,
                fontWeight: 500,
                color:
                  flag === "pick" ? "var(--color-pick)" : "var(--color-t2)",
                borderColor:
                  flag === "pick"
                    ? "rgba(109,176,116,.4)"
                    : "var(--color-line)",
                background:
                  flag === "pick" ? "rgba(109,176,116,.1)" : "transparent",
                cursor: meta ? "pointer" : "default",
              }}
            >
              Pick
            </button>
            <button
              disabled={!meta}
              onClick={() =>
                handlers.onSetFlag(flag === "reject" ? "none" : "reject")
              }
              style={{
                flex: 1,
                padding: 6,
                borderRadius: "var(--radius-sm)",
                border: "1px solid",
                fontSize: 12,
                fontWeight: 500,
                color:
                  flag === "reject" ? "var(--color-reject)" : "var(--color-t2)",
                borderColor:
                  flag === "reject"
                    ? "rgba(197,109,109,.4)"
                    : "var(--color-line)",
                background:
                  flag === "reject" ? "rgba(197,109,109,.1)" : "transparent",
                cursor: meta ? "pointer" : "default",
              }}
            >
              Reject
            </button>
          </div>
        </div>
        {/* Color labels */}
        <div
          style={{
            display: "flex",
            gap: 6,
            alignItems: "center",
            marginTop: 12,
          }}
        >
          {LABEL_COLORS.map(({ key, bg }) => (
            <span
              key={key}
              onClick={() => {
                if (!meta) return;
                handlers.onSetLabel(activeLabel === key ? null : key);
              }}
              style={{
                width: 9,
                height: 9,
                borderRadius: "50%",
                background: bg,
                boxShadow:
                  activeLabel === key
                    ? "0 0 0 2px var(--color-accent-line)"
                    : "0 0 0 2px rgba(0,0,0,.3)",
                cursor: meta ? "pointer" : "default",
                display: "block",
              }}
            />
          ))}
        </div>
      </div>

      {/* Histogram */}
      <div
        style={{
          padding: "14px 16px",
          borderTop: "1px solid var(--color-line)",
        }}
      >
        <div
          style={{
            fontSize: 10.5,
            letterSpacing: ".06em",
            textTransform: "uppercase",
            color: "var(--color-t3)",
            fontWeight: 600,
            marginBottom: 10,
          }}
        >
          Histogram
        </div>
        <div
          style={{
            height: 64,
            borderRadius: "var(--radius-sm)",
            background: "var(--color-stage)",
            outline: "1px solid var(--color-line)",
            overflow: "hidden",
          }}
        >
          <HistogramSvg data={hist} />
        </div>
      </div>

      {/* Metadata */}
      <div
        style={{
          padding: "14px 16px",
          borderTop: "1px solid var(--color-line)",
        }}
      >
        <div
          style={{
            fontSize: 10.5,
            letterSpacing: ".06em",
            textTransform: "uppercase",
            color: "var(--color-t3)",
            fontWeight: 600,
            marginBottom: 10,
          }}
        >
          Metadata
        </div>
        {meta === null ? (
          <div style={{ fontSize: 12, color: "var(--color-t3)" }}>
            No image selected
          </div>
        ) : (
          <dl
            style={{
              display: "grid",
              gridTemplateColumns: "auto 1fr",
              gap: "7px 14px",
              fontSize: 12,
            }}
          >
            {metaRows.map(({ label, value }) => (
              <Fragment key={label}>
                <dt style={{ color: "var(--color-t3)" }}>{label}</dt>
                <dd
                  style={{
                    color: "var(--color-t1)",
                    fontFamily: "var(--font-mono)",
                    fontSize: 11.5,
                    textAlign: "right",
                  }}
                >
                  {value}
                </dd>
              </Fragment>
            ))}
          </dl>
        )}
      </div>

      {/* Keywords */}
      <div
        style={{
          padding: "14px 16px",
          borderTop: "1px solid var(--color-line)",
        }}
      >
        <div
          style={{
            fontSize: 10.5,
            letterSpacing: ".06em",
            textTransform: "uppercase",
            color: "var(--color-t3)",
            fontWeight: 600,
            marginBottom: 10,
          }}
        >
          Keywords
        </div>
        {meta === null ? (
          <div style={{ fontSize: 12, color: "var(--color-t3)" }}>
            No image selected
          </div>
        ) : (
          <>
            <div style={{ display: "flex", flexWrap: "wrap", gap: 6 }}>
              {keywords.length === 0 && (
                <span style={{ fontSize: 11.5, color: "var(--color-t3)" }}>
                  No keywords
                </span>
              )}
              {keywords.map((kw) => (
                <span
                  key={kw.id}
                  style={{
                    display: "inline-flex",
                    alignItems: "center",
                    gap: 5,
                    fontSize: 11.5,
                    color: "var(--color-t2)",
                    background: "var(--color-elev)",
                    border: "1px solid var(--color-line)",
                    borderRadius: 20,
                    padding: "3px 5px 3px 9px",
                  }}
                >
                  {kw.name}
                  <button
                    onClick={() => handlers.onRemoveKeyword(kw.id)}
                    title={`Remove ${kw.name}`}
                    aria-label={`Remove ${kw.name}`}
                    style={{
                      display: "flex",
                      alignItems: "center",
                      justifyContent: "center",
                      width: 14,
                      height: 14,
                      borderRadius: "50%",
                      border: "none",
                      background: "var(--color-line)",
                      color: "var(--color-t1)",
                      fontSize: 11,
                      lineHeight: 1,
                      cursor: "pointer",
                    }}
                  >
                    ×
                  </button>
                </span>
              ))}
            </div>
            <input
              list="kw-suggestions"
              value={kwInput}
              onChange={(e) => setKwInput(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") {
                  e.preventDefault();
                  commitKeyword();
                }
              }}
              onBlur={commitKeyword}
              placeholder="Add keyword…"
              style={{
                marginTop: 8,
                width: "100%",
                background: "var(--color-panel)",
                border: "1px solid var(--color-line)",
                borderRadius: "var(--radius-sm)",
                color: "var(--color-t1)",
                fontSize: 12,
                padding: "5px 8px",
                outline: "none",
              }}
            />
            <datalist id="kw-suggestions">
              {suggestions.map((k) => (
                <option key={k.id} value={k.name} />
              ))}
            </datalist>
          </>
        )}
      </div>

      {/* Detected / AI */}
      <div
        style={{
          padding: "14px 16px",
          borderTop: "1px solid var(--color-line)",
        }}
      >
        <div
          style={{
            fontSize: 10.5,
            letterSpacing: ".06em",
            textTransform: "uppercase",
            color: "var(--color-t3)",
            fontWeight: 600,
            marginBottom: 10,
          }}
        >
          Detected / AI
        </div>
        {meta === null ? (
          <div style={{ fontSize: 12, color: "var(--color-t3)" }}>
            No image selected
          </div>
        ) : aiCaption === null &&
          aiDetections.length === 0 &&
          aiPresence === null &&
          aiFaces.length === 0 ? (
          <div style={{ fontSize: 11.5, color: "var(--color-t3)" }}>
            Not analyzed yet
          </div>
        ) : (
          <>
            {aiCaption !== null && (
              <>
                <div
                  style={{
                    fontSize: 12,
                    color: "var(--color-t2)",
                    fontStyle: "italic",
                    lineHeight: 1.45,
                    marginBottom: aiCaption.keywords.length > 0 ? 6 : 0,
                  }}
                >
                  {aiCaption.caption}
                </div>
                {aiCaption.keywords.length > 0 && (
                  <div
                    style={{
                      fontSize: 11,
                      color: "var(--color-t3)",
                      marginBottom: aiDetections.length > 0 ? 10 : 0,
                      lineHeight: 1.4,
                    }}
                  >
                    {aiCaption.keywords.join(", ")}
                  </div>
                )}
              </>
            )}
            {DETECTION_CATEGORIES.map((cat) => {
              const dets = aiDetections.filter((d) => d.category === cat);
              if (dets.length === 0) return null;
              return (
                <div key={cat} style={{ marginBottom: 8 }}>
                  <div
                    style={{
                      fontSize: 10,
                      letterSpacing: ".05em",
                      textTransform: "uppercase",
                      color: "var(--color-t3)",
                      fontWeight: 600,
                      marginBottom: 5,
                    }}
                  >
                    {cat}
                  </div>
                  <div style={{ display: "flex", flexWrap: "wrap", gap: 5 }}>
                    {dets.map((d, i) => (
                      <span
                        key={i}
                        style={{
                          display: "inline-flex",
                          alignItems: "center",
                          gap: 4,
                          fontSize: 11,
                          color: "var(--color-t2)",
                          background: "var(--color-elev)",
                          border: "1px solid var(--color-line)",
                          borderRadius: 20,
                          padding: "2px 8px",
                        }}
                      >
                        {d.label}
                        <span
                          style={{ color: "var(--color-t3)", fontSize: 10 }}
                        >
                          {Math.round(d.confidence * 100)}%
                        </span>
                      </span>
                    ))}
                  </div>
                </div>
              );
            })}
            {aiFaces.length > 0 && (
              <div style={{ marginBottom: 8 }}>
                <div
                  style={{
                    fontSize: 10,
                    letterSpacing: ".05em",
                    textTransform: "uppercase",
                    color: "var(--color-t3)",
                    fontWeight: 600,
                    marginBottom: 5,
                  }}
                >
                  People
                </div>
                <div style={{ display: "flex", flexWrap: "wrap", gap: 5 }}>
                  {aiFaces.map((f) => (
                    <span
                      key={f.id}
                      style={{
                        display: "inline-flex",
                        alignItems: "center",
                        fontSize: 11,
                        color:
                          f.personName != null
                            ? "var(--color-t2)"
                            : "var(--color-t3)",
                        fontStyle: f.personName != null ? undefined : "italic",
                        background: "var(--color-elev)",
                        border: "1px solid var(--color-line)",
                        borderRadius: 20,
                        padding: "2px 8px",
                      }}
                    >
                      {f.personName ?? "Unknown"}
                    </span>
                  ))}
                </div>
              </div>
            )}
            {aiPresence !== null && (
              <div
                style={{
                  fontSize: 10.5,
                  color: "var(--color-t3)",
                  marginTop: 4,
                }}
                title="MobileCLIP presence probe (advisory; manual labels below are the ground truth)"
              >
                probe&nbsp; person {aiPresence.pPerson.toFixed(2)} ·&nbsp;animal{" "}
                {aiPresence.pAnimal.toFixed(2)}
              </div>
            )}
          </>
        )}
        {/* Ground truth — manual labels (eval dataset). Checked = image contains the subject. */}
        {meta !== null && (
          <div
            style={{
              display: "flex",
              gap: 16,
              marginTop: 12,
              paddingTop: 10,
              borderTop: "1px solid var(--color-line)",
            }}
          >
            {(
              [
                ["person", "Contains person", labels.containsPerson],
                ["animal", "Contains animal", labels.containsAnimal],
              ] as const
            ).map(([field, lbl, val]) => (
              <label
                key={field}
                style={{
                  display: "inline-flex",
                  alignItems: "center",
                  gap: 6,
                  fontSize: 11.5,
                  color: "var(--color-t2)",
                  cursor: "pointer",
                  userSelect: "none",
                }}
              >
                <input
                  type="checkbox"
                  checked={val === true}
                  onChange={(e) => toggleLabel(field, e.target.checked)}
                />
                {lbl}
              </label>
            ))}
          </div>
        )}
      </div>

      {/* Collections */}
      <div
        style={{
          padding: "14px 16px",
          borderTop: "1px solid var(--color-line)",
        }}
      >
        <div
          style={{
            fontSize: 10.5,
            letterSpacing: ".06em",
            textTransform: "uppercase",
            color: "var(--color-t3)",
            fontWeight: 600,
            marginBottom: 10,
          }}
        >
          Collections
        </div>
        {meta === null ? (
          <div style={{ fontSize: 12, color: "var(--color-t3)" }}>
            No image selected
          </div>
        ) : (
          <>
            <div style={{ display: "flex", flexWrap: "wrap", gap: 6 }}>
              {imageCollections.length === 0 && (
                <span style={{ fontSize: 11.5, color: "var(--color-t3)" }}>
                  Not in any collection
                </span>
              )}
              {imageCollections.map((c) => (
                <span
                  key={c.id}
                  style={{
                    display: "inline-flex",
                    alignItems: "center",
                    gap: 5,
                    fontSize: 11.5,
                    color: "var(--color-t2)",
                    background: "var(--color-elev)",
                    border: "1px solid var(--color-line)",
                    borderRadius: 20,
                    padding: "3px 5px 3px 9px",
                  }}
                >
                  {c.name}
                  <button
                    onClick={() => handlers.onRemoveFromCollection(c.id)}
                    title={`Remove from ${c.name}`}
                    aria-label={`Remove from ${c.name}`}
                    style={{
                      display: "flex",
                      alignItems: "center",
                      justifyContent: "center",
                      width: 14,
                      height: 14,
                      borderRadius: "50%",
                      border: "none",
                      background: "var(--color-line)",
                      color: "var(--color-t1)",
                      fontSize: 11,
                      lineHeight: 1,
                      cursor: "pointer",
                    }}
                  >
                    ×
                  </button>
                </span>
              ))}
            </div>
            {addableCollections.length > 0 && (
              <select
                value=""
                onChange={(e) => {
                  const id = Number(e.target.value);
                  if (id) handlers.onAddToCollection(id);
                }}
                style={{
                  marginTop: 8,
                  width: "100%",
                  background: "var(--color-panel)",
                  border: "1px solid var(--color-line)",
                  borderRadius: "var(--radius-sm)",
                  color: "var(--color-t2)",
                  fontSize: 12,
                  padding: "5px 8px",
                  cursor: "pointer",
                }}
              >
                <option value="">Add to collection…</option>
                {addableCollections.map((c) => (
                  <option key={c.id} value={c.id}>
                    {c.name}
                  </option>
                ))}
              </select>
            )}
          </>
        )}
      </div>
    </aside>
  );
}
