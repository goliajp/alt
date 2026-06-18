import { Link, useParams } from "react-router";
import { useCommit, useCommitDiff } from "../lib/hooks";
import { DiffView } from "../components/DiffView";
import { StoragePanel } from "../components/StoragePanel";
import {
  BinaryDiffSummary,
  PartAwareDiff,
  StructuredDiff,
} from "../components/BinaryDiff";
import { formatRelative } from "../lib/format";

export function Commit() {
  const { name = "", oid = "" } = useParams();
  const commit = useCommit(name, oid);
  const diff = useCommitDiff(name, oid);

  return (
    <div className="px-6 py-10">
      <header className="border-b border-border-default pb-7 mb-8">
        <div className="flex items-center gap-2 mb-4 text-sm flex-wrap">
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
            to={`/r/${name}/commits`}
            className="text-fg-muted hover:text-fg-default"
          >
            commits
          </Link>
          <span className="text-fg-subtle">/</span>
          <span className="font-mono text-warm">{oid.slice(0, 7)}</span>
        </div>

        {commit.data?.commit ? (
          <>
            <h1 className="text-2xl font-semibold text-fg-default mb-3 tracking-tight">
              {commit.data.commit.subject || "(no subject)"}
            </h1>
            {commit.data.commit.body ? (
              <pre className="font-sans whitespace-pre-wrap text-sm text-fg-muted leading-relaxed mb-7">
                {commit.data.commit.body.trim()}
              </pre>
            ) : (
              <div className="mb-6" />
            )}

            <div className="grid grid-cols-1 sm:grid-cols-2 gap-3 text-sm">
              <Ident
                role="author"
                name={commit.data.commit.author.name}
                email={commit.data.commit.author.email}
                when={commit.data.commit.author.when}
              />
              <Ident
                role="committer"
                name={commit.data.commit.committer.name}
                email={commit.data.commit.committer.email}
                when={commit.data.commit.committer.when}
              />
            </div>

            <div className="mt-5 flex items-center flex-wrap gap-3 text-xs font-mono">
              <span className="text-fg-subtle uppercase tracking-[0.18em]">
                tree
              </span>
              <Link
                to={`/r/${name}/tree/${commit.data.commit.tree}`}
                className="text-accent hover:underline"
              >
                {commit.data.commit.tree.slice(0, 12)}
              </Link>
              <span className="text-fg-subtle">·</span>
              <span className="text-fg-subtle uppercase tracking-[0.18em]">
                parents
              </span>
              {commit.data.commit.parents.length === 0 ? (
                <span className="text-fg-subtle">∅</span>
              ) : (
                commit.data.commit.parents.map((p) => (
                  <Link
                    key={p}
                    to={`/r/${name}/commits/${p}`}
                    className="text-accent hover:underline"
                  >
                    {p.slice(0, 7)}
                  </Link>
                ))
              )}
            </div>
          </>
        ) : commit.isLoading ? (
          <div className="font-mono text-sm text-fg-muted">loading…</div>
        ) : commit.isError ? (
          <div className="font-mono text-sm text-danger">
            {(commit.error as Error).message}
          </div>
        ) : null}
      </header>

      <section className="space-y-7">
        {diff.isLoading ? (
          <div className="font-mono text-sm text-fg-muted">loading diff…</div>
        ) : diff.isError ? (
          <div className="font-mono text-sm text-danger">
            {(diff.error as Error).message}
          </div>
        ) : diff.data?.files.length === 0 ? (
          <div className="font-mono text-sm text-fg-muted">
            no changes against parent
          </div>
        ) : (
          diff.data?.files.map((file) => (
            <div
              key={file.path + file.kind}
              className="bg-canvas-subtle border border-border-default rounded-lg overflow-hidden"
            >
              <div className="flex items-center gap-3 border-b border-border-default px-4 py-3 bg-canvas-inset/40">
                <span className="font-mono text-warm">●</span>
                <span className="font-mono text-sm text-fg-default flex-1">
                  {file.path}
                </span>
                <DiffKindChip file={file} />
              </div>
              {file.kind === "text" ? (
                <DiffView patch={file.patch} path={file.path} />
              ) : file.kind === "structured" ? (
                <StructuredDiff repo={name} file={file} />
              ) : file.kind === "part_aware" ? (
                <PartAwareDiff repo={name} file={file} />
              ) : (
                <BinaryDiffSummary file={file} />
              )}
              {"new_oid" in file && file.new_oid ? (
                <div className="border-t border-border-default p-3 bg-canvas-inset/20">
                  <StoragePanel repo={name} oid={file.new_oid} />
                </div>
              ) : null}
            </div>
          ))
        )}
      </section>
    </div>
  );
}

function DiffKindChip({ file }: { file: import("../lib/api").DiffFile }) {
  let label = "";
  let tone = "text-fg-subtle border-border-muted";
  switch (file.kind) {
    case "text":
      label = "text";
      break;
    case "structured":
      label = `semantic ${file.format}`;
      tone = "text-accent/90 border-accent/40";
      break;
    case "part_aware": {
      const ext = (file.path.split(".").pop() ?? "").toLowerCase();
      if (file.format === "png") label = "binary png";
      else if (["docx", "xlsx", "pptx"].includes(ext))
        label = `binary ooxml/${ext}`;
      else label = "binary zip";
      tone = "text-warm/95 border-warm/40";
      break;
    }
    case "binary":
      label = "opaque binary";
      tone = "text-attention/95 border-attention/40";
      break;
  }
  return (
    <span
      className={`text-[10px] uppercase tracking-[0.22em] font-mono rounded px-1.5 py-0.5 border ${tone}`}
    >
      {label}
    </span>
  );
}

function Ident({
  role,
  name,
  email,
  when,
}: {
  role: "author" | "committer";
  name: string;
  email: string;
  when: number;
}) {
  return (
    <div className="bg-canvas-subtle border border-border-default rounded-md px-4 py-3">
      <div className="text-[10px] uppercase tracking-[0.22em] font-mono text-fg-subtle mb-1.5">
        {role}
      </div>
      <div className="text-fg-default font-medium">{name || "—"}</div>
      <div className="font-mono text-xs text-fg-muted">
        {email}
        {when ? <> · {formatRelative(when)}</> : null}
      </div>
    </div>
  );
}
