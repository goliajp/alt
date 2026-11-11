import { useEffect, useRef, useState, type ReactNode } from 'react';
import { BookOpen, GitBranch, Inbox, ListTodo, PackageCheck, Search, Settings } from 'lucide-react';
import { Button, Field, Input, NativeSelect } from '@golia/ui/controls';
import { Dialog } from '@golia/ui/Dialog';
import { WorkbenchLayout } from '@golia/ui/WorkbenchLayout';
import { DocumentTabs } from '@golia/ui/DocumentTabs';
import { api, ApiError, type Page, type Repository, type Session } from './api';
import { closeDocuments, documentKey, documentFromHash, documentHash, moveDocument, openDocument, type Document, type OpenDocument } from './documents';
import { messages, type Locale, type Text } from './i18n';
import { errorText, Failure, Loading, useResource } from './resource';
import { DocumentView } from './views';
import { CloudProducts } from './products';
import { DeviceAuthorization } from './device';
import { captureDeviceEntry } from './device-location';
import { About } from './about';

function savedPreference<T extends string>(key: string, choices: readonly T[], fallback: T): T {
  try { const value = localStorage.getItem(key); return choices.includes(value as T) ? value as T : fallback; } catch { return fallback; }
}
function title(document: Document, t: Text) {
  if (document.kind === 'blob') return document.path.split('/').at(-1) || document.oid.slice(0, 10);
  if (document.kind === 'tree') return document.path || t.files;
  if (document.kind === 'record') return `${t[document.recordKind]} · ${document.id.slice(0, 8)}`;
  if (document.kind === 'review') return `${t.review} · ${document.request}`;
  if ('repository' in document) return `${document.repository} · ${t[document.kind]}`;
  return t[document.kind];
}
function Login({ t, done }: { t: Text; done: (session: Session) => void }) {
  const [busy, setBusy] = useState(false), [error, setError] = useState('');
  return <main className="login login-visitor"><form onSubmit={async event => {
    event.preventDefault();
    const form = event.currentTarget;
    const fields = new FormData(form);
    setBusy(true); setError('');
    try {
      const session = await api.request<Session>('/web/login', { login_name: fields.get('login'), password: fields.get('password') });
      form.reset(); done(session);
    } catch (error) {
      setError(error instanceof ApiError && [401, 403].includes(error.status) ? t.invalidLogin : errorText(error, t));
      const input = form.elements.namedItem('password');
      if (input instanceof HTMLInputElement) input.value = '';
    } finally { setBusy(false); }
  }}><img src="/mark-ink.svg" alt="alt" width="48" height="48"/><h1>{t.localLogin}</h1>
    <Field label={t.loginName}><Input name="login" autoComplete="username" required maxLength={128} autoFocus/></Field>
    <Field label={t.password}><Input name="password" type="password" autoComplete="current-password" required maxLength={1024}/></Field>
    {error && <p role="alert" className="failure">{error}</p>}
    <Button type="submit" variant="primary" busy={busy}>{t.signIn}</Button>
  </form><About t={t}/></main>;
}
export function App() {
  const [device,setDevice]=useState(captureDeviceEntry);
  useEffect(()=>{const change=()=>setDevice(captureDeviceEntry());window.addEventListener('hashchange',change);return()=>window.removeEventListener('hashchange',change);},[]);
  const [locale, setLocale] = useState<Locale>(() => savedPreference('alt.locale', ['en', 'zh', 'ja'], 'en'));
  const [theme, setTheme] = useState(() => savedPreference('alt.theme', ['light', 'dark'], 'light'));
  const [session, setSession] = useState<Session | null | undefined>();
  const [revision, refresh] = useState(0), [error, setError] = useState<unknown>();
  const t = messages(locale);
  const configuration = useResource<{ deployment_mode: 'self_hosted' | 'cloud'; workspace_id: string }>('/web/config', revision);
  useEffect(() => {
    document.documentElement.lang = locale; document.documentElement.dataset.theme = theme;
    try { localStorage.setItem('alt.locale', locale); localStorage.setItem('alt.theme', theme); } catch { /* Preferences apply to this page. */ }
  }, [locale, theme]);
  useEffect(() => {
    const controller = new AbortController();
    api.request<Session>('/web/session', undefined, controller.signal).then(value => {
      if (!controller.signal.aborted) { api.setSession(value); setSession(value); setError((previous: unknown) => previous instanceof ApiError && previous.uncertain ? previous : undefined); }
    }, error => {
      if (controller.signal.aborted) return;
      if (error instanceof ApiError && error.status === 401) { api.setSession(null); setSession(null); setError(undefined); }
      else setError(error);
    });
    return () => controller.abort();
  }, [revision]);
  const preferences = <div className="preferences"><NativeSelect aria-label={t.language} value={locale} onChange={event => setLocale(event.target.value as Locale)}>
    <option value="en">English</option><option value="zh">中文</option><option value="ja">日本語</option>
  </NativeSelect><NativeSelect aria-label={t.theme} value={theme} onChange={event => setTheme(event.target.value as 'light' | 'dark')}>
    <option value="light">{t.light}</option><option value="dark">{t.dark}</option>
  </NativeSelect></div>;
  return <>{session && device ? <><div className="login-preferences">{preferences}</div><DeviceAuthorization code={device.code} session={session} t={t} close={()=>{history.replaceState(null,'',location.pathname+location.search);setDevice(null);}}/></>:session ? <Workbench key={`${session.installation_id}:${session.user.actor_id}`} session={session} t={t} locale={locale} preferences={preferences} notification={error}
    signOut={async () => {
      try { await api.request('/web/logout', {}); api.setSession(null); setSession(null); }
      catch (error) { setError(error); refresh(value => value + 1); }
    }}/>:<><div className="login-preferences">{preferences}</div>{error || configuration.error ? <Failure error={error ?? configuration.error} t={t} retry={() => refresh(value => value + 1)}/>:session === undefined || !configuration.value ? <Loading t={t}/>:configuration.value.deployment_mode === 'cloud' ? <main className="login"><h1>{t.signIn}</h1><p>{configuration.value.workspace_id} · {t.cloud}</p><a href="/web/login/start">{t.signIn}</a></main>:<Login t={t} done={value => { api.setSession(value); setSession(value); }}/>}</>}</>;
}
function Workbench({ session, t, locale, preferences, signOut, notification }: { notification?: unknown; session: Session; t: Text; locale: Locale; preferences: ReactNode; signOut: () => Promise<void> }) {
  const initial = openDocument([], documentFromHash(location.hash) ?? { kind: 'inbox' });
  const [documents, setDocuments] = useState<OpenDocument[]>(initial.documents), [active, setActive] = useState(initial.key);
  const [selectedRepository, selectRepository] = useState(''), [palette, showPalette] = useState(false), [search, setSearch] = useState('');
  const [activity, setActivity] = useState<string[]>([]), [repositoryRevision, refreshRepositories] = useState(0);
  const repositories = useResource<Page<Repository>>('/web/repositories', repositoryRevision);
  const activeDocument = documents.find(item => item.key === active);
  const commandTrigger = useRef<HTMLSpanElement>(null);
  const [signingOut, setSigningOut] = useState(false);
  const open = (document: Document) => {
    const key = documentKey(document);
    setDocuments(current => openDocument(current, document).documents); setActive(key);
    if (location.hash !== documentHash(document)) location.hash = documentHash(document);
  };
  useEffect(() => {
    const select = () => { const document = documentFromHash(location.hash); if (document) {setDocuments(current => openDocument(current, document).documents);setActive(documentKey(document));} };
    window.addEventListener('hashchange', select); return () => window.removeEventListener('hashchange', select);
  }, []);
  const close = (keys: string[]) => {
    const next = closeDocuments(documents, active, keys); setDocuments(next.documents); setActive(next.active);
    const selected = next.documents.find(item=>item.key===next.active);
    history.replaceState(null, '', location.pathname + location.search + (selected ? documentHash(selected.document) : ''));
  };
  const log = (message: string) => setActivity(previous => [`${new Date().toLocaleTimeString(locale)} · ${message}`, ...previous].slice(0, 100));
  useEffect(() => {
    const handler = (event: KeyboardEvent) => {
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === 'k') { event.preventDefault(); showPalette(true); }
    };
    window.addEventListener('keydown', handler); return () => window.removeEventListener('keydown', handler);
  }, []);
  const navigate = (kind: 'inbox' | 'repositories' | 'settings' | 'tasks' | 'deliveries') => {
    if (kind === 'tasks' || kind === 'deliveries') {
      if (selectedRepository) open({ kind, repository: selectedRepository });
      else open({ kind: 'repositories' });
    } else open({ kind });
  };
  const labels = {
    title:'alt', explorer:t.explorer, inspector:t.inspector, activity:t.activity,
    toggleExplorer:`${t.toggle} ${t.explorer}`, toggleInspector:`${t.toggle} ${t.inspector}`, toggleActivity:`${t.toggle} ${t.activity}`,
    reset:t.reset, close:t.close, resize:(name:string)=>`${t.resize} ${name}`,
  };
  const commands = [
    ...(['inbox', 'repositories', 'settings'] as const).map(kind => ({ title:t[kind], run:()=>open({kind}) })),
    ...documents.map(item => ({title:title(item.document,t),run:()=>open(item.document)})),
    ...(repositories.value?.items ?? []).map(item => ({title:item.scope.repository_instance_id,run:()=>open({kind:'repository',repository:item.scope.repository_instance_id})})),
  ].filter(item=>item.title.toLocaleLowerCase().includes(search.toLocaleLowerCase())).slice(0,50);
  return <div className="app-shell"><header className="global-bar"><a href="/" className="brand" aria-label="alt"><img src="/mark-ink.svg" alt=""/>alt</a>
    {session.deployment_mode==='cloud'?<CloudProducts t={t} workspace={session.workspace_id}/>:<><NativeSelect aria-label={t.product} value="alt"><option value="alt">alt</option></NativeSelect><span className="workspace" title={t.workspace}>{session.workspace_id}</span></>}
    <span className="mode">{session.deployment_mode==='cloud'?t.cloud:t.selfHosted}</span>
    <span ref={commandTrigger} className="command-anchor"><Button variant="ghost" className="command-trigger" onClick={()=>showPalette(true)} aria-label={t.commands}><Search size={16}/><span className="command-label">{t.search}</span><kbd>⌘ K</kbd></Button></span>
    {preferences}<Button variant="ghost" onClick={()=>open({kind:'settings'})}>{session.user.display_name}</Button><Button variant="ghost" busy={signingOut} onClick={async()=>{setSigningOut(true);try{await signOut();}finally{setSigningOut(false);}}}>{t.signOut}</Button>
  </header>{notification !== undefined && <p className="session-notice failure" role="alert">{errorText(notification,t)}</p>}<WorkbenchLayout labels={labels} storageKey={`alt.layout:${session.installation_id}:${session.user.actor_id}`} routeKey={active}
    toolbar={<><GitBranch size={16}/><NativeSelect aria-label={t.repository} value={selectedRepository} onChange={event=>{
      const repository=event.target.value;selectRepository(repository);if(repository)open({kind:'repository',repository});
    }}><option value="">{t.selectRepository}</option>{repositories.value?.items.map(item=><option key={item.scope.repository_instance_id}>{item.scope.repository_instance_id}</option>)}</NativeSelect>{repositories.error && <Button small onClick={()=>refreshRepositories(value=>value+1)}>{t.retry}</Button>}</>}
    explorer={<nav className="explorer-nav">{([[Inbox,'inbox'],[BookOpen,'repositories'],[ListTodo,'tasks'],[PackageCheck,'deliveries'],[Settings,'settings']] as const).map(([Icon,kind])=><Button key={kind} variant="ghost" aria-current={activeDocument?.document.kind===kind?'page':undefined} onClick={()=>navigate(kind)}><Icon size={16}/>{t[kind]}</Button>)}</nav>}
    inspector={<div className="inspector-content"><h3>{activeDocument ? title(activeDocument.document,t) : 'alt'}</h3><dl><dt>{t.workspace}</dt><dd>{session.workspace_id}</dd>{activeDocument && 'repository' in activeDocument.document && <><dt>{t.repository}</dt><dd>{activeDocument.document.repository}</dd></>}</dl><p>{t.scopeNote}</p></div>}
    tabs={<DocumentTabs locale={locale} entries={documents.map(item=>({key:item.key,title:title(item.document,t)}))} active={active} select={key=>{const item=documents.find(item=>item.key===key);if(item)open(item.document);}} close={key=>close([key])} closeMany={close} reorder={(key,index)=>setDocuments(current=>moveDocument(current,key,index))}
      labels={{navigation:t.tabs,hint:t.tabHint,close:name=>`${t.close} ${name}`,moved:(name,position,total)=>`${t.moved} ${name} · ${position}/${total}`}}/>}
    activity={<div className="activity-content"><h3>{t.activity}</h3><ul>{activity.map((message,index)=><li key={`${index}:${message}`}>{message}</li>)}</ul></div>}
    status={<><span>{session.deployment_mode==='cloud'?t.cloud:t.selfHosted}</span><span>{session.build_id}</span></>}>
    {activeDocument ? <section key={active} role="tabpanel" id={`document-${active}`} aria-labelledby={`tab-${active}`} className="document"><DocumentView document={activeDocument.document} session={session} t={t} open={open} log={log}/></section>:<p className="message">{t.noDocuments}</p>}
  </WorkbenchLayout><Dialog open={palette} onClose={()=>{showPalette(false);commandTrigger.current?.querySelector('button')?.focus();}} title={t.commands} closeLabel={t.close}>
    <Field label={t.search}><Input value={search} onChange={event=>setSearch(event.target.value)} autoFocus/></Field>
    <div className="command-list">{commands.map((item,index)=><Button key={`${index}:${item.title}`} variant="ghost" onClick={()=>{item.run();showPalette(false);}}>{item.title}</Button>)}</div>
  </Dialog></div>;
}
