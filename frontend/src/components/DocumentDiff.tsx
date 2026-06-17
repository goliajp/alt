import { useState } from "react";
import type { DocumentDiff as DocumentDiffData } from "../lib/api";
import { XlsxGridDiff } from "./XlsxGridDiff";

/** Format-native content diff for OOXML files. Each variant gets its
 *  own renderer designed for how a human actually reads that format:
 *  paragraphs for .docx, an Excel-style grid for .xlsx. */
export function DocumentDiff({ data }: { data: DocumentDiffData }) {
  if (data.kind === "docx") {
    return <DocxDiff entries={data.entries} />;
  }
  return <XlsxGridDiff sheets={data.sheets} />;
}

function DocxDiff({
  entries,
}: {
  entries: { change: string; text: string }[];
}) {
  const [showContext, setShowContext] = useState(false);
  const added = entries.filter((e) => e.change === "added").length;
  const removed = entries.filter((e) => e.change === "removed").length;

  const view = showContext ? entries : trimContext(entries, 1);

  if (entries.length === 0 || (added === 0 && removed === 0)) {
    return null;
  }

  return (
    <div className="border-b border-border-default">
      <div className="px-4 py-2.5 bg-canvas-inset/40 border-b border-border-muted flex items-center justify-between gap-3 flex-wrap">
        <div className="flex items-center gap-3 text-sm">
          <span className="text-[10px] uppercase tracking-[0.22em] font-mono text-warm">
            document
          </span>
          <span className="text-fg-default">
            {added > 0 ? (
              <>
                <span className="text-diff-add-fg font-medium">+{added}</span>{" "}
                paragraph{added === 1 ? "" : "s"}
                {removed > 0 ? ", " : " "}
              </>
            ) : null}
            {removed > 0 ? (
              <>
                <span className="text-diff-del-fg font-medium">−{removed}</span>{" "}
                paragraph{removed === 1 ? "" : "s"}
              </>
            ) : null}
          </span>
        </div>
        <label className="text-xs font-mono text-fg-muted flex items-center gap-1.5 cursor-pointer">
          <input
            type="checkbox"
            checked={showContext}
            onChange={(e) => setShowContext(e.target.checked)}
            className="accent-warm"
          />
          show all context
        </label>
      </div>
      <ol className="divide-y divide-border-muted">
        {view.map((entry, i) => (
          <DocxRow key={i} entry={entry} />
        ))}
      </ol>
    </div>
  );
}

function DocxRow({ entry }: { entry: { change: string; text: string } }) {
  if (entry.change === "ellipsis") {
    return (
      <li className="px-4 py-1.5 text-fg-subtle font-mono text-xs text-center bg-canvas-inset/30">
        ⋮ {entry.text}
      </li>
    );
  }
  const bg =
    entry.change === "added"
      ? "bg-diff-add-bg/30"
      : entry.change === "removed"
        ? "bg-diff-del-bg/30"
        : "";
  const marker =
    entry.change === "added" ? "+" : entry.change === "removed" ? "−" : " ";
  const markerCls =
    entry.change === "added"
      ? "text-diff-add-fg"
      : entry.change === "removed"
        ? "text-diff-del-fg"
        : "text-fg-subtle";
  return (
    <li className={`px-4 py-2 ${bg} flex items-start gap-3`}>
      <span
        className={`shrink-0 font-mono text-base leading-tight w-3 text-center ${markerCls}`}
      >
        {marker}
      </span>
      <span className="flex-1 min-w-0 text-fg-default whitespace-pre-wrap break-words leading-relaxed">
        {entry.text || (
          <span className="text-fg-subtle italic">(empty paragraph)</span>
        )}
      </span>
    </li>
  );
}

function trimContext(
  entries: { change: string; text: string }[],
  radius: number,
): { change: string; text: string }[] {
  const keep = new Array(entries.length).fill(false);
  entries.forEach((e, i) => {
    if (e.change !== "same") {
      for (
        let j = Math.max(0, i - radius);
        j <= Math.min(entries.length - 1, i + radius);
        j++
      ) {
        keep[j] = true;
      }
    }
  });
  const out: { change: string; text: string }[] = [];
  let skipped = 0;
  for (let i = 0; i < entries.length; i++) {
    if (keep[i]) {
      if (skipped > 0) {
        out.push({
          change: "ellipsis",
          text: `${skipped} unchanged`,
        });
        skipped = 0;
      }
      out.push(entries[i]);
    } else {
      skipped++;
    }
  }
  if (skipped > 0) {
    out.push({ change: "ellipsis", text: `${skipped} unchanged` });
  }
  return out;
}
