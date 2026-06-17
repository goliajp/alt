import { useState } from "react";
import type { SheetGrid, SheetCell } from "../lib/api";

/** Render an .xlsx diff the way reviewers actually look at spreadsheets:
 *  a grid of cells with column letters and row numbers, the way Excel
 *  draws them. Changed cells show old → new in place; added cells get a
 *  green tint; removed cells get a red tint with a strike-through.
 *
 *  This is the "format-native" diff for spreadsheets — a flat list of
 *  `Sheet!Ref: value` rows is what git-style line tools would give. Real
 *  Excel review is a grid, so we render a grid. */
export function XlsxGridDiff({ sheets }: { sheets: SheetGrid[] }) {
  const changedSheets = sheets.filter((s) => s.has_changes);
  if (changedSheets.length === 0) return null;
  return (
    <div className="border-b border-border-default divide-y divide-border-default">
      {changedSheets.map((sheet) => (
        <SheetView key={sheet.name} sheet={sheet} />
      ))}
    </div>
  );
}

function SheetView({ sheet }: { sheet: SheetGrid }) {
  const [showAll, setShowAll] = useState(false);
  const added = sheet.cells.filter((c) => c.change === "added").length;
  const removed = sheet.cells.filter((c) => c.change === "removed").length;
  const changed = sheet.cells.filter((c) => c.change === "changed").length;

  // For very wide / tall sheets, default-trim to "rows that touched a
  // change" only — reviewer can flip the toggle to see every row.
  const dirtyRows = new Set<number>();
  sheet.cells.forEach((c) => {
    if (c.change !== "same") {
      dirtyRows.add(c.row);
    }
  });
  const rows = rangeRows(sheet, showAll, dirtyRows);
  const cols = rangeCols(sheet);
  const cellMap = new Map<string, SheetCell>();
  sheet.cells.forEach((c) => cellMap.set(c.ref, c));

  return (
    <div className="bg-canvas-subtle">
      <div className="px-4 py-2.5 bg-canvas-inset/40 border-b border-border-muted flex items-center justify-between gap-3 flex-wrap">
        <div className="flex items-center gap-3">
          <span className="text-[10px] uppercase tracking-[0.22em] font-mono text-warm">
            sheet
          </span>
          <span className="text-fg-default font-medium">{sheet.name}</span>
          <span className="text-fg-subtle text-xs font-mono">
            {sheet.max_col}
            {sheet.max_row}
          </span>
        </div>
        <div className="flex items-center gap-3 text-xs font-mono">
          {added > 0 ? (
            <span className="text-diff-add-fg">+{added}</span>
          ) : null}
          {changed > 0 ? <span className="text-warm">~{changed}</span> : null}
          {removed > 0 ? (
            <span className="text-diff-del-fg">−{removed}</span>
          ) : null}
          <label className="text-fg-muted flex items-center gap-1.5 cursor-pointer">
            <input
              type="checkbox"
              checked={showAll}
              onChange={(e) => setShowAll(e.target.checked)}
              className="accent-warm"
            />
            full sheet
          </label>
        </div>
      </div>
      <div className="overflow-x-auto">
        <table className="border-collapse font-mono text-xs">
          <thead>
            <tr>
              <th className="bg-canvas-inset/60 border border-border-muted px-2 py-1 sticky left-0 z-10"></th>
              {cols.map((c) => (
                <th
                  key={c}
                  className="bg-canvas-inset/60 border border-border-muted px-3 py-1 text-fg-muted text-[10px] uppercase tracking-[0.18em] min-w-[80px]"
                >
                  {c}
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {rows.map((row) =>
              row === "ellipsis" ? (
                <tr key="ell">
                  <td
                    colSpan={cols.length + 1}
                    className="text-center text-fg-subtle text-[10px] py-1 bg-canvas-inset/20 border border-border-muted"
                  >
                    ⋮ rows omitted (toggle full sheet to expand)
                  </td>
                </tr>
              ) : (
                <tr key={row}>
                  <th className="bg-canvas-inset/60 border border-border-muted px-2 py-1 text-fg-muted text-[10px] sticky left-0 z-10">
                    {row}
                  </th>
                  {cols.map((c) => {
                    const ref = `${c}${row}`;
                    const cell = cellMap.get(ref);
                    return <Cell key={ref} cell={cell} />;
                  })}
                </tr>
              ),
            )}
          </tbody>
        </table>
      </div>
    </div>
  );
}

function Cell({ cell }: { cell: SheetCell | undefined }) {
  if (!cell) {
    return <td className="border border-border-muted px-3 py-1.5 min-w-[80px]" />;
  }
  if (cell.change === "same") {
    return (
      <td className="border border-border-muted px-3 py-1.5 min-w-[80px] text-fg-default">
        {cell.old ?? cell.new ?? ""}
      </td>
    );
  }
  if (cell.change === "added") {
    return (
      <td className="border border-diff-add-fg/50 bg-diff-add-bg/40 px-3 py-1.5 min-w-[80px] text-diff-add-fg">
        {cell.new ?? ""}
      </td>
    );
  }
  if (cell.change === "removed") {
    return (
      <td className="border border-diff-del-fg/50 bg-diff-del-bg/40 px-3 py-1.5 min-w-[80px] text-diff-del-fg line-through">
        {cell.old ?? ""}
      </td>
    );
  }
  // changed: show old → new in place
  return (
    <td className="border border-warm/60 bg-warm/10 px-3 py-1.5 min-w-[80px]">
      <div className="text-fg-subtle line-through text-[11px] leading-tight">
        {cell.old ?? ""}
      </div>
      <div className="text-warm text-[12px] leading-tight font-medium">
        {cell.new ?? ""}
      </div>
    </td>
  );
}

function rangeCols(sheet: SheetGrid): string[] {
  const max = colLetterToNum(sheet.max_col);
  const out: string[] = [];
  for (let i = 1; i <= max; i++) out.push(colNumToLetter(i));
  return out;
}

function rangeRows(
  sheet: SheetGrid,
  showAll: boolean,
  dirty: Set<number>,
): (number | "ellipsis")[] {
  if (showAll) {
    return Array.from({ length: sheet.max_row }, (_, i) => i + 1);
  }
  // Trim to dirty rows ± 1 context. Long unchanged runs collapse into
  // a single ellipsis marker.
  const keep = new Set<number>();
  // Always keep header row 1 so the reader can see column meanings.
  keep.add(1);
  dirty.forEach((r) => {
    for (let j = Math.max(1, r - 1); j <= Math.min(sheet.max_row, r + 1); j++) {
      keep.add(j);
    }
  });
  const out: (number | "ellipsis")[] = [];
  let last = 0;
  for (let r = 1; r <= sheet.max_row; r++) {
    if (keep.has(r)) {
      if (last !== 0 && r > last + 1) out.push("ellipsis");
      out.push(r);
      last = r;
    }
  }
  return out;
}

function colLetterToNum(s: string): number {
  let n = 0;
  for (const ch of s) n = n * 26 + (ch.charCodeAt(0) - 64);
  return n;
}

function colNumToLetter(n: number): string {
  let s = "";
  while (n > 0) {
    const rem = (n - 1) % 26;
    s = String.fromCharCode(65 + rem) + s;
    n = Math.floor((n - 1) / 26);
  }
  return s;
}
