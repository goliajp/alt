import { useEffect, useState } from 'react';
import { Button } from '@golia/ui/controls';
import { api, ApiError } from './api';
import type { Text } from './i18n';

export function useResource<T>(path: string, revision = 0) {
  const [state, setState] = useState<{ path: string; revision: number; value?: T; error?: unknown }>({ path, revision });
  useEffect(() => {
    const controller = new AbortController();
    api.request<T>(path, undefined, controller.signal).then(value => {
      if (!controller.signal.aborted) setState({ path, revision, value });
    }, error => { if (!controller.signal.aborted) setState({ path, revision, error }); });
    return () => controller.abort();
  }, [path, revision]);
  return state.path === path && state.revision === revision ? state : { path, revision };
}
export function errorText(error: unknown, t: Text) {
  if (!(error instanceof ApiError)) return t.failed;
  if (error.uncertain) return t.unknown;
  if (error.status === 401) return t.expired;
  if (error.status === 403) return t.denied;
  if (error.status === 409) return t.conflict;
  if (error.status === 0) return t.connection;
  return t.failed;
}
export function Failure({ error, retry, t }: { error: unknown; retry: () => void; t: Text }) {
  return <div className="message failure" role="alert"><p>{errorText(error, t)}</p><Button onClick={retry}>{t.retry}</Button></div>;
}
export function Loading({ t }: { t: Text }) { return <p className="message" role="status">{t.loading}</p>; }
