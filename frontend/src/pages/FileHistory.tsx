import { Link, useParams, useSearchParams } from "react-router";
import { useFileHistory } from "../lib/hooks";
import { formatRelative } from "../lib/format";
import { DiffView } from "../components/DiffView";
import {
  BinaryDiffSummary,
  PartAwareDiff,
  StructuredDiff,
} from "../components/BinaryDiff";
import { ImagePreview } from "../components/ImagePreview";
import { ImageDiffView } from "../components/ImageDiffView";
import { isRasterImagePath } from "../lib/image";
import type { FileHistoryEntry } from "../lib/api";

/** /r/:name/history?path=… — timeline of every commit that touched a
 *  single file, with the per-commit diff for that file inlined. */
export function FileHistory() {
  const { name = "" } = useParams();
  const [params] = useSearchParams();
  const path = params.get("path") ?? "";
  const ref = params.get("ref") ?? undefined;
  const history = useFileHistory(name, { path, ref, n: 100 });

  const segments = path.split("/").filter(Boolean);
  const fileName = segments[segments.length - 1] ?? "";

  return (
    <div className="px-6 py-10">
      <header className="border-b border-border-default pb-6 mb-7">
        <div className="flex items-center gap-2 flex-wrap text-sm mb-3">
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
          <Link
            to={`/r/${name}/tree/${ref ?? "HEAD"}/${path}`}
            className="font-mono text-fg-muted hover:text-warm"
          >
            {segments.join("/") || "(no path)"}
          </Link>
          <span className="text-fg-subtle">·</span>
          <span className="text-fg-muted">history</span>
        </div>
        <h1 className="text-xl font-semibold text-fg-default tracking-tight mb-2">
          History of{" "}
          <span className="font-mono text-warm">{fileName || "(no path)"}</span>
        </h1>
        <p className="text-sm text-fg-muted">
          Every commit that added, changed, or removed this file — with the
          file-scoped diff inlined. Click an entry's oid to jump to the full
          commit.
        </p>
      </header>

      {history.isLoading ? (
        <div className="font-mono text-sm text-fg-muted">loading…</div>
      ) : history.isError ? (
        <div className="font-mono text-sm text-danger">
          {(history.error as Error).message}
        </div>
      ) : history.data?.commits.length === 0 ? (
        <div className="font-mono text-sm text-fg-muted">
          no commits matched this path
        </div>
      ) : (
        <div className="space-y-6">
          {history.data?.commits.map((entry) => (
            <HistoryEntry
              key={entry.oid + entry.change}
              repo={name}
              fileName={fileName}
              entry={entry}
            />
          ))}
        </div>
      )}
    </div>
  );
}

function HistoryEntry({
  repo,
  fileName,
  entry,
}: {
  repo: string;
  fileName: string;
  entry: FileHistoryEntry;
}) {
  return (
    <article className="bg-canvas-subtle border border-border-default rounded-lg overflow-hidden">
      <header className="flex items-start gap-4 px-5 py-4 border-b border-border-default bg-canvas-inset/40">
        <ChangeMarker change={entry.change} />
        <div className="flex-1 min-w-0">
          <Link
            to={`/r/${repo}/commits/${entry.oid}`}
            className="text-fg-default hover:text-warm font-medium line-clamp-2"
          >
            {entry.subject || "(no subject)"}
          </Link>
          <div className="mt-1 flex items-center gap-2 text-xs font-mono text-fg-muted">
            <span>{entry.author.name || "unknown"}</span>
            <span className="text-fg-subtle">·</span>
            <span>{formatRelative(entry.author.when)}</span>
            <span className="text-fg-subtle">·</span>
            <Link
              to={`/r/${repo}/commits/${entry.oid}`}
              className="bg-canvas-inset border border-border-muted rounded px-2 py-0.5 hover:border-warm/60 hover:text-warm"
            >
              {entry.oid.slice(0, 7)}
            </Link>
            <span className="uppercase tracking-[0.18em] text-fg-subtle">
              {entry.change}
            </span>
          </div>
        </div>
      </header>

      <EntryDiff repo={repo} fileName={fileName} entry={entry} />
    </article>
  );
}

function EntryDiff({
  repo,
  fileName,
  entry,
}: {
  repo: string;
  fileName: string;
  entry: FileHistoryEntry;
}) {
  // Image side-by-side: when the file is a raster image AND alt's
  // perceptual fingerprint already reported a non-zero distance, the
  // chunk list alone is not the most useful view — show old vs new in
  // place before letting the user expand the part list.
  const isRaster = isRasterImagePath(fileName);
  const { diff } = entry;

  if (diff.kind === "text") {
    return <DiffView patch={diff.patch} path={diff.path} />;
  }
  if (diff.kind === "structured") {
    return <StructuredDiff file={diff} />;
  }
  if (diff.kind === "part_aware") {
    return (
      <>
        {isRaster && diff.format === "png" && entry.change === "changed" ? (
          <ImageDiffView
            oldSrc={`/api/repos/${encodeURIComponent(repo)}/blob/${entry.old_oid}/raw`}
            newSrc={`/api/repos/${encodeURIComponent(repo)}/blob/${entry.new_oid}/raw`}
            oldLabel={`before · ${entry.old_oid.slice(0, 7)}`}
            newLabel={`after · ${entry.new_oid.slice(0, 7)}`}
          />
        ) : null}
        <PartAwareDiff repo={repo} file={diff} />
      </>
    );
  }
  // binary: render image preview if we can, otherwise the size note.
  if (isRaster) {
    return (
      <div className="grid grid-cols-2 gap-px bg-border-muted">
        {entry.old_oid ? (
          <ImageThumb
            label="before"
            src={`/api/repos/${encodeURIComponent(repo)}/blob/${entry.old_oid}/raw`}
            oid={entry.old_oid}
          />
        ) : (
          <div className="p-12 text-center font-mono text-sm text-fg-muted bg-canvas-inset/40">
            (none — added in this commit)
          </div>
        )}
        {entry.new_oid ? (
          <ImageThumb
            label="after"
            src={`/api/repos/${encodeURIComponent(repo)}/blob/${entry.new_oid}/raw`}
            oid={entry.new_oid}
          />
        ) : (
          <div className="p-12 text-center font-mono text-sm text-fg-muted bg-canvas-inset/40">
            (none — removed in this commit)
          </div>
        )}
      </div>
    );
  }
  return <BinaryDiffSummary file={diff} />;
}

function ImageThumb({
  label,
  src,
  oid,
}: {
  label: string;
  src: string;
  oid: string;
}) {
  return (
    <div className="bg-canvas-inset/60 p-3 flex flex-col gap-2 items-center">
      <div className="text-[10px] uppercase tracking-[0.22em] font-mono text-fg-subtle flex items-center gap-2">
        {label}
        <span className="text-fg-default font-mono">{oid.slice(0, 7)}</span>
      </div>
      <ImagePreview src={src} alt={label} maxHeight={360} bare />
    </div>
  );
}

function ChangeMarker({
  change,
}: {
  change: "added" | "changed" | "removed";
}) {
  const cls =
    change === "added"
      ? "bg-diff-add-bg text-diff-add-fg"
      : change === "removed"
        ? "bg-diff-del-bg text-diff-del-fg"
        : "bg-canvas-inset text-warm";
  const sym = change === "added" ? "+" : change === "removed" ? "−" : "~";
  return (
    <span
      className={`shrink-0 mt-0.5 w-7 h-7 rounded font-mono text-base flex items-center justify-center ${cls}`}
    >
      {sym}
    </span>
  );
}
