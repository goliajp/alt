import { useEffect, useId, useRef, useState } from "react";

type Mode = "side" | "swipe" | "onion" | "diff";

interface Props {
  oldSrc: string;
  newSrc: string;
  oldLabel?: string;
  newLabel?: string;
  /** Cap on rendered height — keeps tall art under control. */
  maxHeight?: number;
}

const MODES: Array<{ id: Mode; label: string; hint: string }> = [
  { id: "side", label: "Side by side", hint: "before · after" },
  { id: "swipe", label: "Swipe", hint: "drag the divider" },
  { id: "onion", label: "Onion skin", hint: "opacity slider" },
  { id: "diff", label: "Difference", hint: "pixel-Δ highlight" },
];

/** Compare two images four ways. Mode is local UI state; toggling is
 *  cheap, computing the difference mask happens on a hidden canvas the
 *  first time the user picks that mode. */
export function ImageDiffView({
  oldSrc,
  newSrc,
  oldLabel = "before",
  newLabel = "after",
  maxHeight = 480,
}: Props) {
  const [mode, setMode] = useState<Mode>("swipe");

  return (
    <div className="bg-canvas-inset/40">
      <div className="flex items-center justify-between gap-3 px-4 py-2 border-b border-border-muted">
        <ModeTabs mode={mode} onChange={setMode} />
        <span className="text-[11px] font-mono text-fg-subtle">
          {MODES.find((m) => m.id === mode)?.hint}
        </span>
      </div>
      <div className="p-4">
        {mode === "side" ? (
          <SideBySide
            oldSrc={oldSrc}
            newSrc={newSrc}
            oldLabel={oldLabel}
            newLabel={newLabel}
            maxHeight={maxHeight}
          />
        ) : mode === "swipe" ? (
          <SwipeCompare oldSrc={oldSrc} newSrc={newSrc} maxHeight={maxHeight} />
        ) : mode === "onion" ? (
          <OnionSkin oldSrc={oldSrc} newSrc={newSrc} maxHeight={maxHeight} />
        ) : (
          <DifferenceMask
            oldSrc={oldSrc}
            newSrc={newSrc}
            maxHeight={maxHeight}
          />
        )}
      </div>
    </div>
  );
}

function ModeTabs({
  mode,
  onChange,
}: {
  mode: Mode;
  onChange: (m: Mode) => void;
}) {
  return (
    <div className="inline-flex bg-canvas-inset border border-border-muted rounded-md p-0.5">
      {MODES.map((m) => (
        <button
          key={m.id}
          type="button"
          onClick={() => onChange(m.id)}
          className={`text-[11px] font-mono px-2.5 py-1 rounded transition-colors ${
            mode === m.id
              ? "bg-canvas-subtle text-warm"
              : "text-fg-muted hover:text-fg-default"
          }`}
        >
          {m.label}
        </button>
      ))}
    </div>
  );
}

function SideBySide({
  oldSrc,
  newSrc,
  oldLabel,
  newLabel,
  maxHeight,
}: {
  oldSrc: string;
  newSrc: string;
  oldLabel: string;
  newLabel: string;
  maxHeight: number;
}) {
  return (
    <div className="grid grid-cols-2 gap-3">
      <Frame label={oldLabel}>
        <img
          src={oldSrc}
          alt={oldLabel}
          loading="lazy"
          className="max-w-full object-contain"
          style={{ maxHeight }}
        />
      </Frame>
      <Frame label={newLabel}>
        <img
          src={newSrc}
          alt={newLabel}
          loading="lazy"
          className="max-w-full object-contain"
          style={{ maxHeight }}
        />
      </Frame>
    </div>
  );
}

function Frame({
  label,
  children,
}: {
  label: string;
  children: React.ReactNode;
}) {
  return (
    <div className="flex flex-col items-center gap-2">
      <span className="text-[10px] uppercase tracking-[0.22em] font-mono text-fg-subtle">
        {label}
      </span>
      <div className="bg-canvas-inset border border-border-muted rounded p-2 flex justify-center w-full">
        {children}
      </div>
    </div>
  );
}

/** Draggable vertical divider over a stack — left half is the new image,
 *  right half is the old. Common "before/after slider" pattern. */
function SwipeCompare({
  oldSrc,
  newSrc,
  maxHeight,
}: {
  oldSrc: string;
  newSrc: string;
  maxHeight: number;
}) {
  const [pos, setPos] = useState(50);
  const wrapRef = useRef<HTMLDivElement>(null);
  const drag = useRef(false);

  const onPointerDown = (e: React.PointerEvent) => {
    drag.current = true;
    (e.target as HTMLElement).setPointerCapture(e.pointerId);
    update(e);
  };
  const onPointerMove = (e: React.PointerEvent) => {
    if (drag.current) update(e);
  };
  const onPointerUp = (e: React.PointerEvent) => {
    drag.current = false;
    (e.target as HTMLElement).releasePointerCapture(e.pointerId);
  };
  const update = (e: React.PointerEvent) => {
    const rect = wrapRef.current?.getBoundingClientRect();
    if (!rect) return;
    const x = (e.clientX - rect.left) / rect.width;
    setPos(Math.max(0, Math.min(1, x)) * 100);
  };

  return (
    <div
      ref={wrapRef}
      className="relative bg-canvas-inset border border-border-muted rounded overflow-hidden select-none touch-none"
      style={{ minHeight: 200 }}
    >
      <img
        src={oldSrc}
        alt="before"
        loading="lazy"
        className="block w-full object-contain"
        style={{ maxHeight }}
      />
      <img
        src={newSrc}
        alt="after"
        loading="lazy"
        className="block w-full object-contain absolute inset-0"
        style={{
          maxHeight,
          clipPath: `inset(0 0 0 ${pos}%)`,
        }}
      />
      {/* divider */}
      <div
        className="absolute top-0 bottom-0 pointer-events-none"
        style={{
          left: `${pos}%`,
          width: 0,
          borderLeft: "2px solid var(--color-warm)",
          boxShadow: "0 0 8px rgba(232,168,124,0.45)",
        }}
      />
      <div
        role="slider"
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={Math.round(pos)}
        tabIndex={0}
        className="absolute top-1/2 -translate-x-1/2 -translate-y-1/2 w-9 h-9 rounded-full bg-warm text-canvas-inset cursor-ew-resize flex items-center justify-center font-mono text-sm shadow-lg"
        style={{ left: `${pos}%` }}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerUp}
      >
        ⇆
      </div>
      <div className="absolute top-2 left-2 text-[10px] uppercase tracking-[0.22em] font-mono text-fg-default bg-canvas-inset/80 rounded px-1.5 py-0.5">
        before
      </div>
      <div className="absolute top-2 right-2 text-[10px] uppercase tracking-[0.22em] font-mono text-fg-default bg-canvas-inset/80 rounded px-1.5 py-0.5">
        after
      </div>
    </div>
  );
}

/** Overlay both images and let the user crossfade with an opacity slider. */
function OnionSkin({
  oldSrc,
  newSrc,
  maxHeight,
}: {
  oldSrc: string;
  newSrc: string;
  maxHeight: number;
}) {
  const [alpha, setAlpha] = useState(0.5);
  return (
    <div>
      <div
        className="relative bg-canvas-inset border border-border-muted rounded overflow-hidden"
        style={{ minHeight: 200 }}
      >
        <img
          src={oldSrc}
          alt="before"
          loading="lazy"
          className="block w-full object-contain"
          style={{ maxHeight }}
        />
        <img
          src={newSrc}
          alt="after"
          loading="lazy"
          className="block w-full object-contain absolute inset-0"
          style={{ maxHeight, opacity: alpha }}
        />
      </div>
      <div className="flex items-center gap-3 mt-3 px-1">
        <span className="text-[10px] uppercase tracking-[0.22em] font-mono text-fg-subtle w-12">
          before
        </span>
        <input
          type="range"
          min={0}
          max={1}
          step={0.01}
          value={alpha}
          onChange={(e) => setAlpha(parseFloat(e.target.value))}
          className="flex-1 accent-warm"
        />
        <span className="text-[10px] uppercase tracking-[0.22em] font-mono text-fg-subtle w-12 text-right">
          after
        </span>
        <span className="text-[11px] font-mono text-fg-muted w-10 text-right">
          {Math.round(alpha * 100)}%
        </span>
      </div>
    </div>
  );
}

/** Canvas-computed per-pixel difference mask: load both images into an
 *  off-screen canvas, compute the per-channel max delta, and draw a
 *  transparent black background with the warm accent painted where the
 *  delta exceeds a threshold. Anything over `threshold` shows up as
 *  the bright "what changed" overlay; everything else stays dim. */
function DifferenceMask({
  oldSrc,
  newSrc,
  maxHeight,
}: {
  oldSrc: string;
  newSrc: string;
  maxHeight: number;
}) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const [size, setSize] = useState<{ w: number; h: number } | null>(null);
  const [stats, setStats] = useState<{ changed: number; total: number } | null>(
    null,
  );
  const [threshold, setThreshold] = useState(16);
  const [showOriginal, setShowOriginal] = useState(true);
  const id = useId();

  const imgsRef = useRef<{ old: HTMLImageElement; new: HTMLImageElement } | null>(
    null,
  );

  useEffect(() => {
    let cancelled = false;
    const oldImg = new window.Image();
    const newImg = new window.Image();
    oldImg.crossOrigin = "anonymous";
    newImg.crossOrigin = "anonymous";
    const onLoad = () => {
      if (cancelled) return;
      if (!oldImg.complete || !newImg.complete) return;
      const w = Math.max(oldImg.naturalWidth, newImg.naturalWidth);
      const h = Math.max(oldImg.naturalHeight, newImg.naturalHeight);
      imgsRef.current = { old: oldImg, new: newImg };
      setSize({ w, h });
    };
    oldImg.addEventListener("load", onLoad);
    newImg.addEventListener("load", onLoad);
    oldImg.src = oldSrc;
    newImg.src = newSrc;
    return () => {
      cancelled = true;
      oldImg.removeEventListener("load", onLoad);
      newImg.removeEventListener("load", onLoad);
    };
  }, [oldSrc, newSrc]);

  useEffect(() => {
    if (!size) return;
    const imgs = imgsRef.current;
    if (!imgs) return;
    const canvas = canvasRef.current;
    if (!canvas) return;
    canvas.width = size.w;
    canvas.height = size.h;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    const { old: oldImg, new: newImg } = imgs;

    if (showOriginal) {
      ctx.fillStyle = "#0d1117";
      ctx.fillRect(0, 0, size.w, size.h);
      ctx.globalAlpha = 0.45;
      ctx.drawImage(newImg, 0, 0, size.w, size.h);
      ctx.globalAlpha = 1;
    } else {
      ctx.fillStyle = "#0d1117";
      ctx.fillRect(0, 0, size.w, size.h);
    }

    // Step 2: rasterise both images into auxiliary off-screen contexts
    // at the comparison size, then walk pixels.
    const oldCanvas = document.createElement("canvas");
    const newCanvas = document.createElement("canvas");
    oldCanvas.width = size.w;
    oldCanvas.height = size.h;
    newCanvas.width = size.w;
    newCanvas.height = size.h;
    const oldCtx = oldCanvas.getContext("2d");
    const newCtx = newCanvas.getContext("2d");
    if (!oldCtx || !newCtx) return;
    oldCtx.drawImage(oldImg, 0, 0, size.w, size.h);
    newCtx.drawImage(newImg, 0, 0, size.w, size.h);
    let oldData: ImageData;
    let newData: ImageData;
    try {
      oldData = oldCtx.getImageData(0, 0, size.w, size.h);
      newData = newCtx.getImageData(0, 0, size.w, size.h);
    } catch {
      // Tainted canvas — fall back to "couldn't compute". Defer the
      // state update so React 19's "no setState in effect" check is
      // satisfied (the stats are derived from the effect's work, not
      // an input prop).
      queueMicrotask(() => setStats(null));
      return;
    }

    const overlay = ctx.createImageData(size.w, size.h);
    const ov = overlay.data;
    const o = oldData.data;
    const n = newData.data;
    let changed = 0;
    const total = size.w * size.h;
    // accent: --color-warm = #e8a87c → 232,168,124
    for (let i = 0; i < o.length; i += 4) {
      const dr = Math.abs(o[i] - n[i]);
      const dg = Math.abs(o[i + 1] - n[i + 1]);
      const db = Math.abs(o[i + 2] - n[i + 2]);
      const delta = Math.max(dr, dg, db);
      if (delta >= threshold) {
        const strength = Math.min(1, delta / 128);
        ov[i] = 232;
        ov[i + 1] = 168;
        ov[i + 2] = 124;
        ov[i + 3] = Math.round(180 + 60 * strength); // 180..240
        changed += 1;
      } else {
        ov[i + 3] = 0;
      }
    }
    ctx.putImageData(overlay, 0, 0);
    queueMicrotask(() => setStats({ changed, total }));
  }, [size, threshold, showOriginal]);

  return (
    <div>
      <div className="bg-canvas-inset border border-border-muted rounded p-2 flex justify-center">
        <canvas
          ref={canvasRef}
          className="max-w-full"
          style={{ maxHeight, imageRendering: "auto" }}
        />
      </div>
      <div className="mt-3 grid grid-cols-1 sm:grid-cols-[1fr_auto] gap-3 items-center text-xs font-mono text-fg-muted">
        <div className="flex items-center gap-3">
          <label htmlFor={`${id}-threshold`} className="shrink-0">
            threshold
          </label>
          <input
            id={`${id}-threshold`}
            type="range"
            min={0}
            max={64}
            step={1}
            value={threshold}
            onChange={(e) => setThreshold(parseInt(e.target.value, 10))}
            className="flex-1 accent-warm min-w-32"
          />
          <span className="w-8 text-right">{threshold}</span>
          <label className="flex items-center gap-1.5 ml-3 cursor-pointer">
            <input
              type="checkbox"
              checked={showOriginal}
              onChange={(e) => setShowOriginal(e.target.checked)}
              className="accent-warm"
            />
            backdrop
          </label>
        </div>
        <div className="text-right">
          {stats ? (
            <span>
              {((100 * stats.changed) / stats.total).toFixed(2)}% pixels Δ ·{" "}
              {stats.changed.toLocaleString()} / {stats.total.toLocaleString()}
            </span>
          ) : (
            <span className="text-fg-subtle">computing…</span>
          )}
        </div>
      </div>
    </div>
  );
}
