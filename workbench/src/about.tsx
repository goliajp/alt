import type { Text } from './i18n';

const project = 'https://github.com/goliajp/alt';

export function About({ t }: { t: Text }) {
  const address = <>{location.origin}/git/<span className="slot">&lt;{t.aboutRepositoryName}&gt;</span></>;
  return <section className="about" aria-labelledby="about-title">
    <h2 id="about-title">{t.aboutTitle}</h2>
    <p>{t.aboutWhat}</p>
    <h3>{t.aboutAddress}</h3>
    <p><code>{address}</code></p>
    <pre className="source"><code>git clone {address}</code></pre>
    <h3>{t.aboutAccess}</h3>
    <p>{t.aboutAccessBody}</p>
    <h3>{t.aboutProject}</h3>
    <p>{t.aboutSource}<a href={project} target="_blank" rel="noreferrer">{project}</a></p>
  </section>;
}
