import { useQuery } from "@tanstack/react-query";
import { api } from "./api";

export const useVersion = () =>
  useQuery({
    queryKey: ["version"] as const,
    queryFn: api.version,
    staleTime: Infinity,
  });

export const useRepos = () =>
  useQuery({
    queryKey: ["repos"] as const,
    queryFn: api.repos,
  });

export const useRepo = (name: string) =>
  useQuery({
    queryKey: ["repo", name] as const,
    queryFn: () => api.repo(name),
    enabled: !!name,
  });

export const useRefs = (name: string) =>
  useQuery({
    queryKey: ["refs", name] as const,
    queryFn: () => api.refs(name),
    enabled: !!name,
  });

export const useLog = (
  name: string,
  opts: { ref?: string; n?: number; before?: string } = {},
) =>
  useQuery({
    queryKey: ["log", name, opts] as const,
    queryFn: () => api.log(name, opts),
    enabled: !!name,
  });

export const useCommit = (name: string, oid: string) =>
  useQuery({
    queryKey: ["commit", name, oid] as const,
    queryFn: () => api.commit(name, oid),
    enabled: !!name && !!oid,
  });

export const useCommitDiff = (name: string, oid: string) =>
  useQuery({
    queryKey: ["commitDiff", name, oid] as const,
    queryFn: () => api.commitDiff(name, oid),
    enabled: !!name && !!oid,
  });

export const useTree = (name: string, spec: string) =>
  useQuery({
    queryKey: ["tree", name, spec] as const,
    queryFn: () => api.tree(name, spec),
    enabled: !!name && !!spec,
  });

export const useBlob = (name: string, oid: string) =>
  useQuery({
    queryKey: ["blob", name, oid] as const,
    queryFn: () => api.blob(name, oid),
    enabled: !!name && !!oid,
  });

export const useFileHistory = (
  name: string,
  opts: { path: string; ref?: string; n?: number },
) =>
  useQuery({
    queryKey: ["fileHistory", name, opts] as const,
    queryFn: () => api.fileHistory(name, opts),
    enabled: !!name && !!opts.path,
  });

export const useStorage = (name: string, oid: string) =>
  useQuery({
    queryKey: ["storage", name, oid] as const,
    queryFn: () => api.storage(name, oid),
    enabled: !!name && !!oid,
  });

export const useStorageStats = (name: string) =>
  useQuery({
    queryKey: ["storageStats", name] as const,
    queryFn: () => api.storageStats(name),
    enabled: !!name,
  });
