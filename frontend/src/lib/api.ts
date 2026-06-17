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

export interface DiffFile {
  path: string;
  patch: string;
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
