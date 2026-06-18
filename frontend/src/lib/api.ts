/* Plain fetch wrappers around the alt-web JSON API. Same origin, no
   auth, JSON throughout. */

export interface Version {
  schema_version: number;
  version: string;
  build: string;
}

export interface RepoSummary {
  name: string;
  head: string;
  head_branch: string;
  refs: number;
}

export interface RefRecord {
  name: string;
  oid: string;
  target: string;
}

export interface Author {
  name: string;
  email: string;
  when: number;
}

export interface LogCommit {
  oid: string;
  subject: string;
  author: Author;
}

export interface CommitDetail {
  oid: string;
  tree: string;
  parents: string[];
  subject: string;
  body: string;
  author: Author;
  committer: Author;
}

export type DiffFile =
  | DiffText
  | DiffStructured
  | DiffPartAware
  | DiffBinary;

export interface DiffText {
  kind: "text";
  path: string;
  old_oid: string;
  new_oid: string;
  patch: string;
}

export interface StructuredPath {
  path: string;
  change: "changed" | "added" | "removed";
  old?: string;
  new?: string;
}

export interface DiffStructured {
  kind: "structured";
  path: string;
  format: "json" | "toml";
  old_oid: string;
  new_oid: string;
  paths: StructuredPath[];
}

export interface PartChange {
  name: string;
  change: "same" | "changed" | "added" | "removed";
  old_bytes?: number;
  new_bytes?: number;
  /** Unified diff over the member's *inflated* body, when both sides
   *  are text-shaped. Only set on `changed` parts. */
  text_patch?: string;
}

export interface FileHistoryEntry {
  oid: string;
  subject: string;
  author: Author;
  change: "added" | "changed" | "removed";
  old_oid: string;
  new_oid: string;
  /** Same FileDiff shape as the commit diff endpoint, scoped to this
   *  one path. */
  diff: DiffFile;
}

export interface DocumentEntry {
  change: "added" | "removed" | "same";
  text: string;
}

export interface SheetCell {
  ref: string;
  col: string;
  row: number;
  change: "same" | "added" | "removed" | "changed";
  old: string | null;
  new: string | null;
}

export interface SheetGrid {
  name: string;
  max_col: string;
  max_row: number;
  cells: SheetCell[];
  has_changes: boolean;
}

export type DocumentDiff =
  | { kind: "docx"; entries: DocumentEntry[] }
  | { kind: "xlsx"; sheets: SheetGrid[] };

export interface DiffPartAware {
  kind: "part_aware";
  path: string;
  format: "png" | "zip";
  old_oid: string;
  new_oid: string;
  old_bytes: number;
  new_bytes: number;
  perceptual_distance: number | null;
  /** Old / new 64-bit perceptual fingerprint hashes (hex). PNG only. */
  perceptual_hash_old: string | null;
  perceptual_hash_new: string | null;
  parts: PartChange[];
  /** OOXML semantic content diff. `null` for any non-OOXML zip. */
  document: DocumentDiff | null;
}

export interface DiffBinary {
  kind: "binary";
  path: string;
  old_oid: string;
  new_oid: string;
  old_bytes: number;
  new_bytes: number;
}

export type TreeEntryKind = "blob" | "tree" | "commit";

export interface TreeEntry {
  mode: string;
  name: string;
  oid: string;
  kind: TreeEntryKind;
}

export interface Blob {
  oid: string;
  size: number;
  binary: boolean;
  content: string | null;
}

async function getJSON<T>(path: string): Promise<T> {
  const res = await fetch(path, {
    headers: { Accept: "application/json" },
  });
  if (!res.ok) {
    let detail = `${res.status} ${res.statusText}`;
    try {
      const body = await res.json();
      if (body?.error?.message) detail = `${detail}: ${body.error.message}`;
    } catch {
      /* ignore */
    }
    throw new Error(detail);
  }
  return (await res.json()) as T;
}

export const api = {
  version: () => getJSON<Version>("/api/version"),

  repos: () => getJSON<{ repos: RepoSummary[] }>("/api/repos"),

  repo: (name: string) =>
    getJSON<{ repo: RepoSummary }>(`/api/repos/${encodeURIComponent(name)}`),

  refs: (name: string) =>
    getJSON<{ refs: RefRecord[] }>(
      `/api/repos/${encodeURIComponent(name)}/refs`,
    ),

  log: (
    name: string,
    opts: { ref?: string; n?: number; before?: string } = {},
  ) => {
    const qs = new URLSearchParams();
    if (opts.ref) qs.set("ref", opts.ref);
    if (opts.n != null) qs.set("n", String(opts.n));
    if (opts.before) qs.set("before", opts.before);
    const tail = qs.toString();
    return getJSON<{ commits: LogCommit[] }>(
      `/api/repos/${encodeURIComponent(name)}/log${tail ? `?${tail}` : ""}`,
    );
  },

  commit: (name: string, oid: string) =>
    getJSON<{ commit: CommitDetail }>(
      `/api/repos/${encodeURIComponent(name)}/commits/${oid}`,
    ),

  commitDiff: (name: string, oid: string) =>
    getJSON<{ oid: string; files: DiffFile[] }>(
      `/api/repos/${encodeURIComponent(name)}/commits/${oid}/diff`,
    ),

  tree: (name: string, spec: string) =>
    getJSON<{ oid: string; entries: TreeEntry[] }>(
      `/api/repos/${encodeURIComponent(name)}/tree/${encodeURIComponent(spec)}`,
    ),

  blob: (name: string, oid: string) =>
    getJSON<Blob>(`/api/repos/${encodeURIComponent(name)}/blob/${oid}`),

  fileHistory: (
    name: string,
    opts: { path: string; ref?: string; n?: number },
  ) => {
    const qs = new URLSearchParams({ path: opts.path });
    if (opts.ref) qs.set("ref", opts.ref);
    if (opts.n != null) qs.set("n", String(opts.n));
    return getJSON<{ path: string; commits: FileHistoryEntry[] }>(
      `/api/repos/${encodeURIComponent(name)}/file_history?${qs.toString()}`,
    );
  },

  storage: (name: string, oid: string) =>
    getJSON<StorageView>(
      `/api/repos/${encodeURIComponent(name)}/storage/${oid}`,
    ),
};

export interface StorageView {
  git_oid: string;
  blob_id: string;
  kind: "blob" | "tree" | "commit" | "tag";
  logical_size: number;
  tier: 0 | 1;
  tier1: {
    prism: number;
    recipe_len: number;
    record_blob: string;
    parts: string[];
  } | null;
  chunks: {
    leaf_count: number;
    logical_total: number;
    stored_total: number;
    entries: StorageChunk[];
  };
}

export interface StorageChunk {
  chunk_id: string;
  encoding: "raw" | "zstd" | "delta";
  orig_len: number;
  stored_len: number;
}
