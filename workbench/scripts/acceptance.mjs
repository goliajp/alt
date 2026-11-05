// Run against the real dual-client fixture through ALT_WORKBENCH_ACCEPTANCE.
// Requires a production build and a locally installed Playwright Chromium.
import { chromium } from 'playwright-core';
import assert from 'node:assert/strict';
import { mkdir, writeFile } from 'node:fs/promises';
import { resolve } from 'node:path';
const origin = process.env.ALT_BROWSER_ORIGIN;
const review = process.env.ALT_BROWSER_REVIEW;
assert(origin?.startsWith('http://127.0.0.1:'));
assert(review?.startsWith('/web/repositories/'));
const report = resolve(process.env.ALT_WORKBENCH_REPORT ?? '.tmp/workbench-browser');
await mkdir(report, { recursive: true });
const browser = await chromium.launch({ headless: true, executablePath: process.env.ALT_CHROMIUM || undefined, args: ['--no-sandbox', '--disable-dev-shm-usage'] });
const context = await browser.newContext({ viewport: { width: 1440, height: 1000 }, reducedMotion: 'reduce' });
const page = await context.newPage();
page.setDefaultTimeout(20000);
const errors = [], checks = [];
page.on('pageerror', error => errors.push(String(error)));
const check = (name, condition) => { assert(condition, name); checks.push(name); };
const language = () => page.locator('.preferences select').nth(0);
const theme = () => page.locator('.preferences select').nth(1);
try {
  await page.goto(origin);
  await page.locator('input[name=login]').fill('reviewer');
  await page.locator('input[name=password]').fill('a private browser acceptance password');
  await language().selectOption('zh');
  check('language preserves typed login', await page.locator('input[name=login]').inputValue() === 'reviewer');
  check('language preserves typed password', await page.locator('input[name=password]').inputValue() === 'a private browser acceptance password');
  await language().selectOption('ja');
  check('Japanese document language', await page.locator('html').getAttribute('lang') === 'ja');
  await language().selectOption('en');
  await page.getByRole('button', { name: 'Sign in', exact: true }).click();
  await page.getByRole('button', { name: 'Sign out', exact: true }).waitFor();
  await page.locator('.resource-row').filter({ hasText: 'repository' }).first().click();
  await page.getByRole('button', { name: 'Approve this selection', exact: true }).waitFor();
  check('live review displays the second client change', (await page.locator('.patch').allTextContents()).join('\n').includes('+second-client'));
  check('historical review remains attributed to Bob', (await page.locator('.record-fields').innerText()).includes('bob'));
  const original = await context.request.get(origin + await page.getByRole('link', { name: 'Download original', exact: true }).getAttribute('href'));
  check('full review download is authenticated and byte preserving', original.ok() && (await original.json()).changes.some(change => change.after?.utf8 === 'second-client\n'));
  await page.getByRole('combobox', { name: 'Repository', exact: true }).selectOption('repository');
  await page.getByRole('button', { name: 'History', exact: true }).click();
  await page.locator('.resource-row').first().click();
  await page.locator('.resource-row').filter({ hasText: 'source.txt' }).click();
  await page.locator('.source').waitFor();
  check('published tree remains distinct from pending candidate', (await page.locator('.source').innerText()).includes('base'));
  await page.keyboard.press('Control+k');
  await page.getByRole('dialog').waitFor();
  await page.getByRole('dialog').locator('input').fill('review');
  await page.getByRole('dialog').getByRole('button', { name: /^Review ·/ }).click();
  await page.locator('.patch').first().waitFor();
  check('command navigation returns to bound original review', (await page.locator('.patch').allTextContents()).join('\n').includes('+second-client'));
  const shared = page.url();
  check('address identifies the exact review document', new URL(shared).hash.startsWith('#document='));
  const recipient = await context.newPage();
  recipient.on('pageerror', error=>errors.push(String(error)));
  await recipient.goto(shared);
  await recipient.getByRole('button', { name:'Approve this selection', exact:true }).waitFor();
  check('shared link reloads the original candidate', (await recipient.locator('.patch').allTextContents()).join('\n').includes('+second-client'));
  await recipient.close();
  const anonymous = await browser.newContext();
  const login = await anonymous.newPage();
  await login.goto(shared);
  await login.locator('input[name=login]').waitFor();
  check('share recipient must independently authenticate', await login.locator('.patch').count()===0);
  await anonymous.close();
  for (const locale of ['en', 'zh', 'ja']) {
    await language().selectOption(locale);
    for (const mode of ['light', 'dark']) {
      await theme().selectOption(mode);
      for (const width of [320, 390, 768, 1440, 2560]) {
        await page.setViewportSize({ width, height: 1000 });
        await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
        const geometry = await page.evaluate(() => ({ width: innerWidth, content: document.documentElement.scrollWidth, panel: document.querySelector('.document')?.getBoundingClientRect().width }));
        check(`no page overflow ${locale}/${mode}/${width}`, geometry.content <= geometry.width + 1 && geometry.panel > 0);
      }
    }
  }
  await language().selectOption('en'); await theme().selectOption('light'); await page.setViewportSize({ width: 1440, height: 1000 });
  await page.screenshot({ path: `${report}/review-wide.png`, fullPage: true });
  await page.route(origin + review + '/approve', async route => {
    const accepted = await route.fetch();
    check('approval committed before dropped response', accepted.ok());
    await route.abort('failed');
  }, { times: 1 });
  await page.getByRole('button', { name: 'Approve this selection', exact: true }).click();
  await page.getByRole('alert').filter({ hasText: 'outcome is not yet confirmed' }).waitFor();
  check('lost response blocks blind repeated approval', await page.getByRole('button', { name: 'Approve this selection', exact: true }).isDisabled());
  await page.getByRole('button', { name: 'Refresh', exact: true }).click();
  await page.getByText('Approval recorded', { exact: true }).waitFor();
  check('refresh queries existing approval', await page.getByRole('button', { name: 'Approve this selection', exact: true }).isDisabled());
  await theme().selectOption('dark'); await page.setViewportSize({ width: 390, height: 844 });
  await page.screenshot({ path: `${report}/review-narrow-dark.png`, fullPage: true });
  await page.getByRole('button', { name: 'Sign out', exact: true }).click();
  await page.locator('input[name=login]').waitFor();
  check('logout removes workbench documents', await page.locator('[role=tabpanel]').count() === 0);
  check('no browser runtime errors', errors.length === 0);
  await writeFile(`${report}/report.json`, JSON.stringify({ passed: checks.length, checks, errors }, null, 2));
  console.log(`workbench browser acceptance: ${checks.length} checks passed`);
} catch (error) {
  await page.screenshot({ path: `${report}/failure.png`, fullPage: true }).catch(() => {});
  await writeFile(`${report}/failure.json`, JSON.stringify({ error: String(error), checks, errors }, null, 2));
  throw error;
} finally { await browser.close(); }
