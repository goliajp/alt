import { useEffect, useMemo, useState } from "react";
import { highlight, tokenStyle } from "../lib/highlight";
import { detectLang } from "../lib/lang";

interface HunkHeader {
  oldStart: number;
  newStart: number;
  text: string;
}

interface Hunk {
  header: HunkHeader;
  /** One row per non-header diff line. */
  rows: Row[];
}

type Row =
  | { kind: "ctx"; old: number; new: number; text: string }
  | { kind: "add"; old: null; new: number; text: string }
  | { kind: "del"; old: number; new: null; text: string }
  | { kind: "binary"; text: string };

interface Parsed {
  /** Optional `--- a/path` / `+++ b/path` lines preserved verbatim. */
  fileHeaders: string[];
  hunks: Hunk[];
  binary: boolean;
}

function parsePatch(patch: string): Parsed {
  const fileHeaders: string[] = [];
  const hunks: Hunk[] = [];
  let current: Hunk | null = null;
  let oldLine = 0;
  let newLine = 0;
  let binary = false;

  for (const raw of patch.split("\n")) {
    if (raw === "" && !current && fileHeaders.length === 0) continue;
    if (raw.startsWith("--- ") || raw.startsWith("+++ ")) {
      fileHeaders.push(raw);
      continue;
    }
    if (raw.startsWith("@@")) {
      const m = raw.match(/^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/);
      const header: HunkHeader = {
        oldStart: m ? parseInt(m[1], 10) : 1,
        newStart: m ? parseInt(m[2], 10) : 1,
        text: raw,
      };
      oldLine = header.oldStart;
      newLine = header.newStart;
      current = { header, rows: [] };
      hunks.push(current);
      continue;
    }
    if (raw.startsWith("Binary files")) {
      binary = true;
      if (!current) {
        current = {
          header: { oldStart: 0, newStart: 0, text: "" },
          rows: [],
        };
        hunks.push(current);
      }
      current.rows.push({ kind: "binary", text: raw });
      continue;
    }
    if (!current) {
      // diff lines outside any hunk — skip; the file headers already
      // captured the file identity.
      continue;
    }
    if (raw.startsWith("+")) {
      current.rows.push({
        kind: "add",
        old: null,
        new: newLine,
        text: raw.slice(1),
      });
      newLine += 1;
    } else if (raw.startsWith("-")) {
      current.rows.push({
        kind: "del",
        old: oldLine,
        new: null,
        text: raw.slice(1),
      });
      oldLine += 1;
    } else if (raw.startsWith(" ")) {
      current.rows.push({
        kind: "ctx",
        old: oldLine,
        new: newLine,
        text: raw.slice(1),
      });
      oldLine += 1;
      newLine += 1;
    } else if (raw) {
      // Stray non-empty line — treat as context, don't advance.
      current.rows.push({
        kind: "ctx",
        old: oldLine,
        new: newLine,
        text: raw,
      });
    }
  }
  return { fileHeaders, hunks, binary };
}

interface Props {
  patch: string;
  /** File path used for syntax-highlight language detection. */
  path: string;
}

/** A GitHub-style unified-diff renderer with two line-number gutters
 *  (old | new) and per-line shiki syntax highlighting where possible.
 */
export function DiffView({ patch, path }: Props) {
  const parsed = useMemo(() => parsePatch(patch), [patch]);
  const lang = useMemo(() => detectLang(path), [path]);

  // Highlight every row's text up front, indexed by hunk + row.
  // `lines` parallels parsed.hunks: lines[h][r] gives the styled
  // tokens for that row. We pull this with a single useEffect so the
  // call doesn't block the first paint — until it lands, rows render
  // as plain text.
  const [styled, setStyled] = useState<TokenLine[][] | null>(null);
  useEffect(() => {
    let cancel = false;
    const all: string[][] = parsed.hunks.map((h) =>
      h.rows.map((r) => (r.kind === "binary" ? r.text : r.text)),
    );
    const flat = all.flat();
    highlight(flat.join("\n"), lang)
      .then((lines) => {
        if (cancel) return;
        // Restructure flat back into hunk groups.
        let idx = 0;
        const grouped: TokenLine[][] = parsed.hunks.map((h) => {
          const out: TokenLine[] = [];
          for (let i = 0; i < h.rows.length; i++) {
            out.push({ tokens: lines[idx]?.tokens ?? [] });
            idx += 1;
          }
          return out;
        });
        setStyled(grouped);
      })
      .catch(() => {
        /* leave styled null → fall back to plain text */
      });
    return () => {
      cancel = true;
    };
  }, [parsed, lang]);

  const maxLine = parsed.hunks.reduce((acc, h) => {
    for (const r of h.rows) {
      if (r.kind === "ctx" || r.kind === "del")
        acc = Math.max(acc, r.old ?? 0);
      if (r.kind === "ctx" || r.kind === "add")
        acc = Math.max(acc, r.new ?? 0);
    }
    return acc;
  }, 0);
  const gutterCh = `${Math.max(2, String(maxLine).length)}ch`;

  return (
    <div className="font-mono text-[13px] leading-relaxed overflow-x-auto bg-canvas-inset/40">
      {parsed.binary ? (
        <div className="px-4 py-3 text-attention">Binary files differ</div>
      ) : (
        parsed.hunks.map((hunk, hi) => (
          <div key={hi}>
            <div className="px-4 py-1 text-diff-meta-fg bg-[rgba(47,129,247,0.08)] border-y border-border-muted">
              {hunk.header.text || " "}
            </div>
            {hunk.rows.map((row, ri) => (
              <DiffRow
                key={ri}
                row={row}
                gutterCh={gutterCh}
                styledTokens={styled?.[hi]?.[ri]?.tokens}
              />
            ))}
          </div>
        ))
      )}
    </div>
  );
}

interface TokenLine {
  tokens: Array<{ content: string; color?: string; fontStyle?: number }>;
}

function DiffRow({
  row,
  gutterCh,
  styledTokens,
}: {
  row: Row;
  gutterCh: string;
  styledTokens?: TokenLine["tokens"];
}) {
  if (row.kind === "binary") {
    return <div className="px-4 py-0.5 text-attention">{row.text}</div>;
  }

  let rowBg = "";
  let oldNum: string = "";
  let newNum: string = "";
  let marker: string = " ";
  let markerCls = "text-fg-subtle";

  switch (row.kind) {
    case "add":
      rowBg = "bg-diff-add-bg text-diff-add-fg";
      newNum = String(row.new);
      marker = "+";
      markerCls = "text-diff-add-fg/70";
      break;
    case "del":
      rowBg = "bg-diff-del-bg text-diff-del-fg";
      oldNum = String(row.old);
      marker = "−";
      markerCls = "text-diff-del-fg/70";
      break;
    case "ctx":
      oldNum = String(row.old);
      newNum = String(row.new);
      break;
  }

  return (
    <div className={`flex ${rowBg}`}>
      <span
        aria-hidden
        style={{ minWidth: gutterCh }}
        className="select-none shrink-0 text-right pr-2 pl-3 text-fg-subtle/80"
      >
        {oldNum}
      </span>
      <span
        aria-hidden
        style={{ minWidth: gutterCh }}
        className="select-none shrink-0 text-right pr-3 text-fg-subtle/80 border-r border-border-muted"
      >
        {newNum}
      </span>
      <span
        aria-hidden
        className={`select-none shrink-0 px-3 ${markerCls}`}
      >
        {marker}
      </span>
      <span className="flex-1 pr-4 whitespace-pre">
        {styledTokens && styledTokens.length > 0
          ? styledTokens.map((t, j) => (
              <span key={j} style={tokenStyle(t as never)}>
                {t.content}
              </span>
            ))
          : row.text || " "}
      </span>
    </div>
  );
}
