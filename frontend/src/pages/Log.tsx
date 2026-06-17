import { Link, useParams, useSearchParams } from "react-router";
import { useLog } from "../lib/hooks";
import { formatRelative } from "../lib/format";

const PAGE = 30;

export function Log() {
  const { name = "" } = useParams();
  const [params, setParams] = useSearchParams();
  const before = params.get("before") || undefined;
  const ref = params.get("ref") || undefined;
  const log = useLog(name, { ref, n: PAGE, before });

  const commits = log.data?.commits ?? [];
  const nextBefore =
    commits.length === PAGE ? commits[commits.length - 1].oid : undefined;

  return (
    <div className="px-6 py-10">
      <header className="border-b border-border-default pb-6 mb-7">
        <div className="flex items-center gap-2 mb-2 text-sm">
          <Link to="/" className="text-fg-muted hover:text-fg-default">
            alt.golia.jp
          </Link>
          <span className="text-fg-subtle">/</span>
          <Link
            to={`/r/${name}`}
            className="font-mono text-fg-default hover:text-warm"
          >
            {name}
          </Link>
          <span className="text-fg-subtle">/</span>
          <span className="text-fg-muted">commits</span>
        </div>
        <h1 className="text-2xl font-semibold text-fg-default tracking-tight">
          History
        </h1>
      </header>

      {log.isLoading ? (
        <div className="font-mono text-sm text-fg-muted">loading…</div>
      ) : log.isError ? (
        <div className="font-mono text-sm text-danger">
          {(log.error as Error).message}
        </div>
      ) : (
        <ol className="border border-border-default rounded-lg overflow-hidden divide-y divide-border-muted">
          {commits.map((c) => (
            <li
              key={c.oid}
              className="bg-canvas-subtle hover:bg-canvas-inset/60 transition-colors"
            >
              <Link
                to={`/r/${name}/commits/${c.oid}`}
                className="flex items-start gap-4 px-5 py-4 group"
              >
                <div className="flex-1 min-w-0">
                  <div className="text-fg-default group-hover:text-warm line-clamp-2">
                    {c.subject || "(no subject)"}
                  </div>
                  <div className="mt-1.5 flex items-center gap-2 text-xs font-mono text-fg-muted">
                    <span>{c.author.name || "unknown"}</span>
                    <span className="text-fg-subtle">·</span>
                    <span>{formatRelative(c.author.when)}</span>
                  </div>
                </div>
                <span className="shrink-0 font-mono text-xs text-fg-muted bg-canvas-inset border border-border-muted rounded px-2 py-1 group-hover:border-warm/60 group-hover:text-warm transition-colors">
                  {c.oid.slice(0, 7)}
                </span>
              </Link>
            </li>
          ))}
        </ol>
      )}

      <div className="flex items-center justify-between mt-7 text-sm">
        <button
          onClick={() => setParams({})}
          disabled={!before}
          className="text-accent disabled:text-fg-subtle disabled:cursor-not-allowed hover:underline font-medium"
        >
          ← Newest
        </button>
        <button
          onClick={() => {
            if (nextBefore) setParams({ before: nextBefore });
          }}
          disabled={!nextBefore}
          className="text-accent disabled:text-fg-subtle disabled:cursor-not-allowed hover:underline font-medium"
        >
          Older →
        </button>
      </div>
    </div>
  );
}
