import { useState } from "react";
import type { DocumentDiff as DocumentDiffData } from "../lib/api";

/** Reviewer-friendly content diff for OOXML files. Shows one row per
 *  paragraph (.docx) or per cell (.xlsx), with added/removed marked
 *  inline like a GitHub PR review — the reader can answer "what changed
 *  for someone opening this file in Word/Excel" without ever seeing the
 *  underlying XML.
 *
 *  Defaults to "review" view: skip same-paragraph context rows that
 *  aren't adjacent to a change. A toggle drops back to showing every
 *  row so the reviewer can verify position. */
export function DocumentDiff({ data }: { data: DocumentDiffData }) {
  const [showContext, setShowContext] = useState(false);
  const added = data.entries.filter((e) => e.change === "added").length;
  const removed = data.entries.filter((e) => e.change === "removed").length;
  const unit = data.kind === "docx" ? "paragraph" : "cell";
  const plural = (n: number) => (n === 1 ? unit : `${unit}s`);

  const view = showContext ? data.entries : trimContext(data.entries, 1);

  if (data.entries.length === 0 || (added === 0 && removed === 0)) {
    return null;
  }

  return (
    <div className="border-b border-border-default">
      <div className="px-4 py-2.5 bg-canvas-inset/40 border-b border-border-muted flex items-center justify-between gap-3 flex-wrap">
        <div className="flex items-center gap-3 text-sm">
          <span className="text-[10px] uppercase tracking-[0.22em] font-mono text-warm">
            {data.kind === "docx" ? "document" : "spreadsheet"}
          </span>
          <span className="text-fg-default">
            {added > 0 ? (
              <>
                <span className="text-diff-add-fg font-medium">+{added}</span>{" "}
                {plural(added)}
                {removed > 0 ? ", " : " "}
              </>
            ) : null}
            {removed > 0 ? (
              <>
                <span className="text-diff-del-fg font-medium">−{removed}</span>{" "}
                {plural(removed)}
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
          <DocRow key={i} entry={entry} kind={data.kind} />
        ))}
      </ol>
    </div>
  );
}

function DocRow({
  entry,
  kind,
}: {
  entry: { change: string; text: string };
  kind: "docx" | "xlsx";
}) {
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
      {kind === "xlsx" ? (
        <CellRow text={entry.text} />
      ) : (
        <span className="flex-1 min-w-0 text-fg-default whitespace-pre-wrap break-words leading-relaxed">
          {entry.text || (
            <span className="text-fg-subtle italic">(empty paragraph)</span>
          )}
        </span>
      )}
    </li>
  );
}

/** xlsx rows come in as `Sheet!Ref: value`; render the prefix in a
 *  subdued tone so the actual value stands out. */
function CellRow({ text }: { text: string }) {
  const sep = text.indexOf(": ");
  if (sep === -1) {
    return (
      <span className="flex-1 min-w-0 text-fg-default font-mono break-words">
        {text}
      </span>
    );
  }
  return (
    <span className="flex-1 min-w-0 break-words leading-relaxed">
      <span className="font-mono text-fg-muted">{text.slice(0, sep)}</span>
      <span className="text-fg-muted">: </span>
      <span className="text-fg-default">{text.slice(sep + 2)}</span>
    </span>
  );
}

/** Keep only same-rows that are within `radius` of a change, replacing
 *  longer same-runs with one ellipsis row so the reviewer doesn't have
 *  to scroll past unchanged paragraphs. */
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
