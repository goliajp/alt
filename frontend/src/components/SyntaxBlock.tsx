import { useQuery } from "@tanstack/react-query";
import { highlight, tokenStyle } from "../lib/highlight";

interface Props {
  /** Source code to render. */
  code: string;
  /** Shiki language id, or null for plaintext. */
  lang: string | null;
  /** When true, render a line-number gutter on the left. */
  lineNumbers?: boolean;
}

/** Render a code block with shiki syntax highlighting and an optional
 *  line-number gutter. Falls back to plain monospace text while shiki
 *  is loading or unavailable. */
export function SyntaxBlock({ code, lang, lineNumbers = true }: Props) {
  const lines = useQuery({
    queryKey: ["highlight", lang, code] as const,
    queryFn: () => highlight(code, lang),
    staleTime: Infinity,
  });

  const fallbackLines = code.split("\n");
  const total = lines.data?.length ?? fallbackLines.length;
  const gutterWidth = `${Math.max(2, String(total).length)}ch`;

  return (
    <pre className="font-mono text-[13px] leading-relaxed overflow-x-auto bg-canvas-inset/40">
      <code className="block min-w-full">
        {lines.data
          ? lines.data.map((line, i) => (
              <div key={i} className="flex">
                {lineNumbers ? (
                  <span
                    aria-hidden
                    style={{ minWidth: gutterWidth }}
                    className="select-none shrink-0 text-right pr-4 pl-4 text-fg-subtle/70 border-r border-border-muted"
                  >
                    {i + 1}
                  </span>
                ) : null}
                <span className="flex-1 px-4 whitespace-pre">
                  {line.tokens.length === 0 ? (
                    " "
                  ) : (
                    line.tokens.map((t, j) => (
                      <span key={j} style={tokenStyle(t)}>
                        {t.content}
                      </span>
                    ))
                  )}
                </span>
              </div>
            ))
          : fallbackLines.map((text, i) => (
              <div key={i} className="flex">
                {lineNumbers ? (
                  <span
                    aria-hidden
                    style={{ minWidth: gutterWidth }}
                    className="select-none shrink-0 text-right pr-4 pl-4 text-fg-subtle/70 border-r border-border-muted"
                  >
                    {i + 1}
                  </span>
                ) : null}
                <span className="flex-1 px-4 whitespace-pre text-fg-default">
                  {text || " "}
                </span>
              </div>
            ))}
      </code>
    </pre>
  );
}
