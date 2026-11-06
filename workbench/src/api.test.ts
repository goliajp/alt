import { afterEach, expect, test } from 'bun:test';
import { Api, ApiError } from './api';
const original = globalThis.fetch;
afterEach(() => { globalThis.fetch = original; });
test('a lost or truncated mutation response remains uncertain and is never automatically retried', async () => {
  const api = new Api();
  let requests = 0;
  globalThis.fetch = (async () => { requests++; throw new TypeError('disconnected'); }) as typeof fetch;
  await expect(api.request('/web/repositories/r/epochs/1/requests/a/q/approve', { integration_id: 'i' })).rejects.toMatchObject({ uncertain: true, status: 0 });
  expect(requests).toBe(1);
  globalThis.fetch = (async () => new Response('{', { status: 200 })) as typeof fetch;
  await expect(api.request('/web/logout', {})).rejects.toMatchObject({ uncertain: true, status: 200 });
});
test('browser requests carry credentials only to the fixed same-origin API and reject redirects', async () => {
  const api = new Api();
  globalThis.fetch = (async (path, options) => {
    expect(path).toBe('/web/session');
    expect(options?.credentials).toBe('same-origin');
    expect(options?.redirect).toBe('error');
    expect(options?.cache).toBe('no-store');
    expect(new Headers(options?.headers).has('Authorization')).toBe(false);
    return Response.json({ code: 'unauthenticated' }, { status: 401 });
  }) as typeof fetch;
  await expect(api.request('/web/session')).rejects.toMatchObject({ status: 401, uncertain: false });
  await expect(api.request('https://other.invalid/web/session')).rejects.toThrow('invalid API path');
});
test('an oversized successful response is refused rather than partially accepted', async () => {
  const api = new Api();
  globalThis.fetch = (async () => new Response(new Uint8Array(8 * 1024 * 1024 + 1))) as typeof fetch;
  try { await api.request('/web/session'); throw new Error('unexpected acceptance'); }
  catch (error) { expect(error).toBeInstanceOf(ApiError); expect((error as ApiError).code).toBe('response'); }
});
test('portable empty mutations and binary file slices retain CSRF and uncertain outcomes',async()=>{
  const api=new Api();
  api.setSession({csrf:'csrf-proof'} as Parameters<Api['setSession']>[0]);
  const slice=new Blob(['content']);let requests=0;
  globalThis.fetch=(async(_path,options)=>{
    requests++;expect(options?.credentials).toBe('same-origin');expect(options?.redirect).toBe('error');
    expect(new Headers(options?.headers).get('X-Alt-CSRF')).toBe('csrf-proof');
    if(requests===1){expect(options?.method).toBe('POST');expect(options?.body).toBeUndefined();}
    else {expect(options?.method).toBe('PUT');expect(options?.body).toBe(slice);expect(new Headers(options?.headers).get('Content-Type')).toBe('application/octet-stream');}
    return Response.json({state:'preparing'});
  }) as typeof fetch;
  await api.portable('/web/repositories/r/epochs/1/portable/exports/q','POST');
  await api.portable('/web/repositories/r/epochs/1/portable/imports/a/q/content/b','PUT',slice);
  globalThis.fetch=(async()=>{requests++;throw new TypeError('disconnected');}) as typeof fetch;
  await expect(api.portable('/web/repositories/r/epochs/1/portable/imports/a/q/activate','POST',{})).rejects.toMatchObject({uncertain:true,status:0});
  expect(requests).toBe(3);
});
