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
}

export interface DiffPartAware {
  kind: "part_aware";
  path: string;
  format: "png" | "zip";
  old_oid: string;
  new_oid: string;
  old_bytes: number;
  new_bytes: number;
  perceptual_distance: number | null;
  parts: PartChange[];
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
};
