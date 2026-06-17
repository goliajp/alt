import type { ReactNode } from "react";

interface Props {
  children: ReactNode;
  className?: string;
  inline?: boolean;
}

/** Monospaced container for oids, paths, install snippets, etc. */
export function CodeBlock({ children, className = "", inline = false }: Props) {
  if (inline) {
    return (
      <code
        className={`font-mono text-[0.92em] bg-canvas-inset border border-border-muted rounded px-1.5 py-0.5 text-fg-default ${className}`}
      >
        {children}
      </code>
    );
  }
  return (
    <pre
      className={`font-mono text-sm bg-canvas-inset border border-border-default rounded-lg p-4 overflow-x-auto leading-relaxed text-fg-default ${className}`}
    >
      {children}
    </pre>
  );
}
