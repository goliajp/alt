import { useState } from "react";
import type {
  DiffBinary,
  DiffPartAware,
  DiffStructured,
  PartChange as PartChangeRecord,
} from "../lib/api";
import { formatBytes } from "../lib/format";
import { DiffView } from "./DiffView";

interface CommonProps {
  repo: string;
}

/** A semantic key-level diff for JSON / TOML files. Each row shows a
 *  jq-style path and how the value at that path changed. */
export function StructuredDiff({
  file,
}: { file: DiffStructured } & Partial<CommonProps>) {
  if (file.paths.length === 0) {
    return (
      <div className="px-4 py-3 font-mono text-sm text-fg-muted">
        no semantic changes
      </div>
    );
  }
  return (
    <div className="font-mono text-[13px] leading-relaxed">
      <div className="px-4 py-2 text-fg-muted bg-canvas-inset/40 border-b border-border-muted text-xs uppercase tracking-[0.18em]">
        {file.format} · {file.paths.length} change
        {file.paths.length === 1 ? "" : "s"}
      </div>
      {file.paths.map((p, i) => (
        <div
          key={i}
          className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-4 px-4 py-2 border-b border-border-muted last:border-b-0"
        >
          <div className="text-fg-default font-semibold">{p.path || "·"}</div>
          <div>
            {p.change === "changed" ? (
              <div className="space-y-1">
                <div className="bg-diff-del-bg text-diff-del-fg px-2 py-0.5 rounded">
                  <span className="select-none opacity-60 mr-2">−</span>
                  {p.old}
                </div>
                <div className="bg-diff-add-bg text-diff-add-fg px-2 py-0.5 rounded">
                  <span className="select-none opacity-60 mr-2">+</span>
                  {p.new}
                </div>
              </div>
            ) : p.change === "added" ? (
              <div className="bg-diff-add-bg text-diff-add-fg px-2 py-0.5 rounded">
                <span className="select-none opacity-60 mr-2">+</span>
                {p.new} <span className="text-xs opacity-60">(added)</span>
              </div>
            ) : (
              <div className="bg-diff-del-bg text-diff-del-fg px-2 py-0.5 rounded">
                <span className="select-none opacity-60 mr-2">−</span>
                {p.old} <span className="text-xs opacity-60">(removed)</span>
              </div>
            )}
          </div>
        </div>
      ))}
    </div>
  );
}

/** Part-aware diff for PNG / ZIP / OOXML. PNGs additionally show the
 *  before/after image + perceptual distance. */
export function PartAwareDiff({
  repo,
  file,
}: CommonProps & { file: DiffPartAware }) {
  const sideBySide = file.format === "png" && file.old_oid && file.new_oid;
  return (
    <div className="font-mono text-[13px] leading-relaxed">
      <div className="px-4 py-2 text-fg-muted bg-canvas-inset/40 border-b border-border-muted text-xs uppercase tracking-[0.18em] flex items-center gap-3 flex-wrap">
        <span>{file.format}</span>
        <span className="text-fg-subtle">·</span>
        <span>
          {formatBytes(file.old_bytes)} → {formatBytes(file.new_bytes)}
        </span>
        {file.perceptual_distance != null ? (
          <>
            <span className="text-fg-subtle">·</span>
            <span title="dHash hamming distance / 64; 0 = identical, 1 = unrelated">
              perceptual Δ {file.perceptual_distance.toFixed(3)}
            </span>
          </>
        ) : null}
      </div>

      {sideBySide ? (
        <div className="grid grid-cols-2 gap-px bg-border-muted">
          <ImageBox
            label="before"
            src={`/api/repos/${repo}/blob/${file.old_oid}/raw`}
            oid={file.old_oid}
          />
          <ImageBox
            label="after"
            src={`/api/repos/${repo}/blob/${file.new_oid}/raw`}
            oid={file.new_oid}
          />
        </div>
      ) : null}

      <div>
        {file.parts.map((part, i) => (
          <PartRow key={i} part={part} />
        ))}
      </div>
    </div>
  );
}

function PartRow({ part }: { part: PartChangeRecord }) {
  const [open, setOpen] = useState(false);
  const expandable = part.change === "changed" && !!part.text_patch;
  const bytesNote =
    part.change === "changed"
      ? `${formatBytes(part.old_bytes ?? 0)} → ${formatBytes(part.new_bytes ?? 0)}`
      : part.change === "added"
        ? `+${formatBytes(part.new_bytes ?? 0)}`
        : part.change === "removed"
          ? `−${formatBytes(part.old_bytes ?? 0)}`
          : "";

  return (
    <div className="border-b border-border-muted last:border-b-0">
      <button
        type="button"
        disabled={!expandable}
        onClick={() => expandable && setOpen((v) => !v)}
        className={`w-full grid grid-cols-[auto_minmax(0,1fr)_auto_auto] gap-x-4 px-4 py-1.5 items-baseline text-left ${
          expandable
            ? "hover:bg-canvas-inset/40 cursor-pointer"
            : "cursor-default"
        }`}
      >
        <PartMarker change={part.change} />
        <span className="text-fg-default truncate">{part.name}</span>
        <span className="text-fg-muted text-xs">{bytesNote}</span>
        {expandable ? (
          <span
            className={`text-xs font-mono transition-transform ${
              open ? "rotate-90 text-warm" : "text-fg-subtle"
            }`}
          >
            ▸
          </span>
        ) : (
          <span />
        )}
      </button>
      {expandable && open ? (
        <div className="border-t border-border-muted bg-canvas-inset/20">
          <DiffView patch={part.text_patch!} path={part.name} />
        </div>
      ) : null}
    </div>
  );
}

/** Opaque-binary diff fallback. */
export function BinaryDiffSummary({
  file,
}: {
  file: DiffBinary;
}) {
  return (
    <div className="px-4 py-6 font-mono text-sm text-fg-muted text-center space-y-1">
      <div>Binary files differ</div>
      <div className="text-xs">
        {formatBytes(file.old_bytes)} → {formatBytes(file.new_bytes)}
      </div>
    </div>
  );
}

function ImageBox({
  label,
  src,
  oid,
}: {
  label: string;
  src: string;
  oid: string;
}) {
  return (
    <div className="bg-canvas-inset/60 p-4 flex flex-col gap-2 items-center">
      <div className="text-[10px] uppercase tracking-[0.22em] font-mono text-fg-subtle flex items-center gap-2">
        {label}
        <span className="text-fg-default font-mono">{oid.slice(0, 7)}</span>
      </div>
      <div className="bg-canvas-inset border border-border-muted rounded p-2 max-w-full overflow-hidden">
        <img
          src={src}
          alt={label}
          loading="lazy"
          className="max-w-full max-h-[480px] object-contain"
        />
      </div>
    </div>
  );
}

function PartMarker({ change }: { change: PartChangeRecord["change"] }) {
  switch (change) {
    case "added":
      return <span className="text-diff-add-fg">+</span>;
    case "removed":
      return <span className="text-diff-del-fg">−</span>;
    case "changed":
      return <span className="text-attention">~</span>;
    case "same":
      return <span className="text-fg-subtle">·</span>;
  }
}
