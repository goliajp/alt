import { useMemo } from "react";

type Line =
  | { kind: "header"; text: string }
  | { kind: "hunk"; text: string }
  | { kind: "add"; text: string }
  | { kind: "del"; text: string }
  | { kind: "ctx"; text: string };

function parsePatch(patch: string): Line[] {
  const out: Line[] = [];
  for (const raw of patch.split("\n")) {
    if (raw === "" && out.length === 0) continue;
    if (raw.startsWith("--- ") || raw.startsWith("+++ ")) {
      out.push({ kind: "header", text: raw });
    } else if (raw.startsWith("@@")) {
      out.push({ kind: "hunk", text: raw });
    } else if (raw.startsWith("+")) {
      out.push({ kind: "add", text: raw.slice(1) });
    } else if (raw.startsWith("-")) {
      out.push({ kind: "del", text: raw.slice(1) });
    } else if (raw.startsWith(" ")) {
      out.push({ kind: "ctx", text: raw.slice(1) });
    } else if (raw) {
      // Binary marker line or other free-form note.
      out.push({ kind: "ctx", text: raw });
    }
  }
  return out;
}

interface Props {
  patch: string;
}

/** Render a unified-diff blob with +/- color rows, mono everywhere. */
export function DiffView({ patch }: Props) {
  const lines = useMemo(() => parsePatch(patch), [patch]);
  return (
    <div className="font-mono text-[13px] leading-relaxed overflow-x-auto">
      {lines.map((line, i) => {
        const text = line.text === "" ? " " : line.text;
        switch (line.kind) {
          case "header":
            return (
              <div key={i} className="px-4 py-0.5 text-diff-meta-fg">
                {text}
              </div>
            );
          case "hunk":
            return (
              <div
                key={i}
                className="px-4 py-1 text-diff-meta-fg bg-[rgba(47,129,247,0.07)] border-y border-border-muted"
              >
                {text}
              </div>
            );
          case "add":
            return (
              <div
                key={i}
                className="px-4 py-0.5 bg-diff-add-bg text-diff-add-fg"
              >
                <span className="select-none mr-3 opacity-60">+</span>
                {text}
              </div>
            );
          case "del":
            return (
              <div
                key={i}
                className="px-4 py-0.5 bg-diff-del-bg text-diff-del-fg"
              >
                <span className="select-none mr-3 opacity-60">−</span>
                {text}
              </div>
            );
          case "ctx":
            return (
              <div key={i} className="px-4 py-0.5 text-fg-default">
                <span className="select-none mr-3 text-fg-subtle">·</span>
                {text}
              </div>
            );
        }
      })}
    </div>
  );
}
